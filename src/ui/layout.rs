use super::state::{App, DatasetDetailTab, PaneFocus, PermissionRow, Section};
use crate::ui::modal::{Modal, Tone};
use crate::zfs::disk::DiskMembership;
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
    let gauge_rows = if gauge.is_some() { 1 } else { 0 };
    let status_height = text_rows + gauge_rows;

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(status_height), Constraint::Length(1)])
        .split(area);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(0)])
        .split(outer[0]);

    draw_sidebar(f, app, chunks[0]);
    draw_main(f, app, chunks[1]);
    draw_status_line(f, &status_text, status_style, gauge, &app.theme, outer[1]);
    let hint_line = crate::ui::hints::hint_line(&crate::ui::hints::hints(app), area.width as usize, &app.theme);
    f.render_widget(Paragraph::new(hint_line), outer[2]);

    if let Some(modal) = app.modal.clone() {
        draw_modal(f, &modal, &app.theme, area);
    }
    if let Some(picker) = app.picker.clone() {
        draw_picker(f, &picker, &app.theme, area);
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
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    f.render_widget(Paragraph::new(text).style(style).wrap(Wrap { trim: false }), rows[0]);
    let progress = Gauge::default()
        .gauge_style(theme.info)
        .ratio((percent / 100.0).clamp(0.0, 1.0))
        .label(label);
    f.render_widget(progress, rows[1]);
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme.clone();
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
                Section::DatasetAcl => "Dataset ACL",
            };
            let style = if i == app.selected_section { theme.selected_row() } else { Style::default() };
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
        Section::Vdevs | Section::PoolProperties | Section::DatasetProperties | Section::DatasetPermissions | Section::DatasetAcl => {
            unreachable!("never in App::sections")
        }
    }
}

fn draw_hosts(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme.clone();
    let items: Vec<ListItem> = app
        .hosts
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let style = if i == app.selected_host {
                theme.selected_row()
            } else {
                Style::default()
            };
            ListItem::new(h.as_str()).style(style)
        })
        .collect();
    let list_area = if app.hosts.len() == 1 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(4)])
            .split(area);
        let hint = Paragraph::new("Only this host is configured. Press 'n' to add another host.".to_string())
            .wrap(Wrap { trim: false })
            .style(theme.dim());
        f.render_widget(hint, chunks[1]);
        chunks[0]
    } else {
        area
    };
    let list = List::new(items).block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title("Hosts"));
    f.render_widget(list, list_area);
}

/// Name column width the Pools table protects before it spends space on
/// anything beyond Health and a capacity figure.
const POOL_NAME_PREFERRED: u16 = 16;
const POOL_NAME_MINIMUM: u16 = 8;

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
    let needs = |non_name: &[u16]| non_name.iter().sum::<u16>() + non_name.len() as u16;
    let mut non_name: Vec<u16> = vec![12];
    let mut headers: Vec<&str> = vec!["Name", "Health"];
    #[derive(PartialEq)]
    enum Used {
        Full,
        Compact,
        Hidden,
    }
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
        .header(Row::new(headers))
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
    let widths = [Constraint::Length(16), Constraint::Length(16), Constraint::Min(10)];
    let pool = app.selected_pool_name().unwrap_or_else(|| "?".to_string());
    let title = format!("Properties — {pool} ('r' refresh, Enter to edit, ← back)");
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Property", "Value", "Source"]))
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
        .header(Row::new(vec!["Device", "Health"]))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.vdevs_table);
}

fn draw_datasets(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
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
        Row::new(vec![p.name.clone(), p.value.clone(), p.source.to_string()]).style(read_only_property_style(&theme, p))
    });
    let widths = [Constraint::Length(16), Constraint::Length(16), Constraint::Min(10)];
    let dataset = app.selected_dataset_name().unwrap_or_else(|| "?".to_string());
    let title = format!("Properties — {dataset} ('r' refresh, Enter to edit, '['/']' tabs, ← back)");
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Property", "Value", "Source"]))
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
        .header(Row::new(vec!["Target", "Scope", "Permissions"]))
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
        let target = match &entry.tag {
            crate::acl::AclTag::Owner => "owner@".to_string(),
            crate::acl::AclTag::OwningGroup => "group@".to_string(),
            crate::acl::AclTag::Everyone => "everyone@".to_string(),
            crate::acl::AclTag::User(name) => format!("user {name}"),
            crate::acl::AclTag::NamedGroup(name) => format!("group {name}"),
        };
        let flags = if entry.flags.is_empty() { "—".to_string() } else { entry.flags.join("/") };
        let entry_type = match entry.entry_type {
            crate::acl::AclEntryType::Allow => "Allow",
            crate::acl::AclEntryType::Deny => "Deny",
        };
        Row::new(vec![i.to_string(), target, entry.permissions.join("/"), flags, entry_type.to_string()])
    });
    let widths = [Constraint::Length(3), Constraint::Length(14), Constraint::Min(20), Constraint::Length(16), Constraint::Length(6)];
    let title = format!("ACL — {dataset} ('r' refresh, 'n' add, 'd' remove, '['/']' tabs, ← back)");
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["#", "Target", "Permissions", "Flags", "Type"]))
        .block(Block::default().borders(Borders::ALL).border_style(pane_border_style(&theme, app.pane_focus, PaneFocus::Right)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.acl_entries_table);
}

