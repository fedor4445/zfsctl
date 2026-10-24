//! The Dashboard's rules: facts the app already holds go in, rows and levels come out. Nothing here draws, reads a file or
//! runs a command, so every rule and every boundary is a plain unit test.

use crate::config::Thresholds;
use crate::daemon::history::RunResult;
use crate::jobs;
use crate::ui::alerts::AlertsView;
use crate::ui::cloud::{self, Caps, Fill, Size};
use crate::ui::jobs::{age_text, DaemonState, JobsView};
use crate::ui::state::Section;
use crate::zfs::host_health::{scrub_age_days, used_cell, Assessment, HostProbe, Level as HealthLevel, PoolProbe, ScanSummary};
use crate::zfs::host_resources::{cpu_percent_between, memory_used_percent, swap_used_percent, Resources};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// How worried to be about one reading. `Unknown` means the zfsctl does not know (no answer yet, nothing to compare with): it is
/// never shown as fine. The order is the order of concern, so the worst of several is simply the maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Fine,
    Unknown,
    Watch,
    Issue,
}

impl Level {
    #[cfg(test)]
    pub fn rank(self) -> u8 {
        match self {
            Level::Fine => 0,
            Level::Unknown => 1,
            Level::Watch => 2,
            Level::Issue => 3,
        }
    }
}

/// One value of the host table, with how to colour it.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub text: String,
    pub level: Level,
    /// A percentage worth a small gauge (space, memory, CPU).
    pub gauge: Option<u8>,
    /// What the Needs attention list says about this cell when it is not fine (`zroot never scrubbed`).
    pub note: Option<String>,
}

impl Cell {
    fn new(text: impl Into<String>, level: Level) -> Cell {
        Cell { text: text.into(), level, gauge: None, note: None }
    }

    fn unknown() -> Cell {
        Cell::new("?", Level::Unknown)
    }

    fn none() -> Cell {
        Cell::new("-", Level::Fine)
    }

    fn with_gauge(mut self, percent: u8) -> Cell {
        self.gauge = Some(percent);
        self
    }

    fn with_note(mut self, note: impl Into<String>) -> Cell {
        self.note = Some(note.into());
        self
    }
}

/// One host of the table.
#[derive(Debug, Clone, PartialEq)]
pub struct HostRow {
    pub name: String,
    pub state: Cell,
    pub pools: Cell,
    pub space: Cell,
    pub memory: Cell,
    pub cpu: Cell,
    pub load: Cell,
    pub scrub: Cell,
    pub errors: Cell,
    /// The ARC hit rate since boot, and the L2ARC's size and hit rate: plain text, never judged.
    pub arc: Cell,
    pub l2arc: Cell,
    /// The OS name, plain text (`-` until read); never judged.
    pub os: Cell,
    /// What `assess()` itself says when it is not fine (unreachable, `tank DEGRADED`, `job failed`...).
    pub base: Option<(Level, String)>,
}

/// Everything the rules need to know about one host.
pub struct HostInput<'a> {
    pub name: &'a str,
    pub assessment: Assessment,
    pub probe: Option<&'a HostProbe>,
    /// The newest resource reading and the one before it.
    pub resources: Option<(&'a Resources, Option<&'a Resources>)>,
    pub utc_offset_secs: Option<i32>,
    /// The short OS name read by the first probe (`Linux 6.8 (Ubuntu 24)`).
    pub os_name: Option<&'a str>,
    /// The host's `tag` from `config.toml` (blank when it has none): the group it rolls up into.
    pub tag: &'a str,
}

fn level_of(health: HealthLevel) -> Level {
    match health {
        HealthLevel::Probing => Level::Unknown,
        HealthLevel::Disconnected | HealthLevel::Critical => Level::Issue,
        HealthLevel::Warning => Level::Watch,
        HealthLevel::Ok => Level::Fine,
    }
}

fn by_threshold(value: u8, (warn, crit): (u8, u8)) -> Level {
    if value >= crit {
        Level::Issue
    } else if value >= warn {
        Level::Watch
    } else {
        Level::Fine
    }
}

fn space_cell(pools: &[PoolProbe]) -> Cell {
    let Some(fullest) = pools.iter().max_by_key(|p| p.capacity_percent) else { return Cell::none() };
    let (text, level) = used_cell(pools);
    let level = level_of(level);
    let cell = Cell::new(text, level).with_gauge(fullest.capacity_percent);
    if level == Level::Fine { cell } else { cell.with_note(format!("{} {}% full", fullest.name, fullest.capacity_percent)) }
}

fn errors_cell(pools: &[PoolProbe]) -> Cell {
    if pools.is_empty() {
        return Cell::none();
    }
    let known: Vec<(&PoolProbe, u64)> = pools.iter().filter_map(|p| p.errors.map(|n| (p, n))).collect();
    if known.is_empty() {
        return Cell::unknown();
    }
    let total: u64 = known.iter().map(|(_, n)| n).sum();
    if total == 0 {
        return Cell::new("0", Level::Fine);
    }
    let named: Vec<String> = known.iter().filter(|(_, n)| *n > 0).map(|(p, n)| format!("{} {n} error{}", p.name, if *n == 1 { "" } else { "s" })).collect();
    Cell::new(total.to_string(), Level::Watch).with_note(named.join(", "))
}

