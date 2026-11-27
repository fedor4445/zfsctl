//! Marking snapshots in Snapshots and destroying the marked ones in one go.
#![allow(clippy::wildcard_imports)]
use super::*;

/// `dataset@name`, the key a mark is kept under.
fn key_of(s: &Snapshot) -> String {
    format!("{}@{}", s.dataset, s.name)
}

/// The first five of `list`, joined, with "and N more" for the rest: keeps the question short enough that its input line
/// stays on screen.
fn a_few(list: &[String]) -> String {
    match list.len() {
        0..=5 => list.join(", "),
        n => format!("{} and {} more", list.iter().take(5).cloned().collect::<Vec<_>>().join(", "), n - 5),
    }
}

impl App {
    /// `Space` in Snapshots: marks or unmarks the selected snapshot; a new mark is where `M` starts.
    pub fn toggle_snapshot_mark(&mut self) {
        let Some(key) = self.snapshots_table.selected().and_then(|i| self.snapshots.get(i)).map(key_of) else { return };
        self.marked_snapshots_host = self.selected_host;
        if self.marked_snapshots.remove(&key) {
            if self.snapshot_mark_anchor.as_deref() == Some(key.as_str()) {
                self.snapshot_mark_anchor = None;
            }
        } else {
            self.marked_snapshots.insert(key.clone());
            self.snapshot_mark_anchor = Some(key);
        }
    }

    /// `M` in Snapshots: marks every listed snapshot from the last one marked with `Space` to the selected one, both
    /// included, in either direction. With no such mark it marks the selected one only.
    pub fn mark_snapshots_to_cursor(&mut self) {
        let Some(cursor) = self.snapshots_table.selected().filter(|&i| i < self.snapshots.len()) else { return };
        let anchor = self.snapshot_mark_anchor.as_ref().and_then(|a| self.snapshots.iter().position(|s| &key_of(s) == a)).unwrap_or(cursor);
        let (from, to) = if anchor <= cursor { (anchor, cursor) } else { (cursor, anchor) };
        if self.snapshots.get(from).map(|s| &s.dataset) != self.snapshots.get(to).map(|s| &s.dataset) {
            self.set_error("M marks within one dataset: the last mark and this snapshot belong to different datasets".to_string());
            return;
        }
        self.marked_snapshots_host = self.selected_host;
        let keys: Vec<String> = self.snapshots.iter().skip(from).take(to - from + 1).map(key_of).collect();
        self.marked_snapshots.extend(keys);
    }

    /// Whether a replication job of the selected host (enabled or paused) sends from or receives into this snapshot's
    /// dataset and the snapshot carries the job's prefix. Its next incremental base is one of those, and only the other
    /// host knows which, so the batch leaves them all to the job's own pruning (a single `d` can still destroy one).
    fn replication_owned(&self, s: &Snapshot) -> bool {
        let Some(host) = self.hosts.get(self.selected_host) else { return false };
        let covers = |root: &str, recursive: bool| s.dataset == root || (recursive && s.dataset.strip_prefix(root).is_some_and(|r| r.starts_with('/')));
        self.jobs_view.jobs.iter().filter(|j| j.kind == crate::jobs::KIND_REPLICATE).any(|j| {
            let here = (&j.host == host && j.dataset.as_deref().is_some_and(|d| covers(d, j.recursive)))
                || (j.dest_host.as_ref() == Some(host) && j.dest_dataset.as_deref().is_some_and(|d| covers(d, j.recursive)));
            // No prefix known: every snapshot of the dataset may be the base.
            here && j.prefix.as_deref().is_none_or(|p| crate::daemon::snapshot::is_own(p, &s.name))
        })
    }

    /// `d` with marks: one typed confirm for the marked snapshots, in list order. Held ones and replication bases are
    /// left out and named; the user types how many will be destroyed.
    pub fn begin_destroy_marked_snapshots(&mut self) {
        let mut items = Vec::new();
        let mut skipped = Vec::new();
        for s in self.snapshots.iter().filter(|s| self.marked_snapshots.contains(&key_of(s))) {
            if s.holds > 0 {
                skipped.push(format!("{} (has a hold)", key_of(s)));
            } else if self.replication_owned(s) {
                skipped.push(format!("{} (a replication job's own snapshot: one of them is its next base)", key_of(s)));
            } else {
                items.push((s.dataset.clone(), s.name.clone()));
            }
        }
        let skipped_text = if skipped.is_empty() { String::new() } else { format!(" Left alone: {}.", a_few(&skipped)) };
        let (Some(first), Some(last)) = (items.first(), items.last()) else {
            self.set_error(format!("Nothing to destroy among the marked snapshots.{skipped_text}"));
            return;
        };
        let mut datasets: Vec<String> = items.iter().map(|(d, _)| d.clone()).collect();
        datasets.sort_unstable();
        datasets.dedup();
        let count = items.len();
        // The marks have done their job: a skipped one left behind would turn a later single `d` into a batch.
        self.marked_snapshots.clear();
        self.snapshot_mark_anchor = None;
        let prompt = format!(
            "Destroy {count} snapshot{} for good, in {}: from {}@{} to {}@{}.{skipped_text} Each is checked first (holds, a dry run); if one would fail, none is destroyed. Type {count} to go ahead:",
            if count == 1 { "" } else { "s" },
            a_few(&datasets),
            first.0,
            first.1,
            last.0,
            last.1
        );
        self.modal = Some(Modal::ConfirmTyped {
            prompt,
            expected: count.to_string(),
            input: String::new(),
            action: PendingAction { host: self.selected_host, section: Section::Snapshots, call: MutationCall::DestroySnapshots { items } },
        });
    }
}
