//! Drawing the Dashboard: one borderless screen, a thin rule under each heading, aligned columns, small gauges where the
//! terminal is wide enough. Every status has a glyph and a word as well as a colour, so the `mono` theme reads the same.

use crate::ui::dashboard::{Cell, Dashboard, HostRow, Level};
use crate::ui::state::App;
use crate::ui::theme::Theme;
use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Lines of Needs attention shown at once on a tall screen; the rest are counted. A short screen shows fewer.
const ATTENTION_SHOWN: usize = 6;
/// Cells of a gauge, including its two end caps.
const GAUGE_WIDTH: u16 = 10;

/// The glyph and the word a level always carries, so colour is never the only signal.
pub fn glyph_word(level: Level) -> (&'static str, &'static str) {
    match level {
        Level::Fine => ("●", "ok"),
        Level::Watch => ("▲", "watch"),
        Level::Issue => ("✖", "issue"),
        Level::Unknown => ("○", "unknown"),
    }
}

/// `━━━╌╌╌╌╌╌╌`: exactly `width` cells, filled in proportion to `percent` (over 100 is full). Thin line glyphs, not
/// block glyphs: blocks fill the whole line height and run together between rows in many fonts.
pub fn gauge(percent: u8, width: u16) -> String {
    let width = usize::from(width);
    let filled = (usize::from(percent.min(100)) * width + 50) / 100;
    format!("{}{}", "━".repeat(filled), "╌".repeat(width - filled))
}

fn level_style(theme: &Theme, level: Level) -> Style {
    match level {
        Level::Fine => theme.ok,
        Level::Watch => theme.warn,
        Level::Issue => theme.err,
        Level::Unknown => theme.dim(),
    }
}

/// A table cell's own style: only what needs a look is coloured, so a healthy host is calm.
fn cell_style(theme: &Theme, level: Level) -> Style {
    if level == Level::Fine { Style::default() } else { level_style(theme, level) }
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn pad(text: &str, width: usize) -> String {
    let cut = cut(text, width);
    format!("{cut:<width$}")
}

/// `HOSTS ─────────`: a capitalised heading in the header style with a thin dim rule to the right edge.
fn heading(label: &str, width: usize, theme: &Theme) -> Line<'static> {
    let rule = width.saturating_sub(label.chars().count() + 3);
    Line::from(vec![Span::raw(" "), Span::styled(label.to_string(), theme.header()), Span::styled(format!(" {}", "─".repeat(rule)), theme.dim())])
}

fn header(d: &Dashboard, width: usize, now: &str, theme: &Theme) -> Line<'static> {
    let (glyph, _) = glyph_word(d.verdict.level);
    let right = format!("{glyph} {}", d.verdict.text);
    let long = format!(" Dashboard  ·  {now}");
    let short = " Dashboard".to_string();
    let left = [long, short].into_iter().find(|l| l.chars().count() + right.chars().count() + 2 <= width);
    match left {
        Some(left) => {
            let gap = width.saturating_sub(left.chars().count() + right.chars().count() + 1);
            Line::from(vec![Span::styled(left, theme.accent), Span::raw(" ".repeat(gap)), Span::styled(right, level_style(theme, d.verdict.level))])
        }
        None => Line::styled(cut(&format!(" {right}"), width), level_style(theme, d.verdict.level)),
    }
}

fn attention_lines(d: &Dashboard, selected: Option<usize>, width: usize, cap: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![heading("NEEDS ATTENTION", width, theme)];
    if d.attention.is_empty() {
        let (text, level) = if d.verdict.level == Level::Unknown { ("checking...", Level::Unknown) } else { ("nothing needs attention", Level::Fine) };
        let (glyph, _) = glyph_word(level);
        lines.push(Line::styled(format!("   {glyph} {text}"), level_style(theme, level)));
        return lines;
    }
    let start = selected.filter(|s| *s >= cap).map_or(0, |s| s + 1 - cap);
    let subject_w = d.attention.iter().map(|i| i.subject.chars().count()).max().unwrap_or(0).clamp(6, 16);
    for (index, item) in d.attention.iter().enumerate().skip(start).take(cap) {
        let (glyph, word) = glyph_word(item.level);
        let marker = if selected == Some(index) { "▸" } else { " " };
        let prefix = format!(" {marker} ");
        let used = prefix.chars().count() + 2 + 7 + subject_w + 2;
        let what = cut(&item.what, width.saturating_sub(used));
        let line = Line::from(vec![
            Span::raw(prefix),
            Span::styled(format!("{glyph} {word:<5} "), level_style(theme, item.level)),
            Span::raw(format!("{}  ", pad(&item.subject, subject_w))),
            Span::raw(what),
        ]);
        lines.push(if selected == Some(index) { line.patch_style(theme.selected_row()) } else { line });
    }
    let below = d.attention.len().saturating_sub(start + cap);
    let text = match (start, below) {
        (0, 0) => None,
        (0, b) => Some(format!("     +{b} more")),
        (a, 0) => Some(format!("     ↑ {a} above")),
        (a, b) => Some(format!("     ↑ {a} above, +{b} more")),
    };
    if let Some(text) = text {
        lines.push(Line::styled(text, theme.dim()));
    }
    lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Os,
    Pools,
    Space,
    Memory,
    Arc,
    L2arc,
    Cpu,
    Load,
    Scrub,
    Errors,
}

