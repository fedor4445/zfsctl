//! Tests of `snapshot_browse.rs`.
use super::*;
use crate::exec::privilege::{lookup, Kind};

#[test]
fn rejects_hostile_components() {
    for bad in ["", ".", "..", "a/b", "a\nb", "a\0b", "a\x1b[2J", "a\tb"] {
        assert!(!valid_component(bad), "{bad:?}");
    }
    assert!(valid_component(&"x".repeat(255)));
    assert!(!valid_component(&"x".repeat(256)));
    assert_eq!(join_relative(&["a", "..", "b"]), None);
    let long = vec!["x".repeat(200); 30];
    assert_eq!(join_relative(&long.iter().map(String::as_str).collect::<Vec<_>>()), None, "over 4096 bytes");
}

#[test]
fn accepts_ordinary_names() {
    for good in ["photo 1.jpg", "-leading-dash", "naïve", ".hidden", "a b c"] {
        assert!(valid_component(good), "{good:?}");
    }
    assert_eq!(join_relative(&["a", "b c"]).as_deref(), Some("a/b c"));
    assert_eq!(join_relative(&[]).as_deref(), Some(""));
}

#[test]
fn snapshot_names_follow_zfs_rules() {
    for good in ["daily-2026-10-20", "keep 1", "a:b", "x_y.z"] {
        assert!(valid_snapshot_name(good), "{good:?}");
    }
    for bad in ["", "a/b", "a@b", "..", ".", "x\ny", "a\u{1b}b", "é"] {
        assert!(!valid_snapshot_name(bad), "{bad:?}");
    }
    assert!(!valid_snapshot_name(&"x".repeat(256)));
}

#[test]
fn snapshot_dir_builds_the_path_and_refuses_the_rest() {
    assert_eq!(snapshot_dir("/scratch/data", "snap4", &["sub", "x y"]).unwrap(), "/scratch/data/.zfs/snapshot/snap4/sub/x y");
    assert_eq!(snapshot_dir("/scratch/data", "snap4", &[]).unwrap(), "/scratch/data/.zfs/snapshot/snap4");
    for bad in ["none", "legacy", "-", "relative/path", "/a/../b", "/a//b", "/a/", "", "/a\nb"] {
        assert!(snapshot_dir(bad, "s", &[]).is_err(), "mountpoint {bad:?}");
    }
    assert!(snapshot_dir("/m", "a/b", &[]).is_err());
    assert!(snapshot_dir("/m", "s", &[".."]).is_err());
    assert!(snapshot_dir("/m", "s", &["ok", "a\nb"]).is_err());
    let err = snapshot_dir("legacy", "s", &[]).unwrap_err().to_string();
    assert!(err.contains("not mounted"), "{err}");
}

/// Answers `zfs get` with canned text and records the arguments.
struct GetRunner {
    out: String,
    calls: Mutex<Vec<Vec<String>>>,
}

impl GetRunner {
    fn new(out: &str) -> Self {
        Self { out: out.to_string(), calls: Mutex::new(vec![]) }
    }
}

impl CommandRunner for GetRunner {
    fn run(&self, _cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
        self.calls.lock().unwrap().push(args.iter().map(|a| (*a).to_string()).collect());
        Ok(CommandOutput { stdout: self.out.clone(), stderr: String::new(), status: 0 })
    }
    fn run_with_stdin(&self, _cmd: &str, _args: &[&str], _stdin: &str) -> crate::error::Result<CommandOutput> {
        Err(AppError::Config("no stdin here".to_string()))
    }
    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
        self.run(cmd, args)
    }
    fn command(&self, _cmd: &str, _args: &[&str]) -> crate::error::Result<Command> {
        Err(AppError::Config("no command here".to_string()))
    }
}

#[test]
fn browse_mountpoint_needs_the_dataset_to_be_mounted_at_a_path() {
    let ok = GetRunner::new("mounted\tyes\nmountpoint\t/m\n");
    assert_eq!(browse_mountpoint(&ok, "tank/d").unwrap(), "/m");
    assert_eq!(ok.calls.lock().unwrap()[0], ["get", "-H", "-o", "property,value", "mounted,mountpoint", "tank/d"], "options first");
    for (out, why) in [
        ("mounted\tno\nmountpoint\t/m\n", "not mounted"),
        ("mounted\t-\nmountpoint\t/m\n", "not mounted"),
        ("mounted\tyes\nmountpoint\tlegacy\n", "not mounted at a path"),
        ("mounted\tyes\nmountpoint\tnone\n", "not mounted at a path"),
        ("mountpoint\t/m\n", "not mounted"),
    ] {
        let err = browse_mountpoint(&GetRunner::new(out), "tank/d").unwrap_err().to_string();
        assert!(err.contains(why), "{out:?}: {err}");
    }
}

