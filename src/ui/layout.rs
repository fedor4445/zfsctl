use super::state::{App, DatasetDetailTab, PaneFocus, PermissionRow, Section};
use crate::ui::modal::{Modal, Tone};
use crate::zfs::disk::DiskMembership;
use crate::zfs::metrics::MetricsTarget;
use crate::zfs::pool::ScanStatus;
use crate::zfs::property::Property;
use crate::zfs::share::Share;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use crate::ui::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Gauge, List, ListItem, ListState, Paragraph, Row, Table, Wrap};
use ratatui::Frame;

/// Most rows the status line may take (excluding the gauge row). A
/// narrowing from the old bordered box's 9: a long multi-line command
/// failure is now clipped after this many lines. Esc still dismisses it.
const STATUS_MAX_ROWS: u16 = 3;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let (status_text, status_style, gauge) = status_bar_content(app);
    let text_rows = if status_text.is_empty() {
        0
    } else {
        wrapped_line_count(&status_text, area.width).clamp(1, STATUS_MAX_ROWS)
    };
    let gauge_rows = u16::from(gauge.is_some());
    let status_height = text_rows + gauge_rows;

    let [body_area, status_area, hint_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(status_height), Constraint::Length(1)])
        .areas(area);

    let [sidebar_area, center_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(0)])
        .areas(body_area);
    // Every list section and a drilled pool give the bottom half of the center
    // column to a metrics panel; the sidebar keeps its full height.
    let wants_metrics = app.drilled_pool.is_some() || matches!(app.current_section(), Section::Hosts | Section::Pools | Section::Datasets | Section::Snapshots | Section::Disks | Section::Shares | Section::Jobs | Section::Cloud);
    let (main_area, metrics_area) = if wants_metrics && body_area.height >= crate::ui::metrics::MIN_BODY_HEIGHT {
        let [top, bottom] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(center_area);
        (top, Some(bottom))
    } else {
        (center_area, None)
    };

    draw_sidebar(f, app, sidebar_area);
    draw_main(f, app, main_area);
    if let (Some(area), true) = (metrics_area, app.drilled_pool.is_none() && app.current_section() == Section::Jobs) {
        draw_jobs_panel(f, app, area);
    } else if let (Some(area), true) = (metrics_area, app.drilled_pool.is_none() && app.current_section() == Section::Cloud) {
        draw_cloud_panel(f, app, area);
    } else if let Some(area) = metrics_area {
        let health = app.metrics.target().and_then(|t| app.pools.iter().find(|p| Some(p.name.as_str()) == t.pool())).map(|p| p.health.clone());
        let disk = match app.metrics.target() {
            Some(MetricsTarget::Disk { name, .. }) => app.disks.iter().find(|d| &d.name == name),
            _ => None,
        };
        let share = match app.metrics.target() {
            Some(MetricsTarget::Share { dataset, smb_name }) => app.shares.iter().find(|s| match (s, smb_name) {
                (Share::Nfs { dataset: d, .. }, None) => d == dataset,
                (Share::Smb { dataset: d, name, .. }, Some(n)) => d == dataset && name == n,
                _ => false,
            }),
            _ => None,
        };
        let assessment = app.host_assessment(app.selected_host);
        let ctx = crate::ui::metrics::Context { host_name: app.active_host_name(), health: health.as_ref(), disk, share, assessment: Some(&assessment) };
        crate::ui::metrics::draw(f, &app.metrics, &ctx, &app.theme, area);
    }
    draw_status_line(f, &status_text, status_style, gauge, &app.theme, status_area);
    let hint_line = crate::ui::hints::hint_line(&crate::ui::hints::hints(app), area.width as usize, &app.theme);
    f.render_widget(Paragraph::new(hint_line), hint_area);

    if let Some(modal) = app.modal.clone() {
        draw_modal(f, &modal, app.backup_preflight.as_ref(), app.info_scroll, &app.theme, area);
    }
    if let Some(picker) = app.picker.clone() {
        draw_picker(f, &picker, &app.theme, area);
    }
    paint_canvas(f, &app.theme);
}

/// Gives every cell still on the terminal's default colors the theme's canvas (Solarized dark's teal and light text), so
/// the whole screen, popups included, has one background. Cells with their own colors keep them.
fn paint_canvas(f: &mut Frame, theme: &Theme) {
    let Some(canvas) = theme.canvas else { return };
    for cell in &mut f.buffer_mut().content {
        if cell.fg == ratatui::style::Color::Reset {
            if let Some(fg) = canvas.fg {
                cell.fg = fg;
            }
        }
        if cell.bg == ratatui::style::Color::Reset {
            if let Some(bg) = canvas.bg {
                cell.bg = bg;
            }
        }
    }
}

/// Builds the status line's text, style, and (while a resilver/scrub is
/// running and has reported a percentage) progress-gauge info: the
/// transient action-result message (if any), followed by the
/// drilled-into pool's scrub/resilver status (if any) on its own line.
/// A scan in progress uses the theme's `info` style, a scan that
/// completed with errors uses `err`; a failure message uses `err`. A clean
/// or absent scan status leaves the default style, since it isn't worth
/// drawing attention to.
fn status_bar_content(app: &App) -> (String, Style, Option<(f64, String)>) {
    let mut lines: Vec<String> = Vec::new();
    let mut style = Style::default();
    if let Some(msg) = &app.status_message {
        lines.push(msg.clone());
        if app.status_is_error() {
            style = app.theme.err;
        }
    }
    let failed = app.jobs_view.failed_count();
    if failed > 0 {
        lines.push(format!("{failed} job{} failed — see Jobs", if failed == 1 { "" } else { "s" }));
        if style == Style::default() {
            style = app.theme.err;
        }
    }
    let mut gauge = None;
    if let Some(scan) = &app.pool_scan_status {
        let (summary, scan_style) = match scan {
            ScanStatus::InProgress { summary, .. } => (summary, app.theme.info),
            ScanStatus::Completed { summary, errors: true } => (summary, app.theme.err),
            ScanStatus::Completed { summary, errors: false } => (summary, Style::default()),
        };
        lines.push(summary.clone());
        style = scan_style;
        if let ScanStatus::InProgress { percent_done: Some(pct), eta, .. } = scan {
            let label = match eta {
                Some(e) => format!("{pct:.2}% — ETA {e}"),
                None => format!("{pct:.2}%"),
            };
            gauge = Some((*pct, label));
        }
    }
    (lines.join("\n"), style, gauge)
}

fn draw_status_line(f: &mut Frame, text: &str, style: Style, gauge: Option<(f64, String)>, theme: &Theme, area: Rect) {
    if area.height == 0 {
        return;
    }
    let Some((percent, label)) = gauge else {
        f.render_widget(Paragraph::new(text).style(style).wrap(Wrap { trim: false }), area);
        return;
    };
    let [text_area, gauge_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .areas(area);
    f.render_widget(Paragraph::new(text).style(style).wrap(Wrap { trim: false }), text_area);
    let progress = Gauge::default()
        .gauge_style(theme.info)
        .ratio((percent / 100.0).clamp(0.0, 1.0))
        .label(label);
    f.render_widget(progress, gauge_area);
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme.clone();
    let items: Vec<ListItem> = app
        .sections
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let label = crate::ui::support::section_label(*s);
            // A section this host's OS has no equivalent for yet is dimmed (it still opens, and says why).
            let style = if i == app.selected_section {
                theme.selected_row()
            } else if !app.section_supported(*s) {
                theme.dim()
            } else {
                Style::default()
            };
            ListItem::new(label).style(style)
        })
        .collect();
    let title = if app.is_loading() { "zfsctl (refreshing...)" } else { "zfsctl" };
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

/// Default share of the Pools/Datasets row given to the list (the rest goes to the detail pane).
const SPLIT_LEFT_PERCENT: u16 = 60;
/// Narrowest the list / detail pane may be squeezed to make room for the other side.
const SPLIT_LEFT_MIN: u16 = 30;
const SPLIT_RIGHT_MIN: u16 = 36;

/// Splits `total` columns into (list, detail). Starts from 60/40 and moves
/// width toward whichever side is truncating (`*_want` is the width its
/// content needs, borders included), never taking the donor below its
/// minimum or below what it needs itself. If both sides truncate, or the
/// terminal is too narrow to honour the minimums, the default split stands.
fn split_widths(total: u16, left_want: u16, right_want: u16) -> (u16, u16) {
    let left = total * SPLIT_LEFT_PERCENT / 100;
    let right = total - left;
    if total < SPLIT_LEFT_MIN + SPLIT_RIGHT_MIN {
        return (left, right);
    }
    match (left_want > left, right_want > right) {
        (true, false) => {
            let give = (left_want - left).min(right.saturating_sub(right_want.max(SPLIT_RIGHT_MIN)));
            (left + give, right - give)
        }
        (false, true) => {
            let give = (right_want - right).min(left.saturating_sub(left_want.max(SPLIT_LEFT_MIN)));
            (left - give, right + give)
        }
        _ => (left, right),
    }
}

/// Name column width the Datasets table keeps before it is squeezed.
const DATASET_NAME_MIN: u16 = 24;
/// Width of a human-readable size column (`1.0G`, `100M`).
const SIZE_COL_W: u16 = 8;
/// Width of the Kind column (`Filesystem`).
const KIND_COL_W: u16 = 10;
/// Width of the Quota column (`75% of 4G ref`).
const QUOTA_COL_W: u16 = 14;
/// Width of the Lock column (`LOCKED`).
const LOCK_COL_W: u16 = 6;

/// The Quota cell: how full the nearer limit is, or `-`.
fn quota_cell(d: &crate::zfs::dataset::Dataset) -> (String, Option<u8>) {
    let facts = crate::zfs::quota::Facts { used: d.used, referenced: d.referenced, quota: d.quota, refquota: d.refquota, ..Default::default() };
    match crate::zfs::quota::usage(&facts) {
        Some(u) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a percentage, clamped
            let percent = u.percent.round().clamp(0.0, 999.0) as u16;
            (format!("{percent}% of {}{}", crate::zfs::quota::format_size(u.limit), if u.is_ref { " ref" } else { "" }), Some(u8::try_from(percent.min(255)).unwrap_or(255)))
        }
        None => ("-".to_string(), None),
    }
}

/// Longest value shown in the Properties table's Value column.
const PROPERTY_VALUE_MIN: u16 = 16;
const PROPERTY_VALUE_MAX: u16 = 40;

fn longest_property_value(props: &[Property]) -> u16 {
    let longest = props.iter().map(|p| p.value.chars().count()).max().unwrap_or(0);
    u16::try_from(longest).unwrap_or(PROPERTY_VALUE_MAX).min(PROPERTY_VALUE_MAX)
}

fn property_value_width(props: &[Property]) -> u16 {
    longest_property_value(props).max(PROPERTY_VALUE_MIN)
}

/// Detail-pane width the Properties table needs: Property(16) + the longest value + Source(10), gaps, borders.
fn property_pane_want(props: &[Property]) -> u16 {
    16 + longest_property_value(props) + 10 + 2 + 2
}

/// Widths (borders included) the list and detail pane would like, for `split_widths`.
/// Permissions and ACL tabs keep their fixed columns, so they never ask for more.
fn split_wants(app: &App, section: Section) -> (u16, u16) {
    let widest = |names: Vec<usize>| u16::try_from(names.into_iter().max().unwrap_or(0)).unwrap_or(u16::MAX);
    if section == Section::Pools {
        let name = widest(app.pools.iter().map(|p| p.name.chars().count()).collect()).max(POOL_NAME_MINIMUM);
        // Name + Health(12) + compact Used(5), one gap each, plus borders.
        (name.saturating_add(12 + 5 + 3 + 2), property_pane_want(&app.pool_properties))
    } else {
        let name = widest(app.datasets.iter().map(|d| d.name.chars().count()).collect()).max(DATASET_NAME_MIN);
        let right = match app.dataset_detail_tab {
            DatasetDetailTab::Properties => property_pane_want(&app.dataset_properties),
            _ => 0,
        };
        let quota = if app.datasets.iter().any(|d| d.quota > 0 || d.refquota > 0) { QUOTA_COL_W + 1 } else { 0 };
        let lock = if app.encryption.is_empty() { 0 } else { LOCK_COL_W + 1 };
        (name.saturating_add(SIZE_COL_W + SIZE_COL_W + KIND_COL_W + 3 + 2 + quota + lock), right)
    }
}

fn draw_main(f: &mut Frame, app: &mut App, area: Rect) {
    if app.drilled_pool.is_some() {
        draw_vdevs(f, app, area);
        return;
    }
    let section = app.current_section();
    if section == Section::Pools || section == Section::Datasets {
        let (left_want, right_want) = split_wants(app, section);
        let (left_w, _) = split_widths(area.width, left_want, right_want);
        let [left, right] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(left_w), Constraint::Min(0)])
            .areas(area);
        match section {
            Section::Pools => {
                draw_pools(f, app, left);
                draw_pool_properties(f, app, right);
            }
            Section::Datasets => {
                draw_datasets(f, app, left);
                draw_dataset_properties(f, app, right);
            }
            _ => unreachable!("only Pools/Datasets reach this branch"),
        }
        return;
    }
    match section {
        Section::Dashboard => crate::ui::dashboard_view::draw_dashboard(f, app, area),
        Section::Hosts => draw_hosts(f, app, area),
        Section::Snapshots => draw_snapshots(f, app, area),
        Section::Disks => draw_disks(f, app, area),
        Section::Shares => draw_shares(f, app, area),
        Section::BootEnvs => draw_bootenvs(f, app, area),
        Section::Jobs => draw_jobs(f, app, area),
        Section::Cloud => draw_cloud(f, app, area),
        Section::Alerts => draw_alerts(f, app, area),
        Section::Settings => draw_settings(f, app, area),
        Section::Pools | Section::Datasets => unreachable!("handled above"),
        Section::Vdevs | Section::PoolProperties | Section::DatasetProperties | Section::DatasetPermissions | Section::DatasetAcl => {
            unreachable!("never in App::sections")
        }
    }
}

/// Which Hosts columns fit. Name and State always show; Address while State keeps its minimum width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // one independent show/hide flag per column
pub struct HostColumns {
    pub addr: bool,
    pub type_: bool,
    pub tag: bool,
    pub used: bool,
    pub pools: bool,
    pub scrub: bool,
    pub jobs: bool,
    pub rtt: bool,
    pub os: bool,
}

type Step = (u16, fn(&mut HostColumns));
const TYPE_W: u16 = 6;
const USED_W: u16 = 5;
const POOLS_W: u16 = 5;
const SCRUB_W: u16 = 13;
const JOBS_W: u16 = 5;
const RTT_W: u16 = 6;
/// "Linux 6.8 (Ubuntu 24)" is 21.
const OS_W: u16 = 22;
const ADDR_W: u16 = 18;
const STATE_MIN: u16 = 22;

/// The optional columns are added in the reverse of their drop order (Type, Used, Tag, Pools, Scrub, Jobs, Rtt, OS name)
/// while they fit, stopping at the first that does not, so a narrower column can never outlive a wider one that
/// goes before it. `tag_w` is the longest tag (0 = no host has one, so no Tag column).
pub fn fit_host_columns(inner: u16, name_w: u16, tag_w: u16) -> HostColumns {
    let addr = inner >= 16 + STATE_MIN + ADDR_W + 2;
    let mut rest = inner.saturating_sub(name_w + 1 + STATE_MIN + if addr { ADDR_W + 1 } else { 0 });
    let mut c = HostColumns { addr, type_: false, tag: false, used: false, pools: false, scrub: false, jobs: false, rtt: false, os: false };
    let tag_cols = tag_w.clamp(3, 14);
    let steps: [Step; 8] = [
        (TYPE_W, |c| c.type_ = true),
        (USED_W, |c| c.used = true),
        (tag_cols, |c| c.tag = true),
        (POOLS_W, |c| c.pools = true),
        (SCRUB_W, |c| c.scrub = true),
        (JOBS_W, |c| c.jobs = true),
        (RTT_W, |c| c.rtt = true),
        (OS_W, |c| c.os = true),
    ];
    for (i, (w, set)) in steps.into_iter().enumerate() {
        if i == 2 && tag_w == 0 {
            continue;
        }
        if rest < w + 1 {
            break;
        }
        rest -= w + 1;
        set(&mut c);
    }
    c
}

fn draw_hosts(f: &mut Frame, app: &App, area: Rect) {
    use crate::zfs::host_health::{jobs_cell, pools_cell, scan_cell, used_cell, HostKind, HostProbe, Level};
    use ratatui::widgets::TableState;
    let theme = app.theme.clone();
    let inner = area.width.saturating_sub(2);
    // The name column fits the longest host name (so a narrow table squeezes the State text, never the name).
    let longest = app.hosts.iter().map(|h| h.chars().count()).max().unwrap_or(0);
    let name_w = u16::try_from(longest + 2).unwrap_or(16).clamp(8, 16);
    let tag_w = u16::try_from(app.host_tags.iter().map(|t| t.chars().count()).max().unwrap_or(0)).unwrap_or(14).min(14);
    let cols = fit_host_columns(inner, name_w, tag_w);
    let visible = app.visible_hosts();
    let rows: Vec<Row> = visible
        .iter()
        .filter_map(|&i| app.hosts.get(i).map(|name| (i, name)))
        .map(|(i, name)| {
            let assessment = app.host_assessment(i);
            let (marker_style, name_style, state_style) = match assessment.level {
                Level::Ok => (theme.ok, Style::default(), Style::default()),
                Level::Warning => (theme.warn, theme.warn, theme.warn),
                Level::Critical | Level::Disconnected => (theme.err, theme.err, theme.err),
                Level::Probing => (theme.dim(), theme.dim(), theme.dim()),
            };
            let state = app.host_probes.get(name);
            let reached = match state.and_then(|s| s.last.as_ref()) {
                Some(HostProbe::Reached { rtt_ms, pools, .. }) => Some((*rtt_ms, pools.as_slice())),
                _ => None,
            };
            let mut cells = vec![Cell::from(Line::from(vec![Span::styled("● ", marker_style), Span::styled(name.clone(), name_style)]))];
            if cols.addr {
                cells.push(Cell::from(app.host_address(i).to_string()));
            }
            cells.push(Cell::from(Span::styled(assessment.reason, state_style)));
            if cols.type_ {
                let (word, style) = match state.and_then(|s| s.kind.as_ref()) {
                    Some(k @ HostKind::Vm(_)) => (k.word(), theme.info),
                    Some(k @ HostKind::Jail) => (k.word(), theme.alt),
                    Some(k) => (k.word(), Style::default()),
                    None => ("-", Style::default()),
                };
                cells.push(Cell::from(Span::styled(word, style)));
            }
            if cols.os {
                cells.push(Cell::from(Span::styled(state.and_then(|s| s.os_name.clone()).unwrap_or_else(|| "-".to_string()), theme.dim())));
            }
            if cols.tag {
                cells.push(Cell::from(app.host_tag(i).to_string()));
            }
            if cols.used {
                let (text, level) = reached.map_or(("-".to_string(), Level::Ok), |(_, pools)| used_cell(pools));
                let style = match level {
                    Level::Critical => theme.err,
                    Level::Warning => theme.warn,
                    _ => Style::default(),
                };
                cells.push(Cell::from(Span::styled(text, style)));
            }
            if cols.pools {
                cells.push(Cell::from(reached.map_or("-".to_string(), |(_, pools)| pools_cell(pools))));
            }
            if cols.scrub {
                cells.push(Cell::from(reached.map_or("-".to_string(), |(_, pools)| scan_cell(pools))));
            }
            if cols.jobs {
                let (failed, total) = app.host_jobs_counts(name);
                let (text, bad) = jobs_cell(failed, total);
                cells.push(Cell::from(Span::styled(text, if bad { theme.err } else { Style::default() })));
            }
            if cols.rtt {
                cells.push(Cell::from(reached.map_or("-".to_string(), |(rtt, _)| format!("{rtt}ms"))));
            }
            let row = Row::new(cells);
            if i == app.selected_host { row.style(theme.selected_row()) } else { row }
        })
        .collect();
    let mut widths = vec![Constraint::Length(name_w)];
    let mut headers = vec!["Name"];
    if cols.addr {
        widths.push(Constraint::Length(ADDR_W));
        headers.push("Address");
    }
    widths.push(Constraint::Min(8));
    headers.push("State");
    for (on, w, h) in [(cols.type_, TYPE_W, "Type"), (cols.os, OS_W, "OS"), (cols.tag, tag_w.max(3), "Tag"), (cols.used, USED_W, "Used"), (cols.pools, POOLS_W, "Pools"), (cols.scrub, SCRUB_W, "Scrub"), (cols.jobs, JOBS_W, "Jobs"), (cols.rtt, RTT_W, "Rtt")] {
        if on {
            widths.push(Constraint::Length(w));
            headers.push(h);
        }
    }
    let table_area = if app.hosts.len() == 1 {
        let [table_part, hint_part] = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(0), Constraint::Length(4)]).areas(area);
        let hint = Paragraph::new("Only this host is configured. Press 'n' to add another host.".to_string()).wrap(Wrap { trim: false }).style(theme.dim());
        f.render_widget(hint, hint_part);
        table_part
    } else {
        area
    };
    let title = if app.host_filter.is_empty() && !app.host_filter_editing {
        "Hosts".to_string()
    } else {
        format!("Hosts (filter: {}{} — {}/{})", app.host_filter, if app.host_filter_editing { "_" } else { "" }, visible.len(), app.hosts.len())
    };
    let table = Table::new(rows, widths)
        .header(theme.header_row(headers))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title));
    // The offset follows the selection, so the selected host stays on screen however many hosts there are.
    let mut state = TableState::default().with_selected(visible.iter().position(|i| *i == app.selected_host));
    f.render_stateful_widget(table, table_area, &mut state);
    if visible.is_empty() && table_area.height > 3 {
        let line = Rect { x: table_area.x + 1, y: table_area.y + 2, width: table_area.width.saturating_sub(2), height: 1 };
        f.render_widget(Paragraph::new("no host matches").style(theme.dim()), line);
    }
}

/// Name column width the Pools table protects before it spends space on
/// anything beyond Health and a capacity figure.
const POOL_NAME_PREFERRED: u16 = 16;
const POOL_NAME_MINIMUM: u16 = 8;

#[derive(PartialEq)]
enum Used {
    Full,
    Compact,
    Hidden,
}

