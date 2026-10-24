//! Datasets, their properties, permissions and ACL, and the Snapshots list.
#![allow(clippy::wildcard_imports)]
use super::*;

/// The Quota cell: how full the nearer limit is, or `-`.
pub(super) fn quota_cell(d: &crate::zfs::dataset::Dataset) -> (String, Option<u8>) {
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

pub(super) fn draw_datasets(f: &mut Frame, app: &mut App, area: Rect) {
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

pub(super) fn draw_dataset_properties(f: &mut Frame, app: &mut App, area: Rect) {
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

pub(super) fn draw_dataset_permissions(f: &mut Frame, app: &mut App, area: Rect) {
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

pub(super) fn draw_dataset_acl(f: &mut Frame, app: &mut App, area: Rect) {
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

pub(super) fn draw_snapshots(f: &mut Frame, app: &mut App, area: Rect) {
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
