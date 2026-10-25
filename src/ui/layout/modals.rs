//! Pop-ups: confirmations, forms, the picker and the plain information boxes.
#![allow(clippy::wildcard_imports)]
use super::*;

/// Percentage width the modal popup uses — held constant; only the
/// height is now content-driven (see `draw_modal`).
pub(super) const MODAL_WIDTH_PERCENT: u16 = 60;

/// Border and title for a Confirm/ConfirmTyped modal. Danger and warn
/// get a glyph plus the action's name in the title (so they read as such
/// without color) and the theme's danger/warn style on the border.
pub(super) fn confirm_block(theme: &Theme, tone: Tone, label: &str, neutral_title: &str, risky_suffix: &str) -> Block<'static> {
    let (glyph, border) = match tone {
        Tone::Danger => ("✖", theme.danger),
        Tone::Warn => ("▲", theme.warn),
        Tone::Neutral => ("", Style::default()),
    };
    let title = if tone == Tone::Neutral { neutral_title.to_string() } else { format!("{glyph} {label} — {risky_suffix}") };
    Block::default().borders(Borders::ALL).border_style(border).title(Span::styled(title, border))
}

/// Extra lines drawn under a form's fields, computed from what is typed so far: for a job
/// form, what the schedule means and its next three run times.
pub(super) fn form_preview(kind: &crate::ui::modal::FormKind, fields: &[crate::ui::modal::FormField]) -> Vec<String> {
    match kind {
        crate::ui::modal::FormKind::Job { .. } => crate::ui::jobs::preview_lines(fields.get(2).map_or("", |f| f.value.as_str()), &chrono::Local::now()),
        crate::ui::modal::FormKind::SnapshotJob { .. } => crate::ui::jobs::preview_lines(fields.get(5).map_or("", |f| f.value.as_str()), &chrono::Local::now()),
        crate::ui::modal::FormKind::BackupJob { .. } => crate::ui::jobs::preview_lines(fields.get(8).map_or("", |f| f.value.as_str()), &chrono::Local::now()),
        crate::ui::modal::FormKind::MirrorJob { .. } => crate::ui::jobs::preview_lines(fields.get(7).map_or("", |f| f.value.as_str()), &chrono::Local::now()),
        crate::ui::modal::FormKind::ReplicateJob { .. } => crate::ui::jobs::preview_lines(fields.get(6).map_or("", |f| f.value.as_str()), &chrono::Local::now()),
        _ => Vec::new(),
    }
}

/// The F5 check that still describes what is on screen: editing the host, repository or password file hides it.
pub(super) fn current_check<'a>(kind: &crate::ui::modal::FormKind, fields: &[crate::ui::modal::FormField], check: Option<&'a crate::ui::state::PreflightCheck>) -> Option<&'a crate::ui::state::PreflightCheck> {
    let (crate::ui::modal::FormKind::BackupJob { hosts, .. } | crate::ui::modal::FormKind::MirrorJob { hosts, .. }) = kind else { return None };
    let check = check?;
    let value = |i: usize| fields.get(i).map_or("", |f| f.value.as_str()).trim();
    // A cloud mirror form has no password file: its check carries an empty one.
    let password_file = if matches!(kind, crate::ui::modal::FormKind::MirrorJob { .. }) { "" } else { value(6) };
    (hosts.get(check.host).map(String::as_str) == Some(value(0)) && check.repository == value(5) && check.password_file == password_file).then_some(check)
}

/// The backup form's F5 result as one line, and whether it is a problem. Shown only while the check still
/// describes what is on screen.
pub(super) fn preflight_status(kind: &crate::ui::modal::FormKind, fields: &[crate::ui::modal::FormField], check: Option<&crate::ui::state::PreflightCheck>) -> Option<(String, bool)> {
    let check = current_check(kind, fields, check)?;
    let what = if matches!(kind, crate::ui::modal::FormKind::MirrorJob { .. }) { "remote" } else { "repository" };
    Some(match &check.result {
        None => (format!("Check: checking the {what}..."), false),
        Some(crate::restic::Preflight::Ok) => (format!("Check: OK, the {what} opens."), false),
        Some(crate::restic::Preflight::Problem(m)) => (format!("Check: {m}"), true),
    })
}

