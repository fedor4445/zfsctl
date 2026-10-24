//! Looking inside a snapshot: the directory path of a snapshot's folder, the `ls` command lines that list it, and what they print.

use crate::daemon::transfer::run_lines;
use crate::error::{AppError, Result};
use crate::exec::privilege::lookup;
use crate::exec::CommandRunner;
use crate::zfs::host_health::HostOs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The longest path zfsctl will send to a host.
const MAX_PATH: usize = 4096;
/// The longest single file or folder name.
const MAX_COMPONENT: usize = 255;

/// Whether `name` may be one step of a path zfsctl builds: 1 to 255 bytes, not `.` or `..`, no `/`, no control character
/// (a newline or an escape in a name is shown but never sent back to a host), and no U+FFFD (a name that was not text).
pub fn valid_component(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_COMPONENT && name != "." && name != ".." && !name.contains(['/', '\u{fffd}']) && !name.chars().any(char::is_control)
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
    if matches!(mountpoint, "none" | "legacy" | "-" | "") {
        return Err(AppError::Config(format!("this dataset is not mounted at a path (mountpoint '{}'), so there is no snapshot folder to browse", crate::display_text::sanitise_for_display(mountpoint))));
    }
    let Some(rest) = mountpoint.strip_prefix('/') else {
        return Err(AppError::Config("the dataset's mountpoint is not an absolute path, so it cannot be browsed".to_string()));
    };
    let steps: Vec<&str> = rest.split('/').collect();
    if !rest.is_empty() && join_relative(&steps).is_none() {
        return Err(AppError::Config("the dataset's mountpoint is not a plain path, so it cannot be browsed".to_string()));
    }
    if !valid_snapshot_name(snapshot) {
        return Err(AppError::Config("the snapshot's name is not a valid ZFS snapshot name".to_string()));
    }
    let Some(inside) = join_relative(relative) else {
        return Err(AppError::Config("that folder's name cannot be sent to the host (it has a control character, a slash or is too long)".to_string()));
    };
    let base = if rest.is_empty() { "" } else { mountpoint };
    let mut dir = format!("{base}/.zfs/snapshot/{snapshot}");
    if !inside.is_empty() {
        dir.push('/');
        dir.push_str(&inside);
    }
    if dir.len() > MAX_PATH {
        return Err(AppError::Config("that path is longer than 4096 bytes".to_string()));
    }
    Ok(dir)
}

/// Where the folders of a dataset's snapshots can be read: a directory path that holds `.zfs/snapshot`, and the zone it was
/// found through (a dataset given to a zone is read through that zone's root directory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseBase {
    pub path: String,
    pub zone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ZoneRow {
    name: String,
    running: bool,
    path: String,
}

/// A zone name as `zonecfg -z` takes it: 1 to 64 of letters, digits and `_ . -`, not starting with `-`.
fn valid_zone_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && !name.starts_with('-') && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// `zoneadm list -cp` lines (`id:name:state:path:uuid:brand:ip-type:id`), without the global zone, and how many other lines were
/// skipped because their name or path would not be safe to use (a path must be absolute with plain steps) or the line was not in
/// the eight-field format.
fn parse_zoneadm_rows(stdout: &str) -> (Vec<ZoneRow>, usize) {
    let mut skipped = 0;
    let rows = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            // Exactly the eight fields `zoneadm -p` prints: a line that is part of something else (a path with a newline or a
            // colon in it) is not trusted.
            let [id, name, state, path, _uuid, _brand, _ip, _zid] = fields.as_slice() else {
                skipped += 1;
                return None;
            };
            if *name == "global" {
                return None;
            }
            let steps: Vec<&str> = path.strip_prefix('/').map(|p| p.split('/').collect()).unwrap_or_default();
            let ok = !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit() || b == b'-') && valid_zone_name(name) && join_relative(&steps).is_some_and(|j| !j.is_empty());
            if !ok {
                skipped += 1;
                return None;
            }
            Some(ZoneRow { name: (*name).to_string(), running: *state == "running", path: (*path).to_string() })
        })
        .collect();
    (rows, skipped)
}