impl Col {
    fn title(self) -> &'static str {
        match self {
            Col::Os => "OS",
            Col::Pools => "Pools",
            Col::Space => "Capacity",
            Col::Memory => "Memory",
            Col::Arc => "ARC",
            Col::L2arc => "L2ARC",
            Col::Cpu => "CPU",
            Col::Load => "Load",
            Col::Scrub => "Scrub",
            Col::Errors => "Errors",
        }
    }

    fn cell(self, row: &HostRow) -> &Cell {
        match self {
            Col::Os => &row.os,
            Col::Pools => &row.pools,
            Col::Space => &row.space,
            Col::Memory => &row.memory,
            Col::Arc => &row.arc,
            Col::L2arc => &row.l2arc,
            Col::Cpu => &row.cpu,
            Col::Load => &row.load,
            Col::Scrub => &row.scrub,
            Col::Errors => &row.errors,
        }
    }

    fn has_gauge(self) -> bool {
        matches!(self, Col::Space | Col::Memory | Col::Cpu)
    }
}

/// Columns, in display order, and the order they are given up as the screen narrows: first the OS name, then the cache columns (which
/// are not drawn at all while no host has anything to show in them), then the gauges, then these.
const COLUMNS: [Col; 10] = [Col::Os, Col::Pools, Col::Space, Col::Memory, Col::Arc, Col::L2arc, Col::Cpu, Col::Load, Col::Scrub, Col::Errors];
const DROP_ORDER: [Col; 5] = [Col::Load, Col::Cpu, Col::Scrub, Col::Errors, Col::Pools];

struct Plan {
    columns: Vec<Col>,
    gauges: bool,
    name_w: usize,
    state_w: usize,
}

impl Plan {
    fn col_width(&self, col: Col, hosts: &[HostRow]) -> usize {
        let text = hosts.iter().map(|h| col.cell(h).text.chars().count()).max().unwrap_or(0).max(col.title().chars().count());
        if self.gauges && col.has_gauge() { text.max(4) + usize::from(GAUGE_WIDTH) + 1 } else { text }
    }

    fn total(&self, hosts: &[HostRow]) -> usize {
        let others: usize = self.columns.iter().map(|c| self.col_width(*c, hosts)).sum();
        2 + self.name_w + 2 + self.state_w + self.columns.len() * 2 + others
    }
}

/// Fits the table to `width`: the name and state stay; gauges go first, then the least useful columns, then the state and
/// name columns shrink.
fn plan(hosts: &[HostRow], width: usize) -> Plan {
    let longest = |f: fn(&HostRow) -> usize| hosts.iter().map(f).max().unwrap_or(0);
    // A column of dashes says nothing: the cache columns are drawn only while some host has a reading for them.
    let has = |col: Col| hosts.iter().any(|h| !matches!(col.cell(h).text.as_str(), "-" | "?"));
    let mut plan = Plan {
        columns: COLUMNS.iter().copied().filter(|c| !matches!(c, Col::Arc | Col::L2arc) || has(*c)).collect(),
        gauges: true,
        name_w: longest(|h| h.name.chars().count()).clamp(4, 14),
        state_w: (longest(|h| h.state.text.chars().count()) + 2).clamp(10, 30),
    };
    while plan.total(hosts) > width {
        if plan.columns.contains(&Col::Os) {
            plan.columns.retain(|c| *c != Col::Os);
        } else if plan.columns.contains(&Col::L2arc) {
            plan.columns.retain(|c| *c != Col::L2arc);
        } else if plan.columns.contains(&Col::Arc) {
            plan.columns.retain(|c| *c != Col::Arc);
        } else if plan.gauges {
            plan.gauges = false;
        } else if let Some(col) = DROP_ORDER.iter().find(|c| plan.columns.contains(c)) {
            plan.columns.retain(|c| c != col);
        } else if plan.state_w > 8 {
            plan.state_w = 8.max(plan.state_w.saturating_sub(plan.total(hosts) - width));
        } else if plan.name_w > 6 {
            plan.name_w -= 1;
        } else {
            break;
        }
    }
    plan
}