/// How one pool's last scrub reads: its level, the cell text, how many days (to pick the worst) and the attention note.
fn pool_scrub(pool: &PoolProbe, utc_offset_secs: Option<i32>, t: &Thresholds, now: DateTime<Utc>) -> (Level, String, i64, Option<String>) {
    match pool.scan {
        Some(ScanSummary::Running { percent, .. }) => return (Level::Fine, percent.map_or_else(|| "running".to_string(), |p| format!("running {p}%")), -1, None),
        Some(ScanSummary::Paused { .. }) => return (Level::Fine, "paused".to_string(), -1, None),
        _ => {}
    }
    let Some(when) = pool.last_scrub.as_deref() else {
        return match pool.scan {
            None => (Level::Unknown, "?".to_string(), -1, None),
            Some(ScanSummary::Never) => (Level::Watch, "never".to_string(), i64::MAX, Some(format!("{} never scrubbed", pool.name))),
            // zpool status keeps only the latest scan, so a resilver or a cancelled scrub hides the date of the last good scrub.
            Some(_) => (Level::Watch, "unknown".to_string(), i64::MAX - 1, Some(format!("{} has no finished scrub on record (the last scan was not one)", pool.name))),
        };
    };
    let Some(days) = utc_offset_secs.and_then(|o| scrub_age_days(when, o, now)) else { return (Level::Unknown, "?".to_string(), -1, None) };
    let level = if days >= i64::from(t.scrub_days.1) {
        Level::Issue
    } else if days >= i64::from(t.scrub_days.0) {
        Level::Watch
    } else {
        Level::Fine
    };
    let found = match pool.scan {
        Some(ScanSummary::Finished { errors, .. }) if errors > 0 => Some(errors),
        _ => None,
    };
    let level = if found.is_some() { level.max(Level::Watch) } else { level };
    let note = match (found, level != Level::Fine) {
        (Some(n), _) => Some(format!("{} last scrub found {n} error{}", pool.name, if n == 1 { "" } else { "s" })),
        (None, true) => Some(format!("{} last scrub {days} d ago", pool.name)),
        _ => None,
    };
    (level, if days == 0 { "today".to_string() } else { format!("{days} d ago") }, days, note)
}

fn scrub_cell(pools: &[PoolProbe], utc_offset_secs: Option<i32>, t: &Thresholds, now: DateTime<Utc>) -> Cell {
    let judged: Vec<(Level, String, i64, Option<String>)> = pools.iter().filter(|p| !t.scrub_ignore.contains(&p.name)).map(|p| pool_scrub(p, utc_offset_secs, t, now)).collect();
    let Some((level, text, _, note)) = judged.into_iter().max_by_key(|(level, _, days, _)| (*level, *days)) else { return Cell::none() };
    let cell = Cell::new(text, level);
    match note {
        Some(note) => cell.with_note(note),
        None => cell,
    }
}

fn memory_cell(r: &Resources, t: &Thresholds) -> Cell {
    let memory = memory_used_percent(r);
    let memory_level = by_threshold(memory, t.memory);
    let swap = swap_used_percent(r);
    let swap_level = swap.map_or(Level::Fine, |s| by_threshold(s, t.swap));
    let mut text = format!("{memory}%");
    if let (Some(s), true) = (swap, swap_level != Level::Fine) {
        text.push_str(&format!(" (swap {s}%)"));
    }
    let level = memory_level.max(swap_level);
    let cell = Cell::new(text, level).with_gauge(memory);
    match (memory_level != Level::Fine, swap) {
        (true, _) => cell.with_note(format!("memory {memory}%")),
        (false, Some(s)) if swap_level != Level::Fine => cell.with_note(format!("swap {s}%")),
        _ => cell,
    }
}

fn cpu_cell(resources: Option<(&Resources, Option<&Resources>)>, t: &Thresholds) -> Cell {
    let Some(percent) = resources.and_then(|(now, before)| cpu_percent_between(before?, now)) else { return Cell::unknown() };
    let level = if percent >= t.cpu_warn { Level::Watch } else { Level::Fine };
    let cell = Cell::new(format!("{percent}%"), level).with_gauge(percent);
    if level == Level::Fine { cell } else { cell.with_note(format!("cpu {percent}%")) }
}

fn load_cell(resources: Option<(&Resources, Option<&Resources>)>) -> Cell {
    match resources.and_then(|(now, _)| now.load) {
        Some([one, ..]) => Cell::new(format!("{one:.2}"), Level::Fine),
        None => Cell::unknown(),
    }
}

/// Judges one host. The state starts from `assess()` and is only ever raised by the readings, never lowered.
pub fn host_row(input: &HostInput, t: &Thresholds, now: DateTime<Utc>) -> HostRow {
    let base_level = level_of(input.assessment.level);
    let base = (base_level >= Level::Watch).then(|| (base_level, input.assessment.reason.clone()));
    let (pools, space, errors, scrub) = match input.probe {
        Some(HostProbe::Reached { pools, .. }) => (
            if pools.is_empty() { Cell::none() } else { Cell::new(pools.len().to_string(), Level::Fine) },
            space_cell(pools),
            errors_cell(pools),
            scrub_cell(pools, input.utc_offset_secs, t, now),
        ),
        _ => (Cell::unknown(), Cell::unknown(), Cell::unknown(), Cell::unknown()),
    };
    let reached = matches!(input.probe, Some(HostProbe::Reached { .. }));
    let (memory, cpu, load) = match input.resources {
        Some((newest, _)) if reached => (memory_cell(newest, t), cpu_cell(input.resources, t), load_cell(input.resources)),
        _ => (Cell::unknown(), Cell::unknown(), Cell::unknown()),
    };
    let (arc, l2arc) = match input.resources {
        Some((newest, _)) if reached => cache_cells(newest),
        _ => (Cell::unknown(), Cell::unknown()),
    };
    let worst = [&space, &memory, &cpu, &scrub, &errors].into_iter().filter(|c| c.level >= Level::Watch).max_by_key(|c| c.level);
    let state = match worst {
        Some(cell) if cell.level > base_level => Cell::new(cell.note.clone().unwrap_or_else(|| cell.text.clone()), cell.level),
        _ => Cell::new(input.assessment.reason.clone(), base_level),
    };
    HostRow { name: input.name.to_string(), state, os: input.os_name.map_or_else(Cell::none, |n| Cell::new(n, Level::Fine)), pools, space, memory, cpu, load, scrub, errors, arc, l2arc, base }
}

