//! The bottom health/performance panel: state (a rolling I/O history plus
//! the latest slow facts) and its rendering.

use crate::error::Result;
use crate::ui::theme::Theme;
use crate::zfs::disk::human_size;
use crate::zfs::disk::{DiskInfo, DiskMedia, DiskMembership};
use crate::zfs::disk_detail::DiskLive;
use crate::zfs::host::HostSummary;
use crate::zfs::host_health::ScanSummary;
use crate::zfs::snapshot_detail::SnapshotDetail;
use crate::zfs::metrics::{DatasetSpace, MetricsTarget, PoolFacts, PoolIo, TargetSample};
use crate::zfs::pool::PoolHealth;
use crate::zfs::share::Share;
use crate::zfs::share_detail::ShareLive;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Sparkline, SparklineBar};
use ratatui::Frame;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Samples kept per pool; wider than any terminal's graph column.
const HISTORY: usize = 240;
/// Gap between the end of one sample and the start of the next. A sample
/// itself takes about a second (the iostat window).
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// A snapshot's numbers change slowly and each refresh runs four commands.
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(10);
/// The panel is dropped on terminals whose body is shorter than this, so
/// the lists above it keep a usable height.
pub const MIN_BODY_HEIGHT: u16 = 14;

#[derive(Default)]
pub struct MetricsView {
    target: Option<MetricsTarget>,
    io_history: VecDeque<PoolIo>,
    facts: Option<PoolFacts>,
    space: Option<DatasetSpace>,
    host: Option<HostSummary>,
    snapshot: Option<SnapshotDetail>,
    disk: Option<DiskLive>,
    share: Option<ShareLive>,
    error: Option<String>,
    in_flight: bool,
    last_finished: Option<Instant>,
}

impl MetricsView {
    /// Points the panel at `wanted`, dropping whatever data belonged to a
    /// different object. I/O history survives a move between datasets of
    /// the same pool because it is the pool's.
    pub fn retarget(&mut self, wanted: Option<MetricsTarget>) {
        if self.target == wanted {
            return;
        }
        let same_pool = matches!((&self.target, &wanted), (Some(a), Some(b)) if a.pool().is_some() && a.pool() == b.pool());
        if !same_pool {
            self.io_history.clear();
            self.facts = None;
        }
        self.space = None;
        self.host = None;
        self.snapshot = None;
        self.disk = None;
        self.share = None;
        self.error = None;
        self.last_finished = None;
        self.target = wanted;
    }

    pub fn target(&self) -> Option<&MetricsTarget> {
        self.target.as_ref()
    }

    /// Whether a new sample should start now.
    pub fn due(&self, now: Instant) -> bool {
        let interval = if matches!(self.target, Some(MetricsTarget::Snapshot(_))) { SNAPSHOT_INTERVAL } else { SAMPLE_INTERVAL };
        self.target.is_some() && !self.in_flight && self.last_finished.is_none_or(|t| now.duration_since(t) >= interval)
    }

    pub fn begin(&mut self) {
        self.in_flight = true;
    }