fn draw_pools(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    // Column plan, in priority order. Health always; then a capacity
    // figure (the full bar when Name can keep its preferred width,
    // otherwise just `31%`, dropped only when even Name's minimum
    // won't fit); then Size/Alloc/Free, each only while Name still keeps
    // its preferred width. Columns are dropped, never squeezed: a
    // proportional squeeze clips Health to "Heal".
    let inner = area.width.saturating_sub(2);
    // Width the non-Name columns need, counting one gap per column.
    let needs = |non_name: &[u16]| non_name.iter().sum::<u16>() + u16::try_from(non_name.len()).unwrap_or(u16::MAX);
    let mut non_name: Vec<u16> = vec![12];
    let mut headers: Vec<&str> = vec!["Name", "Health"];
    let used = if POOL_NAME_PREFERRED + needs(&[12, 14]) <= inner {
        Used::Full
    } else if POOL_NAME_MINIMUM + needs(&[12, 5]) <= inner {
        Used::Compact
    } else {
        Used::Hidden
    };
    match used {
        Used::Full => {
            non_name.push(14);
            headers.push("Used");
        }
        Used::Compact => {
            non_name.push(5);
            headers.push("Used");
        }
        Used::Hidden => {}
    }
    let mut extras = 0;
    if used == Used::Full {
        for header in ["Size", "Alloc", "Free"] {
            if POOL_NAME_PREFERRED + needs(&[non_name.as_slice(), &[14]].concat()) <= inner {
                non_name.push(14);
                headers.push(header);
                extras += 1;
            }
        }
    }

    let rows: Vec<Row> = app
        .pools
        .iter()
        .map(|p| {
            let mut cells = vec![
                Cell::from(Span::styled(p.name.clone(), theme.name_style(&p.health))),
                Cell::from(theme.health_span(&p.health)),
            ];
            match used {
                Used::Full => cells.push(Cell::from(theme.capacity_cell(p.allocated, p.size))),
                Used::Compact => cells.push(Cell::from(theme.capacity_compact(p.allocated, p.size))),
                Used::Hidden => {}
            }
            let more = [p.size.to_string(), p.allocated.to_string(), p.free.to_string()];
            cells.extend(more.into_iter().take(extras).map(Cell::from));
            Row::new(cells)
        })
        .collect();
    let widths: Vec<Constraint> = std::iter::once(Constraint::Min(POOL_NAME_MINIMUM))
        .chain(non_name.iter().map(|w| Constraint::Length(*w)))
        .collect();
    let title = format!("Pools — {} ('r' refresh, → for properties)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(theme.header_row(headers))
        .block(Block::default().borders(Borders::ALL).border_style(pane_border_style(&theme, app.pane_focus, PaneFocus::Middle)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.pools_table);
}

/// Dims a read-only property row so it's visually obvious at a glance
/// that it can't be edited — separate from the existing "read-only
/// property" status-message hint, which only appears after you've
/// already tried.
fn read_only_property_style(theme: &Theme, prop: &Property) -> Style {
    if prop.source.is_editable() {
        Style::default()
    } else {
        theme.dim()
    }
}

fn draw_pool_properties(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let rows = app.pool_properties.iter().map(|p| {
        Row::new(vec![p.name.clone(), p.value.clone(), p.source.to_string()]).style(read_only_property_style(&theme, p))
    });
    let widths = [Constraint::Length(16), Constraint::Length(property_value_width(&app.pool_properties)), Constraint::Min(10)];
    let pool = app.selected_pool_name().unwrap_or_else(|| "?".to_string());
    let title = format!("Properties — {pool} ('r' refresh, Enter to edit, ← back)");
    let table = Table::new(rows, widths)
        .header(theme.header_row(vec!["Property", "Value", "Source"]))
        .block(Block::default().borders(Borders::ALL).border_style(pane_border_style(&theme, app.pane_focus, PaneFocus::Right)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.pool_properties_table);
}

fn draw_vdevs(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let rows = app.vdevs.iter().map(|v| {
        let indented = format!("{}{}", "  ".repeat(v.depth), v.name);
        Row::new(vec![Cell::from(indented), Cell::from(theme.health_span(&v.health))])
    });
    let widths = [Constraint::Length(30), Constraint::Length(14)];
    let pool = app.drilled_pool.as_deref().unwrap_or("?");
    let title = format!("Vdevs — {pool} ('r' refresh, Esc/Backspace back)");
    let table = Table::new(rows, widths)
        .header(theme.header_row(vec!["Device", "Health"]))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.vdevs_table);
}

fn draw_datasets(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    // A Quota column only when some dataset has a limit and the pane has room for it after the columns that are always there.
    let fixed = DATASET_NAME_MIN + SIZE_COL_W + SIZE_COL_W + KIND_COL_W + 3;
    // The Lock column comes first: whether data can be read matters more than how full it is.
    let lock_col = !app.encryption.is_empty() && area.width.saturating_sub(2) > fixed + LOCK_COL_W;
    let lock_w = if lock_col { LOCK_COL_W + 1 } else { 0 };
    let quota_col = app.datasets.iter().any(|d| d.quota > 0 || d.refquota > 0) && area.width.saturating_sub(2) > fixed + lock_w + QUOTA_COL_W;
    let (warn, crit) = app.dashboard_thresholds.quota;
    let rows = app.datasets.iter().map(|d| {
        let mut cells = vec![
            Cell::from(d.name.clone()),
            Cell::from(crate::zfs::disk::human_size(d.used)),
            Cell::from(crate::zfs::disk::human_size(d.available)),
        ];
        if quota_col {
            let (text, percent) = quota_cell(d);
            let style = match percent {
                Some(p) if p >= crit => theme.err,
                Some(p) if p >= warn => theme.warn,
                _ => Style::default(),
            };
            cells.push(Cell::from(Span::styled(text, style)));
        }
        if lock_col {
            let enc = app.encryption.get(&d.name);
            let style = if enc.is_some_and(|e| e.locked) { theme.err } else { Style::default() };
            cells.push(Cell::from(Span::styled(crate::zfs::encryption::lock_text(enc), style)));
        }
        cells.push(Cell::from(format!("{:?}", d.kind)));
        Row::new(cells)
    });
    let mut widths = vec![Constraint::Min(DATASET_NAME_MIN), Constraint::Length(SIZE_COL_W), Constraint::Length(SIZE_COL_W)];
    let mut headers = vec!["Name", "Used", "Avail"];
    if quota_col {
        widths.push(Constraint::Length(QUOTA_COL_W));
        headers.push("Quota");
    }
    if lock_col {
        widths.push(Constraint::Length(LOCK_COL_W));
        headers.push("Lock");
    }
    widths.push(Constraint::Length(KIND_COL_W));
    headers.push("Kind");
    let title = format!("Datasets — {} ('r' refresh, → for properties)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(theme.header_row(headers))
        .block(Block::default().borders(Borders::ALL).border_style(pane_border_style(&theme, app.pane_focus, PaneFocus::Middle)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.datasets_table);
}

fn draw_dataset_properties(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    match app.dataset_detail_tab {
        DatasetDetailTab::Permissions => {
            draw_dataset_permissions(f, app, area);
            return;
        }
        DatasetDetailTab::Acl => {
            draw_dataset_acl(f, app, area);
            return;
        }
        DatasetDetailTab::Properties => {}
    }
    let rows = app.dataset_properties.iter().map(|p| {
        Row::new(vec![crate::zfs::property::property_label(&p.name), p.value.clone(), p.source.to_string()]).style(read_only_property_style(&theme, p))
    });
    let widths = [Constraint::Length(16), Constraint::Length(property_value_width(&app.dataset_properties)), Constraint::Min(10)];
    let dataset = app.selected_dataset_name().unwrap_or_else(|| "?".to_string());
    let title = format!("Properties — {dataset} ('r' refresh, Enter to edit, '['/']' tabs, * fixed at creation, ← back)");
    let table = Table::new(rows, widths)
        .header(theme.header_row(vec!["Property", "Value", "Source"]))
        .block(Block::default().borders(Borders::ALL).border_style(pane_border_style(&theme, app.pane_focus, PaneFocus::Right)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.dataset_properties_table);
}

fn draw_dataset_permissions(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let rows = app.dataset_permissions.iter().map(|row| match row {
        PermissionRow::Set { name, perms } => Row::new(vec![format!("Set @{name}"), "—".to_string(), perms.join(",")]),
        PermissionRow::CreateTime { perms } => Row::new(vec!["Create-time".to_string(), "—".to_string(), perms.join(",")]),
        PermissionRow::Grant { recipient, scope, perms } => {
            let target = match recipient {
                crate::zfs::permission::Recipient::User(name) => format!("user {name}"),
                crate::zfs::permission::Recipient::Group(name) => format!("group {name}"),
                crate::zfs::permission::Recipient::Everyone => "everyone".to_string(),
            };
            let scope_label = match scope {
                crate::zfs::permission::Scope::Local => "Local",
                crate::zfs::permission::Scope::Descendent => "Descendent",
                crate::zfs::permission::Scope::Both => "Local+Descendent",
            };
            Row::new(vec![target, scope_label.to_string(), perms.join(",")])
        }
    });
    let widths = [Constraint::Length(20), Constraint::Length(18), Constraint::Min(16)];
    let dataset = app.selected_dataset_name().unwrap_or_else(|| "?".to_string());
    let title = format!("Permissions — {dataset} ('r' refresh, n/s/c grant, d/x revoke, '['/']' tabs, ← back)");
    let table = Table::new(rows, widths)
        .header(theme.header_row(vec!["Target", "Scope", "Permissions"]))
        .block(Block::default().borders(Borders::ALL).border_style(pane_border_style(&theme, app.pane_focus, PaneFocus::Right)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.dataset_permissions_table);
}

fn draw_dataset_acl(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let dataset = app.selected_dataset_name().unwrap_or_else(|| "?".to_string());
    if app.selected_dataset_mountpoint().is_none() {
        let title = format!("ACL — {dataset} (no mountpoint -- not applicable, e.g. a zvol)");
        let table = Table::new(std::iter::empty::<Row>(), [Constraint::Min(1)])
            .block(Block::default().borders(Borders::ALL).border_style(pane_border_style(&theme, app.pane_focus, PaneFocus::Right)).title(title));
        f.render_widget(table, area);
        return;
    }
    let rows = app.acl_entries.iter().enumerate().map(|(i, entry)| {
        let target = entry.tag.label();
        let flags = if entry.flags.is_empty() { "—".to_string() } else { entry.flags.join("/") };
        let entry_type = match entry.entry_type {
            crate::acl::AclEntryType::Allow => "Allow",
            crate::acl::AclEntryType::Deny => "Deny",
        };
        Row::new(vec![i.to_string(), target, entry.permissions.join("/"), flags, entry_type.to_string()])
    });
    let widths = [Constraint::Length(3), Constraint::Length(14), Constraint::Min(20), Constraint::Length(16), Constraint::Length(6)];
    let title = match app.acl_unsupported() {
        // Short on purpose: the right pane is narrow. The full reason is in the status line when n or d is pressed.
        Some(reason) if !reason.contains("acltype") => format!("ACL — {dataset} ({reason})"),
        Some(_) => format!("ACL — {dataset} (acltype=posix: NFSv4 ACLs only)"),
        None => format!("ACL — {dataset} ('r' refresh, 'n' add, 'd' remove, '['/']' tabs, ← back)"),
    };
    let table = Table::new(rows, widths)
        .header(theme.header_row(vec!["#", "Target", "Permissions", "Flags", "Type"]))
        .block(Block::default().borders(Borders::ALL).border_style(pane_border_style(&theme, app.pane_focus, PaneFocus::Right)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.acl_entries_table);
}

fn draw_snapshots(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    if app.browse_view_active() {
        if let Some(view) = &app.browse_view {
            crate::ui::browse::draw(f, view, &theme, app.active_host_name(), area);
        }
        return;
    }
    if app.changes_view_active() {
        if let Some(view) = &app.changes_view {
            crate::ui::changes::draw(f, view, &theme, app.active_host_name(), area);
        }
        return;
    }
    let rows = app
        .snapshots
        .iter()
        .map(|s| Row::new(vec![format!("{}@{}", s.dataset, s.name), crate::zfs::disk::human_size(s.used), crate::zfs::disk::human_size(s.written), app.hold_marker(s).to_string()]));
    let widths = [Constraint::Length(34), Constraint::Length(SIZE_COL_W), Constraint::Length(SIZE_COL_W), Constraint::Length(4)];
    let title = format!("Snapshots — {} ('r' to refresh)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(theme.header_row(vec!["Snapshot", "Used", "Written", "Hold"]))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.snapshots_table);
}

/// The body of a section the selected host's OS has no equivalent for: a bordered box that says so, in the dim style.
fn draw_unsupported(f: &mut Frame, app: &App, area: Rect, section: Section) {
    let os = app.host_os(app.selected_host).map_or("this", crate::zfs::host_health::HostOs::word);
    let text = match app.host_os(app.selected_host) {
        Some(os) => crate::ui::support::unsupported_text(os, section),
        None => format!("{} is not supported on {os} hosts yet.", crate::ui::support::section_label(section)),
    };
    let title = format!("{} — {}", crate::ui::support::section_label(section), app.active_host_name());
    let block = Block::default().borders(Borders::ALL).border_style(app.theme.pane_border(true)).title(title);
    f.render_widget(Paragraph::new(text).style(app.theme.dim()).wrap(Wrap { trim: false }).block(block), area);
}

fn draw_disks(f: &mut Frame, app: &mut App, area: Rect) {
    use crate::zfs::disk::{human_size, DiskMedia};
    const NAME_W: u16 = 18;
    const SIZE_W: u16 = 8;
    const STATUS_MIN: u16 = 20;
    const TYPE_W: u16 = 9;
    const MODEL_W: u16 = 30;
    const SERIAL_W: u16 = 20;
    let theme = app.theme.clone();
    // Name, Size and Status are never dropped; Type, Model, Serial are
    // added only while they fit (one gap per column), so the first to go
    // on a narrow terminal is Serial, then Model, then Type.
    let inner = area.width.saturating_sub(2);
    let mut used = NAME_W + SIZE_W + STATUS_MIN + 3;
    let mut show_type = false;
    let mut show_model = false;
    let mut show_serial = false;
    if used + TYPE_W < inner {
        show_type = true;
        used += TYPE_W + 1;
        if used + MODEL_W < inner {
            show_model = true;
            used += MODEL_W + 1;
            if used + SERIAL_W < inner {
                show_serial = true;
            }
        }
    }
    let rows = app.disks.iter().map(|d| {
        let marker = if app.selected_disks.contains(&d.name) { "[x]" } else { "[ ]" };
        let status = match &d.membership {
            DiskMembership::Available => "available".to_string(),
            DiskMembership::InPool { pool, vdev } => format!("in {pool} ({vdev})"),
        };
        let style = match &d.membership {
            DiskMembership::Available => Style::default(),
            DiskMembership::InPool { .. } => theme.dim(),
        };
        let mut cells = vec![format!("{marker} {}", d.name), human_size(d.size)];
        if show_type {
            cells.push(match d.media {
                DiskMedia::Ssd => "SSD".to_string(),
                DiskMedia::Hdd(0) => "HDD".to_string(),
                DiskMedia::Hdd(rpm) => format!("HDD {rpm}"),
                DiskMedia::Unknown => String::new(),
            });
        }
        if show_model {
            cells.push(sanitise_for_display(d.model.as_deref().unwrap_or("")));
        }
        if show_serial {
            cells.push(sanitise_for_display(d.serial.as_deref().unwrap_or("")));
        }
        cells.push(status);
        Row::new(cells).style(style)
    });
    let mut widths = vec![Constraint::Length(NAME_W), Constraint::Length(SIZE_W)];
    let mut headers = vec!["Name", "Size"];
    if show_type {
        widths.push(Constraint::Length(TYPE_W));
        headers.push("Type");
    }
    if show_model {
        widths.push(Constraint::Length(MODEL_W));
        headers.push("Model");
    }
    if show_serial {
        widths.push(Constraint::Length(SERIAL_W));
        headers.push("Serial");
    }
    widths.push(Constraint::Min(STATUS_MIN));
    headers.push("Status");
    let title = format!(
        "Disks — {} ('r' refresh, Space select, 'n' create pool, 'a' add vdev, Esc clear selection)",
        app.active_host_name()
    );
    let table = Table::new(rows, widths)
        .header(theme.header_row(headers))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.disks_table);
}

pub(crate) fn smb_access_label(guest_ok: bool, valid_user: Option<&str>) -> String {
    match (guest_ok, valid_user) {
        (true, _) => "guest".to_string(),
        (false, Some(u)) => format!("user={u}"),
        (false, None) => "any Samba user".to_string(),
    }
}

/// The bottom panel of the Jobs section: the selected job's recent runs on the left, its
/// schedule, next run times and flags on the right. Drawn straight from the loaded data.
fn draw_jobs_panel(f: &mut Frame, app: &App, area: Rect) {
    use crate::ui::jobs::{preview_lines, run_summary, schedule_label};
    let theme = &app.theme;
    let selected = app.jobs_table.selected().and_then(|i| app.jobs_view.jobs.get(i));
    let title = selected.map_or_else(|| "Job".to_string(), |j| format!("Job — {}", j.id));
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(false)).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(job) = selected else {
        f.render_widget(Paragraph::new("nothing selected").style(theme.dim()), inner);
        return;
    };
    let [left, right] = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(55), Constraint::Min(0)]).areas(inner);

    let mut runs = vec![Line::styled("Last runs", theme.header())];
    let history = app.jobs_view.runs_of(&job.id, 8);
    if history.is_empty() {
        runs.push(Line::from("No runs yet"));
    }
    for run in history {
        let style = if run.result == Some(crate::daemon::history::RunResult::Failed) { theme.err } else { Style::default() };
        runs.push(Line::styled(run_summary(run, &chrono::Local), style));
    }
    f.render_widget(Paragraph::new(runs), left);

    let label = schedule_label(&job.schedule);
    let cron = job.schedule.trim();
    let mut info = vec![Line::from(if label == cron { cron.to_string() } else { format!("{label}  ({cron})") })];
    match app.jobs_view.unsupported.get(&job.id) {
        Some(reason) => info.push(Line::styled(reason.clone(), theme.err)),
        None => info.extend(preview_lines(&job.schedule, &chrono::Local::now()).into_iter().skip(1).map(Line::from)),
    }
    info.push(Line::from(format!("{}  catch up {}", if job.enabled { "enabled" } else { "disabled" }, if job.catch_up { "yes" } else { "no" })));
    if job.kind == crate::jobs::KIND_SNAPSHOT {
        info.push(Line::from(format!("keep {}  prefix {}{}", job.keep.unwrap_or(0), crate::jobs::effective_prefix(job), if job.recursive { "  recursive" } else { "" })));
    }
    if job.kind == crate::jobs::KIND_REPLICATE {
        let dest = match job.dest_host.as_deref() {
            Some(crate::jobs::DEST_FILE) => format!("file:{}", job.dest_dir.as_deref().unwrap_or("")),
            Some(host) => format!("{host}:{}", job.dest_dataset.as_deref().unwrap_or("")),
            None => String::new(),
        };
        let flags = format!("{}{}", if job.force { "  force" } else { "" }, if job.raw { "  raw" } else { "" });
        info.push(Line::from(format!("to {dest}  prefix {}{flags}", crate::jobs::effective_prefix(job))));
        if let Some(keep) = job.dest_keep {
            info.push(Line::from(format!("keep {keep} on target")));
        }
        if let Some(p) = app.jobs_view.progress_of(&job.id) {
            info.push(Line::from(crate::daemon::progress::progress_line(p)));
        }
    }
    if job.kind == crate::jobs::KIND_BACKUP {
        let keep = job.retention.as_deref().map(str::trim).filter(|k| !k.is_empty()).map_or_else(|| "keep everything".to_string(), |k| format!("keep {k}"));
        info.push(Line::from(format!("to {}  {keep}  {}", job.repository.as_deref().unwrap_or(""), crate::ui::jobs::source_summary(job))));
        if let Some(p) = app.jobs_view.progress_of(&job.id) {
            info.push(Line::from(crate::daemon::progress::progress_line(p)));
        }
    }
    if job.kind == crate::jobs::KIND_MIRROR {
        let limit = job.bwlimit.as_deref().map(|l| format!("  limit {l}/s")).unwrap_or_default();
        info.push(Line::from(format!("to {}{limit}  {}", job.remote.as_deref().unwrap_or(""), crate::ui::jobs::source_summary(job))));
        if let Some(p) = app.jobs_view.progress_of(&job.id) {
            info.push(Line::from(crate::daemon::progress::progress_line(p)));
        }
    }
    f.render_widget(Paragraph::new(info).wrap(Wrap { trim: false }), right);
}

/// The Cloud section: every rclone remote the hub (and any host with a cloud job) knows, with its jobs.
fn draw_cloud(f: &mut Frame, app: &mut App, area: Rect) {
    use crate::ui::cloud::{failed, row_fill, table_cells, Fill};
    let theme = app.theme.clone();
    let now = chrono::Utc::now();
    let rows = app.cloud_rows();
    let jobs: usize = rows.iter().map(|r| r.job_ids.len()).sum();
    let mut title = vec![Span::raw(format!("Cloud — {} remote{}, {} job{}", rows.len(), if rows.len() == 1 { "" } else { "s" }, jobs, if jobs == 1 { "" } else { "s" }))];
    let mut problems: Vec<_> = app.cloud_errors.iter().collect();
    problems.sort();
    for (host, why) in problems {
        title.push(Span::styled(format!(" — {host}: {why}"), theme.err));
    }
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(Line::from(title));
    if rows.is_empty() {
        let text = if app.cloud_known.is_empty() && app.cloud_errors.is_empty() {
            "Looking for rclone remotes..."
        } else {
            "No rclone remotes found. Create one as root with `rclone config` (see the book, chapter 15), then press 'r'. 'C' makes a cloud mirror job."
        };
        f.render_widget(Paragraph::new(text).style(theme.dim()).wrap(Wrap { trim: false }).block(block), area);
        return;
    }
    let table_rows: Vec<Row> = rows
        .iter()
        .map(|r| {
            let style = if failed(r, &app.jobs_view) || row_fill(r, &app.cloud_sizes, &app.cloud_caps) == Some(Fill::Full) {
                theme.err
            } else if row_fill(r, &app.cloud_sizes, &app.cloud_caps) == Some(Fill::Warn) {
                theme.warn
            } else if r.job_ids.is_empty() {
                theme.dim()
            } else {
                Style::default()
            };
            Row::new(table_cells(r, &app.jobs_view, &app.cloud_sizes, &app.cloud_caps, now)).style(style)
        })
        .collect();
    let widths = [Constraint::Length(16), Constraint::Length(14), Constraint::Length(8), Constraint::Length(4), Constraint::Length(12), Constraint::Min(10)];
    let table = Table::new(table_rows, widths).header(theme.header_row(vec!["Provider", "Remote", "Host", "Jobs", "Last", "In bucket"])).block(block).row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.cloud_table);
}

/// The Cloud section's detail panel: the selected remote's jobs, what `s` measured, and the F5 check.
fn draw_cloud_panel(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let row = app.cloud_table.selected().and_then(|i| app.cloud_rows().into_iter().nth(i));
    let title = row.as_ref().map_or_else(|| "Remote".to_string(), |r| format!("Remote — {}:", r.name));
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(false)).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(row) = row else {
        f.render_widget(Paragraph::new("nothing selected").style(theme.dim()), inner);
        return;
    };
    // The plan line is yellow from 80% and red from 95%; every other problem line is red.
    let fill = crate::ui::cloud::row_fill(&row, &app.cloud_sizes, &app.cloud_caps);
    let mut lines: Vec<Line> = crate::ui::cloud::detail(&row, &app.jobs_view, &app.cloud_sizes, &app.cloud_caps, chrono::Utc::now())
        .into_iter()
        .enumerate()
        .map(|(i, (text, bad))| {
            let style = match (bad, text.starts_with("Plan cap"), fill) {
                (true, true, Some(crate::ui::cloud::Fill::Warn)) => theme.warn,
                (true, _, _) => theme.err,
                _ if i == 0 => theme.dim(),
                _ => Style::default(),
            };
            Line::styled(text, style)
        })
        .collect();
    if let Some(why) = app.cloud_errors.get(&row.host) {
        lines.push(Line::styled(format!("rclone on {}: {why}", row.host), theme.err));
    }
    let mirror_remote = crate::ui::cloud::check_target(&row, &app.jobs_view.jobs);
    let check = app.backup_preflight.as_ref().filter(|c| c.password_file.is_empty() && app.hosts.get(c.host) == Some(&row.host) && Some(&c.repository) == mirror_remote.as_ref());
    if let Some(check) = check {
        lines.push(match &check.result {
            None => Line::styled("Check: checking the remote...".to_string(), theme.dim()),
            Some(crate::restic::Preflight::Ok) => Line::from("Check: OK, the remote opens.".to_string()),
            Some(crate::restic::Preflight::Problem(m)) => Line::styled(format!("Check: {m}"), theme.err),
        });
        if let Some(note) = check.file_note.as_deref() {
            lines.push(Line::styled(format!("Note: {note}"), theme.dim()));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The Alerts section: the saved `[notify]` settings, the monitor's status, the recent health events.
/// The Settings section: the rows of `ui::settings` under their group titles, the selected one as a bar.
fn draw_settings(f: &mut Frame, app: &App, area: Rect) {
    use crate::ui::settings::{Row, Setting};
    let theme = app.theme.clone();
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title("Settings — config.toml");
    let width = usize::from(area.width.saturating_sub(2));
    let height = usize::from(area.height.saturating_sub(2));
    let v = &app.settings_view;
    if !v.loaded {
        f.render_widget(Paragraph::new("Loading…").style(theme.dim()).block(block), area);
        return;
    }
    if let Some(problem) = &v.problem {
        let lines = vec![
            Line::styled(truncate_with_ellipsis(problem, width), theme.err),
            Line::styled("Fix config.toml by hand: nothing can be edited here until it parses.".to_string(), theme.dim()),
        ];
        f.render_widget(Paragraph::new(lines).block(block), area);
        return;
    }
    let selected = app.settings_table.selected();
    let mut lines: Vec<Line> = Vec::new();
    let mut selected_line = 0;
    let mut group = "";
    let title = |lines: &mut Vec<Line>, name: &'static str| {
        if !lines.is_empty() {
            lines.push(Line::raw(String::new()));
        }
        lines.push(Line::styled(name.to_string(), theme.header()));
    };
    for (i, row) in v.rows.iter().enumerate() {
        let Row { group: g, label, value, is_default, setting, .. } = row;
        if *g != group {
            group = g;
            title(&mut lines, g);
            if let (Setting::ProtLevel, Some(problem)) = (setting, &v.protection_problem) {
                lines.push(Line::styled(truncate_with_ellipsis(&format!("  {}", crate::config::protection_problem_line(problem)), width), theme.err));
            }
        }
        let label = if matches!(setting, Setting::Cap(_)) { format!("Cap of {label}") } else { label.clone() };
        let text = format!("  {label:<24} {value}");
        if selected == Some(i) {
            selected_line = lines.len();
            lines.push(Line::styled(format!("{text:<width$}"), theme.selected_row()));
        } else if *is_default {
            lines.push(Line::from(vec![Span::raw(truncate_with_ellipsis(&text, width.saturating_sub(12))), Span::styled("   (default)".to_string(), theme.dim())]));
        } else {
            lines.push(Line::raw(truncate_with_ellipsis(&text, width)));
        }
    }
    if group != "Cloud caps" {
        title(&mut lines, "Cloud caps");
        lines.push(Line::styled("  (open Cloud once to list your remotes)".to_string(), theme.dim()));
    }
    let skip = (selected_line + 1).saturating_sub(height);
    f.render_widget(Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()).block(block), area);
}

fn draw_alerts(f: &mut Frame, app: &App, area: Rect) {
    use crate::ui::alerts::{event_line, EVENTS_SHOWN};
    use crate::ui::jobs::DaemonState;
    let theme = app.theme.clone();
    let daemon = match app.jobs_view.daemon(chrono::Utc::now()) {
        DaemonState::Running { pid } => Span::raw(format!("daemon running (pid {pid})")),
        DaemonState::NotRunning => Span::styled("daemon NOT running: no alerts will be sent".to_string(), theme.err),
    };
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(Line::from(vec![Span::raw("Alerts — "), daemon]));
    let width = usize::from(area.width.saturating_sub(2));
    let height = usize::from(area.height.saturating_sub(2));
    let v = &app.alerts_view;
    if !v.loaded {
        f.render_widget(Paragraph::new("Loading…").style(theme.dim()).block(block), area);
        return;
    }
    let mut lines: Vec<Line> = vec![Line::styled("Settings (config.toml)".to_string(), theme.header())];
    for (label, value, is_default) in v.setting_rows() {
        let text = format!("{label:<14} {value}");
        if is_default && label != "Hook command" {
            lines.push(Line::from(vec![Span::raw(truncate_with_ellipsis(&text, width.saturating_sub(12))), Span::styled("   (default)".to_string(), theme.dim())]));
        } else {
            lines.push(Line::raw(truncate_with_ellipsis(&text, width)));
        }
    }
    lines.push(Line::raw(String::new()));
    if let Some(problem) = &v.problem {
        lines.push(Line::styled(truncate_with_ellipsis(problem, width), theme.err));
    }
    lines.push(Line::raw(truncate_with_ellipsis(&v.monitor_text(), width)));
    lines.push(Line::raw(match &v.last_report {
        Some(date) => format!("Last report sent {date}"),
        None => "Last report: never".to_string(),
    }));
    lines.push(Line::raw(String::new()));
    lines.push(Line::styled("Recent events".to_string(), theme.header()));
    if v.events.is_empty() {
        lines.push(Line::styled("  (none yet)".to_string(), theme.dim()));
    } else {
        let room = height.saturating_sub(lines.len()).min(EVENTS_SHOWN);
        for e in v.events.iter().skip(v.scroll).take(room) {
            lines.push(Line::raw(truncate_with_ellipsis(&event_line(e), width)));
        }
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_jobs(f: &mut Frame, app: &mut App, area: Rect) {
    use crate::ui::jobs::{job_target, last_result_text, next_run_text, schedule_label, DaemonState};
    let theme = app.theme.clone();
    let now = chrono::Utc::now();
    let local_now = chrono::Local::now();
    let daemon = match app.jobs_view.daemon(now) {
        DaemonState::Running { pid } => Span::raw(format!("daemon running (pid {pid})")),
        DaemonState::NotRunning => Span::styled("daemon NOT running: jobs will not fire".to_string(), theme.err),
    };
    let mut title = vec![Span::raw("Jobs — "), daemon];
    if app.jobs_view.error.is_some() {
        title.push(Span::styled(" — jobs.toml unreadable: showing the last good list".to_string(), theme.err));
    }
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(Line::from(title));
    if app.jobs_view.loaded && app.jobs_view.jobs.is_empty() {
        let text = "No scheduled jobs. Press 'n' for a scrub job, 'N' for a snapshot job or 'R' for a replication job, 'B' for a backup job, or 'J' on a pool (Pools) or a dataset (Datasets).";
        f.render_widget(Paragraph::new(text).style(theme.dim()).wrap(Wrap { trim: false }).block(block), area);
        return;
    }
    let rows: Vec<Row> = app
        .jobs_view
        .jobs
        .iter()
        .map(|j| {
            let last = app.jobs_view.last_run(&j.id);
            let (next, last_text, style) = if let Some(reason) = app.jobs_view.unsupported.get(&j.id) {
                ("-".to_string(), reason.clone(), theme.err)
            } else if !j.enabled {
                ("disabled".to_string(), last_result_text(last, now), theme.dim())
            } else {
                (next_run_text(&j.schedule, &local_now), last_result_text(last, now), Style::default())
            };
            Row::new(vec![j.id.clone(), j.host.clone(), job_target(j), schedule_label(&j.schedule), next, last_text]).style(style)
        })
        .collect();
    // The Target column is as wide as its longest entry (a destination or a repository path can be long), but
    // never so wide that Schedule, Next run and the start of Last are pushed off a narrow terminal.
    let others: u16 = 22 + 8 + 20 + 15 + 5 + 10; // the other columns, their gaps and Last's minimum
    let room = area.width.saturating_sub(2).saturating_sub(others).max(14);
    let longest = app.jobs_view.jobs.iter().map(|j| job_target(j).chars().count()).max().unwrap_or(0);
    let target_w = u16::try_from(longest).unwrap_or(u16::MAX).clamp(14, room);
    let widths = [Constraint::Length(22), Constraint::Length(8), Constraint::Length(target_w), Constraint::Length(20), Constraint::Length(15), Constraint::Min(10)];
    let table = Table::new(rows, widths)
        .header(theme.header_row(vec!["Job", "Host", "Target", "Schedule", "Next run", "Last"]))
        .block(block)
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.jobs_table);
}

fn draw_shares(f: &mut Frame, app: &mut App, area: Rect) {
    if !app.section_supported(Section::Shares) {
        draw_unsupported(f, app, area, Section::Shares);
        return;
    }
    let theme = app.theme.clone();
    let rows = app.shares.iter().map(|s| match s {
        Share::Nfs { dataset, raw, .. } => Row::new(vec![dataset.clone(), "NFS".to_string(), raw.clone()]),
        Share::Smb { dataset, name, valid_user, read_only, guest_ok, .. } => Row::new(vec![
            dataset.clone(),
            "SMB".to_string(),
            format!("\\\\{name}  {}  {}", smb_access_label(*guest_ok, valid_user.as_deref()), if *read_only { "ro" } else { "rw" }),
        ]),
    });
    let widths = [Constraint::Length(24), Constraint::Length(8), Constraint::Min(20)];
    let mut hints = Vec::new();
    // The services' own names: FreeBSD's `nfsd` and `smbd`, `nfs-server` and `smbd` on Linux, SMF's `nfs/server` and `smb/server`.
    let (nfs, smb) = match app.host_os(app.selected_host) {
        Some(os @ (crate::zfs::host_health::HostOs::Linux | crate::zfs::host_health::HostOs::Illumos)) => (crate::zfs::share::nfs_name(os), crate::zfs::share::smb_name(os)),
        _ => ("nfsd", "smbd"),
    };
    let (nfs_text, smb_text) = (format!("NFS unavailable: {nfs} not running"), format!("SMB unavailable: {smb} not running"));
    if app.nfs_available == Some(false) {
        hints.push(nfs_text.as_str());
    }
    let samba_listed = app.host_os(app.selected_host) != Some(crate::zfs::host_health::HostOs::Linux);
    if app.samba_available == Some(false) && samba_listed {
        hints.push(smb_text.as_str());
    }
    let hint_suffix = if hints.is_empty() { String::new() } else { format!(" [{}]", hints.join("; ")) };
    let actions = match app.host_os(app.selected_host) {
        Some(os) if !app.shares_editable() => format!("'r' refresh; read-only on {}", os.word()),
        _ => "'r' refresh, Enter edit, 'd' remove".to_string(),
    };
    let title = format!("Shares — {} ({actions}){hint_suffix}", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(theme.header_row(vec!["Dataset", "Protocol", "Details"]))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.shares_table);
}

fn draw_bootenvs(f: &mut Frame, app: &mut App, area: Rect) {
    if !app.section_supported(Section::BootEnvs) {
        draw_unsupported(f, app, area, Section::BootEnvs);
        return;
    }
    const NAME_W: u16 = 24;
    const ACTIVE_W: u16 = 12;
    const SPACE_W: u16 = 8;
    const MOUNT_W: u16 = 24;
    const CREATED_W: u16 = 16;
    let theme = app.theme.clone();
    // Name, Active and Space always show; Mountpoint then Created only while they fit.
    let inner = area.width.saturating_sub(2);
    // The name column grows to the longest name (up to 48): `freebsd-update` names are ~33 characters
    // and differ only at the end.
    let name_w = u16::try_from(app.bootenvs.iter().map(|b| b.name.chars().count()).max().unwrap_or(0).clamp(usize::from(NAME_W), 48)).unwrap_or(NAME_W);
    let mut used = name_w + ACTIVE_W + SPACE_W + 2;
    let show_mount = used + MOUNT_W < inner;
    if show_mount {
        used += MOUNT_W + 1;
    }
    let show_created = show_mount && used + CREATED_W < inner;
    let placeholder = (app.bootenvs_loaded && app.bootenvs.is_empty() && app.bootenvs_unavailable.is_none()).then(|| Row::new(vec!["(no boot environments)".to_string()]).style(theme.dim()));
    let rows = app.bootenvs.iter().map(|b| {
        let mut cells = vec![sanitise_for_display(&b.name), b.flags.label(), b.space.clone()];
        if show_mount {
            cells.push(sanitise_for_display(b.mountpoint.as_deref().unwrap_or("-")));
        }
        if show_created {
            cells.push(sanitise_for_display(&b.created));
        }
        Row::new(cells)
    });
    let rows: Vec<Row> = rows.chain(placeholder).collect();
    let mut widths = vec![Constraint::Min(name_w), Constraint::Length(ACTIVE_W), Constraint::Length(SPACE_W)];
    let mut headers = vec!["Name", "Active", "Space"];
    if show_mount {
        widths.push(Constraint::Length(MOUNT_W));
        headers.push("Mountpoint");
    }
    if show_created {
        widths.push(Constraint::Min(CREATED_W));
        headers.push("Created");
    }
    let suffix = app
        .bootenvs_unavailable
        .as_ref()
        .map(|m| format!(" [unavailable: {}]", sanitise_for_display(m.lines().next().unwrap_or(""))))
        .unwrap_or_default();
    // The Mountpoint column is the first to go on a narrow terminal, so when the selected boot environment is
    // mounted (where `bectl mount` put it) that path leads the title, ahead of the clippable rest.
    let host = app.active_host_name();
    let title = match app.selected_bootenv().filter(|b| b.can_unmount()).and_then(|b| b.mountpoint.as_deref().map(|m| (b.name.as_str(), m))) {
        Some((name, path)) => format!("'{}' mounted at {} — {host}{suffix}", sanitise_for_display(name), sanitise_for_display(path)),
        None => format!("Boot Environments — {host} ('r' to refresh){suffix}"),
    };
    let table = Table::new(rows, widths)
        .header(theme.header_row(headers))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.bootenvs_table);
}

/// Percentage width the modal popup uses — held constant; only the
/// height is now content-driven (see `draw_modal`).
const MODAL_WIDTH_PERCENT: u16 = 60;

/// Border and title for a Confirm/ConfirmTyped modal. Danger and warn
/// get a glyph plus the action's name in the title (so they read as such
/// without color) and the theme's danger/warn style on the border.
fn confirm_block(theme: &Theme, tone: Tone, label: &str, neutral_title: &str, risky_suffix: &str) -> Block<'static> {
    let (glyph, border) = match tone {
        Tone::Danger => ("✖", theme.danger),
        Tone::Warn => ("▲", theme.warn),
        Tone::Neutral => ("", Style::default()),
    };
    let title = if tone == Tone::Neutral { neutral_title.to_string() } else { format!("{glyph} {label} — {risky_suffix}") };
    Block::default().borders(Borders::ALL).border_style(border).title(Span::styled(title, border))
}

/// Extra lines drawn under a form's fields, computed from what is typed so far: for a job
/// form, what the schedule means and its next three run times.
fn form_preview(kind: &crate::ui::modal::FormKind, fields: &[crate::ui::modal::FormField]) -> Vec<String> {
    match kind {
        crate::ui::modal::FormKind::Job { .. } => crate::ui::jobs::preview_lines(fields.get(2).map_or("", |f| f.value.as_str()), &chrono::Local::now()),
        crate::ui::modal::FormKind::SnapshotJob { .. } => crate::ui::jobs::preview_lines(fields.get(5).map_or("", |f| f.value.as_str()), &chrono::Local::now()),
        crate::ui::modal::FormKind::BackupJob { .. } => crate::ui::jobs::preview_lines(fields.get(8).map_or("", |f| f.value.as_str()), &chrono::Local::now()),
        crate::ui::modal::FormKind::MirrorJob { .. } => crate::ui::jobs::preview_lines(fields.get(7).map_or("", |f| f.value.as_str()), &chrono::Local::now()),
        crate::ui::modal::FormKind::ReplicateJob { .. } => crate::ui::jobs::preview_lines(fields.get(6).map_or("", |f| f.value.as_str()), &chrono::Local::now()),
        _ => Vec::new(),
    }
}

/// The F5 check that still describes what is on screen: editing the host, repository or password file hides it.
fn current_check<'a>(kind: &crate::ui::modal::FormKind, fields: &[crate::ui::modal::FormField], check: Option<&'a crate::ui::state::PreflightCheck>) -> Option<&'a crate::ui::state::PreflightCheck> {
    let (crate::ui::modal::FormKind::BackupJob { hosts, .. } | crate::ui::modal::FormKind::MirrorJob { hosts, .. }) = kind else { return None };
    let check = check?;
    let value = |i: usize| fields.get(i).map_or("", |f| f.value.as_str()).trim();
    // A cloud mirror form has no password file: its check carries an empty one.
    let password_file = if matches!(kind, crate::ui::modal::FormKind::MirrorJob { .. }) { "" } else { value(6) };
    (hosts.get(check.host).map(String::as_str) == Some(value(0)) && check.repository == value(5) && check.password_file == password_file).then_some(check)
}

