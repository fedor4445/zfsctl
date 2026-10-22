//! Planning a restore: one regular file copied out of a snapshot to a new name in the live tree. Pure: it checks every name,
//! builds both paths and the exact `cp` argument list, and refuses anything that could replace or reach beyond one new file.

use crate::error::{AppError, Result};
use crate::exec::privilege::{lookup, pinned_forms};
use crate::zfs::host_health::HostOs;
use crate::zfs::snapshot_browse::{snapshot_dir, valid_component, EntryKind, ListEntry};

const MAX_PATH: usize = 4096;
const MAX_COMPONENT: usize = 255;

/// What the user picked: a file inside a snapshot, and where its dataset is mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreSource {
    pub dataset: String,
    pub snapshot: String,
    pub mountpoint: String,
    /// The folder steps below the dataset root, each one listed as a real folder.
    pub folder: Vec<String>,
    pub entry: ListEntry,
    /// The zone a dataset was given to, if any: a restore is refused then.
    pub zone: Option<String>,
    pub os: HostOs,
}

/// A checked restore: the two paths, the new name and the `cp` argument list (no program name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    pub source: String,
    pub destination: String,
    pub dest_folder: String,
    pub dest_name: String,
    pub command: Vec<String>,
    pub size: u64,
    pub mode: String,
}

/// `<stem>.restored-<snapshot><ext>`: the extension is what follows the last dot that is not the first character. Characters of
/// the snapshot name outside letters, digits and `_ . : -` become `_`. Over 255 bytes, the stem is shortened (at a character
/// boundary), never the tail.
pub fn default_destination_name(file: &str, snapshot: &str) -> String {
    let (stem, ext) = match file.rfind('.') {
        Some(i) if i > 0 => file.split_at(i),
        _ => (file, ""),
    };
    let snap: String = snapshot.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') { c } else { '_' }).collect();
    let mut tail = format!(".restored-{snap}{ext}");
    if tail.len() > MAX_COMPONENT {
        let mut cut = MAX_COMPONENT;
        while !tail.is_char_boundary(cut) {
            cut -= 1;
        }
        tail.truncate(cut);
        return tail;
    }
    let room = MAX_COMPONENT - tail.len();
    let mut cut = stem.len().min(room);
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{tail}", &stem[..cut])
}

fn refuse<T>(text: &str) -> Result<T> {
    Err(AppError::Config(text.to_string()))
}

