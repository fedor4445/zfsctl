//! Looking inside a snapshot: the directory path of a snapshot's folder, the `ls` command lines that list it, and what they print.

use crate::error::{AppError, Result};
use crate::exec::privilege::{lookup, Kind};
use crate::zfs::host_health::HostOs;

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
}
