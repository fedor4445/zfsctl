//! The sudoers entry a managed host needs: generated from the registry (`print-sudoers`) and verified against
//! the host (`check-host`). Pure pieces first; the two commands are below.

use super::privilege::{self, Entry, Feature, Kind};
use super::CommandRunner;
use crate::zfs::host_health::{host_os_from_uname, HostOs};
use std::fmt::Write as _;

/// Why a run stopped. `Unreachable` (ssh exit 255) maps to exit code 2 so a script can tell "the host is
/// down" from "the host is misconfigured".
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    Unreachable(String),
    Problem(String),
}

impl Failure {
    pub fn exit_code(&self) -> i32 {
        match self {
            Failure::Unreachable(_) => 2,
            Failure::Problem(_) => 1,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Failure::Unreachable(m) | Failure::Problem(m) => m,
        }
    }
}

fn problem(message: impl Into<String>) -> Failure {
    Failure::Problem(message.into())
}

#[derive(Debug)]
pub struct Resolved {
    pub entry: &'static Entry,
    pub path: String,
}

/// Directories only root can write to. A resolved path anywhere else could be a binary the connecting user
/// planted earlier in `PATH`, and must never become a sudoers entry.
const SYSTEM_DIRS: [&str; 6] = ["/bin/", "/sbin/", "/usr/bin/", "/usr/sbin/", "/usr/local/bin/", "/usr/local/sbin/"];

/// Run as `sh -c RESOLVE_SCRIPT sh <name>...`: prints `name=path` per command (empty path when missing, which
/// keeps the lines aligned with the names). The extended `PATH` matters because a non-interactive ssh session
/// often has only `/usr/bin:/bin`; sudo itself resolves from its own `secure_path`.
pub const RESOLVE_SCRIPT: &str = r#"PATH="$PATH:/sbin:/usr/sbin:/usr/local/sbin:/usr/local/bin"; for c in "$@"; do printf '%s=%s\n' "$c" "$(command -v "$c")"; done"#;

/// The account zfsctl connects as: the part of `ssh_target` before `@`. An alias with no user part is refused:
/// the sudoers line would have to guess whose account it is for.
pub fn user_of(ssh_target: &str) -> Result<&str, Failure> {
    // `ALL` and any name of only capitals, digits and underscores is an alias to sudoers, not an account.
    let alias_like = |user: &str| user.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') && user.chars().any(|c| c.is_ascii_uppercase());
    let ok = |user: &str| !user.is_empty() && !user.starts_with('-') && !alias_like(user) && user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    match ssh_target.split_once('@') {
        Some((user, _)) if ok(user) => Ok(user),
        _ => Err(problem(format!("ssh_target '{ssh_target}' needs a plain user@host form (letters, digits, . _ -), so the sudoers line knows which account it is for"))),
    }
}

pub fn vet_path(name: &str, path: &str) -> Result<(), Failure> {
    let plain = path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+'));
    if plain && !path.contains("..") && SYSTEM_DIRS.iter().any(|dir| path.starts_with(dir)) {
        Ok(())
    } else {
        Err(problem(format!("{name} resolved to '{path}', which is not under a system directory ({}); refusing to put it in a sudoers entry", SYSTEM_DIRS.join(" "))))
    }
}

pub fn parse_resolution(host: &str, stdout: &str, wanted: &[&'static Entry]) -> Result<Vec<Resolved>, Failure> {
    let found: std::collections::HashMap<&str, &str> = stdout.lines().filter_map(|line| line.split_once('=')).collect();
    let mut missing = Vec::new();
    let mut resolved = Vec::new();
    for entry in wanted {
        match found.get(entry.name).copied().filter(|p| !p.is_empty()) {
            Some(path) => {
                vet_path(entry.name, path)?;
                resolved.push(Resolved { entry, path: path.to_string() });
            }
            None => missing.push(entry.name),
        }
    }
    if missing.is_empty() {
        Ok(resolved)
    } else {
        Err(problem(format!("not found on {host}: {} (install them, or leave out the feature that needs them with --without)", missing.join(", "))))
    }
}

/// One sudoers command spec per allowed form: the bare path for an open entry, `path args` for each pinned one.
pub fn sudoers_items(resolved: &[Resolved]) -> Vec<String> {
    let mut items = Vec::new();
    for r in resolved {
        match &r.entry.kind {
            Kind::Privileged { pinned, .. } if !pinned.is_empty() => items.extend(pinned.iter().map(|args| format!("{} {}", r.path, args.join(" ")))),
            _ => items.push(r.path.clone()),
        }
    }
    items
}

/// Where a sudoers drop-in lives: FreeBSD's sudo package reads `/usr/local/etc/sudoers.d`, Ubuntu's and OmniOS's sudo
/// `/etc/sudoers.d`.
pub fn install_path(os: HostOs) -> &'static str {
    if matches!(os, HostOs::Linux | HostOs::Illumos) { "/etc/sudoers.d/zfsctl" } else { "/usr/local/etc/sudoers.d/zfsctl" }
}

/// The OS of the host behind `runner` (`uname -s`, no sudo); `Other` when it cannot be read.
pub fn detect_os(runner: &dyn CommandRunner) -> HostOs {
    match runner.run_unprivileged("uname", &["-s"]) {
        Ok(out) if out.success() => host_os_from_uname(&out.stdout),
        _ => HostOs::Other,
    }
}