fn host_lines(d: &Dashboard, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![heading("HOSTS", width, theme)];
    if d.hosts.is_empty() {
        lines.push(Line::styled("   ○ no hosts configured", theme.dim()));
        return lines;
    }
    let plan = plan(&d.hosts, width);
    let mut head = vec![Span::raw("  "), Span::styled(pad("Host", plan.name_w), theme.header()), Span::raw("  "), Span::styled(pad("State", plan.state_w), theme.header())];
    for col in &plan.columns {
        head.push(Span::styled(format!("  {}", pad(col.title(), plan.col_width(*col, &d.hosts))), theme.header()));
    }
    lines.push(Line::from(head));
    for row in &d.hosts {
        let (glyph, _) = glyph_word(row.state.level);
        let name_style = if row.state.level == Level::Unknown { theme.dim() } else { Style::default() };
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(pad(&row.name, plan.name_w), name_style),
            Span::raw("  "),
            Span::styled(pad(&format!("{glyph} {}", row.state.text), plan.state_w), level_style(theme, row.state.level)),
        ];
        for col in &plan.columns {
            let cell = col.cell(row);
            let w = plan.col_width(*col, &d.hosts);
            spans.push(Span::raw("  "));
            match cell.gauge.filter(|_| plan.gauges && col.has_gauge()) {
                Some(percent) => {
                    spans.push(Span::styled(gauge(percent, GAUGE_WIDTH), level_style(theme, cell.level)));
                    spans.push(Span::styled(pad(&format!(" {}", cell.text), w - usize::from(GAUGE_WIDTH)), cell_style(theme, cell.level)));
                }
                None => spans.push(Span::styled(pad(&cell.text, w), cell_style(theme, cell.level))),
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn status_line(level: Level, label: &str, text: &str, width: usize, theme: &Theme) -> Line<'static> {
    let (glyph, _) = glyph_word(level);
    Line::styled(cut(&format!("   {glyph} {label}{text}"), width), level_style(theme, level))
}

/// Everything below the host table: the daemon and jobs, the cloud remotes and the alerts. `compact` drops the headings and
/// blank lines (each line then says what it is) for a screen that is too short for the full layout.
fn component_lines(d: &Dashboard, width: usize, compact: bool, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let section = |lines: &mut Vec<Line<'static>>, title: &str| {
        if !compact {
            lines.push(Line::raw(""));
            lines.push(heading(title, width, theme));
        }
    };
    let label = |full: &'static str, short: &'static str| if compact { short } else { full };
    section(&mut lines, "DAEMON AND JOBS");
    lines.push(status_line(d.daemon.level, "daemon  ", &d.daemon.text, width, theme));
    lines.push(status_line(d.jobs.level, "jobs    ", &d.jobs.text, width, theme));
    section(&mut lines, "CLOUD");
    if d.cloud.is_empty() {
        lines.push(status_line(Level::Unknown, label("", "cloud   "), "no rclone remotes found", width, theme));
    }
    if !d.cloud_header.is_empty() {
        // Aligned with the lines below: the same three-space marker and label prefix, then the column names.
        lines.push(Line::styled(cut(&format!("     {}{}", label("", "        "), d.cloud_header), width), theme.header()));
    }
    for line in &d.cloud {
        lines.push(status_line(line.level, label("", "cloud   "), &line.text, width, theme));
    }
    section(&mut lines, "ALERTS");
    lines.push(status_line(d.alerts.level, label("", "alerts  "), &d.alerts.text, width, theme));
    lines
}

/// The screen with a given attention cap, in the full or the compact layout.
fn assemble(d: &Dashboard, selected: Option<usize>, width: usize, now: &str, theme: &Theme, cap: usize, compact: bool) -> Vec<Line<'static>> {
    // The line under the header is blank, or the red notice that a check is running (the screen keeps its height).
    let notice = d.running.as_ref().map_or_else(|| Line::raw(""), |text| Line::styled(cut(&format!("  ▶ {text}"), width), theme.err));
    let mut lines = vec![header(d, width, now, theme), notice];
    lines.extend(attention_lines(d, selected, width, cap, theme));
    lines.push(Line::raw(""));
    lines.extend(host_lines(d, width, theme));
    lines.extend(component_lines(d, width, compact, theme));
    lines
}

