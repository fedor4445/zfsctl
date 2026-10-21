//! Looking inside a snapshot: the directory path of a snapshot's folder, the `ls` command lines that list it, and what they print.

use crate::daemon::transfer::run_lines;
use crate::error::{AppError, Result};
use crate::exec::privilege::{lookup, Kind};
use crate::exec::CommandRunner;
use crate::zfs::host_health::HostOs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The longest path zfsctl will send to a host.
const MAX_PATH: usize = 4096;
/// The longest single file or folder name.
const MAX_COMPONENT: usize = 255;

/// Whether `name` may be one step of a path zfsctl builds: 1 to 255 bytes, not `.` or `..`, no `/`, no control character
/// (a newline or an escape in a name is shown but never sent back to a host).
pub fn valid_component(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_COMPONENT && name != "." && name != ".." && !name.contains('/') && !name.chars().any(char::is_control)
}

/// `a/b/c` from valid steps; `None` when a step is invalid or the whole is longer than 4096 bytes. No steps give `""`.
pub fn join_relative(parts: &[&str]) -> Option<String> {
    if !parts.iter().all(|p| valid_component(p)) {
        return None;
    }
    let joined = parts.join("/");
    (joined.len() <= MAX_PATH).then_some(joined)
}

/// ZFS's rule for the part of a snapshot name after `@`: letters, digits and `_ . : - ` and space, 1 to 255 bytes, not `.` or `..`.
pub fn valid_snapshot_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_COMPONENT && name != "." && name != ".." && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-' | ' '))
}

/// `<mountpoint>/.zfs/snapshot/<snapshot>[/<step>...]`, after validating every part. A dataset that is not mounted at a path
/// (`none`, `legacy`, `-`) has no snapshot folder to list.
pub fn snapshot_dir(mountpoint: &str, snapshot: &str, relative: &[&str]) -> Result<String> {
    if matches!(mountpoint, "none" | "legacy" | "-" | "" | "/") {
        return Err(AppError::Config(format!("this dataset is not mounted at a path (mountpoint '{}'), so there is no snapshot folder to browse", crate::display_text::sanitise_for_display(mountpoint))));
    }
    let Some(rest) = mountpoint.strip_prefix('/') else {
        return Err(AppError::Config("the dataset's mountpoint is not an absolute path, so it cannot be browsed".to_string()));
    };
    let steps: Vec<&str> = rest.split('/').collect();
    if join_relative(&steps).is_none() {
        return Err(AppError::Config("the dataset's mountpoint is not a plain path, so it cannot be browsed".to_string()));
    }
    if !valid_snapshot_name(snapshot) {
        return Err(AppError::Config("the snapshot's name is not a valid ZFS snapshot name".to_string()));
    }
    let Some(inside) = join_relative(relative) else {
        return Err(AppError::Config("that folder's name cannot be sent to the host (it has a control character, a slash or is too long)".to_string()));
    };
    let mut dir = format!("{mountpoint}/.zfs/snapshot/{snapshot}");
    if !inside.is_empty() {
        dir.push('/');
        dir.push_str(&inside);
    }
    if dir.len() > MAX_PATH {
        return Err(AppError::Config("that path is longer than 4096 bytes".to_string()));
    }
    Ok(dir)
}

/// Which of a file's times a listing prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Times {
    Modified,
    Changed,
    Created,
}

/// The listings to read for a host: illumos records no creation time that `ls` can print.
pub fn times_for(os: HostOs) -> &'static [Times] {
    if os == HostOs::Illumos { &[Times::Modified, Times::Changed] } else { &[Times::Modified, Times::Changed, Times::Created] }
}