/// Answers each command by its name and first arguments; every call is recorded as `cmd args...`.
struct ScriptedRunner {
    answers: Vec<(&'static str, String)>,
    calls: Mutex<Vec<String>>,
    /// The calls made through `run_unprivileged` (no sudo), by command name.
    unprivileged: Mutex<Vec<String>>,
}

impl ScriptedRunner {
    fn new(answers: &[(&'static str, &str)]) -> Self {
        Self { answers: answers.iter().map(|(k, v)| (*k, (*v).to_string())).collect(), calls: Mutex::new(vec![]), unprivileged: Mutex::new(vec![]) }
    }
}

impl CommandRunner for ScriptedRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
        let line = format!("{cmd} {}", args.join(" "));
        self.calls.lock().unwrap().push(line.clone());
        let out = self.answers.iter().find(|(prefix, _)| line.starts_with(prefix)).map(|(_, out)| out.clone());
        match out {
            Some(stdout) => Ok(CommandOutput { stdout, stderr: String::new(), status: 0 }),
            None => Ok(CommandOutput { stdout: String::new(), stderr: format!("no answer for {line}"), status: 1 }),
        }
    }
    fn run_with_stdin(&self, _cmd: &str, _args: &[&str], _stdin: &str) -> crate::error::Result<CommandOutput> {
        Err(AppError::Config("no stdin here".to_string()))
    }
    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
        self.unprivileged.lock().unwrap().push(cmd.to_string());
        self.run(cmd, args)
    }
    fn command(&self, _cmd: &str, _args: &[&str]) -> crate::error::Result<Command> {
        Err(AppError::Config("no command here".to_string()))
    }
}

#[test]
fn the_mountpoint_keeps_its_exact_text_including_spaces_at_the_end() {
    let r = ScriptedRunner::new(&[("zfs get", "mounted\tyes\nmountpoint\t/data/my dir \n")]);
    assert_eq!(browse_mountpoint(&r, "tank/d").unwrap(), "/data/my dir ");
}

#[test]
fn a_dataset_mounted_at_the_root_can_be_browsed() {
    assert_eq!(browse_mountpoint(&GetRunner::new("mounted\tyes\nmountpoint\t/\n"), "zroot/ROOT/default").unwrap(), "/");
    assert_eq!(snapshot_dir("/", "s", &["etc"]).unwrap(), "/.zfs/snapshot/s/etc");
    assert_eq!(snapshot_dir("/", "s", &[]).unwrap(), "/.zfs/snapshot/s");
}

#[test]
fn a_dataset_name_starting_with_a_dash_is_refused_before_anything_runs() {
    let r = GetRunner::new("");
    assert!(browse_mountpoint(&r, "-r").is_err());
    assert!(r.calls.lock().unwrap().is_empty());
}

#[test]
fn ls_args_match_the_pinned_forms_exactly() {
    let Some(crate::exec::privilege::Entry { kind: Kind::Privileged { pinned, .. }, .. }) = lookup("ls") else { panic!("no ls entry") };
    let dir = "/m/.zfs/snapshot/s";
    for times in TIMES {
        let args = ls_args(times, dir).expect("a form for every time");
        assert_eq!(args.last().map(String::as_str), Some(dir));
        assert_eq!(args[args.len() - 2], "--");
        let prefix = &args[..args.len() - 1];
        assert!(pinned.iter().any(|form| form[..form.len() - 1] == *prefix), "{args:?} is not a pinned form");
    }
    assert_eq!(ls_args(Times::Modified, dir).unwrap(), ["-lAb", "-D", "%s", "--", dir]);
    assert_eq!(ls_args(Times::Created, dir).unwrap(), ["-lAbU", "-D", "%s", "--", dir]);
}

