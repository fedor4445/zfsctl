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
}