    /// Takes a finished sample. One for an object the panel has since moved off is dropped.
    pub fn apply(&mut self, target: &MetricsTarget, result: Result<TargetSample>, now: Instant) {
        self.in_flight = false;
        if self.target.as_ref() != Some(target) {
            // Leave the timer alone: the object we moved to is due immediately.
            return;
        }
        self.last_finished = Some(now);
        match result {
            Ok(sample) => {
                // A snapshot sample carries no pool data; don't push an all-zero point.
                if sample.snapshot.is_none() {
                    if self.io_history.len() == HISTORY {
                        self.io_history.pop_front();
                    }
                    self.io_history.push_back(sample.pool.io);
                    self.facts = Some(sample.pool.facts);
                }
                self.space = sample.space;
                self.host = sample.host;
                self.snapshot = sample.snapshot;
                self.disk = sample.disk;
                self.share = sample.share;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}

fn per_sec(bytes: u64) -> String {
    format!("{}/s", human_size(bytes))
}

#[allow(clippy::cast_precision_loss)] // display only
fn latency(ns: Option<u64>) -> String {
    match ns {
        None => "-".to_string(),
        Some(n) if n < 1_000 => format!("{n}ns"),
        Some(n) if n < 1_000_000 => format!("{:.0}µs", n as f64 / 1e3),
        Some(n) if n < 1_000_000_000 => format!("{:.1}ms", n as f64 / 1e6),
        Some(n) => format!("{:.1}s", n as f64 / 1e9),
    }
}

#[allow(clippy::cast_precision_loss)] // display only
fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        "-".to_string()
    } else if part > 0 && part.saturating_mul(200) < whole {
        "<1%".to_string()
    } else {
        format!("{:.0}%", part as f64 * 100.0 / whole as f64)
    }
}

fn unset_or_size(bytes: u64) -> String {
    if bytes == 0 { "-".to_string() } else { human_size(bytes) }
}

fn pool_lines(health: Option<&PoolHealth>, facts: &PoolFacts, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(match health {
        Some(h) => Line::from(vec![Span::raw("Health  "), theme.health_span(h)]),
        None => Line::from("Health  ?"),
    });
    lines.push(Line::from(format!("Used    {}  {} / {}", percent(facts.allocated, facts.size), human_size(facts.allocated), human_size(facts.size))));
    let frag = facts.fragmentation.map_or("-".to_string(), |f| format!("{f}%"));
    lines.push(Line::from(format!("Frag    {frag}   Dedup {:.2}x", facts.dedup_ratio)));
    let e = facts.errors;
    let style = if e.total() > 0 { theme.err } else { Style::default() };
    lines.push(Line::from(Span::styled(format!("Errors  R{} W{} C{}", e.read, e.write, e.cksum), style)));
    lines
}

fn disk_lines(info: Option<&DiskInfo>, live: &DiskLive, theme: &Theme) -> Vec<Line<'static>> {
    let dash = || "-".to_string();
    let Some(info) = info else {
        return vec![Line::from("Disk     (no longer listed)")];
    };
    let media = match info.media {
        DiskMedia::Ssd => "SSD".to_string(),
        DiskMedia::Hdd(rpm) => format!("HDD {rpm} rpm"),
        DiskMedia::Unknown => "type unknown".to_string(),
    };
    let membership = match &info.membership {
        DiskMembership::Available => "available (not in a pool)".to_string(),
        DiskMembership::InPool { pool, vdev } => format!("{pool} / {vdev}"),
    };
    let health = match (&live.state, live.errors) {
        (Some(state), Some(e)) => {
            let bad = state != "ONLINE" || e.total() > 0;
            Line::from(Span::styled(format!("Health   {state}   R{} W{} C{}", e.read, e.write, e.cksum), if bad { theme.err } else { Style::default() }))
        }
        _ => Line::from("Health   -"),
    };
    let busy = live.busy_percent.map_or_else(dash, |b| format!("{b:.0}%"));
    let queue = live.queue.map_or_else(dash, |q| q.to_string());
    vec![
        Line::from(format!("Disk     {}  {}  {media}", info.name, human_size(info.size))),
        Line::from(format!("Model    {}", info.model.clone().unwrap_or_else(dash))),
        Line::from(format!("Serial   {}", info.serial.clone().unwrap_or_else(dash))),
        Line::from(format!("Pool     {membership}")),
        health,
        Line::from(format!("Busy     {busy}   queue {queue}")),
    ]
}

fn share_lines(share: Option<&Share>, live: &ShareLive, space: Option<&DatasetSpace>, health: Option<&PoolHealth>, facts: &PoolFacts, theme: &Theme) -> Vec<Line<'static>> {
    let dash = || "-".to_string();
    let (kind, access) = match share {
        Some(Share::Nfs { networks, hosts, read_only, root, .. }) => {
            let mut who: Vec<String> = networks.iter().chain(hosts.iter()).cloned().collect();
            if who.is_empty() {
                who.push("everyone".to_string());
            }
            let root = root.as_deref().map_or(String::new(), |r| format!("  root={r}"));
            ("NFS".to_string(), format!("{}  {}{root}", who.join(","), if *read_only { "ro" } else { "rw" }))
        }
        Some(Share::Smb { name, valid_user, read_only, guest_ok, .. }) => (
            format!("SMB \\\\{name}"),
            format!("{}  {}", crate::ui::layout::smb_access_label(*guest_ok, valid_user.as_deref()), if *read_only { "ro" } else { "rw" }),
        ),
        None => ("(no longer listed)".to_string(), dash()),
    };
    let clients = match &live.clients {
        None => "unavailable (host won't say)".to_string(),
        Some(c) if c.is_empty() => "none connected".to_string(),
        Some(c) => format!("{}: {}", c.len(), c.join(", ")),
    };
    let space_line = space.map_or_else(dash, |s| {
        let total = s.used.saturating_add(s.available);
        format!("{}  {} of {}", percent(s.used, total), human_size(s.used), human_size(total))
    });
    let errors = facts.errors.total();
    let health_text = health.map_or("?".to_string(), |h| format!("{h:?}").to_uppercase());
    let pool_style = if errors > 0 { theme.err } else { Style::default() };
    vec![
        Line::from(format!("Share    {kind}")),
        Line::from(format!("Access   {access}")),
        Line::from(format!("Mounted  {}", live.mountpoint.clone().unwrap_or_else(dash))),
        Line::from(format!("Clients  {clients}")),
        Line::from(format!("Space    {space_line}")),
        Line::from(format!("Quota    {}   {}", space.map_or_else(dash, |s| unset_or_size(s.quota)), space.map_or_else(dash, |s| format!("{} {:.2}x", s.compression, s.compress_ratio)))),
        Line::from(Span::styled(format!("Pool     {health_text}  errs {errors}"), pool_style)),
    ]
}

fn age_text(created: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format!("{} ago", duration_text(now.saturating_sub(created)))
}

