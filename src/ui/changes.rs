//! The changes view of the Snapshots section: what changed between a snapshot and the one before it (or the live
//! dataset), grouped by folder. It only reads; the one action it offers is holding the older snapshot.

use crate::display_text::{sanitise_for_display, truncate_with_ellipsis};
use crate::error::{AppError, Result};
use crate::ui::theme::Theme;
use crate::zfs::snapshot_changes::{ChangeSummary, Counts, Row as ChangeRow, Stop, MAX_DEPTH};
use crate::zfs::snapshot_diff::Change;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Entries listed for one group; the rest are counted ("... N more").
pub const ENTRIES_PER_GROUP: usize = 5_000;
/// Folders listed; the busiest come first and the rest are counted ("... N more folders"). The totals stay exact.
pub const GROUPS_SHOWN: usize = 5_000;
const DEFAULT_DEPTH: usize = 2;

/// What `c` and `C` compare with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Against {
    /// The snapshot before the selected one (`c`).
    Previous,
    /// The live dataset, as it is now (`C`).
    Live,
}

/// A read the key handler queued; the event loop gives it its host's runner.
#[derive(Debug, Clone)]
pub struct PendingChanges {
    pub host: usize,
    pub request: u64,
    pub dataset: String,
    pub name: String,
    pub against: Against,
    pub cancel: Arc<AtomicBool>,
}

/// What the worker found. One of these exists per read, so the size of `Done` does not matter.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ChangesOutcome {
    NoPrevious,
    Done { older: String, newer: String, summary: ChangeSummary },
}

#[derive(Debug)]
pub enum Phase {
    Reading,
    NoPrevious,
    Failed(String),
    Ready { older: String, newer: String, summary: ChangeSummary },
}

#[derive(Debug)]
pub struct ChangesView {
    pub host: usize,
    pub request: u64,
    pub dataset: String,
    pub name: String,
    pub against: Against,
    pub cancel: Arc<AtomicBool>,
    pub phase: Phase,
    pub depth: usize,
    pub selected: usize,
    pub open_group: Option<String>,
    /// The folders at the current depth (at most `GROUPS_SHOWN`), computed when the result arrives or the depth
    /// changes, so a redraw or a key press never regroups the diff; how many more there are.
    pub groups: Vec<(String, Counts)>,
    pub groups_more: usize,
    /// The rows of the open folder, copied when it is opened; how many more there are.
    pub entries: Vec<ChangeRow>,
    pub entries_more: usize,
}

