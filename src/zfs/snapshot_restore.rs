//! Planning a restore: one regular file copied out of a snapshot to a new name in the live tree. Pure: it checks every name,
//! builds both paths and the exact `cp` argument list, and refuses anything that could replace or reach beyond one new file.

use crate::error::{AppError, Result};
use crate::exec::privilege::{lookup, pinned_forms};
use crate::exec::CommandRunner;
use crate::zfs::host_health::HostOs;
use crate::zfs::snapshot_browse::{explain, read_folder, read_live_folder, snapshot_dir, valid_component, BrowseLimits, EntryKind, Listing, ListEntry};
use std::sync::atomic::{AtomicBool, Ordering};

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

/// What the verified copy looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    pub destination: String,
    pub size: u64,
    pub mode: String,
    pub owner: String,
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) { refuse("cancelled") } else { Ok(()) }
}

/// Lists a folder for a check; a host failure becomes the same plain sentence the browser shows.
fn list(runner: &dyn CommandRunner, host: &str, os: HostOs, dir: &str, live: bool, cancel: &AtomicBool) -> Result<Listing> {
    let limits = BrowseLimits::default();
    let read = if live { read_live_folder(runner, os, dir, &limits, cancel) } else { read_folder(runner, os, dir, &limits, cancel) };
    read.map_err(|e| match e {
        AppError::CommandFailed { stderr, .. } => AppError::Config(explain(host, &stderr)),
        other => other,
    })
}

fn too_large<T>(folder: &str) -> Result<T> {
    Err(AppError::Config(format!("the folder {} is too large to check", crate::display_text::sanitise_for_display(folder))))
}

fn twice<T>(folder: &str, name: &str) -> Result<T> {
    Err(AppError::Config(format!(
        "two entries in {} have the name {} once escapes are decoded, or one of them cannot be decoded with certainty, so the restore is refused",
        crate::display_text::sanitise_for_display(folder),
        crate::display_text::sanitise_for_display(name)
    )))
}

/// The one entry called `name` in `listing` (of `folder`), or `None` when there is none. Every entry whose DECODED name equals
/// `name` counts: more than one match, or a single match that cannot be decoded with certainty (`ambiguous`, as on illumos where
/// `ls -b` prints a backslash as it is), is refused, so a decoy such as `\144ocs` cannot stand in for `docs`. A cut-off listing
/// cannot show that a name is absent.
fn find_one<'a>(listing: &'a Listing, folder: &str, name: &str) -> Result<Option<&'a ListEntry>> {
    let mut hits = listing.entries.iter().map(|r| &r.entry).filter(|e| e.name == name);
    match (hits.next(), hits.next()) {
        (None, _) if listing.truncated => too_large(folder),
        (None, _) => Ok(None),
        (Some(e), None) if !e.ambiguous => Ok(Some(e)),
        _ => twice(folder, name),
    }
}

/// Whether anything at all (ambiguous or not) decodes to `name`; a cut-off listing cannot show that it is absent.
fn exists_any(listing: &Listing, folder: &str, name: &str) -> Result<bool> {
    if listing.entries.iter().any(|r| r.entry.name == name) {
        return Ok(true);
    }
    if listing.truncated { too_large(folder) } else { Ok(false) }
}

const NOT_ENABLED: &str = "restore is not enabled for this host: run zfsctl print-sudoers HOST --with restore, install the file as the host's sudoers entry and try again";

/// Whether cp's output says sudo refused it: a line that starts with `sudo:` or `Sorry, user` and gives one of sudo's reasons.
/// A cp error that merely names a file called "not allowed to execute" is not one.
fn sudo_refused(text: &str) -> bool {
    text.lines().map(|l| l.trim().to_lowercase()).any(|l| {
        (l.starts_with("sudo:") || l.starts_with("sorry, user")) && ["a password is required", "not allowed to execute", "no tty present", "may not run sudo"].iter().any(|p| l.contains(p))
    })
}

fn reason(e: &AppError) -> String {
    match e {
        AppError::Config(m) => m.clone(),
        other => crate::display_text::sanitise_for_display(&other.to_string()),
    }
}