/// One group of the Dashboard: a row that stands for every host with the same tag, and the hosts of it that need a look.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// The tag the group stands for; empty for the hosts that have none.
    pub tag: String,
    pub row: HostRow,
    /// Only the hosts whose state is not fine (all of them when the groups are expanded), in the order of the configuration.
    pub members: Vec<HostRow>,
}

/// The name a group shows for hosts that have no tag.
pub const NO_TAG: &str = "(no tag)";

/// One row for a whole group: the state is the worst of its hosts and counts them (`1 issue, 2 to watch`), pools and errors add up,
/// and every other reading is the worst reading of any host (the fullest pool, the biggest memory use, the oldest scrub).
fn roll_up(name: String, members: &[&HostRow]) -> HostRow {
    let worst = |pick: fn(&HostRow) -> &Cell| -> Cell {
        let cell = members.iter().map(|m| pick(m)).max_by_key(|c| (c.level, c.gauge.unwrap_or(0)));
        cell.cloned().map_or_else(Cell::none, |mut c| {
            c.note = None;
            c
        })
    };
    let total = |pick: fn(&HostRow) -> &Cell| -> Cell {
        let numbers: Option<Vec<u64>> = members.iter().map(|m| pick(m).text.split_whitespace().next().and_then(|t| t.parse().ok())).collect();
        match numbers {
            Some(n) => Cell::new(n.iter().sum::<u64>().to_string(), members.iter().map(|m| pick(m).level).max().unwrap_or(Level::Fine)),
            None => worst(pick),
        }
    };
    let count = |level: Level| members.iter().filter(|m| m.state.level == level).count();
    let level = members.iter().map(|m| m.state.level).max().unwrap_or(Level::Fine);
    let mut parts = Vec::new();
    for (n, text) in [(count(Level::Issue), "issue"), (count(Level::Watch), "to watch"), (count(Level::Unknown), "unknown")] {
        if n > 0 {
            parts.push(if text == "issue" { plural(n, text) } else { format!("{n} {text}") });
        }
    }
    let state = Cell::new(if parts.is_empty() { "ok".to_string() } else { parts.join(", ") }, level);
    HostRow {
        name,
        state,
        pools: total(|h| &h.pools),
        space: worst(|h| &h.space),
        memory: worst(|h| &h.memory),
        cpu: worst(|h| &h.cpu),
        load: members.iter().filter_map(|m| m.load.text.parse::<f64>().ok().map(|v| (v, &m.load))).max_by(|a, b| a.0.total_cmp(&b.0)).map_or_else(Cell::none, |(_, c)| c.clone()),
        scrub: worst(|h| &h.scrub),
        errors: total(|h| &h.errors),
        arc: Cell::none(),
        l2arc: Cell::none(),
        os: Cell::none(),
        base: None,
    }
}

/// The hosts rolled up by tag: `None` when no host has a tag (the Dashboard then shows every host, as it always did). Untagged
/// hosts form the last group. A group with a problem comes first; inside a group only the hosts that are not fine are listed.
fn groups_of(inputs: &[HostInput], rows: &[HostRow], expanded: bool) -> Option<Vec<Group>> {
    if inputs.iter().all(|i| i.tag.trim().is_empty()) {
        return None;
    }
    let mut tags: Vec<&str> = Vec::new();
    for tag in inputs.iter().map(|i| i.tag.trim()) {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    let mut groups: Vec<(bool, Group)> = tags
        .into_iter()
        .map(|tag| {
            let members: Vec<&HostRow> = inputs.iter().zip(rows).filter(|(i, _)| i.tag.trim() == tag).map(|(_, r)| r).collect();
            let shown = if tag.is_empty() { NO_TAG } else { tag };
            let row = roll_up(format!("{shown} ({})", members.len()), &members);
            let listed = members.iter().filter(|m| expanded || m.state.level != Level::Fine).map(|m| (*m).clone()).collect();
            (tag.is_empty(), Group { tag: tag.to_string(), row, members: listed })
        })
        .collect();
    groups.sort_by(|a, b| b.1.row.state.level.cmp(&a.1.row.state.level).then(a.0.cmp(&b.0)).then_with(|| a.1.row.name.cmp(&b.1.row.name)));
    Some(groups.into_iter().map(|(_, g)| g).collect())
}

/// The ARC hit rate and the L2ARC's size and hit rate, since boot as the Hosts panel shows them; `-` where the host has no cache counters,
/// no cache device, or nothing has been read yet.
fn cache_cells(r: &Resources) -> (Cell, Cell) {
    let percent = |p: Option<f64>| p.map_or_else(|| "-".to_string(), |p| format!("{p:.1}%"));
    match r.cache {
        None => (Cell::none(), Cell::none()),
        Some(c) => {
            let l2 = if c.l2_size == 0 { "-".to_string() } else { format!("{} {}", crate::zfs::disk::human_size(c.l2_size), percent(c.l2_hit_percent())) };
            (Cell::new(percent(c.arc_hit_percent()), Level::Fine), Cell::new(l2, Level::Fine))
        }
    }
}

/// The lines a host adds to Needs attention: every cell that is not fine, plus what `assess()` itself says, unless a cell
/// already says the same (the space and error cells repeat `tank 85% full` and `tank 2 errors`). Unknown cells add nothing.
pub fn host_attention(row: &HostRow) -> Vec<(Level, String)> {
    let mut items: Vec<(Level, String)> = [&row.space, &row.memory, &row.cpu, &row.scrub, &row.errors]
        .into_iter()
        .filter(|c| c.level >= Level::Watch)
        .map(|c| (c.level, c.note.clone().unwrap_or_else(|| c.text.clone())))
        .collect();
    if let Some((level, reason)) = &row.base {
        let covered = (reason.ends_with("% full") && row.space.level >= Level::Watch) || (reason.contains(" error") && row.errors.level >= Level::Watch);
        if !covered {
            items.insert(0, (*level, reason.clone()));
        }
    }
    items
}

/// One line of a component's status.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub level: Level,
    pub text: String,
}