/// The arguments of one listing: a pinned `ls` form from the registry (so what is sent and what sudoers allows cannot drift),
/// then `--` and the directory. `None` for a time the OS does not record.
pub fn ls_args(os: HostOs, times: Times, dir: &str) -> Option<Vec<String>> {
    let Kind::Privileged { pinned, .. } = &lookup("ls")?.kind else { return None };
    let (first, count) = match os {
        HostOs::Linux => (3, 3),
        HostOs::Illumos => (6, 2),
        HostOs::FreeBsd | HostOs::Other => (0, 3),
    };
    let offset = match times {
        Times::Modified => 0,
        Times::Changed => 1,
        Times::Created => 2,
    };
    if offset >= count {
        return None;
    }
    let form = pinned.get(first + offset)?;
    let (_star, words) = form.split_last()?;
    let mut args: Vec<String> = words.iter().map(|a| (*a).to_string()).collect();
    args.push(dir.to_string());
    Some(args)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Block,
    Character,
    Pipe,
    Socket,
    Door,
    Port,
    Other,
}

impl EntryKind {
    pub fn word(self) -> &'static str {
        match self {
            EntryKind::File => "file",
            EntryKind::Directory => "folder",
            EntryKind::Symlink => "symlink",
            EntryKind::Block => "block device",
            EntryKind::Character => "character device",
            EntryKind::Pipe => "pipe",
            EntryKind::Socket => "socket",
            EntryKind::Door => "door",
            EntryKind::Port => "event port",
            EntryKind::Other => "other",
        }
    }

    fn from_mode_char(c: char) -> Self {
        match c {
            '-' => EntryKind::File,
            'd' => EntryKind::Directory,
            'l' => EntryKind::Symlink,
            'b' => EntryKind::Block,
            'c' => EntryKind::Character,
            'p' => EntryKind::Pipe,
            's' => EntryKind::Socket,
            'D' => EntryKind::Door,
            'P' => EntryKind::Port,
            _ => EntryKind::Other,
        }
    }
}

/// One line of `ls -l`: what the host printed, as text and numbers. The name is the host's, so it must be sanitised before it is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    pub name: String,
    pub kind: EntryKind,
    pub mode: String,
    pub owner: String,
    pub group: String,
    /// Bytes for a file; for a folder the raw ZFS size (its entries plus 2); 0 for a device.
    pub size: u64,
    /// Seconds since 1970 (UTC) of the time this listing printed.
    pub time: Option<i64>,
    pub link_target: Option<String>,
}

impl ListEntry {
    /// How many entries a folder holds: ZFS reports a directory's size as its entry count plus 2.
    pub fn folder_entries(&self) -> Option<u64> {
        (self.kind == EntryKind::Directory).then(|| self.size.saturating_sub(2))
    }
}

/// The next whitespace-delimited word and the text after it (starting at the whitespace that ended the word).
fn word(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start();
    if text.is_empty() {
        return None;
    }
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    Some((text.get(..end)?, text.get(end..)?))
}

/// Parses one `ls -lA` line as printed by [`ls_args`]. `None` for the `total` line and anything that is not an entry.
pub fn parse_ls_line(os: HostOs, line: &str) -> Option<ListEntry> {
    let (mode, rest) = word(line)?;
    let kind = EntryKind::from_mode_char(mode.chars().next()?);
    if mode.len() < 10 || (kind == EntryKind::Other && !mode.starts_with('?')) {
        return None;
    }
    let (_links, rest) = word(rest)?;
    let (owner, rest) = word(rest)?;
    let (group, rest) = word(rest)?;
    let (size_word, mut rest) = word(rest)?;
    let size = if matches!(kind, EntryKind::Block | EntryKind::Character) {
        // A device prints "major, minor" where the size would be.
        size_word.strip_suffix(',')?.parse::<u64>().ok()?;
        let (_minor, after) = word(rest)?;
        rest = after;
        0
    } else {
        size_word.parse::<u64>().ok()?
    };
    let time = if os == HostOs::Illumos {
        let (date, r1) = word(rest)?;
        let (clock, r2) = word(r1)?;
        let (zone, r3) = word(r2)?;
        rest = r3;
        chrono::DateTime::parse_from_str(&format!("{date} {clock} {zone}"), "%Y-%m-%d %H:%M:%S%.f %z").ok()?.timestamp()
    } else {
        let (epoch, r1) = word(rest)?;
        rest = r1;
        epoch.parse::<i64>().ok()?
    };
    let text = rest.strip_prefix(' ').unwrap_or(rest);
    if text.is_empty() {
        return None;
    }
    let (name, link_target) = match text.split_once(" -> ") {
        Some((name, target)) if kind == EntryKind::Symlink => (name.to_string(), Some(target.to_string())),
        _ => (text.to_string(), None),
    };
    Some(ListEntry { name, kind, mode: mode.to_string(), owner: owner.to_string(), group: group.to_string(), size, time: Some(time), link_target })
}

