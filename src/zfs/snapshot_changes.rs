//! What changed between two snapshots: the diff folded into counts per folder, with a capped list of entries.

// Nothing outside the tests calls this until the reader (Task 4) and the view (Task 5) exist; remove this then.
#![allow(dead_code)]

use crate::zfs::snapshot_diff::{relative_to_mount, Change, DiffEntry};
use std::collections::BTreeMap;

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