fn draw_snapshots(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let rows = app
        .snapshots
        .iter()
        .map(|s| Row::new(vec![format!("{}@{}", s.dataset, s.name), s.used.to_string()]));
    let widths = [Constraint::Length(34), Constraint::Length(14)];
    let title = format!("Snapshots — {} ('r' to refresh)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Snapshot", "Used"]))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.snapshots_table);
}

fn draw_disks(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
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
        Row::new(vec![format!("{marker} {}", d.name), d.size.to_string(), status]).style(style)
    });
    let widths = [Constraint::Length(18), Constraint::Length(14), Constraint::Min(20)];
    let title = format!(
        "Disks — {} ('r' refresh, Space select, 'n' create pool, 'a' add vdev, Esc clear selection)",
        app.active_host_name()
    );
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Name", "Size", "Status"]))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.disks_table);
}

fn draw_shares(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let rows = app.shares.iter().map(|s| match s {
        Share::Nfs { dataset, raw, .. } => Row::new(vec![dataset.clone(), "NFS".to_string(), raw.clone()]),
        Share::Smb { dataset, name, valid_user, read_only, .. } => Row::new(vec![
            dataset.clone(),
            "SMB".to_string(),
            format!("\\\\{name}  user={}  {}", valid_user.as_deref().unwrap_or("(guest)"), if *read_only { "ro" } else { "rw" }),
        ]),
    });
    let widths = [Constraint::Length(24), Constraint::Length(8), Constraint::Min(20)];
    let mut hints = Vec::new();
    if app.nfs_available == Some(false) {
        hints.push("NFS unavailable: nfsd not running");
    }
    if app.samba_available == Some(false) {
        hints.push("SMB unavailable: smbd not running");
    }
    let hint_suffix = if hints.is_empty() { String::new() } else { format!(" [{}]", hints.join("; ")) };
    let title = format!("Shares — {} ('r' refresh, Enter edit, 'd' remove){hint_suffix}", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Dataset", "Protocol", "Details"]))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.shares_table);
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

fn draw_modal(f: &mut Frame, modal: &Modal, theme: &Theme, area: Rect) {
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
        Modal::Info { body, .. } => wrapped_line_count(body, inner_width),
    };
    // +2 for the top/bottom border; floor of 5 so a one-line prompt
    // doesn't render a cramped, near-invisible box.
    let popup_height = (content_lines + 2).max(5);

    let popup_area = centered_rect(MODAL_WIDTH_PERCENT, popup_height, area);
    f.render_widget(Clear, popup_area);
    match modal {
        Modal::Confirm { prompt, action } => {
            let block = confirm_block(theme, action.call.tone(), action.call.label(), "Confirm (y/n)", "confirm (y/n)");
            let p = Paragraph::new(prompt.as_str()).wrap(Wrap { trim: false }).block(block);
            f.render_widget(p, popup_area);
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
                .block(Block::default().borders(Borders::ALL).border_style(theme.accent).title(title.as_str()));
            f.render_widget(p, popup_area);
        }
        Modal::Info { title, body } => {
            let p = Paragraph::new(body.as_str())
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title(title.as_str()));
            f.render_widget(p, popup_area);
        }
    }
}

/// Renders on top of `draw_modal`'s output whenever `app.picker` is
/// `Some` -- the form underneath stays drawn exactly as `draw_modal`
/// left it; this is a second, smaller popup layered above it.
fn draw_picker(f: &mut Frame, picker: &crate::ui::picker::PickerState, theme: &Theme, area: Rect) {
    let filter_lower = picker.filter.to_lowercase();
    let filtered: Vec<&String> = picker.values.iter().filter(|v| v.to_lowercase().contains(&filter_lower)).collect();

    const PICKER_WIDTH_PERCENT: u16 = 50;
    let popup_height = (filtered.len() as u16 + 4).clamp(6, area.height.saturating_sub(4).max(6));
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

    let title = if picker.filter.is_empty() {
        "Pick a value (type filter, Space toggle, Enter OK, Esc cancel)".to_string()
    } else {
        format!("Pick — filter '{}' (Space toggle, Enter OK, Esc cancel)", picker.filter)
    };
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

/// The border style for a Pools/Datasets split-pane block: the theme's
/// accent when `pane` is the one `current` currently points at, the
/// terminal's default style otherwise.
fn pane_border_style(theme: &Theme, current: PaneFocus, pane: PaneFocus) -> Style {
    theme.pane_border(current == pane)
}

#[cfg(test)]
mod tests {
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

    fn pool(name: &str, allocated: u64, size: u64, health: PoolHealth) -> Pool {
        Pool { name: name.to_string(), size, allocated, free: size.saturating_sub(allocated), health }
    }

    fn app_in_pools(pools: Vec<Pool>) -> App {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
        app.pools = pools;
        app.pools_table.select(Some(0));
        app
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
        buf.content().iter().find(|c| c.symbol() == symbol).map(|c| c.style())
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
        app.modal = Some(Modal::Confirm {
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
        app.modal = Some(Modal::Confirm {
            prompt: "Scrub pool 'tank'? (y/n)".to_string(),
            action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::ScrubPool { pool: "tank".to_string() } },
        });
        let rows = render(&mut app, 100, 30).join("\n");
        assert!(rows.contains("Confirm (y/n)"), "{rows}");
        assert!(!rows.contains('✖'));
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
}