/// One entry of Needs attention: what is wrong, where, and which section owns it.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub level: Level,
    pub subject: String,
    pub what: String,
    pub goto: Section,
    /// The host or job to select there: a host name for Hosts, a job id for Jobs.
    pub target: Option<String>,
}

/// The whole screen, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Dashboard {
    pub verdict: Line,
    pub attention: Vec<Item>,
    /// Every unprotected dataset as its own line, whatever `[protection] dashboard` shows (the pop-up lists these).
    pub unprotected: Vec<Item>,
    /// What each of `unprotected` lacks (the same order), for the pop-up's fixes.
    pub gaps: Vec<crate::daemon::protection::Unprotected>,
    /// The line that stands for those past the first `PROTECTION_NAMED`, or `None` when all of them have a line.
    pub protection_more: Option<Item>,
    pub hosts: Vec<HostRow>,
    /// The hosts rolled up by tag, when any host has one.
    pub groups: Option<Vec<Group>>,
    pub daemon: Line,
    pub jobs: Line,
    /// Ids of the enabled jobs whose latest run failed.
    pub failed_jobs: Vec<String>,
    pub cloud: Vec<Line>,
    /// The names of the cloud lines' columns, spaced like them (empty when there are no remotes).
    pub cloud_header: String,
    pub alerts: Line,
    /// While a check runs: what to tell the operator (shown in red under the header). Not a problem, so not in `attention`.
    pub running: Option<String>,
}

/// Everything the Dashboard is built from. All of it is already in memory: building it asks nobody anything.
pub struct Inputs<'a> {
    pub hosts: Vec<HostInput<'a>>,
    pub jobs: &'a JobsView,
    pub cloud_rows: Vec<cloud::Row>,
    pub cloud_sizes: &'a HashMap<String, Size>,
    pub cloud_caps: &'a Caps,
    pub alerts: &'a AlertsView,
    pub thresholds: &'a Thresholds,
    /// How well protected a dataset must be (`[protection]`).
    pub protection: &'a crate::config::ProtectionSettings,
    /// Why `[protection]` is wrong (then `protection` is Basic's defaults), for a line of its own.
    pub protection_problem: Option<&'a str>,
    /// What the daemon's daily `smartctl` check found (`smart.json`).
    pub smart: &'a crate::daemon::smart_check::SmartFile,
    /// What the daily check found about file sharing (`shares.json`).
    pub shares: &'a crate::daemon::shares_check::SharesFile,
    /// What the storage check found: datasets near their quota (`storage.json`).
    pub storage: &'a crate::daemon::storage_check::StorageFile,
    /// The check that is running now, if any (a fresh marker).
    pub running: Option<&'a crate::daemon::checks::Marker>,
    /// List every host under its group, not only the ones that need a look.
    pub expand_groups: bool,
    pub now: DateTime<Utc>,
}

fn daemon_line(view: &JobsView, now: DateTime<Utc>) -> Line {
    match view.daemon(now) {
        DaemonState::Running { pid } => {
            let age = view.heartbeat.as_ref().map_or(0, |h| (now - h.last_tick).num_seconds().max(0));
            Line { level: Level::Fine, text: format!("running (pid {pid}), heartbeat {age}s ago") }
        }
        DaemonState::NotRunning => Line { level: Level::Issue, text: "not running: jobs will not fire and no alerts are sent".to_string() },
    }
}

const KIND_LABELS: [(&str, &str); 5] = [(jobs::KIND_SCRUB, "scrub"), (jobs::KIND_SNAPSHOT, "snapshot"), (jobs::KIND_REPLICATE, "replicate"), (jobs::KIND_BACKUP, "backup"), (jobs::KIND_MIRROR, "cloud")];

fn is_failed(view: &JobsView, job: &jobs::Job) -> bool {
    view.last_run(&job.id).is_some_and(|r| r.result == Some(RunResult::Failed))
}

/// `scrub 1/1 ok  backup 0/1 FAILED`: enabled jobs by kind, how many are not failed, and the failed ones' ids.
fn jobs_line(view: &JobsView) -> (Line, Vec<String>) {
    let enabled: Vec<&jobs::Job> = view.jobs.iter().filter(|j| j.enabled).collect();
    if enabled.is_empty() {
        return (Line { level: Level::Unknown, text: "no jobs scheduled".to_string() }, vec![]);
    }
    let mut parts = Vec::new();
    for (kind, label) in KIND_LABELS {
        let of_kind: Vec<&&jobs::Job> = enabled.iter().filter(|j| j.kind == kind).collect();
        if of_kind.is_empty() {
            continue;
        }
        let failed = of_kind.iter().filter(|j| is_failed(view, j)).count();
        let total = of_kind.len();
        parts.push(if failed > 0 { format!("{label} {}/{total} FAILED", total - failed) } else { format!("{label} {total}/{total} ok") });
    }
    let failed_jobs: Vec<String> = enabled.iter().filter(|j| is_failed(view, j)).map(|j| j.id.clone()).collect();
    let level = if failed_jobs.is_empty() { Level::Fine } else { Level::Issue };
    (Line { level, text: parts.join("  ") }, failed_jobs)
}

