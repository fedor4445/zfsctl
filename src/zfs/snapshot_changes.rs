//! What changed between two snapshots: the diff folded into counts per folder, with a capped list of entries.


use crate::daemon::transfer::run_lines;
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use crate::zfs::snapshot_diff::{parse_diff_line, relative_to_mount, Change, DiffEntry};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The deepest grouping kept; shallower depths are derived from it, so changing the depth never re-reads the diff.
pub const MAX_DEPTH: usize = 4;

/// Where a read stops, and the most the summary keeps in memory. The three `max_` names are the plan's.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_field_names)]
pub struct Limits {
    pub max_lines: u64,
    pub max_seconds: u64,
    pub max_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_lines: 2_000_000, max_seconds: 120, max_entries: 100_000 }
    }
}

/// Why a read ended early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Lines,
    Time,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub added: u64,
    pub removed: u64,
    pub modified: u64,
    pub renamed: u64,
}

impl Counts {
    pub fn total(&self) -> u64 {
        self.added + self.removed + self.modified + self.renamed
    }

    fn bump(&mut self, change: Change) {
        match change {
            Change::Added => self.added += 1,
            Change::Removed => self.removed += 1,
            Change::Modified => self.modified += 1,
            Change::Renamed => self.renamed += 1,
        }
    }

    fn merge(&mut self, other: Counts) {
        self.added += other.added;
        self.removed += other.removed;
        self.modified += other.modified;
        self.renamed += other.renamed;
    }
}

/// A kept entry and the path as it is shown (relative to the dataset when that was possible).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub entry: DiffEntry,
    pub shown_path: String,
}

/// The folder of a shown path cut to `depth` components, with a trailing `/`; `/` for the dataset root.
fn folder_of(shown_path: &str, depth: usize) -> String {
    let parts: Vec<&str> = shown_path.split('/').filter(|p| !p.is_empty()).collect();
    let folder = parts.split_last().map_or(&[][..], |(_, parents)| parents);
    let cut: Vec<&str> = folder.iter().take(depth).copied().collect();
    if cut.is_empty() {
        "/".to_string()
    } else {
        format!("{}/", cut.join("/"))
    }
}

#[derive(Debug, Clone)]
pub struct ChangeSummary {
    mountpoint: Option<String>,
    max_entries: usize,
    totals: Counts,
    unreadable: u64,
    lines: u64,
    range: Option<(u64, u64)>,
    stopped: Option<Stop>,
    all_relative: bool,
    folders: BTreeMap<String, Counts>,
    rows: Vec<Row>,
    dropped: u64,
}

impl ChangeSummary {
    pub fn new(limits: &Limits, mountpoint: Option<String>) -> Self {
        Self {
            mountpoint,
            max_entries: limits.max_entries,
            totals: Counts::default(),
            unreadable: 0,
            lines: 0,
            range: None,
            stopped: None,
            all_relative: true,
            folders: BTreeMap::new(),
            rows: Vec::new(),
            dropped: 0,
        }
    }

    pub fn add(&mut self, entry: DiffEntry) {
        self.lines += 1;
        self.totals.bump(entry.change);
        self.range = Some(match self.range {
            Some((lo, hi)) => (lo.min(entry.changed_at), hi.max(entry.changed_at)),
            None => (entry.changed_at, entry.changed_at),
        });
        let relative = self.mountpoint.as_deref().and_then(|m| relative_to_mount(&entry.path, m));
        if relative.is_none() {
            self.all_relative = false;
        }
        let shown_path = relative.unwrap_or_else(|| entry.path.clone());
        self.folders.entry(folder_of(&shown_path, MAX_DEPTH)).or_default().bump(entry.change);
        if self.rows.len() < self.max_entries {
            self.rows.push(Row { entry, shown_path });
        } else {
            self.dropped += 1;
        }
    }

    pub fn add_unreadable(&mut self) {
        self.lines += 1;
        self.unreadable += 1;
    }

    pub fn totals(&self) -> Counts {
        self.totals
    }

    pub fn unreadable(&self) -> u64 {
        self.unreadable
    }

    /// Lines read so far (entries and unreadable lines).
    pub fn lines(&self) -> u64 {
        self.lines
    }

    pub fn time_range(&self) -> Option<(u64, u64)> {
        self.range
    }

    pub fn stopped(&self) -> Option<Stop> {
        self.stopped
    }

    pub fn set_stopped(&mut self, stop: Stop) {
        self.stopped = Some(stop);
    }