const FREEBSD: &str = include_str!("../snapshot_browse_fixtures/freebsd.txt");

fn parse_all(text: &str) -> Vec<ListEntry> {
    text.lines().filter_map(parse_ls_line).collect()
}

#[test]
fn parses_freebsd_output() {
    let all = parse_all(FREEBSD);
    let dir = &all[0];
    assert_eq!((dir.name.as_str(), dir.kind, dir.mode.as_str(), dir.owner.as_str(), dir.group.as_str(), dir.size, dir.time), ("X11", EntryKind::Directory, "drwxr-xr-x", "root", "wheel", 2, Some(1_784_933_440)));
    let link = &all[1];
    assert_eq!((link.name.as_str(), link.kind, link.link_target.as_deref()), ("aliases", EntryKind::Symlink, Some("mail/aliases")));
    let file = all.iter().find(|e| e.name == "auto_master").unwrap();
    assert_eq!((file.kind, file.size, file.time), (EntryKind::File, 151, Some(1_785_266_797)));
}

const FREEBSD_ESC: &str = include_str!("../snapshot_browse_fixtures/freebsd_escaped.txt");

fn names_of(text: &str) -> Vec<(String, bool)> {
    parse_all(text).into_iter().map(|e| (e.name, e.ambiguous)).collect()
}

#[test]
fn escaped_names_decode() {
    let n = |s: &str, amb: bool| (s.to_string(), amb);
    // `ls -b` escapes the backslash itself, so nothing is ambiguous.
    let clear = vec![n("arrow -> name", false), n("back\\slash", false), n("caf\u{fffd}", false), n("lnk", false), n("nl\nx", false), n("quo\"te", false), n("sp ace", false), n("tab\ttab", false)];
    assert_eq!(names_of(FREEBSD_ESC), clear);
}

#[test]
fn a_symlink_with_a_spaced_target_decodes() {
    let link = parse_all(FREEBSD_ESC).into_iter().find(|e| e.name == "lnk").unwrap();
    assert_eq!((link.kind, link.link_target.as_deref()), (EntryKind::Symlink, Some("target with space")));
}

#[test]
fn a_forged_entry_inside_a_name_stays_one_entry_and_cannot_be_opened() {
    // The hostile name is: first line "evil", then a line that looks like another entry.
    let all = parse_all("-rw-r--r--  1 u g 1 100 evil\\n-rw-r--r-- 1 u g 5 100 fake");
    assert_eq!(all.len(), 1, "{all:?}");
    assert!(!valid_component(&all[0].name), "{:?}", all[0].name);
}

#[test]
fn octal_escapes_are_bytes_and_bad_escapes_are_kept_as_text() {
    assert_eq!(decode_escapes("a\\101b\\60"), "aAb0");
    assert_eq!(decode_escapes("\\\\x\\q\\"), "\\x\\q\\", "unknown escapes and a trailing backslash are kept");
    assert_eq!(decode_escapes("\\777"), "\\777", "an octal value above 255 is not a byte");
    assert_eq!(decode_escapes("\\303\\251"), "é", "two octal bytes that form UTF-8 decode to the character");
}

#[test]
fn a_name_with_spaces_and_an_arrow_in_it() {
    let e = parse_ls_line("-rw-r--r--  1 u g 5 1700000000 a -> b -> c").unwrap();
    assert_eq!((e.kind, e.name.as_str(), e.link_target), (EntryKind::File, "a -> b -> c", None), "only a symlink is split at the arrow");
    let e = parse_ls_line("-rw-r--r-- 1 u g 5 1700000000  two  spaces.txt").unwrap();
    assert_eq!(e.name, " two  spaces.txt", "the name keeps its own spaces");
    let e = parse_ls_line("lrwxr-xr-x  1 u g 3 1700000000 x y -> /etc/z w").unwrap();
    assert_eq!((e.name.as_str(), e.link_target.as_deref()), ("x y", Some("/etc/z w")));
}

#[test]
fn garbage_and_total_lines_are_none() {
    for bad in ["", "total 541", "drwx", "-rw-r--r-- 1 u g notanumber 1700000000 f", "-rw-r--r-- 1 u g 5 notatime f", "-rw-r--r-- 1 u g 5 1700000000", "    "] {
        assert_eq!(parse_ls_line(bad), None, "{bad:?}");
    }
}