/// A remote's name, with its host in front unless it is the hub's, so two hosts' `b2` read differently.
fn cloud_label(row: &cloud::Row) -> String {
    if row.host == "local" { row.name.clone() } else { format!("{}:{}", row.host, row.name) }
}

/// The widths of the cloud block's columns (each as wide as its widest cell or its name), so the lines and the header line up.
struct CloudWidths {
    remote: usize,
    last: usize,
    traffic: usize,
}

const CLOUD_PROVIDER_W: usize = 16;
const CLOUD_JOBS_W: usize = 7;
const CLOUD_NEXT_W: usize = 15;

/// The line's columns, two spaces apart: Provider, Remote, Jobs, Last run, Next run, Traffic, In bucket.
fn cloud_columns(w: &CloudWidths, provider: &str, remote: &str, jobs: &str, last: &str, next: &str, traffic: &str, bucket: &str) -> String {
    format!("{provider:<CLOUD_PROVIDER_W$}  {remote:<rw$}  {jobs:<CLOUD_JOBS_W$}  {last:<lw$}  {next:<CLOUD_NEXT_W$}  {traffic:<tw$}  {bucket}", rw = w.remote, lw = w.last, tw = w.traffic).trim_end().to_string()
}

fn cloud_header(w: &CloudWidths) -> String {
    cloud_columns(w, "Provider", "Remote", "Jobs", "Last run", "Next run", "Traffic", "In bucket")
}

/// What the "Last run" cell says for a row: the last result and its age, or `-` for a remote no job uses.
fn cloud_last(row: &cloud::Row, view: &JobsView, now: DateTime<Utc>) -> String {
    if row.job_ids.is_empty() { "-".to_string() } else { crate::ui::jobs::last_result_text(cloud::last_run(row, view), now) }
}

/// The Traffic cell for a row; the arrow moves one cell a second, from the clock.
fn cloud_traffic(row: &cloud::Row, view: &JobsView, now: DateTime<Utc>) -> String {
    if row.job_ids.is_empty() { "-".to_string() } else { cloud::traffic_cell(&cloud::traffic(row, view), now.timestamp().unsigned_abs()) }
}

fn cloud_line(row: &cloud::Row, view: &JobsView, inputs: &Inputs, w: &CloudWidths) -> Line {
    let cells = cloud::cells(row, view, inputs.cloud_sizes, inputs.cloud_caps, inputs.now);
    let provider = cloud::provider_name(row.kind.as_deref());
    if row.job_ids.is_empty() {
        return Line { level: Level::Unknown, text: cloud_columns(w, &provider, &cloud_label(row), "no jobs", "-", "-", "-", "-") };
    }
    let level = if cloud::failed(row, view) {
        Level::Issue
    } else {
        match cloud::row_fill(row, inputs.cloud_sizes, inputs.cloud_caps) {
            Some(Fill::Full) => Level::Issue,
            Some(Fill::Warn) => Level::Watch,
            _ => Level::Fine,
        }
    };
    let jobs_text = format!("{} job{}", cells[3], if cells[3] == "1" { "" } else { "s" });
    let next = cloud::next_run_text(row, view, &inputs.now.with_timezone(&chrono::Local));
    Line { level, text: cloud_columns(w, &provider, &cloud_label(row), &jobs_text, &cells[4], &next, &cloud_traffic(row, view, inputs.now), &cells[5]) }
}