/// Where a folder read stops: the most entries kept and the most seconds all of its listings may take together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseLimits {
    pub max_entries: usize,
    pub max_seconds: u64,
}

impl Default for BrowseLimits {
    fn default() -> Self {
        Self { max_entries: 20_000, max_seconds: 30 }
    }
}

/// An entry with the three times of its listings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub entry: ListEntry,
    pub changed: Option<i64>,
    pub created: Option<i64>,
}

/// One folder as read: its entries, whether the read stopped early, how many lines could not be understood, and whether
/// this OS records a creation time at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<Row>,
    pub truncated: bool,
    pub unreadable: usize,
    pub has_created: bool,
}

struct Once {
    entries: Vec<ListEntry>,
    truncated: bool,
    unreadable: usize,
    failure: Option<(i32, String)>,
}

/// Runs one `ls`, keeping at most `max_entries` parsed entries; the child is killed at the entry cap, the deadline or `cancel`.
fn list_once(runner: &dyn CommandRunner, args: &[String], max_entries: usize, deadline: Instant, os: HostOs, cancel: &AtomicBool) -> Result<Once> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let cmd = runner.command("ls", &words)?;
    let stop = AtomicBool::new(false);
    let finish = AtomicBool::new(false);
    let timed_out = AtomicBool::new(false);
    let cancelled = AtomicBool::new(false);
    let mut once = Once { entries: Vec::new(), truncated: false, unreadable: 0, failure: None };
    let ran = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !finish.load(Ordering::SeqCst) {
                if cancel.load(Ordering::SeqCst) {
                    cancelled.store(true, Ordering::SeqCst);
                    stop.store(true, Ordering::SeqCst);
                    break;
                }
                if Instant::now() >= deadline {
                    timed_out.store(true, Ordering::SeqCst);
                    stop.store(true, Ordering::SeqCst);
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let ran = run_lines(cmd, &stop, |line| {
            if stop.load(Ordering::SeqCst) || line.starts_with("total ") {
                return;
            }
            match parse_ls_line(os, line) {
                Some(_) if once.entries.len() >= max_entries => {
                    once.truncated = true;
                    stop.store(true, Ordering::SeqCst);
                }
                Some(entry) => once.entries.push(entry),
                None if line.trim().is_empty() => {}
                None => once.unreadable += 1,
            }
        });
        finish.store(true, Ordering::SeqCst);
        ran
    });
    let lines = ran.map_err(|source| AppError::Spawn { cmd: "ls".to_string(), source })?;
    if cancelled.load(Ordering::SeqCst) {
        return Err(AppError::Config("cancelled".to_string()));
    }
    if timed_out.load(Ordering::SeqCst) {
        once.truncated = true;
    } else if !once.truncated && lines.code != Some(0) {
        let first = lines.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no message").to_string();
        once.failure = Some((lines.code.unwrap_or(-1), first));
    }
    Ok(once)
}