/// The backup form's F5 result as one line, and whether it is a problem. Shown only while the check still
/// describes what is on screen.
fn preflight_status(kind: &crate::ui::modal::FormKind, fields: &[crate::ui::modal::FormField], check: Option<&crate::ui::state::PreflightCheck>) -> Option<(String, bool)> {
    let check = current_check(kind, fields, check)?;
    let what = if matches!(kind, crate::ui::modal::FormKind::MirrorJob { .. }) { "remote" } else { "repository" };
    Some(match &check.result {
        None => (format!("Check: checking the {what}..."), false),
        Some(crate::restic::Preflight::Ok) => (format!("Check: OK, the {what} opens."), false),
        Some(crate::restic::Preflight::Problem(m)) => (format!("Check: {m}"), true),
    })
}

/// Advice about the password file under the F5 result: where it is, and who can read it. Empty until the check has
/// answered, and while it no longer describes what is on screen.
fn preflight_notes(kind: &crate::ui::modal::FormKind, fields: &[crate::ui::modal::FormField], check: Option<&crate::ui::state::PreflightCheck>) -> Vec<String> {
    let Some(check) = current_check(kind, fields, check).filter(|c| c.result.is_some()) else { return Vec::new() };
    let location = crate::restic::password_file_location_note(&check.password_file);
    location.into_iter().chain(check.file_note.clone()).map(|n| format!("Note: {n}")).collect()
}

/// The line under a y/n confirm's prompt. `CONFIRM_YES` is the longest, so it sizes the window.
const CONFIRM_NONE: &str = "Press y for yes or n for no, then Enter.";
const CONFIRM_YES: &str = "▶ YES selected: press Enter to run it (n or Esc to change).";
const CONFIRM_NO: &str = "▶ NO selected: press Enter to cancel (y to change).";

fn draw_modal(f: &mut Frame, modal: &Modal, check: Option<&crate::ui::state::PreflightCheck>, info_scroll: u16, theme: &Theme, area: Rect) {
    // Estimate the popup's eventual inner width (minus borders) so we
    // can reserve enough height up front — sizing the popup by a flat
    // percentage of the terminal's height let long prompts or the
    // typed-confirmation input line clip off the bottom of the popup,
    // making it look like typing did nothing.
    let approx_width = u16::try_from(u32::from(area.width) * u32::from(MODAL_WIDTH_PERCENT) / 100).unwrap_or(u16::MAX);
    let inner_width = approx_width.saturating_sub(2).max(1);

    let content_lines = match modal {
        // The prompt, a blank line and the selection line (sized by the longest of its three texts).
        // Word wrapping can need more rows than the byte-based estimate, so it gets the same ~25% + 1 margin as the forms.
        Modal::Confirm { prompt, .. } => {
            let n = wrapped_line_count(&format!("{prompt}\n\n{CONFIRM_YES}"), inner_width);
            n + n / 4 + 1
        }
        Modal::ConfirmTyped { prompt, input, .. } => {
            wrapped_line_count(&format!("{prompt}\n\n> {input}"), inner_width)
        }
        Modal::Form { fields, description, kind, .. } => {
            let preview = form_preview(kind, fields);
            let preview_lines: u16 = if preview.is_empty() { 0 } else { 1 + preview.iter().map(|l| wrapped_line_count(l, inner_width)).sum::<u16>() };
            let status_lines: u16 = preflight_status(kind, fields, check).map_or(0, |(text, _)| {
                let n = wrapped_line_count(&text, inner_width);
                1 + n + n / 4 + 1
            }) + preflight_notes(kind, fields, check)
                .iter()
                .map(|note| {
                    let n = wrapped_line_count(note, inner_width);
                    n + n / 4 + 1
                })
                .sum::<u16>();
            let fields_lines: u16 = preview_lines
                + status_lines
                + fields
                    .iter()
                    .map(|field| wrapped_line_count(&format!("  {}: {}", field.label, field.value), inner_width))
                    .sum::<u16>();
            match description {
                // +1 blank separator line between the description and the fields.
                // Word wrapping can need more rows than the byte-based
                // estimate, so the description gets the same ~25% + 1 row
                // margin as the picker popup (clamped to the terminal below).
                Some(desc) => {
                    let d = wrapped_line_count(desc, inner_width);
                    d + d / 4 + 1 + 1 + fields_lines
                }
                None => fields_lines,
            }
        }
        Modal::Info { body, .. } => wrapped_line_count(body, inner_width),
        Modal::Choose { note, rows, look, .. } => u16::try_from(note.lines().count() + 1 + rows.len() + (look.heads.len() * 2).saturating_sub(1)).unwrap_or(u16::MAX),
    };
    // +2 for the top/bottom border; floor of 5 so a one-line prompt
    // doesn't render a cramped, near-invisible box.
    let popup_height = (content_lines + 2).max(5).min(area.height);

    let popup_area = centered_rect(MODAL_WIDTH_PERCENT, popup_height, area);
    f.render_widget(Clear, popup_area);
    match modal {
        Modal::Confirm { prompt, action, choice } => {
            let tone = action.call.tone();
            let block = confirm_block(theme, tone, action.call.label(), "Confirm (y/n, then Enter)", "confirm (y/n, then Enter)");
            let selected = match tone {
                Tone::Danger => theme.danger,
                Tone::Warn => theme.warn,
                Tone::Neutral => Style::default().add_modifier(Modifier::BOLD),
            };
            let mut lines: Vec<Line> = prompt.lines().map(|l| Line::raw(l.to_string())).collect();
            lines.push(Line::raw(""));
            lines.push(match choice {
                None => Line::styled(CONFIRM_NONE, theme.dim()),
                Some(true) => Line::styled(CONFIRM_YES, selected),
                Some(false) => Line::styled(CONFIRM_NO, Style::default().add_modifier(Modifier::BOLD)),
            });
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), popup_area);
        }
        Modal::ConfirmTyped { prompt, input, action, .. } => {
            let block = confirm_block(
                theme,
                action.call.tone(),
                action.call.label(),
                "Type to confirm (Enter/Esc)",
                "type to confirm (Enter/Esc)",
            );
            let mut lines: Vec<Line> = prompt.lines().map(|l| Line::raw(l.to_string())).collect();
            lines.push(Line::raw(""));
            lines.push(Line::styled(format!("> {input}"), Style::default().add_modifier(Modifier::BOLD)));
            let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(block);
            f.render_widget(p, popup_area);
        }
        Modal::Form { title, fields, focused, description, kind } => {
            let mut lines: Vec<Line> = Vec::new();
            if let Some(desc) = description {
                lines.extend(desc.split('\n').map(|l| Line::raw(l.to_string())));
                lines.push(Line::raw(String::new()));
            }
            for (i, field) in fields.iter().enumerate() {
                let marker = if i == *focused { ">" } else { " " };
                let shown = if kind.masks_field(i) { "*".repeat(field.value.chars().count()) } else { field.value.clone() };
                lines.push(Line::raw(format!("{marker} {}: {shown}", field.label)));
            }
            let preview = form_preview(kind, fields);
            if !preview.is_empty() {
                lines.push(Line::raw(String::new()));
                lines.extend(preview.into_iter().map(Line::raw));
            }
            if let Some((text, problem)) = preflight_status(kind, fields, check) {
                lines.push(Line::raw(String::new()));
                lines.push(if problem { Line::styled(text, theme.danger) } else { Line::raw(text) });
            }
            lines.extend(preflight_notes(kind, fields, check).into_iter().map(|note| Line::styled(note, theme.warn)));
            let p = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).border_style(theme.accent).title(title.as_str()));
            f.render_widget(p, popup_area);
        }
        Modal::Info { title, body } => {
            // The wordmark (About) is drawn in the warn colour, and left out when the pop-up is too narrow for it: wrapped, it is garbage.
            let fits = usize::from(popup_area.width.saturating_sub(2)) >= crate::ui::logo::WIDTH;
            let lines: Vec<Line> = body
                .lines()
                .filter(|l| fits || !crate::ui::logo::is_wordmark_line(l))
                .map(|l| {
                    // Help's section titles ("Global", a section's name) are the lines that do not start with a space.
                    let help_title = title == "Help" && !l.is_empty() && !l.starts_with(' ');
                    let line = if crate::ui::logo::is_wordmark_line(l) {
                        Line::styled(l.to_string(), theme.warn)
                    } else if help_title {
                        Line::styled(l.to_string(), theme.header())
                    } else {
                        Line::raw(l.to_string())
                    };
                    if crate::ui::logo::is_centred_line(l) { line.centered() } else { line }
                })
                .collect();
            let p = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((info_scroll, 0))
                .block(Block::default().borders(Borders::ALL).title(title.as_str()));
            f.render_widget(p, popup_area);
        }
        Modal::Choose { title, note, rows, selected, look, .. } => {
            let mut lines: Vec<Line> = note.lines().map(|l| Line::raw(l.to_string())).collect();
            lines.push(Line::raw(""));
            let mut chosen_at = lines.len();
            for (i, row) in rows.iter().enumerate() {
                if let Some((_, head)) = look.heads.iter().find(|(at, _)| *at == i) {
                    if i > 0 {
                        lines.push(Line::raw(""));
                    }
                    lines.push(Line::styled(head.clone(), theme.header()));
                }
                let base = look.levels.get(i).map_or_else(Style::default, |level| crate::ui::dashboard_view::level_style(theme, *level));
                let text = if look.heads.is_empty() { row.clone() } else { format!("  {row}") };
                if i == *selected {
                    chosen_at = lines.len();
                    lines.push(Line::styled(text, base.patch(theme.selected_row())));
                } else {
                    lines.push(Line::styled(text, base));
                }
            }
            // Keep the chosen row in view: scroll just far enough that it is the last line shown.
            let visible = usize::from(popup_area.height.saturating_sub(2));
            let scroll = u16::try_from((chosen_at + 1).saturating_sub(visible)).unwrap_or(u16::MAX);
            let p = Paragraph::new(lines).scroll((scroll, 0)).block(Block::default().borders(Borders::ALL).title(title.as_str()));
            f.render_widget(p, popup_area);
        }
    }
}