pub fn render(host: &str, user: &str, os: HostOs, features: &[Feature], resolved: &[Resolved], generated: &str) -> String {
    // The host name is the user's own config text: control characters would start a new line, which in a
    // sudoers file is a new rule.
    let host: String = host.chars().map(|c| if c.is_control() { '?' } else { c }).collect();
    let mut text = format!("# zfsctl sudoers entry for host {host}, generated {generated}\n");
    let _ = writeln!(text, "# features: {}", features.iter().map(|f| f.name()).collect::<Vec<_>>().join(", "));
    let powerful: Vec<&str> = resolved.iter().map(|r| r.entry.name).filter(|name| matches!(*name, "zfs" | "bectl" | "beadm" | "restic")).collect();
    if !powerful.is_empty() {
        let (verb, pronoun) = if powerful.len() == 1 { ("is", "it") } else { ("are", "them") };
        let _ = writeln!(text, "# {} {verb} as powerful as root: grant {pronoun} only to hosts you trust zfsctl with.", powerful.join(", "));
    }
    if resolved.iter().any(|r| r.entry.name == "smartctl") {
        text.push_str("# smartctl: the /dev/* form cannot stop an extra flag (sudo's * also matches spaces); zfsctl only ever sends read-only flags,\n# and this account already holds zfs and zpool, which are as powerful as root.\n");
    }
    if resolved.iter().any(|r| r.entry.name == "ls") {
        text.push_str("# ls: sudo matches the words as one string, so these forms cannot stop an extra path or an option slipped in by merging words\n# (sudo's * also matches spaces and /). ls has no option that writes or runs anything (the worst is a recursive listing), zfsctl only\n# reads .zfs/snapshot folders with them, and this account already holds zfs and zpool, which are as powerful as root.\n");
    }
    let path = install_path(os);
    let _ = write!(text, "# Install as {path} (owner root, mode 0440) after checking it:\n#   visudo -c -f {path}\n");
    let unprivileged: Vec<&str> = privilege::unprivileged_for(features).iter().map(|e| e.name).collect();
    if !unprivileged.is_empty() {
        let _ = writeln!(text, "# These run without sudo (needed only if the ssh key is locked to a forced command): {}", unprivileged.join(", "));
    }
    let _ = writeln!(text, "{user} ALL=(root) NOPASSWD: \\\n    {}", sudoers_items(resolved).join(", \\\n    "));
    text
}

fn unreachable(host: &str, detail: impl std::fmt::Display) -> Failure {
    Failure::Unreachable(format!("cannot reach {host}: {detail}"))
}

