use crate::display_text::{sanitise_for_display, truncate_with_ellipsis};
use crate::error::Result;
use crate::ui::changes::group_digits;
use crate::ui::theme::Theme;
use crate::zfs::host_health::HostOs;
use crate::zfs::snapshot_browse::{explain, parse_date_range, parse_size_range, sort_rows, valid_component, EntryKind, Filter, KindFilter, Listing, Row as BrowseRow};
use crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A read the key handler queued; the event loop gives it its host's runner.
#[derive(Debug, Clone)]
pub struct PendingBrowse {
    pub host: usize,
    pub request: u64,
    pub dataset: String,
    pub name: String,
    /// The folder to list, as steps below the snapshot's top.
    pub path: Vec<String>,
    /// The dataset's mountpoint once known, so only the first read asks for it.
    pub mountpoint: Option<String>,
    pub os: HostOs,
    pub cancel: Arc<AtomicBool>,
}

/// What the worker found for one folder.
#[derive(Debug)]
pub struct BrowseOutcome {
    pub listing: Listing,
    pub mountpoint: String,
}

#[derive(Debug)]
pub enum Phase {
    Reading,
    Failed(String),
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Name,
    Modified,
    Size,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub kind: InputKind,
    pub text: String,
}

#[derive(Debug)]
pub struct BrowseView {
    pub host: usize,
    pub request: u64,
    pub dataset: String,
    pub name: String,
    pub os: HostOs,
    pub utc_offset: i32,
    pub cancel: Arc<AtomicBool>,
    pub mountpoint: Option<String>,
    pub path: Vec<String>,
    pub phase: Phase,
    listing: Option<Listing>,
    /// The entries on screen: the listing filtered and sorted, kept so a redraw or a key press never redoes it.
    pub rows: Vec<BrowseRow>,
    pub selected: usize,
    pub filter: Filter,
    pub input: Option<Input>,
    /// A one-line message: why a filter was refused or a folder not opened.
    pub notice: Option<String>,
    /// The row each level was left from, to come back to it.
    back: Vec<usize>,
    restore: Option<usize>,
}

const INPUT_MAX: usize = 80;

impl BrowseView {
    pub fn new(p: &PendingBrowse, utc_offset: i32) -> Self {
        Self {
            host: p.host,
            request: p.request,
            dataset: p.dataset.clone(),
            name: p.name.clone(),
            os: p.os,
            utc_offset,
            cancel: Arc::clone(&p.cancel),
            mountpoint: p.mountpoint.clone(),
            path: p.path.clone(),
            phase: Phase::Reading,
            listing: None,
            rows: Vec::new(),
            selected: 0,
            filter: Filter::default(),
            input: None,
            notice: None,
            back: Vec::new(),
            restore: None,
        }
    }

    /// An answer for this view's current read (the caller has checked the request number and host).
    pub fn apply(&mut self, host_name: &str, result: Result<BrowseOutcome>) {
        match result {
            Ok(outcome) => {
                self.mountpoint = Some(outcome.mountpoint);
                self.listing = Some(outcome.listing);
                self.phase = Phase::Ready;
                self.refilter();
                self.selected = self.restore.take().unwrap_or(0).min(self.rows.len().saturating_sub(1));
            }
            Err(e) => {
                self.restore = None;
                self.listing = None;
                self.rows.clear();
                self.phase = Phase::Failed(truncate_with_ellipsis(&sanitise_for_display(&explain(host_name, &e.to_string())), 300));
            }
        }
    }