/// 2505 as `2,505`.
pub fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn time_text(secs: u64) -> String {
    i64::try_from(secs)
        .ok()
        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
        .map_or_else(|| "?".to_string(), |t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
}

impl ChangesView {
    pub fn new(p: &PendingChanges) -> Self {
        Self {
            host: p.host,
            request: p.request,
            dataset: p.dataset.clone(),
            name: p.name.clone(),
            against: p.against,
            cancel: Arc::clone(&p.cancel),
            phase: Phase::Reading,
            depth: DEFAULT_DEPTH,
            selected: 0,
            open_group: None,
            groups: Vec::new(),
            groups_more: 0,
            entries: Vec::new(),
            entries_more: 0,
        }
    }

    pub fn apply(&mut self, result: Result<ChangesOutcome>) {
        self.phase = match result {
            Ok(ChangesOutcome::NoPrevious) => Phase::NoPrevious,
            Ok(ChangesOutcome::Done { older, newer, summary }) => Phase::Ready { older, newer, summary },
            Err(e) => Phase::Failed(truncate_with_ellipsis(&sanitise_for_display(&friendly(&e)), 300)),
        };
        self.selected = 0;
        self.open_group = None;
        self.entries.clear();
        self.recompute_groups();
    }

    fn recompute_groups(&mut self) {
        let Phase::Ready { summary, .. } = &self.phase else {
            self.groups.clear();
            self.groups_more = 0;
            return;
        };
        let mut all = summary.groups(self.depth);
        self.groups_more = all.len().saturating_sub(GROUPS_SHOWN);
        all.truncate(GROUPS_SHOWN);
        self.groups = all;
    }

    /// How many rows the list on screen has: the groups, or the entries of the open group.
    pub fn list_len(&self) -> usize {
        if self.open_group.is_some() {
            self.entries.len()
        } else {
            self.groups.len()
        }
    }

    pub fn move_selection(&mut self, down: bool) {
        let len = self.list_len();
        if len == 0 {
            return;
        }
        self.selected = if down { (self.selected + 1).min(len - 1) } else { self.selected.saturating_sub(1) };
    }

    /// Enter on a group: list its entries.
    pub fn open_selected(&mut self) {
        let Phase::Ready { summary, .. } = &self.phase else { return };
        if self.open_group.is_some() {
            return;
        }
        if let Some((name, _)) = self.groups.get(self.selected) {
            let (rows, more) = summary.entries_in(name, self.depth, ENTRIES_PER_GROUP);
            self.entries = rows.into_iter().cloned().collect();
            self.entries_more = more;
            self.open_group = Some(name.clone());
            self.selected = 0;
        }
    }

    /// Esc: closes the open group; `false` when none was open (the view itself is next to close).
    pub fn back(&mut self) -> bool {
        let closed = self.open_group.take().is_some();
        if closed {
            self.selected = 0;
            self.entries.clear();
        }
        closed
    }

    pub fn set_depth(&mut self, depth: usize) {
        self.depth = depth.clamp(1, MAX_DEPTH);
        self.selected = 0;
        self.open_group = None;
        self.entries.clear();
        self.recompute_groups();
    }

    /// The older snapshot as (dataset, name), once the diff is ready: what `H` would protect.
    pub fn older_snapshot(&self) -> Option<(String, String)> {
        let Phase::Ready { older, .. } = &self.phase else { return None };
        older.split_once('@').map(|(d, n)| (d.to_string(), n.to_string()))
    }

    /// The lines above the list: the counts, the time range and what the user should know about the read.
    fn header(&self) -> Vec<String> {
        let Phase::Ready { summary, .. } = &self.phase else { return Vec::new() };
        let t = summary.totals();
        let mut lines = vec![format!(
            "{} removed · {} added · {} modified · {} renamed",
            group_digits(t.removed),
            group_digits(t.added),
            group_digits(t.modified),
            group_digits(t.renamed)
        )];
        if let Some((first, last)) = summary.time_range() {
            lines.push(format!("changed between {} and {}", time_text(first), time_text(last)));
        }
        if let Some(stop) = summary.stopped() {
            let why = match stop {
                Stop::Lines => "line limit",
                Stop::Time => "time limit",
                Stop::Cancelled => "cancelled",
            };
            lines.push(format!("stopped after {} changes ({why}): the counts are partial", group_digits(summary.lines())));
        }
        if summary.unreadable() > 0 {
            lines.push(format!("{} unreadable line(s) were counted, not shown", group_digits(summary.unreadable())));
        }
        if self.groups_more > 0 {
            lines.push(format!("{} more folders are counted but not listed (the busiest {} are)", group_digits(self.groups_more as u64), group_digits(GROUPS_SHOWN as u64)));
        }
        if summary.entries_dropped() > 0 {
            lines.push(format!("{} entries are counted but not listed", group_digits(summary.entries_dropped())));
        }
        if !summary.paths_relative() {
            lines.push("paths as the host prints them".to_string());
        }
        lines
    }
}

fn friendly(e: &AppError) -> String {
    e.to_string()
}

fn change_mark(c: Change) -> &'static str {
    match c {
        Change::Added => "+",
        Change::Removed => "-",
        Change::Modified => "~",
        Change::Renamed => "R",
    }
}

