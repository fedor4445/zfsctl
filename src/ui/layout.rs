use super::state::{App, DatasetDetailTab, PermissionRow, Section};
use crate::ui::modal::Modal;
use crate::zfs::disk::DiskMembership;
use crate::zfs::pool::ScanStatus;
use crate::zfs::property::Property;
use crate::zfs::share::Share;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, Paragraph, Row, Table, Wrap};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let (status_text, status_style, gauge) = status_bar_content(app);
    // Sized from content, not a flat constant — a multi-line command
    // failure (e.g. `zpool add`'s "invalid vdev specification\nuse
    // '-f' to override...\nmismatched replication level...") must not
    // be silently clipped to its first line, the same lesson already
    // learned for the modal popup (see `draw_modal`). Capped at 9 so a
    // pathologically long message can't swallow the whole screen — one
    // row taller than the plain-text cap, to leave room for the gauge
    // row when one is showing.
    let status_inner_width = area.width.saturating_sub(2).max(1);
    let extra_row_for_gauge = if gauge.is_some() { 1 } else { 0 };
    let status_height = (wrapped_line_count(&status_text, status_inner_width) + 2 + extra_row_for_gauge).clamp(3, 9);

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(status_height)])
        .split(area);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(0)])
        .split(outer[0]);

    draw_sidebar(f, app, chunks[0]);
    draw_main(f, app, chunks[1]);
    draw_status_bar(f, &status_text, status_style, gauge, outer[1]);

    if let Some(modal) = app.modal.clone() {
        draw_modal(f, &modal, area);
    }
}

