//! The bottom health/performance panel: state (a rolling I/O history plus
//! the latest slow facts) and its rendering.

use crate::error::Result;
use crate::ui::theme::Theme;
use crate::zfs::disk::human_size;
use crate::zfs::disk::{DiskInfo, DiskMedia, DiskMembership};
use crate::zfs::disk_detail::DiskLive;
use crate::zfs::host::HostSummary;
use crate::zfs::snapshot_detail::SnapshotDetail;
use crate::zfs::metrics::{DatasetSpace, MetricsTarget, PoolFacts, PoolIo, TargetSample};
use crate::zfs::pool::PoolHealth;
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

fn host_lines(h: &HostSummary, theme: &Theme) -> Vec<Line<'static>> {
    let dash = || "-".to_string();
    let (alloc, size) = h.capacity();
    let unhealthy = h.unhealthy();
    let pools = if unhealthy == 0 { format!("{} (all ONLINE)", h.pools.len()) } else { format!("{}, {unhealthy} not ONLINE", h.pools.len()) };
    let pools_style = if unhealthy == 0 { Style::default() } else { theme.err };
    let arc = h.arc.as_ref().map_or_else(dash, |a| {
        let hit = a.hit_percent().map_or_else(dash, |p| format!("{p:.1}%"));
        format!("{} / {}  hit {hit}", human_size(a.size), human_size(a.max))
    });
    let load = h.load.map_or_else(dash, |[a, b, c]| format!("{a:.2} {b:.2} {c:.2}"));
    let up = h.uptime_secs.map_or_else(dash, duration_text);
    let rtt = h.rtt_ms.map_or_else(dash, |ms| format!("{ms}ms"));
    let status = h.status.clone().unwrap_or_else(dash);
    let status_style = if status.contains("healthy") { Style::default() } else { theme.warn };
    vec![
        Line::from(format!("OS      {}", h.os.clone().unwrap_or_else(dash))),
        Line::from(format!("ZFS     {}", h.zfs_version.clone().unwrap_or_else(dash))),
        Line::from(format!("Up      {up}   rtt {rtt}")),
        Line::from(Span::styled(format!("Pools   {pools}"), pools_style)),
        Line::from(format!("Space   {}  {} / {}", percent(alloc, size), human_size(alloc), human_size(size))),
        Line::from(format!("ARC     {arc}")),
        Line::from(format!("Load    {load}")),
        Line::from(Span::styled(format!("Status  {status}"), status_style)),
    ]
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

pub fn draw(f: &mut Frame, view: &MetricsView, host_name: &str, health: Option<&PoolHealth>, disk: Option<&DiskInfo>, theme: &Theme, area: Rect) {
    let title = match &view.target {
        Some(MetricsTarget::Pool(p)) => format!("Metrics — {p}"),
        Some(MetricsTarget::Dataset(d)) => format!("Metrics — {d} (I/O of pool {})", view.target.as_ref().and_then(MetricsTarget::pool).unwrap_or("")),
        Some(MetricsTarget::Snapshot(n)) => format!("Metrics — {n}"),
        Some(MetricsTarget::Disk { name, .. }) => format!("Metrics — disk {name}"),
        Some(MetricsTarget::Host) => format!("Metrics — host {host_name} (all pools)"),
        None => "Metrics".to_string(),
    };
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(false)).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if let Some(err) = &view.error {
        let [msg, rest] = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
        f.render_widget(Paragraph::new(format!("metrics unavailable: {err}")).style(theme.err), msg);
        draw_body(f, view, health, disk, theme, rest);
    } else {
        draw_body(f, view, health, disk, theme, inner);
    }
}

fn draw_body(f: &mut Frame, view: &MetricsView, health: Option<&PoolHealth>, disk: Option<&DiskInfo>, theme: &Theme, area: Rect) {
    if let Some(snap) = &view.snapshot {
        draw_snapshot(f, snap, theme, area);
        return;
    }
    if let (Some(live), Some(last)) = (&view.disk, view.io_history.back()) {
        draw_graphs_and_text(f, view, last, disk_lines(disk, live, theme), theme, area);
        return;
    }
    if let (Some(host), Some(last)) = (&view.host, view.io_history.back()) {
        draw_graphs_and_text(f, view, last, host_lines(host, theme), theme, area);
        return;
    }
    let (Some(facts), Some(last)) = (&view.facts, view.io_history.back()) else {
        f.render_widget(Paragraph::new("sampling…").style(theme.dim()), area);
        return;
    };
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
        Ok(TargetSample { pool: PoolSample { io, facts: PoolFacts::default() }, space: None, host: None, snapshot: None, disk: None })
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
        let sample = TargetSample { pool: PoolSample::default(), space: None, host: None, snapshot: Some(SnapshotDetail::default()), disk: None };
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
}
