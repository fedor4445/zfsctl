use super::state::{App, Section};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Row, Table};
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
    match app.sections[app.selected_section] {
        Section::Hosts => draw_hosts(f, app, area),
        Section::Pools => draw_pools(f, app, area),
        Section::Datasets => draw_datasets(f, app, area),
        Section::Snapshots => draw_snapshots(f, app, area),
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
