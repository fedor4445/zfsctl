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
const TYPE_W: u16 = 6;
const POOL_NAME_MINIMUM: u16 = 8;

#[derive(PartialEq)]
enum Used {
    Full,
    Compact,
    Hidden,
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

/// The border style for a Pools/Datasets split-pane block: the theme's
/// accent when `pane` is the one `current` currently points at, the
/// terminal's default style otherwise.
fn pane_border_style(theme: &Theme, current: PaneFocus, pane: PaneFocus) -> Style {
    theme.pane_border(current == pane)
}

mod bootenvs;
mod cloud;
mod datasets;
mod disks;
mod hosts;
mod jobs;
mod modals;
mod pools;
mod settings;
mod shares;
#[allow(clippy::wildcard_imports)]
use bootenvs::*;
#[allow(clippy::wildcard_imports)]
use cloud::*;
#[allow(clippy::wildcard_imports)]
use datasets::*;
#[allow(clippy::wildcard_imports)]
use disks::*;
#[allow(clippy::wildcard_imports)]
use hosts::*;
#[allow(clippy::wildcard_imports)]
use jobs::*;
#[allow(clippy::wildcard_imports)]
use modals::*;
#[allow(clippy::wildcard_imports)]
use pools::*;
#[allow(clippy::wildcard_imports)]
use settings::*;
#[allow(clippy::wildcard_imports)]
use shares::*;
pub(crate) use shares::smb_access_label;

#[cfg(test)]
mod tests;
