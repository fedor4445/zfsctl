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

/// Lines of Needs attention shown at once; the rest are counted.
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

/// `▕███░░░░░▏`: exactly `width` cells, filled in proportion to `percent` (over 100 is full).
pub fn gauge(percent: u8, width: u16) -> String {
    let inner = usize::from(width).saturating_sub(2);
    let filled = (usize::from(percent.min(100)) * inner + 50) / 100;
    format!("▕{}{}▏", "█".repeat(filled), "░".repeat(inner - filled))
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

/// `HOSTS ─────────`: a dim capitalised heading with a thin rule to the right edge.
fn heading(label: &str, width: usize, theme: &Theme) -> Line<'static> {
    let rule = width.saturating_sub(label.chars().count() + 3);
    Line::styled(format!(" {label} {}", "─".repeat(rule)), theme.dim())
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

fn attention_lines(d: &Dashboard, selected: Option<usize>, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![heading("NEEDS ATTENTION", width, theme)];
    if d.attention.is_empty() {
        let (text, level) = if d.verdict.level == Level::Unknown { ("checking...", Level::Unknown) } else { ("nothing needs attention", Level::Fine) };
        let (glyph, _) = glyph_word(level);
        lines.push(Line::styled(format!("   {glyph} {text}"), level_style(theme, level)));
        return lines;
    }
    let start = selected.filter(|s| *s >= ATTENTION_SHOWN).map_or(0, |s| s + 1 - ATTENTION_SHOWN);
    let subject_w = d.attention.iter().map(|i| i.subject.chars().count()).max().unwrap_or(0).clamp(6, 16);
    for (index, item) in d.attention.iter().enumerate().skip(start).take(ATTENTION_SHOWN) {
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
    let hidden = d.attention.len().saturating_sub(ATTENTION_SHOWN);
    if hidden > 0 {
        lines.push(Line::styled(format!("     +{hidden} more"), theme.dim()));
    }
    lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Pools,
    Space,
    Memory,
    Cpu,
    Load,
    Scrub,
    Errors,
}

impl Col {
    fn title(self) -> &'static str {
        match self {
            Col::Pools => "Pools",
            Col::Space => "Space",
            Col::Memory => "Memory",
            Col::Cpu => "CPU",
            Col::Load => "Load",
            Col::Scrub => "Scrub",
            Col::Errors => "Errors",
        }
    }

    fn cell(self, row: &HostRow) -> &Cell {
        match self {
            Col::Pools => &row.pools,
            Col::Space => &row.space,
            Col::Memory => &row.memory,
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

/// Columns, in display order, and the order they are given up as the screen narrows: first the gauges, then these.
const COLUMNS: [Col; 7] = [Col::Pools, Col::Space, Col::Memory, Col::Cpu, Col::Load, Col::Scrub, Col::Errors];
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
    let mut plan = Plan {
        columns: COLUMNS.to_vec(),
        gauges: true,
        name_w: longest(|h| h.name.chars().count()).clamp(4, 14),
        state_w: (longest(|h| h.state.text.chars().count()) + 2).clamp(10, 30),
    };
    while plan.total(hosts) > width {
        if plan.gauges {
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
    let mut head = vec![Span::raw("  "), Span::styled(pad("Host", plan.name_w), theme.dim()), Span::raw("  "), Span::styled(pad("State", plan.state_w), theme.dim())];
    for col in &plan.columns {
        head.push(Span::styled(format!("  {}", pad(col.title(), plan.col_width(*col, &d.hosts))), theme.dim()));
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

/// Everything below the host table: the daemon and jobs, the cloud remotes and the alerts.
fn component_lines(d: &Dashboard, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::raw(""), heading("DAEMON AND JOBS", width, theme)];
    lines.push(status_line(d.daemon.level, "daemon  ", &d.daemon.text, width, theme));
    lines.push(status_line(d.jobs.level, "jobs    ", &d.jobs.text, width, theme));
    lines.push(Line::raw(""));
    lines.push(heading("CLOUD", width, theme));
    if d.cloud.is_empty() {
        lines.push(status_line(Level::Unknown, "", "no rclone remotes found", width, theme));
    }
    for line in &d.cloud {
        lines.push(status_line(line.level, "", &line.text, width, theme));
    }
    lines.push(Line::raw(""));
    lines.push(heading("ALERTS", width, theme));
    lines.push(status_line(d.alerts.level, "", &d.alerts.text, width, theme));
    lines
}

/// The whole screen as lines, for a given width and the time to show.
pub fn screen_lines(d: &Dashboard, selected: Option<usize>, width: usize, now: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![header(d, width, now, theme), Line::raw("")];
    lines.extend(attention_lines(d, selected, width, theme));
    lines.push(Line::raw(""));
    lines.extend(host_lines(d, width, theme));
    lines.extend(component_lines(d, width, theme));
    lines
}

/// Draws the Dashboard into `area`: no borders, nothing to type into, nothing it can change.
pub fn draw_dashboard(f: &mut Frame, app: &mut App, area: Rect) {
    let d = app.dashboard();
    let selected = app.dashboard_table.selected().filter(|_| !d.attention.is_empty()).map(|s| s.min(d.attention.len() - 1));
    app.dashboard_table.select(selected);
    let now = chrono::Local::now().format("%a %d %b %H:%M").to_string();
    let lines = screen_lines(&d, selected, usize::from(area.width), &now, &app.theme);
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
        Resources { cores: 4, physical: 20 << 30, free: 10 << 30, inactive: 0, arc: 0, load: Some([0.05, 0.1, 0.1]), cpu_ticks: Some(crate::zfs::host::CpuTimes { user: busy, nice: 0, sys: 0, intr: 0, idle }), swap: None }
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
        answered(&mut app, "local", HostProbe::Reached { rtt_ms: 3, pools: vec![pool(43)], kind: None });
        answered(&mut app, "fbsd16", HostProbe::Reached { rtt_ms: 3, pools: vec![pool(85)], kind: None });
        app.host_probes.insert("fbsd16t1".into(), HostProbeState { last: Some(HostProbe::Disconnected), ..HostProbeState::default() });
        heartbeat(&mut app);
        app
    }

    #[test]
    fn a_gauge_is_exactly_as_wide_as_asked_and_clamped() {
        assert_eq!(gauge(43, 10), "▕███░░░░░▏");
        assert_eq!(gauge(0, 10), "▕░░░░░░░░▏");
        assert_eq!(gauge(100, 10), "▕████████▏");
        assert_eq!(gauge(250, 10), "▕████████▏", "over 100 is full");
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
        for want in ["Dashboard", "NEEDS ATTENTION", "HOSTS", "DAEMON", "JOBS", "CLOUD", "ALERTS", "local", "fbsd16", "fbsd16t1", "▕", "●", "▲", "✖", "tank 85% full", "unreachable", "running (pid 4242)"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        assert!(text.contains("2 to watch") || text.contains("1 issue"), "the verdict counts what needs attention:\n{text}");
    }

    #[test]
    fn at_80_columns_the_gauges_are_gone_and_the_numbers_stay() {
        let mut app = mixed_app();
        let text = render(&mut app, 80, 40).join("\n");
        assert!(!text.contains('▕'), "{text}");
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
        let shown = lines.iter().filter(|l| l.contains("unreachable")).count();
        assert!(shown >= 6, "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains("+3 more")), "9 hosts down, 6 listed: {lines:#?}");
    }

    #[test]
    fn when_all_is_well_it_says_so_in_the_attention_block_and_the_header() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Dashboard).unwrap();
        answered(&mut app, "local", HostProbe::Reached { rtt_ms: 3, pools: vec![pool(10)], kind: None });
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
        assert!(text.contains("b2 (b2)") && text.contains("no jobs"), "{text}");
    }
}
