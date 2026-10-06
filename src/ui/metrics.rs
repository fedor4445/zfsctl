//! The bottom health/performance panel: state (a rolling I/O history plus
//! the latest slow facts) and its rendering.

use crate::error::Result;
use crate::ui::theme::Theme;
use crate::zfs::disk::human_size;
use crate::zfs::metrics::{DatasetSpace, MetricsTarget, PoolFacts, PoolIo, TargetSample};
use crate::zfs::pool::PoolHealth;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Sparkline};
use ratatui::Frame;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Samples kept per pool; wider than any terminal's graph column.
const HISTORY: usize = 240;
/// Gap between the end of one sample and the start of the next. A sample
/// itself takes about a second (the iostat window).
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// The panel is dropped on terminals whose body is shorter than this, so
/// the lists above it keep a usable height.
pub const MIN_BODY_HEIGHT: u16 = 14;

#[derive(Default)]
pub struct MetricsView {
    target: Option<MetricsTarget>,
    io_history: VecDeque<PoolIo>,
    facts: Option<PoolFacts>,
    space: Option<DatasetSpace>,
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
        let same_pool = matches!((&self.target, &wanted), (Some(a), Some(b)) if a.pool() == b.pool());
        if !same_pool {
            self.io_history.clear();
            self.facts = None;
        }
        self.space = None;
        self.error = None;
        self.last_finished = None;
        self.target = wanted;
    }

    pub fn target(&self) -> Option<&MetricsTarget> {
        self.target.as_ref()
    }

    /// Whether a new sample should start now.
    pub fn due(&self, now: Instant) -> bool {
        self.target.is_some() && !self.in_flight && self.last_finished.is_none_or(|t| now.duration_since(t) >= SAMPLE_INTERVAL)
    }

    pub fn begin(&mut self) {
        self.in_flight = true;
    }

    /// Takes a finished sample. One for an object the panel has since moved off is dropped.
    pub fn apply(&mut self, target: &MetricsTarget, result: Result<TargetSample>, now: Instant) {
        self.in_flight = false;
        self.last_finished = Some(now);
        if self.target.as_ref() != Some(target) {
            return;
        }
        match result {
            Ok(sample) => {
                if self.io_history.len() == HISTORY {
                    self.io_history.pop_front();
                }
                self.io_history.push_back(sample.pool.io);
                self.facts = Some(sample.pool.facts);
                self.space = sample.space;
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

pub fn draw(f: &mut Frame, view: &MetricsView, health: Option<&PoolHealth>, theme: &Theme, area: Rect) {
    let title = match &view.target {
        Some(MetricsTarget::Pool(p)) => format!("Metrics — {p}"),
        Some(MetricsTarget::Dataset(d)) => format!("Metrics — {d} (I/O of pool {})", view.target.as_ref().map_or("", |t| t.pool())),
        None => "Metrics".to_string(),
    };
    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(false)).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if let Some(err) = &view.error {
        let [msg, rest] = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
        f.render_widget(Paragraph::new(format!("metrics unavailable: {err}")).style(theme.err), msg);
        draw_body(f, view, health, theme, rest);
    } else {
        draw_body(f, view, health, theme, inner);
    }
}

fn draw_body(f: &mut Frame, view: &MetricsView, health: Option<&PoolHealth>, theme: &Theme, area: Rect) {
    let (Some(facts), Some(last)) = (&view.facts, view.io_history.back()) else {
        f.render_widget(Paragraph::new("sampling…").style(theme.dim()), area);
        return;
    };
    let [read, write, text] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(30), Constraint::Percentage(40)])
        .areas(area);
    let reads: Vec<u64> = view.io_history.iter().map(|s| s.read_bps).collect();
    let writes: Vec<u64> = view.io_history.iter().map(|s| s.write_bps).collect();
    draw_rate(f, read, "Read ", &reads, last.read_ops, last.read_latency_ns, theme.info);
    draw_rate(f, write, "Write", &writes, last.write_ops, last.write_latency_ns, theme.warn);
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
        Ok(TargetSample { pool: PoolSample { io, facts: PoolFacts::default() }, space: None })
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
    fn a_result_for_an_object_we_moved_off_is_dropped() {
        let now = Instant::now();
        let mut v = MetricsView::default();
        v.retarget(Some(pool("tank")));
        v.begin();
        v.retarget(Some(pool("other")));
        v.apply(&pool("tank"), sample(9), now);
        assert!(v.io_history.is_empty());
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