/// Checks the pick and the chosen name and builds the plan. The destination is in the live tree (no `.zfs`), the source is the
/// snapshot's copy; the command is the registry's pinned `cp` form with the two operands in place of its two `*` words.
pub fn plan_restore(src: &RestoreSource, dest_name: &str) -> Result<RestorePlan> {
    if src.zone.is_some() {
        return refuse("restore from a dataset given to a zone must be done inside the zone");
    }
    if src.entry.kind != EntryKind::File {
        return Err(AppError::Config(format!("only a regular file can be restored: this is a {}", src.entry.kind.word())));
    }
    if src.entry.ambiguous {
        return refuse("this name cannot be told apart from an escape sequence on this host, so it is not restored");
    }
    let bad_name = "that name cannot be used: it has a control character, a slash, is empty or too long";
    if !valid_component(&src.entry.name) || !valid_component(dest_name) || !src.folder.iter().all(|s| valid_component(s)) {
        return refuse(bad_name);
    }
    if dest_name == src.entry.name {
        return refuse("choose a different name: the restore never replaces the original");
    }
    let mut steps: Vec<&str> = src.folder.iter().map(String::as_str).collect();
    steps.push(&src.entry.name);
    let source = snapshot_dir(&src.mountpoint, &src.snapshot, &steps)?;
    // snapshot_dir has accepted the mountpoint; build the live folder the same way (a mountpoint of "/" adds no extra slash).
    let base = if src.mountpoint == "/" { "" } else { src.mountpoint.as_str() };
    let mut dest_folder = base.to_string();
    for s in &src.folder {
        dest_folder.push('/');
        dest_folder.push_str(s);
    }
    if dest_folder.is_empty() {
        dest_folder.push('/');
    }
    let destination = if dest_folder == "/" { format!("/{dest_name}") } else { format!("{dest_folder}/{dest_name}") };
    if source.len() > MAX_PATH || destination.len() > MAX_PATH {
        return refuse("that path is longer than 4096 bytes");
    }
    let Some(entry) = lookup("cp") else { return refuse("cp is not registered") };
    let forms = pinned_forms(entry, src.os);
    let [form] = forms.as_slice() else { return refuse("cp has no single pinned form on this host") };
    let Some(prefix) = form.strip_suffix(&["*", "*"]) else { return refuse("cp's pinned form does not end in two operands") };
    let mut command: Vec<String> = prefix.iter().map(ToString::to_string).collect();
    command.push(source.clone());
    command.push(destination.clone());
    Ok(RestorePlan { source, destination, dest_folder, dest_name: dest_name.to_string(), command, size: src.entry.size, mode: src.entry.mode.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> ListEntry {
        ListEntry { name: name.into(), kind: EntryKind::File, mode: "-rw-r--r--".into(), owner: "alice".into(), group: "alice".into(), size: 1234, time: None, link_target: None, ambiguous: false }
    }

    fn source(mountpoint: &str, folder: &[&str], name: &str, os: HostOs) -> RestoreSource {
        RestoreSource { dataset: "tank/data".into(), snapshot: "daily-1".into(), mountpoint: mountpoint.into(), folder: folder.iter().map(ToString::to_string).collect(), entry: entry(name), zone: None, os }
    }

    fn msg<T: std::fmt::Debug>(r: Result<T>) -> String {
        match r {
            Err(AppError::Config(m)) => m,
            other => panic!("expected a Config refusal, got {other:?}"),
        }
    }

    #[test]
    fn default_names() {
        assert_eq!(default_destination_name("photo.jpg", "daily-1"), "photo.restored-daily-1.jpg");
        assert_eq!(default_destination_name("archive.tar.gz", "daily-1"), "archive.tar.restored-daily-1.gz");
        assert_eq!(default_destination_name("README", "daily-1"), "README.restored-daily-1");
        assert_eq!(default_destination_name(".bashrc", "daily-1"), ".bashrc.restored-daily-1");
        assert_eq!(default_destination_name("a.txt", "my snap"), "a.restored-my_snap.txt");
        assert_eq!(default_destination_name("a.txt", "v1.2:x-y_z"), "a.restored-v1.2:x-y_z.txt");
    }

    #[test]
    fn long_default_names_keep_the_tail_and_a_character_boundary() {
        let long = format!("{}.jpg", "é".repeat(200));
        let n = default_destination_name(&long, "daily-1");
        assert!(n.len() <= 255 && !n.is_empty());
        assert!(n.ends_with(".restored-daily-1.jpg"));
        let huge_snap = "s".repeat(255);
        let n = default_destination_name("a.txt", &huge_snap);
        assert!(n.len() <= 255 && !n.is_empty());
        let n = default_destination_name(&"x".repeat(255), "daily-1");
        assert_eq!(n.len(), 255);
        assert!(n.ends_with(".restored-daily-1"));
    }

    #[test]
    fn refusals_are_plain_sentences() {
        let mut s = source("/tank/data", &[], "a.txt", HostOs::FreeBsd);
        s.zone = Some("z1".into());
        assert_eq!(msg(plan_restore(&s, "b.txt")), "restore from a dataset given to a zone must be done inside the zone");
        let mut s = source("/tank/data", &[], "a", HostOs::FreeBsd);
        s.entry.kind = EntryKind::Directory;
        assert_eq!(msg(plan_restore(&s, "b")), "only a regular file can be restored: this is a folder");
        s.entry.kind = EntryKind::Symlink;
        assert_eq!(msg(plan_restore(&s, "b")), "only a regular file can be restored: this is a symlink");
        let mut s = source("/tank/data", &[], "a", HostOs::Illumos);
        s.entry.ambiguous = true;
        assert_eq!(msg(plan_restore(&s, "b")), "this name cannot be told apart from an escape sequence on this host, so it is not restored");
        let bad = "that name cannot be used: it has a control character, a slash, is empty or too long";
        let s = source("/tank/data", &[], "a", HostOs::Linux);
        for n in ["", "x\ny", "x\u{1b}y", "a/b", "..", ".", "x\u{fffd}", &"n".repeat(256)] {
            assert_eq!(msg(plan_restore(&s, n)), bad, "{n:?}");
        }
        assert_eq!(msg(plan_restore(&source("/tank/data", &[], "a\nb", HostOs::Linux), "c")), bad);
        assert_eq!(msg(plan_restore(&source("/tank/data", &["ok", "b\u{7}d"], "a", HostOs::Linux), "c")), bad);
        assert_eq!(msg(plan_restore(&source("/tank/data", &[".."], "a", HostOs::Linux), "c")), bad);
        assert_eq!(msg(plan_restore(&s, "a")), "choose a different name: the restore never replaces the original");
    }

    #[test]
    fn a_destination_name_with_a_slash_or_dotdot_is_refused() {
        let s = source("/tank/data", &[], "a", HostOs::FreeBsd);
        for n in ["../x", "x/..", "/etc/passwd", "..", "a/b"] {
            assert!(plan_restore(&s, n).is_err(), "{n}");
        }
    }

    #[test]
    fn the_command_is_the_pinned_form_with_the_two_paths() {
        for os in [HostOs::FreeBsd, HostOs::Linux, HostOs::Illumos] {
            let s = source("/tank/data", &["docs", "2024"], "photo.jpg", os);
            let plan = plan_restore(&s, "photo.restored-daily-1.jpg").unwrap();
            let want: Vec<String> = ["-p", "-n", "--", "/tank/data/.zfs/snapshot/daily-1/docs/2024/photo.jpg", "/tank/data/docs/2024/photo.restored-daily-1.jpg"].iter().map(ToString::to_string).collect();
            assert_eq!(plan.command, want, "{os:?}");
            let forms = pinned_forms(lookup("cp").unwrap(), os);
            assert_eq!(forms.len(), 1);
            let form = forms[0];
            assert_eq!(&form[form.len() - 2..], &["*", "*"]);
            assert_eq!(plan.command[..form.len() - 2], form[..form.len() - 2]);
            assert_eq!(plan.command.len(), form.len());
            assert_eq!(plan.size, 1234);
            assert_eq!(plan.mode, "-rw-r--r--");
            assert_eq!(plan.dest_folder, "/tank/data/docs/2024");
            assert_eq!(plan.dest_name, "photo.restored-daily-1.jpg");
        }
    }

    #[test]
    fn the_destination_is_in_the_live_tree_not_in_dot_zfs() {
        let plan = plan_restore(&source("/tank/data", &["d"], "a.txt", HostOs::Linux), "b.txt").unwrap();
        assert!(!plan.destination.contains(".zfs"));
        assert!(plan.source.contains("/.zfs/snapshot/daily-1/"));
        assert_eq!(plan.destination, "/tank/data/d/b.txt");
    }

    #[test]
    fn a_root_mountpoint_has_no_extra_slash() {
        let plan = plan_restore(&source("/", &["docs"], "x.jpg", HostOs::FreeBsd), "y.jpg").unwrap();
        assert_eq!(plan.destination, "/docs/x.jpg".replace("x.jpg", "y.jpg"));
        assert_eq!(plan.source, "/.zfs/snapshot/daily-1/docs/x.jpg");
        assert_eq!(plan.dest_folder, "/docs");
        let top = plan_restore(&source("/", &[], "x.jpg", HostOs::FreeBsd), "y.jpg").unwrap();
        assert_eq!(top.destination, "/y.jpg");
        assert_eq!(top.dest_folder, "/");
    }

    #[test]
    fn unmounted_and_overlong_paths_are_refused() {
        assert!(plan_restore(&source("legacy", &[], "a", HostOs::Linux), "b").is_err());
        let deep: Vec<String> = (0..20).map(|_| "d".repeat(250)).collect();
        let mut s = source("/tank/data", &[], "a", HostOs::Linux);
        s.folder = deep;
        assert!(plan_restore(&s, "b").is_err());
    }
}