fn snapshot_lines(s: &SnapshotDetail, theme: &Theme) -> Vec<Line<'static>> {
    let dash = || "-".to_string();
    let short = s.full_name.split_once('@').map_or(s.full_name.as_str(), |(_, n)| n);
    let reclaim = s.reclaim.map_or_else(dash, human_size);
    let clones = if s.clones.is_empty() { "-".to_string() } else { s.clones.join(",") };
    let blocked_style = if s.blocked() { theme.warn } else { Style::default() };
    let count = s.siblings.len();
    let (oldest, newest) = (s.siblings.first(), s.siblings.last());
    let age = |e: Option<&crate::zfs::snapshot_detail::SnapshotEntry>| e.map_or_else(dash, |e| age_text(e.created));
    vec![
        Line::from(format!("Snapshot  {short}")),
        Line::from(format!("Created   {}", age_text(s.created))),
        Line::from(format!("Used      {}   Refer {}   Written {}", human_size(s.used), human_size(s.referenced), human_size(s.written))),
        Line::from(format!("Reclaim   {reclaim}  (freed if destroyed)")),
        Line::from(Span::styled(
            format!("Holds     {}   Clones {clones}{}", s.holds, if s.blocked() { "  (blocks destroy)" } else { "" }),
            blocked_style,
        )),
        Line::from(format!(
            "Dataset   {count} snaps, {} = {} of used",
            human_size(s.dataset_snapshots_used),
            percent(s.dataset_snapshots_used, s.dataset_used)
        )),
        Line::from(format!("Oldest    {}", age(oldest))),
        Line::from(format!("Newest    {}", age(newest))),
    ]
}

fn draw_snapshot(f: &mut Frame, s: &SnapshotDetail, theme: &Theme, area: Rect) {
    let [text, bars] = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area);
    f.render_widget(Paragraph::new(snapshot_lines(s, theme)), text);
    let [head, spark] = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(2), Constraint::Min(0)]).areas(bars);
    f.render_widget(Paragraph::new(vec![Line::from("Snapshot sizes"), Line::from("oldest → newest, this one lit")]).style(theme.dim()), head);
    // Only the newest snapshots that fit are drawn, but always the highlighted one.
    let width = usize::from(spark.width);
    let selected = s.position();
    let start = selected.map_or(0, |i| (i + 1).saturating_sub(width)).min(s.siblings.len().saturating_sub(width));
    let data: Vec<SparklineBar> = s
        .siblings
        .iter()
        .enumerate()
        .skip(start)
        .take(width)
        .map(|(i, e)| SparklineBar::from(e.used).style(Some(if Some(i) == selected { theme.info } else { theme.dim() })))
        .collect();
    f.render_widget(Sparkline::default().data(data), spark);
}