/// Reads one folder of a snapshot (`dir` comes from [`snapshot_dir`]): the modified-time listing decides the entries, the
/// changed- and created-time listings are matched to them line by line and left unknown where they disagree.
pub fn read_folder(runner: &dyn CommandRunner, os: HostOs, dir: &str, limits: &BrowseLimits, cancel: &AtomicBool) -> Result<Listing> {
    if !dir.starts_with('/') || !dir.contains("/.zfs/snapshot/") || dir.len() > MAX_PATH || dir.chars().any(char::is_control) {
        return Err(AppError::Config("refusing to list a folder that is not inside a snapshot".to_string()));
    }
    let deadline = Instant::now() + Duration::from_secs(limits.max_seconds);
    let first_args = ls_args(os, Times::Modified, dir).ok_or_else(|| AppError::Config("no ls form for this host".to_string()))?;
    let first = list_once(runner, &first_args, limits.max_entries, deadline, os, cancel)?;
    if let Some((status, stderr)) = &first.failure {
        if first.entries.is_empty() {
            return Err(AppError::CommandFailed { cmd: "ls".to_string(), status: *status, stderr: stderr.clone() });
        }
    }
    let unreadable = first.unreadable + usize::from(first.failure.is_some());
    let mut rows: Vec<Row> = first.entries.into_iter().map(|entry| Row { entry, changed: None, created: None }).collect();
    for times in times_for(os).iter().copied().filter(|t| *t != Times::Modified) {
        let Some(args) = ls_args(os, times, dir) else { continue };
        let other = list_once(runner, &args, limits.max_entries, deadline, os, cancel)?;
        if other.failure.is_some() {
            continue;
        }
        for (row, entry) in rows.iter_mut().zip(&other.entries) {
            if entry.name == row.entry.name && entry.kind == row.entry.kind {
                match times {
                    Times::Changed => row.changed = entry.time,
                    Times::Created => row.created = entry.time,
                    Times::Modified => {}
                }
            }
        }
    }
    Ok(Listing { entries: rows, truncated: first.truncated, unreadable, has_created: times_for(os).contains(&Times::Created) })
}