    fn refilter(&mut self) {
        let Some(listing) = &self.listing else {
            self.rows.clear();
            return;
        };
        self.rows = listing.entries.iter().filter(|r| self.filter.matches(r)).cloned().collect();
        sort_rows(&mut self.rows);
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    pub fn move_selection(&mut self, down: bool) {
        if down {
            self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1));
        } else {
            self.selected = self.selected.saturating_sub(1);
        }
    }

    fn start_read(&mut self, request: u64) -> PendingBrowse {
        self.cancel.store(true, Ordering::SeqCst);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.request = request;
        self.phase = Phase::Reading;
        self.notice = None;
        PendingBrowse { host: self.host, request, dataset: self.dataset.clone(), name: self.name.clone(), path: self.path.clone(), mountpoint: self.mountpoint.clone(), os: self.os, cancel: Arc::clone(&self.cancel) }
    }

    /// Opens the selected folder: the read to queue, `None` when the row is not a folder (a symlink is never entered), or
    /// an error message when its name cannot be sent to the host.
    pub fn enter_selected(&mut self, request: u64) -> std::result::Result<Option<PendingBrowse>, String> {
        if !matches!(self.phase, Phase::Ready) {
            return Ok(None);
        }
        let Some(row) = self.rows.get(self.selected) else { return Ok(None) };
        if row.entry.kind != EntryKind::Directory {
            return Ok(None);
        }
        if !valid_component(&row.entry.name) {
            let msg = "unusual name, not opened (it has a control character or is too long)".to_string();
            self.notice = Some(msg.clone());
            return Err(msg);
        }
        let name = row.entry.name.clone();
        self.back.push(self.selected);
        self.path.push(name);
        Ok(Some(self.start_read(request)))
    }

    /// Goes up one folder: the read to queue, `None` at the top of the snapshot.
    pub fn go_up(&mut self, request: u64) -> Option<PendingBrowse> {
        if self.path.is_empty() || matches!(self.phase, Phase::Reading) {
            return None;
        }
        self.path.pop();
        self.restore = self.back.pop();
        Some(self.start_read(request))
    }

    pub fn start_input(&mut self, kind: InputKind) {
        let text = if kind == InputKind::Name { self.filter.name.clone() } else { String::new() };
        self.input = Some(Input { kind, text });
        self.notice = None;
    }

    /// A key while a filter is being typed. Returns whether it was taken (it is, whenever a prompt is open).
    pub fn input_key(&mut self, key: KeyCode) -> bool {
        let Some(input) = self.input.as_mut() else { return false };
        match key {
            KeyCode::Esc => {
                self.input = None;
                self.notice = None;
            }
            KeyCode::Backspace => {
                input.text.pop();
            }
            KeyCode::Char(c) if !c.is_control() && input.text.chars().count() < INPUT_MAX => input.text.push(c),
            KeyCode::Enter => {
                let (kind, text) = (input.kind, input.text.trim().to_string());
                self.commit_input(kind, &text);
            }
            _ => {}
        }
        true
    }

    fn commit_input(&mut self, kind: InputKind, text: &str) {
        match kind {
            InputKind::Name => self.filter.name = text.to_string(),
            InputKind::Modified => {
                if text.is_empty() {
                    (self.filter.modified_from, self.filter.modified_to) = (None, None);
                } else if let Some((from, to)) = parse_date_range(text, self.utc_offset) {
                    (self.filter.modified_from, self.filter.modified_to) = (from, to);
                } else {
                    self.notice = Some("modified date not understood: use 2026-10-01..2026-10-20, 2026-10-01.., ..2026-10-20 or one day".to_string());
                    return;
                }
            }
            InputKind::Size => {
                if text.is_empty() {
                    (self.filter.size_min, self.filter.size_max) = (None, None);
                } else if let Some((min, max)) = parse_size_range(text) {
                    (self.filter.size_min, self.filter.size_max) = (min, max);
                } else {
                    self.notice = Some("size not understood: use 10M.., ..1G, 1K..1M or one size".to_string());
                    return;
                }
            }
        }
        self.input = None;
        self.notice = None;
        self.selected = 0;
        self.refilter();
    }

    pub fn cycle_kind(&mut self) {
        self.filter.kind = match self.filter.kind {
            KindFilter::All => KindFilter::Files,
            KindFilter::Files => KindFilter::Folders,
            KindFilter::Folders => KindFilter::Links,
            KindFilter::Links => KindFilter::All,
        };
        self.selected = 0;
        self.refilter();
    }

    pub fn clear_filters(&mut self) {
        if self.filter.is_empty() {
            return;
        }
        self.filter = Filter::default();
        self.selected = 0;
        self.refilter();
    }

    fn filter_text(&self) -> Option<String> {
        if self.filter.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if !self.filter.name.is_empty() {
            parts.push(format!("name has \"{}\"", self.filter.name));
        }
        if self.filter.modified_from.is_some() || self.filter.modified_to.is_some() {
            let day = |s: Option<i64>| s.map_or_else(String::new, |s| time_text(Some(s), self.utc_offset).chars().take(10).collect());
            parts.push(format!("modified {}..{}", day(self.filter.modified_from), day(self.filter.modified_to)));
        }
        if self.filter.size_min.is_some() || self.filter.size_max.is_some() {
            let size = |s: Option<u64>| s.map_or_else(String::new, crate::zfs::disk::human_size);
            parts.push(format!("size {}..{}", size(self.filter.size_min), size(self.filter.size_max)));
        }
        match self.filter.kind {
            KindFilter::All => {}
            KindFilter::Files => parts.push("files only".to_string()),
            KindFilter::Folders => parts.push("folders only".to_string()),
            KindFilter::Links => parts.push("symlinks only".to_string()),
        }
        Some(format!("showing {} of {}: {}", group_digits(self.rows.len() as u64), group_digits(self.listing.as_ref().map_or(0, |l| l.entries.len()) as u64), parts.join(", ")))
    }

    fn header(&self) -> Vec<String> {
        let mut lines = vec![format!("/{}", self.path.join("/"))];
        if let Some(text) = self.filter_text() {
            lines.push(text);
        }
        if let Some(listing) = &self.listing {
            if listing.truncated {
                lines.push("the folder is larger: only the first 20,000 entries (or what was read in 30 s) are listed".to_string());
            }
            if listing.unreadable > 0 {
                lines.push(format!("{} entries could not be read (unusual names or output)", group_digits(listing.unreadable as u64)));
            }
        }
        if let Some(notice) = &self.notice {
            lines.push(notice.clone());
        }
        lines
    }
}

