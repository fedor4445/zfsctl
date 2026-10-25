//! One line of `zfs diff -FHt` output: when it changed, how, what kind of thing, and where.


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

/// `zfs diff` writes a space, a tab, a backslash and other unusual bytes as a backslash and four octal digits with a
/// leading zero (`\\0040` for a space; seen on FreeBSD). The three-digit form is read too, when no four-digit escape
/// fits. Bytes are decoded first and the result read as UTF-8 (a multi-byte character arrives as several escapes); a
/// backslash that starts no escape is kept as written.
pub fn decode_zfs_path(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'\\' {
            let four = bytes.get(i + 1..i + 5).and_then(octal_byte);
            let three = bytes.get(i + 1..i + 4).and_then(octal_byte);
            if let Some((byte, used)) = four.map(|byte| (byte, 5)).or_else(|| three.map(|byte| (byte, 4))) {
                out.push(byte);
                i += used;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Octal digits (three or four of them) as one byte; `None` for anything else, or a value over 255.
fn octal_byte(digits: &[u8]) -> Option<u8> {
    if !(3..=4).contains(&digits.len()) {
        return None;
    }
    let mut value: u32 = 0;
    for &d in digits {
        if !(b'0'..=b'7').contains(&d) {
            return None;
        }
        value = value * 8 + u32::from(d - b'0');
    }
    u8::try_from(value).ok()
}

/// The path as seen from the dataset's own root: `/scratch/data/a/b` with the mountpoint `/scratch/data` is `a/b`
/// and the dataset's root directory is the empty string. On a delegated dataset the host prints the path through
/// the zone root (`/rpool/zones/z/root/tank/photos/state/x` for the mountpoint `/tank/photos`), so the
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
        // As real hosts print them (`%04o`: a backslash, then four octal digits with a leading zero).
        assert_eq!(decode_zfs_path("/a/b\\0040c"), "/a/b c");
        assert_eq!(decode_zfs_path("a\\0134b"), "a\\b");
        assert_eq!(decode_zfs_path("a\\0011b"), "a\tb");
        assert_eq!(decode_zfs_path("caf\\0303\\0251"), "café");
        assert_eq!(decode_zfs_path("/s/photo\\0040dir/new\\0040file.txt"), "/s/photo dir/new file.txt");
        // A literal digit after an escape stays a digit.
        assert_eq!(decode_zfs_path("a\\00400"), "a 0");
        // The three-digit form is read too, when no four-digit escape fits.
        assert_eq!(decode_zfs_path("a\\040b"), "a b");
        assert_eq!(decode_zfs_path("a\\0407"), "a 7", "0407 is over 255, so it is \\040 and a 7");
        // Anything that is not an escape is kept as written.
        assert_eq!(decode_zfs_path("trailing\\"), "trailing\\");
        assert_eq!(decode_zfs_path("odd\\9zz"), "odd\\9zz");
        assert_eq!(parse_diff_line("1.0\t+\tF\t/a/b\\0040c").unwrap().path, "/a/b c");
    }

    // Lines copied from real hosts on 2026-10-20 (a FreeBSD 15 hub, an OmniOS zone dataset, Ubuntu 24.04 with its OpenZFS).
    #[test]
    fn real_output_from_three_systems_parses_and_reads_relative_to_the_dataset() {
        let freebsd = parse_diff_line("1792530621.342899000\t+\tF\t/scratch/data/zt-tmp/photo\\0040dir/new\\0040file.txt").unwrap();
        assert_eq!(relative_to_mount(&freebsd.path, "/scratch/data").as_deref(), Some("zt-tmp/photo dir/new file.txt"));
        let rename = parse_diff_line("1792530621.338421000\tR\tF\t/scratch/data/zt-tmp/docs/f2.txt\t/scratch/data/zt-tmp/docs/f2-renamed.txt").unwrap();
        assert_eq!((rename.change, rename.kind), (Change::Renamed, FileKind::File));
        assert_eq!(relative_to_mount(rename.renamed_to.as_deref().unwrap(), "/scratch/data").as_deref(), Some("zt-tmp/docs/f2-renamed.txt"));
        let link = parse_diff_line("1792530621.344011000\t+\t@\t/scratch/data/zt-tmp/link").unwrap();
        assert_eq!(link.kind, FileKind::Symlink);

        let illumos = parse_diff_line("1785097718.962873562\t+\tF\t/rpool/zones/z/root/tank/photos/Pictures/2026/photo1.jpg").unwrap();
        assert_eq!(relative_to_mount(&illumos.path, "/tank/photos").as_deref(), Some("Pictures/2026/photo1.jpg"));

        let dir = parse_diff_line("1792529230.252399441\t+\t/\t/ztest/data/zt-sub").unwrap();
        assert_eq!((dir.change, dir.kind), (Change::Added, FileKind::Directory));
        assert_eq!(relative_to_mount(&dir.path, "/ztest/data").as_deref(), Some("zt-sub"));
        let root = parse_diff_line("1792529230.252399441\tM\t/\t/ztest/data/").unwrap();
        assert_eq!(relative_to_mount(&root.path, "/ztest/data").as_deref(), Some(""), "the dataset's own root directory");
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
        let p = "/rpool/zones/z/root/tank/photos/state/x";
        assert_eq!(relative_to_mount(p, "/tank/photos").as_deref(), Some("state/x"));
        for not_a_path in ["legacy", "none", "-", ""] {
            assert_eq!(relative_to_mount(p, not_a_path), None, "{not_a_path:?}");
        }
    }
}