fn alerts_line(alerts: &AlertsView, daemon_running: bool, now: DateTime<Utc>) -> Line {
    if let Some(problem) = &alerts.problem {
        return Line { level: Level::Watch, text: format!("alert settings: {problem}") };
    }
    if !alerts.monitor_on() {
        return Line { level: Level::Unknown, text: "monitor off: no email address is set".to_string() };
    }
    if !daemon_running {
        return Line { level: Level::Watch, text: "monitor configured but the daemon is not running: no alerts will be sent".to_string() };
    }
    let mut text = "monitor on".to_string();
    if let Some(e) = alerts.events.iter().find(|e| e.kind == "alert") {
        text.push_str(&format!(" · last alert {} {} ({})", e.host, e.text, age_text(now, e.at)));
    }
    if let Some(report) = &alerts.last_report {
        text.push_str(&format!(" · report {report}"));
    }
    Line { level: Level::Fine, text }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// Builds the whole Dashboard. The verdict says `checking...` until every host has answered its first probe, so a screen that
/// does not know yet never claims everything is quiet.
/// A check older than this, with the daemon running, is a broken check rather than good health.
const CHECK_STALE_DAYS: i64 = 3;

/// One line per reachable host whose disks have a problem, and one line when the daily checks themselves have stopped
/// happening. Healthy hosts, hosts that never answered and unreachable hosts (the Hosts row says so) add nothing.
fn smart_items(inputs: &Inputs, daemon_running: bool) -> Vec<Item> {
    use crate::smart::Severity;
    let reached = |name: &str| inputs.hosts.iter().any(|h| h.name == name && matches!(h.probe, Some(HostProbe::Reached { .. })));
    let mut items = Vec::new();
    let role_of = |host: &str, dev: &str| {
        let os = inputs.hosts.iter().find(|h| h.name == host).and_then(|h| match h.probe {
            Some(HostProbe::Reached { os, .. }) => *os,
            _ => None,
        });
        crate::daemon::storage_check::role_of(inputs.storage, host, dev, os)
    };
    for found in crate::daemon::smart_check::problems_with(inputs.smart, &role_of).into_iter().filter(|p| reached(&p.host)) {
        let level = if found.severity == Severity::Issue { Level::Issue } else { Level::Watch };
        let what = format!("{}   (checked {})", found.disks, age_text(inputs.now, found.checked_at));
        items.push(Item { level, subject: found.host.clone(), what, goto: Section::Disks, target: Some(found.host) });
    }
    for found in crate::daemon::shares_check::problems(inputs.shares).into_iter().filter(|p| reached(&p.host)) {
        let level = if found.severity == Severity::Issue { Level::Issue } else { Level::Watch };
        let what = format!("{}   (checked {})", found.text, age_text(inputs.now, found.checked_at));
        items.push(Item { level, subject: found.host.clone(), what, goto: Section::Shares, target: Some(found.host) });
    }
    for found in crate::daemon::storage_check::problems(inputs.storage, inputs.thresholds.quota).into_iter().filter(|p| reached(&p.host)) {
        let level = if found.severity == Severity::Issue { Level::Issue } else { Level::Watch };
        let what = format!("{}   (checked {})", found.text, age_text(inputs.now, found.checked_at));
        items.push(Item { level, subject: found.host.clone(), what, goto: Section::Datasets, target: Some(found.host) });
    }
    for found in crate::daemon::storage_check::integrity_problems(inputs.storage, inputs.thresholds.dedup_ratio, inputs.now).into_iter().filter(|p| reached(&p.host)) {
        let what = format!("{}   (checked {})", found.text, age_text(inputs.now, found.checked_at));
        items.push(Item { level: Level::Watch, subject: found.host.clone(), what, goto: Section::Datasets, target: Some(found.host) });
    }
    for found in crate::daemon::storage_check::locked_problems(inputs.storage, &inputs.jobs.jobs).into_iter().filter(|p| reached(&p.host)) {
        let what = format!("{}   (checked {})", found.text, age_text(inputs.now, found.checked_at));
        items.push(Item { level: Level::Watch, subject: found.host.clone(), what, goto: Section::Datasets, target: Some(found.host) });
    }
    for found in crate::daemon::storage_check::pool_problems(inputs.storage).into_iter().filter(|p| reached(&p.host)) {
        let level = if found.severity == Severity::Issue { Level::Issue } else { Level::Watch };
        let what = format!("{}   (checked {})", found.text, age_text(inputs.now, found.checked_at));
        items.push(Item { level, subject: found.host.clone(), what, goto: Section::Pools, target: Some(found.host) });
    }
    // The storage check runs every ten minutes: silent for three times that is a loop that stopped.
    if let (true, Some(last)) = (daemon_running, inputs.storage.ran_at) {
        if inputs.now - last > chrono::Duration::minutes(30) {
            items.push(Item { level: Level::Watch, subject: "checks".to_string(), what: format!("the quota check last ran {}", age_text(inputs.now, last)), goto: Section::Jobs, target: None });
        }
    }
    // The newest answer over both files: a file that is missing is no data, not "never checked".
    let newest = [inputs.smart.newest_check(), inputs.shares.newest_check()].into_iter().flatten().max();
    if let (true, Some(last)) = (daemon_running, newest) {
        let days = (inputs.now - last).num_days();
        if days >= CHECK_STALE_DAYS {
            items.push(Item { level: Level::Watch, subject: "checks".to_string(), what: format!("disk and shares check is {days} days old"), goto: Section::Jobs, target: None });
        }
    }
    items
}

/// One line per unprotected dataset of a reached host: red for the operator's data, amber for the operating system's.
/// Always shown whatever the email timing is (that setting is about email only).
fn protection_items(inputs: &Inputs) -> Vec<(Item, crate::daemon::protection::Unprotected)> {
    use crate::daemon::protection::{judge, Tier};
    let reached = |name: &str| inputs.hosts.iter().any(|h| h.name == name && matches!(h.probe, Some(HostProbe::Reached { .. })));
    judge(inputs.storage, &inputs.jobs.jobs, &inputs.jobs.runs, inputs.protection, inputs.now)
        .into_iter()
        .filter(|u| reached(&u.host))
        .map(|u| {
            let item = Item {
                level: if u.tier == Tier::Data { Level::Issue } else { Level::Watch },
                subject: if u.host == "local" { u.dataset.clone() } else { format!("{}:{}", u.host, u.dataset) },
                what: u.describe(&|t| age_text(inputs.now, t)),
                goto: Section::Jobs,
                target: None,
            };
            (item, u)
        })
        .collect()
}

/// How many unprotected datasets the Dashboard names, one line each; the rest are folded into one line that opens the list.
pub const PROTECTION_NAMED: usize = 3;

/// The one line for the datasets past the named ones: red while any of your data is among them, amber when only the
/// operating system's is. The datasets are still judged, and email and the report still name each one.
fn protection_more(rest: &[Item]) -> Option<Item> {
    if rest.is_empty() {
        return None;
    }
    let data = rest.iter().filter(|i| i.level == Level::Issue).count();
    Some(Item { level: if data > 0 { Level::Issue } else { Level::Watch }, subject: "protection".to_string(), what: format!("and {} more not protected ({data} of your data)", rest.len()), goto: Section::Jobs, target: None })
}

/// The title of the pop-up that lists the unprotected datasets.
pub const PROTECTION_POPUP_TITLE: &str = "Unprotected data";

/// The pop-up behind a protection line: what to say above the list, one row per dataset (your data first), what each
/// lacks (the same order) and the row the chosen line stands for.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtectionPopup {
    pub note: String,
    pub rows: Vec<String>,
    pub gaps: Vec<crate::daemon::protection::Unprotected>,
    pub selected: usize,
    /// A heading per host and the colour of each row.
    pub look: crate::ui::modal::ChooseLook,
}