/// `2026-10-20 05:25` in the host's own zone; `-` when unknown.
fn time_text(secs: Option<i64>, utc_offset: i32) -> String {
    let zone = chrono::FixedOffset::east_opt(utc_offset).unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("UTC"));
    secs.and_then(|s| chrono::DateTime::from_timestamp(s, 0)).map_or_else(|| "-".to_string(), |t| t.with_timezone(&zone).format("%Y-%m-%d %H:%M").to_string())
}

fn size_text(row: &BrowseRow) -> String {
    match (row.entry.kind, row.entry.folder_entries()) {
        (_, Some(1)) => "1 item".to_string(),
        (_, Some(n)) => format!("{} items", group_digits(n)),
        (EntryKind::File | EntryKind::Symlink, None) => crate::zfs::disk::human_size(row.entry.size),
        _ => "-".to_string(),
    }
}

fn input_prompt(input: &Input) -> String {
    let what = match input.kind {
        InputKind::Name => "name contains",
        InputKind::Modified => "modified (2026-10-01..2026-10-20)",
        InputKind::Size => "size (10M.. or ..1G or 1K..1M)",
    };
    format!("{what}: {}▏  (Enter applies, Esc cancels)", sanitise_for_display(&input.text))
}

/// Draws the view in `area`, in place of the snapshot list.
pub fn draw(f: &mut Frame, view: &BrowseView, theme: &Theme, host: &str, area: Rect) {
    let title = format!("Browse — {host} · {}@{}", sanitise_for_display(&view.dataset), sanitise_for_display(&view.name));
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let say = |f: &mut Frame, text: &str, style: ratatui::style::Style| {
        f.render_widget(Paragraph::new(text.to_string()).style(style).wrap(Wrap { trim: false }), inner);
    };
    match &view.phase {
        Phase::Reading => say(f, "reading the folder… (Esc cancels)", theme.dim()),
        Phase::Failed(text) => say(f, &format!("Could not list this folder: {text}\n\nEsc closes; ← goes up one folder."), theme.err),
        Phase::Ready => {
            let header = view.header();
            let input_h = u16::from(view.input.is_some());
            let header_h = u16::try_from(header.len()).unwrap_or(4);
            let [top, list, details, prompt] = Layout::vertical([Constraint::Length(header_h), Constraint::Min(2), Constraint::Length(2), Constraint::Length(input_h)]).areas(inner);
            let lines: Vec<Line> = header.iter().map(|l| Line::from(sanitise_for_display(l))).collect();
            f.render_widget(Paragraph::new(lines), top);
            // Only the rows that fit are built: a folder can hold 20,000 entries.
            let fit = usize::from(list.height.saturating_sub(1)).max(1);
            let start = view.selected.saturating_sub(fit / 2).min(view.rows.len().saturating_sub(fit));
            let rows: Vec<Row> = view
                .rows
                .iter()
                .skip(start)
                .take(fit)
                .map(|r| {
                    let e = &r.entry;
                    Row::new(vec![e.kind.word().to_string(), size_text(r), time_text(e.time, view.utc_offset), e.mode.clone(), truncate_with_ellipsis(&sanitise_for_display(&e.owner), 12), truncate_with_ellipsis(&sanitise_for_display(&e.name), 80)])
                })
                .collect();
            let widths = [Constraint::Length(8), Constraint::Length(12), Constraint::Length(16), Constraint::Length(11), Constraint::Length(12), Constraint::Min(10)];
            let table = Table::new(rows, widths).header(Row::new(vec!["Type", "Size", "Modified", "Mode", "Owner", "Name"])).row_highlight_style(theme.selected_row());
            let mut state = TableState::default();
            state.select(Some(view.selected.saturating_sub(start)));
            f.render_stateful_widget(table, list, &mut state);
            if let Some(r) = view.rows.get(view.selected) {
                let created = match (r.created, view.listing.as_ref().is_some_and(|l| l.has_created)) {
                    (Some(c), _) => time_text(Some(c), view.utc_offset),
                    (None, false) => "not recorded on this OS".to_string(),
                    (None, true) => "-".to_string(),
                };
                let mut text = format!("Created: {created}   Changed: {}   Group: {}", time_text(r.changed, view.utc_offset), sanitise_for_display(&r.entry.group));
                if let Some(target) = &r.entry.link_target {
                    text = format!("{text}\n→ {}", sanitise_for_display(target));
                }
                f.render_widget(Paragraph::new(text).style(theme.dim()), details);
            }
            if let Some(input) = &view.input {
                f.render_widget(Paragraph::new(input_prompt(input)), prompt);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zfs::host_health::HostOs;
    use crate::zfs::snapshot_browse::{EntryKind, ListEntry, Listing, Row as BrowseRow};
    use crossterm::event::KeyCode;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn pending(path: &[&str]) -> PendingBrowse {
        PendingBrowse {
            host: 0,
            request: 1,
            dataset: "tank/data".to_string(),
            name: "s1".to_string(),
            path: path.iter().map(|s| (*s).to_string()).collect(),
            mountpoint: None,
            os: HostOs::FreeBsd,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    fn entry(name: &str, kind: EntryKind, size: u64, time: i64) -> BrowseRow {
        let entry = ListEntry { name: name.to_string(), kind, mode: "-rw-r--r--".to_string(), owner: "root".to_string(), group: "wheel".to_string(), size, time: Some(time), link_target: None };
        BrowseRow { entry, changed: Some(time + 10), created: Some(time - 10) }
    }

    fn listing(rows: Vec<BrowseRow>) -> Listing {
        Listing { entries: rows, truncated: false, unreadable: 0, has_created: true }
    }

    fn ready(rows: Vec<BrowseRow>) -> BrowseView {
        let mut view = BrowseView::new(&pending(&[]), 0);
        view.apply("local", Ok(BrowseOutcome { listing: listing(rows), mountpoint: "/m".to_string() }));
        view
    }

    fn sample() -> Vec<BrowseRow> {
        vec![entry("b.txt", EntryKind::File, 2048, 1_700_000_000), entry("pics", EntryKind::Directory, 5, 1_700_000_100), entry("Alpha.jpg", EntryKind::File, 10, 1_600_000_000), entry("link", EntryKind::Symlink, 4, 1_700_000_200)]
    }

    fn draw_to_rows(view: &BrowseView, w: u16, h: u16) -> Vec<String> {
        let theme = crate::ui::state::App::new(vec!["local".to_string()]).theme.clone();
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, view, &theme, "local", f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect()
    }

    fn names(view: &BrowseView) -> Vec<&str> {
        view.rows.iter().map(|r| r.entry.name.as_str()).collect()
    }

    #[test]
    fn ready_view_lists_folders_first_with_item_counts() {
        let view = ready(sample());
        assert_eq!(names(&view), ["pics", "Alpha.jpg", "b.txt", "link"]);
        let shown = draw_to_rows(&view, 110, 16).join("\n");
        assert!(shown.contains("3 items"), "a folder shows its entry count: {shown}");
        assert!(shown.contains("Browse — local") && shown.contains("tank/data@s1"), "{shown}");
    }

    #[test]
    fn enter_opens_a_folder_and_left_goes_back_to_the_same_row() {
        let mut view = ready(sample());
        view.move_selection(true);
        assert_eq!(view.selected, 1);
        view.selected = 0;
        let next = view.enter_selected(2).unwrap().expect("pics is a folder");
        assert_eq!((next.path.clone(), next.request, next.mountpoint.as_deref()), (vec!["pics".to_string()], 2, Some("/m")));
        assert!(matches!(view.phase, Phase::Reading) && view.request == 2);
        view.apply("local", Ok(BrowseOutcome { listing: listing(vec![entry("x.txt", EntryKind::File, 1, 1)]), mountpoint: "/m".to_string() }));
        assert_eq!(view.path, ["pics"]);
        let up = view.go_up(3).expect("one level up");
        assert!(up.path.is_empty());
        view.apply("local", Ok(BrowseOutcome { listing: listing(sample()), mountpoint: "/m".to_string() }));
        assert_eq!(view.selected, 0, "back on the folder it came from");
        assert!(view.go_up(4).is_none(), "at the top there is nowhere to go");
    }

    #[test]
    fn enter_on_a_symlink_does_nothing() {
        let mut view = ready(sample());
        view.selected = view.rows.iter().position(|r| r.entry.kind == EntryKind::Symlink).unwrap();
        assert!(view.enter_selected(2).unwrap().is_none());
        assert!(matches!(view.phase, Phase::Ready));
    }

    #[test]
    fn a_name_with_a_control_character_is_shown_sanitised_and_not_opened() {
        let mut view = ready(vec![entry("evil\u{1b}[2Jdir", EntryKind::Directory, 3, 1_700_000_000)]);
        let err = view.enter_selected(2).unwrap_err();
        assert!(err.contains("unusual name"), "{err}");
        assert!(matches!(view.phase, Phase::Ready));
        let shown = draw_to_rows(&view, 100, 14).join("\n");
        assert!(!shown.contains('\u{1b}'), "{shown:?}");
        assert!(shown.contains("evil"), "{shown}");
    }

    fn type_text(view: &mut BrowseView, text: &str) {
        for c in text.chars() {
            assert!(view.input_key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn filters_narrow_the_list_and_x_clears_them() {
        let mut view = ready(sample());
        view.start_input(InputKind::Name);
        type_text(&mut view, "PIC");
        assert!(view.input_key(KeyCode::Enter));
        assert!(view.input.is_none());
        assert_eq!(names(&view), ["pics"]);
        view.clear_filters();
        assert_eq!(view.rows.len(), 4);
        view.cycle_kind();
        assert_eq!(view.filter.kind, KindFilter::Files);
        assert_eq!(names(&view), ["Alpha.jpg", "b.txt"]);
        view.start_input(InputKind::Size);
        type_text(&mut view, "1K..");
        view.input_key(KeyCode::Enter);
        assert_eq!(names(&view), ["b.txt"]);
        view.clear_filters();
        view.start_input(InputKind::Modified);
        type_text(&mut view, "2020-01-01..2020-12-31");
        view.input_key(KeyCode::Enter);
        assert_eq!(names(&view), ["pics", "Alpha.jpg"], "folders stay, Alpha.jpg was modified in 2020");
    }

    #[test]
    fn bad_filter_text_keeps_the_prompt_and_says_why() {
        let mut view = ready(sample());
        view.start_input(InputKind::Size);
        type_text(&mut view, "abc");
        view.input_key(KeyCode::Enter);
        assert!(view.input.is_some(), "the prompt stays open");
        assert!(view.notice.as_deref().is_some_and(|n| n.contains("size")), "{:?}", view.notice);
        assert_eq!(view.rows.len(), 4);
        assert!(view.input_key(KeyCode::Esc));
        assert!(view.input.is_none() && view.filter.is_empty());
    }

    #[test]
    fn the_list_notes_truncation_and_unreadable_entries() {
        let mut view = BrowseView::new(&pending(&[]), 0);
        let mut l = listing(sample());
        l.truncated = true;
        l.unreadable = 3;
        view.apply("local", Ok(BrowseOutcome { listing: l, mountpoint: "/m".to_string() }));
        let shown = draw_to_rows(&view, 120, 16).join("\n");
        assert!(shown.contains("first 20,000") && shown.contains("3 entries could not be read"), "{shown}");
    }

    #[test]
    fn details_show_created_changed_and_say_when_the_os_records_no_creation_time() {
        let mut view = ready(sample());
        let shown = draw_to_rows(&view, 120, 16).join("\n");
        assert!(shown.contains("Created:") && shown.contains("Changed:") && shown.contains("Group:"), "{shown}");
        let mut l = listing(sample());
        l.has_created = false;
        for r in &mut l.entries {
            r.created = None;
        }
        view.apply("local", Ok(BrowseOutcome { listing: l, mountpoint: "/m".to_string() }));
        let shown = draw_to_rows(&view, 120, 16).join("\n");
        assert!(shown.contains("not recorded on this OS"), "{shown}");
    }

    #[test]
    fn a_failed_read_shows_the_explained_text_and_a_huge_folder_draws_only_a_window() {
        let mut view = BrowseView::new(&pending(&[]), 0);
        view.apply("ub24", Err(crate::error::AppError::CommandFailed { cmd: "ls".to_string(), status: 1, stderr: "sudo: a password is required".to_string() }));
        let shown = draw_to_rows(&view, 120, 10).join("\n");
        assert!(shown.contains("print-sudoers ub24 --with browse"), "{shown}");
        let many: Vec<BrowseRow> = (0..20_000).map(|i| entry(&format!("file{i:05}"), EntryKind::File, 1, 1_700_000_000)).collect();
        let big = ready(many);
        let mut big = big;
        for _ in 0..500 {
            big.move_selection(true);
        }
        let shown = draw_to_rows(&big, 110, 14).join("\n");
        assert!(shown.contains("file00500"), "the selected row is in the window: {shown}");
    }
}