/// Builds the status bar's text, color, and (while a resilver/scrub is
/// running and has reported a percentage) progress-gauge info: the
/// transient action-result message (if any), followed by the
/// drilled-into pool's scrub/resilver status (if any) on its own line.
/// Colored yellow while a resilver/scrub is running, red if the most
/// recent one completed with errors — a clean or absent scan status
/// leaves the default color, since it isn't worth drawing attention to.
fn status_bar_content(app: &App) -> (String, Style, Option<(f64, String)>) {
    let mut lines: Vec<String> = Vec::new();
    if let Some(msg) = &app.status_message {
        lines.push(msg.clone());
    }
    let mut style = Style::default();
    let mut gauge = None;
    if let Some(scan) = &app.pool_scan_status {
        let (summary, scan_style) = match scan {
            ScanStatus::InProgress { summary, .. } => (summary, Style::default().fg(Color::Yellow)),
            ScanStatus::Completed { summary, errors: true } => (summary, Style::default().fg(Color::Red)),
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

fn draw_status_bar(f: &mut Frame, text: &str, style: Style, gauge: Option<(f64, String)>, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title("Status");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some((percent, label)) = gauge else {
        let status = Paragraph::new(text).style(style).wrap(Wrap { trim: false });
        f.render_widget(status, inner);
        return;
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    let status = Paragraph::new(text).style(style).wrap(Wrap { trim: false });
    f.render_widget(status, rows[0]);
    let progress = Gauge::default()
        .gauge_style(Style::default().fg(Color::Yellow))
        .ratio((percent / 100.0).clamp(0.0, 1.0))
        .label(label);
    f.render_widget(progress, rows[1]);
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .sections
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let label = match s {
                Section::Hosts => "Hosts",
                Section::Pools => "Pools",
                Section::Datasets => "Datasets",
                Section::Snapshots => "Snapshots",
                Section::Disks => "Disks",
                Section::Shares => "Shares",
                Section::Vdevs => "Vdevs",
                Section::PoolProperties => "Pool Properties",
                Section::DatasetProperties => "Dataset Properties",
                Section::DatasetPermissions => "Dataset Permissions",
            };
            let style = if i == app.selected_section {
                Style::default().add_modifier(Modifier::REVERSED)
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

fn draw_main(f: &mut Frame, app: &mut App, area: Rect) {
    if app.drilled_pool.is_some() {
        draw_vdevs(f, app, area);
        return;
    }
    let section = app.sections[app.selected_section];
    if section == Section::Pools || section == Section::Datasets {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);
        match section {
            Section::Pools => {
                draw_pools(f, app, panes[0]);
                draw_pool_properties(f, app, panes[1]);
            }
            Section::Datasets => {
                draw_datasets(f, app, panes[0]);
                draw_dataset_properties(f, app, panes[1]);
            }
            _ => unreachable!("only Pools/Datasets reach this branch"),
        }
        return;
    }
    match section {
        Section::Hosts => draw_hosts(f, app, area),
        Section::Snapshots => draw_snapshots(f, app, area),
        Section::Disks => draw_disks(f, app, area),
        Section::Shares => draw_shares(f, app, area),
        Section::Pools | Section::Datasets => unreachable!("handled above"),
        Section::Vdevs | Section::PoolProperties | Section::DatasetProperties | Section::DatasetPermissions => {
            unreachable!("never in App::sections")
        }
    }
}

fn draw_hosts(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .hosts
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let style = if i == app.selected_host {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(h.as_str()).style(style)
        })
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Hosts"));
    f.render_widget(list, area);
}

fn draw_pools(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.pools.iter().map(|p| {
        Row::new(vec![
            p.name.clone(),
            p.size.to_string(),
            p.allocated.to_string(),
            p.free.to_string(),
            format!("{:?}", p.health),
        ])
    });
    let widths = [
        Constraint::Length(20),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(10),
    ];
    let title = format!("Pools — {} ('r' refresh, → for properties)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Name", "Size", "Alloc", "Free", "Health"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.pools_table);
}

/// Dims a read-only property row so it's visually obvious at a glance
/// that it can't be edited — separate from the existing "read-only
/// property" status-message hint, which only appears after you've
/// already tried.
fn read_only_property_style(prop: &Property) -> Style {
    if prop.source.is_editable() {
        Style::default()
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_pool_properties(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.pool_properties.iter().map(|p| {
        Row::new(vec![p.name.clone(), p.value.clone(), p.source.to_string()]).style(read_only_property_style(p))
    });
    let widths = [Constraint::Length(16), Constraint::Length(16), Constraint::Min(10)];
    let pool = app.selected_pool_name().unwrap_or_else(|| "?".to_string());
    let title = format!("Properties — {pool} ('r' refresh, Enter to edit, ← back)");
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Property", "Value", "Source"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.pool_properties_table);
}

fn draw_vdevs(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.vdevs.iter().map(|v| {
        let indented = format!("{}{}", "  ".repeat(v.depth), v.name);
        Row::new(vec![indented, format!("{:?}", v.health)])
    });
    let widths = [Constraint::Length(30), Constraint::Length(12)];
    let pool = app.drilled_pool.as_deref().unwrap_or("?");
    let title = format!("Vdevs — {pool} ('r' refresh, Esc/Backspace back)");
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Device", "Health"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.vdevs_table);
}

fn draw_datasets(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.datasets.iter().map(|d| {
        Row::new(vec![
            d.name.clone(),
            d.used.to_string(),
            d.available.to_string(),
            format!("{:?}", d.kind),
        ])
    });
    let widths = [
        Constraint::Length(24),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(12),
    ];
    let title = format!("Datasets — {} ('r' refresh, → for properties)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Name", "Used", "Avail", "Kind"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.datasets_table);
}

fn draw_dataset_properties(f: &mut Frame, app: &mut App, area: Rect) {
    if app.dataset_detail_tab == DatasetDetailTab::Permissions {
        draw_dataset_permissions(f, app, area);
        return;
    }
    let rows = app.dataset_properties.iter().map(|p| {
        Row::new(vec![p.name.clone(), p.value.clone(), p.source.to_string()]).style(read_only_property_style(p))
    });
    let widths = [Constraint::Length(16), Constraint::Length(16), Constraint::Min(10)];
    let dataset = app.selected_dataset_name().unwrap_or_else(|| "?".to_string());
    let title = format!("Properties — {dataset} ('r' refresh, Enter to edit, '['/']' tabs, ← back)");
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Property", "Value", "Source"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.dataset_properties_table);
}

fn draw_dataset_permissions(f: &mut Frame, app: &mut App, area: Rect) {
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
        .header(Row::new(vec!["Target", "Scope", "Permissions"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.dataset_permissions_table);
}

fn draw_snapshots(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app
        .snapshots
        .iter()
        .map(|s| Row::new(vec![format!("{}@{}", s.dataset, s.name), s.used.to_string()]));
    let widths = [Constraint::Length(34), Constraint::Length(14)];
    let title = format!("Snapshots — {} ('r' to refresh)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Snapshot", "Used"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.snapshots_table);
}

fn draw_disks(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.disks.iter().map(|d| {
        let marker = if app.selected_disks.contains(&d.name) { "[x]" } else { "[ ]" };
        let status = match &d.membership {
            DiskMembership::Available => "available".to_string(),
            DiskMembership::InPool { pool, vdev } => format!("in {pool} ({vdev})"),
        };
        let style = match &d.membership {
            DiskMembership::Available => Style::default(),
            DiskMembership::InPool { .. } => Style::default().fg(Color::DarkGray),
        };
        Row::new(vec![format!("{marker} {}", d.name), d.size.to_string(), status]).style(style)
    });
    let widths = [Constraint::Length(18), Constraint::Length(14), Constraint::Min(20)];
    let title = format!(
        "Disks — {} ('r' refresh, Space select, 'n' create pool, 'a' add vdev, Esc clear selection)",
        app.active_host_name()
    );
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Name", "Size", "Status"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.disks_table);
}

fn draw_shares(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.shares.iter().map(|s| match s {
        Share::Nfs { dataset, raw, .. } => Row::new(vec![dataset.clone(), "NFS".to_string(), raw.clone()]),
        Share::Smb { dataset, name } => {
            Row::new(vec![dataset.clone(), "SMB".to_string(), name.clone().unwrap_or_else(|| "(default name)".to_string())])
        }
    });
    let widths = [Constraint::Length(24), Constraint::Length(8), Constraint::Min(20)];
    let mut hints = Vec::new();
    if app.nfs_available == Some(false) {
        hints.push("NFS unavailable: nfsd not running");
    }
    let hint_suffix = if hints.is_empty() { String::new() } else { format!(" [{}]", hints.join("; ")) };
    let title = format!("Shares — {} ('r' refresh, Enter edit, 'd' remove){hint_suffix}", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Dataset", "Protocol", "Details"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.shares_table);
}

/// Percentage width the modal popup uses — held constant; only the
/// height is now content-driven (see `draw_modal`).
const MODAL_WIDTH_PERCENT: u16 = 60;

fn draw_modal(f: &mut Frame, modal: &Modal, area: Rect) {
    // Estimate the popup's eventual inner width (minus borders) so we
    // can reserve enough height up front — sizing the popup by a flat
    // percentage of the terminal's height let long prompts or the
    // typed-confirmation input line clip off the bottom of the popup,
    // making it look like typing did nothing.
    let approx_width = (area.width as u32 * MODAL_WIDTH_PERCENT as u32 / 100) as u16;
    let inner_width = approx_width.saturating_sub(2).max(1);

    let content_lines = match modal {
        Modal::Confirm { prompt, .. } => wrapped_line_count(prompt, inner_width),
        Modal::ConfirmTyped { prompt, input, .. } => {
            wrapped_line_count(&format!("{prompt}\n\n> {input}"), inner_width)
        }
        Modal::Form { fields, description, .. } => {
            let fields_lines: u16 = fields
                .iter()
                .map(|field| wrapped_line_count(&format!("  {}: {}", field.label, field.value), inner_width))
                .sum();
            match description {
                // +1 blank separator line between the description and the fields.
                Some(desc) => wrapped_line_count(desc, inner_width) + 1 + fields_lines,
                None => fields_lines,
            }
        }
    };
    // +2 for the top/bottom border; floor of 5 so a one-line prompt
    // doesn't render a cramped, near-invisible box.
    let popup_height = (content_lines + 2).max(5);

    let popup_area = centered_rect(MODAL_WIDTH_PERCENT, popup_height, area);
    f.render_widget(Clear, popup_area);
    match modal {
        Modal::Confirm { prompt, .. } => {
            let p = Paragraph::new(prompt.as_str())
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("Confirm (y/n)"));
            f.render_widget(p, popup_area);
        }
        Modal::ConfirmTyped { prompt, input, .. } => {
            let text = format!("{prompt}\n\n> {input}");
            let p = Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("Type to confirm (Enter/Esc)"));
            f.render_widget(p, popup_area);
        }
        Modal::Form { title, fields, focused, description, .. } => {
            let mut lines = Vec::new();
            if let Some(desc) = description {
                lines.push(desc.clone());
                lines.push(String::new());
            }
            for (i, field) in fields.iter().enumerate() {
                let marker = if i == *focused { ">" } else { " " };
                lines.push(format!("{marker} {}: {}", field.label, field.value));
            }
            let p = Paragraph::new(lines.join("\n"))
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title(title.as_str()));
            f.render_widget(p, popup_area);
        }
    }
}

/// Conservatively estimates how many terminal rows `text` occupies when
/// wrapped at `width` columns. Doesn't replicate `Paragraph`'s exact
/// word-boundary wrapping, but over-reserves rather than under-reserves
/// height, which is the safe direction — a popup with a little extra
/// blank space is harmless, one that clips content is not.
fn wrapped_line_count(text: &str, width: u16) -> u16 {
    let width = width.max(1) as usize;
    text.lines()
        .map(|line| if line.is_empty() { 1 } else { ((line.len() + width - 1) / width) as u16 })
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
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(margin), Constraint::Length(height), Constraint::Min(0)])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
