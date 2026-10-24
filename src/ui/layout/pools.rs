//! The Pools list, its properties and the vdev drill-down.
#![allow(clippy::wildcard_imports)]
use super::*;

/// Name column width the Pools table protects before it spends space on
/// anything beyond Health and a capacity figure.
pub(super) const POOL_NAME_PREFERRED: u16 = 16;

pub(super) fn draw_pools(f: &mut Frame, app: &mut App, area: Rect) {
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

pub(super) fn draw_pool_properties(f: &mut Frame, app: &mut App, area: Rect) {
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

pub(super) fn draw_vdevs(f: &mut Frame, app: &mut App, area: Rect) {
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
