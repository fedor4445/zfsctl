//! Marking snapshots in Snapshots and destroying the marked ones in one go.
#![allow(clippy::wildcard_imports)]
use super::*;

/// `dataset@name`, the key a mark is kept under.
fn key_of(s: &Snapshot) -> String {
    format!("{}@{}", s.dataset, s.name)
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
        self.marked_snapshots_host = self.selected_host;
        let keys: Vec<String> = self.snapshots.iter().skip(from).take(to - from + 1).map(key_of).collect();
        self.marked_snapshots.extend(keys);
    }

    /// The newest listed snapshot of each dataset that an enabled replication job of the selected host sends from or
    /// receives into (the job's tree, when it is recursive): the base the next incremental send starts from.
    fn replication_bases(&self) -> std::collections::BTreeSet<String> {
        let Some(host) = self.hosts.get(self.selected_host) else { return std::collections::BTreeSet::new() };
        let covers = |root: &str, dataset: &str, recursive: bool| dataset == root || (recursive && dataset.strip_prefix(root).is_some_and(|r| r.starts_with('/')));
        let used = |dataset: &str| {
            self.jobs_view.jobs.iter().filter(|j| j.enabled && j.kind == crate::jobs::KIND_REPLICATE).any(|j| {
                (&j.host == host && j.dataset.as_deref().is_some_and(|d| covers(d, dataset, j.recursive)))
                    || (j.dest_host.as_ref() == Some(host) && j.dest_dataset.as_deref().is_some_and(|d| covers(d, dataset, j.recursive)))
            })
        };
        let mut newest: std::collections::BTreeMap<&str, &Snapshot> = std::collections::BTreeMap::new();
        for s in &self.snapshots {
            newest.insert(s.dataset.as_str(), s);
        }
        newest.into_iter().filter(|(dataset, _)| used(dataset)).map(|(_, s)| key_of(s)).collect()
    }

    /// `d` with marks: one typed confirm for the marked snapshots, in list order. Held ones and replication bases are
    /// left out and named; the user types how many will be destroyed.
    pub fn begin_destroy_marked_snapshots(&mut self) {
        let bases = self.replication_bases();
        let mut items = Vec::new();
        let mut skipped = Vec::new();
        for s in self.snapshots.iter().filter(|s| self.marked_snapshots.contains(&key_of(s))) {
            if s.holds > 0 {
                skipped.push(format!("{} (has a hold)", key_of(s)));
            } else if bases.contains(&key_of(s)) {
                skipped.push(format!("{} (the newest one a replication job uses: its next incremental base)", key_of(s)));
            } else {
                items.push((s.dataset.clone(), s.name.clone()));
            }
        }
        let skipped_text = if skipped.is_empty() { String::new() } else { format!(" Left alone: {}.", skipped.join(", ")) };
        let (Some(first), Some(last)) = (items.first(), items.last()) else {
            self.set_error(format!("Nothing to destroy among the marked snapshots.{skipped_text}"));
            return;
        };
        let mut datasets: Vec<&str> = items.iter().map(|(d, _)| d.as_str()).collect();
        datasets.dedup();
        datasets.sort_unstable();
        datasets.dedup();
        let count = items.len();
        let prompt = format!(
            "Destroy {count} snapshot{} for good, in {}: from {}@{} to {}@{}.{skipped_text} Each is checked first (holds, a dry run); if one would fail, none is destroyed. Type {count} to go ahead:",
            if count == 1 { "" } else { "s" },
            datasets.join(", "),
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