/// Checks, copies and verifies one restore. Nothing is retried, deleted or repaired: a failed check stops before `cp`, a failed
/// verify is reported as it is. `cp` exits 0 when a no-clobber copy skips, so the destination is checked absent before and
/// present, regular, of the planned size and mode after; the exit status alone is never trusted.
///
/// What this does NOT remove:
/// - The folders are checked right before `cp` runs, not while it runs. A local user who can write in one of them could swap it
///   for a symlink in that window; the window is short but it is not closed.
/// - The dataset's mountpoint (it comes from the zfs property) and every path above it are trusted and not checked.
/// - An entry that appears at the destination between the absence check and the verify would be seen by the verify as if it
///   were the copy (the size and mode check is what limits that).
pub fn run_restore(runner: &dyn CommandRunner, host: &str, plan: &RestorePlan, src: &RestoreSource, cancel: &AtomicBool) -> Result<Restored> {
    // The plan must be exactly the one these picks produce; the chain is derived from the picks, not from the plan's text.
    if plan_restore(src, &plan.dest_name)? != *plan {
        return refuse("the restore plan does not match the picked file: nothing was copied");
    }
    let base = if src.mountpoint == "/" { "" } else { src.mountpoint.as_str() };
    let mut chain: Vec<String> = vec![if base.is_empty() { "/".to_string() } else { base.to_string() }];
    for s in &src.folder {
        let prev = chain.last().map_or("/", String::as_str);
        let next = if prev == "/" { format!("/{s}") } else { format!("{prev}/{s}") };
        chain.push(next);
    }
    let dest_folder = chain.last().cloned().unwrap_or_else(|| "/".to_string());
    let steps: Vec<&str> = src.folder.iter().map(String::as_str).collect();
    let source_folder = snapshot_dir(&src.mountpoint, &src.snapshot, &steps)?;

    // 1. The source is still a regular file of the planned size in the snapshot.
    check_cancel(cancel)?;
    let listing = list(runner, host, src.os, &source_folder, false, cancel)?;
    match find_one(&listing, &source_folder, &src.entry.name)? {
        None => return refuse("the file is no longer in the snapshot listing"),
        Some(e) if e.kind != EntryKind::File || e.size != plan.size => return refuse("the file in the snapshot is not a regular file of the expected size"),
        Some(_) => {}
    }

    // 2. Every folder on the way is a real folder (not a symlink), listed from its parent. The mountpoint itself is trusted.
    for (pair, step) in chain.windows(2).zip(&src.folder) {
        check_cancel(cancel)?;
        let [parent, child] = pair else { return refuse("the destination folder could not be worked out: nothing was copied") };
        let listing = list(runner, host, src.os, parent, true, cancel)?;
        match find_one(&listing, parent, step)? {
            Some(e) if e.kind == EntryKind::Directory => {}
            _ => return Err(AppError::Config(format!("a folder on the way to the destination is a symlink or is gone: {}", crate::display_text::sanitise_for_display(child)))),
        }
    }

    // 3. The new name is not in the destination folder (anything that decodes to it counts as there).
    check_cancel(cancel)?;
    let listing = list(runner, host, src.os, &dest_folder, true, cancel)?;
    if exists_any(&listing, &dest_folder, &plan.dest_name)? {
        return refuse("that name already exists in the folder: nothing was copied");
    }

    // 4. Copy. The checks above are the last look before cp; see the doc comment for the window that stays open.
    check_cancel(cancel)?;
    let args: Vec<&str> = plan.command.iter().map(String::as_str).collect();
    let (failure, may_have_finished) = match runner.run("cp", &args) {
        Ok(out) if out.status == 0 => (None, false),
        Ok(out) => (Some(format!("{}\n{}", out.stderr, out.stdout)), false),
        Err(AppError::CommandFailed { stderr, .. }) => (Some(stderr), false),
        Err(e) => (Some(reason(&e)), true),
    };
    if let Some(text) = failure {
        if sudo_refused(&text) {
            return refuse(&NOT_ENABLED.replace("HOST", host));
        }
        let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no message");
        let first = crate::display_text::sanitise_for_display(first);
        let tail = if may_have_finished { "; it may or may not have finished: check the destination folder" } else { "" };
        return Err(AppError::Config(format!("the copy failed: {first}{tail}")));
    }

    // 5. Verify, whatever cp said. The copy is done, so a cancel no longer cuts this check short. A result that cannot be read is
    //    reported as unchecked, never as "not copied".
    let unchecked = |why: String| AppError::Config(format!("the copy ran (cp exited 0) but its result could not be checked: {why}"));
    let listing = list(runner, host, src.os, &dest_folder, true, &AtomicBool::new(false)).map_err(|e| unchecked(reason(&e)))?;
    let Some(found) = find_one(&listing, &dest_folder, &plan.dest_name).map_err(|e| unchecked(reason(&e)))? else {
        return refuse("not copied: the file is not in the destination after the copy (a name the filesystem treats as an existing one, a full disk, or a refused copy)");
    };
    if found.kind != EntryKind::File {
        return refuse("copied, but what is at the destination is not a regular file");
    }
    if found.size != plan.size {
        return Err(AppError::Config(format!("copied, but the size differs: expected {} bytes, found {}", plan.size, found.size)));
    }
    let want_mode: String = plan.mode.chars().take(10).collect();
    let got_mode: String = found.mode.chars().take(10).collect();
    if got_mode != want_mode {
        return refuse("copied, but the mode differs");
    }
    Ok(Restored { destination: plan.destination.clone(), size: found.size, mode: found.mode.clone(), owner: found.owner.clone() })
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

    use crate::exec::CommandOutput;
    use std::collections::HashMap;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Copy, PartialEq)]
    enum CpMode {
        Copy,
        Nothing,
        Fail,
        CopyBigger,
        CopyOtherMode,
        /// cp exits 0 and the new name shows up twice (a decoy that decodes to the same name).
        CopyTwice,
        /// cp exits 0 and the new name shows up only in a form that cannot be decoded with certainty.
        CopyAmbiguous,
    }

    /// One canned entry: the mode, the size and the name exactly as `ls` prints it.
    #[derive(Clone)]
    struct Ent(String, u64, String);

    fn ent(mode: &str, size: u64, name: &str) -> Ent {
        Ent(mode.into(), size, name.into())
    }

    struct Fake {
        os: HostOs,
        tables: Mutex<HashMap<String, Vec<Ent>>>,
        listed: Mutex<Vec<String>>,
        cps: Mutex<Vec<Vec<String>>>,
        mode: CpMode,
        stderr: String,
        /// Raised when cp runs.
        cancel_at_cp: Option<Arc<AtomicBool>>,
        /// Raised when this folder is listed.
        cancel_at_list: Option<(String, Arc<AtomicBool>)>,
        /// Folders whose listing is cut off by the entry cap (the flag: only once cp has run).
        huge: Vec<(String, bool)>,
        /// The listing of this folder fails once cp has run.
        broken_after_cp: Option<String>,
        /// cp itself cannot be run (a dropped ssh).
        cp_errors: bool,
    }

    impl Fake {
        fn new(mode: CpMode) -> Self {
            Self::on(HostOs::FreeBsd, mode)
        }
        fn on(os: HostOs, mode: CpMode) -> Self {
            let mut t: HashMap<String, Vec<Ent>> = HashMap::new();
            t.insert("/tank/data/.zfs/snapshot/daily-1/docs".into(), vec![ent("-rw-r--r--", 1234, "a.txt")]);
            t.insert("/tank/data".into(), vec![ent("drwxr-xr-x", 2, "docs")]);
            t.insert("/tank/data/docs".into(), vec![ent("-rw-r--r--", 1234, "a.txt")]);
            Self { os, tables: Mutex::new(t), listed: Mutex::new(vec![]), cps: Mutex::new(vec![]), mode, stderr: String::new(), cancel_at_cp: None, cancel_at_list: None, huge: vec![], broken_after_cp: None, cp_errors: false }
        }
        fn set(&self, dir: &str, lines: Vec<Ent>) {
            self.tables.lock().unwrap().insert(dir.into(), lines);
        }
        fn directories(&self) -> Vec<String> {
            let mut out: Vec<String> = vec![];
            for d in self.listed.lock().unwrap().iter() {
                if out.last() != Some(d) {
                    out.push(d.clone());
                }
            }
            out
        }
        fn render(&self, e: &Ent) -> String {
            if self.os == HostOs::Illumos {
                format!("{}  1 alice alice {} 2023-11-14 22:13:20.000000000 +0000 {}", e.0, e.1, e.2)
            } else {
                format!("{}  1 alice alice {} 1700000000 {}", e.0, e.1, e.2)
            }
        }
    }

    impl CommandRunner for Fake {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            assert_eq!(cmd, "cp");
            self.cps.lock().unwrap().push(args.iter().map(ToString::to_string).collect());
            self.listed.lock().unwrap().push("<cp>".into());
            if let Some(flag) = &self.cancel_at_cp {
                flag.store(true, Ordering::SeqCst);
            }
            if self.cp_errors {
                return Err(AppError::Config("ssh: connection dropped".into()));
            }
            let dest = args[args.len() - 1];
            let (folder, name) = dest.rsplit_once('/').unwrap();
            let mut tables = self.tables.lock().unwrap();
            match self.mode {
                CpMode::Copy => tables.get_mut(folder).unwrap().push(ent("-rw-r--r--", 1234, name)),
                CpMode::CopyTwice => {
                    tables.get_mut(folder).unwrap().push(ent("-rw-r--r--", 1234, name));
                    tables.get_mut(folder).unwrap().push(ent("-rw-r--r--", 1234, name));
                }
                CpMode::CopyAmbiguous => tables.get_mut(folder).unwrap().push(ent("-rw-r--r--", 1234, &name.replace("txt", "\\164xt"))),
                CpMode::CopyBigger => tables.get_mut(folder).unwrap().push(ent("-rw-r--r--", 99, name)),
                CpMode::CopyOtherMode => tables.get_mut(folder).unwrap().push(ent("-rw-------", 1234, name)),
                CpMode::Nothing => {}
                CpMode::Fail => return Ok(CommandOutput { stdout: String::new(), stderr: self.stderr.clone(), status: 1 }),
            }
            Ok(CommandOutput { stdout: String::new(), stderr: String::new(), status: 0 })
        }
        fn run_with_stdin(&self, _c: &str, _a: &[&str], _s: &str) -> Result<CommandOutput> {
            Err(AppError::Config("no stdin".into()))
        }
        fn run_unprivileged(&self, _c: &str, _a: &[&str]) -> Result<CommandOutput> {
            Err(AppError::Config("no unprivileged".into()))
        }
        fn command(&self, cmd: &str, args: &[&str]) -> Result<Command> {
            assert_eq!(cmd, "ls");
            let dir = args[args.len() - 1].to_string();
            self.listed.lock().unwrap().push(dir.clone());
            if let Some((at, flag)) = &self.cancel_at_list {
                if *at == dir {
                    flag.store(true, Ordering::SeqCst);
                }
            }
            let after_cp = !self.cps.lock().unwrap().is_empty();
            let mut c = Command::new("sh");
            if self.broken_after_cp.as_deref() == Some(dir.as_str()) && after_cp {
                c.arg("-c").arg("echo 'ls: boom' >&2; exit 2");
                return Ok(c);
            }
            if self.huge.iter().any(|(d, only_after)| *d == dir && (!*only_after || after_cp)) {
                c.arg("-c").arg("seq 20001 | sed 's/.*/-rw-r--r-- 1 a a 1 1700000000 f&/'");
                return Ok(c);
            }
            let body = match self.tables.lock().unwrap().get(&dir) {
                Some(lines) => format!("total 1\n{}\n", lines.iter().map(|e| self.render(e)).collect::<Vec<_>>().join("\n")),
                None => "total 0\n".to_string(),
            };
            c.arg("-c").arg("printf \"%s\" \"$0\"").arg(body);
            Ok(c)
        }
    }

    fn pick() -> RestoreSource {
        source("/tank/data", &["docs"], "a.txt", HostOs::FreeBsd)
    }

    fn plan_for(src: &RestoreSource) -> RestorePlan {
        plan_restore(src, "a.restored-daily-1.txt").unwrap()
    }

    fn go(fake: &Fake) -> Result<Restored> {
        let src = pick();
        run_restore(fake, "hub", &plan_for(&src), &src, &AtomicBool::new(false))
    }

    #[test]
    fn a_restore_runs_the_checks_then_cp_then_verifies() {
        let fake = Fake::new(CpMode::Copy);
        let done = go(&fake).unwrap();
        assert_eq!(done, Restored { destination: "/tank/data/docs/a.restored-daily-1.txt".into(), size: 1234, mode: "-rw-r--r--".into(), owner: "alice".into() });
        assert_eq!(fake.directories(), vec!["/tank/data/.zfs/snapshot/daily-1/docs", "/tank/data", "/tank/data/docs", "<cp>", "/tank/data/docs"]);
        let cps = fake.cps.lock().unwrap();
        assert_eq!(cps.len(), 1);
        assert_eq!(cps[0], ["-p", "-n", "--", "/tank/data/.zfs/snapshot/daily-1/docs/a.txt", "/tank/data/docs/a.restored-daily-1.txt"]);
    }

    #[test]
    fn a_symlink_in_the_destination_chain_is_refused_and_cp_never_runs() {
        for bad in [ent("lrwxr-xr-x", 9, "docs -> /etc"), ent("-rw-r--r--", 9, "docs")] {
            let fake = Fake::new(CpMode::Copy);
            fake.set("/tank/data", vec![bad]);
            let m = msg(go(&fake));
            assert!(m.starts_with("a folder on the way to the destination is a symlink or is gone: "), "{m}");
            assert!(fake.cps.lock().unwrap().is_empty());
        }
        let fake = Fake::new(CpMode::Copy);
        fake.set("/tank/data", vec![]);
        assert!(msg(go(&fake)).contains("is a symlink or is gone"));
        assert!(fake.cps.lock().unwrap().is_empty());
    }

    #[test]
    fn an_existing_destination_is_refused_and_cp_never_runs() {
        let fake = Fake::new(CpMode::Copy);
        fake.set("/tank/data/docs", vec![ent("-rw-r--r--", 1234, "a.txt"), ent("-rw-r--r--", 5, "a.restored-daily-1.txt")]);
        assert_eq!(msg(go(&fake)), "that name already exists in the folder: nothing was copied");
        assert!(fake.cps.lock().unwrap().is_empty());
    }

    #[test]
    fn a_silent_skip_is_reported_as_not_copied() {
        let fake = Fake::new(CpMode::Nothing);
        assert!(msg(go(&fake)).starts_with("not copied: the file is not in the destination after the copy"));
        assert_eq!(fake.cps.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_case_only_difference_the_filesystem_treats_as_the_same_name_is_reported_as_not_copied() {
        // cp exits 0, but the listing after the copy does not show the new name.
        let fake = Fake::new(CpMode::Nothing);
        fake.set("/tank/data/docs", vec![ent("-rw-r--r--", 1234, "a.txt"), ent("-rw-r--r--", 5, "A.RESTORED-daily-1.txt")]);
        assert!(msg(go(&fake)).starts_with("not copied:"));
    }

    #[test]
    fn a_size_mismatch_is_reported() {
        let fake = Fake::new(CpMode::CopyBigger);
        assert_eq!(msg(go(&fake)), "copied, but the size differs: expected 1234 bytes, found 99");
    }

    #[test]
    fn a_mode_mismatch_is_reported() {
        let fake = Fake::new(CpMode::CopyOtherMode);
        assert_eq!(msg(go(&fake)), "copied, but the mode differs");
    }

    #[test]
    fn sudo_refusal_names_the_restore_feature() {
        for text in ["sudo: a password is required", "Sorry, user fb is not allowed to execute '/bin/cp' as root", "sudo: no tty present and no askpass program specified", "Sorry, user fb may not run sudo on hub."] {
            let mut fake = Fake::new(CpMode::Fail);
            fake.stderr = text.into();
            assert_eq!(msg(go(&fake)), "restore is not enabled for this host: run zfsctl print-sudoers hub --with restore, install the file as the host's sudoers entry and try again");
        }
        let mut fake = Fake::new(CpMode::Fail);
        fake.stderr = "cp: /x: No space left on device\nsecond".into();
        let m = msg(go(&fake));
        assert!(m.contains("No space left on device") && !m.contains("second"), "{m}");
    }

    #[test]
    fn a_cp_error_that_only_mentions_the_sudo_words_is_not_a_sudo_refusal() {
        let mut fake = Fake::new(CpMode::Fail);
        fake.stderr = "cp: cannot stat 'not allowed to execute': No such file or directory".into();
        assert_eq!(msg(go(&fake)), "the copy failed: cp: cannot stat 'not allowed to execute': No such file or directory");
    }

    #[test]
    fn a_cp_that_could_not_be_run_may_or_may_not_have_finished() {
        let mut fake = Fake::new(CpMode::Copy);
        fake.cp_errors = true;
        let m = msg(go(&fake));
        assert_eq!(m, "the copy failed: ssh: connection dropped; it may or may not have finished: check the destination folder");
    }

    #[test]
    fn a_missing_source_is_reported() {
        let fake = Fake::new(CpMode::Copy);
        fake.set("/tank/data/.zfs/snapshot/daily-1/docs", vec![]);
        assert_eq!(msg(go(&fake)), "the file is no longer in the snapshot listing");
        let fake = Fake::new(CpMode::Copy);
        fake.set("/tank/data/.zfs/snapshot/daily-1/docs", vec![ent("-rw-r--r--", 7, "a.txt")]);
        assert_eq!(msg(go(&fake)), "the file in the snapshot is not a regular file of the expected size");
        assert!(fake.cps.lock().unwrap().is_empty());
    }

    #[test]
    fn cancel_before_the_copy_stops_before_cp() {
        let fake = Fake::new(CpMode::Copy);
        let src = pick();
        assert_eq!(msg(run_restore(&fake, "hub", &plan_for(&src), &src, &AtomicBool::new(true))), "cancelled");
        assert!(fake.cps.lock().unwrap().is_empty());
        assert!(fake.listed.lock().unwrap().is_empty());
    }

    #[test]
fn a_plan_that_does_not_match_the_pick_is_refused() {
        let want = "the restore plan does not match the picked file: nothing was copied";
        let fake = Fake::new(CpMode::Copy);
        let src = pick();
        let run = |plan: &RestorePlan| msg(run_restore(&fake, "hub", plan, &src, &AtomicBool::new(false)));
        let mut plan = plan_for(&src);
        plan.dest_folder = "/etc".into();
        assert_eq!(run(&plan), want);
        let mut plan = plan_for(&src);
        plan.command[4] = "/etc/passwd".into();
        assert_eq!(run(&plan), want);
        let mut plan = plan_for(&src);
        plan.command.insert(0, "-f".into());
        assert_eq!(run(&plan), want);
        let mut plan = plan_for(&src);
        plan.size = 1;
        assert_eq!(run(&plan), want);
        let mut plan = plan_for(&src);
        plan.mode = "-rwsrwxrwx".into();
        assert_eq!(run(&plan), want);
        assert!(fake.cps.lock().unwrap().is_empty());
        assert!(fake.listed.lock().unwrap().is_empty());
    }

    #[test]
    fn the_live_listing_refuses_odd_paths() {
        let fake = Fake::new(CpMode::Copy);
        let c = AtomicBool::new(false);
        for d in ["tank", "/a//b", "/a/./b", "/a/../b", "/a/b/", "/a\nb", "/a\u{fffd}b", ""] {
            assert!(read_live_folder(&fake, HostOs::FreeBsd, d, &BrowseLimits::default(), &c).is_err(), "{d:?}");
        }
        assert!(read_live_folder(&fake, HostOs::FreeBsd, "/", &BrowseLimits::default(), &c).is_ok());
    }

    const TWICE: &str = "two entries in {} have the name {} once escapes are decoded, or one of them cannot be decoded with certainty, so the restore is refused";

    fn twice(folder: &str, name: &str) -> String {
        TWICE.replacen("{}", folder, 1).replacen("{}", name, 1)
    }

    fn illumos_pick() -> RestoreSource {
        source("/tank/data", &["docs"], "a.txt", HostOs::Illumos)
    }

    fn go_on(fake: &Fake, src: &RestoreSource) -> Result<Restored> {
        run_restore(fake, "hub", &plan_for(src), src, &AtomicBool::new(false))
    }

    #[test]
    fn a_decoy_that_decodes_to_the_same_chain_name_is_refused_and_cp_never_runs() {
        // (a) a symlink called docs and a folder whose raw name is \144ocs (which decodes to docs).
        let fake = Fake::on(HostOs::Illumos, CpMode::Copy);
        fake.set("/tank/data", vec![ent("lrwxrwxrwx", 9, "\\144ocs"), ent("lrwxrwxrwx", 9, "docs -> /etc"), ent("drwxr-xr-x", 2, "\\144ocs")]);
        assert_eq!(msg(go_on(&fake, &illumos_pick())), twice("/tank/data", "docs"));
        assert!(fake.cps.lock().unwrap().is_empty());
        let fake = Fake::on(HostOs::Illumos, CpMode::Copy);
        fake.set("/tank/data", vec![ent("lrwxrwxrwx", 9, "docs -> /etc"), ent("drwxr-xr-x", 2, "\\144ocs")]);
        assert_eq!(msg(go_on(&fake, &illumos_pick())), twice("/tank/data", "docs"));
        assert!(fake.cps.lock().unwrap().is_empty());
    }

    #[test]
    fn a_single_ambiguous_chain_entry_is_refused() {
        // (b)
        let fake = Fake::on(HostOs::Illumos, CpMode::Copy);
        fake.set("/tank/data", vec![ent("drwxr-xr-x", 2, "\\144ocs")]);
        assert_eq!(msg(go_on(&fake, &illumos_pick())), twice("/tank/data", "docs"));
        assert!(fake.cps.lock().unwrap().is_empty());
    }

    #[test]
    fn a_duplicate_or_ambiguous_source_entry_is_refused() {
        // (c)
        let snap = "/tank/data/.zfs/snapshot/daily-1/docs";
        for rows in [vec![ent("-rw-r--r--", 1234, "a.txt"), ent("-rw-r--r--", 1234, "a.txt")], vec![ent("-rw-r--r--", 1234, "a.\\164xt")], vec![ent("-rw-r--r--", 1234, "a.txt"), ent("-rw-r--r--", 1234, "a.\\164xt")]] {
            let fake = Fake::on(HostOs::Illumos, CpMode::Copy);
            fake.set(snap, rows);
            assert_eq!(msg(go_on(&fake, &illumos_pick())), twice(snap, "a.txt"));
            assert!(fake.cps.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn an_ambiguous_entry_with_the_new_name_makes_the_absence_check_refuse() {
        // (e)
        let fake = Fake::on(HostOs::Illumos, CpMode::Copy);
        fake.set("/tank/data/docs", vec![ent("-rw-r--r--", 1234, "a.txt"), ent("-rw-r--r--", 5, "a.restored-daily-1.\\164xt")]);
        assert_eq!(msg(go_on(&fake, &illumos_pick())), "that name already exists in the folder: nothing was copied");
        assert!(fake.cps.lock().unwrap().is_empty());
    }

    #[test]
    fn a_duplicate_or_ambiguous_match_at_the_verify_step_is_not_verified() {
        // (d) cp ran.
        for mode in [CpMode::CopyTwice, CpMode::CopyAmbiguous] {
            let fake = Fake::on(HostOs::Illumos, mode);
            let m = msg(go_on(&fake, &illumos_pick()));
            assert_eq!(m, format!("the copy ran (cp exited 0) but its result could not be checked: {}", twice("/tank/data/docs", "a.restored-daily-1.txt")));
            assert_eq!(fake.cps.lock().unwrap().len(), 1);
        }
    }

    #[test]
    fn a_verify_listing_that_fails_or_is_cut_off_is_reported_as_unchecked_not_as_not_copied() {
        let mut fake = Fake::new(CpMode::Copy);
        fake.broken_after_cp = Some("/tank/data/docs".into());
        let m = msg(go(&fake));
        assert!(m.starts_with("the copy ran (cp exited 0) but its result could not be checked: "), "{m}");
        assert!(!m.contains("not copied"), "{m}");
        let mut fake = Fake::new(CpMode::Copy);
        fake.huge = vec![("/tank/data/docs".into(), true)];
        assert_eq!(msg(go(&fake)), "the copy ran (cp exited 0) but its result could not be checked: the folder /tank/data/docs is too large to check");
    }

    #[test]
    fn a_cut_off_listing_in_the_chain_or_the_absence_check_is_refused_and_cp_never_runs() {
        let mut fake = Fake::new(CpMode::Copy);
        fake.huge = vec![("/tank/data".into(), false)];
        assert_eq!(msg(go(&fake)), "the folder /tank/data is too large to check");
        assert!(fake.cps.lock().unwrap().is_empty());
        let mut fake = Fake::new(CpMode::Copy);
        fake.huge = vec![("/tank/data/docs".into(), false)];
        assert_eq!(msg(go(&fake)), "the folder /tank/data/docs is too large to check");
        assert!(fake.cps.lock().unwrap().is_empty());
    }

    #[test]
    fn a_root_mountpoint_restore_lists_in_order() {
        let fake = Fake::new(CpMode::Copy);
        fake.set("/.zfs/snapshot/daily-1/docs", vec![ent("-rw-r--r--", 1234, "a.txt")]);
        fake.set("/", vec![ent("drwxr-xr-x", 2, "docs")]);
        fake.set("/docs", vec![ent("-rw-r--r--", 1234, "a.txt")]);
        let src = source("/", &["docs"], "a.txt", HostOs::FreeBsd);
        let done = run_restore(&fake, "hub", &plan_for(&src), &src, &AtomicBool::new(false)).unwrap();
        assert_eq!(done.destination, "/docs/a.restored-daily-1.txt");
        assert_eq!(fake.directories(), vec!["/.zfs/snapshot/daily-1/docs", "/", "/docs", "<cp>", "/docs"]);
        assert_eq!(fake.cps.lock().unwrap()[0], ["-p", "-n", "--", "/.zfs/snapshot/daily-1/docs/a.txt", "/docs/a.restored-daily-1.txt"]);
    }

    #[test]
    fn a_cancel_during_cp_still_verifies_and_reports_the_result() {
        let flag = Arc::new(AtomicBool::new(false));
        let mut fake = Fake::new(CpMode::Copy);
        fake.cancel_at_cp = Some(flag.clone());
        let src = pick();
        let done = run_restore(&fake, "hub", &plan_for(&src), &src, &flag).unwrap();
        assert_eq!(done.size, 1234);
        assert_eq!(fake.directories().last().unwrap(), "/tank/data/docs");
    }

    #[test]
    fn a_cancel_in_the_middle_of_the_chain_stops_before_cp() {
        let flag = Arc::new(AtomicBool::new(false));
        let mut fake = Fake::new(CpMode::Copy);
        fake.cancel_at_list = Some(("/tank/data".into(), flag.clone()));
        let src = pick();
        assert_eq!(msg(run_restore(&fake, "hub", &plan_for(&src), &src, &flag)), "cancelled");
        assert!(fake.cps.lock().unwrap().is_empty());
    }
}
