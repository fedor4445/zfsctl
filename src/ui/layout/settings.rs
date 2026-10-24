//! The Settings and Alerts screens.
#![allow(clippy::wildcard_imports)]
use super::*;

/// The Alerts section: the saved `[notify]` settings, the monitor's status, the recent health events.
/// The Settings section: the rows of `ui::settings` under their group titles, the selected one as a bar.
pub(super) fn draw_settings(f: &mut Frame, app: &App, area: Rect) {
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

pub(super) fn draw_alerts(f: &mut Frame, app: &App, area: Rect) {
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