/// The running zones as `(name, zone path)`.
#[cfg(test)]
fn parse_zoneadm(stdout: &str) -> Vec<(String, String)> {
    parse_zoneadm_rows(stdout).0.into_iter().filter(|z| z.running).map(|z| (z.name, z.path)).collect()
}

/// The zone that has `dataset` (or a dataset above it) given to it, as `(name, zone path)`. Asked with two commands that need no
/// sudo: `zoneadm list -cp`, then `zonecfg -z NAME info dataset` for each zone. Several zones, none, or a stopped one is refused.
pub fn zone_for_dataset(runner: &dyn CommandRunner, dataset: &str) -> Result<(String, String)> {
    let listed = runner.run_unprivileged("zoneadm", &["list", "-cp"])?;
    if !listed.success() {
        return Err(AppError::CommandFailed { cmd: "zoneadm list -cp".to_string(), status: listed.status, stderr: listed.stderr });
    }
    let (mut running, mut stopped, mut unchecked) = (Vec::new(), Vec::new(), Vec::new());
    let (zones, skipped) = parse_zoneadm_rows(&listed.stdout);
    for zone in zones {
        let info = match runner.run_unprivileged("zonecfg", &["-z", &zone.name, "info", "dataset"]) {
            Ok(info) if info.success() => info,
            _ => {
                unchecked.push(zone.name);
                continue;
            }
        };
        let delegated = info.stdout.lines().filter_map(|l| l.trim().strip_prefix("name:")).map(str::trim).any(|d| dataset == d || dataset.strip_prefix(d).is_some_and(|rest| rest.starts_with('/')));
        if delegated {
            if zone.running { running.push(zone) } else { stopped.push(zone) }
        }
    }
    // A zone that could not be asked (or a line that could not be used) might be the one that holds the dataset: refuse rather
    // than pick another zone or say "no zone".
    if !unchecked.is_empty() || skipped > 0 {
        let mut parts = Vec::new();
        if !unchecked.is_empty() {
            parts.push(format!("zonecfg failed for {}", unchecked.join(", ")));
        }
        if skipped > 0 {
            parts.push(format!("skipped {skipped} zone line(s) it could not use"));
        }
        return Err(AppError::Config(format!("could not check every zone ({}), so zfsctl cannot be sure which zone holds this dataset", parts.join("; "))));
    }
    match (running.as_slice(), stopped.as_slice()) {
        ([one], _) => Ok((one.name.clone(), one.path.clone())),
        ([], []) => Err(AppError::Config("no running zone is given this dataset, so its snapshots cannot be browsed from here".to_string())),
        ([], [zone, ..]) => Err(AppError::Config(format!("the zone {} that holds this dataset is not running, so its snapshots cannot be browsed (start the zone first)", zone.name))),
        (many, _) => Err(AppError::Config(format!("more than one zone ({}) is given this dataset, so zfsctl cannot tell which to read", many.iter().map(|z| z.name.as_str()).collect::<Vec<_>>().join(", ")))),
    }
}