/// Title of a ready picker popup. `Space toggle` is only mentioned for
/// multi-select pickers; in a single-select picker Space does nothing.
fn picker_title(filter: &str, multi_select: bool) -> String {
    let keys = if multi_select { "Space toggle, Enter OK, Esc cancel" } else { "Enter OK, Esc cancel" };
    if filter.is_empty() {
        format!("Pick a value (type filter, {keys})")
    } else {
        format!("Pick — filter '{filter}' ({keys})")
    }
}

/// Renders on top of `draw_modal`'s output whenever `app.picker` is
/// `Some` -- the form underneath stays drawn exactly as `draw_modal`
/// left it; this is a second, smaller popup layered above it.
fn draw_picker(f: &mut Frame, picker: &crate::ui::picker::PickerState, theme: &Theme, area: Rect) {
    const PICKER_WIDTH_PERCENT: u16 = 50;
    if picker.load != crate::ui::picker::PickerLoad::Ready {
        use crate::ui::picker::PickerLoad;
        let what = if picker.pool_host.is_some() { "names" } else { "accounts" };
        let body = match &picker.load {
            PickerLoad::Failed(msg) => {
                format!("Could not load {what}: {}\n\nEsc to close", sanitise_for_display(msg))
            }
            PickerLoad::Loading => format!("Loading {what}…\n\nEsc to close"),
            PickerLoad::Ready => unreachable!("guarded by the enclosing `!= Ready` check"),
        };
        // Width first (it only depends on the percentage), then a height
        // that fits the wrapped body so the closing hint is never clipped.
        let width = centered_rect(PICKER_WIDTH_PERCENT, 1, area).width;
        let popup_area = centered_rect(PICKER_WIDTH_PERCENT, failed_popup_height(&body, width, area.height), area);
        f.render_widget(Clear, popup_area);
        f.render_widget(
            Paragraph::new(body)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).border_style(theme.accent).title("Pick a value")),
            popup_area,
        );
        return;
    }
    let filter_lower = picker.filter.to_lowercase();
    let filtered: Vec<&String> = picker.values.iter().filter(|v| v.to_lowercase().contains(&filter_lower)).collect();

    let popup_height = (u16::try_from(filtered.len()).unwrap_or(u16::MAX).saturating_add(4)).clamp(6, area.height.saturating_sub(4).max(6));
    let popup_area = centered_rect(PICKER_WIDTH_PERCENT, popup_height, area);
    f.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = filtered
        .iter()
        .map(|value| {
            let marker = if picker.multi_select {
                if picker.checked.contains(value.as_str()) { "[x] " } else { "[ ] " }
            } else {
                ""
            };
            ListItem::new(format!("{marker}{value}"))
        })
        .collect();

    let mut title = picker_title(&picker.filter, picker.multi_select);
    if let Some(note) = &picker.note {
        let inner = popup_area.width.saturating_sub(2) as usize;
        title = format!("{title} — {}", truncate_with_ellipsis(&sanitise_for_display(note), inner));
    }
    // `ListState` (not a manually-painted `i == picker.selected` style
    // per item, and not plain `render_widget`) is what makes ratatui
    // actually scroll the visible window to keep the selected row in
    // view -- essential once the vocabulary is longer than the popup
    // is tall (the zfs-allow Permissions list has 96 entries; a
    // typical terminal shows well under half of that at once).
    let mut list_state = ListState::default();
    if !filtered.is_empty() {
        list_state.select(Some(picker.selected.min(filtered.len() - 1)));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(theme.accent).title(title))
        .highlight_style(theme.selected_row());
    f.render_stateful_widget(list, popup_area, &mut list_state);
}

pub(crate) use crate::display_text::{sanitise_for_display, truncate_with_ellipsis};

/// Height of the Loading/Failed picker popup: the wrapped body plus two
/// border rows, at least 5, never more than `max_height`. The body estimate
/// gets a ~25% + 1 row margin because word wrapping can need more rows.
fn failed_popup_height(body: &str, width: u16, max_height: u16) -> u16 {
    let inner = width.saturating_sub(2);
    let lines = wrapped_line_count(body, inner);
    (lines + lines / 4 + 1 + 2).max(5).min(max_height)
}

/// Byte-based estimate (ceil(len / width) per line) of how many terminal
/// rows `text` occupies when wrapped at `width` columns. It does not
/// replicate `Paragraph`'s word-boundary wrapping, which loses columns at
/// each break, so it can under-count with word wrapping; callers that must
/// not clip add a safety margin (see `failed_popup_height`).
fn wrapped_line_count(text: &str, width: u16) -> u16 {
    let width = width.max(1) as usize;
    text.lines()
        .map(|line| if line.is_empty() { 1 } else { u16::try_from(line.len().div_ceil(width)).unwrap_or(u16::MAX) })
        .sum::<u16>()
        .max(1)
}

/// Carves a centered rectangle out of `area`: `percent_x` percent of
/// `area`'s width, `height` rows tall (clamped to fit within `area`).
/// Standard ratatui popup pattern, adapted to a content-driven height
/// instead of a flat percentage (see `draw_modal`).
fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let height = height.min(area.height);
    let margin = area.height.saturating_sub(height) / 2;
    let [_, middle_row, _] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(margin), Constraint::Length(height), Constraint::Min(0)])
        .areas(area);
    let [_, popup, _] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .areas(middle_row);
    popup
}

/// The border style for a Pools/Datasets split-pane block: the theme's
/// accent when `pane` is the one `current` currently points at, the
/// terminal's default style otherwise.
fn pane_border_style(theme: &Theme, current: PaneFocus, pane: PaneFocus) -> Style {
    theme.pane_border(current == pane)
}

#[cfg(test)]
mod tests {
    #[test]
    fn failed_popup_height_fits_body_within_bounds() {
        assert_eq!(super::failed_popup_height("Loading accounts…\n\nEsc to close", 40, 24), 6);
        let body = format!("Could not load accounts: {}\n\nEsc to close", "x".repeat(130));
        let h = super::failed_popup_height(&body, 40, 24);
        assert!(h >= super::wrapped_line_count(&body, 38) + 2 && h > 5, "{h}");
        assert_eq!(super::failed_popup_height(&body, 40, 6), 6);
    }

    #[test]
    fn split_widths_keeps_sixty_forty_when_both_sides_fit() {
        assert_eq!(split_widths(100, 50, 30), (60, 40));
    }

    #[test]
    fn split_widths_gives_a_truncating_list_the_detail_panes_spare_width() {
        // right needs 36 (its minimum) so it can donate 40-36=4
        assert_eq!(split_widths(100, 70, 20), (64, 36));
        // asks for less than the spare: only what is needed moves
        assert_eq!(split_widths(100, 62, 20), (62, 38));
    }

    #[test]
    fn split_widths_gives_a_truncating_detail_pane_the_lists_spare_width() {
        assert_eq!(split_widths(100, 35, 55), (45, 55));
        // list never drops below its own need or below 30
        assert_eq!(split_widths(100, 50, 70), (50, 50));
        assert_eq!(split_widths(100, 10, 90), (30, 70));
    }

    #[test]
    fn split_widths_keeps_the_default_when_both_sides_truncate_or_the_terminal_is_tiny() {
        assert_eq!(split_widths(100, 80, 60), (60, 40));
        assert_eq!(split_widths(60, 80, 10), (36, 24));
    }

    #[test]
    fn split_widths_always_sums_to_total() {
        for total in 0..200u16 {
            for lw in (0..200).step_by(7) {
                for rw in (0..200).step_by(11) {
                    let (l, r) = split_widths(total, lw, rw);
                    assert_eq!(l + r, total);
                }
            }
        }
    }

    use super::*;
    use crate::ui::theme::Theme;
    // Explicit (not via `super::*`): Task 7 removes `Color` from the
    // module's own imports once the status bar stops naming colors.
    use ratatui::style::Color;

    #[test]
    fn pane_border_style_highlights_only_the_current_pane() {
        let theme = Theme::ansi();
        assert_eq!(pane_border_style(&theme, PaneFocus::Middle, PaneFocus::Middle), Style::default().fg(Color::Cyan));
        assert_eq!(pane_border_style(&theme, PaneFocus::Middle, PaneFocus::Right), Style::default());
        assert_eq!(pane_border_style(&theme, PaneFocus::Right, PaneFocus::Right), Style::default().fg(Color::Cyan));
        assert_eq!(pane_border_style(&theme, PaneFocus::Right, PaneFocus::Middle), Style::default());
    }

    use crate::zfs::pool::{Pool, PoolHealth, VdevLine};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Renders one frame and returns it as one string per terminal row.
    fn render(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect()
    }