fn duration_text(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

pub struct HostPanel {
    pub system: Vec<Line<'static>>,
    pub zfs: Vec<Line<'static>>,
    pub pools: Vec<Line<'static>>,
    /// Every line in one column, most important first, for panels too small or narrow for two columns.
    pub single: Vec<Line<'static>>,
}

/// `Thu Oct  8 19:48:07 2026` without the weekday, which the date already implies.
fn short_time(when: &str) -> String {
    match when.split_once(' ') {
        Some((day, rest)) if day.len() == 3 && day.chars().all(|c| c.is_ascii_alphabetic()) => rest.trim_start().to_string(),
        _ => when.to_string(),
    }
}

fn scan_text(scan: Option<&ScanSummary>) -> (String, bool) {
    match scan {
        None => ("-".to_string(), false),
        Some(ScanSummary::Never) => ("never".to_string(), false),
        Some(ScanSummary::Running { percent: Some(p) }) => (format!("running {p}%"), false),
        Some(ScanSummary::Running { percent: None }) => ("running".to_string(), false),
        Some(ScanSummary::Canceled) => ("canceled".to_string(), false),
        Some(ScanSummary::Paused) => ("paused".to_string(), false),
        Some(ScanSummary::Finished { when, errors: 0 }) => (short_time(when), false),
        Some(ScanSummary::Finished { when, errors }) => (format!("{}, found {errors} errors", short_time(when)), true),
    }
}

/// The Hosts panel's text, split into the System and ZFS columns and the per-pool rows. A stale panel (the
/// last sample failed) is drawn dim.
pub fn host_panel(h: &HostSummary, theme: &Theme, stale: bool) -> HostPanel {
    let dash = || "-".to_string();
    let dim = |s: Style| if stale { theme.dim() } else { s };
    let (alloc, size) = h.capacity();
    let unhealthy = h.unhealthy();
    let pools = if unhealthy == 0 { format!("{} (all ONLINE)", h.pools.len()) } else { format!("{}, {unhealthy} not ONLINE", h.pools.len()) };
    let arc = h.arc.as_ref().map_or_else(dash, |a| {
        let hit = a.hit_percent().map_or_else(dash, |p| format!("{p:.1}%"));
        format!("{} / {}  hit {hit}", human_size(a.size), human_size(a.max))
    });
    // Cores and usage lead (they must stay visible); the long model name follows on its own line.
    let cpu = h.cpu.as_ref().map_or_else(dash, |c| format!("{} cores{}", c.cores, c.percent.map_or_else(String::new, |p| format!("  {p:.0}%"))));
    let memory = h.memory.as_ref().map_or_else(dash, |m| format!("{}  free {}  wired {}", human_size(m.physical), human_size(m.free), human_size(m.wired)));
    let swap = h.swap.as_ref().map_or_else(|| "none".to_string(), |s| format!("{} / {}", human_size(s.used), human_size(s.total)));
    let status = h.status.clone().unwrap_or_else(dash);
    let status_style = if status.contains("healthy") { Style::default() } else { theme.warn };
    let line = |style: Style, text: String| Line::from(Span::styled(text, dim(style)));
    let os = line(Style::default(), format!("OS      {}", h.os.clone().unwrap_or_else(dash)));
    let up = line(Style::default(), format!("Up      {}   rtt {}", h.uptime_secs.map_or_else(dash, duration_text), h.rtt_ms.map_or_else(dash, |ms| format!("{ms}ms"))));
    let load = line(Style::default(), format!("Load    {}", h.load.map_or_else(dash, |[a, b, c]| format!("{a:.2} {b:.2} {c:.2}"))));
    let cpu_lines: Vec<Line<'static>> = std::iter::once(line(Style::default(), format!("CPU     {cpu}")))
        .chain(h.cpu.as_ref().map(|c| line(Style::default(), format!("        {}", c.model))))
        .collect();
    let memory = line(Style::default(), format!("Memory  {memory}"));
    let swap = line(Style::default(), format!("Swap    {swap}"));
    let zfs_version = line(Style::default(), format!("ZFS     {}", h.zfs_version.clone().unwrap_or_else(dash)));
    let pools_line = line(if unhealthy == 0 { Style::default() } else { theme.err }, format!("Pools   {pools}"));
    let space = line(Style::default(), format!("Space   {}  {} / {}", percent(alloc, size), human_size(alloc), human_size(size)));
    let arc = line(Style::default(), format!("ARC     {arc}"));
    let l2arc: Option<Line<'static>> = h.l2arc.as_ref().map(|l2| {
        let hit = l2.hit_percent().map_or_else(dash, |p| format!("{p:.1}%"));
        line(Style::default(), format!("L2ARC   {}  hit {hit}", human_size(l2.size)))
    });
    let status_line = line(status_style, format!("Status  {status}"));
    let rows: Vec<Line<'static>> = h
        .pool_rows
        .iter()
        .map(|p| {
            let (scrub, scrub_bad) = scan_text(p.scan.as_ref());
            let frag = p.frag_percent.map_or_else(dash, |f| format!("{f}%"));
            let bad = p.health != PoolHealth::Online || p.errors.is_some_and(|e| e > 0) || scrub_bad;
            let health = format!("{:?}", p.health).to_uppercase();
            line(if bad { theme.err } else if p.capacity_percent >= 80 { theme.warn } else { Style::default() }, format!("{:<12} {:>3}% frag {:>3} {:<8} scrub {scrub}", p.name, p.capacity_percent, frag, health))
        })
        .collect();
    let mut single = vec![pools_line.clone(), status_line.clone()];
    single.extend(rows.iter().cloned());
    single.push(space.clone());
    single.push(arc.clone());
    single.extend(l2arc.iter().cloned());
    single.extend(cpu_lines.iter().cloned());
    single.extend([memory.clone(), swap.clone(), load.clone(), up.clone(), os.clone(), zfs_version.clone()]);
    let mut system = vec![os, up, load];
    system.extend(cpu_lines);
    system.extend([memory, swap]);
    let mut zfs = vec![zfs_version, pools_line, space, arc];
    zfs.extend(l2arc);
    zfs.push(status_line);
    HostPanel { system, zfs, pools: rows, single }
}

/// Draws the host panel: the two graphs (20% each) and the text (60%): System and ZFS side by side when the
/// text is wide enough, stacked otherwise, then one row per pool. Pool rows that do not fit are replaced by a
/// final "+N more pool rows" line, never clipped silently.
/// The panel's three areas: the read graph, the write graph and the text, with a gutter between each so a graph
/// never touches the neighbouring text.
fn panel_areas(area: Rect) -> [Rect; 3] {
    Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(20), Constraint::Percentage(20), Constraint::Percentage(60)]).spacing(3).areas(area)
}