/// Where `dataset` can be browsed: its mountpoint, but only when ZFS says it is mounted there (an unmounted, `canmount=noauto`,
/// key-not-loaded or `recv -u` dataset still reports a mountpoint, and another dataset may be mounted at that path). On illumos
/// a dataset given to a zone is read through that zone's root directory (`<zone path>/root<mountpoint>`), because the global zone
/// has no `.zfs` folder for it.
pub fn browse_mountpoint(runner: &dyn CommandRunner, os: HostOs, dataset: &str) -> Result<BrowseBase> {
    super::not_an_option(dataset)?;
    let props = if os == HostOs::Illumos { "mounted,mountpoint,zoned" } else { "mounted,mountpoint" };
    let out = super::run_checked(runner, "zfs", &["get", "-H", "-o", "property,value", props, dataset], &format!("zfs get {props} {dataset}"))?;
    let raw = |name: &str| out.stdout.lines().filter_map(|l| l.split_once('\t')).find(|(k, _)| *k == name).map(|(_, v)| v.to_string());
    let value = |name: &str| raw(name).map(|v| v.trim().to_string());
    // A dataset given to a zone is looked up first: a halted zone has its datasets unmounted, and "the zone is not running" is the
    // useful message, not "not mounted".
    let zone = if os == HostOs::Illumos && value("zoned").as_deref() == Some("on") { Some(zone_for_dataset(runner, dataset)?) } else { None };
    if value("mounted").as_deref() != Some("yes") {
        return Err(AppError::Config("this dataset is not mounted, so there is no snapshot folder to browse".to_string()));
    }
    let mountpoint = match raw("mountpoint") {
        Some(m) if m.starts_with('/') => m,
        _ => return Err(AppError::Config("this dataset is not mounted at a path (mountpoint none, legacy or -), so there is no snapshot folder to browse".to_string())),
    };
    if let Some((zone, zone_path)) = zone {
        // Everything after `<zone path>/root` is controlled by the zone's own root user (folders, symlinks, even this
        // mountpoint), so the global zone's `ls` can be pointed elsewhere on the host by a hostile zone. What is shown is a
        // hint, and a restore must never read or write through this path.
        let inside = mountpoint.trim_start_matches('/');
        let path = if inside.is_empty() { format!("{zone_path}/root") } else { format!("{zone_path}/root/{inside}") };
        return Ok(BrowseBase { path, zone: Some(zone) });
    }
    Ok(BrowseBase { path: mountpoint, zone: None })
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
    let forms = crate::exec::privilege::pinned_forms(lookup("ls")?, os);
    let offset = match times {
        Times::Modified => 0,
        Times::Changed => 1,
        Times::Created => 2,
    };
    let form = forms.get(offset)?;
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
            EntryKind::Block => "block",
            EntryKind::Character => "char",
            EntryKind::Pipe => "pipe",
            EntryKind::Socket => "socket",
            EntryKind::Door => "door",
            EntryKind::Port => "port",
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
    /// The name (or link target) could not be decoded with certainty: illumos prints a backslash unescaped, so a name that holds
    /// one may be an escape sequence or literal text. Such a name is shown but never opened or restored.
    pub ambiguous: bool,
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

/// Decodes the C-style escapes of `ls -b` and `ls -Q`: `\\ \" \a \b \f \n \r \t \v \e` and up to three octal digits (a byte).
/// Bytes that do not form UTF-8 become U+FFFD, so such a name can be shown but is never sent back. An unknown escape and a
/// trailing backslash are kept as they are.
fn decode_escapes(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b != b'\\' {
            out.push(b);
            i += 1;
            continue;
        }
        let Some(&next) = bytes.get(i + 1) else {
            out.push(b);
            i += 1;
            continue;
        };
        let simple = match next {
            b'\\' => Some(b'\\'),
            b'"' => Some(b'"'),
            b'a' => Some(7),
            b'b' => Some(8),
            b'f' => Some(12),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            b'v' => Some(11),
            b'e' => Some(27),
            _ => None,
        };
        if let Some(s) = simple {
            out.push(s);
            i += 2;
        } else if next.is_ascii_digit() && next < b'8' {
            let digits: Vec<u8> = bytes.get(i + 1..).unwrap_or_default().iter().take(3).take_while(|d| (b'0'..=b'7').contains(*d)).copied().collect();
            let value = digits.iter().fold(0u32, |acc, d| acc * 8 + u32::from(d - b'0'));
            if let Ok(byte) = u8::try_from(value) {
                out.push(byte);
                i += 1 + digits.len();
            } else {
                out.push(b);
                i += 1;
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The text between the opening double quote at the start of `text` and its closing quote (an escaped quote does not close it),
/// and what follows the closing quote.
fn split_quoted(text: &str) -> Option<(&str, &str)> {
    let inner = text.strip_prefix('"')?;
    let mut escaped = false;
    for (i, c) in inner.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some((inner.get(..i)?, inner.get(i + 1..)?)),
            _ => {}
        }
    }
    None
}

/// Name, symlink target and whether they are ambiguous, from the name part of an escaped `ls -l` line.
fn split_name(os: HostOs, kind: EntryKind, text: &str) -> Option<(String, Option<String>, bool)> {
    let is_link = kind == EntryKind::Symlink;
    if os == HostOs::Linux {
        // GNU `ls -Q`: "name", and for a symlink "name" -> "target".
        let (name, rest) = split_quoted(text)?;
        if !is_link {
            return rest.is_empty().then(|| (decode_escapes(name), None, false));
        }
        let target = rest.strip_prefix(" -> ")?;
        let (target, tail) = split_quoted(target)?;
        return tail.is_empty().then(|| (decode_escapes(name), Some(decode_escapes(target)), false));
    }
    // `ls -b`: the name as it is, with escapes. A symlink target follows ` -> `; a link whose name or target holds that arrow too
    // cannot be split with certainty.
    let ambiguous_text = |s: &str| os == HostOs::Illumos && s.contains('\\');
    if !is_link {
        return Some((decode_escapes(text), None, ambiguous_text(text)));
    }
    let arrows = text.matches(" -> ").count();
    let (name, target) = text.split_once(" -> ")?;
    Some((decode_escapes(name), Some(decode_escapes(target)), arrows > 1 || ambiguous_text(name) || ambiguous_text(target)))
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
    let (name, link_target, ambiguous) = split_name(os, kind, text)?;
    Some(ListEntry { name, kind, mode: mode.to_string(), owner: owner.to_string(), group: group.to_string(), size, time: Some(time), link_target, ambiguous })
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
    /// The changed- or created-time listing ran out of time, so some of those times are unknown.
    pub times_incomplete: bool,
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
                // A line that is not an entry (the escaped listing never splits a name over lines) is counted, never glued on.
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
    read_listing(runner, os, dir, limits, cancel)
}

/// Reads one folder of the live tree (the restore's checks): the same listing as [`read_folder`], for an absolute path of at
/// most 4096 bytes with no control character or U+FFFD and no empty, `.` or `..` step.
pub fn read_live_folder(runner: &dyn CommandRunner, os: HostOs, dir: &str, limits: &BrowseLimits, cancel: &AtomicBool) -> Result<Listing> {
    let plain = dir.starts_with('/') && dir.len() <= MAX_PATH && !dir.chars().any(|c| c.is_control() || c == '\u{fffd}') && (dir == "/" || dir[1..].split('/').all(|s| !s.is_empty() && s != "." && s != ".."));
    if !plain {
        return Err(AppError::Config("refusing to list a folder whose path is not a plain absolute path".to_string()));
    }
    read_listing(runner, os, dir, limits, cancel)
}

fn read_listing(runner: &dyn CommandRunner, os: HostOs, dir: &str, limits: &BrowseLimits, cancel: &AtomicBool) -> Result<Listing> {
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
    let mut times_incomplete = false;
    for times in times_for(os).iter().copied().filter(|t| *t != Times::Modified) {
        let Some(args) = ls_args(os, times, dir) else { continue };
        let other = list_once(runner, &args, limits.max_entries, deadline, os, cancel)?;
        times_incomplete |= other.truncated;
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
    Ok(Listing { entries: rows, truncated: first.truncated, unreadable, has_created: times_for(os).contains(&Times::Created), times_incomplete })
}

/// What to tell the user about a failed listing on `host`: the sudoers entry that is missing, a snapshot folder that is not
/// visible, or the host's own words (sanitised).
pub fn explain(host: &str, error_text: &str) -> String {
    let lower = error_text.to_lowercase();
    if ["password is required", "not allowed to execute", "no tty present", "may not run sudo"].iter().any(|p| lower.contains(p)) {
        return format!("browse is not enabled for this host: run zfsctl print-sudoers {host} --with browse, install the file as the host's sudoers entry and try again");
    }
    if lower.contains("no such file or directory") {
        return "this folder cannot be browsed here: it is not visible on the host (no .zfs/snapshot folder, or it was removed); zfsctl does not clone to work around that".to_string();
    }
    crate::display_text::sanitise_for_display(error_text)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KindFilter {
    #[default]
    All,
    Files,
    Folders,
    Links,
}

/// What the browser hides: by name (a case-insensitive piece of it), modified time, size and kind. Folders are never hidden by
/// size or date, so the user can still walk down to what they are looking for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub name: crate::zfs::name_filter::NameFilter,
    pub modified_from: Option<i64>,
    pub modified_to: Option<i64>,
    pub size_min: Option<u64>,
    pub size_max: Option<u64>,
    pub kind: KindFilter,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.name.is_empty() && self.modified_from.is_none() && self.modified_to.is_none() && self.size_min.is_none() && self.size_max.is_none() && self.kind == KindFilter::All
    }

    pub fn matches(&self, row: &Row) -> bool {
        let e = &row.entry;
        let kind_ok = match self.kind {
            KindFilter::All => true,
            KindFilter::Files => !matches!(e.kind, EntryKind::Directory | EntryKind::Symlink),
            KindFilter::Folders => e.kind == EntryKind::Directory,
            KindFilter::Links => e.kind == EntryKind::Symlink,
        };
        if !kind_ok || !self.name.matches(&e.name) {
            return false;
        }
        if e.kind == EntryKind::Directory {
            return true;
        }
        if self.size_min.is_some_and(|min| e.size < min) || self.size_max.is_some_and(|max| e.size > max) {
            return false;
        }
        match e.time {
            Some(t) => !(self.modified_from.is_some_and(|from| t < from) || self.modified_to.is_some_and(|to| t > to)),
            None => self.modified_from.is_none() && self.modified_to.is_none(),
        }
    }
}

/// One size such as `500`, `10M`, `2KB` (units are powers of 1024).
fn parse_size(text: &str) -> Option<u64> {
    let upper = text.to_ascii_uppercase();
    let body = upper.strip_suffix('B').unwrap_or(&upper);
    let (digits, factor) = [('K', 1u64 << 10), ('M', 1 << 20), ('G', 1 << 30), ('T', 1 << 40)].into_iter().find_map(|(unit, factor)| body.strip_suffix(unit).map(|d| (d, factor))).unwrap_or((body, 1));
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(factor)
}

/// `10M..`, `..1G`, `1K..1M` or one size (exactly that many bytes); `None` for anything else or a reversed range.
pub fn parse_size_range(text: &str) -> Option<(Option<u64>, Option<u64>)> {
    let parts: Vec<&str> = text.split("..").collect();
    let range = match parts.as_slice() {
        [one] => {
            let n = parse_size(one)?;
            (Some(n), Some(n))
        }
        [lo, hi] => {
            let lo = if lo.is_empty() { None } else { Some(parse_size(lo)?) };
            let hi = if hi.is_empty() { None } else { Some(parse_size(hi)?) };
            if lo.is_none() && hi.is_none() {
                return None;
            }
            (lo, hi)
        }
        _ => return None,
    };
    match range {
        (Some(lo), Some(hi)) if lo > hi => None,
        ok => Some(ok),
    }
}

/// The start of `YYYY-MM-DD` in a zone `utc_offset_secs` east of UTC, as epoch seconds.
fn day_start(text: &str, utc_offset_secs: i32) -> Option<i64> {
    let day = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
    Some(day.and_hms_opt(0, 0, 0)?.and_utc().timestamp() - i64::from(utc_offset_secs))
}

/// `2026-10-01..2026-10-20`, `2026-10-01..`, `..2026-10-20` or one day; the end day is included to its last second.
pub fn parse_date_range(text: &str, utc_offset_secs: i32) -> Option<(Option<i64>, Option<i64>)> {
    let parts: Vec<&str> = text.split("..").collect();
    let range = match parts.as_slice() {
        [one] => {
            let start = day_start(one, utc_offset_secs)?;
            (Some(start), Some(start + 86_399))
        }
        [lo, hi] => {
            let lo = if lo.is_empty() { None } else { Some(day_start(lo, utc_offset_secs)?) };
            let hi = if hi.is_empty() { None } else { Some(day_start(hi, utc_offset_secs)? + 86_399) };
            if lo.is_none() && hi.is_none() {
                return None;
            }
            (lo, hi)
        }
        _ => return None,
    };
    match range {
        (Some(lo), Some(hi)) if lo > hi => None,
        ok => Some(ok),
    }
}

/// Folders first, then by name (case-insensitive, then bytewise so the order is the same every time).
pub fn sort_rows(rows: &mut [Row]) {
    rows.sort_by(|a, b| {
        let folder = |r: &Row| r.entry.kind != EntryKind::Directory;
        folder(a).cmp(&folder(b)).then_with(|| a.entry.name.to_lowercase().cmp(&b.entry.name.to_lowercase())).then_with(|| a.entry.name.cmp(&b.entry.name))
    });
}

#[cfg(test)]
mod tests;