impl Dashboard {
    /// How many lines the selection moves through: the Needs attention lines, then one row per group.
    pub fn selectable(&self) -> usize {
        self.attention.len() + self.groups.as_ref().map_or(0, Vec::len)
    }

    /// The group a selection index stands for (the indexes past the attention lines), if any.
    pub fn group_at(&self, selection: usize) -> Option<&Group> {
        self.groups.as_ref()?.get(selection.checked_sub(self.attention.len())?)
    }

    /// What a named protection line lacks (Enter goes straight to its fix); `None` for any other line.
    pub fn protection_fix(&self, item: &Item) -> Option<&crate::daemon::protection::Unprotected> {
        let at = self.unprotected.iter().take(PROTECTION_NAMED).position(|u| u == item)?;
        self.gaps.get(at)
    }

    /// The pop-up behind the "and N more" line: every unprotected dataset, starting at the first one that has no line of
    /// its own. `None` for any other line.
    pub fn protection_popup(&self, item: &Item) -> Option<ProtectionPopup> {
        if self.protection_more.as_ref() != Some(item) {
            return None;
        }
        let data = self.unprotected.iter().filter(|u| u.level == Level::Issue).count();
        let note = format!(
            "{} not protected ({data} of your data)\nProtected means snapshots plus a copy on another machine. A new job counts once it has run.\nEnter fixes the selected one.",
            if self.unprotected.len() == 1 { "1 dataset is".to_string() } else { format!("{} datasets are", self.unprotected.len()) }
        );
        // Grouped by host, the hub first and then by name; inside a host your data before the operating system's.
        let mut found: Vec<(usize, &crate::daemon::protection::Unprotected, &Item)> = self.gaps.iter().zip(&self.unprotected).enumerate().map(|(i, (g, item))| (i, g, item)).collect();
        found.sort_by(|a, b| (a.1.host != "local", &a.1.host, a.1.tier, &a.1.dataset).cmp(&(b.1.host != "local", &b.1.host, b.1.tier, &b.1.dataset)));
        let width = found.iter().map(|(_, g, _)| g.dataset.chars().count()).max().unwrap_or(0);
        let rows = found.iter().map(|(_, g, item)| format!("{:<5}  {:<width$}  {}", if item.level == Level::Issue { "issue" } else { "watch" }, g.dataset, item.what)).collect();
        let mut heads = Vec::new();
        let mut at = 0;
        for group in found.chunk_by(|a, b| a.1.host == b.1.host) {
            let of_yours = group.iter().filter(|(_, _, item)| item.level == Level::Issue).count();
            let count = if group.len() == 1 { "1 dataset".to_string() } else { format!("{} datasets", group.len()) };
            let host = group.first().map_or("", |(_, g, _)| g.host.as_str());
            heads.push((at, if of_yours > 0 { format!("{host}  ({count}, {of_yours} of your data)") } else { format!("{host}  ({count})") }));
            at += group.len();
        }
        let levels = found.iter().map(|(_, _, item)| item.level).collect();
        let selected = found.iter().position(|(i, _, _)| *i >= PROTECTION_NAMED).unwrap_or(0);
        let gaps = found.iter().map(|(_, g, _)| (*g).clone()).collect();
        Some(ProtectionPopup { note, rows, gaps, selected, look: crate::ui::modal::ChooseLook { heads, levels } })
    }
}

/// What the red notice says while a check runs.
fn running_notice(marker: &crate::daemon::checks::Marker) -> String {
    let since = marker.since.with_timezone(&chrono::Local).format("%H:%M");
    match &marker.host {
        Some(host) => format!("checking {host}: disks and shares since {since}"),
        None => format!("checking: daily disk and shares check since {since}"),
    }
}

