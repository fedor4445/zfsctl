//! One line of `zfs diff -FHt` output: when it changed, how, what kind of thing, and where.

// Nothing calls these until the summary (Task 3) and the reader (Task 4) exist; remove this then.
#![allow(dead_code)]

/// What happened to the path between the two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Removed,
    Modified,
    Renamed,
}

impl Change {
    pub fn word(self) -> &'static str {
        match self {
            Change::Added => "added",
            Change::Removed => "removed",
            Change::Modified => "modified",
            Change::Renamed => "renamed",
        }
    }
}

/// The type letter of `zfs diff -F`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Socket,
    Door,
    Pipe,
    BlockDevice,
    CharDevice,
    EventPort,
    Other,
}

impl FileKind {
    pub fn word(self) -> &'static str {
        match self {
            FileKind::File => "file",
            FileKind::Directory => "directory",
            FileKind::Symlink => "symlink",
            FileKind::Socket => "socket",
            FileKind::Door => "door",
            FileKind::Pipe => "pipe",
            FileKind::BlockDevice => "block device",
            FileKind::CharDevice => "character device",
            FileKind::EventPort => "event port",
            FileKind::Other => "other",
        }
    }

    fn from_letter(c: char) -> Self {
        match c {
            'F' => FileKind::File,
            '/' => FileKind::Directory,
            '@' => FileKind::Symlink,
            '=' => FileKind::Socket,
            '>' => FileKind::Door,
            '|' => FileKind::Pipe,
            'B' => FileKind::BlockDevice,
            'C' => FileKind::CharDevice,
            'P' => FileKind::EventPort,
            _ => FileKind::Other,
        }
    }
}

/// One changed path. `changed_at` is the change time in seconds; `renamed_to` is the new path of a rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    pub changed_at: u64,
    pub change: Change,
    pub kind: FileKind,
    pub path: String,
    pub renamed_to: Option<String>,
}

/// `<seconds>.<nanoseconds> <tab> change <tab> type <tab> path [<tab> new path]`. Anything else is `None`: the
/// caller counts it as unreadable and never guesses.
pub fn parse_diff_line(line: &str) -> Option<DiffEntry> {
    let fields: Vec<&str> = line.split('\t').collect();
    let (time, change, kind, path, renamed) = match fields.as_slice() {
        [time, change, kind, path] => (time, change, kind, path, None),
        [time, change, kind, path, new] => (time, change, kind, path, Some(*new)),
        _ => return None,
    };
    let change = match *change {
        "+" => Change::Added,
        "-" => Change::Removed,
        "M" => Change::Modified,
        "R" => Change::Renamed,
        _ => return None,
    };
    // A rename has both paths and only a rename does.
    if (change == Change::Renamed) != renamed.is_some() {
        return None;
    }
    let mut kind_chars = kind.chars();
    let (Some(letter), None) = (kind_chars.next(), kind_chars.next()) else { return None };
    let changed_at = time.split('.').next()?.parse::<u64>().ok()?;
    Some(DiffEntry { changed_at, change, kind: FileKind::from_letter(letter), path: decode_zfs_path(path), renamed_to: renamed.map(decode_zfs_path) })
}