/// The whole screen as lines for a screen of `width` x `height`. Nothing is ever cut silently: a short screen first shows
/// fewer Needs attention lines, then the compact layout, and only if that is still too tall does it cut the end and say how
/// many lines are missing.
pub fn screen_lines(d: &Dashboard, selected: Option<usize>, width: usize, height: usize, now: &str, theme: &Theme) -> Vec<Line<'static>> {
    for (cap, compact) in [(ATTENTION_SHOWN, false), (4, false), (3, true), (2, true), (1, true)] {
        let lines = assemble(d, selected, width, now, theme, cap, compact);
        if lines.len() <= height {
            return lines;
        }
    }
    let mut lines = assemble(d, selected, width, now, theme, 1, true);
    if height == 0 {
        return Vec::new();
    }
    let missing = lines.len() - height + 1;
    lines.truncate(height - 1);
    lines.push(Line::styled(format!("  ▼ {missing} more lines below: make the window taller"), theme.dim()));
    lines
}

/// Draws the Dashboard into `area`: no borders, nothing to type into, nothing it can change.
pub fn draw_dashboard(f: &mut Frame, app: &mut App, area: Rect) {
    let d = app.dashboard();
    let selected = app.dashboard_table.selected().filter(|_| !d.attention.is_empty()).map(|s| s.min(d.attention.len() - 1));
    app.dashboard_table.select(selected);
    let now = chrono::Local::now().format("%a %d %b %H:%M").to_string();
    let lines = screen_lines(&d, selected, usize::from(area.width), usize::from(area.height), &now, &app.theme);
    f.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::{App, HostProbeState, Section};
    use crate::ui::theme::Theme;
    use crate::zfs::host_health::{HostProbe, PoolProbe};
    use crate::zfs::host_resources::Resources;
    use crate::zfs::pool::PoolHealth;
    use ratatui::{backend::TestBackend, Terminal};

    fn render(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw_dashboard(f, app, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect()
    }

    fn reading(busy: u64, idle: u64) -> Resources {
        Resources { cores: 4, physical: 20 << 30, free: 10 << 30, inactive: 0, arc: 0, load: Some([0.05, 0.1, 0.1]), cpu_ticks: Some(crate::zfs::host::CpuTimes { user: busy, nice: 0, sys: 0, intr: 0, idle }), swap: None, cache: None }
    }

    fn pool(capacity: u8) -> PoolProbe {
        let when = chrono::Utc::now().format("%a %b %e %H:%M:%S %Y").to_string();
        PoolProbe { name: "tank".into(), health: PoolHealth::Online, capacity_percent: capacity, frag_percent: None, errors: Some(0), scan: None, last_scrub: Some(when) }
    }

    fn answered(app: &mut App, name: &str, probe: HostProbe) {
        app.host_probes.insert(name.into(), HostProbeState { last: Some(probe), resources: vec![reading(0, 1000), reading(10, 1100)], utc_offset_secs: Some(0), ..HostProbeState::default() });
    }

    fn heartbeat(app: &mut App) {
        app.jobs_view.heartbeat = Some(crate::daemon::heartbeat::Heartbeat { pid: 4242, started_at: chrono::Utc::now(), last_tick: chrono::Utc::now() });
        app.jobs_view.loaded = true;
    }

    /// Three hosts: one fine, one with a pool 85% full, one unreachable; the daemon running.
    fn mixed_app() -> App {
        let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string(), "fbsd16t1".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Dashboard).unwrap();
        answered(&mut app, "local", HostProbe::Reached { rtt_ms: 3, pools: vec![pool(43)], kind: None, os: None, os_name: None });
        answered(&mut app, "fbsd16", HostProbe::Reached { rtt_ms: 3, pools: vec![pool(85)], kind: None, os: None, os_name: None });
        app.host_probes.insert("fbsd16t1".into(), HostProbeState { last: Some(HostProbe::Disconnected), ..HostProbeState::default() });
        heartbeat(&mut app);
        app
    }

    #[test]
    fn dashboard_headings_and_column_titles_use_the_header_style_and_the_rule_stays_dim() {
        let app = mixed_app();
        let lines = screen_lines(&app.dashboard(), None, 120, 40, "Sat", &app.theme);
        let span_style = |needle: &str, line_has: &str| {
            let line = lines.iter().find(|l| l.to_string().contains(line_has)).unwrap_or_else(|| panic!("{line_has}"));
            line.spans.iter().find(|s| s.content.contains(needle)).unwrap_or_else(|| panic!("{needle}")).style
        };
        assert_eq!(span_style("HOSTS", "HOSTS"), app.theme.header(), "block heading");
        assert_eq!(span_style("NEEDS ATTENTION", "NEEDS ATTENTION"), app.theme.header());
        assert_eq!(span_style("─", "HOSTS"), app.theme.dim(), "the rule stays dim");
        assert_eq!(span_style("Host", "Host   "), app.theme.header(), "table column title");
        assert_eq!(span_style("State", "Host   "), app.theme.header());
    }

    #[test]
    fn the_hosts_table_names_the_os_and_gives_that_column_up_before_anything_else() {
        let mut app = mixed_app();
        app.host_probes.get_mut("local").unwrap().os_name = Some("FreeBSD 16.0".into());
        let text = |width| screen_lines(&app.dashboard(), None, width, 40, "Sat", &app.theme).iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        let wide = text(140);
        assert!(wide.contains(" OS ") && wide.contains("FreeBSD 16.0"), "{wide}");
        // the width at which the OS column is the only thing short: everything else is still there without it
        let narrow = text(100);
        assert!(!narrow.contains("FreeBSD 16.0") && narrow.contains("Capacity") && narrow.contains("Memory") && narrow.contains("Errors"), "{narrow}");
    }

    #[test]
    fn the_arc_columns_appear_only_where_a_host_has_the_counters_and_go_right_after_the_os_name_when_the_screen_narrows() {
        use crate::zfs::host_resources::CacheStats;
        let mut app = mixed_app();
        let text = |app: &App, width| screen_lines(&app.dashboard(), None, width, 40, "Sat", &app.theme).iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        let plain = text(&app, 200);
        assert!(!plain.contains("ARC"), "no host reads the counters: no column of dashes\n{plain}");
        let cache = CacheStats { hits: 9990, misses: 10, l2_size: 2 << 30, l2_hits: 300, l2_misses: 100 };
        app.host_probes.get_mut("local").unwrap().resources = vec![Resources { cache: Some(cache), ..reading(0, 1000) }, Resources { cache: Some(cache), ..reading(10, 1100) }];
        let wide = text(&app, 200);
        assert!(wide.contains("ARC") && wide.contains("99.9%") && wide.contains("L2ARC") && wide.contains("2.0G 75.0%"), "{wide}");
        // the host without counters shows a dash in the new column, not a hole
        assert!(wide.lines().any(|l| l.contains("fbsd16 ") && l.contains(" - ")), "{wide}");
        // no host has a cache device: the L2ARC column is not drawn, the ARC one is
        app.host_probes.get_mut("local").unwrap().resources = vec![Resources { cache: Some(CacheStats { l2_size: 0, ..cache }), ..reading(10, 1100) }];
        let no_l2 = text(&app, 200);
        assert!(no_l2.contains("ARC") && !no_l2.contains("L2ARC"), "{no_l2}");
        app.host_probes.get_mut("local").unwrap().resources = vec![Resources { cache: Some(cache), ..reading(0, 1000) }, Resources { cache: Some(cache), ..reading(10, 1100) }];
        // narrower: the OS name goes first, then the cache columns, before the gauges and every other column
        let narrow = text(&app, 105);
        assert!(!narrow.contains("ARC") && narrow.contains("Capacity") && narrow.contains("Memory") && narrow.contains("Errors") && narrow.contains("Load"), "{narrow}");
    }

    #[test]
    fn the_running_notice_sits_under_the_header_in_the_error_style_and_costs_no_extra_line() {
        let mut app = App::new(vec!["local".to_string()]);
        let quiet = screen_lines(&app.dashboard(), None, 100, 40, "Sat", &app.theme);
        let dir = tempfile::tempdir().unwrap();
        app.state_dir = dir.path().to_path_buf();
        let _guard = crate::daemon::checks::RunGuard::start(dir.path(), "tui", Some("fbsd16"));
        app.refresh_checks_marker();
        let lines = screen_lines(&app.dashboard(), None, 100, 40, "Sat", &app.theme);
        let text: String = lines[1].spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("▶ checking fbsd16: disks and shares since"), "{text}");
        assert_eq!(lines[1].style, app.theme.err);
        assert_eq!(lines.len(), quiet.len(), "the notice replaces the blank line under the header");
    }

    #[test]
    fn a_gauge_is_exactly_as_wide_as_asked_and_clamped() {
        assert_eq!(gauge(43, 10), "━━━━╌╌╌╌╌╌");
        assert_eq!(gauge(0, 10), "╌╌╌╌╌╌╌╌╌╌");
        assert_eq!(gauge(100, 10), "━━━━━━━━━━");
        assert_eq!(gauge(250, 10), "━━━━━━━━━━", "over 100 is full");
        for w in [3, 5, 12, 20] {
            assert_eq!(gauge(50, w).chars().count(), usize::from(w));
        }
    }

    #[test]
    fn every_level_has_its_own_glyph_and_word() {
        use crate::ui::dashboard::Level;
        let all = [Level::Fine, Level::Watch, Level::Issue, Level::Unknown].map(glyph_word);
        let glyphs: std::collections::HashSet<&str> = all.iter().map(|g| g.0).collect();
        let words: std::collections::HashSet<&str> = all.iter().map(|g| g.1).collect();
        assert_eq!((glyphs.len(), words.len()), (4, 4));
        assert_eq!(glyph_word(Level::Fine), ("●", "ok"));
        assert_eq!(glyph_word(Level::Watch), ("▲", "watch"));
        assert_eq!(glyph_word(Level::Issue), ("✖", "issue"));
    }

    #[test]
    fn wide_it_shows_every_section_the_gauges_and_a_glyph_for_each_level() {
        let mut app = mixed_app();
        let text = render(&mut app, 130, 40).join("\n");
        for want in ["Dashboard", "NEEDS ATTENTION", "HOSTS", "DAEMON", "JOBS", "CLOUD", "ALERTS", "local", "fbsd16", "fbsd16t1", "━", "●", "▲", "✖", "tank 85% full", "unreachable", "running (pid 4242)"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        assert!(text.contains("2 to watch") || text.contains("1 issue"), "the verdict counts what needs attention:\n{text}");
    }

    #[test]
    fn at_80_columns_the_gauges_are_gone_and_the_numbers_stay() {
        let mut app = mixed_app();
        let text = render(&mut app, 80, 40).join("\n");
        assert!(!text.contains('━'), "{text}");
        for want in ["local", "fbsd16", "85%", "unreachable"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        assert!(render(&mut app, 80, 40).iter().all(|l| l.chars().count() <= 80));
    }

    #[test]
    fn at_60_and_40_columns_nothing_breaks_and_the_host_and_its_state_stay() {
        let mut app = mixed_app();
        for w in [60u16, 40] {
            let lines = render(&mut app, w, 30);
            let text = lines.join("\n");
            assert!(text.contains("local") && text.contains("fbsd16t1"), "{w}: {text}");
            assert!(lines.iter().all(|l| l.chars().count() <= usize::from(w)), "{w}: a line is wider than the screen:\n{text}");
        }
        assert_eq!(render(&mut app, 20, 3).len(), 3, "a tiny terminal still draws something");
    }

    #[test]
    fn the_mono_theme_reads_by_glyph_and_word_alone() {
        let mut app = mixed_app();
        app.theme = Theme::mono();
        let text = render(&mut app, 120, 40).join("\n");
        assert!(text.contains("✖ issue") && text.contains("▲ watch"), "{text}");
    }

    #[test]
    fn needs_attention_is_capped_at_six_lines_and_says_how_many_more() {
        let names: Vec<String> = std::iter::once("local".to_string()).chain((1..=8).map(|i| format!("h{i}"))).collect();
        let mut app = App::new(names.clone());
        app.selected_section = app.sections.iter().position(|s| *s == Section::Dashboard).unwrap();
        for n in &names {
            app.host_probes.insert(n.clone(), HostProbeState { last: Some(HostProbe::Disconnected), ..HostProbeState::default() });
        }
        heartbeat(&mut app);
        let lines = render(&mut app, 120, 50);
        // Attention lines start with their glyph; host rows start with the host's name.
        let attention = lines.iter().filter(|l| l.trim_start().starts_with("✖ issue") || l.trim_start().starts_with("▸ ✖ issue")).count();
        assert_eq!(attention, 6, "exactly six attention lines: {lines:#?}");
        assert!(lines.iter().any(|l| l.contains("+3 more")), "9 hosts down, 6 listed: {lines:#?}");
    }

    #[test]
    fn when_all_is_well_it_says_so_in_the_attention_block_and_the_header() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Dashboard).unwrap();
        answered(&mut app, "local", HostProbe::Reached { rtt_ms: 3, pools: vec![pool(10)], kind: None, os: None, os_name: None });
        heartbeat(&mut app);
        let lines = render(&mut app, 120, 40);
        let text = lines.join("\n");
        assert!(text.contains("nothing needs attention") && text.contains("all quiet"), "{text}");
        assert!(lines[0].trim_end().ends_with("all quiet"), "the verdict sits at the right end of the header: {:?}", lines[0]);
    }

    #[test]
    fn before_every_host_has_answered_it_says_checking_not_all_quiet() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Dashboard).unwrap();
        heartbeat(&mut app);
        let text = render(&mut app, 120, 40).join("\n");
        assert!(text.contains("checking...") && !text.contains("all quiet"), "{text}");
    }

    #[test]
    fn the_selected_attention_line_is_marked_and_a_selection_past_the_end_is_clamped() {
        let mut app = mixed_app();
        app.dashboard_table.select(Some(0));
        let lines = render(&mut app, 120, 40);
        assert!(lines.iter().any(|l| l.trim_start().starts_with('▸')), "{lines:#?}");
        app.dashboard_table.select(Some(99));
        let lines = render(&mut app, 120, 40);
        assert!(lines.iter().filter(|l| l.trim_start().starts_with('▸')).count() == 1, "the selection is clamped to the last line: {lines:#?}");
    }

    #[test]
    fn a_known_remote_and_a_failed_job_show_in_their_sections() {
        let mut app = mixed_app();
        app.cloud_known.insert("local".into(), vec![("b2".into(), "b2".into())]);
        let text = render(&mut app, 120, 50).join("\n");
        assert!(text.contains("Backblaze B2") && text.contains("b2") && text.contains("no jobs"), "{text}");
    }

    /// Nine hosts down plus a fine one, the daemon running: nine Needs attention lines and a tall host table.
    fn crowded_app() -> App {
        let mut app = mixed_app();
        for i in 1..=8 {
            let name = format!("h{i}");
            app.hosts.push(name.clone());
            app.host_probes.insert(name, HostProbeState { last: Some(HostProbe::Disconnected), ..HostProbeState::default() });
        }
        app
    }

    #[test]
    fn a_short_terminal_keeps_the_header_and_every_component_by_shrinking_the_attention_list_first() {
        let mut app = mixed_app();
        let lines = render(&mut app, 120, 24);
        let text = lines.join("\n");
        for want in ["Dashboard", "daemon", "jobs", "monitor off", "fbsd16t1"] {
            assert!(text.contains(want), "missing {want:?} on a 24-row terminal:\n{text}");
        }
        assert!(!text.contains("more lines below"), "nothing needed cutting here:\n{text}");
    }

    #[test]
    fn when_even_the_compact_screen_does_not_fit_it_says_how_much_is_cut_never_silently() {
        let mut app = crowded_app();
        for height in [24u16, 16, 12, 8] {
            let lines = render(&mut app, 120, height);
            let text = lines.join("\n");
            assert!(lines[0].contains("Dashboard"), "{height}: the header always stays:\n{text}");
            let shows_everything = text.contains("monitor off") && text.contains("daemon");
            assert!(shows_everything || text.contains("more lines below"), "{height}: lines were cut without a word:\n{text}");
        }
        let tall = render(&mut app, 120, 60).join("\n");
        assert!(!tall.contains("more lines below"), "{tall}");
    }

    #[test]
    fn the_marker_replaces_the_last_line_so_the_screen_never_exceeds_its_height() {
        let mut app = crowded_app();
        let lines = render(&mut app, 120, 10);
        assert_eq!(lines.len(), 10);
        assert!(lines[9].contains("more lines below"), "{:?}", lines[9]);
    }
}
