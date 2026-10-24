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
    if src.os == HostOs::Illumos && dest_name.contains('\\') {
        return refuse("a backslash in a name cannot be told apart from an escape sequence on this host, so choose another name");
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

fn unreadable<T>(folder: &str) -> Result<T> {
    Err(AppError::Config(format!("some entries of {} could not be read, so it cannot be checked", crate::display_text::sanitise_for_display(folder))))
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
    if listing.unreadable > 0 {
        return unreadable(folder);
    }
    let mut hits = listing.entries.iter().map(|r| &r.entry).filter(|e| e.name == name);
    match (hits.next(), hits.next()) {
        (None, _) if listing.truncated => too_large(folder),
        (None, _) => Ok(None),
        (Some(e), None) if !e.ambiguous => Ok(Some(e)),
        _ => twice(folder, name),
    }
}

/// The first entry that is, or may be, the same name as `name`: an exact decoded match or one that differs only by case (a dataset
/// with casesensitivity=insensitive|mixed resolves them to one name, so a planted `EVIL.X` would take the copy). Ambiguous
/// entries count. Conservative on case-sensitive datasets: a harmless extra refusal. A listing that is cut off or has lines that
/// could not be read cannot show that a name is absent.
fn colliding(listing: &Listing, folder: &str, name: &str) -> Result<Option<String>> {
    let lower = name.to_lowercase();
    if let Some(hit) = listing.entries.iter().map(|r| &r.entry.name).find(|n| n.as_str() == name || n.to_lowercase() == lower) {
        return Ok(Some(hit.clone()));
    }
    if listing.unreadable > 0 {
        return unreadable(folder);
    }
    if listing.truncated { too_large(folder) } else { Ok(None) }
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

/// Fails closed on a dataset that normalizes names: a destination name with non-ASCII characters could then collide with an
/// existing entry that looks different byte for byte, which the absence check cannot see.
fn check_normalization(runner: &dyn CommandRunner, src: &RestoreSource, dest_name: &str) -> Result<()> {
    crate::zfs::not_an_option(&src.dataset)?;
    let unreadable = || AppError::Config("could not read the dataset's name normalization, so the restore is refused".to_string());
    let out = runner.run("zfs", &["get", "-H", "-o", "value", "normalization", &src.dataset]).map_err(|_| unreadable())?;
    let value = out.stdout.trim();
    if out.status != 0 || value.is_empty() {
        return Err(unreadable());
    }
    if value != "none" && !dest_name.is_ascii() {
        return Err(AppError::Config(format!(
            "this dataset normalizes names (normalization={}), so a name with non-ASCII characters cannot be checked safely: choose an ASCII name",
            crate::display_text::sanitise_for_display(value)
        )));
    }
    Ok(())
}

/// Refuses a destination folder that a child dataset is mounted over: the file would land in that dataset, not in the one the
/// snapshot belongs to (it was created after the snapshot, or the folder moved into it). Fails closed when the children cannot be read.
fn check_not_in_child_dataset(runner: &dyn CommandRunner, src: &RestoreSource, dest_folder: &str) -> Result<()> {
    let unreadable = || AppError::Config("could not read the dataset's child datasets, so the restore is refused".to_string());
    let out = runner.run("zfs", &["list", "-H", "-r", "-t", "filesystem", "-o", "name,mounted,mountpoint", &src.dataset]).map_err(|_| unreadable())?;
    if out.status != 0 {
        return Err(unreadable());
    }
    for line in out.stdout.lines().filter(|l| !l.is_empty()) {
        let fields: Vec<&str> = line.split('\t').collect();
        let [name, mounted, mountpoint] = fields[..] else { return Err(unreadable()) };
        if name == src.dataset || mounted != "yes" {
            continue;
        }
        let under = |m: &str| m != "/" && m.starts_with('/') && (dest_folder == m || dest_folder.starts_with(&format!("{m}/")));
        if under(mountpoint) {
            return Err(AppError::Config(format!(
                "the destination folder is now inside the child dataset {}, so the copy would not land in {}: restore from that dataset's own snapshot instead",
                crate::display_text::sanitise_for_display(name),
                crate::display_text::sanitise_for_display(&src.dataset)
            )));
        }
    }
    Ok(())
}

/// Checks, copies and verifies one restore. Nothing is retried, deleted or repaired: a failed check stops before `cp`, a failed
/// verify is reported as it is. `cp` exits 0 when a no-clobber copy skips, so the destination is checked absent before and
/// present, regular, of the planned size and mode after; the exit status alone is never trusted.
///
/// What this does NOT remove:
/// - The folders are checked right before `cp` runs, not while it runs. A local user who can write in one of them could swap it
///   for a symlink in that window; the window is short but it is not closed.
/// - The dataset's mountpoint (it comes from the zfs property) and every path above it are trusted and not checked.
/// - The same gap exists at the destination name: someone who can write to the destination folder can, between the absence check
///   and the copy, plant a directory or a symlink AT the new name, and the root `cp` then lands elsewhere on the host (copying
///   into a directory works on every OS; on Linux `-T` stops the directory case; FreeBSD also writes through a dangling
///   symlink). Restore only into folders you control when the dataset is shared with people you do not trust.
/// - An entry that appears at the destination between the absence check and the verify would be seen by the verify as if it
///   were the copy (the size and mode check is what limits that).
pub fn run_restore(runner: &dyn CommandRunner, host: &str, plan: &RestorePlan, src: &RestoreSource, cancel: &AtomicBool) -> Result<Restored> {
    // The plan must be exactly the one these picks produce; the chain is derived from the picks, not from the plan's text.
    if plan_restore(src, &plan.dest_name)? != *plan {
        return refuse("the restore plan does not match the picked file: nothing was copied");
    }
    check_cancel(cancel)?;
    check_normalization(runner, src, &plan.dest_name)?;
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
    check_not_in_child_dataset(runner, src, &dest_folder)?;

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
    match colliding(&listing, &dest_folder, &plan.dest_name)? {
        Some(hit) if hit == plan.dest_name => return refuse("that name already exists in the folder: nothing was copied"),
        Some(hit) => return Err(AppError::Config(format!("an entry whose name differs only by case already exists: {}", crate::display_text::sanitise_for_display(&hit)))),
        None => {}
    }

    // 4. Copy. The checks above are the last look before cp; see the doc comment for the window that stays open.
    check_cancel(cancel)?;
    let args: Vec<&str> = plan.command.iter().map(String::as_str).collect();
    // A sudo refusal means cp never ran. A start failure (Spawn) means the same. Any other failure (a non-zero status, always 255
    // when ssh dropped) may have left a file behind, so the destination is looked at before saying anything.
    let failure = match runner.run("cp", &args) {
        Ok(out) if out.status == 0 => None,
        Ok(out) => Some(format!("{}\n{}", out.stderr, out.stdout)),
        Err(AppError::CommandFailed { stderr, .. }) => Some(stderr),
        Err(e @ AppError::Spawn { .. }) => return Err(AppError::Config(format!("the copy was not started: {}", reason(&e)))),
        Err(e) => Some(reason(&e)),
    };
    if let Some(text) = failure {
        if sudo_refused(&text) {
            return refuse(&NOT_ENABLED.replace("HOST", host));
        }
        let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no message");
        let first = crate::display_text::sanitise_for_display(first);
        let looked = list(runner, host, src.os, &dest_folder, true, &AtomicBool::new(false)).and_then(|l| find_one(&l, &dest_folder, &plan.dest_name).map(|f| f.map(|e| (e.size, e.mode.clone()))));
        return Err(AppError::Config(match looked {
            Ok(None) => format!("the copy failed: {first}; nothing was found at the destination"),
            Ok(Some((size, mode))) => format!("the copy reported a failure ({first}) but a file of that name exists at the destination ({size} bytes, {mode}): it may be incomplete, check it before you use it"),
            Err(e) => format!("the copy failed ({first}) and the destination could not be checked: {}; check the destination folder", reason(&e)),
        }));
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
mod tests;