pub fn build(inputs: &Inputs) -> Dashboard {
    let now = inputs.now;
    let hosts: Vec<HostRow> = inputs.hosts.iter().map(|h| host_row(h, inputs.thresholds, now)).collect();
    let groups = groups_of(&inputs.hosts, &hosts, inputs.expand_groups);
    let daemon = daemon_line(inputs.jobs, now);
    let (jobs, failed_jobs) = jobs_line(inputs.jobs);
    let widths = CloudWidths {
        remote: inputs.cloud_rows.iter().map(|r| cloud_label(r).chars().count()).max().unwrap_or(0).max("Remote".len()),
        last: inputs.cloud_rows.iter().map(|r| cloud_last(r, inputs.jobs, inputs.now).chars().count()).max().unwrap_or(0).max("Last run".len()),
        traffic: inputs.cloud_rows.iter().map(|r| cloud_traffic(r, inputs.jobs, inputs.now).chars().count()).max().unwrap_or(0).max("Traffic".len()),
    };
    let cloud: Vec<Line> = inputs.cloud_rows.iter().map(|r| cloud_line(r, inputs.jobs, inputs, &widths)).collect();
    let cloud_header = if cloud.is_empty() { String::new() } else { cloud_header(&widths) };
    let alerts = alerts_line(inputs.alerts, daemon.level == Level::Fine, now);

    let mut attention: Vec<Item> = Vec::new();
    for row in &hosts {
        attention.extend(host_attention(row).into_iter().map(|(level, what)| Item { level, subject: row.name.clone(), what, goto: Section::Hosts, target: Some(row.name.clone()) }));
    }
    for host in &inputs.hosts {
        let Some(HostProbe::Reached { pools, .. }) = host.probe else { continue };
        for pool in pools {
            let what = match &pool.scan {
                Some(ScanSummary::Running { percent, resilver: true }) => percent.map_or_else(|| format!("{} resilvering", pool.name), |p| format!("{} resilvering {p}%", pool.name)),
                Some(ScanSummary::Paused { resilver: true }) => format!("{} resilver paused", pool.name),
                _ => continue,
            };
            attention.push(Item { level: Level::Watch, subject: host.name.to_string(), what, goto: Section::Pools, target: Some(host.name.to_string()) });
        }
    }
    attention.extend(smart_items(inputs, daemon.level == Level::Fine));
    if daemon.level == Level::Issue {
        attention.push(Item { level: Level::Issue, subject: "daemon".to_string(), what: daemon.text.clone(), goto: Section::Jobs, target: None });
    }
    for job in inputs.jobs.jobs.iter().filter(|j| j.enabled && is_failed(inputs.jobs, j)) {
        let Some(run) = inputs.jobs.last_run(&job.id) else { continue };
        let age = age_text(now, run.finished_at.unwrap_or(run.started_at));
        let why = run.message.lines().next().unwrap_or("").trim();
        let what = if why.is_empty() { format!("failed {age}") } else { format!("failed {age}: {why}") };
        attention.push(Item { level: Level::Issue, subject: job.id.clone(), what, goto: Section::Jobs, target: Some(job.id.clone()) });
    }
    let mut protection = Vec::new();
    let mut unprotected = Vec::new();
    let mut gaps = Vec::new();
    let mut more = None;
    if daemon.level == Level::Fine {
        for late in crate::ui::overdue::overdue(inputs.jobs, now) {
            attention.push(Item { level: if late.red { Level::Issue } else { Level::Watch }, subject: late.job_id.clone(), what: late.text, goto: Section::Jobs, target: Some(late.job_id) });
        }
        let (items, found): (Vec<Item>, Vec<_>) = protection_items(inputs).into_iter().unzip();
        protection = items;
        gaps = found;
        unprotected.clone_from(&protection);
        let rest = protection.split_off(PROTECTION_NAMED.min(protection.len()));
        more = protection_more(&rest);
        protection.extend(more.clone());
    }
    for row in &inputs.cloud_rows {
        let level = match cloud::row_fill(row, inputs.cloud_sizes, inputs.cloud_caps) {
            Some(Fill::Full) => Level::Issue,
            Some(Fill::Warn) => Level::Watch,
            _ => continue,
        };
        attention.push(Item { level, subject: cloud_label(row), what: format!("plan {}", cloud::in_bucket(row, inputs.cloud_sizes, inputs.cloud_caps)), goto: Section::Cloud, target: None });
    }
    if let Some(problem) = &inputs.alerts.problem {
        attention.push(Item { level: Level::Watch, subject: "alerts".to_string(), what: problem.clone(), goto: Section::Alerts, target: None });
    }
    if let Some(problem) = inputs.protection_problem {
        attention.push(Item { level: Level::Watch, subject: "config.toml".to_string(), what: crate::config::protection_problem_line(problem), goto: Section::Settings, target: None });
    }
    attention.sort_by(|a, b| b.level.cmp(&a.level).then_with(|| a.subject.cmp(&b.subject)));
    // Within each level the protection lines come after the other items, so dozens of them never push a degraded pool or a
    // failed job out of sight; among themselves they keep the judging's order (data first, then host and dataset).
    protection.sort_by_key(|p| std::cmp::Reverse(p.level));
    let mut rest = protection.into_iter().peekable();
    let mut merged = Vec::with_capacity(attention.len() + rest.len());
    for item in attention {
        while rest.peek().is_some_and(|p| p.level > item.level) {
            merged.extend(rest.next());
        }
        merged.push(item);
    }
    merged.extend(rest);
    let attention = merged;

    let checking = inputs.hosts.iter().any(|h| h.assessment.level == HealthLevel::Probing);
    let verdict = {
        let issues = attention.iter().filter(|i| i.level == Level::Issue).count();
        let watches = attention.iter().filter(|i| i.level == Level::Watch).count();
        match (issues, watches) {
            (0, 0) if checking => Line { level: Level::Unknown, text: "checking...".to_string() },
            (0, 0) => Line { level: Level::Fine, text: "all quiet".to_string() },
            (i, w) if checking => {
                let mut parts = Vec::new();
                if i > 0 {
                    parts.push(plural(i, "issue"));
                }
                if w > 0 {
                    parts.push(format!("{w} to watch"));
                }
                Line { level: if i > 0 { Level::Issue } else { Level::Watch }, text: format!("checking... {}", parts.join(", ")) }
            }
            (0, w) => Line { level: Level::Watch, text: format!("{w} to watch") },
            (i, 0) => Line { level: Level::Issue, text: plural(i, "issue") },
            (i, w) => Line { level: Level::Issue, text: format!("{}, {w} to watch", plural(i, "issue")) },
        }
    };
    Dashboard { verdict, attention, unprotected, gaps, protection_more: more, hosts, groups, daemon, jobs, failed_jobs, cloud, cloud_header, alerts, running: inputs.running.map(running_notice) }
}


#[cfg(test)]
mod tests;