/// Asks the host where each command lives, over the unprivileged ssh path (no sudo, so it works before
/// any sudoers entry exists), and vets the answers.
pub fn resolve(runner: &dyn CommandRunner, host: &str, entries: &[&'static Entry]) -> Result<Vec<Resolved>, Failure> {
    let mut args: Vec<&str> = vec!["-c", RESOLVE_SCRIPT, "sh"];
    args.extend(entries.iter().map(|e| e.name));
    let out = runner.run_unprivileged("sh", &args).map_err(|e| unreachable(host, e))?;
    if out.status == 255 {
        return Err(unreachable(host, out.stderr.trim()));
    }
    if !out.success() {
        return Err(problem(format!("looking up paths on {host} failed: {}", out.stderr.trim())));
    }
    parse_resolution(host, &out.stdout, entries)
}

pub fn print_sudoers(runner: &dyn CommandRunner, host: &str, ssh_target: &str, os: HostOs, features: &[Feature], generated: &str) -> Result<String, Failure> {
    let user = user_of(ssh_target)?;
    let entries = privilege::privileged_for(features);
    if entries.is_empty() {
        return Err(problem("the selected features need no privileged commands, so there is nothing to put in a sudoers entry"));
    }
    let resolved = resolve(runner, host, &entries)?;
    Ok(render(host, user, os, features, &resolved, generated))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Missing,
    TooBroad,
}

#[derive(Debug)]
pub struct Line {
    pub name: &'static str,
    /// The path and, for a pinned entry, its arguments: what sudoers has to allow.
    pub detail: String,
    pub status: Status,
}

#[derive(Debug)]
pub struct Report {
    pub lines: Vec<Line>,
    /// Things that make the lines less trustworthy than they look; any note fails the run.
    pub notes: Vec<String>,
}

impl Report {
    pub fn exit_code(&self) -> i32 {
        i32::from(self.lines.iter().any(|l| l.status != Status::Ok) || !self.notes.is_empty())
    }

    pub fn render(&self) -> String {
        let mut text = String::new();
        for line in &self.lines {
            let label = match line.status {
                Status::Ok => "ok",
                Status::Missing => "MISSING",
                Status::TooBroad => "TOO BROAD",
            };
            let _ = writeln!(text, "{label:<10}{:<11}{}", line.name, line.detail);
        }
        let count = |s: Status| self.lines.iter().filter(|l| l.status == s).count();
        let _ = writeln!(text, "{} ok, {} missing, {} too broad", count(Status::Ok), count(Status::Missing), count(Status::TooBroad));
        for note in &self.notes {
            let _ = writeln!(text, "NOTE: {note}");
        }
        text
    }
}

struct Asked {
    allowed: bool,
    /// The first line sudo printed to stderr when it refused (empty when it said nothing).
    refusal: String,
    /// The path sudo says it would run (the first word of `sudo -l`'s answer), empty when refused.
    printed: String,
}

/// `sudo -n -l <name> [args]`: whether the policy permits that command, and which binary sudo resolves the
/// bare name to. zfsctl runs bare names (`sudo -n zfs ...`), so that is what is asked, not the path
/// `command -v` found: sudo resolves through its own `secure_path`, which can differ.
fn sudo_ask(runner: &dyn CommandRunner, host: &str, name: &str, args: &[&str]) -> Result<Asked, Failure> {
    let mut full = vec!["-n", "-l", name];
    full.extend_from_slice(args);
    let out = runner.run_unprivileged("sudo", &full).map_err(|e| unreachable(host, e))?;
    if out.status == 255 {
        return Err(unreachable(host, out.stderr.trim()));
    }
    let printed = if out.success() { out.stdout.split_whitespace().next().unwrap_or("").to_string() } else { String::new() };
    let refusal = if out.success() { String::new() } else { out.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string() };
    Ok(Asked { allowed: out.success(), refusal, printed })
}

/// How many of the account's sudo rules would ask for a password, counted from `sudo -ll`: an entry without
/// `!authenticate`. `sudo -l <command>` says "allowed" for those too, so without this a `%wheel` rule could make
/// a missing zfsctl entry look fine. Output with no `Sudoers entry:` blocks (an older sudo) gives 0: no verdict.
pub fn password_rules(listing: &str) -> usize {
    listing.split("Sudoers entry:").skip(1).filter(|block| !block.lines().any(|l| l.trim_start().starts_with("Options:") && l.contains("!authenticate"))).count()
}

/// `check_host` for a configured host: the account in `ssh_target` is vetted first, exactly as `print_sudoers`
/// does, so both commands agree on which targets they will work with.
pub fn check_host_target(runner: &dyn CommandRunner, host: &str, ssh_target: &str, features: &[Feature]) -> Result<Report, Failure> {
    user_of(ssh_target)?;
    check_host(runner, host, features)
}

pub fn check_host(runner: &dyn CommandRunner, host: &str, features: &[Feature]) -> Result<Report, Failure> {
    let entries = privilege::privileged_for(features);
    if entries.is_empty() {
        return Err(problem("the selected features need no privileged commands, so there is nothing to check"));
    }
    let resolved = resolve(runner, host, &entries)?;
    let mut lines = Vec::new();
    let mut first_refusal = String::new();
    for r in &resolved {
        // Each pinned form, with the extra arguments sudo must refuse after it: a different argument in its
        // place, and one appended (a wildcard such as `smb4.conf*` refuses the first and allows the second).
        let forms: Vec<(&[&str], Vec<Vec<&str>>)> = match &r.entry.kind {
            Kind::Privileged { pinned, probe } if !pinned.is_empty() && probe.is_empty() => pinned.iter().map(|args| (*args, Vec::new())).collect(),
            Kind::Privileged { pinned, probe } if !pinned.is_empty() => {
                pinned.iter().map(|args| (*args, vec![vec![*probe], args.iter().copied().chain([*probe]).collect()])).collect()
            }
            _ => vec![(&[][..], Vec::new())],
        };
        for (args, probes) in forms {
            let mut detail = if args.is_empty() { r.path.clone() } else { format!("{} {}", r.path, args.join(" ")) };
            let asked = sudo_ask(runner, host, r.entry.name, args)?;
            if first_refusal.is_empty() {
                first_refusal.clone_from(&asked.refusal);
            }
            let status = if !asked.allowed {
                Status::Missing
            } else if asked.printed != r.path {
                detail = format!("{detail} (sudo would run {})", if asked.printed.is_empty() { "?" } else { &asked.printed });
                Status::Missing
            } else {
                let mut status = Status::Ok;
                for probe in &probes {
                    if sudo_ask(runner, host, r.entry.name, probe)?.allowed {
                        status = Status::TooBroad;
                        break;
                    }
                }
                status
            };
            lines.push(Line { name: r.entry.name, detail, status });
        }
    }
    let mut notes = Vec::new();
    if !first_refusal.is_empty() && lines.iter().all(|l| l.status == Status::Missing) {
        notes.push(format!("sudo said: {first_refusal}"));
    }
    // Last, so a host that drops mid-run fails on the checks first. A failure to list is no verdict.
    if let Ok(out) = runner.run_unprivileged("sudo", &["-n", "-ll"]) {
        if out.success() {
            let n = password_rules(&out.stdout);
            if n > 0 {
                notes.push(format!("this account also has {n} sudo rule(s) that ask for a password (a %wheel rule, say). sudo may satisfy a command through one of those, so an ok above does not prove zfsctl will never be asked for a password, and TOO BROAD may be a false alarm. Use an account whose only sudo rules are the zfsctl ones."));
            }
        }
    }
    Ok(Report { lines, notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result as AppResult;
    use crate::exec::privilege::{privileged_for, select};
    use crate::exec::CommandOutput;
    use std::sync::Mutex;

    fn resolution_for(host: &str, lines: &str, features: &[Feature]) -> Result<Vec<Resolved>, Failure> {
        parse_resolution(host, lines, &privileged_for(features))
    }

    const DEFAULT_LINES: &str = "zpool=/sbin/zpool\nzfs=/sbin/zfs\ngetfacl=/bin/getfacl\nsetfacl=/bin/setfacl\ngeom=/sbin/geom\nbectl=/sbin/bectl\nservice=/usr/sbin/service\nsmbpasswd=/usr/local/bin/smbpasswd\nsmbstatus=/usr/local/bin/smbstatus\ncat=/bin/cat\ntee=/usr/bin/tee\n";

    #[test]
    fn the_user_is_the_part_before_the_at_sign() {
        assert_eq!(user_of("fbsd@172.16.10.60"), Ok("fbsd"));
        assert_eq!(user_of("zfs.admin_1@host"), Ok("zfs.admin_1"));
    }

    #[test]
    fn a_user_name_sudoers_would_read_as_an_alias_is_refused() {
        // `ALL` is every user; an all-uppercase name is a User_Alias. Neither names an account.
        for bad in ["ALL@h", "FBSD@h", "A_1@h", "X9@h"] {
            assert_eq!(user_of(bad).unwrap_err().exit_code(), 1, "{bad:?}");
        }
        assert_eq!(user_of("Fbsd@h"), Ok("Fbsd"));
        assert_eq!(user_of("a1@h"), Ok("a1"));
    }

    #[test]
    fn control_characters_in_a_host_name_cannot_start_a_new_line_in_the_output() {
        let features = select(&[], &[]).unwrap();
        let resolved = parse_resolution("h", DEFAULT_LINES, &privilege::privileged_for(&features)).unwrap();
        let text = render("evil\n%root ALL=(ALL) NOPASSWD: ALL\r", "fbsd", HostOs::FreeBsd, &features, &resolved, "2026-10-10");
        assert!(text.lines().all(|l| l.starts_with('#') || l.starts_with("fbsd ") || l.starts_with("    ")), "{text}");
        assert!(text.contains("evil?%root"), "{text}");
    }

    #[test]
    fn check_host_vets_the_user_like_print_sudoers_and_asks_nothing_when_it_is_refused() {
        let runner = sudo_host(EXACT, ALL_PATHS);
        let err = check_host_target(&runner, "h", "ALL@h", &select(&[], &[]).unwrap()).unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(runner.count(), 0);
        assert!(check_host_target(&runner, "h", "fbsd@h", &select(&[], &[]).unwrap()).is_ok());
    }

    #[test]
    fn when_sudo_refuses_everything_its_own_message_is_shown() {
        let runner = Scripted::new(|cmd, args| match cmd {
            "sh" => {
                let lines = args[3..].iter().fold(String::new(), |mut acc, n| {
                    let _ = writeln!(acc, "{n}=/sbin/{n}");
                    acc
                });
                out(&lines, 0)
            }
            _ => CommandOutput { stdout: String::new(), stderr: "sudo: a password is required\n".to_string(), status: 1 },
        });
        let report = check_host(&runner, "h", &[Feature::Core]).unwrap();
        assert!(report.lines.iter().all(|l| l.status == Status::Missing));
        assert!(report.render().contains("sudo said: sudo: a password is required"), "{}", report.render());
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn a_target_without_a_user_is_refused_because_the_user_would_be_guessed() {
        for bad in ["fb16", "172.16.10.60", "@host", "-oProxyCommand=x@host", "a b@host", "a,b@host", ""] {
            let err = user_of(bad).unwrap_err();
            assert_eq!(err.exit_code(), 1, "{bad:?}");
        }
        assert!(user_of("fb16").unwrap_err().message().contains("user@"));
    }

    #[test]
    fn system_paths_are_accepted() {
        for p in ["/sbin/zpool", "/bin/cat", "/usr/bin/tee", "/usr/sbin/service", "/usr/local/bin/restic", "/usr/local/sbin/x"] {
            assert!(vet_path("x", p).is_ok(), "{p}");
        }
    }

    #[test]
    fn paths_outside_the_system_directories_are_refused() {
        for p in ["/home/fbsd/restic", "/tmp/zfs", "/var/tmp/zfs", "zfs", "./zfs", "/usr/local/bin/../../tmp/x", "/usr/local/bin/a b", "/usr/local/bin/a,b", "/usr/local/bin/a\\b", ""] {
            assert!(vet_path("restic", p).is_err(), "{p:?} must be refused");
        }
        let err = vet_path("restic", "/home/fbsd/restic").unwrap_err();
        assert!(err.message().contains("restic") && err.message().contains("/home/fbsd/restic"), "{}", err.message());
    }

    #[test]
    fn a_shell_alias_or_builtin_answer_is_not_a_path() {
        let lines = "zpool=alias zpool='rm -rf /'\nzfs=/sbin/zfs\n";
        let wanted: Vec<_> = privileged_for(&[Feature::Core]);
        let err = parse_resolution("h", lines, &wanted).unwrap_err();
        assert!(err.message().contains("zpool"), "{}", err.message());
    }

    #[test]
    fn resolution_parses_name_equals_path_lines_in_registry_order() {
        let features = select(&[], &[]).unwrap();
        let resolved = resolution_for("h", DEFAULT_LINES, &features).unwrap();
        assert_eq!(resolved.len(), 11);
        assert_eq!((resolved[0].entry.name, resolved[0].path.as_str()), ("zpool", "/sbin/zpool"));
        assert_eq!((resolved[10].entry.name, resolved[10].path.as_str()), ("tee", "/usr/bin/tee"));
    }

    #[test]
    fn a_missing_command_names_the_host_and_every_missing_command() {
        let lines = DEFAULT_LINES.replace("geom=/sbin/geom\n", "geom=\n").replace("bectl=/sbin/bectl\n", "");
        let err = resolution_for("fbsd16", &lines, &select(&[], &[]).unwrap()).unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert!(err.message().contains("fbsd16") && err.message().contains("geom") && err.message().contains("bectl"), "{}", err.message());
    }

    #[test]
    fn the_items_are_paths_with_the_pinned_arguments_written_out() {
        let resolved = resolution_for("h", DEFAULT_LINES, &select(&[], &[]).unwrap()).unwrap();
        assert_eq!(
            sudoers_items(&resolved),
            [
                "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/geom", "/sbin/bectl", "/usr/sbin/service",
                "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/usr/bin/tee /usr/local/etc/smb4.conf",
            ]
        );
    }

    #[test]
    fn the_rendered_entry_has_the_install_steps_and_one_continued_line() {
        let features = select(&[], &[]).unwrap();
        let resolved = resolution_for("fbsd16", DEFAULT_LINES, &features).unwrap();
        let text = render("fbsd16", "fbsd", HostOs::FreeBsd, &features, &resolved, "2026-10-09");
        assert!(text.starts_with("# zfsctl sudoers entry for host fbsd16, generated 2026-10-09\n"), "{text}");
        assert!(text.contains("# features: core, acl, disks, bootenvs, shares, samba\n"), "{text}");
        assert!(text.contains("visudo -c -f /usr/local/etc/sudoers.d/zfsctl") && text.contains("mode 0440"), "{text}");
        assert!(text.contains("fbsd ALL=(root) NOPASSWD: \\\n    /sbin/zpool, \\\n    /sbin/zfs, \\\n"), "{text}");
        assert!(text.trim_end().ends_with("/usr/bin/tee /usr/local/etc/smb4.conf"), "{text}");
        assert!(text.contains("without sudo") && text.contains("getent"), "unprivileged commands are listed in a comment: {text}");
        assert!(!text.contains("restic"), "restic was not selected: {text}");
        assert!(text.contains("zfs, bectl are as powerful as root"), "{text}");
    }

    #[test]
    fn restic_is_in_the_line_only_when_selected() {
        let features = select(&["restic".to_string()], &["samba".to_string()]).unwrap();
        let lines = "zpool=/sbin/zpool\nzfs=/sbin/zfs\ngetfacl=/bin/getfacl\nsetfacl=/bin/setfacl\ngeom=/sbin/geom\nbectl=/sbin/bectl\nservice=/usr/sbin/service\nrestic=/usr/local/bin/restic\nreadlink=/usr/bin/readlink\n";
        let resolved = resolution_for("h", lines, &features).unwrap();
        let text = render("h", "fbsd", HostOs::FreeBsd, &features, &resolved, "d");
        assert!(text.contains("/usr/local/bin/restic"), "{text}");
        assert!(!text.contains("smbpasswd") && !text.contains("tee"), "{text}");
    }

    /// Answers every call through `answer(cmd, args)` and records the call.
    struct Scripted {
        calls: Mutex<Vec<String>>,
        answer: Box<dyn Fn(&str, &[&str]) -> CommandOutput + Send + Sync>,
    }

    impl Scripted {
        fn new(answer: impl Fn(&str, &[&str]) -> CommandOutput + Send + Sync + 'static) -> Self {
            Scripted { calls: Mutex::new(Vec::new()), answer: Box::new(answer) }
        }
        fn count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl CommandRunner for Scripted {
        fn run(&self, cmd: &str, args: &[&str]) -> AppResult<CommandOutput> {
            self.calls.lock().unwrap().push(format!("{cmd} {}", args.join(" ")));
            Ok((self.answer)(cmd, args))
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _stdin: &str) -> AppResult<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> AppResult<CommandOutput> {
            self.run(cmd, args)
        }
    }

    fn out(stdout: &str, status: i32) -> CommandOutput {
        CommandOutput { stdout: stdout.to_string(), stderr: String::new(), status }
    }

    /// `sh -c SCRIPT sh name...` answered from a table of where each command lives.
    fn host_with(paths: &'static [(&'static str, &'static str)]) -> Scripted {
        Scripted::new(move |cmd, args| {
            assert_eq!(cmd, "sh");
            assert_eq!(&args[..3], ["-c", RESOLVE_SCRIPT, "sh"]);
            let lines = args[3..].iter().fold(String::new(), |mut acc, n| {
                let _ = writeln!(acc, "{n}={}", paths.iter().find(|(name, _)| name == n).map_or("", |(_, p)| *p));
                acc
            });
            out(&lines, 0)
        })
    }

    const ALL_PATHS: &[(&str, &str)] = &[
        ("zpool", "/sbin/zpool"), ("zfs", "/sbin/zfs"), ("getfacl", "/bin/getfacl"), ("setfacl", "/bin/setfacl"), ("geom", "/sbin/geom"),
        ("bectl", "/sbin/bectl"), ("service", "/usr/sbin/service"), ("smbpasswd", "/usr/local/bin/smbpasswd"), ("smbstatus", "/usr/local/bin/smbstatus"),
        ("cat", "/bin/cat"), ("tee", "/usr/bin/tee"), ("restic", "/usr/local/bin/restic"), ("readlink", "/usr/bin/readlink"),
    ];

    #[test]
    fn print_sudoers_asks_the_host_once_for_exactly_the_selected_commands() {
        let runner = host_with(ALL_PATHS);
        let features = select(&[], &[]).unwrap();
        let text = print_sudoers(&runner, "fbsd16", "fbsd@172.16.10.60", HostOs::FreeBsd, &features, "2026-10-09").unwrap();
        assert_eq!(runner.count(), 1);
        let call = runner.calls.lock().unwrap()[0].clone();
        assert!(call.ends_with("sh zpool zfs getfacl setfacl geom bectl service smbpasswd smbstatus cat tee"), "{call}");
        assert!(text.contains("fbsd ALL=(root) NOPASSWD: \\\n    /sbin/zpool"), "{text}");
        assert!(!text.contains("restic"), "{text}");
    }

    #[test]
    fn with_restic_the_host_is_asked_for_restic_too() {
        let runner = host_with(ALL_PATHS);
        let features = select(&["restic".to_string()], &[]).unwrap();
        let text = print_sudoers(&runner, "fbsd16", "fbsd@h", HostOs::FreeBsd, &features, "d").unwrap();
        assert!(runner.calls.lock().unwrap()[0].ends_with("tee restic readlink"));
        assert!(text.contains("/usr/local/bin/restic"), "{text}");
    }

    #[test]
    fn an_alias_target_is_refused_before_any_ssh_call() {
        let runner = host_with(ALL_PATHS);
        let err = print_sudoers(&runner, "fb16", "fb16", HostOs::FreeBsd, &select(&[], &[]).unwrap(), "d").unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(runner.count(), 0, "no line may be produced and nothing may be asked of the host");
    }

    #[test]
    fn a_user_writable_resolution_is_refused_and_no_line_is_printed() {
        let runner = host_with(&[("zpool", "/home/fbsd/bin/zpool")]);
        let err = print_sudoers(&runner, "h", "fbsd@h", HostOs::FreeBsd, &[Feature::Core], "d").unwrap_err();
        assert!(err.message().contains("/home/fbsd/bin/zpool"), "{}", err.message());
    }

    #[test]
    fn a_missing_command_stops_the_run_with_exit_1() {
        let runner = host_with(&[("zpool", "/sbin/zpool")]);
        let err = print_sudoers(&runner, "h", "fbsd@h", HostOs::FreeBsd, &[Feature::Core], "d").unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert!(err.message().contains("zfs"), "{}", err.message());
    }

    #[test]
    fn an_unreachable_host_is_exit_2() {
        let runner = Scripted::new(|_, _| CommandOutput { stdout: String::new(), stderr: "ssh: connect to host h port 22: Operation timed out".to_string(), status: 255 });
        let err = print_sudoers(&runner, "h", "fbsd@h", HostOs::FreeBsd, &[Feature::Core], "d").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.message().contains("timed out"), "{}", err.message());
    }

    #[test]
    fn selecting_no_privileged_commands_is_a_problem_not_an_empty_line() {
        let runner = host_with(ALL_PATHS);
        let err = print_sudoers(&runner, "h", "fbsd@h", HostOs::FreeBsd, &[], "d").unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(runner.count(), 0);
    }

    /// A clean `sudo -ll`: every rule is NOPASSWD.
    const CLEAN_LISTING: &str = "User fbsd may run the following commands on h:\n\nSudoers entry: /usr/local/etc/sudoers.d/zfsctl\n    RunAsUsers: root\n    Options: !authenticate\n    Commands:\n\t/sbin/zpool\n";

    /// A host whose sudoers allows exactly `allowed` command lines (path plus arguments, space-joined). Like
    /// the real sudo, `sudo -n -l <name> args` answers with the path it would run.
    fn sudo_host_with(allowed: &'static [&'static str], paths: &'static [(&'static str, &'static str)], listing: &'static str) -> Scripted {
        Scripted::new(move |cmd, args| match cmd {
            "sh" => {
                let lines = args[3..].iter().fold(String::new(), |mut acc, n| {
                    let _ = writeln!(acc, "{n}={}", paths.iter().find(|(name, _)| name == n).map_or("", |(_, p)| *p));
                    acc
                });
                out(&lines, 0)
            }
            "sudo" => {
                if args == ["-n", "-ll"] {
                    return out(listing, 0);
                }
                assert_eq!(&args[..2], ["-n", "-l"]);
                let path = paths.iter().find(|(n, _)| *n == args[2]).map_or(args[2], |(_, p)| *p);
                let asked = std::iter::once(path).chain(args[3..].iter().copied()).collect::<Vec<_>>().join(" ");
                out(&asked, i32::from(!allowed.contains(&asked.as_str())))
            }
            other => panic!("unexpected {other}"),
        })
    }

    fn sudo_host(allowed: &'static [&'static str], paths: &'static [(&'static str, &'static str)]) -> Scripted {
        sudo_host_with(allowed, paths, CLEAN_LISTING)
    }

    const EXACT: &[&str] = &[
        "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/geom", "/sbin/bectl", "/usr/sbin/service",
        "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/usr/bin/tee /usr/local/etc/smb4.conf",
    ];

    fn statuses(report: &Report) -> Vec<(&'static str, Status)> {
        report.lines.iter().map(|l| (l.name, l.status)).collect()
    }

    #[test]
    fn a_host_with_exactly_the_right_entries_is_all_ok() {
        let runner = sudo_host(EXACT, ALL_PATHS);
        let report = check_host(&runner, "fbsd16", &select(&[], &[]).unwrap()).unwrap();
        assert_eq!(report.lines.len(), 11);
        assert!(report.lines.iter().all(|l| l.status == Status::Ok), "{}", report.render());
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn a_missing_entry_is_reported_by_name_and_fails_the_run() {
        const WITHOUT_GEOM: &[&str] = &[
            "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/bectl", "/usr/sbin/service",
            "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/usr/bin/tee /usr/local/etc/smb4.conf",
        ];
        let report = check_host(&sudo_host(WITHOUT_GEOM, ALL_PATHS), "h", &select(&[], &[]).unwrap()).unwrap();
        assert!(statuses(&report).contains(&("geom", Status::Missing)), "{}", report.render());
        assert_eq!(report.exit_code(), 1);
        assert!(report.render().contains("MISSING") && report.render().contains("1 missing"), "{}", report.render());
    }

    #[test]
    fn a_plain_cat_entry_is_too_broad() {
        const BROAD_CAT: &[&str] = &[
            "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/geom", "/sbin/bectl", "/usr/sbin/service",
            "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/bin/cat /etc/master.passwd", "/usr/bin/tee /usr/local/etc/smb4.conf",
        ];
        let report = check_host(&sudo_host(BROAD_CAT, ALL_PATHS), "h", &select(&[], &[]).unwrap()).unwrap();
        assert!(statuses(&report).contains(&("cat", Status::TooBroad)), "{}", report.render());
        assert_eq!(report.exit_code(), 1);
        assert!(report.render().contains("TOO BROAD"), "{}", report.render());
    }

    #[test]
    fn restic_is_checked_only_when_selected() {
        let runner = sudo_host(&["/usr/local/bin/restic", "/usr/bin/readlink"], ALL_PATHS);
        let report = check_host(&runner, "h", &select(&["restic".to_string()], &["core", "acl", "disks", "bootenvs", "shares", "samba"].map(String::from)).unwrap()).unwrap();
        assert_eq!(statuses(&report), [("restic", Status::Ok), ("readlink", Status::Ok)]);
    }

    #[test]
    fn an_unreachable_host_is_exit_2_after_a_single_ssh_call() {
        let runner = Scripted::new(|_, _| CommandOutput { stdout: String::new(), stderr: "ssh: Could not resolve hostname".to_string(), status: 255 });
        let err = check_host(&runner, "h", &select(&[], &[]).unwrap()).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(runner.count(), 1, "one timeout, not one per entry");
    }

    #[test]
    fn ssh_dropping_mid_run_is_exit_2_and_stops_asking() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let runner = Scripted::new(move |cmd, args| {
            if cmd == "sh" {
                let lines = args[3..].iter().fold(String::new(), |mut acc, n| {
                    let _ = writeln!(acc, "{n}=/sbin/{n}");
                    acc
                });
                return out(&lines, 0);
            }
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 { out("", 0) } else { CommandOutput { stdout: String::new(), stderr: "gone".to_string(), status: 255 } }
        });
        let err = check_host(&runner, "h", &[Feature::Core]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(runner.count(), 3, "lookup, one answered check, the one that failed");
    }

    #[test]
    fn a_missing_binary_on_the_host_is_a_problem_before_any_sudo_check() {
        let runner = sudo_host(EXACT, &[("zpool", "/sbin/zpool")]);
        let err = check_host(&runner, "h", &[Feature::Core]).unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(runner.count(), 1);
    }

    #[test]
    fn the_report_lists_one_line_per_entry_with_its_pinned_arguments() {
        let report = check_host(&sudo_host(EXACT, ALL_PATHS), "h", &select(&[], &[]).unwrap()).unwrap();
        let text = report.render();
        assert!(text.contains("ok") && text.contains("/bin/cat /usr/local/etc/smb4.conf"), "{text}");
        assert!(text.trim_end().ends_with("11 ok, 0 missing, 0 too broad"), "{text}");
    }


    const WHEEL_LISTING: &str = "User fbsd may run the following commands on h:\n\nSudoers entry: /usr/local/etc/sudoers.d/zfsctl\n    RunAsUsers: root\n    Options: !authenticate\n    Commands:\n\t/sbin/zpool\n\nSudoers entry: /usr/local/etc/sudoers\n    RunAsUsers: ALL\n    Commands:\n\tALL\n";

    #[test]
    fn rules_that_ask_for_a_password_are_counted_from_the_listing() {
        assert_eq!(password_rules(CLEAN_LISTING), 0);
        assert_eq!(password_rules(WHEEL_LISTING), 1);
        assert_eq!(password_rules("no entries here"), 0, "a sudo that formats -ll differently gives no verdict, not a false alarm");
        let mixed = "Sudoers entry: a\n    Options: !authenticate, setenv\n    Commands:\n\tx\n";
        assert_eq!(password_rules(mixed), 0, "!authenticate among other options still means no password");
    }

    #[test]
    fn a_password_rule_alongside_the_zfsctl_entry_makes_every_ok_unverified() {
        let runner = sudo_host_with(EXACT, ALL_PATHS, WHEEL_LISTING);
        let report = check_host(&runner, "h", &select(&[], &[]).unwrap()).unwrap();
        assert!(report.lines.iter().all(|l| l.status == Status::Ok));
        assert_eq!(report.notes.len(), 1, "{}", report.render());
        assert!(report.render().contains("ask for a password"), "{}", report.render());
        assert_eq!(report.exit_code(), 1, "ok lines that sudo may satisfy through another rule are not a clean bill");
    }

    #[test]
    fn a_clean_host_has_no_notes() {
        let report = check_host(&sudo_host(EXACT, ALL_PATHS), "h", &select(&[], &[]).unwrap()).unwrap();
        assert!(report.notes.is_empty() && report.exit_code() == 0);
    }

    #[test]
    fn an_entry_is_not_ok_when_sudo_would_run_a_different_binary_than_the_one_found() {
        // `command -v zfs` finds /sbin/zfs, but sudo (secure_path) resolves the bare name to the OpenZFS port.
        let runner = Scripted::new(|cmd, args| match cmd {
            "sh" => out("zpool=/sbin/zpool\nzfs=/sbin/zfs\n", 0),
            "sudo" if args == ["-n", "-ll"] => out(CLEAN_LISTING, 0),
            "sudo" => match args[2] {
                "zpool" => out("/sbin/zpool", 0),
                "zfs" => out("/usr/local/sbin/zfs", 0),
                other => panic!("zfsctl runs bare names, so only those may be asked: {other}"),
            },
            other => panic!("unexpected {other}"),
        });
        let report = check_host(&runner, "h", &[Feature::Core]).unwrap();
        assert_eq!(statuses(&report), [("zpool", Status::Ok), ("zfs", Status::Missing)]);
        assert!(report.lines[1].detail.contains("/sbin/zfs") && report.lines[1].detail.contains("/usr/local/sbin/zfs"), "{}", report.lines[1].detail);
    }

    const LINUX_PATHS: &[(&str, &str)] = &[("zpool", "/usr/sbin/zpool"), ("zfs", "/usr/sbin/zfs")];

    #[test]
    fn detect_os_reads_uname_and_falls_back_to_other() {
        let answer = |text: &'static str, status: i32| Scripted::new(move |cmd, args| {
            assert_eq!((cmd, args), ("uname", &["-s"][..]));
            CommandOutput { stdout: text.to_string(), stderr: String::new(), status }
        });
        assert_eq!(detect_os(&answer("Linux\n", 0)), HostOs::Linux);
        assert_eq!(detect_os(&answer("SunOS\n", 0)), HostOs::Illumos);
        assert_eq!(detect_os(&answer("FreeBSD\n", 0)), HostOs::FreeBsd);
        assert_eq!(detect_os(&answer("", 255)), HostOs::Other);
        assert_eq!(detect_os(&answer("garbage", 1)), HostOs::Other);
    }

    #[test]
    fn print_sudoers_for_a_linux_host_prints_only_zpool_and_zfs_and_the_etc_path() {
        let text = print_sudoers(&host_with(LINUX_PATHS), "ub24", "ub@172.16.10.52", HostOs::Linux, &[Feature::Core], "d").unwrap();
        assert!(text.contains("    /usr/sbin/zpool, \\\n    /usr/sbin/zfs"), "{text}");
        assert!(text.contains("/etc/sudoers.d/zfsctl") && !text.contains("/usr/local/etc/sudoers.d"), "{text}");
        assert!(!text.contains("geom") && !text.contains("bectl") && !text.contains("service"), "{text}");
        let freebsd = print_sudoers(&host_with(ALL_PATHS), "h", "fbsd@h", HostOs::FreeBsd, &[Feature::Core], "d").unwrap();
        assert!(freebsd.contains("/usr/local/etc/sudoers.d/zfsctl"), "{freebsd}");
    }

    const SMART_PATHS: &[(&str, &str)] = &[("smartctl", "/usr/local/sbin/smartctl")];
    const SMART_ALLOWED: &[&str] = &["/usr/local/sbin/smartctl --scan -j", "/usr/local/sbin/smartctl -j -H -A -i /dev/*"];

    #[test]
    fn smart_is_opt_in_like_restic_and_rclone() {
        assert!(!select(&[], &[]).unwrap().contains(&Feature::Smart));
        assert!(select(&["smart".to_string()], &[]).unwrap().contains(&Feature::Smart));
    }

    #[test]
    fn print_sudoers_with_smart_pins_the_two_forms_and_says_the_wildcard_is_not_a_barrier() {
        let text = print_sudoers(&host_with(SMART_PATHS), "h", "fbsd@h", HostOs::FreeBsd, &[Feature::Smart], "d").unwrap();
        assert!(text.contains("/usr/local/sbin/smartctl --scan -j") && text.contains("/usr/local/sbin/smartctl -j -H -A -i /dev/*"), "{text}");
        assert!(text.contains("# smartctl: the /dev/* form cannot stop an extra flag"), "{text}");
    }

    #[test]
    fn print_sudoers_with_browse_lists_every_ls_form_and_without_it_lists_none() {
        const LS_PATHS: &[(&str, &str)] = &[("ls", "/bin/ls")];
        let text = print_sudoers(&host_with(LS_PATHS), "h", "fbsd@h", HostOs::FreeBsd, &[Feature::Browse], "d").unwrap();
        assert!(text.contains("/bin/ls -lA -D %s -- *") && text.contains("/bin/ls -lAU -D %s -- *") && text.contains("/bin/ls -lAE -- *"), "{text}");
        assert!(text.contains("# ls: sudo matches the words as one string"), "{text}");
        let without = print_sudoers(&host_with(ALL_PATHS), "h", "fbsd@h", HostOs::FreeBsd, &[Feature::Core], "d").unwrap();
        assert!(!without.contains("/bin/ls") && !without.contains("# ls:"), "{without}");
    }

    #[test]
    fn check_host_does_not_flag_the_device_wildcard_as_too_broad() {
        let report = check_host(&sudo_host(SMART_ALLOWED, SMART_PATHS), "h", &[Feature::Smart]).unwrap();
        assert_eq!(statuses(&report), [("smartctl", Status::Ok), ("smartctl", Status::Ok)], "{}", report.render());
    }

    #[test]
    fn check_host_reports_smartctl_missing_when_sudo_refuses_it() {
        let report = check_host(&sudo_host(&[], SMART_PATHS), "h", &[Feature::Smart]).unwrap();
        assert_eq!(statuses(&report), [("smartctl", Status::Missing), ("smartctl", Status::Missing)], "{}", report.render());
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn a_wildcard_pinned_entry_is_too_broad_even_though_a_single_wrong_argument_is_refused() {
        // `cat /usr/local/etc/smb4.conf*` refuses `cat /etc/master.passwd` but allows an extra argument.
        const WILDCARD: &[&str] = &[
            "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/geom", "/sbin/bectl", "/usr/sbin/service",
            "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/bin/cat /usr/local/etc/smb4.conf /etc/master.passwd", "/usr/bin/tee /usr/local/etc/smb4.conf",
        ];
        let report = check_host(&sudo_host(WILDCARD, ALL_PATHS), "h", &select(&[], &[]).unwrap()).unwrap();
        assert!(statuses(&report).contains(&("cat", Status::TooBroad)), "{}", report.render());
    }

}