#[test]
fn a_hostile_name_is_kept_as_text() {
    let e = parse_ls_line("-rw-r--r-- 1 u g 5 1700000000 \\033[2Jevil\\007").unwrap();
    assert_eq!(e.name, "\x1b[2Jevil\u{7}");
}

#[test]
fn folder_entries_is_size_minus_two() {
    let mk = |kind, size| ListEntry { name: "x".into(), kind, mode: String::new(), owner: String::new(), group: String::new(), size, time: None, link_target: None, ambiguous: false };
    assert_eq!(mk(EntryKind::Directory, 5).folder_entries(), Some(3));
    assert_eq!(mk(EntryKind::Directory, 2).folder_entries(), Some(0));
    assert_eq!(mk(EntryKind::Directory, 0).folder_entries(), Some(0));
    assert_eq!(mk(EntryKind::File, 5).folder_entries(), None);
    assert_eq!(EntryKind::Directory.word(), "folder");
    assert_eq!(EntryKind::Symlink.word(), "symlink");
}

use crate::error::AppError;
use crate::exec::{CommandOutput, CommandRunner};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DIR: &str = "/m/.zfs/snapshot/s";

/// Hands out `sh -c <script>` for each `ls` call in turn (the last script repeats) and records the arguments.
struct LsRunner {
    scripts: Vec<String>,
    spawned: Mutex<Vec<Vec<String>>>,
}

impl LsRunner {
    fn new(scripts: &[&str]) -> Self {
        Self { scripts: scripts.iter().map(|s| (*s).to_string()).collect(), spawned: Mutex::new(vec![]) }
    }
    fn spawned(&self) -> Vec<Vec<String>> {
        self.spawned.lock().unwrap().clone()
    }
}

impl CommandRunner for LsRunner {
    fn run(&self, _cmd: &str, _args: &[&str]) -> crate::error::Result<CommandOutput> {
        Err(AppError::Config("run is not used".to_string()))
    }
    fn run_with_stdin(&self, _cmd: &str, _args: &[&str], _stdin: &str) -> crate::error::Result<CommandOutput> {
        Err(AppError::Config("no stdin here".to_string()))
    }
    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
        self.run(cmd, args)
    }
    fn command(&self, cmd: &str, args: &[&str]) -> crate::error::Result<Command> {
        assert_eq!(cmd, "ls");
        let mut spawned = self.spawned.lock().unwrap();
        let script = self.scripts.get(spawned.len()).or(self.scripts.last()).cloned().unwrap_or_default();
        spawned.push(args.iter().map(|a| (*a).to_string()).collect());
        let mut c = Command::new("sh");
        c.args(["-c", &script]);
        Ok(c)
    }
}

fn files(names: &[&str], epoch: u64) -> String {
    use std::fmt::Write;
    let mut script = "echo 'total 3'; ".to_string();
    for n in names {
        let _ = write!(script, "printf '%s\\n' '-rw-r--r-- 1 u g 5 {epoch} {n}'; ");
    }
    script
}

