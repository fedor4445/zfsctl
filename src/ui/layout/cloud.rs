//! The Cloud table and the panel of the selected remote.
#![allow(clippy::wildcard_imports)]
use super::*;

/// The Cloud section: every rclone remote the hub (and any host with a cloud job) knows, with its jobs.
pub(super) fn draw_cloud(f: &mut Frame, app: &mut App, area: Rect) {
    use crate::ui::cloud::{failed, row_fill, table_cells, Fill};
    let theme = app.theme.clone();
    let now = chrono::Utc::now();
    let rows = app.cloud_rows();
    let jobs: usize = rows.iter().map(|r| r.job_ids.len()).sum();
    let mut title = vec![Span::raw(format!("Cloud — {} remote{}, {} job{}", rows.len(), if rows.len() == 1 { "" } else { "s" }, jobs, if jobs == 1 { "" } else { "s" }))];
    let mut problems: Vec<_> = app.cloud_errors.iter().collect();
    problems.sort();
    for (host, why) in problems {
        title.push(Span::styled(format!(" — {host}: {why}"), theme.err));
    }
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(Line::from(title));
    if rows.is_empty() {
        let text = if app.cloud_known.is_empty() && app.cloud_errors.is_empty() {
            "Looking for rclone remotes..."
        } else {
            "No rclone remotes found. Create one as root with `rclone config` (see the book, chapter 15), then press 'r'. 'C' makes a cloud mirror job."
        };
        f.render_widget(Paragraph::new(text).style(theme.dim()).wrap(Wrap { trim: false }).block(block), area);
        return;
    }
    let table_rows: Vec<Row> = rows
        .iter()
        .map(|r| {
            let style = if failed(r, &app.jobs_view) || row_fill(r, &app.cloud_sizes, &app.cloud_caps) == Some(Fill::Full) {
                theme.err
            } else if row_fill(r, &app.cloud_sizes, &app.cloud_caps) == Some(Fill::Warn) {
                theme.warn
            } else if r.job_ids.is_empty() {
                theme.dim()
            } else {
                Style::default()
            };
            Row::new(table_cells(r, &app.jobs_view, &app.cloud_sizes, &app.cloud_caps, now)).style(style)
        })
        .collect();
    let widths = [Constraint::Length(16), Constraint::Length(14), Constraint::Length(8), Constraint::Length(4), Constraint::Length(12), Constraint::Min(10)];
    let table = Table::new(table_rows, widths).header(theme.header_row(vec!["Provider", "Remote", "Host", "Jobs", "Last", "In bucket"])).block(block).row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.cloud_table);
}

/// The Cloud section's detail panel: the selected remote's jobs, what `s` measured, and the F5 check.
pub(super) fn draw_cloud_panel(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let row = app.cloud_table.selected().and_then(|i| app.cloud_rows().into_iter().nth(i));
    let title = row.as_ref().map_or_else(|| "Remote".to_string(), |r| format!("Remote — {}:", r.name));
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(false)).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(row) = row else {
        f.render_widget(Paragraph::new("nothing selected").style(theme.dim()), inner);
        return;
    };
    // The plan line is yellow from 80% and red from 95%; every other problem line is red.
    let fill = crate::ui::cloud::row_fill(&row, &app.cloud_sizes, &app.cloud_caps);
    let mut lines: Vec<Line> = crate::ui::cloud::detail(&row, &app.jobs_view, &app.cloud_sizes, &app.cloud_caps, chrono::Utc::now())
        .into_iter()
        .enumerate()
        .map(|(i, (text, bad))| {
            let style = match (bad, text.starts_with("Plan cap"), fill) {
                (true, true, Some(crate::ui::cloud::Fill::Warn)) => theme.warn,
                (true, _, _) => theme.err,
                _ if i == 0 => theme.dim(),
                _ => Style::default(),
            };
            Line::styled(text, style)
        })
        .collect();
    if let Some(why) = app.cloud_errors.get(&row.host) {
        lines.push(Line::styled(format!("rclone on {}: {why}", row.host), theme.err));
    }
    let mirror_remote = crate::ui::cloud::check_target(&row, &app.jobs_view.jobs);
    let check = app.backup_preflight.as_ref().filter(|c| c.password_file.is_empty() && app.hosts.get(c.host) == Some(&row.host) && Some(&c.repository) == mirror_remote.as_ref());
    if let Some(check) = check {
        lines.push(match &check.result {
            None => Line::styled("Check: checking the remote...".to_string(), theme.dim()),
            Some(crate::restic::Preflight::Ok) => Line::from("Check: OK, the remote opens.".to_string()),
            Some(crate::restic::Preflight::Problem(m)) => Line::styled(format!("Check: {m}"), theme.err),
        });
        if let Some(note) = check.file_note.as_deref() {
            lines.push(Line::styled(format!("Note: {note}"), theme.dim()));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
