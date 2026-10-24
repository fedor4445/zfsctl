//! The Jobs list and the panel of the selected job.
#![allow(clippy::wildcard_imports)]
use super::*;

/// The bottom panel of the Jobs section: the selected job's recent runs on the left, its
/// schedule, next run times and flags on the right. Drawn straight from the loaded data.
pub(super) fn draw_jobs_panel(f: &mut Frame, app: &App, area: Rect) {
    use crate::ui::jobs::{preview_lines, run_summary, schedule_label};
    let theme = &app.theme;
    let selected = app.jobs_table.selected().and_then(|i| app.jobs_view.jobs.get(i));
    let title = selected.map_or_else(|| "Job".to_string(), |j| format!("Job — {}", j.id));
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(false)).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(job) = selected else {
        f.render_widget(Paragraph::new("nothing selected").style(theme.dim()), inner);
        return;
    };
    let [left, right] = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(55), Constraint::Min(0)]).areas(inner);

    let mut runs = vec![Line::styled("Last runs", theme.header())];
    let history = app.jobs_view.runs_of(&job.id, 8);
    if history.is_empty() {
        runs.push(Line::from("No runs yet"));
    }
    for run in history {
        let style = if run.result == Some(crate::daemon::history::RunResult::Failed) { theme.err } else { Style::default() };
        runs.push(Line::styled(run_summary(run, &chrono::Local), style));
    }
    f.render_widget(Paragraph::new(runs), left);

    let label = schedule_label(&job.schedule);
    let cron = job.schedule.trim();
    let mut info = vec![Line::from(if label == cron { cron.to_string() } else { format!("{label}  ({cron})") })];
    match app.jobs_view.unsupported.get(&job.id) {
        Some(reason) => info.push(Line::styled(reason.clone(), theme.err)),
        None => info.extend(preview_lines(&job.schedule, &chrono::Local::now()).into_iter().skip(1).map(Line::from)),
    }
    info.push(Line::from(format!("{}  catch up {}", if job.enabled { "enabled" } else { "disabled" }, if job.catch_up { "yes" } else { "no" })));
    if job.kind == crate::jobs::KIND_SNAPSHOT {
        info.push(Line::from(format!("keep {}  prefix {}{}", job.keep.unwrap_or(0), crate::jobs::effective_prefix(job), if job.recursive { "  recursive" } else { "" })));
    }
    if job.kind == crate::jobs::KIND_REPLICATE {
        let dest = match job.dest_host.as_deref() {
            Some(crate::jobs::DEST_FILE) => format!("file:{}", job.dest_dir.as_deref().unwrap_or("")),
            Some(host) => format!("{host}:{}", job.dest_dataset.as_deref().unwrap_or("")),
            None => String::new(),
        };
        let flags = format!("{}{}", if job.force { "  force" } else { "" }, if job.raw { "  raw" } else { "" });
        info.push(Line::from(format!("to {dest}  prefix {}{flags}", crate::jobs::effective_prefix(job))));
        if let Some(keep) = job.dest_keep {
            info.push(Line::from(format!("keep {keep} on target")));
        }
        if let Some(p) = app.jobs_view.progress_of(&job.id) {
            info.push(Line::from(crate::daemon::progress::progress_line(p)));
        }
    }
    if job.kind == crate::jobs::KIND_BACKUP {
        let keep = job.retention.as_deref().map(str::trim).filter(|k| !k.is_empty()).map_or_else(|| "keep everything".to_string(), |k| format!("keep {k}"));
        info.push(Line::from(format!("to {}  {keep}  {}", job.repository.as_deref().unwrap_or(""), crate::ui::jobs::source_summary(job))));
        if let Some(p) = app.jobs_view.progress_of(&job.id) {
            info.push(Line::from(crate::daemon::progress::progress_line(p)));
        }
    }
    if job.kind == crate::jobs::KIND_MIRROR {
        let limit = job.bwlimit.as_deref().map(|l| format!("  limit {l}/s")).unwrap_or_default();
        info.push(Line::from(format!("to {}{limit}  {}", job.remote.as_deref().unwrap_or(""), crate::ui::jobs::source_summary(job))));
        if let Some(p) = app.jobs_view.progress_of(&job.id) {
            info.push(Line::from(crate::daemon::progress::progress_line(p)));
        }
    }
    f.render_widget(Paragraph::new(info).wrap(Wrap { trim: false }), right);
}

pub(super) fn draw_jobs(f: &mut Frame, app: &mut App, area: Rect) {
    use crate::ui::jobs::{job_target, last_result_text, next_run_text, schedule_label, DaemonState};
    let theme = app.theme.clone();
    let now = chrono::Utc::now();
    let local_now = chrono::Local::now();
    let daemon = match app.jobs_view.daemon(now) {
        DaemonState::Running { pid } => Span::raw(format!("daemon running (pid {pid})")),
        DaemonState::NotRunning => Span::styled("daemon NOT running: jobs will not fire".to_string(), theme.err),
    };
    let mut title = vec![Span::raw("Jobs — "), daemon];
    if app.jobs_view.error.is_some() {
        title.push(Span::styled(" — jobs.toml unreadable: showing the last good list".to_string(), theme.err));
    }
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(Line::from(title));
    if app.jobs_view.loaded && app.jobs_view.jobs.is_empty() {
        let text = "No scheduled jobs. Press 'n' for a scrub job, 'N' for a snapshot job or 'R' for a replication job, 'B' for a backup job, or 'J' on a pool (Pools) or a dataset (Datasets).";
        f.render_widget(Paragraph::new(text).style(theme.dim()).wrap(Wrap { trim: false }).block(block), area);
        return;
    }
    let rows: Vec<Row> = app
        .jobs_view
        .jobs
        .iter()
        .map(|j| {
            let last = app.jobs_view.last_run(&j.id);
            let (next, last_text, style) = if let Some(reason) = app.jobs_view.unsupported.get(&j.id) {
                ("-".to_string(), reason.clone(), theme.err)
            } else if !j.enabled {
                ("disabled".to_string(), last_result_text(last, now), theme.dim())
            } else {
                (next_run_text(&j.schedule, &local_now), last_result_text(last, now), Style::default())
            };
            Row::new(vec![j.id.clone(), j.host.clone(), job_target(j), schedule_label(&j.schedule), next, last_text]).style(style)
        })
        .collect();
    // The Target column is as wide as its longest entry (a destination or a repository path can be long), but
    // never so wide that Schedule, Next run and the start of Last are pushed off a narrow terminal.
    let others: u16 = 22 + 8 + 20 + 15 + 5 + 10; // the other columns, their gaps and Last's minimum
    let room = area.width.saturating_sub(2).saturating_sub(others).max(14);
    let longest = app.jobs_view.jobs.iter().map(|j| job_target(j).chars().count()).max().unwrap_or(0);
    let target_w = u16::try_from(longest).unwrap_or(u16::MAX).clamp(14, room);
    let widths = [Constraint::Length(22), Constraint::Length(8), Constraint::Length(target_w), Constraint::Length(20), Constraint::Length(15), Constraint::Min(10)];
    let table = Table::new(rows, widths)
        .header(theme.header_row(vec!["Job", "Host", "Target", "Schedule", "Next run", "Last"]))
        .block(block)
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.jobs_table);
}
