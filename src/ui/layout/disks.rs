//! The Disks table and the lines that describe a disk.
#![allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn draw_disks(f: &mut Frame, app: &mut App, area: Rect) {
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
            DiskMembership::InUse { what } => format!("in use: {what}"),
        };
        let style = match &d.membership {
            DiskMembership::Available => Style::default(),
            DiskMembership::InPool { .. } | DiskMembership::InUse { .. } => theme.dim(),
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
