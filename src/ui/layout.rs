use super::state::{App, Section};
use crate::ui::modal::Modal;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Row, Table, Wrap};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(f.area());

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(0)])
        .split(outer[0]);

    draw_sidebar(f, app, chunks[0]);
    draw_main(f, app, chunks[1]);
    draw_status_bar(f, app, outer[1]);

    if let Some(modal) = app.modal.clone() {
        let area = f.area();
        draw_modal(f, &modal, area);
    }
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let text = app.status_message.as_deref().unwrap_or("");
    let status = Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("Status"));
    f.render_widget(status, area);
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
                Section::Vdevs => "Vdevs",
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
    match app.sections[app.selected_section] {
        Section::Hosts => draw_hosts(f, app, area),
        Section::Pools => draw_pools(f, app, area),
        Section::Datasets => draw_datasets(f, app, area),
        Section::Snapshots => draw_snapshots(f, app, area),
        Section::Vdevs => unreachable!("Vdevs is never in App::sections"),
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
    let title = format!("Pools — {} ('r' to refresh)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Name", "Size", "Alloc", "Free", "Health"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.pools_table);
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
    let title = format!("Datasets — {} ('r' to refresh)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Name", "Used", "Avail", "Kind"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.datasets_table);
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
        Modal::Form { fields, .. } => fields
            .iter()
            .map(|field| wrapped_line_count(&format!("  {}: {}", field.label, field.value), inner_width))
            .sum(),
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
        Modal::Form { title, fields, focused, .. } => {
            let mut lines = Vec::new();
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