#[test]
fn read_folder_joins_modified_changed_and_created() {
    let r = LsRunner::new(&[&files(&["a", "b"], 100), &files(&["a", "b"], 200), &files(&["a", "b"], 300)]);
    let l = read_folder(&r, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap();
    let times: Vec<_> = l.entries.iter().map(|r| (r.entry.name.as_str(), r.entry.time, r.changed, r.created)).collect();
    assert_eq!(times, [("a", Some(100), Some(200), Some(300)), ("b", Some(100), Some(200), Some(300))]);
    assert!(l.has_created && !l.truncated && l.unreadable == 0);
    let spawned = r.spawned();
    assert_eq!(spawned[0], ["-lAb", "-D", "%s", "--", DIR]);
    assert_eq!(spawned[1], ["-lAbc", "-D", "%s", "--", DIR]);
    assert_eq!(spawned[2], ["-lAbU", "-D", "%s", "--", DIR]);
}

#[test]
fn the_entry_cap_stops_the_read_and_says_so() {
    let r = LsRunner::new(&[&format!("{} sleep 30", files(&["a", "b", "c", "d", "e"], 1))]);
    let limits = BrowseLimits { max_entries: 3, ..BrowseLimits::default() };
    let started = Instant::now();
    let l = read_folder(&r, DIR, &limits, &AtomicBool::new(false)).unwrap();
    assert_eq!((l.entries.len(), l.truncated), (3, true));
    assert!(started.elapsed() < Duration::from_secs(10), "the child was killed, not waited for");
}

#[test]
fn exactly_the_cap_is_not_truncated() {
    let r = LsRunner::new(&[&files(&["a", "b", "c"], 1)]);
    let limits = BrowseLimits { max_entries: 3, ..BrowseLimits::default() };
    let l = read_folder(&r, DIR, &limits, &AtomicBool::new(false)).unwrap();
    assert_eq!((l.entries.len(), l.truncated), (3, false));
}

#[test]
fn the_time_limit_stops_the_read() {
    let r = LsRunner::new(&[&format!("{} sleep 30", files(&["a"], 1))]);
    let limits = BrowseLimits { max_seconds: 1, ..BrowseLimits::default() };
    let started = Instant::now();
    let l = read_folder(&r, DIR, &limits, &AtomicBool::new(false)).unwrap();
    assert_eq!((l.entries.len(), l.truncated), (1, true));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn cancel_is_an_error_and_returns_quickly() {
    let r = LsRunner::new(&["sleep 30"]);
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let result = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        read_folder(&r, DIR, &BrowseLimits::default(), &cancel)
    });
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_failing_first_call_is_an_error_with_the_first_stderr_line() {
    let r = LsRunner::new(&["echo 'ls: /x: Permission denied' >&2; echo second >&2; exit 2"]);
    let err = read_folder(&r, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap_err().to_string();
    assert!(err.contains("Permission denied") && !err.contains("second"), "{err}");
}

#[test]
fn a_changed_or_created_listing_cut_short_by_the_time_limit_is_reported() {
    let r = LsRunner::new(&[&files(&["a", "b"], 100), &format!("{} sleep 30", files(&["a"], 200))]);
    let limits = BrowseLimits { max_seconds: 2, ..BrowseLimits::default() };
    let started = Instant::now();
    let l = read_folder(&r, DIR, &limits, &AtomicBool::new(false)).unwrap();
    assert!(l.times_incomplete, "the listing of times was cut off");
    assert!(!l.truncated, "the folder itself was read completely");
    assert_eq!(l.entries.len(), 2);
    assert!(started.elapsed() < Duration::from_secs(10));
    let complete = read_folder(&LsRunner::new(&[&files(&["a"], 1)]), DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap();
    assert!(!complete.times_incomplete);
}

#[test]
fn a_mismatched_second_listing_leaves_changed_empty_not_wrong() {
    let r = LsRunner::new(&[&files(&["a", "b"], 100), &files(&["a", "zzz"], 200), "exit 3"]);
    let l = read_folder(&r, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap();
    assert_eq!((l.entries[0].changed, l.entries[1].changed), (Some(200), None));
    assert_eq!((l.entries[0].created, l.entries[1].created), (None, None), "a failing later listing leaves the time unknown");
}

#[test]
fn unparsable_lines_before_any_entry_are_counted_not_shown() {
    let r = LsRunner::new(&[&format!("echo 'garbage line'; echo 'more garbage'; {}", files(&["a"], 1))]);
    let l = read_folder(&r, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap();
    assert_eq!((l.entries.len(), l.unreadable), (1, 2));
}

#[test]
fn a_stray_line_is_counted_not_glued_onto_the_entry_before_it() {
    let script = "echo 'total 3'; echo '-rw-r--r-- 1 u g 5 100 first'; echo 'newline'; echo '-rw-r--r-- 1 u g 5 100 last'";
    let r = LsRunner::new(&[script, script]);
    let l = read_folder(&r, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap();
    let names: Vec<&str> = l.entries.iter().map(|r| r.entry.name.as_str()).collect();
    assert_eq!(names, ["first", "last"], "no entry is altered by the line next to it");
    assert_eq!(l.unreadable, 1);
}

#[test]
fn a_name_with_a_newline_arrives_escaped_as_one_entry_that_cannot_be_opened() {
    let script = "echo 'total 3'; echo '-rw-r--r-- 1 u g 5 100 first'; echo 'drwxr-xr-x 2 u g 5 100 dir\\nnewline'; echo '-rw-r--r-- 1 u g 5 100 last'";
    let r = LsRunner::new(&[script, script]);
    let l = read_folder(&r, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap();
    let names: Vec<&str> = l.entries.iter().map(|r| r.entry.name.as_str()).collect();
    assert_eq!(names, ["first", "dir\nnewline", "last"]);
    assert_eq!(l.unreadable, 0);
    assert!(!valid_component(&l.entries[1].entry.name), "so the folder is never opened under a wrong name");
    assert_eq!(l.entries[1].changed, Some(100), "the later listings agree, so the times still match");
}

#[test]
fn a_line_that_looks_like_an_entry_but_does_not_parse_is_counted_not_glued_onto_the_entry_before_it() {
    // An owner name with a space (a Windows domain account) breaks the column split, but it is a real entry.
    let script = "echo '-rw-r--r-- 1 u g 5 100 first'; echo '-rw-r--r-- 1 DOMAIN\\\\my user g 5 100 second'; echo '-rw-r--r-- 1 u g 5 100 third'";
    let r = LsRunner::new(&[script]);
    let l = read_folder(&r, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap();
    let names: Vec<&str> = l.entries.iter().map(|r| r.entry.name.as_str()).collect();
    assert_eq!(names, ["first", "third"], "the real entry is not welded onto its neighbour's name");
    assert_eq!(l.unreadable, 1);
}

#[test]
fn a_name_that_is_not_utf8_does_not_cut_the_folder_short_and_cannot_be_opened() {
    let script = "echo 'total 3'; echo '-rw-r--r-- 1 u g 5 100 a'; printf -- '-rw-r--r-- 1 u g 5 100 caf\\351\\n'; echo '-rw-r--r-- 1 u g 5 100 z'";
    let r = LsRunner::new(&[script]);
    let l = read_folder(&r, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap();
    let names: Vec<&str> = l.entries.iter().map(|r| r.entry.name.as_str()).collect();
    assert_eq!(names, ["a", "caf\u{fffd}", "z"], "entries after the odd name are still listed");
    assert!(!valid_component(names[1]), "a name that did not survive as text is never sent back");
}

#[test]
fn a_directory_that_was_not_built_by_snapshot_dir_is_refused_before_anything_runs() {
    let r = LsRunner::new(&["true"]);
    for bad in ["/etc", "/m/.zfs/snapshot/s\nfoo", "relative/.zfs/snapshot/s", "/m/.zfs/snapshot/s/\u{1b}x"] {
        assert!(read_folder(&r, bad, &BrowseLimits::default(), &AtomicBool::new(false)).is_err(), "{bad:?}");
    }
    assert!(r.spawned().is_empty());
}

#[test]
fn explain_names_the_sudoers_command_when_sudo_refuses() {
    for text in ["sudo: a password is required", "Sorry, user ub is not allowed to execute '/usr/bin/ls -lA -- /x' as root", "sudo: no tty present and no askpass program specified"] {
        let msg = explain("ub24", text);
        assert!(msg.contains("browse is not enabled") && msg.contains("print-sudoers ub24 --with browse"), "{msg}");
    }
}

#[test]
fn explain_does_not_blame_zones_for_a_missing_folder() {
    let msg = explain("h", "ls: /m/.zfs/snapshot/s/sub: No such file or directory");
    assert!(msg.contains("cannot be browsed here") && !msg.contains("zone"), "{msg}");
}

#[test]
fn explain_handles_the_missing_snapshot_folder_and_other_errors() {
    let msg = explain("h", "ls: /m/.zfs/snapshot/s: No such file or directory");
    assert!(msg.contains("cannot be browsed here") && msg.contains("does not clone"), "{msg}");
    assert_eq!(explain("h", "weird \u{1b}[31m failure"), "weird [31m failure");
}

fn row(name: &str, kind: EntryKind, size: u64, time: Option<i64>) -> Row {
    Row { entry: ListEntry { name: name.into(), kind, mode: String::new(), owner: String::new(), group: String::new(), size, time, link_target: None, ambiguous: false }, changed: None, created: None }
}

#[test]
fn size_ranges_parse_with_units() {
    assert_eq!(parse_size_range("10M.."), Some((Some(10 << 20), None)));
    assert_eq!(parse_size_range("..1G"), Some((None, Some(1 << 30))));
    assert_eq!(parse_size_range("1K..1M"), Some((Some(1024), Some(1 << 20))));
    assert_eq!(parse_size_range("1k..1m"), parse_size_range("1K..1M"));
    assert_eq!(parse_size_range("500"), Some((Some(500), Some(500))));
    assert_eq!(parse_size_range("2KB..3 K"), None, "no spaces inside a number");
    assert_eq!(parse_size_range("2KB..3K"), Some((Some(2048), Some(3072))));
    for bad in ["", "..", "5G..1G", "abc", "1X", "-5..", "1..2..3", "99999999999999999999T.."] {
        assert_eq!(parse_size_range(bad), None, "{bad:?}");
    }
}

#[test]
fn date_ranges_parse_in_the_hosts_zone_and_a_day_is_inclusive() {
    // 2026-10-05 00:00:00 UTC is 1791158400; a host at UTC-4 starts that day 4 hours later.
    assert_eq!(parse_date_range("2026-10-05", 0), Some((Some(1_791_158_400), Some(1_791_158_400 + 86_399))));
    assert_eq!(parse_date_range("2026-10-05", -4 * 3600), Some((Some(1_791_158_400 + 4 * 3600), Some(1_791_158_400 + 4 * 3600 + 86_399))));
    assert_eq!(parse_date_range("2026-10-01..", 0), Some((Some(1_790_812_800), None)));
    assert_eq!(parse_date_range("..2026-10-05", 0), Some((None, Some(1_791_158_400 + 86_399))));
    assert_eq!(parse_date_range("2026-10-01..2026-10-05", 0), Some((Some(1_790_812_800), Some(1_791_158_400 + 86_399))));
    for bad in ["", "..", "2026-02-30", "2026-10-05..2026-10-01", "yesterday", "2026-10", "2026-10-01..x"] {
        assert_eq!(parse_date_range(bad, 0), None, "{bad:?}");
    }
}

#[test]
fn filters_narrow_by_name_kind_size_and_date() {
    let f = row("Photo.JPG", EntryKind::File, 5000, Some(100));
    let d = row("pics", EntryKind::Directory, 7, Some(100));
    let l = row("link", EntryKind::Symlink, 4, Some(100));
    assert!(Filter::default().matches(&f) && Filter::default().is_empty());
    let name = Filter { name: crate::zfs::name_filter::NameFilter::new("photo"), ..Filter::default() };
    assert!(name.matches(&f) && !name.matches(&d) && !name.is_empty());
    let special = Filter { name: crate::zfs::name_filter::NameFilter::new("%*\\"), ..Filter::default() };
    assert!(!special.matches(&f), "wildcards are plain characters");
    let size = Filter { size_min: Some(1000), ..Filter::default() };
    assert!(size.matches(&f) && !size.matches(&l) && size.matches(&d), "a size filter never hides a folder");
    let date = Filter { modified_from: Some(200), ..Filter::default() };
    assert!(!date.matches(&f) && date.matches(&d));
    let unknown = row("x", EntryKind::File, 1, None);
    assert!(!date.matches(&unknown), "an unknown time does not satisfy a date range");
    let links = Filter { kind: KindFilter::Links, ..Filter::default() };
    assert!(links.matches(&l) && !links.matches(&f) && !links.matches(&d));
    let folders = Filter { kind: KindFilter::Folders, name: crate::zfs::name_filter::NameFilter::new("PIC"), ..Filter::default() };
    assert!(folders.matches(&d) && !folders.matches(&f));
    let files = Filter { kind: KindFilter::Files, ..Filter::default() };
    assert!(files.matches(&f) && !files.matches(&d) && !files.matches(&l));
}

#[test]
fn rows_sort_folders_first_then_by_name_ignoring_case() {
    let mut rows = vec![row("b.txt", EntryKind::File, 1, None), row("Zed", EntryKind::Directory, 2, None), row("A.txt", EntryKind::File, 1, None), row("alpha", EntryKind::Directory, 2, None)];
    sort_rows(&mut rows);
    assert_eq!(rows.iter().map(|r| r.entry.name.as_str()).collect::<Vec<_>>(), ["alpha", "Zed", "A.txt", "b.txt"]);
}