/// Advice about the password file under the F5 result: where it is, and who can read it. Empty until the check has
/// answered, and while it no longer describes what is on screen.
pub(super) fn preflight_notes(kind: &crate::ui::modal::FormKind, fields: &[crate::ui::modal::FormField], check: Option<&crate::ui::state::PreflightCheck>) -> Vec<String> {
    let Some(check) = current_check(kind, fields, check).filter(|c| c.result.is_some()) else { return Vec::new() };
    let location = crate::restic::password_file_location_note(&check.password_file);
    location.into_iter().chain(check.file_note.clone()).map(|n| format!("Note: {n}")).collect()
}

/// The line under a y/n confirm's prompt. `CONFIRM_YES` is the longest, so it sizes the window.
pub(super) const CONFIRM_NONE: &str = "Press y for yes or n for no, then Enter.";
pub(super) const CONFIRM_YES: &str = "▶ YES selected: press Enter to run it (n or Esc to change).";
pub(super) const CONFIRM_NO: &str = "▶ NO selected: press Enter to cancel (y to change).";

pub(super) fn draw_modal(f: &mut Frame, modal: &Modal, check: Option<&crate::ui::state::PreflightCheck>, info_scroll: u16, theme: &Theme, area: Rect) {
    // Estimate the popup's eventual inner width (minus borders) so we
    // can reserve enough height up front — sizing the popup by a flat
    // percentage of the terminal's height let long prompts or the
    // typed-confirmation input line clip off the bottom of the popup,
    // making it look like typing did nothing.
    let approx_width = u16::try_from(u32::from(area.width) * u32::from(MODAL_WIDTH_PERCENT) / 100).unwrap_or(u16::MAX);
    let inner_width = approx_width.saturating_sub(2).max(1);

    let content_lines = match modal {
        // The prompt, a blank line and the selection line (sized by the longest of its three texts).
        // Word wrapping can need more rows than the byte-based estimate, so it gets the same ~25% + 1 margin as the forms.
        Modal::Confirm { prompt, .. } => {
            let n = wrapped_line_count(&format!("{prompt}\n\n{CONFIRM_YES}"), inner_width);
            n + n / 4 + 1
        }
        Modal::ConfirmTyped { prompt, input, .. } => {
            wrapped_line_count(&format!("{prompt}\n\n> {input}"), inner_width)
        }
        Modal::Form { fields, description, kind, .. } => {
            let preview = form_preview(kind, fields);
            let preview_lines: u16 = if preview.is_empty() { 0 } else { 1 + preview.iter().map(|l| wrapped_line_count(l, inner_width)).sum::<u16>() };
            let status_lines: u16 = preflight_status(kind, fields, check).map_or(0, |(text, _)| {
                let n = wrapped_line_count(&text, inner_width);
                1 + n + n / 4 + 1
            }) + preflight_notes(kind, fields, check)
                .iter()
                .map(|note| {
                    let n = wrapped_line_count(note, inner_width);
                    n + n / 4 + 1
                })
                .sum::<u16>();
            let fields_lines: u16 = preview_lines
                + status_lines
                + fields
                    .iter()
                    .map(|field| wrapped_line_count(&format!("  {}: {}", field.label, field.value), inner_width))
                    .sum::<u16>();
            match description {
                // +1 blank separator line between the description and the fields.
                // Word wrapping can need more rows than the byte-based
                // estimate, so the description gets the same ~25% + 1 row
                // margin as the picker popup (clamped to the terminal below).
                Some(desc) => {
                    let d = wrapped_line_count(desc, inner_width);
                    d + d / 4 + 1 + 1 + fields_lines
                }
                None => fields_lines,
            }
        }
        Modal::Info { body, .. } => wrapped_line_count(body, inner_width),
        Modal::Choose { note, rows, look, .. } => u16::try_from(note.lines().count() + 1 + rows.len() + (look.heads.len() * 2).saturating_sub(1)).unwrap_or(u16::MAX),
    };
    // +2 for the top/bottom border; floor of 5 so a one-line prompt
    // doesn't render a cramped, near-invisible box.
    let popup_height = (content_lines + 2).max(5).min(area.height);

    let popup_area = centered_rect(MODAL_WIDTH_PERCENT, popup_height, area);
    f.render_widget(Clear, popup_area);
    match modal {
        Modal::Confirm { prompt, action, choice } => {
            let tone = action.call.tone();
            let block = confirm_block(theme, tone, action.call.label(), "Confirm (y/n, then Enter)", "confirm (y/n, then Enter)");
            let selected = match tone {
                Tone::Danger => theme.danger,
                Tone::Warn => theme.warn,
                Tone::Neutral => Style::default().add_modifier(Modifier::BOLD),
            };
            let mut lines: Vec<Line> = prompt.lines().map(|l| Line::raw(l.to_string())).collect();
            lines.push(Line::raw(""));
            lines.push(match choice {
                None => Line::styled(CONFIRM_NONE, theme.dim()),
                Some(true) => Line::styled(CONFIRM_YES, selected),
                Some(false) => Line::styled(CONFIRM_NO, Style::default().add_modifier(Modifier::BOLD)),
            });
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), popup_area);
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
        Modal::Form { title, fields, focused, description, kind } => {
            let mut lines: Vec<Line> = Vec::new();
            if let Some(desc) = description {
                lines.extend(desc.split('\n').map(|l| Line::raw(l.to_string())));
                lines.push(Line::raw(String::new()));
            }
            for (i, field) in fields.iter().enumerate() {
                let marker = if i == *focused { ">" } else { " " };
                let shown = if kind.masks_field(i) { "*".repeat(field.value.chars().count()) } else { field.value.clone() };
                lines.push(Line::raw(format!("{marker} {}: {shown}", field.label)));
            }
            let preview = form_preview(kind, fields);
            if !preview.is_empty() {
                lines.push(Line::raw(String::new()));
                lines.extend(preview.into_iter().map(Line::raw));
            }
            if let Some((text, problem)) = preflight_status(kind, fields, check) {
                lines.push(Line::raw(String::new()));
                lines.push(if problem { Line::styled(text, theme.danger) } else { Line::raw(text) });
            }
            lines.extend(preflight_notes(kind, fields, check).into_iter().map(|note| Line::styled(note, theme.warn)));
            let p = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).border_style(theme.accent).title(title.as_str()));
            f.render_widget(p, popup_area);
        }
        Modal::Info { title, body } => {
            // The wordmark (About) is drawn in the warn colour, and left out when the pop-up is too narrow for it: wrapped, it is garbage.
            let fits = usize::from(popup_area.width.saturating_sub(2)) >= crate::ui::logo::WIDTH;
            let lines: Vec<Line> = body
                .lines()
                .filter(|l| fits || !crate::ui::logo::is_wordmark_line(l))
                .map(|l| {
                    // Help's section titles ("Global", a section's name) are the lines that do not start with a space.
                    let help_title = title == "Help" && !l.is_empty() && !l.starts_with(' ');
                    let line = if crate::ui::logo::is_wordmark_line(l) {
                        Line::styled(l.to_string(), theme.warn)
                    } else if help_title {
                        Line::styled(l.to_string(), theme.header())
                    } else {
                        Line::raw(l.to_string())
                    };
                    if crate::ui::logo::is_centred_line(l) { line.centered() } else { line }
                })
                .collect();
            let p = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((info_scroll, 0))
                .block(Block::default().borders(Borders::ALL).title(title.as_str()));
            f.render_widget(p, popup_area);
        }
        Modal::Choose { title, note, rows, selected, look, .. } => {
            let mut lines: Vec<Line> = note.lines().map(|l| Line::raw(l.to_string())).collect();
            lines.push(Line::raw(""));
            let mut chosen_at = lines.len();
            for (i, row) in rows.iter().enumerate() {
                if let Some((_, head)) = look.heads.iter().find(|(at, _)| *at == i) {
                    if i > 0 {
                        lines.push(Line::raw(""));
                    }
                    lines.push(Line::styled(head.clone(), theme.header()));
                }
                let base = look.levels.get(i).map_or_else(Style::default, |level| crate::ui::dashboard_view::level_style(theme, *level));
                let mark = if look.marked.is_empty() { "" } else if look.marked.get(i) == Some(&true) { "+ " } else { "  " };
                let text = if look.heads.is_empty() { format!("{mark}{row}") } else { format!("  {mark}{row}") };
                if i == *selected {
                    chosen_at = lines.len();
                    lines.push(Line::styled(text, base.patch(theme.selected_row())));
                } else {
                    lines.push(Line::styled(text, base));
                }
            }
            // Keep the chosen row in view: scroll just far enough that it is the last line shown.
            let visible = usize::from(popup_area.height.saturating_sub(2));
            let scroll = u16::try_from((chosen_at + 1).saturating_sub(visible)).unwrap_or(u16::MAX);
            let p = Paragraph::new(lines).scroll((scroll, 0)).block(Block::default().borders(Borders::ALL).title(title.as_str()));
            f.render_widget(p, popup_area);
        }
    }
}

