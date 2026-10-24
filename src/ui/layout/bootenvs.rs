//! The Boot Envs list.
#![allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn draw_bootenvs(f: &mut Frame, app: &mut App, area: Rect) {
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