    /// False when any path could not be shown relative to the dataset.
    pub fn paths_relative(&self) -> bool {
        self.all_relative
    }

    pub fn entries_dropped(&self) -> u64 {
        self.dropped
    }

    /// The folders at `depth` (1 to `MAX_DEPTH`) with their counts, the busiest first, ties by name.
    pub fn groups(&self, depth: usize) -> Vec<(String, Counts)> {
        let mut merged: BTreeMap<String, Counts> = BTreeMap::new();
        for (folder, counts) in &self.folders {
            let key = if folder == "/" { folder.clone() } else { folder_of(&format!("{folder}x"), depth) };
            merged.entry(key).or_default().merge(*counts);
        }
        let mut out: Vec<(String, Counts)> = merged.into_iter().collect();
        out.sort_by(|a, b| b.1.total().cmp(&a.1.total()).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// The kept rows of one group, newest change first, at most `limit` of them, and how many more there are.
    pub fn entries_in(&self, group: &str, depth: usize, limit: usize) -> (Vec<&Row>, usize) {
        let mut rows: Vec<&Row> = self.rows.iter().filter(|r| folder_of(&r.shown_path, depth) == group).collect();
        rows.sort_by(|a, b| b.entry.changed_at.cmp(&a.entry.changed_at).then_with(|| a.shown_path.cmp(&b.shown_path)));
        let more = rows.len().saturating_sub(limit);
        rows.truncate(limit);
        (rows, more)
    }
}

/// The snapshot created just before `dataset@name`, by creation order (`createtxg`, not by name), as a full name.
/// `None` for the oldest; an error when `dataset@name` is not in the listing.
pub fn previous_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<Option<String>> {
    super::not_an_option(dataset)?;
    let args = ["list", "-H", "-t", "snapshot", "-d", "1", "-o", "name", "-s", "createtxg", dataset];
    let out = super::run_checked(runner, "zfs", &args, &format!("zfs list -t snapshot {dataset}"))?;
    let wanted = format!("{dataset}@{name}");
    let names: Vec<&str> = out.stdout.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let at = names.iter().position(|n| *n == wanted).ok_or_else(|| AppError::Parse(format!("{wanted} is not in the snapshot list")))?;
    Ok(at.checked_sub(1).and_then(|i| names.get(i)).map(|n| (*n).to_string()))
}

/// Where the dataset is mounted, or `None` when it has no path (`legacy`, `none`, `-`).
pub fn dataset_mountpoint(runner: &dyn CommandRunner, dataset: &str) -> Result<Option<String>> {
    super::not_an_option(dataset)?;
    let out = super::run_checked(runner, "zfs", &["get", "-H", "-o", "value", "mountpoint", dataset], &format!("zfs get mountpoint {dataset}"))?;
    let value = out.stdout.trim();
    Ok(value.starts_with('/').then(|| value.to_string()))
}

/// Streams `zfs diff -FHt -- <older> <newer>` into a summary. `older` is a full snapshot name; `newer` a full
/// snapshot name or the dataset itself. The read ends early, keeping what it has, when `limits` are reached or
/// `cancel` is set; the child is killed then. A diff that fails is an error with the first line of its stderr.
pub fn read_changes(runner: &dyn CommandRunner, older: &str, newer: &str, mountpoint: Option<String>, limits: &Limits, cancel: &AtomicBool) -> Result<ChangeSummary> {
    super::not_an_option(older)?;
    super::not_an_option(newer)?;
    let cmd = runner.command("zfs", &["diff", "-FHt", "--", older, newer])?;
    let stop = AtomicBool::new(false);
    let reason: Mutex<Option<Stop>> = Mutex::new(None);
    let finish = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(limits.max_seconds);
    let mut summary = ChangeSummary::new(limits, mountpoint);
    let note = |why: Stop| {
        let mut slot = reason.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.get_or_insert(why);
        stop.store(true, Ordering::SeqCst);
    };
    let ran = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !finish.load(Ordering::SeqCst) {
                if cancel.load(Ordering::SeqCst) {
                    note(Stop::Cancelled);
                    break;
                }
                if Instant::now() >= deadline {
                    note(Stop::Time);
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let ran = run_lines(cmd, &stop, |line| {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            match parse_diff_line(line) {
                Some(entry) => summary.add(entry),
                None => summary.add_unreadable(),
            }
            if summary.lines() >= limits.max_lines {
                note(Stop::Lines);
            }
        });
        finish.store(true, Ordering::SeqCst);
        ran
    });
    let lines = ran.map_err(|source| AppError::Spawn { cmd: "zfs diff".to_string(), source })?;
    let why = *reason.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(why) = why {
        summary.set_stopped(why);
        return Ok(summary);
    }
    if lines.code != Some(0) {
        let first = lines.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no message").to_string();
        return Err(AppError::CommandFailed { cmd: "zfs diff".to_string(), status: lines.code.unwrap_or(-1), stderr: first });
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zfs::snapshot_diff::{Change, DiffEntry, FileKind};

    fn entry(path: &str, change: Change, at: u64) -> DiffEntry {
        DiffEntry { changed_at: at, change, kind: FileKind::File, path: path.to_string(), renamed_to: None }
    }

    fn summary(mount: Option<&str>) -> ChangeSummary {
        ChangeSummary::new(&Limits::default(), mount.map(str::to_string))
    }

    fn group(s: &ChangeSummary, depth: usize) -> Vec<(String, u64)> {
        s.groups(depth).into_iter().map(|(name, c)| (name, c.total())).collect()
    }

    #[test]
    fn counts_each_kind_and_the_total() {
        let mut s = summary(Some("/ds"));
        for _ in 0..3 {
            s.add(entry("/ds/a", Change::Added, 1));
        }
        for _ in 0..2 {
            s.add(entry("/ds/b", Change::Removed, 1));
        }
        for _ in 0..4 {
            s.add(entry("/ds/c", Change::Modified, 1));
        }
        s.add(entry("/ds/d", Change::Renamed, 1));
        let t = s.totals();
        assert_eq!((t.added, t.removed, t.modified, t.renamed, t.total()), (3, 2, 4, 1, 10));
    }

    #[test]
    fn groups_by_the_folder_at_the_depth_asked_for() {
        let mut s = summary(Some("/ds"));
        for p in ["/ds/a/b/c/f1", "/ds/a/b/c/f2", "/ds/a/b/g", "/ds/a/h", "/ds/top"] {
            s.add(entry(p, Change::Added, 1));
        }
        assert_eq!(group(&s, 1), [("a/".to_string(), 4), ("/".to_string(), 1)]);
        assert_eq!(group(&s, 2), [("a/b/".to_string(), 3), ("/".to_string(), 1), ("a/".to_string(), 1)]);
        assert_eq!(group(&s, 4), [("a/b/c/".to_string(), 2), ("/".to_string(), 1), ("a/".to_string(), 1), ("a/b/".to_string(), 1)]);
    }

    #[test]
    fn the_dataset_root_directory_line_counts_under_slash() {
        let mut s = summary(Some("/ds"));
        let mut root = entry("/ds/", Change::Modified, 5);
        root.kind = FileKind::Directory;
        s.add(root);
        assert_eq!(group(&s, 2), [("/".to_string(), 1)]);
        assert!(s.paths_relative());
    }

    #[test]
    fn zone_root_paths_group_by_their_dataset_relative_folder() {
        let mut s = summary(Some("/ds"));
        s.add(entry("/rpool/zones/z/root/ds/state/x", Change::Modified, 1));
        assert_eq!(group(&s, 2), [("state/".to_string(), 1)]);
        assert!(s.paths_relative());
    }

    #[test]
    fn a_path_that_cannot_be_made_relative_keeps_the_raw_path_and_flags_it() {
        let mut s = summary(None);
        s.add(entry("/var/data/sub/f", Change::Added, 1));
        assert_eq!(group(&s, 2), [("var/data/".to_string(), 1)]);
        assert!(!s.paths_relative());
        let (rows, more) = s.entries_in("var/data/", 2, 10);
        assert_eq!((rows.len(), more), (1, 0));
        assert_eq!(rows[0].shown_path, "/var/data/sub/f");
    }

    #[test]
    fn entries_are_capped_but_counts_are_not() {
        let mut s = ChangeSummary::new(&Limits { max_entries: 3, ..Limits::default() }, Some("/ds".to_string()));
        for i in 0..5 {
            s.add(entry(&format!("/ds/f{i}"), Change::Added, i));
        }
        assert_eq!(s.entries_dropped(), 2);
        assert_eq!(s.totals().total(), 5);
        assert_eq!(s.lines(), 5);
    }

    #[test]
    fn entries_in_returns_the_rows_of_one_group_newest_first_and_how_many_more() {
        let mut s = summary(Some("/ds"));
        for i in 0..10 {
            s.add(entry(&format!("/ds/a/f{i}"), Change::Added, 100 + i));
        }
        s.add(entry("/ds/b/other", Change::Added, 1));
        let (rows, more) = s.entries_in("a/", 2, 4);
        assert_eq!((rows.len(), more), (4, 6));
        assert_eq!(rows[0].entry.changed_at, 109, "newest first");
        assert_eq!(rows[3].entry.changed_at, 106);
        let (none, zero) = s.entries_in("zzz/", 2, 4);
        assert_eq!((none.len(), zero), (0, 0));
    }

    #[test]
    fn time_range_is_min_and_max_and_unreadable_lines_are_counted() {
        let mut s = summary(Some("/ds"));
        assert_eq!(s.time_range(), None);
        s.add(entry("/ds/x", Change::Added, 50));
        s.add(entry("/ds/y", Change::Added, 20));
        s.add(entry("/ds/z", Change::Added, 90));
        s.add_unreadable();
        s.add_unreadable();
        assert_eq!(s.time_range(), Some((20, 90)));
        assert_eq!((s.unreadable(), s.lines()), (2, 5));
    }

    #[test]
    fn a_stop_is_remembered() {
        let mut s = summary(None);
        assert_eq!(s.stopped(), None);
        s.set_stopped(Stop::Lines);
        assert_eq!(s.stopped(), Some(Stop::Lines));
    }
}

#[cfg(test)]
mod reader_tests {
    use super::*;
    use crate::error::{AppError, Result};
    use crate::exec::{CommandOutput, CommandRunner};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Answers `zfs list` and `zfs get` from canned text and hands out `sh -c <script>` as the diff command.
    struct DiffRunner {
        list_out: String,
        mount_out: String,
        script: String,
        calls: Mutex<Vec<Vec<String>>>,
        spawned: Mutex<Vec<Vec<String>>>,
    }

    impl DiffRunner {
        fn new(script: &str) -> Self {
            Self { list_out: String::new(), mount_out: String::new(), script: script.to_string(), calls: Mutex::new(vec![]), spawned: Mutex::new(vec![]) }
        }
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
        fn spawned(&self) -> Vec<Vec<String>> {
            self.spawned.lock().unwrap().clone()
        }
    }

    impl CommandRunner for DiffRunner {
        fn run(&self, _cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(args.iter().map(|a| (*a).to_string()).collect());
            let out = match args.first().copied() {
                Some("list") => self.list_out.clone(),
                Some("get") => self.mount_out.clone(),
                other => return Err(AppError::Config(format!("unexpected zfs {other:?}"))),
            };
            Ok(CommandOutput { stdout: out, stderr: String::new(), status: 0 })
        }
        fn run_with_stdin(&self, _cmd: &str, _args: &[&str], _stdin: &str) -> Result<CommandOutput> {
            Err(AppError::Config("no stdin here".to_string()))
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn command(&self, _cmd: &str, args: &[&str]) -> Result<Command> {
            self.spawned.lock().unwrap().push(args.iter().map(|a| (*a).to_string()).collect());
            let mut c = Command::new("sh");
            c.args(["-c", &self.script]);
            Ok(c)
        }
    }

    const FIVE_LINES_AND_GARBAGE: &str = "printf '1.0\\t+\\tF\\t/ds/a/f1\\n1.0\\t+\\tF\\t/ds/a/f2\\n2.0\\tM\\t/\\t/ds/\\n3.0\\t-\\tF\\t/ds/b/g\\n4.0\\tR\\tF\\t/ds/c/o\\t/ds/c/n\\ngarbage\\n'";

    #[test]
    fn previous_snapshot_is_the_one_before_by_creation_order() {
        let mut r = DiffRunner::new("true");
        r.list_out = "tank/d@a\ntank/d@b\ntank/d@c\n".to_string();
        assert_eq!(previous_snapshot(&r, "tank/d", "b").unwrap().as_deref(), Some("tank/d@a"));
        assert_eq!(previous_snapshot(&r, "tank/d", "c").unwrap().as_deref(), Some("tank/d@b"));
        assert_eq!(previous_snapshot(&r, "tank/d", "a").unwrap(), None);
        assert!(previous_snapshot(&r, "tank/d", "zz").is_err());
        assert_eq!(r.calls()[0], ["list", "-H", "-t", "snapshot", "-d", "1", "-o", "name", "-s", "createtxg", "tank/d"]);
    }

    #[test]
    fn a_dataset_name_starting_with_a_dash_is_refused_and_nothing_runs() {
        let r = DiffRunner::new("true");
        let cancel = AtomicBool::new(false);
        assert!(previous_snapshot(&r, "-x", "a").is_err());
        assert!(dataset_mountpoint(&r, "-x").is_err());
        assert!(read_changes(&r, "-x@a", "tank/d@b", None, &Limits::default(), &cancel).is_err());
        assert!(read_changes(&r, "tank/d@a", "-x", None, &Limits::default(), &cancel).is_err());
        assert!(r.calls().is_empty() && r.spawned().is_empty());
    }

    #[test]
    fn mountpoint_words_that_are_not_paths_become_none() {
        let mut r = DiffRunner::new("true");
        for word in ["legacy\n", "none\n", "-\n", "\n"] {
            r.mount_out = word.to_string();
            assert_eq!(dataset_mountpoint(&r, "tank/d").unwrap(), None, "{word:?}");
        }
        r.mount_out = "/ds\n".to_string();
        assert_eq!(dataset_mountpoint(&r, "tank/d").unwrap().as_deref(), Some("/ds"));
        assert_eq!(r.calls().last().unwrap(), &["get", "-H", "-o", "value", "mountpoint", "tank/d"]);
    }

    #[test]
    fn read_changes_folds_a_streamed_diff() {
        let r = DiffRunner::new(FIVE_LINES_AND_GARBAGE);
        let s = read_changes(&r, "tank/d@a", "tank/d@b", Some("/ds".to_string()), &Limits::default(), &AtomicBool::new(false)).unwrap();
        let t = s.totals();
        assert_eq!((t.added, t.removed, t.modified, t.renamed), (2, 1, 1, 1));
        assert_eq!((s.unreadable(), s.lines(), s.stopped()), (1, 6, None));
        assert_eq!(s.groups(2).iter().map(|(n, c)| (n.as_str(), c.total())).collect::<Vec<_>>(), [("a/", 2), ("/", 1), ("b/", 1), ("c/", 1)]);
        assert_eq!(r.spawned()[0], ["diff", "-FHt", "--", "tank/d@a", "tank/d@b"]);
    }

    #[test]
    fn the_line_limit_stops_the_read_and_says_so() {
        let r = DiffRunner::new("i=0; while [ $i -lt 10 ]; do printf '1.0\\t+\\tF\\t/ds/f%s\\n' $i; i=$((i+1)); done; sleep 30");
        let started = Instant::now();
        let limits = Limits { max_lines: 3, ..Limits::default() };
        let s = read_changes(&r, "tank/d@a", "tank/d@b", Some("/ds".to_string()), &limits, &AtomicBool::new(false)).unwrap();
        assert_eq!(s.stopped(), Some(Stop::Lines));
        assert_eq!(s.lines(), 3, "nothing is added once the limit is hit");
        assert!(started.elapsed() < Duration::from_secs(10), "the child was killed, not waited for");
    }

    #[test]
    fn the_time_limit_stops_the_read() {
        let r = DiffRunner::new("printf '1.0\\t+\\tF\\t/ds/x\\n'; sleep 30");
        let started = Instant::now();
        let limits = Limits { max_seconds: 1, ..Limits::default() };
        let s = read_changes(&r, "tank/d@a", "tank/d@b", None, &limits, &AtomicBool::new(false)).unwrap();
        assert_eq!((s.stopped(), s.lines()), (Some(Stop::Time), 1));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn cancel_stops_the_read_and_keeps_what_was_read() {
        let r = DiffRunner::new("printf '1.0\\t+\\tF\\t/ds/x\\n'; sleep 30");
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let s = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(400));
                cancel.store(true, Ordering::SeqCst);
            });
            read_changes(&r, "tank/d@a", "tank/d@b", None, &Limits::default(), &cancel).unwrap()
        });
        assert_eq!((s.stopped(), s.lines()), (Some(Stop::Cancelled), 1));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_failing_diff_is_an_error_with_the_first_stderr_line() {
        let r = DiffRunner::new("echo \"cannot open 'x': dataset does not exist\" >&2; echo second >&2; exit 1");
        let err = read_changes(&r, "tank/d@a", "tank/d@b", None, &Limits::default(), &AtomicBool::new(false)).unwrap_err();
        match err {
            AppError::CommandFailed { stderr, status, .. } => {
                assert!(stderr.contains("cannot open 'x': dataset does not exist"), "{stderr:?}");
                assert!(!stderr.contains("second"), "only the first line: {stderr:?}");
                assert_eq!(status, 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_runner_that_cannot_hand_out_a_command_is_an_error() {
        let r = crate::exec::test_support::FakeRunner::new("");
        assert!(read_changes(&r, "tank/d@a", "tank/d@b", None, &Limits::default(), &AtomicBool::new(false)).is_err());
    }
}
