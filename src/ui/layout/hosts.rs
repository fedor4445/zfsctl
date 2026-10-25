//! The Hosts table: which columns fit, and the host panel.
#![allow(clippy::wildcard_imports)]
use super::*;

/// Which Hosts columns fit. Name and State always show; Address while State keeps its minimum width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // one independent show/hide flag per column
pub struct HostColumns {
    pub addr: bool,
    pub type_: bool,
    pub tag: bool,
    pub site: bool,
    pub used: bool,
    pub pools: bool,
    pub scrub: bool,
    pub jobs: bool,
    pub rtt: bool,
    pub os: bool,
}

pub(super) type Step = (u16, fn(&mut HostColumns));
pub(super) const USED_W: u16 = 5;
pub(super) const POOLS_W: u16 = 5;
pub(super) const SCRUB_W: u16 = 13;
pub(super) const JOBS_W: u16 = 5;
pub(super) const RTT_W: u16 = 6;
/// "Linux 6.8 (Ubuntu 24)" is 21.
pub(super) const OS_W: u16 = 22;
pub(super) const ADDR_W: u16 = 18;
pub(super) const STATE_MIN: u16 = 22;

/// The optional columns are added in the reverse of their drop order (Type, Used, Tag, Pools, Scrub, Jobs, Rtt, OS name)
/// while they fit, stopping at the first that does not, so a narrower column can never outlive a wider one that
/// goes before it. `tag_w` is the longest tag (0 = no host has one, so no Tag column).
#[cfg(test)]
pub fn fit_host_columns(inner: u16, name_w: u16, tag_w: u16) -> HostColumns {
    fit_host_columns_with_site(inner, name_w, tag_w, 0)
}

/// `fit_host_columns` with a Site column, right after Tag, when `site_w` (the longest site) is not 0.
pub fn fit_host_columns_with_site(inner: u16, name_w: u16, tag_w: u16, site_w: u16) -> HostColumns {
    let addr = inner >= 16 + STATE_MIN + ADDR_W + 2;
    let mut rest = inner.saturating_sub(name_w + 1 + STATE_MIN + if addr { ADDR_W + 1 } else { 0 });
    let mut c = HostColumns { addr, type_: false, tag: false, site: false, used: false, pools: false, scrub: false, jobs: false, rtt: false, os: false };
    let tag_cols = tag_w.clamp(3, 14);
    let site_cols = site_w.clamp(4, 14);
    let steps: [Step; 9] = [
        (TYPE_W, |c| c.type_ = true),
        (USED_W, |c| c.used = true),
        (tag_cols, |c| c.tag = true),
        (site_cols, |c| c.site = true),
        (POOLS_W, |c| c.pools = true),
        (SCRUB_W, |c| c.scrub = true),
        (JOBS_W, |c| c.jobs = true),
        (RTT_W, |c| c.rtt = true),
        (OS_W, |c| c.os = true),
    ];
    for (i, (w, set)) in steps.into_iter().enumerate() {
        if (i == 2 && tag_w == 0) || (i == 3 && site_w == 0) {
            continue;
        }
        if rest < w + 1 {
            break;
        }
        rest -= w + 1;
        set(&mut c);
    }
    c
}