/// `line` cut to `width` columns, ending in an ellipsis when something was dropped, so a long value (a CPU model
/// name) never runs into the column beside it.
fn clip_line(line: &Line<'static>, width: u16) -> Line<'static> {
    let width = usize::from(width);
    if line.width() <= width {
        return line.clone();
    }
    let mut left = width.saturating_sub(1);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for span in &line.spans {
        let take: String = span.content.chars().take(left).collect();
        left -= take.chars().count();
        spans.push(Span::styled(take, span.style));
        if left == 0 {
            break;
        }
    }
    spans.push(Span::raw("…"));
    Line::from(spans)
}

fn clip_lines(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    lines.iter().map(|l| clip_line(l, width)).collect()
}

fn draw_host_panel(f: &mut Frame, view: &MetricsView, last: &PoolIo, host: &HostSummary, stale: bool, theme: &Theme, area: Rect) {
    let mut panel = host_panel(host, theme, stale);
    // Below this width the graphs would leave the text too narrow to read: drop them and keep the rates as a line.
    let text = if area.width < 90 {
        let io = Line::raw(format!("I/O     read {}  write {}", per_sec(last.read_bps), per_sec(last.write_bps)));
        panel.single.insert(0, io.clone());
        panel.system.insert(0, io);
        area
    } else {
        let [read, write, text] = panel_areas(area);
        let reads: Vec<u64> = view.io_history.iter().map(|s| s.read_bps).collect();
        let writes: Vec<u64> = view.io_history.iter().map(|s| s.write_bps).collect();
        draw_rate(f, read, "Read ", &reads, last.read_ops, last.read_latency_ns, theme.info);
        draw_rate(f, write, "Write", &writes, last.write_ops, last.write_latency_ns, theme.warn);
        text
    };
    let height = |lines: &[Line]| u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let columns = height(&panel.system).max(height(&panel.zfs));
    let rows_need = u16::from(!panel.pools.is_empty());
    if text.width >= 72 && text.height >= columns.saturating_add(rows_need) {
        // System and ZFS side by side, the pool rows below.
        let [left, right] = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(50), Constraint::Percentage(50)]).spacing(2).areas(Rect { height: columns, ..text });
        f.render_widget(Paragraph::new(clip_lines(&panel.system, left.width)), left);
        f.render_widget(Paragraph::new(clip_lines(&panel.zfs, right.width)), right);
        draw_pool_rows(f, &panel.pools, Rect { y: text.y + columns, height: text.height - columns, ..text });
    } else {
        // One column, most important lines first; whatever does not fit is counted, never cut silently.
        let room = usize::from(text.height);
        let mut shown: Vec<Line<'static>> = panel.single.iter().take(room).cloned().collect();
        if panel.single.len() > room && room > 0 {
            shown.truncate(room - 1);
            shown.push(Line::raw(format!("+{} more lines: enlarge the terminal", panel.single.len() - (room - 1))));
        }
        f.render_widget(Paragraph::new(clip_lines(&shown, text.width)), text);
    }
}

fn draw_pool_rows(f: &mut Frame, rows: &[Line<'static>], area: Rect) {
    let room = usize::from(area.height);
    let mut shown: Vec<Line<'static>> = rows.iter().take(room).cloned().collect();
    if rows.len() > room && room > 0 {
        let hidden = rows.len() - (room - 1);
        shown.truncate(room - 1);
        shown.push(Line::raw(format!("+{hidden} more pool row{}: enlarge the terminal", if hidden == 1 { "" } else { "s" })));
    }
    f.render_widget(Paragraph::new(shown), area);
}

fn dataset_lines(s: &DatasetSpace) -> Vec<Line<'static>> {
    let total = s.used.saturating_add(s.available);
    vec![
        Line::from(format!("Used    {}  {} of {}", percent(s.used, total), human_size(s.used), human_size(total))),
        Line::from(format!("Refer   {}   Logical {}", human_size(s.referenced), human_size(s.logical_used))),
        Line::from(format!("Data    {}   Snaps {}", human_size(s.by_dataset), human_size(s.by_snapshots))),
        Line::from(format!("Kids    {}   Refres {}", human_size(s.by_children), human_size(s.by_refreservation))),
        Line::from(format!("Compr   {} {:.2}x   Rec {}", s.compression, s.compress_ratio, unset_or_size(s.recordsize))),
        Line::from(format!("Quota   {}   Ref {}", unset_or_size(s.quota), unset_or_size(s.refquota))),
    ]
}

fn draw_rate(f: &mut Frame, area: Rect, label: &str, history: &[u64], ops: u64, lat: Option<u64>, style: Style) {
    let [head, spark] = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(2), Constraint::Min(0)]).areas(area);
    let now = history.last().copied().unwrap_or(0);
    let text = vec![Line::from(format!("{label} {}", per_sec(now))), Line::from(format!("{ops} ops  lat {}", latency(lat)))];
    f.render_widget(Paragraph::new(text), head);
    let width = usize::from(spark.width);
    let tail = history.get(history.len().saturating_sub(width)..).unwrap_or_default();
    f.render_widget(Sparkline::default().data(tail).style(style), spark);
}

/// What the panel needs from the rest of the app besides its own state.
pub struct Context<'a> {
    pub host_name: &'a str,
    /// Health of the pool the target belongs to.
    pub health: Option<&'a PoolHealth>,
    /// The listed disk, for a disk target.
    pub disk: Option<&'a DiskInfo>,
    /// The listed share, for a share target.
    pub share: Option<&'a Share>,
}