    #[test]
    fn the_hold_column_shows_a_capital_for_ours_and_a_small_h_for_others() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
        let snap = |name: &str, holds| crate::zfs::snapshot::Snapshot { dataset: "tank/a".to_string(), name: name.to_string(), used: 1, holds, written: 0 };
        app.snapshots = vec![snap("free", 0), snap("ours", 1), snap("theirs", 1)];
        app.snapshot_tags.insert("tank/a@ours".to_string(), vec!["zfsctl".to_string(), "x".to_string()]);
        app.snapshot_tags.insert("tank/a@theirs".to_string(), vec!["x".to_string()]);
        app.snapshots_table.select(Some(0));
        let rows = render(&mut app, 100, 12);
        let row = |name: &str| rows.iter().find(|r| r.contains(name)).unwrap_or_else(|| panic!("{rows:?}")).clone();
        assert!(rows.iter().any(|r| r.contains("Hold")), "{rows:?}");
        let hold_of = |name: &str| {
            let r = row(name);
            let after = &r[r.find(name).unwrap() + name.len()..];
            after.split_whitespace().nth(2).filter(|t| *t != "│").map(str::to_string)
        };
        assert_eq!(hold_of("tank/a@free"), None, "{rows:?}");
        assert_eq!(hold_of("tank/a@ours").as_deref(), Some("H"), "{rows:?}");
        assert_eq!(hold_of("tank/a@theirs").as_deref(), Some("h"), "{rows:?}");
    }

    #[test]
    fn the_snapshot_list_has_a_written_column() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
        app.snapshots = vec![crate::zfs::snapshot::Snapshot { dataset: "tank/a".to_string(), name: "s".to_string(), used: 1024, holds: 0, written: 2048 }];
        app.snapshots_table.select(Some(0));
        let rows = render(&mut app, 100, 12);
        let header = rows.iter().find(|r| r.contains("Snapshot") && r.contains("Used")).unwrap_or_else(|| panic!("{rows:?}")).clone();
        assert!(header.find("Used").unwrap() < header.find("Written").unwrap(), "Written comes after Used: {header:?}");
        let row = rows.iter().find(|r| r.contains("tank/a@s")).unwrap_or_else(|| panic!("{rows:?}")).clone();
        let after: Vec<&str> = row[row.find("tank/a@s").unwrap()..].split_whitespace().collect();
        assert_eq!(&after[1..3], ["1.0K", "2.0K"], "{row:?}");
    }

    #[test]
    fn the_snapshots_section_shows_the_changes_view_in_place_of_the_list_and_only_for_its_host() {
        use crate::ui::changes::{Against, ChangesView, PendingChanges};
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
        app.snapshots = vec![crate::zfs::snapshot::Snapshot { dataset: "tank/a".to_string(), name: "s".to_string(), used: 1, holds: 0, written: 0 }];
        let pending = PendingChanges { host: 0, request: 1, dataset: "tank/a".to_string(), name: "s".to_string(), against: Against::Previous, cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)) };
        app.changes_view = Some(ChangesView::new(&pending));
        let shown = render(&mut app, 100, 12).join("\n");
        assert!(shown.contains("Changes —") && shown.contains("reading changes"), "{shown}");
        assert!(!shown.contains("Snapshot  "), "the list is not drawn under the view: {shown}");
        app.selected_host = 3;
        let other = render(&mut app, 100, 12).join("\n");
        assert!(!other.contains("Changes —"), "a view of another host is not shown: {other}");
    }

    #[test]
    fn the_snapshots_section_shows_the_browser_in_place_of_the_list_and_only_for_its_host() {
        use crate::ui::browse::{BrowseView, PendingBrowse};
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
        app.snapshots = vec![crate::zfs::snapshot::Snapshot { dataset: "tank/a".to_string(), name: "s".to_string(), used: 1, holds: 0, written: 0 }];
        let pending = PendingBrowse { host: 0, request: 1, dataset: "tank/a".to_string(), name: "s".to_string(), path: vec![], mountpoint: None, zone: None, os: crate::zfs::host_health::HostOs::FreeBsd, cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)) };
        app.browse_view = Some(BrowseView::new(&pending, Some(0)));
        for (w, h) in [(100, 12), (70, 8)] {
            let shown = render(&mut app, w, h).join("\n");
            assert!(shown.contains("Browse —") && shown.contains("reading the folder"), "{w}x{h}: {shown}");
            assert!(!shown.contains("Snapshot  "), "the list is not drawn under the view: {shown}");
        }
        app.selected_host = 3;
        let other = render(&mut app, 100, 12).join("\n");
        assert!(!other.contains("Browse —"), "a view of another host is not shown: {other}");
    }

    fn identity_disk(name: &str, model: &str, serial: &str) -> crate::zfs::disk::DiskInfo {
        crate::zfs::disk::DiskInfo {
            name: name.to_string(),
            size: 4_000_787_030_016,
            model: Some(model.to_string()),
            serial: Some(serial.to_string()),
            media: crate::zfs::disk::DiskMedia::Hdd(5400),
            ..Default::default()
        }
    }

    fn app_in_disks() -> App {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Disks).unwrap();
        app.disks = vec![identity_disk("ada1", "WDC WD40EFRX-68N32N0", "WD-WCC7K1234567")];
        app.disks_table.select(Some(0));
        app
    }

    #[test]
    fn disks_table_shows_type_model_serial_and_human_size_when_wide() {
        let mut app = app_in_disks();
        let text = render(&mut app, 160, 24).join("\n");
        for want in ["Type", "Model", "Serial", "HDD 5400", "WDC WD40EFRX-68N32N0", "WD-WCC7K1234567", "3.6T", "available"] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
        assert!(!text.contains("4000787030016"), "raw bytes must be gone");
    }

    #[test]
    fn disks_table_drops_serial_and_model_but_keeps_name_size_status_when_narrow() {
        let mut app = app_in_disks();
        let text = render(&mut app, 80, 24).join("\n");
        for want in ["Name", "ada1", "Size", "3.6T", "Status", "available"] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
        assert!(!text.contains("Serial") && !text.contains("WD-WCC7K1234567"), "serial must drop first:\n{text}");
        assert!(!text.contains("Model") && !text.contains("WDC WD40"), "model must drop next:\n{text}");
    }

    #[test]
    fn disks_table_columns_drop_in_order_serial_model_type_at_every_width() {
        let mut app = app_in_disks();
        app.disks[0].model = Some("Samsung SSD 970 EVO Plus 1TB".to_string());
        // (terminal width, type, model, serial); the sidebar takes ~22 columns
        for (w, ty, model, serial) in [(60u16, false, false, false), (80, false, false, false), (100, true, false, false), (120, true, true, false), (160, true, true, true)] {
            let text = render(&mut app, w, 24).join("\n");
            // At 60 columns the sidebar leaves the table so little room
            // that Name is squeezed; the headers must still all be there.
            let wants: &[&str] = if w < 80 { &["Name", "Size", "3.6T", "Status"] } else { &["Name", "ada1", "Size", "3.6T", "Status", "available"] };
            for want in wants {
                assert!(text.contains(want), "{w}: missing {want:?} in:\n{text}");
            }
            assert_eq!(text.contains("Type"), ty, "{w} type:\n{text}");
            assert_eq!(text.contains("Samsung SSD 970 EVO Plus 1TB"), model, "{w} full model:\n{text}");
            assert_eq!(text.contains("WD-WCC7K1234567"), serial, "{w} serial:\n{text}");
        }
    }

    fn app_with_form_and_disks(n: usize, w: u16, h: u16) -> Vec<String> {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Disks).unwrap();
        app.disks = (0..n)
            .map(|i| identity_disk(&format!("da{i}"), &format!("Some Very Long Manufacturer Model Name {i}"), &format!("SERIAL-NUMBER-{i:04}-ABCDEFGH")))
            .collect();
        for d in &app.disks {
            app.selected_disks.insert(d.name.clone());
        }
        app.disks_table.select(Some(0));
        app.handle_key(crossterm::event::KeyCode::Char('n'));
        render(&mut app, w, h)
    }

    #[test]
    fn create_pool_form_with_six_long_identity_lines_still_shows_last_field_at_80x24() {
        let text = app_with_form_and_disks(6, 80, 24).join("\n");
        assert!(text.contains("Redundancy"), "last input field clipped:\n{text}");
        assert!(text.contains("Name:"), "{text}");
    }

    #[test]
    fn create_pool_form_with_eight_disks_shows_overflow_line_and_last_field() {
        let text = app_with_form_and_disks(8, 80, 24).join("\n");
        assert!(text.contains("+2 more selected") && text.contains("Redundancy"), "{text}");
    }

    #[test]
    fn disks_table_strips_escapes_newlines_and_bidi_from_model_and_serial() {
        let mut app = app_in_disks();
        app.disks[0].model = Some("Ev\u{1b}[31mil\nModel\u{202e}X".to_string());
        app.disks[0].serial = Some("SN\u{7}\u{9b}1".to_string());
        let rows = render(&mut app, 160, 24);
        let text = rows.join("\n");
        for bad in ['\u{1b}', '\u{202e}', '\u{7}', '\u{9b}'] {
            assert!(!text.contains(bad), "{bad:?} leaked:\n{text}");
        }
        assert!(text.contains("Model") && text.contains("SN"), "{text}");
    }

    fn pool(name: &str, allocated: u64, size: u64, health: PoolHealth) -> Pool {
        Pool { name: name.to_string(), size, allocated, free: size.saturating_sub(allocated), health }
    }

    fn app_in_datasets(names: &[&str]) -> App {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Datasets).unwrap();
        app.datasets = names
            .iter()
            .map(|n| crate::zfs::dataset::Dataset { name: (*n).to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 })
            .collect();
        app.datasets_table.select(Some(0));
        app
    }

    #[test]
    fn a_long_dataset_name_widens_the_list_at_the_detail_panes_expense() {
        let long = "tank/backups/hosts/webserver-01/home/alice";
        let mut app = app_in_datasets(&[long]);
        assert!(!render(&mut app, 120, 24).join("\n").contains(long), "premise: a fixed 60/40 split clips this name");
        let mut app = app_in_datasets(&[long]);
        app.dataset_properties = vec![crate::zfs::property::Property { name: "compression".to_string(), value: "lz4".to_string(), source: crate::zfs::property::PropertySource::Default }];
        // 150 cols - 22 sidebar = 128: 60% = 76 can't hold the 87 the name table needs; the detail pane can lend 14.
        let text = render(&mut app, 150, 24).join("\n");
        assert!(text.contains(long), "{text}");
    }

    #[test]
    fn short_names_keep_the_default_split() {
        let mut app = app_in_datasets(&["tank/a"]);
        let rows = render(&mut app, 130, 24);
        // The detail pane's left border sits in the same column on every row where it starts.
        let col = rows[0].find("Properties").map(|i| rows[0][..i].chars().count());
        assert!(col.is_some_and(|c| c >= 22 + 60), "{:?}", rows[0]);
    }

    fn app_in_pools(pools: Vec<Pool>) -> App {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
        app.pools = pools;
        app.pools_table.select(Some(0));
        app
    }

    #[test]
    fn pools_get_a_metrics_panel_across_the_bottom_half() {
        use crate::zfs::metrics::{MetricsTarget, PoolFacts, PoolIo, PoolSample, TargetSample};
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.metrics.retarget(Some(MetricsTarget::Pool("tank".to_string())));
        let io = PoolIo { read_bps: 88_080_384, read_ops: 840, ..PoolIo::default() };
        app.metrics.apply(
            &MetricsTarget::Pool("tank".to_string()),
            Ok(TargetSample { pool: PoolSample { io, facts: PoolFacts { size: 1000, allocated: 100, ..PoolFacts::default() } }, space: None, host: None, snapshot: None, disk: None, share: None, contents: None }),
            std::time::Instant::now(),
        );
        let rows = render(&mut app, 100, 30);
        let panel = rows.iter().position(|r| r.contains("Metrics — tank")).expect("metrics panel title");
        assert!((13..=16).contains(&panel), "panel starts around the middle of the body, got row {panel}");
        let text = rows.join("\n");
        assert!(text.contains("Read  84.0M/s"), "{text}");
        assert!(text.contains("840 ops"), "{text}");
        assert!(text.contains("ONLINE"), "{text}");
        assert!(rows[..panel].iter().any(|r| r.contains("Pools —")), "the lists stay above the panel");
        assert!(rows[panel].starts_with('│'), "the sidebar keeps its full height beside the panel: {:?}", rows[panel]);
    }

    fn pools_app_with_info(contents: Option<Result<crate::zfs::pool_info::PoolContents, String>>) -> App {
        use crate::zfs::host_health::ScanSummary;
        use crate::zfs::metrics::{MetricsTarget, PoolFacts, PoolIo, PoolSample, TargetSample};
        use crate::zfs::pool_info::PoolLayout;
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        let target = MetricsTarget::Pool("tank".to_string());
        app.metrics.retarget(Some(target.clone()));
        let layout = PoolLayout { vdevs: 4, kinds: vec![("mirror".into(), 2), ("raidz1".into(), 1), ("disk".into(), 1)], disks: 10, classes: vec![("data", 6), ("log", 2), ("cache", 1), ("spare", 1)] };
        let facts = PoolFacts { size: 1000, allocated: 100, layout: Some(layout), scrub: Some(ScanSummary::Finished { when: "Thu Oct  8 19:48:07 2026".into(), errors: 0 }), ..PoolFacts::default() };
        app.metrics.apply(&target, Ok(TargetSample { pool: PoolSample { io: PoolIo::default(), facts }, space: None, host: None, snapshot: None, disk: None, share: None, contents }), std::time::Instant::now());
        app
    }

    #[test]
    fn the_pool_panel_shows_what_the_pool_is_made_of_and_holds() {
        use crate::zfs::pool_info::PoolContents;
        let contents = PoolContents { filesystems: 12, volumes: 3, snapshots: 482, compression: Some("lz4".into()), compress_ratio: Some(1.43), autotrim: Some(false) };
        let mut app = pools_app_with_info(Some(Ok(contents.clone())));
        let text = render(&mut app, 170, 40).join("\n");
        for want in ["Vdevs", "4  (2 mirror, 1 raidz1, 1 disk)", "Disks", "10  (6 data, 2 log, 1 cache, 1 spare)", "Datasets", "12 filesystems, 3 volumes", "Snapshots", "482", "Scrub", "Oct  8 19:48:07 2026", "lz4 1.43x", "Trim off"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        // On a narrow terminal the text is one column, cut but never missing the four counts the panel exists for.
        let mut narrow = pools_app_with_info(Some(Ok(contents)));
        let text = render(&mut narrow, 100, 30).join("\n");
        for want in ["Health", "Vdevs", "Disks", "Datasets", "Snapshots  482"] {
            assert!(text.contains(want), "narrow: missing {want:?} in\n{text}");
        }
        // Too short for every line: the counts outrank the lesser pool lines, and the cut is announced.
        let mut short = pools_app_with_info(Some(Ok(crate::zfs::pool_info::PoolContents { snapshots: 482, ..Default::default() })));
        let text = render(&mut short, 100, 24).join("\n");
        assert!(text.contains("Snapshots  482") && text.contains("more lines"), "{text}");
        assert!(!text.contains("Scrub   "), "{text}");
    }

    #[test]
    fn one_filesystem_and_one_volume_are_not_pluralised() {
        use crate::zfs::pool_info::PoolContents;
        let mut app = pools_app_with_info(Some(Ok(PoolContents { filesystems: 1, volumes: 1, snapshots: 0, ..Default::default() })));
        let text = render(&mut app, 170, 40).join("\n");
        assert!(text.contains("1 filesystem, 1 volume") && !text.contains("1 filesystems"), "{text}");
        assert!(text.contains("Snapshots  0"), "an empty count is 0, not a dash:\n{text}");
    }

    #[test]
    fn the_counts_show_an_ellipsis_while_pending_and_a_dash_when_the_listing_failed() {
        let mut pending = pools_app_with_info(None);
        let text = render(&mut pending, 170, 40).join("\n");
        assert!(text.contains("Datasets   …") && text.contains("Snapshots  …"), "{text}");
        assert!(text.contains("Vdevs") && text.contains("10  (6 data"), "the layout does not wait for the counts:\n{text}");
        let mut failed = pools_app_with_info(Some(Err("boom".into())));
        let text = render(&mut failed, 170, 40).join("\n");
        assert!(text.contains("Datasets   -") && text.contains("Snapshots  -"), "{text}");
    }

    #[test]
    fn other_targets_do_not_get_the_pool_info() {
        use crate::zfs::metrics::{MetricsTarget, PoolFacts, PoolIo, PoolSample, TargetSample};
        let mut app = pools_app_with_info(None);
        let target = MetricsTarget::Dataset("tank/a".to_string());
        app.metrics.retarget(Some(target.clone()));
        app.metrics.apply(&target, Ok(TargetSample { pool: PoolSample { io: PoolIo::default(), facts: PoolFacts { size: 1000, allocated: 100, ..PoolFacts::default() } }, space: None, host: None, snapshot: None, disk: None, share: None, contents: None }), std::time::Instant::now());
        let text = render(&mut app, 170, 40).join("\n");
        assert!(!text.contains("Trim ") && !text.contains("Vdevs      "), "{text}");
    }

    #[test]
    fn the_hosts_section_shows_a_whole_host_summary() {
        use crate::zfs::host::{ArcStats, HostSummary};
        use crate::zfs::metrics::{MetricsTarget, PoolFacts, PoolIo, PoolSample, TargetSample};
        let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
        app.pools = vec![pool("tank", 10, 100, PoolHealth::Online), pool("old", 50, 100, PoolHealth::Degraded)];
        app.metrics.retarget(Some(MetricsTarget::Host));
        let host = HostSummary {
            os: Some("FreeBSD 15.1-RELEASE".to_string()),
            uptime_secs: Some(90_000),
            load: Some([0.5, 0.25, 0.1]),
            arc: Some(ArcStats { size: 1 << 30, max: 4 << 30, hits: 99, misses: 1 }),
            pools: app.pools.clone(),
            status: Some("all pools are healthy".to_string()),
            ..HostSummary::default()
        };
        app.metrics.apply(
            &MetricsTarget::Host,
            Ok(TargetSample { pool: PoolSample { io: PoolIo::default(), facts: PoolFacts::default() }, space: None, host: Some(host), snapshot: None, disk: None, share: None, contents: None }),
            std::time::Instant::now(),
        );
        let text = render(&mut app, 120, 30).join("\n");
        assert!(text.contains("Metrics — host local (all pools)"), "{text}");
        for want in ["FreeBSD 15.1-RELEASE", "1d 1h", "1 not ONLINE", "hit 99.0%", "0.50 0.25 0.10", "all pools are healthy"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn datasets_list_shows_human_sizes_not_raw_bytes_at_110_columns() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Datasets).unwrap();
        app.datasets = vec![crate::zfs::dataset::Dataset {
            name: "tank/data".to_string(),
            kind: crate::zfs::dataset::DatasetKind::Filesystem,
            used: 105_250_816,
            available: 32_899_702_784,
            referenced: 0,
            mountpoint: None, quota: 0, refquota: 0,
        }];
        app.datasets_table.select(Some(0));
        let text = render(&mut app, 110, 30).join("\n");
        for want in ["100M", "30.6G", "Filesys"] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
        assert!(!text.contains("105250816") && !text.contains("32899702784"), "raw bytes must be gone:\n{text}");
    }

    #[test]
    fn snapshots_list_shows_human_sizes_not_raw_bytes() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
        app.snapshots = vec![crate::zfs::snapshot::Snapshot { dataset: "tank/data".to_string(), name: "b".to_string(), used: 2_096_734_848, holds: 0, written: 0 }];
        app.snapshots_table.select(Some(0));
        let text = render(&mut app, 110, 30).join("\n");
        assert!(text.contains("tank/data@b") && text.contains("2.0G"), "{text}");
        assert!(!text.contains("2096734848"), "raw bytes must be gone:\n{text}");
    }

    #[test]
    fn the_snapshots_section_shows_what_destroying_the_selection_would_free() {
        use crate::zfs::metrics::{MetricsTarget, PoolSample, TargetSample};
        use crate::zfs::snapshot_detail::{SnapshotDetail, SnapshotEntry};
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
        app.snapshots = vec![crate::zfs::snapshot::Snapshot { dataset: "tank/data".to_string(), name: "b".to_string(), used: 4096, holds: 0, written: 0 }];
        app.snapshots_table.select(Some(0));
        let target = MetricsTarget::Snapshot("tank/data@b".to_string());
        assert_eq!(app.metrics_target(), Some(target.clone()));
        app.metrics.retarget(Some(target.clone()));
        let entry = |n: &str, created, used| SnapshotEntry { name: format!("tank/data@{n}"), created, used };
        let detail = SnapshotDetail {
            full_name: "tank/data@b".to_string(),
            used: 4096,
            holds: 1,
            reclaim: Some(2048),
            siblings: vec![entry("a", 1_000_000_000, 100), entry("b", 1_500_000_000, 4096)],
            dataset_used: 1000,
            dataset_snapshots_used: 250,
            ..SnapshotDetail::default()
        };
        app.metrics.apply(
            &target,
            Ok(TargetSample { pool: PoolSample::default(), space: None, host: None, snapshot: Some(detail), disk: None, share: None, contents: None }),
            std::time::Instant::now(),
        );
        let text = render(&mut app, 120, 30).join("\n");
        for want in ["Metrics — tank/data@b", "Snapshot  b", "Reclaim   2.0K", "Holds     1", "(blocks destroy)", "2 snaps", "25%"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    fn cloud_app() -> App {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Cloud).unwrap();
        app.jobs_view = crate::ui::jobs::JobsView { jobs: vec![crate::jobs::Job::test_mirror("mirror-local-tank-photos", "tank/photos", "hourly", "b2:zfsctl/photos")], loaded: true, ..Default::default() };
        app.cloud_known.insert("local".into(), vec![("b2".into(), "b2".into()), ("t1".into(), "sftp".into()), ("zfsctl-aws".into(), "s3".into())]);
        app
    }

    #[test]
    fn the_cloud_section_lists_every_remote_with_its_jobs_and_the_selected_ones_detail() {
        let mut app = cloud_app();
        app.cloud_table.select(Some(0));
        app.cloud_sizes.insert("mirror-local-tank-photos".into(), crate::ui::cloud::Size { current: Some((412, 1_992_294)), old: Some((3, 24_576)), at: chrono::Utc::now(), problem: None });
        let text = render(&mut app, 130, 40).join("\n");
        for want in ["Cloud — 3 remotes, 1 job", "Remote", "In bucket", "b2:", "zfsctl-aws:", "t1:", "SFTP", "1.9 MiB", "Remote — b2:", "mirror-local-tank-photos", "current/ 1.9 MiB (412 files)", "old/ 24.0 KiB (3 files)"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        app.cloud_table.select(Some(1));
        let idle = render(&mut app, 130, 40).join("\n");
        assert!(idle.contains("Remote — t1:") && idle.contains("No job uses this remote"), "{idle}");
    }

    fn linux_host_app() -> App {
        use crate::zfs::host_health::HostOs;
        let mut app = App::new(vec!["local".to_string(), "ub24".to_string()]);
        app.host_probes.insert("ub24".to_string(), crate::ui::state::HostProbeState { os: Some(HostOs::Linux), ..Default::default() });
        app.selected_host = 1;
        app
    }

    fn limited_datasets_app() -> App {
        use crate::zfs::dataset::{Dataset, DatasetKind};
        let g: u64 = 1 << 30;
        let ds = |name: &str, used: u64, referenced: u64, quota: u64, refquota: u64| Dataset { name: name.into(), kind: DatasetKind::Filesystem, used, available: 50 * g, referenced, mountpoint: None, quota, refquota };
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Datasets).unwrap();
        app.datasets = vec![ds("tank/home", 96 * g / 10, g, 10 * g, 0), ds("tank/ref", 9 * g, 3 * g, 0, 4 * g), ds("tank/free", g, g, 0, 0)];
        app.datasets_table.select(Some(0));
        app
    }

    #[test]
    fn the_datasets_table_shows_how_full_a_quota_is_and_nothing_when_no_dataset_has_one() {
        let mut app = limited_datasets_app();
        let wide = render(&mut app, 160, 12).join("\n");
        assert!(wide.contains("Quota") && wide.contains("96% of 10G") && wide.contains("75% of 4G ref"), "{wide}");
        for d in &mut app.datasets {
            (d.quota, d.refquota) = (0, 0);
        }
        let none = render(&mut app, 160, 12).join("\n");
        assert!(!none.contains("Quota"), "no column when nothing has a limit:\n{none}");
    }

    #[test]
    fn the_datasets_table_marks_encrypted_datasets_open_or_locked() {
        use crate::zfs::encryption::Enc;
        let mut app = limited_datasets_app();
        let wide = render(&mut app, 160, 12).join("\n");
        assert!(!wide.contains("Lock"), "no column when nothing is encrypted:\n{wide}");
        let enc = |locked| Enc { cipher: "aes-256-gcm".into(), key_format: "passphrase".into(), root: "tank/home".into(), locked };
        app.encryption.insert("tank/home".into(), enc(false));
        app.encryption.insert("tank/ref".into(), enc(true));
        let wide = render(&mut app, 160, 12).join("\n");
        assert!(wide.contains("Lock") && wide.contains("open") && wide.contains("LOCKED"), "{wide}");
        let mut dropped_while_kind_stays = false;
        for width in 50..200 {
            let screen = render(&mut app, width, 12).join("\n");
            if screen.contains("Lock") {
                assert!(screen.contains("Avail") && screen.contains("Kind"), "Lock only shows with the others, width {width}");
            } else if screen.contains("Kind") {
                dropped_while_kind_stays = true;
            }
        }
        assert!(dropped_while_kind_stays);
    }

    #[test]
    fn the_quota_column_is_the_first_to_go_on_a_narrow_pane() {
        let mut app = limited_datasets_app();
        let mut dropped_while_kind_stays = false;
        for width in 60..200 {
            let screen = render(&mut app, width, 12).join("\n");
            if screen.contains("Quota") {
                assert!(screen.contains("Avail") && screen.contains("Kind"), "Quota only shows with the others, width {width}");
            } else if screen.contains("Kind") {
                dropped_while_kind_stays = true;
            }
        }
        assert!(dropped_while_kind_stays, "somewhere the Quota column is gone while Kind still fits");
    }

    #[test]
    fn the_sidebar_dims_unsupported_entries_for_the_selected_host() {
        use ratatui::style::Modifier;
        let mut app = linux_host_app();
        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let style_of = |label: &str| {
            let (x, y) = (0..30u16).find_map(|y| (0..40u16).find_map(|x| ((0..label.len() as u16).all(|i| buf[(x + i, y)].symbol() == &label[i as usize..=i as usize])).then_some((x, y)))).unwrap_or_else(|| panic!("no {label} on screen"));
            (buf[(x, y)].fg, buf[(x, y)].modifier)
        };
        let dim = app.theme.dim();
        let dimmed = |label: &str| style_of(label).0 == dim.fg.unwrap_or(ratatui::style::Color::Reset) && style_of(label).1.contains(dim.add_modifier | Modifier::empty());
        assert!(dimmed("Boot Envs"), "unsupported entries are dimmed");
        assert!(!dimmed("Disks") && !dimmed("Shares") && !dimmed("Pools") && !dimmed("Jobs"), "supported ones are not");
        app.selected_host = 0;
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf2 = terminal.backend().buffer().clone();
        let boot = (0..30u16).find(|y| (0..40u16).any(|x| buf2[(x, *y)].symbol() == "B" && buf2[(x + 1, *y)].symbol() == "o" && buf2[(x + 5, *y)].symbol() == "E")).unwrap();
        let x = (0..40u16).find(|x| buf2[(*x, boot)].symbol() == "B").unwrap();
        assert_ne!(buf2[(x, boot)].fg, dim.fg.unwrap_or(ratatui::style::Color::Reset), "back on the FreeBSD host it is restored");
    }

    #[test]
    fn a_dimmed_section_says_why_in_its_body() {
        let mut app = linux_host_app();
        for (section, text) in [(Section::BootEnvs, "Boot Envs is not supported on Linux hosts yet.")] {
            app.selected_section = app.sections.iter().position(|s| *s == section).unwrap();
            let screen = render(&mut app, 120, 20).join("\n");
            assert!(screen.contains(text), "{screen}");
        }
    }

    #[test]
    fn the_shares_title_names_the_services_of_the_hosts_os() {
        let mut app = linux_host_app();
        app.selected_section = app.sections.iter().position(|s| *s == Section::Shares).unwrap();
        (app.nfs_available, app.samba_available) = (Some(false), Some(false));
        let screen = render(&mut app, 160, 12).join("\n");
        assert!(screen.contains("NFS unavailable: nfs-server not running") && screen.contains("read-only on Linux"), "{screen}");
        assert!(!screen.contains("SMB unavailable"), "Samba shares are not listed on Linux, so its state says nothing here:\n{screen}");
        app.selected_host = 0;
        let screen = render(&mut app, 160, 12).join("\n");
        assert!(screen.contains("NFS unavailable: nfsd not running"), "{screen}");
    }

    #[test]
    fn the_cloud_table_starts_with_the_providers_name() {
        let mut app = cloud_app();
        app.cloud_table.select(Some(0));
        let lines = render(&mut app, 130, 40);
        let header = lines.iter().find(|l| l.contains("Provider")).expect("a Provider header");
        assert!(header.find("Provider").unwrap() < header.find("Remote").unwrap(), "{header}");
        let text = lines.join("\n");
        for want in ["Backblaze B2", "SFTP", "Amazon S3 (etc.)"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn the_cloud_section_shows_how_much_of_the_plan_cap_is_used_and_warns_near_the_limit() {
        let mut app = cloud_app();
        app.cloud_table.select(Some(0));
        app.cloud_caps.insert("b2".into(), Ok(10_000_000_000));
        app.cloud_sizes.insert("mirror-local-tank-photos".into(), crate::ui::cloud::Size { current: Some((412, 1_992_294)), old: Some((3, 24_576)), at: chrono::Utc::now(), problem: None });
        let text = render(&mut app, 130, 40).join("\n");
        assert!(text.contains("1.9 MiB of 10 GB (0.02%)") && text.contains("Plan cap 10 GB: using 1.9 MiB (0.02%)"), "{text}");
        app.cloud_caps.insert("b2".into(), Err("ten gigs".into()));
        assert!(render(&mut app, 130, 40).join("\n").contains("Plan cap 'ten gigs'"));
        app.cloud_caps.insert("b2".into(), Ok(2_500_000));
        let warn = render(&mut app, 130, 40).join("\n");
        assert!(warn.contains("of 2.5 MB (81%)") && warn.contains("Plan cap 2.5 MB: using 1.9 MiB (81%)"), "{warn}");
    }

    #[test]
    fn the_plan_percentage_fits_a_110_column_terminal() {
        let mut app = cloud_app();
        app.cloud_table.select(Some(0));
        app.cloud_caps.insert("b2".into(), Ok(10_000_000_000));
        app.cloud_sizes.insert("mirror-local-tank-photos".into(), crate::ui::cloud::Size { current: Some((412, 1_992_294)), old: None, at: chrono::Utc::now(), problem: None });
        let text = render(&mut app, 110, 30).join("\n");
        assert!(text.contains("1.9 MiB of 10 GB (0.02%)"), "{text}");
    }

    #[test]
    fn the_cloud_section_says_why_a_host_could_not_be_asked_and_what_to_do_when_there_are_no_remotes() {
        let mut app = cloud_app();
        app.cloud_errors.insert("local".into(), "rclone is not installed on local".into());
        let text = render(&mut app, 130, 40).join("\n");
        assert!(text.contains("local: rclone is not installed on local"), "{text}");
        let mut empty = App::new(vec!["local".to_string()]);
        empty.selected_section = empty.sections.iter().position(|s| *s == Section::Cloud).unwrap();
        assert!(render(&mut empty, 130, 40).join("\n").contains("Looking for rclone remotes"));
        empty.cloud_known.insert("local".into(), vec![]);
        assert!(render(&mut empty, 130, 40).join("\n").contains("No rclone remotes found"));
    }

    #[test]
    fn the_cloud_section_shows_the_f5_check_for_the_selected_remote_and_survives_a_small_screen() {
        let mut app = cloud_app();
        app.cloud_table.select(Some(0));
        app.backup_preflight = Some(crate::ui::state::PreflightCheck { host: 0, repository: "b2:zfsctl/photos".into(), password_file: String::new(), result: Some(crate::restic::Preflight::Ok), file_note: None });
        assert!(render(&mut app, 130, 40).join("\n").contains("Check: OK, the remote opens."));
        app.backup_preflight = Some(crate::ui::state::PreflightCheck { host: 0, repository: "other:x".into(), password_file: String::new(), result: Some(crate::restic::Preflight::Ok), file_note: None });
        assert!(!render(&mut app, 130, 40).join("\n").contains("Check:"), "a check of another remote is not shown here");
        let tiny = render(&mut app, 60, 12).join("\n");
        assert!(tiny.contains("Cloud"), "{tiny}");
    }

    #[test]
    fn the_disks_section_shows_a_disks_activity_and_pool_health() {
        use crate::zfs::disk::{DiskInfo, DiskMedia, DiskMembership};
        use crate::zfs::disk_detail::DiskLive;
        use crate::zfs::metrics::{ErrorCounts, MetricsTarget, PoolIo, PoolSample, TargetSample};
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Disks).unwrap();
        app.disks = vec![DiskInfo {
            name: "ada1".to_string(),
            size: 5 << 30,
            membership: DiskMembership::InPool { pool: "tpool".to_string(), vdev: "raidz2-0".to_string() },
            model: Some("QEMU HARDDISK".to_string()),
            serial: Some("SN123".to_string()),
            media: DiskMedia::Hdd(7200),
        }];
        app.disks_table.select(Some(0));
        let target = MetricsTarget::Disk { name: "ada1".to_string(), pool: Some("tpool".to_string()) };
        assert_eq!(app.metrics_target(), Some(target.clone()));
        app.metrics.retarget(Some(target.clone()));
        let io = PoolIo { read_bps: 3584, write_ops: 4, write_latency_ns: Some(3_000_000), ..PoolIo::default() };
        let live = DiskLive { state: Some("DEGRADED".to_string()), errors: Some(ErrorCounts { read: 1, write: 0, cksum: 2 }), busy_percent: Some(12.0), queue: Some(2) };
        app.metrics.apply(
            &target,
            Ok(TargetSample { pool: PoolSample { io, ..PoolSample::default() }, space: None, host: None, snapshot: None, disk: Some(live), share: None, contents: None }),
            std::time::Instant::now(),
        );
        let text = render(&mut app, 120, 30).join("\n");
        for want in ["Metrics — disk ada1", "QEMU HARDDISK", "SN123", "HDD 7200 rpm", "tpool / raidz2-0", "DEGRADED   R1 W0 C2", "12%", "queue 2", "lat 3.0ms"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn the_shares_section_shows_access_clients_and_space_for_nfs_and_smb() {
        use crate::zfs::metrics::{DatasetSpace, MetricsTarget, PoolSample, TargetSample};
        use crate::zfs::share::Share;
        use crate::zfs::share_detail::ShareLive;
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Shares).unwrap();
        app.shares = vec![
            Share::Nfs { dataset: "tank/a".to_string(), raw: "x".to_string(), networks: vec!["10.0.0.0/24".to_string()], hosts: vec![], read_only: true, root: Some("root".to_string()) },
            Share::Smb { dataset: "tank/b".to_string(), name: "bee".to_string(), path: "/b".to_string(), valid_user: Some("carol".to_string()), read_only: false, guest_ok: false },
        ];
        let space = DatasetSpace { used: 1 << 30, available: 3 << 30, quota: 10 << 30, compression: "lz4".to_string(), compress_ratio: 1.5, ..DatasetSpace::default() };
        let sample = |clients| {
            Ok(TargetSample {
                pool: PoolSample::default(),
                space: Some(space.clone()),
                host: None,
                snapshot: None,
                disk: None,
                share: Some(ShareLive { mountpoint: Some("/tank/a".to_string()), clients }),
                contents: None,
            })
        };
        // NFS row
        app.shares_table.select(Some(0));
        let nfs = MetricsTarget::Share { dataset: "tank/a".to_string(), smb_name: None };
        assert_eq!(app.metrics_target(), Some(nfs.clone()));
        app.metrics.retarget(Some(nfs.clone()));
        app.metrics.apply(&nfs, sample(Some(vec!["10.0.0.7".to_string(), "10.0.0.8".to_string()])), std::time::Instant::now());
        let text = render(&mut app, 120, 30).join("\n");
        for want in ["Metrics — NFS share tank/a", "10.0.0.0/24  ro  root=root", "/tank/a", "2: 10.0.0.7, 10.0.0.8", "25%", "Quota    10.0G", "lz4 1.50x"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        // SMB row, host won't say who is connected
        app.shares_table.select(Some(1));
        let smb = MetricsTarget::Share { dataset: "tank/b".to_string(), smb_name: Some("bee".to_string()) };
        assert_eq!(app.metrics_target(), Some(smb.clone()));
        app.metrics.retarget(Some(smb.clone()));
        app.metrics.apply(&smb, sample(None), std::time::Instant::now());
        let text = render(&mut app, 120, 30).join("\n");
        for want in ["SMB share \\\\bee", "user=carol  rw", "unavailable"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    fn jobs_app(jobs_list: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        crate::jobs::save(&dir.path().join("jobs.toml"), jobs_list).unwrap();
        let mut app = App::new(vec!["local".to_string()]);
        app.state_dir = dir.path().to_path_buf();
        app.selected_section = app.sections.iter().position(|s| *s == Section::Jobs).unwrap();
        app.refresh_jobs();
        (app, dir)
    }

    fn a_job(id: &str, schedule: &str) -> crate::jobs::Job {
        crate::jobs::Job { id: id.into(), host: "local".into(), kind: crate::jobs::KIND_SCRUB.into(), pool: Some("tank".into()), schedule: schedule.into(), catch_up: true, enabled: true, created_at: None, dataset: None, recursive: false, prefix: None, keep: None, dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, remote: None, bwlimit: None, force: false, raw: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None }
    }

    #[test]
    fn the_jobs_table_shows_each_job_with_its_schedule_next_run_and_last_result() {
        let (mut app, dir) = jobs_app(&[a_job("scrub-local-tank", "0 3 * * 0"), a_job("custom", "*/10 * * * *")]);
        let h = crate::daemon::history::History::new(dir.path().join("history.jsonl"));
        let when = chrono::Utc::now() - chrono::Duration::hours(2);
        h.append(&crate::daemon::history::Event { job_id: "scrub-local-tank".into(), run_id: "r1".into(), event: crate::daemon::history::EventKind::Finished, at: when, scheduled_for: Some(when), result: Some(crate::daemon::history::RunResult::Ok), message: String::new(), catch_up: false, summary: None }).unwrap();
        app.refresh_jobs();
        let text = render(&mut app, 120, 30).join("\n");
        for want in ["Job", "Host", "Target", "Schedule", "Next run", "Last", "scrub-local-tank", "Weekly, Sunday 03:00", "*/10 * * * *", "ok 2h ago", "never"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn the_next_run_column_is_wide_enough_for_the_whole_time() {
        let (mut app, _dir) = jobs_app(&[a_job("a", "* * * * *")]);
        let before = crate::ui::jobs::next_run_text("* * * * *", &chrono::Local::now());
        let text = render(&mut app, 120, 30).join("\n");
        let after = crate::ui::jobs::next_run_text("* * * * *", &chrono::Local::now());
        assert!(text.contains(&before) || text.contains(&after), "the next-run time is cut off (wanted {before:?} or {after:?}):\n{text}");
    }

    #[test]
    fn a_disabled_job_says_so_and_an_unsupported_one_shows_why() {
        let mut off = a_job("off", "0 3 * * 0");
        off.enabled = false;
        let mut bad = a_job("bad", "0 3 * * 0");
        bad.host = "gone".into();
        let (mut app, _dir) = jobs_app(&[off, bad]);
        let text = render(&mut app, 150, 30).join("\n");
        assert!(text.contains("disabled"), "{text}");
        assert!(text.contains("'gone' is not in config.toml"), "{text}");
    }

    #[test]
    fn the_jobs_title_says_whether_the_daemon_is_running() {
        let (mut app, dir) = jobs_app(&[a_job("a", "0 3 * * 0")]);
        let text = render(&mut app, 120, 30).join("\n");
        assert!(text.contains("daemon NOT running"), "{text}");
        crate::daemon::heartbeat::write(&dir.path().join("daemon.json"), &crate::daemon::heartbeat::Heartbeat { pid: 4242, started_at: chrono::Utc::now(), last_tick: chrono::Utc::now() }).unwrap();
        app.refresh_jobs();
        let text = render(&mut app, 120, 30).join("\n");
        assert!(text.contains("daemon running (pid 4242)"), "{text}");
    }

    #[test]
    fn an_empty_jobs_list_explains_how_to_add_one_and_an_unreadable_file_is_flagged() {
        let (mut app, dir) = jobs_app(&[]);
        let text = render(&mut app, 120, 30).join("\n");
        assert!(text.contains("No scheduled jobs") && text.contains("'n'"), "{text}");
        std::fs::write(dir.path().join("jobs.toml"), "not [[[ toml").unwrap();
        app.refresh_jobs();
        let text = render(&mut app, 140, 30).join("\n");
        assert!(text.contains("jobs.toml unreadable"), "{text}");
    }

    fn job_form_app() -> App {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
        app.pools = vec![pool("tank", 10, 100, PoolHealth::Online)];
        app.pools_table.select(Some(0));
        app.handle_key(crossterm::event::KeyCode::Char('J'));
        app
    }

    #[test]
    fn the_job_form_previews_what_the_schedule_means_and_when_it_next_runs() {
        let mut app = job_form_app();
        let text = render(&mut app, 100, 30).join("\n");
        for want in ["New scrub job", "Host: local", "Pool: tank", "Schedule: 0 3 * * 0", "Weekly, Sunday 03:00", "Next: Sun "] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        assert_eq!(text.matches("03:00").count(), 4, "the label and the three listed run times:\n{text}");
    }

    #[test]
    fn the_job_forms_preview_follows_what_is_typed_and_shows_a_parse_error() {
        let mut app = job_form_app();
        let Some(crate::ui::modal::Modal::Form { fields, .. }) = &mut app.modal else { panic!() };
        fields[2].value = "61 * * * *".to_string();
        let text = render(&mut app, 100, 30).join("\n");
        assert!(text.contains("Schedule error:") && text.contains("outside 0-59"), "{text}");
        assert!(!text.contains("Next:"));
    }

    fn history_event(job_id: &str, run_id: &str, kind: crate::daemon::history::EventKind, ago_mins: i64, result: Option<crate::daemon::history::RunResult>, message: &str) -> crate::daemon::history::Event {
        let at = chrono::Utc::now() - chrono::Duration::minutes(ago_mins);
        crate::daemon::history::Event { job_id: job_id.into(), run_id: run_id.into(), event: kind, at, scheduled_for: Some(at), result, message: message.into(), catch_up: false, summary: None }
    }

    #[test]
    fn the_jobs_panel_shows_the_selected_jobs_runs_schedule_next_times_and_flags() {
        use crate::daemon::history::{EventKind, History, RunResult};
        let (mut app, dir) = jobs_app(&[a_job("scrub-local-tank", "0 3 * * 0")]);
        let h = History::new(dir.path().join("history.jsonl"));
        h.append(&history_event("scrub-local-tank", "r1", EventKind::Started, 300, None, "")).unwrap();
        h.append(&history_event("scrub-local-tank", "r1", EventKind::Finished, 290, Some(RunResult::Failed), "scrub repaired 12K with 3 errors")).unwrap();
        h.append(&history_event("scrub-local-tank", "r2", EventKind::Skipped, 10, Some(RunResult::Skipped), "skipped: already scanning")).unwrap();
        app.refresh_jobs();
        app.handle_key(crossterm::event::KeyCode::Down);
        let text = render(&mut app, 130, 34).join("\n");
        for want in ["Job — scrub-local-tank", "Last runs", "failed", "10m 00s", "scrub repaired 12K", "skipped", "skipped: already scanning", "Weekly, Sunday 03:00", "0 3 * * 0", "Next:", "enabled", "catch up"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn the_jobs_panel_says_when_nothing_is_selected_and_shows_an_unsupported_reason_in_full() {
        let mut bad = a_job("bad", "0 3 * * 0");
        bad.host = "somewhere-else".into();
        let (mut app, _dir) = jobs_app(&[bad]);
        let text = render(&mut app, 120, 34).join("\n");
        assert!(text.contains("nothing selected"), "{text}");
        app.handle_key(crossterm::event::KeyCode::Down);
        let text = render(&mut app, 150, 34).join("\n");
        assert!(text.contains("host 'somewhere-else' is not in config.toml"), "{text}");
        assert!(text.contains("No runs yet") || text.contains("never"), "{text}");
    }

    #[test]
    fn the_jobs_panel_is_dropped_on_a_short_terminal_so_the_table_keeps_its_height() {
        let (mut app, _dir) = jobs_app(&[a_job("a", "0 3 * * 0")]);
        let text = render(&mut app, 120, 12).join("\n");
        assert!(!text.contains("Last runs"), "{text}");
    }

    #[test]
    fn failed_jobs_are_flagged_in_the_status_line_from_any_section() {
        use crate::daemon::history::{EventKind, History, RunResult};
        let (mut app, dir) = jobs_app(&[a_job("a", "0 3 * * 0"), a_job("b", "0 3 * * 0")]);
        let h = History::new(dir.path().join("history.jsonl"));
        h.append(&history_event("a", "r1", EventKind::Finished, 60, Some(RunResult::Failed), "boom")).unwrap();
        app.refresh_jobs();
        app.selected_section = app.sections.iter().position(|s| *s == Section::Hosts).unwrap();
        let text = render(&mut app, 120, 30).join("\n");
        assert!(text.contains("1 job failed"), "{text}");
        h.append(&history_event("b", "r2", EventKind::Finished, 30, Some(RunResult::Failed), "boom")).unwrap();
        app.refresh_jobs();
        assert!(render(&mut app, 120, 30).join("\n").contains("2 jobs failed"));
        h.append(&history_event("a", "r3", EventKind::Finished, 5, Some(RunResult::Ok), "")).unwrap();
        h.append(&history_event("b", "r4", EventKind::Finished, 5, Some(RunResult::Ok), "")).unwrap();
        app.refresh_jobs();
        assert!(!render(&mut app, 120, 30).join("\n").contains("failed —"), "recovered jobs clear the indicator");
    }

    #[test]
    fn tiny_terminals_have_no_metrics_panel_and_an_empty_list_says_nothing_is_selected() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        assert!(!render(&mut app, 100, 12).join("\n").contains("Metrics"), "too short for a panel");
        app.selected_section = app.sections.iter().position(|s| *s == Section::Shares).unwrap();
        let text = render(&mut app, 100, 30).join("\n");
        assert!(text.contains("nothing selected"), "{text}");
    }

    #[test]
    fn pools_table_shows_health_glyphs_and_capacity_bars() {
        let mut app = app_in_pools(vec![
            pool("tank", 31, 100, PoolHealth::Online),
            pool("scratch", 90, 100, PoolHealth::Degraded),
        ]);
        let rows = render(&mut app, 200, 30).join("\n");
        assert!(rows.contains("● ONLINE"), "{rows}");
        assert!(rows.contains("▲ DEGRADED"), "{rows}");
        assert!(rows.contains("31%"), "{rows}");
        assert!(rows.contains("90%!"), "{rows}");
    }

    #[test]
    fn pools_table_survives_zero_size_and_overfull_pools() {
        let mut app = app_in_pools(vec![
            pool("empty", 0, 0, PoolHealth::Offline),
            pool("over", 150, 100, PoolHealth::Faulted),
        ]);
        let rows = render(&mut app, 200, 30).join("\n");
        assert!(rows.contains("○ OFFLINE"), "{rows}");
        assert!(rows.contains("✖ FAULTED"), "{rows}");
        assert!(rows.contains("100%!"), "{rows}");
    }

    #[test]
    fn unrecognized_pool_health_is_rendered_as_its_raw_text() {
        let mut app = app_in_pools(vec![pool("odd", 1, 100, PoolHealth::Other("SUSPENDED".to_string()))]);
        let rows = render(&mut app, 200, 30).join("\n");
        assert!(rows.contains("SUSPENDED"), "{rows}");
        assert!(!rows.contains("● SUSPENDED"), "{rows}");
    }

    #[test]
    fn health_is_visible_at_80_columns() {
        let mut app = app_in_pools(vec![pool("tank", 31, 100, PoolHealth::Online)]);
        let rows = render(&mut app, 80, 24).join("\n");
        assert!(rows.contains("ONLINE"), "Health must survive a narrow terminal:\n{rows}");
    }

    #[test]
    fn vdev_tree_shows_health_glyphs() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
        app.drilled_pool = Some("tank".to_string());
        app.vdevs = vec![
            VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Degraded },
            VdevLine { depth: 1, name: "da0".to_string(), health: PoolHealth::Faulted },
        ];
        let rows = render(&mut app, 100, 20).join("\n");
        assert!(rows.contains("▲ DEGRADED"), "{rows}");
        assert!(rows.contains("✖ FAULTED"), "{rows}");
    }

    #[test]
    fn hint_bar_is_the_last_row_and_shows_the_pinned_keys() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        let rows = render(&mut app, 80, 24);
        let last = &rows[23];
        assert!(last.contains("? Help"), "{last:?}");
        assert!(last.contains("q Quit"), "{last:?}");
        assert!(last.contains("s Scrub"), "{last:?}");
    }

    #[test]
    fn the_hint_bar_follows_focus() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        assert!(render(&mut app, 100, 24)[23].contains("s Scrub"));
        app.pane_focus = PaneFocus::Right;
        let last = render(&mut app, 100, 24)[23].clone();
        assert!(!last.contains("s Scrub"), "{last:?}");
        assert!(last.contains("← Back"), "{last:?}");
    }

    #[test]
    fn status_message_sits_directly_above_the_hint_bar() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.status_message = Some("hello status".to_string());
        let rows = render(&mut app, 80, 24);
        assert!(rows[22].contains("hello status"), "{:?}", rows[22]);
        assert!(rows[23].contains("Quit"), "{:?}", rows[23]);
    }

    #[test]
    fn with_no_message_or_scan_the_status_line_takes_no_row() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        let rows = render(&mut app, 80, 24);
        // The panes' bottom border is on row 22, directly above the hint bar.
        assert!(rows[22].contains('└'), "{:?}", rows[22]);
        assert!(rows[23].contains("Quit"));
    }

    #[test]
    fn a_very_long_status_message_is_capped_and_cannot_push_the_hint_bar_off_screen() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.status_message = Some((0..30).map(|i| format!("failure line {i}")).collect::<Vec<_>>().join("\n"));
        let rows = render(&mut app, 80, 24);
        assert!(rows[23].contains("Quit"), "{:?}", rows[23]);
        assert!(rows[20].contains("failure line 0"), "{:?}", rows[20]);
        assert!(rows[22].contains("failure line 2"), "{:?}", rows[22]);
        assert!(!rows.join("\n").contains("failure line 3"), "capped at STATUS_MAX_ROWS");
        assert!(rows[0].contains("zfsctl"), "the sidebar must still render");
    }

    #[test]
    fn the_sidebar_no_longer_has_its_own_help_footer() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        let rows = render(&mut app, 100, 24);
        // "? Help" now appears only once, in the bottom bar.
        let occurrences = rows.iter().filter(|r| r.contains("? Help")).count();
        assert_eq!(occurrences, 1, "{rows:#?}");
    }

    #[test]
    fn drawing_never_panics_on_a_tiny_terminal() {
        for (w, h) in [(20, 4), (30, 6), (10, 3), (1, 1), (80, 2)] {
            let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
            app.status_message = Some("a message that is far wider than this terminal".to_string());
            let _ = render(&mut app, w, h);
        }
    }

    #[test]
    fn tiny_terminals_still_show_the_pinned_hints() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        let rows = render(&mut app, 30, 8);
        assert!(rows[7].contains("? Help"), "{:?}", rows[7]);
        assert!(rows[7].contains("q Quit"), "{:?}", rows[7]);
    }

    #[test]
    fn failure_messages_use_the_err_style_and_ordinary_ones_do_not() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        let theme = app.theme.clone();
        let style_of_row22 = |app: &mut App| {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            terminal.backend().buffer()[(0, 22)].style()
        };
        app.set_error("zpool destroy failed".to_string());
        assert_eq!(style_of_row22(&mut app).fg, theme.err.fg);
        app.status_message = Some("select a disk with Space first".to_string());
        assert_eq!(style_of_row22(&mut app).fg, Some(Color::Reset)); // an unstyled buffer cell reports Reset, not None
    }

    use crate::ui::modal::{Modal, MutationCall, PendingAction};

    fn buffer_of(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn style_of_symbol(buf: &ratatui::buffer::Buffer, symbol: &str) -> Option<Style> {
        buf.content().iter().find(|c| c.symbol() == symbol).map(ratatui::buffer::Cell::style)
    }

    fn destroy_pool_modal() -> Modal {
        Modal::ConfirmTyped {
            prompt: "Type the pool name to destroy 'tank':".to_string(),
            expected: "tank".to_string(),
            input: "ta".to_string(),
            action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::DestroyPool { pool: "tank".to_string() } },
        }
    }

    #[test]
    fn a_destroy_modal_is_titled_with_a_glyph_and_the_action_and_drawn_in_red() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.modal = Some(destroy_pool_modal());
        let rows = render(&mut app, 100, 30).join("\n");
        assert!(rows.contains("✖ DESTROY POOL"), "{rows}");
        let buf = buffer_of(&mut app, 100, 30);
        assert_eq!(style_of_symbol(&buf, "✖").unwrap().fg, Some(Color::Red));
    }

    #[test]
    fn a_destroy_modal_still_shows_the_typed_input_line() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.modal = Some(destroy_pool_modal());
        let rows = render(&mut app, 100, 30).join("\n");
        assert!(rows.contains("> ta"), "{rows}");
    }

    #[test]
    fn a_warn_confirm_is_titled_with_a_triangle_and_drawn_in_yellow() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.modal = Some(Modal::Confirm { choice: None,
            prompt: "Offline disk 'da0' in pool 'tank'? (y/n)".to_string(),
            action: PendingAction { host: 0, section: Section::Vdevs, call: MutationCall::OfflineDisk { pool: "tank".to_string(), disk: "da0".to_string() } },
        });
        let rows = render(&mut app, 100, 30).join("\n");
        assert!(rows.contains("▲ OFFLINE DISK"), "{rows}");
        let buf = buffer_of(&mut app, 100, 30);
        assert_eq!(style_of_symbol(&buf, "▲").unwrap().fg, Some(Color::Yellow));
    }

    #[test]
    fn a_routine_confirm_keeps_its_plain_title() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.modal = Some(Modal::Confirm { choice: None,
            prompt: "Scrub pool 'tank'? (y/n)".to_string(),
            action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::ScrubPool { pool: "tank".to_string() } },
        });
        let rows = render(&mut app, 100, 30).join("\n");
        assert!(rows.contains("Confirm (y/n, then Enter)"), "{rows}");
        assert!(!rows.contains('✖'));
    }

    fn confirm_with(choice: Option<bool>, prompt: &str) -> App {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.modal = Some(Modal::Confirm { choice, prompt: prompt.to_string(), action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::ScrubPool { pool: "tank".to_string() } } });
        app
    }

    #[test]
    fn a_confirm_says_what_is_selected_and_what_enter_will_do() {
        let none = render(&mut confirm_with(None, "Scrub pool 'tank'? (y/n)"), 100, 30).join("\n");
        assert!(none.contains("Press y for yes or n for no, then Enter"), "{none}");
        let yes = render(&mut confirm_with(Some(true), "Scrub pool 'tank'? (y/n)"), 100, 30).join("\n");
        assert!(yes.contains("YES selected") && yes.contains("Enter to run"), "{yes}");
        let no = render(&mut confirm_with(Some(false), "Scrub pool 'tank'? (y/n)"), 100, 30).join("\n");
        assert!(no.contains("NO selected") && no.contains("Enter to cancel"), "{no}");
    }

    #[test]
    fn the_selection_line_is_not_clipped_under_a_long_prompt_on_a_small_terminal() {
        let long = "Remove SMB share 'media' for 'tank/media'? Connected clients lose access. They will have to reconnect and any open files may be lost. (y/n)";
        for choice in [None, Some(true), Some(false)] {
            let rows = render(&mut confirm_with(choice, long), 64, 20).join("\n");
            let want = match choice {
                None => "Press y",
                Some(true) => "YES selected",
                Some(false) => "NO selected",
            };
            assert!(rows.contains(want), "{choice:?} clipped:\n{rows}");
        }
    }

    #[test]
    fn the_danger_modal_is_distinguishable_without_color_in_the_mono_theme() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.theme = Theme::mono();
        app.modal = Some(destroy_pool_modal());
        let rows = render(&mut app, 100, 30).join("\n");
        assert!(rows.contains("✖ DESTROY POOL"), "{rows}");
    }

    #[test]
    fn capacity_is_visible_at_80_columns() {
        let mut app = app_in_pools(vec![pool("tank", 31, 100, PoolHealth::Online)]);
        let rows = render(&mut app, 80, 24).join("\n");
        assert!(rows.contains("ONLINE"), "{rows}");
        assert!(rows.contains("31%"), "capacity must survive 80 columns:\n{rows}");
    }

    #[test]
    fn pool_names_keep_a_readable_width_as_columns_are_added() {
        // 16-character name: the Name column must not be squeezed below
        // this just because a low-priority column (Size/Alloc/Free) fits.
        for width in [90u16, 110, 120, 140, 160, 200] {
            let mut app = app_in_pools(vec![pool("sixteen-char-nm!", 31, 100, PoolHealth::Online)]);
            let rows = render(&mut app, width, 24).join("\n");
            assert!(rows.contains("sixteen-char-nm!"), "name clipped at {width} columns:\n{rows}");
            assert!(rows.contains("ONLINE"), "health missing at {width} columns:\n{rows}");
        }
    }

    #[test]
    fn the_full_capacity_bar_is_used_when_there_is_room() {
        let mut app = app_in_pools(vec![pool("tank", 31, 100, PoolHealth::Online)]);
        let rows = render(&mut app, 200, 24).join("\n");
        assert!(rows.contains("███░░░░░ 31%") || rows.contains("██░░░░░░ 31%"), "{rows}");
    }

    #[test]
    fn a_long_failed_picker_message_keeps_esc_hint_and_the_error_tail_visible() {
        let mut app = app_in_pools(vec![]);
        let msg = format!("ssh: connect to host 172.16.10.90 port 22: {} Connection timed out ENDMARK", "y".repeat(80));
        app.picker = Some(crate::ui::picker::PickerState {
            values: vec![],
            filter: String::new(),
            selected: 0,
            checked: std::collections::HashSet::new(),
            multi_select: false,
            load: crate::ui::picker::PickerLoad::Failed(msg),
            note: None,
            account: None,
            pool_host: None,
        });
        let rows = render(&mut app, 80, 24).join("\n");
        assert!(rows.contains("Esc to close"), "{rows}");
        assert!(rows.contains("ENDMARK"), "{rows}");
    }

    #[test]
    fn failed_popup_height_reserves_a_word_wrap_margin() {
        let body = format!("Could not load accounts: {}\n\nEsc to close", "word ".repeat(26));
        let old = super::wrapped_line_count(&body, 38) + 2;
        let h = super::failed_popup_height(&body, 40, 24);
        assert!(h > old, "{h} vs old {old}");
        assert_eq!(super::failed_popup_height(&body, 40, 3), 3);
        assert_eq!(super::failed_popup_height(&body, 40, 0), 0);
    }

    #[test]
    fn a_realistic_multi_word_ssh_failure_keeps_esc_hint_and_tail_visible() {
        let mut app = app_in_pools(vec![]);
        let msg = "Could not load accounts: command 'getent passwd' failed with status 255: fb@172.16.10.90: Permission denied (publickey,password). Warning: Permanently added '172.16.10.90' (ED25519) to the list of known hosts. ENDMARK";
        app.picker = Some(crate::ui::picker::PickerState {
            values: vec![],
            filter: String::new(),
            selected: 0,
            checked: std::collections::HashSet::new(),
            multi_select: false,
            load: crate::ui::picker::PickerLoad::Failed(msg.to_string()),
            note: None,
            account: None,
            pool_host: None,
        });
        let rows = render(&mut app, 80, 24).join("\n");
        assert!(rows.contains("Esc to close"), "{rows}");
        assert!(rows.contains("ENDMARK"), "{rows}");
    }
    #[test]
    fn picker_title_mentions_space_only_for_multi_select() {
        assert_eq!(super::picker_title("", true), "Pick a value (type filter, Space toggle, Enter OK, Esc cancel)");
        assert_eq!(super::picker_title("", false), "Pick a value (type filter, Enter OK, Esc cancel)");
        assert_eq!(super::picker_title("wh", true), "Pick — filter 'wh' (Space toggle, Enter OK, Esc cancel)");
        assert_eq!(super::picker_title("wh", false), "Pick — filter 'wh' (Enter OK, Esc cancel)");
    }

    #[test]
    fn a_snapshot_job_shows_its_dataset_as_the_target_and_its_keep_prefix_and_recursion_in_the_panel() {
        let mut snap = crate::jobs::Job::test_snapshot("s", "tank/data", "hourly", 24);
        snap.recursive = true;
        let (mut app, _dir) = jobs_app(&[snap]);
        app.handle_key(crossterm::event::KeyCode::Down);
        let text = render(&mut app, 130, 34).join("\n");
        assert!(text.contains("Target"), "{text}");
        assert!(text.contains("tank/data"), "{text}");
        assert!(text.contains("keep 24") && text.contains("hourly") && text.contains("recursive"), "{text}");
    }

    #[test]
    fn the_snapshot_forms_preview_follows_its_schedule_field() {
        let (mut app, _dir) = jobs_app(&[]);
        app.handle_key(crossterm::event::KeyCode::Char('N'));
        let text = render(&mut app, 120, 30).join("\n");
        assert!(text.contains("New snapshot job") && text.contains("Hourly") && text.contains("Next:"), "the default 0 * * * * is the Hourly preset:\n{text}");
    }

    #[test]
    fn the_mirror_forms_check_result_reads_remote_not_repository() {
        let mut app = App::new(vec!["local".to_string()]);
        app.modal = Some(Modal::Form {
            title: "x".into(),
            fields: ["local", "tank/data", "", "hourly", "", "aws:bkt", "", "30 3 * * *", "y"].iter().map(|v| crate::ui::modal::FormField { label: "x".into(), value: (*v).into() }).collect(),
            focused: 5,
            description: None,
            kind: crate::ui::modal::FormKind::MirrorJob { hosts: vec!["local".into()], existing: None, jobs: vec![] },
        });
        app.backup_preflight = Some(crate::ui::state::PreflightCheck { host: 0, repository: "aws:bkt".into(), password_file: String::new(), result: Some(crate::restic::Preflight::Ok), file_note: Some("chmod it".into()) });
        let text = render(&mut app, 120, 40).join("\n");
        assert!(text.contains("Check: OK, the remote opens.") && text.contains("Note: chmod it"), "{text}");
        app.backup_preflight = Some(crate::ui::state::PreflightCheck { host: 0, repository: "aws:other".into(), password_file: String::new(), result: Some(crate::restic::Preflight::Ok), file_note: None });
        assert!(!render(&mut app, 120, 40).join("\n").contains("Check:"), "an edited remote hides the old answer");
    }

    #[test]
    fn a_mirror_job_shows_its_remote_as_the_target_and_the_limit_only_when_set() {
        let mut m = crate::jobs::Job::test_mirror("m", "tank/data", "hourly", "aws:bkt/photos");
        m.bwlimit = Some("5M".into());
        let (mut app, _dir) = jobs_app(&[m]);
        app.handle_key(crossterm::event::KeyCode::Down);
        let text = render(&mut app, 130, 34).join("\n");
        assert!(text.contains("tank/data") && text.contains("aws:bkt/photos") && text.contains("limit 5M/s"), "{text}");
        let (mut app, _dir) = jobs_app(&[crate::jobs::Job::test_mirror("m", "tank/data", "hourly", "aws:bkt/photos")]);
        app.handle_key(crossterm::event::KeyCode::Down);
        assert!(!render(&mut app, 130, 34).join("\n").contains("limit "));
    }

    #[test]
    fn a_replicate_job_with_dest_keep_says_how_many_it_keeps_on_the_target() {
        let mut repl = crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "local", "backup/data");
        repl.dest_keep = Some(7);
        let (mut app, _dir) = jobs_app(&[repl]);
        app.handle_key(crossterm::event::KeyCode::Down);
        let text = render(&mut app, 130, 34).join("\n");
        assert!(text.contains("keep 7 on target"), "{text}");
        let plain = crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "local", "backup/data");
        let (mut app, _dir) = jobs_app(&[plain]);
        app.handle_key(crossterm::event::KeyCode::Down);
        assert!(!render(&mut app, 130, 34).join("\n").contains("on target"));
    }

    #[test]
    fn a_replicate_job_shows_its_source_as_the_target_and_destination_prefix_and_progress_in_the_panel() {
        let repl = crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "local", "backup/data");
        let (mut app, dir) = jobs_app(&[repl]);
        let p = crate::daemon::progress::Progress { job_id: "r".into(), bytes: 5, total: Some(10), started_at: chrono::Utc::now(), updated_at: chrono::Utc::now(), rate: None };
        crate::daemon::progress::write(&dir.path().join("progress"), "run1", &p).unwrap();
        app.refresh_jobs();
        app.handle_key(crossterm::event::KeyCode::Down);
        let text = render(&mut app, 130, 34).join("\n");
        for want in ["tank/data", "to local:backup/data", "prefix hourly", "sent 5 B of 10 B (50%)"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn the_replication_forms_preview_follows_its_schedule_field() {
        let (mut app, _dir) = jobs_app(&[]);
        app.handle_key(crossterm::event::KeyCode::Char('R'));
        let text = render(&mut app, 120, 34).join("\n");
        assert!(text.contains("New replication job") && text.contains("Next:"), "{text}");
    }

    #[test]
    fn the_boot_envs_table_decodes_activity_and_drops_columns_when_narrow() {
        use crate::zfs::bootenv::{ActiveFlags, BootEnv};
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
        app.bootenvs = vec![
            BootEnv { name: "default".into(), flags: ActiveFlags { now: true, reboot: true, temp: false }, mountpoint: Some("/".into()), space: "5.18G".into(), created: "2026-07-24 18:48".into() },
            BootEnv { name: "spare".into(), flags: ActiveFlags { now: false, reboot: false, temp: true }, mountpoint: None, space: "8K".into(), created: "2026-10-07 19:26".into() },
        ];
        let wide = render(&mut app, 120, 24).join("\n");
        for want in ["Boot Environments", "Name", "Active", "Space", "Mountpoint", "Created", "default", "Now+Reboot", "Temp", "5.18G", "2026-07-24 18:48"] {
            assert!(wide.contains(want), "missing {want:?} in:\n{wide}");
        }
        let narrow = render(&mut app, 80, 24).join("\n");
        for want in ["Name", "Active", "Space", "default"] {
            assert!(narrow.contains(want), "missing {want:?} in:\n{narrow}");
        }
        assert!(!narrow.contains("Mountpoint") && !narrow.contains("Created"), "optional columns must drop:\n{narrow}");
    }

    #[test]
    fn an_unavailable_boot_envs_section_says_why_in_its_title() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
        app.bootenvs_unavailable = Some("command 'bectl list' failed with status 127: bectl: not found".to_string());
        let text = render(&mut app, 140, 24).join("\n");
        assert!(text.contains("[unavailable: command 'bectl list' failed"), "{text}");
    }

    #[test]
    fn a_long_boot_environment_name_is_shown_in_full_even_on_a_narrow_terminal() {
        use crate::zfs::bootenv::{ActiveFlags, BootEnv};
        let long = "15.1-RELEASE-p3_2026-10-07_184800";
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
        app.bootenvs = vec![BootEnv { name: long.into(), flags: ActiveFlags::default(), mountpoint: None, space: "8K".into(), created: "2026-10-07 18:48".into() }];
        for width in [80, 120] {
            let text = render(&mut app, width, 24).join("\n");
            assert!(text.contains(long), "{width} cols: name cut off in:\n{text}");
        }
    }

    #[test]
    fn a_loaded_empty_list_says_there_are_no_boot_environments_but_an_unloaded_one_does_not() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
        assert!(!render(&mut app, 120, 24).join("\n").contains("no boot environments"));
        app.bootenvs_loaded = true;
        assert!(render(&mut app, 120, 24).join("\n").contains("no boot environments"));
    }

    #[test]
    fn a_backup_job_shows_its_repository_retention_and_progress_in_the_details() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Jobs).unwrap();
        let mut job = crate::jobs::Job::test_backup("backup-x", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
        job.retention = Some("7d 4w".to_string());
        app.jobs_view.jobs = vec![job];
        app.jobs_table.select(Some(0));
        let text = render(&mut app, 140, 30).join("\n");
        for want in ["tank/data", "to /mnt/usb/restic", "keep 7d 4w", "prefix hourly"] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
        assert!(!text.contains("password"), "the password file path is not shown");
    }


    #[test]
    fn a_mounted_boot_environments_path_is_in_the_title_even_when_the_column_is_hidden() {
        use crate::zfs::bootenv::{ActiveFlags, BootEnv};
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
        app.bootenvs = vec![BootEnv { name: "zit-a".into(), flags: ActiveFlags::default(), mountpoint: Some("/tmp/be_mount.Xy12".into()), space: "8K".into(), created: "2026-10-08 18:48".into() }];
        app.bootenvs_table.select(Some(0));
        let narrow = render(&mut app, 80, 24).join("\n");
        assert!(!narrow.contains("Mountpoint"), "the column is hidden at this width:\n{narrow}");
        assert!(narrow.contains("'zit-a' mounted at /tmp/be_mount.Xy12"), "{narrow}");
        app.bootenvs[0].mountpoint = None;
        assert!(!render(&mut app, 80, 24).join("\n").contains("mounted at"));
        app.bootenvs[0].mountpoint = Some("/".into());
        assert!(!render(&mut app, 80, 24).join("\n").contains("mounted at"), "the running BE at / is not news");
    }


    #[test]
    fn the_jobs_table_shows_where_each_job_sends_its_data_and_clamps_on_a_narrow_terminal() {
        let repl = crate::jobs::Job::test_replicate("repl", "tank/data", "hourly", "local", "backup/data");
        let backup = crate::jobs::Job::test_backup("bak", "tank/media", "hourly", "/mnt/usb/restic", "/root/pw");
        let (mut app, _dir) = jobs_app(&[repl, backup]);
        app.refresh_jobs();
        let wide = render(&mut app, 170, 30).join("\n");
        for want in ["tank/data → local:backup/data", "tank/media → /mnt/usb/restic"] {
            assert!(wide.contains(want), "missing {want:?} in\n{wide}");
        }
        let narrow = render(&mut app, 100, 30).join("\n");
        assert!(narrow.contains("tank/data") && narrow.contains("Schedule") && narrow.contains("Last"), "other columns survive:\n{narrow}");
    }


    #[test]
    fn the_acl_pane_title_says_when_the_dataset_is_posix() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Datasets).unwrap();
        app.datasets = vec![crate::zfs::dataset::Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
        app.datasets_table.select(Some(0));
        app.pane_focus = crate::ui::state::PaneFocus::Right;
        app.dataset_detail_tab = crate::ui::state::DatasetDetailTab::Acl;
        app.dataset_properties_for = Some("tank/data".to_string());
        app.dataset_properties = vec![crate::zfs::property::Property { name: "acltype".to_string(), value: "posix".to_string(), source: crate::zfs::property::PropertySource::Local }];
        let text = render(&mut app, 160, 30).join("\n");
        assert!(text.contains("acltype=posix") && text.contains("NFSv4"), "{text}");
    }

    fn hosts_app() -> App {
        let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string(), "fbsd16t1".to_string(), "deadbox".to_string()]);
        app.set_host_addresses(vec!["local".into(), "172.16.10.60".into(), "172.16.10.61".into(), "172.16.10.99".into()]);
        let probe = |p: crate::zfs::host_health::HostProbe| crate::ui::state::HostProbeState { last: Some(p), in_flight: false, finished_at: Some(std::time::Instant::now()), started_at: None, os: None, os_name: None, kind: None, resources: vec![], utc_offset_secs: None };
        let pool = |n: &str, h: crate::zfs::pool::PoolHealth, c: u8| crate::zfs::host_health::PoolProbe { name: n.to_string(), health: h, capacity_percent: c, frag_percent: None, errors: Some(0), scan: None, last_scrub: None };
        use crate::zfs::host_health::HostProbe;
        app.host_probes.insert("local".into(), probe(HostProbe::Reached { rtt_ms: 3, pools: vec![pool("zroot", crate::zfs::pool::PoolHealth::Online, 20)], kind: None, os: None, os_name: None }));
        app.host_probes.insert("fbsd16".into(), probe(HostProbe::Reached { rtt_ms: 12, pools: vec![pool("tank", crate::zfs::pool::PoolHealth::Degraded, 20)], kind: None, os: None, os_name: None }));
        app.host_probes.insert("fbsd16t1".into(), probe(HostProbe::Reached { rtt_ms: 7, pools: vec![pool("tank", crate::zfs::pool::PoolHealth::Online, 85)], kind: None, os: None, os_name: None }));
        app.host_probes.insert("deadbox".into(), probe(HostProbe::Disconnected));
        use crate::zfs::host_health::HostKind;
        app.host_probes.get_mut("local").unwrap().kind = Some(HostKind::Metal);
        app.host_probes.get_mut("fbsd16").unwrap().kind = Some(HostKind::Vm("bhyve".into()));
        app.host_probes.get_mut("fbsd16t1").unwrap().kind = Some(HostKind::Jail);
        app.set_host_tags(vec![String::new(), "backup".into(), String::new(), String::new()]);
        app
    }

    /// The foreground color of the first cell of `needle` on the first row that contains it.
    fn fg_of(app: &mut App, needle: &str, w: u16, h: u16) -> Option<ratatui::style::Color> {
        let rows = render(app, w, h);
        let buf = buffer_of(app, w, h);
        for (y, row) in rows.iter().enumerate() {
            if let Some(x) = row.find(needle) {
                let col = row[..x].chars().count();
                return buf[(u16::try_from(col).ok()?, u16::try_from(y).ok()?)].fg.into();
            }
        }
        None
    }

    #[test]
    fn the_about_screen_draws_the_wordmark_in_the_warn_colour_and_leaves_it_out_when_it_cannot_fit() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(crossterm::event::KeyCode::Char('A'));
        let rows = render(&mut app, 120, 40);
        let logo = crate::ui::logo::wordmark();
        assert!(logo.iter().all(|line| rows.iter().any(|r| r.contains(line.as_str()))), "{rows:?}");
        assert_eq!(fg_of(&mut app, "▀▀▀▀▀", 120, 40), Some(ratatui::style::Color::Yellow), "the theme's warn colour");
        assert!(rows.iter().any(|r| r.contains("zfsctl v")), "the version stays");
        // a pop-up too narrow for the drawing shows the text without it (a wrapped wordmark would be garbage)
        let narrow = render(&mut app, 60, 40);
        assert!(logo.iter().all(|line| narrow.iter().all(|r| !r.contains(line.as_str()))), "the big wordmark is left out: {narrow:?}");
        assert!(narrow.iter().any(|r| r.contains("zfsctl v")), "{narrow:?}");
    }

    #[test]
    fn the_about_wordmark_and_version_are_centred_in_the_pop_up() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(crossterm::event::KeyCode::Char('A'));
        let rows: Vec<Vec<char>> = render(&mut app, 120, 40).iter().map(|r| r.chars().collect()).collect();
        let find = |needle: &str| -> (usize, usize) {
            let n: Vec<char> = needle.chars().collect();
            rows.iter().enumerate().find_map(|(y, r)| r.windows(n.len()).position(|w| w == n.as_slice()).map(|x| (y, x))).unwrap_or_else(|| panic!("{needle} not on screen"))
        };
        let (top, left) = find("┌About");
        let right = left + rows[top][left..].iter().position(|c| *c == '┐').expect("the pop-up's corner");
        let gaps = |x: usize, width: usize| ((x - (left + 1)) as i64, ((right - 1) - (x + width - 1)) as i64);
        let last_line = crate::ui::logo::wordmark().pop().unwrap();
        let (_, x) = find(&last_line);
        let (l, r) = gaps(x, crate::ui::logo::WIDTH);
        assert!((l - r).abs() <= 1 && l > 0, "the wordmark has {l} columns on its left and {r} on its right");
        let version = format!("zfsctl v{}", env!("CARGO_PKG_VERSION"));
        let (_, x) = find(&version);
        let (l, r) = gaps(x, version.chars().count());
        assert!((l - r).abs() <= 1 && l > 0, "the version line has {l} on its left and {r} on its right");
        let (_, x) = find("A terminal UI for managing ZFS");
        assert_eq!(x, left + 1, "the description stays flush left");
    }

    #[test]
    fn the_host_columns_drop_in_the_documented_order_and_the_required_three_stay() {
        // Array order is the drop order: the OS name first, Type last.
        let order = |c: &HostColumns| [c.os, c.rtt, c.jobs, c.scrub, c.pools, c.tag, c.used, c.type_];
        let wide = fit_host_columns(200, 10, 8);
        assert!(order(&wide).iter().all(|b| *b) && wide.addr, "everything fits at 200");
        for w in 30..200 {
            let on = order(&fit_host_columns(w, 10, 8));
            // a column that survives implies every column that drops after it survives too
            assert!(on.windows(2).all(|p| !(p[0] && !p[1])), "width {w}: {on:?}");
            // widening never removes a column (except when Address appears, which takes priority over them)
            let (now, next) = (fit_host_columns(w, 10, 8), fit_host_columns(w + 1, 10, 8));
            if now.addr == next.addr {
                assert!(on.iter().zip(order(&next)).all(|(a, b)| !*a || b), "width {w}");
            }
        }
        assert!(!order(&fit_host_columns(30, 10, 8)).iter().any(|b| *b), "at 30 columns only the required ones remain");
        assert!(!order(&fit_host_columns(200, 10, 0))[5], "no tags configured: no Tag column");
    }

    #[test]
    fn the_hosts_table_shows_the_short_os_name_when_it_fits() {
        let mut app = linux_host_app();
        app.host_probes.get_mut("ub24").unwrap().os_name = Some("Linux 6.8 (Ubuntu 24)".into());
        app.selected_section = app.sections.iter().position(|s| *s == Section::Hosts).unwrap();
        let wide = render(&mut app, 200, 14).join("\n");
        assert!(wide.contains("OS") && wide.contains("Linux 6.8 (Ubuntu 24)"), "{wide}");
        let narrow = render(&mut app, 80, 14).join("\n");
        assert!(!narrow.contains("Ubuntu 24"), "the OS name is the first column to go:\n{narrow}");
    }

    #[test]
    fn a_one_or_two_character_tag_is_fitted_as_wide_as_it_is_drawn() {
        // The drawn Tag column is at least 3 wide (its header), so the fitting must count it that way.
        for inner in 0..200 {
            assert_eq!(fit_host_columns(inner, 8, 1), fit_host_columns(inner, 8, 3), "inner {inner}");
            assert_eq!(fit_host_columns(inner, 8, 2), fit_host_columns(inner, 8, 3), "inner {inner}");
        }
    }

    #[test]
    fn the_hosts_table_shows_the_new_columns_and_cells() {
        let mut app = hosts_app();
        app.selected_host = 0;
        let text = render(&mut app, 170, 30).join("\n");
        for want in ["Type", "Tag", "Used", "Pools", "Scrub", "Jobs", "Rtt", "vm", "metal", "jail", "backup", "85%", "20%"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn the_type_words_have_their_own_colors_apart_from_the_health_colors() {
        let mut app = hosts_app();
        let vm = fg_of(&mut app, "vm", 170, 30);
        let jail = fg_of(&mut app, "jail", 170, 30);
        let ok = fg_of(&mut app, "ok", 170, 30);
        assert!(vm.is_some() && vm != ok && jail != ok && vm != jail, "{vm:?} {jail:?} {ok:?}");
    }

    #[test]
    fn a_narrow_hosts_table_still_shows_name_address_and_state() {
        let mut app = hosts_app();
        let text = render(&mut app, 100, 30).join("\n");
        for want in ["Name", "Address", "State", "172.16.10.60"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    fn alerts_render_app(config: &str) -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), config).unwrap();
        let mut app = App::new(vec!["local".to_string()]);
        app.config_path = dir.path().join("config.toml");
        app.state_dir = dir.path().to_path_buf();
        app.selected_section = app.sections.iter().position(|s| *s == Section::Alerts).unwrap();
        app.refresh_alerts();
        (app, dir)
    }

    #[test]
    fn the_alerts_screen_shows_settings_status_and_events() {
        let (mut app, dir) = alerts_render_app("[daemon]\nnotify_command = \"curl -s x\"\n\n[notify]\nmail_to = \"me@x.org\"\n");
        std::fs::write(dir.path().join("notify.json"), "{\"last_report\":\"2026-10-10\"}").unwrap();
        crate::daemon::health_text::EventLog::new(dir.path().join("health-events.jsonl")).append(&crate::daemon::health_text::HealthEvent { at: chrono::Utc::now(), host: "fbsd16".into(), kind: "alert".into(), text: "tank DEGRADED".into() }).unwrap();
        app.refresh_alerts();
        let text = render(&mut app, 140, 40).join("\n");
        for want in ["Alerts", "Email to", "me@x.org", "Mail command", "msmtp -t", "(default)", "Hook command", "curl -s x", "edit in config.toml", "Monitor on", "Last report sent 2026-10-10", "fbsd16", "alert", "tank DEGRADED"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    fn settings_render_app(config: &str) -> (App, tempfile::TempDir) {
        let (mut app, dir) = alerts_render_app(config);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Settings).unwrap();
        app.refresh_settings();
        (app, dir)
    }

    /// The foreground colour and boldness of the first cell of `word` on the first row that contains it.
    fn style_of(buf: &ratatui::buffer::Buffer, w: u16, h: u16, word: &str) -> (ratatui::style::Color, bool) {
        for y in 0..h {
            let row: Vec<String> = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
            let text = row.concat();
            if let Some(at) = text.find(word) {
                let x = u16::try_from(text[..at].chars().count()).unwrap();
                let cell = &buf[(x, y)];
                return (cell.fg, cell.modifier.contains(ratatui::style::Modifier::BOLD));
            }
        }
        panic!("{word:?} not on screen");
    }

    fn render_buffer(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn settings_group_titles_use_the_header_style_and_their_rows_do_not() {
        let (mut app, _dir) = settings_render_app("");
        let buf = render_buffer(&mut app, 120, 30);
        let header = app.theme.header();
        assert_eq!(style_of(&buf, 120, 30, "Dashboard thresholds"), (header.fg.unwrap(), true), "group title");
        let (row_fg, row_bold) = style_of(&buf, 120, 30, "Memory warn");
        assert!(row_fg != header.fg.unwrap() && !row_bold, "a row stays body-coloured");
    }

    #[test]
    fn the_help_screens_section_titles_use_the_header_style_and_its_keys_do_not() {
        let mut app = App::new(vec!["local".to_string()]);
        app.modal = Some(Modal::Info { title: "Help".to_string(), body: crate::ui::state::help_text_for_book() });
        let buf = render_buffer(&mut app, 140, 50);
        assert_eq!(style_of(&buf, 140, 50, "Global"), (app.theme.header().fg.unwrap(), true), "section title");
        let (fg, bold) = style_of(&buf, 140, 50, "switch sections");
        assert!(fg != app.theme.header().fg.unwrap() && !bold, "a key line stays body-coloured");
    }

    #[test]
    fn a_choose_list_scrolls_to_keep_the_chosen_row_on_screen_and_marks_it() {
        let mut app = App::new(vec!["local".to_string()]);
        let rows: Vec<String> = (0..40).map(|i| format!("dataset-{i:02} lacks something")).collect();
        app.modal = Some(Modal::Choose { title: "Unprotected data".into(), note: "40 datasets\nsecond line".into(), rows, selected: 35, then: crate::ui::modal::ChooseThen::Copy { host: "h".into(), dataset: "d".into() }, look: crate::ui::modal::ChooseLook::default() });
        let buf = render_buffer(&mut app, 100, 20);
        let text: String = (0..20).map(|y| (0..100).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect();
        assert!(text.contains("dataset-35") && !text.contains("dataset-00"), "the chosen row is shown, the top is scrolled off:\n{text}");
        let find = |word: &str| {
            (0..20u16).find_map(|y| {
                let row: String = (0..100u16).map(|x| buf[(x, y)].symbol().to_string()).collect();
                row.find(word).map(|at| (u16::try_from(row[..at].chars().count()).unwrap(), y))
            })
            .unwrap()
        };
        let (cx, cy) = find("dataset-35");
        let (ox, oy) = find("dataset-34");
        assert!(buf[(cx, cy)].style() != buf[(ox, oy)].style(), "the chosen row is drawn differently from its neighbour");
    }

    #[test]
    fn a_choose_list_draws_host_headings_in_the_header_style_and_rows_in_their_level_colour() {
        use crate::ui::dashboard::Level;
        let mut app = App::new(vec!["local".to_string()]);
        let look = crate::ui::modal::ChooseLook { heads: vec![(0, "alphahost  (2 datasets)".into()), (2, "betahost  (1 dataset)".into())], levels: vec![Level::Issue, Level::Watch, Level::Issue] };
        let rows = vec!["issue  tank/one  no snapshots".to_string(), "watch  zroot/var  no copy".to_string(), "issue  tank/two  no snapshots".to_string()];
        app.modal = Some(Modal::Choose { title: "Unprotected data".into(), note: "3 datasets".into(), rows, selected: 2, then: crate::ui::modal::ChooseThen::Copy { host: "h".into(), dataset: "d".into() }, look });
        let buf = render_buffer(&mut app, 100, 24);
        let header = app.theme.header();
        assert_eq!(style_of(&buf, 100, 24, "alphahost"), (header.fg.unwrap(), true));
        assert_eq!(style_of(&buf, 100, 24, "betahost"), (header.fg.unwrap(), true));
        assert_eq!(style_of(&buf, 100, 24, "tank/one").0, app.theme.err.fg.unwrap(), "a red row");
        assert_eq!(style_of(&buf, 100, 24, "zroot/var").0, app.theme.warn.fg.unwrap(), "an amber row");
        let lines: Vec<String> = (0..24u16).map(|y| (0..100u16).map(|x| buf[(x, y)].symbol().to_string()).collect()).collect();
        let at = |w: &str| lines.iter().position(|l| l.contains(w)).unwrap();
        assert!(at("betahost") > at("zroot/var") + 1, "a blank line before the next host: {lines:#?}");
    }

    #[test]
    fn an_info_pop_up_other_than_help_keeps_its_plain_lines() {
        let mut app = App::new(vec!["local".to_string()]);
        app.modal = Some(Modal::Info { title: "Report preview (nothing sent)".to_string(), body: "NEEDS ATTENTION\n  nothing".to_string() });
        let buf = render_buffer(&mut app, 100, 30);
        let (fg, bold) = style_of(&buf, 100, 30, "NEEDS ATTENTION");
        assert!(fg != app.theme.header().fg.unwrap() && !bold, "only Help styles its non-indented lines");
    }

    #[test]
    fn a_table_screens_column_titles_use_the_header_style() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
        let buf = render_buffer(&mut app, 120, 30);
        assert_eq!(style_of(&buf, 120, 30, "Written"), (app.theme.header().fg.unwrap(), true));
    }

    #[test]
    fn the_settings_screen_shows_groups_rows_and_defaults() {
        let (mut app, _dir) = settings_render_app("[dashboard]\nmemory_warn = 80\n[cloud.caps]\nb2 = \"10G\"\n");
        let text = render(&mut app, 120, 30).join("\n");
        for want in ["Settings", "Display", "Theme", "Dashboard thresholds", "Memory warn / crit", "80 / 97 %", "CPU warn", "95 %", "(default)", "Cloud caps", "b2", "10G"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn the_settings_screen_without_remotes_says_how_to_list_them() {
        let (mut app, _dir) = settings_render_app("");
        assert!(render(&mut app, 120, 30).join("\n").contains("open Cloud once to list your remotes"));
    }

    #[test]
    fn the_settings_screen_shows_the_problem_when_the_file_is_unreadable() {
        let (mut app, _dir) = settings_render_app("[dashboard\n");
        let text = render(&mut app, 120, 30).join("\n");
        assert!(text.contains("config.toml") && text.contains("by hand"), "{text}");
        assert!(!text.contains("Memory warn"), "{text}");
    }

    #[test]
    fn a_wrong_protection_value_is_shown_in_red_under_its_group() {
        let (mut app, _dir) = settings_render_app("[protection]\nemail = \"of\"\n");
        let text = render(&mut app, 140, 40).join("\n");
        assert!(text.contains("[protection]: email must be immediate, report or off; judging at Basic"), "{text}");
        assert!(text.contains("Memory warn"), "the rest is still shown: {text}");
        let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s} in {text}"));
        assert!(at("Data protection") < at("judging at Basic") && at("judging at Basic") < at("Level"), "{text}");
    }

    #[test]
    fn before_the_first_load_the_settings_screen_says_so() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Settings).unwrap();
        assert!(render(&mut app, 100, 30).join("\n").contains("Loading"));
    }

    #[test]
    fn a_selection_below_the_visible_rows_scrolls_into_view() {
        let (mut app, _dir) = settings_render_app("");
        app.settings_table.select(Some(app.settings_view.rows.len() - 1));
        let text = render(&mut app, 100, 12).join("\n");
        assert!(text.contains("Scrub ignore"), "{text}");
    }

    #[test]
    fn before_the_first_load_the_alerts_screen_says_so_instead_of_showing_empty_settings() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Alerts).unwrap();
        assert!(render(&mut app, 100, 30).join("\n").contains("Loading"));
    }

    #[test]
    fn nothing_configured_says_the_monitor_is_off_and_how_to_turn_it_on() {
        let (mut app, _dir) = alerts_render_app("");
        let text = render(&mut app, 140, 40).join("\n");
        assert!(text.contains("Monitor off") && text.contains("(not set)") && text.contains("Last report: never") && text.contains("(none yet)"), "{text}");
    }

    #[test]
    fn an_unreadable_config_is_reported_on_the_screen() {
        let (mut app, _dir) = alerts_render_app("[notify\nbroken");
        let text = render(&mut app, 140, 40).join("\n");
        assert!(text.contains("could not be read"), "{text}");
    }

    #[test]
    fn a_narrow_terminal_cuts_long_values_with_an_ellipsis_and_never_panics() {
        let (mut app, _dir) = alerts_render_app("[notify]\nmail_to = \"a@x.org\"\nmail_command = \"msmtp -a default -t --some --very --long --set --of --options\"\n");
        for (w, h) in [(60, 20), (80, 24), (30, 10)] {
            let text = render(&mut app, w, h).join("\n");
            assert!(!text.is_empty());
        }
        assert!(render(&mut app, 80, 24).join("\n").contains('…'));
    }

    #[test]
    fn the_hosts_title_shows_the_filter_and_only_matching_rows_are_drawn() {
        let mut app = hosts_app();
        app.host_filter = "dead".into();
        app.selected_host = 3;
        let text = render(&mut app, 120, 30).join("\n");
        assert!(text.contains("deadbox") && !text.contains("fbsd16t1"), "{text}");
        assert!(text.contains("filter: dead") && text.contains("1/4"), "{text}");
        app.host_filter_editing = true;
        assert!(render(&mut app, 120, 30).join("\n").contains("filter: dead_"), "a cursor while typing");
        app.host_filter = "zzz".into();
        assert!(render(&mut app, 120, 30).join("\n").contains("no host matches"));
    }

    #[test]
    fn the_hosts_table_scrolls_to_keep_the_selected_host_visible() {
        let names: Vec<String> = (0..40).map(|i| format!("host{i:02}")).collect();
        let mut app = App::new(names);
        app.selected_host = 39;
        let text = render(&mut app, 120, 24).join("\n");
        assert!(text.contains("host39"), "the selected host must be on screen:\n{text}");
        assert!(!text.contains("host01"), "the top rows scrolled away:\n{text}");
        app.selected_host = 0;
        assert!(render(&mut app, 120, 24).join("\n").contains("host00"));
    }

    #[test]
    fn the_hosts_table_shows_name_address_state_and_rtt() {
        let mut app = hosts_app();
        app.selected_host = 0;
        let text = render(&mut app, 150, 30).join("\n");
        for want in ["Name", "Address", "State", "Rtt", "fbsd16", "172.16.10.60", "tank DEGRADED", "tank 85% full", "unreachable", "12ms", "ok"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        assert!(text.contains("local"), "the hub's address column says local");
    }

    #[test]
    fn health_colors_the_marker_and_name_and_the_unreachable_host_is_red() {
        let mut app = hosts_app();
        app.selected_host = 0;
        let theme = app.theme.clone();
        assert_eq!(fg_of(&mut app, "fbsd16 ", 120, 30), theme.err.fg, "a degraded pool is red");
        assert_eq!(fg_of(&mut app, "fbsd16t1", 120, 30), theme.warn.fg, "85% full is yellow");
        assert_eq!(fg_of(&mut app, "deadbox", 120, 30), theme.err.fg, "unreachable is red");
    }

    #[test]
    fn a_host_that_has_not_been_probed_is_dim_and_says_probing() {
        let mut app = App::new(vec!["local".to_string(), "new".to_string()]);
        let text = render(&mut app, 120, 30).join("\n");
        assert!(text.contains("probing"), "{text}");
    }

    #[test]
    fn narrow_terminals_drop_rtt_then_address_but_keep_name_and_state() {
        let mut app = hosts_app();
        let wide = render(&mut app, 140, 30).join("\n");
        assert!(wide.contains("Rtt") && wide.contains("Address"));
        let mid = render(&mut app, 90, 30).join("\n");
        assert!(!mid.contains("Rtt") && mid.contains("Address") && mid.contains("State"), "{mid}");
        let narrow = render(&mut app, 50, 30).join("\n");
        assert!(!narrow.contains("Address") && narrow.contains("Name") && narrow.contains("State") && narrow.contains("unreachable"), "{narrow}");
    }

    #[test]
    fn the_single_host_hint_and_the_selection_are_kept() {
        let mut app = App::new(vec!["local".to_string()]);
        let text = render(&mut app, 120, 30).join("\n");
        assert!(text.contains("Only this host is configured"), "{text}");
        let mut many = hosts_app();
        many.selected_host = 1;
        let buf = buffer_of(&mut many, 120, 30);
        let rows = render(&mut many, 120, 30);
        let y = rows.iter().position(|r| r.contains("172.16.10.60")).unwrap();
        let x = rows[y].find("172.16.10.60").unwrap();
        let selected = buf[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())].modifier;
        let other = {
            let y2 = rows.iter().position(|r| r.contains("172.16.10.61")).unwrap();
            buf[(u16::try_from(x).unwrap(), u16::try_from(y2).unwrap())].modifier
        };
        assert_ne!(selected, other, "the selected host's row looks selected");
    }

    fn host_panel_app(w_extra: ()) -> App {
        let _ = w_extra;
        let mut app = App::new(vec!["local".to_string()]);
        let target = MetricsTarget::Host;
        app.metrics.retarget(Some(target.clone()));
        app.metrics.apply(&target, Ok(crate::zfs::metrics::TargetSample { pool: crate::zfs::metrics::PoolSample::default(), space: None, host: Some(crate::ui::metrics::tests_support::rich_host()), snapshot: None, disk: None, share: None, contents: None }), std::time::Instant::now());
        app
    }

    #[test]
    fn the_host_panel_shows_two_columns_when_wide_and_one_when_narrow_and_says_when_it_cuts_pool_rows() {
        let mut wide = host_panel_app(());
        let text = render(&mut wide, 190, 40).join("\n");
        for want in ["Metrics — host local", "CPU", "36 cores", "Memory", "Swap", "L2ARC", "scratch", "tank", "zroot"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        let mut narrow = host_panel_app(());
        let text = render(&mut narrow, 90, 40).join("\n");
        assert!(text.contains("CPU") && text.contains("Swap"), "one column still shows everything:\n{text}");
        let mut short = host_panel_app(());
        let text = render(&mut short, 190, 24).join("\n");
        assert!(text.contains("more pool row") || text.contains("scratch"), "never silently clipped:\n{text}");
    }

    #[test]
    fn a_long_cpu_model_is_cut_before_it_reaches_the_zfs_column() {
        let mut app = host_panel_app(());
        let target = MetricsTarget::Host;
        let mut host = crate::ui::metrics::tests_support::rich_host();
        host.cpu.as_mut().unwrap().model = "Intel Xeon E312xx (Sandy Bridge, IBRS update) with a long tail".to_string();
        app.metrics.apply(&target, Ok(crate::zfs::metrics::TargetSample { pool: crate::zfs::metrics::PoolSample::default(), space: None, host: Some(host), snapshot: None, disk: None, share: None, contents: None }), std::time::Instant::now());
        let rows = render(&mut app, 170, 48);
        let model_row = rows.iter().find(|r| r.contains("Intel Xeon E312xx")).expect("the model line is shown");
        assert!(model_row.contains('…'), "{model_row}");
        let status_row = rows.iter().find(|r| r.contains("Status")).unwrap();
        let at = status_row.find("Status").unwrap();
        assert!(status_row[..at].ends_with("  "), "the ZFS column keeps a gutter: {status_row}");
    }

    #[test]
    fn a_short_host_panel_keeps_the_pools_and_the_unhealthy_rows_and_says_how_much_it_hides() {
        for (w, h) in [(80, 24), (100, 30)] {
            let mut app = host_panel_app(());
            let text = render(&mut app, w, h).join("\n");
            assert!(text.contains("DEGRADED"), "{w}x{h}: the unhealthy pool row must survive:\n{text}");
            assert!(text.contains("Status"), "{w}x{h}: the status line must survive:\n{text}");
        }
        let mut tiny = host_panel_app(());
        let text = render(&mut tiny, 80, 24).join("\n");
        assert!(text.contains("more lines: enlarge the terminal"), "{text}");
    }

    fn backup_form_fields(host: &str, repo: &str, pw: &str) -> Vec<crate::ui::modal::FormField> {
        let values = [host, "tank/data", "", "auto", "", repo, pw, "", "30 3 * * *", "y"];
        values.iter().map(|v| crate::ui::modal::FormField { label: "x".into(), value: (*v).to_string() }).collect()
    }

    fn backup_kind_for_status() -> crate::ui::modal::FormKind {
        crate::ui::modal::FormKind::BackupJob { hosts: vec!["local".into(), "fbsd16".into()], existing: None, jobs: vec![] }
    }

    fn check(host: usize, repo: &str, pw: &str, result: Option<crate::restic::Preflight>) -> crate::ui::state::PreflightCheck {
        crate::ui::state::PreflightCheck { host, repository: repo.into(), password_file: pw.into(), result, file_note: None }
    }

    #[test]
    fn the_check_line_shows_checking_ok_and_a_problem_for_the_values_on_screen() {
        let fields = backup_form_fields("fbsd16", "/r", "/pw");
        let kind = backup_kind_for_status();
        assert_eq!(preflight_status(&kind, &fields, Some(&check(1, "/r", "/pw", None))), Some(("Check: checking the repository...".to_string(), false)));
        assert_eq!(preflight_status(&kind, &fields, Some(&check(1, "/r", "/pw", Some(crate::restic::Preflight::Ok)))), Some(("Check: OK, the repository opens.".to_string(), false)));
        let problem = crate::restic::Preflight::Problem("no readable password file at /pw on fbsd16".to_string());
        assert_eq!(preflight_status(&kind, &fields, Some(&check(1, "/r", "/pw", Some(problem)))), Some(("Check: no readable password file at /pw on fbsd16".to_string(), true)));
    }

    #[test]
    fn the_check_line_disappears_when_a_field_it_depends_on_changes() {
        let kind = backup_kind_for_status();
        let ok = check(1, "/r", "/pw", Some(crate::restic::Preflight::Ok));
        assert!(preflight_status(&kind, &backup_form_fields("fbsd16", "/r2", "/pw"), Some(&ok)).is_none(), "repository edited");
        assert!(preflight_status(&kind, &backup_form_fields("fbsd16", "/r", "/pw2"), Some(&ok)).is_none(), "password file edited");
        assert!(preflight_status(&kind, &backup_form_fields("local", "/r", "/pw"), Some(&ok)).is_none(), "host edited");
        assert!(preflight_status(&kind, &backup_form_fields("fbsd16", " /r ", "/pw"), Some(&ok)).is_some(), "surrounding spaces are trimmed like the check does");
    }

    #[test]
    fn the_notes_wait_for_the_answer_name_the_location_and_the_permissions_and_follow_the_values_on_screen() {
        let kind = backup_kind_for_status();
        let fields = backup_form_fields("fbsd16", "/r", "/home/fb/pw");
        let mut c = check(1, "/r", "/home/fb/pw", None);
        assert!(preflight_notes(&kind, &fields, Some(&c)).is_empty(), "nothing before the check has answered");
        c.result = Some(crate::restic::Preflight::Ok);
        let notes = preflight_notes(&kind, &fields, Some(&c));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("Note: ") && notes[0].contains("home or temporary directory"), "{notes:?}");
        c.file_note = Some("the password file can be read or changed by other users (mode 644): run `chmod 600 /home/fb/pw`".to_string());
        let notes = preflight_notes(&kind, &fields, Some(&c));
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[1].contains("mode 644"), "{notes:?}");
        // Editing the password file or the host hides them, like the check line.
        assert!(preflight_notes(&kind, &backup_form_fields("fbsd16", "/r", "/root/pw"), Some(&c)).is_empty());
        assert!(preflight_notes(&kind, &backup_form_fields("local", "/r", "/home/fb/pw"), Some(&c)).is_empty());
        // A good location with nothing else to say has no notes.
        let fine = backup_form_fields("fbsd16", "/r", "/root/pw");
        let mut ok = check(1, "/r", "/root/pw", Some(crate::restic::Preflight::Ok));
        assert!(preflight_notes(&kind, &fine, Some(&ok)).is_empty());
        ok.file_note = Some(crate::restic::PASSWORD_FILE_UNCHECKED.to_string());
        assert!(preflight_notes(&kind, &fine, Some(&ok))[0].contains("could not be checked"));
    }

    #[test]
    fn only_the_backup_form_shows_a_check_line() {
        let ok = check(0, "/r", "/pw", Some(crate::restic::Preflight::Ok));
        let kind = crate::ui::modal::FormKind::CreateDataset { host: 0 };
        assert!(preflight_status(&kind, &backup_form_fields("local", "/r", "/pw"), Some(&ok)).is_none());
        assert!(preflight_status(&backup_kind_for_status(), &backup_form_fields("local", "/r", "/pw"), None).is_none());
    }
}