pub(super) fn draw_hosts(f: &mut Frame, app: &App, area: Rect) {
    use crate::zfs::host_health::{jobs_cell, pools_cell, scan_cell, used_cell, HostKind, HostProbe, Level};
    use ratatui::widgets::TableState;
    let theme = app.theme.clone();
    let inner = area.width.saturating_sub(2);
    // The name column fits the longest host name (so a narrow table squeezes the State text, never the name).
    let longest = app.hosts.iter().map(|h| h.chars().count()).max().unwrap_or(0);
    let name_w = u16::try_from(longest + 2).unwrap_or(16).clamp(8, 16);
    let tag_w = u16::try_from(app.host_tags.iter().map(|t| t.chars().count()).max().unwrap_or(0)).unwrap_or(14).min(14);
    let site_w = u16::try_from(app.host_sites.iter().map(|t| t.chars().count()).max().unwrap_or(0)).unwrap_or(14).min(14);
    let cols = fit_host_columns_with_site(inner, name_w, tag_w, site_w);
    let visible = app.visible_hosts();
    let rows: Vec<Row> = visible
        .iter()
        .filter_map(|&i| app.hosts.get(i).map(|name| (i, name)))
        .map(|(i, name)| {
            let assessment = app.host_assessment(i);
            let (marker_style, name_style, state_style) = match assessment.level {
                Level::Ok => (theme.ok, Style::default(), Style::default()),
                Level::Warning => (theme.warn, theme.warn, theme.warn),
                Level::Critical | Level::Disconnected => (theme.err, theme.err, theme.err),
                Level::Probing => (theme.dim(), theme.dim(), theme.dim()),
            };
            let state = app.host_probes.get(name);
            let reached = match state.and_then(|s| s.last.as_ref()) {
                Some(HostProbe::Reached { rtt_ms, pools, .. }) => Some((*rtt_ms, pools.as_slice())),
                _ => None,
            };
            let mut cells = vec![Cell::from(Line::from(vec![Span::styled(if app.marked_hosts.contains(name) { "+ " } else { "● " }, marker_style), Span::styled(name.clone(), name_style)]))];
            if cols.addr {
                cells.push(Cell::from(app.host_address(i).to_string()));
            }
            cells.push(Cell::from(Span::styled(assessment.reason, state_style)));
            if cols.type_ {
                let (word, style) = match state.and_then(|s| s.kind.as_ref()) {
                    Some(k @ HostKind::Vm(_)) => (k.word(), theme.info),
                    Some(k @ HostKind::Jail) => (k.word(), theme.alt),
                    Some(k) => (k.word(), Style::default()),
                    None => ("-", Style::default()),
                };
                cells.push(Cell::from(Span::styled(word, style)));
            }
            if cols.os {
                cells.push(Cell::from(Span::styled(state.and_then(|s| s.os_name.clone()).unwrap_or_else(|| "-".to_string()), theme.dim())));
            }
            if cols.tag {
                cells.push(Cell::from(app.host_tag(i).to_string()));
            }
            if cols.site {
                cells.push(Cell::from(app.host_site(i).to_string()));
            }
            if cols.used {
                let (text, level) = reached.map_or(("-".to_string(), Level::Ok), |(_, pools)| used_cell(pools));
                let style = match level {
                    Level::Critical => theme.err,
                    Level::Warning => theme.warn,
                    _ => Style::default(),
                };
                cells.push(Cell::from(Span::styled(text, style)));
            }
            if cols.pools {
                cells.push(Cell::from(reached.map_or("-".to_string(), |(_, pools)| pools_cell(pools))));
            }
            if cols.scrub {
                cells.push(Cell::from(reached.map_or("-".to_string(), |(_, pools)| scan_cell(pools))));
            }
            if cols.jobs {
                let (failed, total) = app.host_jobs_counts(name);
                let (text, bad) = jobs_cell(failed, total);
                cells.push(Cell::from(Span::styled(text, if bad { theme.err } else { Style::default() })));
            }
            if cols.rtt {
                cells.push(Cell::from(reached.map_or("-".to_string(), |(rtt, _)| format!("{rtt}ms"))));
            }
            let row = Row::new(cells);
            if i == app.selected_host { row.style(theme.selected_row()) } else { row }
        })
        .collect();
    let mut widths = vec![Constraint::Length(name_w)];
    let mut headers = vec!["Name"];
    if cols.addr {
        widths.push(Constraint::Length(ADDR_W));
        headers.push("Address");
    }
    widths.push(Constraint::Min(8));
    headers.push("State");
    for (on, w, h) in [(cols.type_, TYPE_W, "Type"), (cols.os, OS_W, "OS"), (cols.tag, tag_w.max(3), "Tag"), (cols.site, site_w.max(4), "Site"), (cols.used, USED_W, "Used"), (cols.pools, POOLS_W, "Pools"), (cols.scrub, SCRUB_W, "Scrub"), (cols.jobs, JOBS_W, "Jobs"), (cols.rtt, RTT_W, "Rtt")] {
        if on {
            widths.push(Constraint::Length(w));
            headers.push(h);
        }
    }
    let table_area = if app.hosts.len() == 1 {
        let [table_part, hint_part] = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(0), Constraint::Length(4)]).areas(area);
        let hint = Paragraph::new("Only this host is configured. Press 'n' to add another host.".to_string()).wrap(Wrap { trim: false }).style(theme.dim());
        f.render_widget(hint, hint_part);
        table_part
    } else {
        area
    };
    let title = if app.host_filter.is_empty() && !app.host_filter_editing {
        "Hosts".to_string()
    } else {
        format!("Hosts (filter: {}{} — {}/{})", app.host_filter, if app.host_filter_editing { "_" } else { "" }, visible.len(), app.hosts.len())
    };
    let table = Table::new(rows, widths)
        .header(theme.header_row(headers))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title));
    // The offset follows the selection, so the selected host stays on screen however many hosts there are.
    let mut state = TableState::default().with_selected(visible.iter().position(|i| *i == app.selected_host));
    f.render_stateful_widget(table, table_area, &mut state);
    if visible.is_empty() && table_area.height > 3 {
        let line = Rect { x: table_area.x + 1, y: table_area.y + 2, width: table_area.width.saturating_sub(2), height: 1 };
        f.render_widget(Paragraph::new("no host matches").style(theme.dim()), line);
    }
}
