//! Captures real zfsctl screens (or command output) for The zfsctl Book.
//!
//! TUI capture:     cargo run --example capture -- --name 08-pools --section Pools --cols 110 --rows 30 --wait 10 [--keys '<Down>'] [--crop 0:20]
//! Command capture: cargo run --example capture -- --name 05-check-host --cmd 'sudo ./zfsctl/target/release/zfsctl check-host fbsd16'
//!
//! Environment: HUB (default fb@172.16.10.90) and ZFSCTL_BIN (default ./zfsctl/target/release/zfsctl), both on the hub.
//! The TUI runs over `ssh -tt` with its terminal size set by `stty`, and what it draws is read through a terminal emulator
//! (the `vt100` crate), so the capture is the screen as a person would see it. Captures are written to
//! docs/book/captures/NAME.txt and are never edited by hand.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const SECTIONS: [&str; 12] = ["Dashboard", "Hosts", "Pools", "Datasets", "Snapshots", "Disks", "Shares", "Boot Envs", "Jobs", "Cloud", "Alerts", "Settings"];
const KEYS: [(&str, &str); 12] = [
    ("<Tab>", "\t"),
    ("<S-Tab>", "\x1b[Z"),
    ("<Enter>", "\r"),
    ("<Esc>", "\x1b"),
    ("<Down>", "\x1b[B"),
    ("<Up>", "\x1b[A"),
    ("<Left>", "\x1b[D"),
    ("<Right>", "\x1b[C"),
    ("<Space>", " "),
    ("<Backspace>", "\x7f"),
    ("<F2>", "\x1bOQ"),
    ("<F5>", "\x1b[15~"),
];

#[derive(Debug)]
struct Args {
    name: String,
    cols: u16,
    rows: u16,
    wait: f64,
    after: f64,
    keys: String,
    section: Option<String>,
    crop: Option<String>,
    cmd: Option<String>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args { name: String::new(), cols: 110, rows: 30, wait: 10.0, after: 1.5, keys: String::new(), section: None, crop: None, cmd: None };
    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--name" => args.name = value()?,
            "--cols" => args.cols = value()?.parse().map_err(|_| "--cols must be a number")?,
            "--rows" => args.rows = value()?.parse().map_err(|_| "--rows must be a number")?,
            "--wait" => args.wait = value()?.parse().map_err(|_| "--wait must be a number")?,
            "--after" => args.after = value()?.parse().map_err(|_| "--after must be a number")?,
            "--keys" => args.keys = value()?,
            "--section" => {
                let section = value()?;
                if !SECTIONS.contains(&section.as_str()) {
                    return Err(format!("unknown section '{section}'; one of: {}", SECTIONS.join(", ")));
                }
                args.section = Some(section);
            }
            "--crop" => args.crop = Some(value()?),
            "--cmd" => args.cmd = Some(value()?),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    if args.name.is_empty() {
        return Err("--name is required".to_string());
    }
    Ok(args)
}