/// Draws the view in `area`, in place of the snapshot list.
pub fn draw(f: &mut Frame, view: &ChangesView, theme: &Theme, host: &str, area: Rect) {
    let what = match (&view.phase, view.against) {
        (Phase::Ready { older, newer, .. }, Against::Previous) => format!("{newer} vs {older}"),
        (Phase::Ready { older, newer, .. }, Against::Live) => format!("{older} → now ({newer})"),
        (_, Against::Previous) => format!("{}@{} vs the previous snapshot", view.dataset, view.name),
        (_, Against::Live) => format!("{}@{} → now", view.dataset, view.name),
    };
    let title = format!("Changes — {host} · {}", sanitise_for_display(&what));
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let say = |f: &mut Frame, text: &str, style: ratatui::style::Style| {
        f.render_widget(Paragraph::new(text.to_string()).style(style).wrap(Wrap { trim: false }), inner);
    };
    match &view.phase {
        Phase::Reading => say(f, "reading changes… (Esc cancels)", theme.dim()),
        Phase::NoPrevious => say(
            f,
            "This is the oldest snapshot of the dataset: there is nothing before it to compare with.\nPress Esc, then C to compare it with the live dataset.",
            theme.dim(),
        ),
        Phase::Failed(text) => say(f, &format!("Could not read the changes: {text}"), theme.err),
        Phase::Ready { .. } => {
            let header = view.header();
            let [top, list] = Layout::vertical([Constraint::Length(u16::try_from(header.len()).unwrap_or(4).saturating_add(1)), Constraint::Min(1)]).areas(inner);
            let lines: Vec<Line> = header.iter().map(|l| Line::from(sanitise_for_display(l))).collect();
            f.render_widget(Paragraph::new(lines), top);
            let mut state = TableState::default();
            state.select(Some(view.selected));
            if view.open_group.is_none() {
                let mut rows: Vec<Row> = view
                    .groups
                    .iter()
                    .map(|(name, c)| {
                        Row::new(vec![
                            truncate_with_ellipsis(&sanitise_for_display(name), 60),
                            group_digits(c.added),
                            group_digits(c.removed),
                            group_digits(c.modified),
                            group_digits(c.renamed),
                        ])
                    })
                    .collect();
                if view.groups_more > 0 {
                    rows.push(Row::new(vec![format!("… {} more folders", group_digits(view.groups_more as u64))]));
                }
                let widths = [Constraint::Min(20), Constraint::Length(9), Constraint::Length(9), Constraint::Length(9), Constraint::Length(9)];
                let table = Table::new(rows, widths).header(Row::new(vec!["Folder", "Added", "Removed", "Modified", "Renamed"])).row_highlight_style(theme.selected_row());
                f.render_stateful_widget(table, list, &mut state);
            } else {
                let mut table_rows: Vec<Row> = view
                    .entries
                    .iter()
                    .map(|r| {
                        let mut path = sanitise_for_display(&r.shown_path);
                        if let Some(new) = &r.shown_renamed_to {
                            path = format!("{path} → {}", sanitise_for_display(new));
                        }
                        Row::new(vec![
                            format!("{} {}", change_mark(r.entry.change), r.entry.change.word()),
                            r.entry.kind.word().to_string(),
                            time_text(r.entry.changed_at),
                            path,
                        ])
                    })
                    .collect();
                if view.entries_more > 0 {
                    table_rows.push(Row::new(vec![format!("… {} more", group_digits(view.entries_more as u64))]));
                }
                let widths = [Constraint::Length(12), Constraint::Length(17), Constraint::Length(20), Constraint::Min(20)];
                let table = Table::new(table_rows, widths).header(Row::new(vec!["Change", "Type", "Changed", "Path"])).row_highlight_style(theme.selected_row());
                f.render_stateful_widget(table, list, &mut state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use crate::zfs::snapshot_changes::{ChangeSummary, Limits, Stop};
    use crate::zfs::snapshot_diff::{Change, DiffEntry, FileKind};
    use ratatui::{backend::TestBackend, Terminal};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn pending(against: Against) -> PendingChanges {
        PendingChanges { host: 0, request: 7, dataset: "tank/data".to_string(), name: "s2".to_string(), against, cancel: Arc::new(AtomicBool::new(false)) }
    }

    fn entry(path: &str, change: Change, at: u64) -> DiffEntry {
        DiffEntry { changed_at: at, change, kind: FileKind::File, path: path.to_string(), renamed_to: None }
    }

    fn ready(mount: Option<&str>, entries: &[(&str, Change)]) -> ChangesView {
        let mut summary = ChangeSummary::new(&Limits::default(), mount.map(str::to_string));
        for (i, (path, change)) in entries.iter().enumerate() {
            summary.add(entry(path, *change, 1_788_654_605 + i as u64));
        }
        let mut view = ChangesView::new(&pending(Against::Previous));
        view.apply(Ok(ChangesOutcome::Done { older: "tank/data@s1".to_string(), newer: "tank/data@s2".to_string(), summary }));
        view
    }

    fn draw_to_rows(view: &ChangesView, w: u16, h: u16) -> Vec<String> {
        let theme = crate::ui::state::App::new(vec!["local".to_string()]).theme.clone();
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, view, &theme, "local", f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect()
    }

    #[test]
    fn digits_are_grouped_by_thousands() {
        assert_eq!(
            [0, 999, 1000, 2505, 1_234_567].map(group_digits),
            ["0", "999", "1,000", "2,505", "1,234,567"]
        );
    }

    #[test]
    fn a_new_view_is_reading_at_depth_two() {
        let v = ChangesView::new(&pending(Against::Previous));
        assert!(matches!(v.phase, Phase::Reading));
        assert_eq!((v.depth, v.selected, v.open_group.as_deref(), v.request, v.host), (2, 0, None, 7, 0));
    }

    #[test]
    fn a_result_sets_the_phase_and_an_error_is_stored_sanitised() {
        let mut v = ChangesView::new(&pending(Against::Previous));
        v.apply(Ok(ChangesOutcome::NoPrevious));
        assert!(matches!(v.phase, Phase::NoPrevious));
        v.apply(Err(AppError::CommandFailed { cmd: "zfs diff".to_string(), status: 1, stderr: "bad\u{1b}[31m thing".to_string() }));
        match &v.phase {
            Phase::Failed(text) => assert!(!text.chars().any(char::is_control) && text.contains("bad"), "{text:?}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn up_down_select_groups_and_enter_lists_the_entries_of_the_group() {
        let mut v = ready(Some("/ds"), &[("/ds/a/f1", Change::Added), ("/ds/a/f2", Change::Added), ("/ds/a/f3", Change::Added), ("/ds/b/g", Change::Removed)]);
        assert_eq!(v.selected, 0);
        v.move_selection(true);
        assert_eq!(v.selected, 1);
        v.move_selection(true);
        assert_eq!(v.selected, 1, "stays on the last group");
        v.move_selection(false);
        v.open_selected();
        assert_eq!(v.open_group.as_deref(), Some("a/"));
        assert_eq!(v.selected, 0);
        assert!(v.back(), "the first Esc closes the group");
        assert_eq!(v.open_group, None);
        assert!(!v.back(), "the second has no group left to close");
    }

    #[test]
    fn brackets_change_the_depth_between_one_and_four_and_reset_the_selection() {
        let mut v = ready(Some("/ds"), &[("/ds/a/b/f", Change::Added)]);
        v.selected = 3;
        v.set_depth(0);
        assert_eq!((v.depth, v.selected, v.open_group.as_deref()), (1, 0, None));
        v.set_depth(9);
        assert_eq!(v.depth, 4);
    }

    #[test]
    fn a_view_with_very_many_folders_lists_a_capped_number_and_counts_the_rest() {
        let mut summary = ChangeSummary::new(&Limits::default(), Some("/ds".to_string()));
        for i in 0..(GROUPS_SHOWN + 1_000) {
            summary.add(entry(&format!("/ds/d{i}/f"), Change::Added, 1_788_654_605));
        }
        let mut v = ChangesView::new(&pending(Against::Previous));
        v.apply(Ok(ChangesOutcome::Done { older: "tank/data@s1".to_string(), newer: "tank/data@s2".to_string(), summary }));
        assert_eq!(v.list_len(), GROUPS_SHOWN, "only the busiest folders are listed");
        assert_eq!(v.groups_more, 1_000);
        let text = draw_to_rows(&v, 110, 14).join("\n");
        assert!(text.contains("1,000 more folders"), "{text}");
        assert!(text.contains(&format!("{} added", group_digits((GROUPS_SHOWN + 1_000) as u64))) || text.contains("0 removed"), "the totals still count every folder: {text}");
    }

    #[test]
    fn the_groups_and_the_open_folders_rows_are_kept_not_recomputed() {
        let mut v = ready(Some("/ds"), &[("/ds/a/f1", Change::Added), ("/ds/a/f2", Change::Added), ("/ds/b/g", Change::Removed)]);
        assert_eq!(v.groups.len(), 2, "computed when the result arrived");
        v.set_depth(1);
        assert_eq!(v.groups.len(), 2);
        v.open_selected();
        assert_eq!(v.entries.len(), 2, "the open folder's rows are cached at Enter");
        assert!(v.back());
        assert!(v.entries.is_empty());
    }

    #[test]
    fn the_older_snapshot_is_split_into_dataset_and_name_only_when_ready() {
        let v = ready(Some("/ds"), &[("/ds/a", Change::Added)]);
        assert_eq!(v.older_snapshot(), Some(("tank/data".to_string(), "s1".to_string())));
        assert_eq!(ChangesView::new(&pending(Against::Previous)).older_snapshot(), None);
    }

    #[test]
    fn the_header_reports_counts_the_time_range_unreadable_lines_and_a_stop() {
        let mut summary = ChangeSummary::new(&Limits::default(), Some("/ds".to_string()));
        for i in 0..2505 {
            summary.add(entry(&format!("/ds/Camera/f{i}"), Change::Removed, 1_788_654_605 + i));
        }
        summary.add(entry("/ds/x", Change::Added, 1_788_654_700));
        summary.add_unreadable();
        summary.set_stopped(Stop::Lines);
        let mut v = ChangesView::new(&pending(Against::Previous));
        v.apply(Ok(ChangesOutcome::Done { older: "tank/data@s1".to_string(), newer: "tank/data@s2".to_string(), summary }));
        let text = draw_to_rows(&v, 110, 20).join("\n");
        assert!(text.contains("2,505 removed"), "{text}");
        assert!(text.contains("1 added"), "{text}");
        assert!(text.contains("stopped after"), "{text}");
        assert!(text.contains("1 unreadable"), "{text}");
        assert!(text.contains("Camera/"), "{text}");
    }

    #[test]
    fn paths_the_host_prints_get_a_note_when_they_could_not_be_made_relative() {
        let v = ready(None, &[("/var/data/f", Change::Added)]);
        let text = draw_to_rows(&v, 110, 20).join("\n");
        assert!(text.contains("paths as the host prints them"), "{text}");
    }

    #[test]
    fn a_failed_diff_is_drawn_without_control_characters() {
        let mut v = ChangesView::new(&pending(Against::Previous));
        v.apply(Err(AppError::CommandFailed { cmd: "zfs diff".to_string(), status: 1, stderr: "no\u{1b}[2Jpe".to_string() }));
        let rows = draw_to_rows(&v, 110, 12);
        assert!(rows.iter().all(|r| !r.chars().any(|c| c == '\u{1b}')), "{rows:?}");
        assert!(rows.join("\n").contains("no"), "{rows:?}");
    }

    #[test]
    fn a_path_with_control_characters_is_drawn_without_them() {
        let mut v = ready(Some("/ds"), &[("/ds/a/x\u{1b}[2Jy", Change::Added)]);
        v.open_selected();
        let rows = draw_to_rows(&v, 110, 14);
        assert!(rows.iter().all(|r| !r.chars().any(|c| c == '\u{1b}')), "{rows:?}");
        assert!(rows.join("\n").contains("added"), "{rows:?}");
    }

    #[test]
    fn the_entry_list_shows_change_type_time_and_path() {
        let mut v = ready(Some("/ds"), &[("/ds/a/f1", Change::Added)]);
        v.open_selected();
        let text = draw_to_rows(&v, 110, 14).join("\n");
        assert!(text.contains("added") && text.contains("file") && text.contains("a/f1"), "{text}");
        assert!(text.contains("2026-"), "a change time is shown: {text}");
    }

    #[test]
    fn no_previous_says_so_and_offers_capital_c_and_reading_says_esc_cancels() {
        let mut v = ChangesView::new(&pending(Against::Previous));
        assert!(draw_to_rows(&v, 110, 10).join("\n").contains("Esc cancels"));
        v.apply(Ok(ChangesOutcome::NoPrevious));
        let text = draw_to_rows(&v, 110, 10).join("\n");
        assert!(text.contains("oldest snapshot") && text.contains('C'), "{text}");
    }
}