/// Title of a ready picker popup. `Space toggle` is only mentioned for
/// multi-select pickers; in a single-select picker Space does nothing.
pub(super) fn picker_title(filter: &str, multi_select: bool) -> String {
    let keys = if multi_select { "Space toggle, Enter OK, Esc cancel" } else { "Enter OK, Esc cancel" };
    if filter.is_empty() {
        format!("Pick a value (type filter, {keys})")
    } else {
        format!("Pick — filter '{filter}' ({keys})")
    }
}

/// Renders on top of `draw_modal`'s output whenever `app.picker` is
/// `Some` -- the form underneath stays drawn exactly as `draw_modal`
/// left it; this is a second, smaller popup layered above it.
pub(super) fn draw_picker(f: &mut Frame, picker: &crate::ui::picker::PickerState, theme: &Theme, area: Rect) {
    const PICKER_WIDTH_PERCENT: u16 = 50;
    if picker.load != crate::ui::picker::PickerLoad::Ready {
        use crate::ui::picker::PickerLoad;
        let what = if picker.pool_host.is_some() { "names" } else { "accounts" };
        let body = match &picker.load {
            PickerLoad::Failed(msg) => {
                format!("Could not load {what}: {}\n\nEsc to close", sanitise_for_display(msg))
            }
            PickerLoad::Loading => format!("Loading {what}…\n\nEsc to close"),
            PickerLoad::Ready => unreachable!("guarded by the enclosing `!= Ready` check"),
        };
        // Width first (it only depends on the percentage), then a height
        // that fits the wrapped body so the closing hint is never clipped.
        let width = centered_rect(PICKER_WIDTH_PERCENT, 1, area).width;
        let popup_area = centered_rect(PICKER_WIDTH_PERCENT, failed_popup_height(&body, width, area.height), area);
        f.render_widget(Clear, popup_area);
        f.render_widget(
            Paragraph::new(body)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).border_style(theme.accent).title("Pick a value")),
            popup_area,
        );
        return;
    }
    let filter_lower = picker.filter.to_lowercase();
    let filtered: Vec<&String> = picker.values.iter().filter(|v| v.to_lowercase().contains(&filter_lower)).collect();

    let popup_height = (u16::try_from(filtered.len()).unwrap_or(u16::MAX).saturating_add(4)).clamp(6, area.height.saturating_sub(4).max(6));
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

    let mut title = picker_title(&picker.filter, picker.multi_select);
    if let Some(note) = &picker.note {
        let inner = popup_area.width.saturating_sub(2) as usize;
        title = format!("{title} — {}", truncate_with_ellipsis(&sanitise_for_display(note), inner));
    }
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

/// Carves a centered rectangle out of `area`: `percent_x` percent of
/// `area`'s width, `height` rows tall (clamped to fit within `area`).
/// Standard ratatui popup pattern, adapted to a content-driven height
/// instead of a flat percentage (see `draw_modal`).
pub(super) fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let height = height.min(area.height);
    let margin = area.height.saturating_sub(height) / 2;
    let [_, middle_row, _] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(margin), Constraint::Length(height), Constraint::Min(0)])
        .areas(area);
    let [_, popup, _] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .areas(middle_row);
    popup
}