/// `zfs diff` writes a space, a tab, a backslash and other unusual bytes as `\NNN` (three octal digits). Bytes are
/// decoded first and the result read as UTF-8 (a multi-byte character arrives as several escapes); a backslash that
/// does not start three octal digits is kept as written.
pub fn decode_zfs_path(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'\\' {
            if let Some(byte) = bytes.get(i + 1..i + 4).and_then(octal_byte) {
                out.push(byte);
                i += 4;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Three octal digits as one byte; `None` for anything else (including a value over 255).
fn octal_byte(digits: &[u8]) -> Option<u8> {
    let &[a, b, c] = digits else { return None };
    let digit = |d: u8| (b'0'..=b'7').contains(&d).then(|| u32::from(d - b'0'));
    u8::try_from(digit(a)? * 64 + digit(b)? * 8 + digit(c)?).ok()
}

/// The path as seen from the dataset's own root: `/scratch/data/a/b` with the mountpoint `/scratch/data` is `a/b`
/// and the dataset's root directory is the empty string. On a delegated dataset the host prints the path through
/// the zone root (`/rpool/zones/z/root/omni30/syncthing/state/x` for the mountpoint `/omni30/syncthing`), so the
/// mountpoint is also looked for inside the path, as a whole run of components. `None` when the mountpoint is not a
/// path (`legacy`, `none`, `-`, empty) or is not in the path.
pub fn relative_to_mount(path: &str, mountpoint: &str) -> Option<String> {
    if !mountpoint.starts_with('/') {
        return None;
    }
    let mp = mountpoint.trim_end_matches('/');
    if mp.is_empty() {
        return Some(path.trim_matches('/').to_string());
    }
    let mut from = 0;
    while let Some(found) = path.get(from..).and_then(|rest| rest.find(mp)) {
        let start = from + found;
        if let Some(rest) = path.get(start + mp.len()..) {
            if rest.is_empty() || rest.starts_with('/') {
                return Some(rest.trim_matches('/').to_string());
            }
        }
        from = start + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_added_file() {
        let e = parse_diff_line("1788654605.572189000\t+\tF\t/scratch/data/test.txt").unwrap();
        assert_eq!((e.changed_at, e.change, e.kind), (1_788_654_605, Change::Added, FileKind::File));
        assert_eq!((e.path.as_str(), e.renamed_to), ("/scratch/data/test.txt", None));
    }

    #[test]
    fn parses_modified_removed_and_a_directory() {
        let m = parse_diff_line("1788654605.5\tM\t/\t/scratch/data/").unwrap();
        assert_eq!((m.change, m.kind), (Change::Modified, FileKind::Directory));
        let r = parse_diff_line("1788654605.5\t-\tF\t/scratch/data/gone").unwrap();
        assert_eq!((r.change, r.kind), (Change::Removed, FileKind::File));
    }

    #[test]
    fn a_rename_line_has_a_fifth_field() {
        let e = parse_diff_line("1788654605.5\tR\tF\t/a/old\t/a/new").unwrap();
        assert_eq!((e.change, e.path.as_str(), e.renamed_to.as_deref()), (Change::Renamed, "/a/old", Some("/a/new")));
    }

    #[test]
    fn every_type_letter_maps() {
        let kind = |c: &str| parse_diff_line(&format!("1.0\t+\t{c}\t/x")).unwrap().kind;
        assert_eq!(kind("F"), FileKind::File);
        assert_eq!(kind("/"), FileKind::Directory);
        assert_eq!(kind("@"), FileKind::Symlink);
        assert_eq!(kind("="), FileKind::Socket);
        assert_eq!(kind(">"), FileKind::Door);
        assert_eq!(kind("|"), FileKind::Pipe);
        assert_eq!(kind("B"), FileKind::BlockDevice);
        assert_eq!(kind("C"), FileKind::CharDevice);
        assert_eq!(kind("P"), FileKind::EventPort);
        assert_eq!(kind("Z"), FileKind::Other);
        assert_eq!(FileKind::BlockDevice.word(), "block device");
        assert_eq!(Change::Removed.word(), "removed");
    }

    #[test]
    fn garbage_is_none() {
        for bad in ["", "a\tb\tc", "x.1\t+\tF\t/p", "1.0\tX\tF\t/p", "1.0\tR\tF\t/p", "1.0\t+\tF\t/p\t/extra", "1.0\t+\tFF\t/p"] {
            assert!(parse_diff_line(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn octal_escapes_are_decoded() {
        assert_eq!(decode_zfs_path("/a/b\\040c"), "/a/b c");
        assert_eq!(decode_zfs_path("a\\134b"), "a\\b");
        assert_eq!(decode_zfs_path("a\\011b"), "a\tb");
        assert_eq!(decode_zfs_path("trailing\\"), "trailing\\");
        assert_eq!(decode_zfs_path("odd\\9zz"), "odd\\9zz");
        assert_eq!(decode_zfs_path("caf\\303\\251"), "café");
        assert_eq!(parse_diff_line("1.0\t+\tF\t/a/b\\040c").unwrap().path, "/a/b c");
    }

    #[test]
    fn relative_to_mount_strips_a_plain_prefix() {
        assert_eq!(relative_to_mount("/scratch/data/test.txt", "/scratch/data").as_deref(), Some("test.txt"));
        assert_eq!(relative_to_mount("/scratch/data/", "/scratch/data").as_deref(), Some(""));
        assert_eq!(relative_to_mount("/scratch/data", "/scratch/data").as_deref(), Some(""));
        assert_eq!(relative_to_mount("/scratch/database/x", "/scratch/data"), None, "no false prefix match");
        assert_eq!(relative_to_mount("/etc/rc.conf", "/").as_deref(), Some("etc/rc.conf"));
    }

    #[test]
    fn relative_to_mount_finds_the_mountpoint_behind_a_zone_root() {
        let p = "/rpool/zones/syncthing/root/omni30/syncthing/state/x";
        assert_eq!(relative_to_mount(p, "/omni30/syncthing").as_deref(), Some("state/x"));
        for not_a_path in ["legacy", "none", "-", ""] {
            assert_eq!(relative_to_mount(p, not_a_path), None, "{not_a_path:?}");
        }
    }
}