/// What to tell the user about a failed listing on `host`: the sudoers entry that is missing, a snapshot folder that is not
/// visible, or the host's own words (sanitised).
pub fn explain(host: &str, error_text: &str) -> String {
    let lower = error_text.to_lowercase();
    if ["password is required", "not allowed to execute", "no tty present", "may not run sudo"].iter().any(|p| lower.contains(p)) {
        return format!("browse is not enabled for this host: run zfsctl print-sudoers {host} --with browse, install the file as the host's sudoers entry and try again");
    }
    if lower.contains("no such file or directory") {
        return "this snapshot cannot be browsed here: its .zfs/snapshot folder is not visible on the host (for example a dataset given to a zone); zfsctl does not clone to work around that".to_string();
    }
    crate::display_text::sanitise_for_display(error_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::privilege::{lookup, Kind};
    use crate::zfs::host_health::HostOs;

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
        for bad in ["/", "none", "legacy", "-", "relative/path", "/a/../b", "/a//b", "/a/", "", "/a\nb"] {
            assert!(snapshot_dir(bad, "s", &[]).is_err(), "mountpoint {bad:?}");
        }
        assert!(snapshot_dir("/m", "a/b", &[]).is_err());
        assert!(snapshot_dir("/m", "s", &[".."]).is_err());
        assert!(snapshot_dir("/m", "s", &["ok", "a\nb"]).is_err());
        let err = snapshot_dir("legacy", "s", &[]).unwrap_err().to_string();
        assert!(err.contains("not mounted"), "{err}");
    }

    #[test]
    fn ls_args_match_the_pinned_forms_exactly() {
        let Some(crate::exec::privilege::Entry { kind: Kind::Privileged { pinned, .. }, .. }) = lookup("ls") else { panic!("no ls entry") };
        let dir = "/m/.zfs/snapshot/s";
        let mut used = 0;
        for os in [HostOs::FreeBsd, HostOs::Linux, HostOs::Illumos, HostOs::Other] {
            for times in [Times::Modified, Times::Changed, Times::Created] {
                let Some(args) = ls_args(os, times, dir) else {
                    assert_eq!((os, times), (HostOs::Illumos, Times::Created));
                    continue;
                };
                assert_eq!(args.last().map(String::as_str), Some(dir));
                assert_eq!(args[args.len() - 2], "--");
                let prefix = &args[..args.len() - 1];
                assert!(pinned.iter().any(|form| form[..form.len() - 1] == *prefix), "{args:?} is not a pinned form");
                used += 1;
            }
        }
        assert_eq!(used, 11);
        assert_eq!(ls_args(HostOs::Illumos, Times::Changed, dir).unwrap(), ["-lAEc", "--", dir]);
        assert_eq!(ls_args(HostOs::Linux, Times::Created, dir).unwrap(), ["-lA", "--time-style", "+%s", "--time", "birth", "--", dir]);
        assert_eq!(ls_args(HostOs::Other, Times::Modified, dir).unwrap(), ["-lA", "-D", "%s", "--", dir]);
        assert_eq!(times_for(HostOs::Illumos), [Times::Modified, Times::Changed]);
        assert_eq!(times_for(HostOs::Linux), [Times::Modified, Times::Changed, Times::Created]);
    }

    const FREEBSD: &str = include_str!("snapshot_browse_fixtures/freebsd.txt");
    const LINUX: &str = include_str!("snapshot_browse_fixtures/linux.txt");
    const ILLUMOS: &str = include_str!("snapshot_browse_fixtures/illumos.txt");

    fn parse_all(os: HostOs, text: &str) -> Vec<ListEntry> {
        text.lines().filter_map(|l| parse_ls_line(os, l)).collect()
    }

    #[test]
    fn parses_freebsd_output() {
        let all = parse_all(HostOs::FreeBsd, FREEBSD);
        let dir = &all[0];
        assert_eq!((dir.name.as_str(), dir.kind, dir.mode.as_str(), dir.owner.as_str(), dir.group.as_str(), dir.size, dir.time), ("X11", EntryKind::Directory, "drwxr-xr-x", "root", "wheel", 2, Some(1_784_933_440)));
        let link = &all[1];
        assert_eq!((link.name.as_str(), link.kind, link.link_target.as_deref()), ("aliases", EntryKind::Symlink, Some("mail/aliases")));
        let file = all.iter().find(|e| e.name == "auto_master").unwrap();
        assert_eq!((file.kind, file.size, file.time), (EntryKind::File, 151, Some(1_785_266_797)));
    }

    #[test]
    fn parses_linux_output_including_devices_and_symlinks() {
        let all = parse_all(HostOs::Linux, LINUX);
        let file = &all[0];
        assert_eq!((file.name.as_str(), file.kind, file.size, file.owner.as_str(), file.group.as_str(), file.time), ("adduser.conf", EntryKind::File, 3444, "root", "root", Some(1_688_578_959)));
        assert_eq!(all[1].kind, EntryKind::Directory);
        let link = all.iter().find(|e| e.name == "mtab").unwrap();
        assert_eq!((link.kind, link.link_target.as_deref()), (EntryKind::Symlink, Some("../proc/self/mounts")));
        let dev = all.iter().find(|e| e.name == "btrfs-control").unwrap();
        assert_eq!((dev.kind, dev.size, dev.group.as_str(), dev.time), (EntryKind::Character, 0, "disk", Some(1_791_711_741)));
    }

    #[test]
    fn parses_illumos_output_and_turns_offsets_into_utc() {
        let all = parse_all(HostOs::Illumos, ILLUMOS);
        let file = &all[0];
        assert_eq!((file.name.as_str(), file.size, file.time), (".cpr_config", 2236, Some(1_792_488_330)));
        let link = all.iter().find(|e| e.name == ".syslog_door").unwrap();
        assert_eq!(link.link_target.as_deref(), Some("../var/run/syslog_door"));
        let dev = all.iter().find(|e| e.name == "acpippm@0:acpi-ppm").unwrap();
        assert_eq!((dev.kind, dev.size), (EntryKind::Character, 0));
    }

    #[test]
    fn a_name_with_spaces_and_an_arrow_in_it() {
        let e = parse_ls_line(HostOs::FreeBsd, "-rw-r--r--  1 u g 5 1700000000 a -> b -> c").unwrap();
        assert_eq!((e.kind, e.name.as_str(), e.link_target), (EntryKind::File, "a -> b -> c", None), "only a symlink is split at the arrow");
        let e = parse_ls_line(HostOs::Linux, "-rw-r--r-- 1 u g 5 1700000000  two  spaces.txt").unwrap();
        assert_eq!(e.name, " two  spaces.txt", "exactly one separating space is dropped");
        let e = parse_ls_line(HostOs::FreeBsd, "lrwxr-xr-x  1 u g 3 1700000000 x y -> /etc/z w").unwrap();
        assert_eq!((e.name.as_str(), e.link_target.as_deref()), ("x y", Some("/etc/z w")));
    }

    #[test]
    fn garbage_and_total_lines_are_none() {
        for bad in ["", "total 541", "drwx", "-rw-r--r-- 1 u g notanumber 1700000000 f", "-rw-r--r-- 1 u g 5 notatime f", "-rw-r--r-- 1 u g 5 1700000000", "    "] {
            assert_eq!(parse_ls_line(HostOs::FreeBsd, bad), None, "{bad:?}");
        }
        assert_eq!(parse_ls_line(HostOs::Illumos, "-rw-r--r-- 1 u g 5 2026-13-45 99:99:99.0 -0400 f"), None);
        assert_eq!(parse_ls_line(HostOs::Illumos, "-rw-r--r-- 1 u g 5 1700000000 f"), None, "illumos lines carry a date, not an epoch");
    }

    #[test]
    fn a_hostile_name_is_kept_as_text() {
        let e = parse_ls_line(HostOs::Linux, "-rw-r--r-- 1 u g 5 1700000000 \x1b[2Jevil\u{7}").unwrap();
        assert_eq!(e.name, "\x1b[2Jevil\u{7}");
    }

    #[test]
    fn folder_entries_is_size_minus_two() {
        let mk = |kind, size| ListEntry { name: "x".into(), kind, mode: String::new(), owner: String::new(), group: String::new(), size, time: None, link_target: None };
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
        let mut script = "echo 'total 3'; ".to_string();
        for n in names {
            script.push_str(&format!("printf '%s\\n' '-rw-r--r-- 1 u g 5 {epoch} {n}'; "));
        }
        script
    }

    #[test]
    fn read_folder_joins_modified_changed_and_created() {
        let r = LsRunner::new(&[&files(&["a", "b"], 100), &files(&["a", "b"], 200), &files(&["a", "b"], 300)]);
        let l = read_folder(&r, HostOs::FreeBsd, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap();
        let times: Vec<_> = l.entries.iter().map(|r| (r.entry.name.as_str(), r.entry.time, r.changed, r.created)).collect();
        assert_eq!(times, [("a", Some(100), Some(200), Some(300)), ("b", Some(100), Some(200), Some(300))]);
        assert!(l.has_created && !l.truncated && l.unreadable == 0);
        let spawned = r.spawned();
        assert_eq!(spawned[0], ["-lA", "-D", "%s", "--", DIR]);
        assert_eq!(spawned[1], ["-lAc", "-D", "%s", "--", DIR]);
        assert_eq!(spawned[2], ["-lAU", "-D", "%s", "--", DIR]);
    }

    #[test]
    fn illumos_has_no_created_time() {
        let one = "echo 'total 1'; echo '-rw-r--r--   1 u g 5 2026-10-20 05:25:30.5 -0400 a'";
        let r = LsRunner::new(&[one]);
        let l = read_folder(&r, HostOs::Illumos, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap();
        assert!(!l.has_created);
        assert_eq!((l.entries[0].created, l.entries[0].changed), (None, Some(1_792_488_330)));
        assert_eq!(r.spawned().len(), 2);
        assert_eq!(r.spawned()[1], ["-lAEc", "--", DIR]);
    }

    #[test]
    fn the_entry_cap_stops_the_read_and_says_so() {
        let r = LsRunner::new(&[&format!("{} sleep 30", files(&["a", "b", "c", "d", "e"], 1))]);
        let limits = BrowseLimits { max_entries: 3, ..BrowseLimits::default() };
        let started = Instant::now();
        let l = read_folder(&r, HostOs::FreeBsd, DIR, &limits, &AtomicBool::new(false)).unwrap();
        assert_eq!((l.entries.len(), l.truncated), (3, true));
        assert!(started.elapsed() < Duration::from_secs(10), "the child was killed, not waited for");
    }

    #[test]
    fn exactly_the_cap_is_not_truncated() {
        let r = LsRunner::new(&[&files(&["a", "b", "c"], 1)]);
        let limits = BrowseLimits { max_entries: 3, ..BrowseLimits::default() };
        let l = read_folder(&r, HostOs::FreeBsd, DIR, &limits, &AtomicBool::new(false)).unwrap();
        assert_eq!((l.entries.len(), l.truncated), (3, false));
    }

    #[test]
    fn the_time_limit_stops_the_read() {
        let r = LsRunner::new(&[&format!("{} sleep 30", files(&["a"], 1))]);
        let limits = BrowseLimits { max_seconds: 1, ..BrowseLimits::default() };
        let started = Instant::now();
        let l = read_folder(&r, HostOs::FreeBsd, DIR, &limits, &AtomicBool::new(false)).unwrap();
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
            read_folder(&r, HostOs::FreeBsd, DIR, &BrowseLimits::default(), &cancel)
        });
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_failing_first_call_is_an_error_with_the_first_stderr_line() {
        let r = LsRunner::new(&["echo 'ls: /x: Permission denied' >&2; echo second >&2; exit 2"]);
        let err = read_folder(&r, HostOs::Linux, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap_err().to_string();
        assert!(err.contains("Permission denied") && !err.contains("second"), "{err}");
    }

    #[test]
    fn a_mismatched_second_listing_leaves_changed_empty_not_wrong() {
        let r = LsRunner::new(&[&files(&["a", "b"], 100), &files(&["a", "zzz"], 200), "exit 3"]);
        let l = read_folder(&r, HostOs::FreeBsd, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap();
        assert_eq!((l.entries[0].changed, l.entries[1].changed), (Some(200), None));
        assert_eq!((l.entries[0].created, l.entries[1].created), (None, None), "a failing later listing leaves the time unknown");
    }

    #[test]
    fn unparsable_lines_are_counted_not_shown() {
        let r = LsRunner::new(&[&format!("{} echo 'garbage line'; echo 'more garbage';", files(&["a"], 1))]);
        let l = read_folder(&r, HostOs::FreeBsd, DIR, &BrowseLimits::default(), &AtomicBool::new(false)).unwrap();
        assert_eq!((l.entries.len(), l.unreadable), (1, 2));
    }

    #[test]
    fn a_directory_that_was_not_built_by_snapshot_dir_is_refused_before_anything_runs() {
        let r = LsRunner::new(&["true"]);
        for bad in ["/etc", "/m/.zfs/snapshot/s\nfoo", "relative/.zfs/snapshot/s", "/m/.zfs/snapshot/s/\u{1b}x"] {
            assert!(read_folder(&r, HostOs::FreeBsd, bad, &BrowseLimits::default(), &AtomicBool::new(false)).is_err(), "{bad:?}");
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
    fn explain_handles_the_missing_snapshot_folder_and_other_errors() {
        let msg = explain("h", "ls: /m/.zfs/snapshot/s: No such file or directory");
        assert!(msg.contains("cannot be browsed here") && msg.contains("does not clone"), "{msg}");
        assert_eq!(explain("h", "weird \u{1b}[31m failure"), "weird [31m failure");
    }
}