pub fn draw(f: &mut Frame, view: &MetricsView, ctx: &Context, theme: &Theme, area: Rect) {
    let title = match &view.target {
        Some(MetricsTarget::Pool(p)) => format!("Metrics — {p}"),
        Some(MetricsTarget::Dataset(d)) => format!("Metrics — {d} (I/O of pool {})", view.target.as_ref().and_then(MetricsTarget::pool).unwrap_or("")),
        Some(MetricsTarget::Snapshot(n)) => format!("Metrics — {n}"),
        Some(MetricsTarget::Disk { name, .. }) => format!("Metrics — disk {name}"),
        Some(MetricsTarget::Share { dataset, smb_name }) => match smb_name {
            Some(n) => format!("Metrics — SMB share \\\\{n} ({dataset})"),
            None => format!("Metrics — NFS share {dataset}"),
        },
        Some(MetricsTarget::Host) => format!("Metrics — host {} (all pools){}", ctx.host_name, if view.error.is_some() && view.host.is_some() { " (stale)" } else { "" }),
        None => "Metrics".to_string(),
    };
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(false)).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if let Some(err) = &view.error {
        let [msg, rest] = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
        f.render_widget(Paragraph::new(format!("metrics unavailable: {err}")).style(theme.err), msg);
        draw_body(f, view, ctx, theme, rest);
    } else {
        draw_body(f, view, ctx, theme, inner);
    }
}

fn draw_body(f: &mut Frame, view: &MetricsView, ctx: &Context, theme: &Theme, area: Rect) {
    let (health, disk) = (ctx.health, ctx.disk);
    if let Some(snap) = &view.snapshot {
        draw_snapshot(f, snap, theme, area);
        return;
    }
    if let (Some(live), Some(last)) = (&view.disk, view.io_history.back()) {
        draw_graphs_and_text(f, view, last, disk_lines(disk, live, theme), theme, area);
        return;
    }
    if let (Some(host), Some(last)) = (&view.host, view.io_history.back()) {
        draw_host_panel(f, view, last, host, view.error.is_some(), theme, area);
        return;
    }
    let (Some(facts), Some(last)) = (&view.facts, view.io_history.back()) else {
        let text = if view.target.is_some() { "sampling…" } else { "nothing selected" };
        f.render_widget(Paragraph::new(text).style(theme.dim()), area);
        return;
    };
    if let Some(live) = &view.share {
        draw_graphs_and_text(f, view, last, share_lines(ctx.share, live, view.space.as_ref(), health, facts, theme), theme, area);
        return;
    }
    let mut lines = match &view.space {
        Some(space) => dataset_lines(space),
        None => Vec::new(),
    };
    let pool = pool_lines(health, facts, theme);
    if lines.is_empty() {
        lines = pool;
    } else {
        // A dataset's own space needs the room; the pool's state is squeezed to one line.
        let e = facts.errors;
        let health_text = health.map_or("?".to_string(), |h| format!("{h:?}").to_uppercase());
        lines.push(Line::from(format!("Pool    {health_text}  errs {}", e.total())));
    }
    draw_graphs_and_text(f, view, last, lines, theme, area);
}

fn draw_graphs_and_text(f: &mut Frame, view: &MetricsView, last: &PoolIo, lines: Vec<Line<'static>>, theme: &Theme, area: Rect) {
    let [read, write, text] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(30), Constraint::Percentage(40)])
        .areas(area);
    let reads: Vec<u64> = view.io_history.iter().map(|s| s.read_bps).collect();
    let writes: Vec<u64> = view.io_history.iter().map(|s| s.write_bps).collect();
    draw_rate(f, read, "Read ", &reads, last.read_ops, last.read_latency_ns, theme.info);
    draw_rate(f, write, "Write", &writes, last.write_ops, last.write_latency_ns, theme.warn);
    f.render_widget(Paragraph::new(lines), text);
}

#[cfg(test)]
pub(crate) mod tests_support {
    use crate::zfs::host::{CpuInfo, HostSummary, L2arc, Memory, Swap};
    use crate::zfs::host_health::{PoolProbe, ScanSummary};
    use crate::zfs::pool::PoolHealth;