/// `<Down>` and friends as the bytes a terminal sends; anything else is typed as it is.
fn expand(keys: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = keys;
    'next: while let Some(c) = rest.chars().next() {
        for (token, seq) in KEYS {
            if let Some(after) = rest.strip_prefix(token) {
                out.push(seq.to_string());
                rest = after;
                continue 'next;
            }
        }
        out.push(c.to_string());
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// An address, but not user@192.0.2.1 (an ssh target with a numeric host is not an email address).
fn replace_emails(line: &str) -> String {
    let local = |c: char| c.is_alphanumeric() || matches!(c, '_' | '.' | '+' | '-');
    let domain = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-');
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let mut start = out.chars().count();
            while start > 0 && out.chars().nth(start - 1).is_some_and(local) {
                start -= 1;
            }
            // the host: labels joined by dots, at least two of them
            let mut end = i + 1;
            let mut labels = 0;
            loop {
                let label_start = end;
                while end < chars.len() && domain(chars[end]) {
                    end += 1;
                }
                if end == label_start {
                    break;
                }
                labels += 1;
                if end + 1 < chars.len() && chars[end] == '.' && domain(chars[end + 1]) {
                    end += 1;
                } else {
                    break;
                }
            }
            let host: String = chars[i + 1..end].iter().collect();
            let numeric = host.split('.').count() == 4 && host.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
            if start < out.chars().count() && labels >= 2 && !numeric {
                let keep: String = out.chars().take(start).collect();
                out = keep + "you@example.com";
                i = end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Trailing spaces off; emails replaced; lines that mention a password dropped (except the backup form's password-file label
/// and its notes about the file, which hold only a path).
fn clean(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = lines
        .iter()
        .filter(|line| {
            let low = line.to_lowercase();
            !low.contains("password") || ["password file (absolute", "password (blank", "the password file"].iter().any(|ok| low.contains(ok))
        })
        .map(|line| replace_emails(line.trim_end()))
        .collect();
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

fn crop(lines: Vec<String>, spec: &str) -> Result<Vec<String>, String> {
    let (first, last) = spec.split_once(':').ok_or("--crop is FIRST:LAST")?;
    let parse = |s: &str| if s.is_empty() { Ok(None) } else { s.parse::<usize>().map(Some).map_err(|_| format!("bad crop number '{s}'")) };
    let (first, last) = (parse(first)?.unwrap_or(0), parse(last)?.unwrap_or(lines.len()).min(lines.len()));
    Ok(lines.get(first..last.max(first)).unwrap_or_default().to_vec())
}

/// Feeds what the TUI draws to the emulator for `seconds`.
fn pump(parser: &mut vt100::Parser, rx: &mpsc::Receiver<Vec<u8>>, seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left.min(Duration::from_millis(300))) {
            Ok(bytes) => parser.process(&bytes),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn tui(args: &Args) -> Result<Vec<String>, String> {
    let hub = std::env::var("HUB").unwrap_or_else(|_| "fb@172.16.10.90".to_string());
    let binary = std::env::var("ZFSCTL_BIN").unwrap_or_else(|_| "./zfsctl/target/release/zfsctl".to_string());
    let remote = format!("stty rows {} cols {}; exec sudo {binary}", args.rows, args.cols);
    let mut child = Command::new("ssh")
        .args(["-tt", "-o", "BatchMode=yes", &hub, &remote])
        .env("TERM", "xterm-256color")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run ssh: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut parser = vt100::Parser::new(args.rows, args.cols, 0);
    let mut send = |text: &str| {
        let _ = stdin.write_all(text.as_bytes());
        let _ = stdin.flush();
    };
    pump(&mut parser, &rx, args.wait);
    if let Some(section) = &args.section {
        for _ in 0..SECTIONS.iter().position(|s| s == section).unwrap_or(0) {
            send("\t");
            pump(&mut parser, &rx, 0.6);
        }
        pump(&mut parser, &rx, args.wait / 2.0);
    }
    for key in expand(&args.keys) {
        send(&key);
        pump(&mut parser, &rx, 0.6);
    }
    pump(&mut parser, &rx, args.after);
    let lines: Vec<String> = parser.screen().rows(0, args.cols).collect();
    send("q");
    pump(&mut parser, &rx, 0.5);
    let _ = child.kill();
    let _ = child.wait();
    Ok(lines)
}

fn command(cmd: &str) -> Result<Vec<String>, String> {
    let hub = std::env::var("HUB").unwrap_or_else(|_| "fb@172.16.10.90".to_string());
    let out = Command::new("ssh").args(["-o", "BatchMode=yes", &hub, cmd]).output().map_err(|e| format!("cannot run ssh: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Ok(text.lines().map(str::to_string).collect())
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&argv)?;
    let mut lines = match &args.cmd {
        Some(cmd) => command(cmd)?,
        None => tui(&args)?,
    };
    if let Some(spec) = &args.crop {
        lines = crop(lines, spec)?;
    }
    let text = clean(&lines).join("\n") + "\n";
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/book/captures").join(format!("{}.txt", args.name));
    std::fs::write(&path, &text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    println!("wrote {} ({} lines)", path.display(), text.lines().count());
    Ok(())
}

fn main() {
    if let Err(message) = run() {
        eprintln!("capture: {message}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| (*i).to_string()).collect()
    }

    #[test]
    fn named_keys_become_terminal_bytes_and_other_text_is_typed() {
        assert_eq!(expand("<Down><Down>ab<Enter>"), s(&["\x1b[B", "\x1b[B", "a", "b", "\r"]));
        assert_eq!(expand("<F5>é"), s(&["\x1b[15~", "é"]));
        assert_eq!(expand("<Nope>"), s(&["<", "N", "o", "p", "e", ">"]));
    }

    #[test]
    fn emails_are_replaced_but_ssh_targets_with_a_numeric_host_are_not() {
        assert_eq!(replace_emails("to: someone@example.net, ok"), "to: you@example.com, ok");
        assert_eq!(replace_emails("fb@172.16.10.90"), "fb@172.16.10.90");
        assert_eq!(replace_emails("a.b+c@mail.example.co.uk."), "you@example.com.");
        assert_eq!(replace_emails("user@host and @home"), "user@host and @home", "one label is not an address");
    }

    #[test]
    fn password_lines_are_dropped_except_the_backup_forms_file_label() {
        let lines = s(&["a   ", "Password: hunter2", "Restic password file (absolute path)", "the password file is checked", "b", "", ""]);
        assert_eq!(clean(&lines), s(&["a", "Restic password file (absolute path)", "the password file is checked", "b"]));
    }

    #[test]
    fn crop_takes_first_to_last_exclusive_and_allows_open_ends() {
        let lines = s(&["0", "1", "2", "3"]);
        assert_eq!(crop(lines.clone(), "1:3").unwrap(), s(&["1", "2"]));
        assert_eq!(crop(lines.clone(), ":2").unwrap(), s(&["0", "1"]));
        assert_eq!(crop(lines.clone(), "2:").unwrap(), s(&["2", "3"]));
        assert_eq!(crop(lines.clone(), "3:99").unwrap(), s(&["3"]));
        assert!(crop(lines, "x").is_err());
    }

    #[test]
    fn arguments_are_checked() {
        let a = parse_args(&s(&["--name", "08-pools", "--section", "Pools", "--cols", "100", "--keys", "<Down>"])).unwrap();
        assert_eq!((a.name.as_str(), a.cols, a.rows, a.section.as_deref(), a.keys.as_str()), ("08-pools", 100, 30, Some("Pools"), "<Down>"));
        assert!(parse_args(&s(&[])).is_err(), "--name is required");
        assert!(parse_args(&s(&["--name", "x", "--section", "Nope"])).is_err());
        assert!(parse_args(&s(&["--name", "x", "--wat"])).is_err());
    }

    #[test]
    fn the_emulator_turns_what_a_tui_draws_into_plain_screen_lines() {
        let mut parser = vt100::Parser::new(3, 20, 0);
        parser.process(b"\x1b[2J\x1b[Hhello \x1b[1;31mred\x1b[0m\r\n\x1b[3;1Hlast");
        let lines: Vec<String> = parser.screen().rows(0, 20).collect();
        assert_eq!((lines[0].trim_end(), lines[2].trim_end()), ("hello red", "last"));
    }
}