    pub fn rich_host() -> HostSummary {
        HostSummary {
            os: Some("FreeBSD 15.1-RELEASE-p4".into()),
            zfs_version: Some("2.4.2-1".into()),
            uptime_secs: Some(90_000),
            load: Some([0.1, 0.2, 0.3]),
            rtt_ms: Some(4),
            status: Some("all pools are healthy".into()),
            cpu: Some(CpuInfo { model: "Intel(R) Xeon(R) CPU E5-2660 0 @ 2.20GHz".into(), cores: 36, percent: Some(12.4) }),
            memory: Some(Memory { physical: 21_418_688_512, free: 10_000_000_000, wired: 7_000_000_000 }),
            swap: Some(Swap { total: 2_147_483_648, used: 524_288 }),
            l2arc: Some(L2arc { size: 12_884_901_888, hits: 600, misses: 400 }),
            arc: Some(crate::zfs::host::ArcStats { size: 1 << 30, max: 2 << 30, hits: 90, misses: 10 }),
            pools: vec![
                crate::zfs::pool::Pool { name: "scratch".into(), size: 100, allocated: 42, free: 58, health: PoolHealth::Online },
                crate::zfs::pool::Pool { name: "tank".into(), size: 100, allocated: 91, free: 9, health: PoolHealth::Degraded },
                crate::zfs::pool::Pool { name: "zroot".into(), size: 100, allocated: 24, free: 76, health: PoolHealth::Online },
            ],
            pool_rows: vec![
                PoolProbe { name: "scratch".into(), health: PoolHealth::Online, capacity_percent: 42, frag_percent: Some(39), errors: Some(0), scan: Some(ScanSummary::Finished { when: "Thu Oct  8 19:48:07 2026".into(), errors: 0 }) },
                PoolProbe { name: "tank".into(), health: PoolHealth::Degraded, capacity_percent: 91, frag_percent: None, errors: Some(3), scan: Some(ScanSummary::Running { percent: Some(34) }) },
                PoolProbe { name: "zroot".into(), health: PoolHealth::Online, capacity_percent: 24, frag_percent: Some(33), errors: Some(0), scan: Some(ScanSummary::Never) },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use crate::zfs::metrics::PoolSample;

    fn pool(name: &str) -> MetricsTarget {
        MetricsTarget::Pool(name.to_string())
    }

    #[allow(clippy::unnecessary_wraps)] // handed straight to `apply`, which takes a Result
    fn sample(read_bps: u64) -> Result<TargetSample> {
        let io = PoolIo { read_bps, ..PoolIo::default() };
        Ok(TargetSample { pool: PoolSample { io, facts: PoolFacts::default() }, space: None, host: None, snapshot: None, disk: None, share: None })
    }

    #[test]
    fn the_graphs_and_the_text_are_separated_by_a_gutter() {
        for width in [90, 120, 170, 240] {
            let [read, write, text] = panel_areas(Rect { x: 0, y: 0, width, height: 10 });
            assert!(write.x >= read.x + read.width + 2, "{width}: read and write graphs touch");
            assert!(text.x >= write.x + write.width + 2, "{width}: the write graph touches the text");
        }
    }

    #[test]
    fn a_long_line_is_cut_with_an_ellipsis_and_keeps_its_styles() {
        let theme = Theme::ansi();
        let line = Line::from(vec![Span::raw("CPU     "), Span::styled("Intel Xeon E312xx (Sandy Bridge, IBRS update)", theme.warn)]);
        let cut = clip_line(&line, 20);
        assert_eq!(cut.width(), 20);
        assert!(cut.to_string().ends_with('…') && cut.to_string().starts_with("CPU     Intel Xeon"), "{cut}");
        assert_eq!(cut.spans[1].style, theme.warn);
        let short = Line::raw("short");
        assert_eq!(clip_line(&short, 20), short);
        assert_eq!(clip_line(&line, 3).width(), 3);
        assert!(clip_line(&line, 0).width() <= 1, "a zero-width column never panics");
    }

    #[test]
    fn nothing_is_due_without_a_target_or_while_a_sample_is_running() {
        let now = Instant::now();
        let mut v = MetricsView::default();
        assert!(!v.due(now));
        v.retarget(Some(pool("tank")));
        assert!(v.due(now));
        v.begin();
        assert!(!v.due(now));
    }

    #[test]
    fn the_next_sample_waits_out_the_interval() {
        let now = Instant::now();
        let mut v = MetricsView::default();
        v.retarget(Some(pool("tank")));
        v.begin();
        v.apply(&pool("tank"), sample(1), now);
        assert!(!v.due(now));
        assert!(v.due(now + SAMPLE_INTERVAL));
    }

    #[test]
    fn history_follows_the_pool_across_datasets_but_not_across_pools() {
        let now = Instant::now();
        let mut v = MetricsView::default();
        v.retarget(Some(MetricsTarget::Dataset("tank/a".into())));
        v.apply(&MetricsTarget::Dataset("tank/a".into()), sample(5), now);
        v.retarget(Some(MetricsTarget::Dataset("tank/b".into())));
        assert_eq!(v.io_history.len(), 1, "same pool keeps its graph");
        assert!(v.space.is_none());
        v.retarget(Some(MetricsTarget::Dataset("other/a".into())));
        assert!(v.io_history.is_empty() && v.facts.is_none());
    }

    #[test]
    fn a_snapshot_sample_adds_no_io_point_and_waits_longer_for_the_next() {
        let now = Instant::now();
        let target = MetricsTarget::Snapshot("tank@a".into());
        let mut v = MetricsView::default();
        v.retarget(Some(target.clone()));
        let sample = TargetSample { pool: PoolSample::default(), space: None, host: None, snapshot: Some(SnapshotDetail::default()), disk: None, share: None };
        v.apply(&target, Ok(sample), now);
        assert!(v.io_history.is_empty() && v.snapshot.is_some());
        assert!(!v.due(now + SAMPLE_INTERVAL), "snapshots refresh every 10s, not every second");
        assert!(v.due(now + SNAPSHOT_INTERVAL));
    }

    #[test]
    fn a_host_target_never_shares_history_with_a_pool() {
        let now = Instant::now();
        let mut v = MetricsView::default();
        v.retarget(Some(MetricsTarget::Host));
        v.apply(&MetricsTarget::Host, sample(5), now);
        v.retarget(Some(pool("tank")));
        assert!(v.io_history.is_empty(), "all-pools totals must not seed one pool's graph");
    }

    #[test]
    fn a_result_for_an_object_we_moved_off_is_dropped() {
        let now = Instant::now();
        let mut v = MetricsView::default();
        v.retarget(Some(pool("tank")));
        v.begin();
        v.retarget(Some(pool("other")));
        v.apply(&pool("tank"), sample(9), now);
        assert!(v.io_history.is_empty());
        assert!(v.due(now), "a dropped result must not delay the sample for the new object");
        assert!(!v.in_flight, "the sample slot frees up either way");
    }

    #[test]
    fn a_failure_keeps_the_last_good_data_and_records_the_error() {
        let now = Instant::now();
        let mut v = MetricsView::default();
        v.retarget(Some(pool("tank")));
        v.apply(&pool("tank"), sample(7), now);
        v.apply(&pool("tank"), Err(AppError::Parse("boom".into())), now);
        assert_eq!(v.io_history.len(), 1);
        assert!(v.error.as_deref().is_some_and(|e| e.contains("boom")));
        v.apply(&pool("tank"), sample(8), now);
        assert!(v.error.is_none());
    }

    #[test]
    fn history_is_capped() {
        let now = Instant::now();
        let mut v = MetricsView::default();
        v.retarget(Some(pool("tank")));
        for i in 0..(HISTORY as u64 + 10) {
            v.apply(&pool("tank"), sample(i), now);
        }
        assert_eq!(v.io_history.len(), HISTORY);
        assert_eq!(v.io_history.front().unwrap().read_bps, 10);
    }

    #[test]
    fn latency_picks_a_readable_unit() {
        assert_eq!(latency(None), "-");
        assert_eq!(latency(Some(800)), "800ns");
        assert_eq!(latency(Some(250_000)), "250µs");
        assert_eq!(latency(Some(1_200_000)), "1.2ms");
        assert_eq!(latency(Some(2_500_000_000)), "2.5s");
    }

    use super::tests_support::rich_host;

    fn text_of(lines: &[Line]) -> String {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn the_panel_names_cpu_memory_swap_and_l2arc() {
        let panel = host_panel(&rich_host(), &Theme::ansi(), false);
        let system = text_of(&panel.system);
        for want in ["Intel(R) Xeon(R) CPU E5-2660 0 @ 2.20GHz", "36 cores", "12%", "Memory", "19.9G", "Swap", "512K / 2.0G"] {
            assert!(system.contains(want), "missing {want:?} in\n{system}");
        }
        let zfs = text_of(&panel.zfs);
        assert!(zfs.contains("L2ARC") && zfs.contains("12.0G") && zfs.contains("60.0%"), "{zfs}");
    }

    #[test]
    fn missing_figures_show_a_dash_and_l2arc_is_absent_without_a_cache_device() {
        let mut h = rich_host();
        h.cpu = None;
        h.memory = None;
        h.swap = None;
        h.l2arc = None;
        let panel = host_panel(&h, &Theme::ansi(), false);
        let all = format!("{}\n{}", text_of(&panel.system), text_of(&panel.zfs));
        assert!(!all.contains("L2ARC"), "{all}");
        assert!(all.contains("CPU     -") && all.contains("Memory  -") && all.contains("Swap    none"), "{all}");
    }

    #[test]
    fn each_pool_gets_a_row_with_its_capacity_fragmentation_health_and_last_scrub() {
        let panel = host_panel(&rich_host(), &Theme::ansi(), false);
        let rows = text_of(&panel.pools);
        for want in ["scratch", "42%", "39%", "ONLINE", "Oct  8 19:48:07 2026", "tank", "91%", "DEGRADED", "running 34%", "never"] {
            assert!(rows.contains(want), "missing {want:?} in\n{rows}");
        }
        assert!(rows.contains("tank") && rows.lines().any(|l| l.contains("tank") && l.contains(" - ")), "an unknown fragmentation is a dash:\n{rows}");
    }

    #[test]
    fn a_pool_that_is_not_online_or_has_scrub_errors_is_drawn_in_the_error_color() {
        let theme = Theme::ansi();
        let panel = host_panel(&rich_host(), &theme, false);
        let tank = panel.pools.iter().find(|l| text_of(std::slice::from_ref(l)).contains("tank")).unwrap();
        assert!(tank.spans.iter().any(|s| s.style == theme.err), "{tank:?}");
        let scratch = panel.pools.iter().find(|l| text_of(std::slice::from_ref(l)).contains("scratch")).unwrap();
        assert!(!scratch.spans.iter().any(|s| s.style == theme.err));
    }

    #[test]
    fn a_stale_panel_is_drawn_dim() {
        let theme = Theme::ansi();
        let fresh = host_panel(&rich_host(), &theme, false);
        let stale = host_panel(&rich_host(), &theme, true);
        assert!(stale.system.iter().all(|l| l.spans.iter().all(|s| s.style.add_modifier.contains(ratatui::style::Modifier::DIM) || s.style == theme.dim())), "{:?}", stale.system);
        assert!(!fresh.system.iter().all(|l| l.spans.iter().all(|s| s.style == theme.dim())));
    }
}
