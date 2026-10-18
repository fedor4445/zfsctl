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
    let worst = [&space, &memory, &cpu, &scrub, &errors].into_iter().filter(|c| c.level >= Level::Watch).max_by_key(|c| c.level);
    let state = match worst {
        Some(cell) if cell.level > base_level => Cell::new(cell.note.clone().unwrap_or_else(|| cell.text.clone()), cell.level),
        _ => Cell::new(input.assessment.reason.clone(), base_level),
    };
    HostRow { name: input.name.to_string(), state, os: input.os_name.map_or_else(Cell::none, |n| Cell::new(n, Level::Fine)), pools, space, memory, cpu, load, scrub, errors, base }
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
    pub hosts: Vec<HostRow>,
    pub daemon: Line,
    pub jobs: Line,
    /// Ids of the enabled jobs whose latest run failed.
    pub failed_jobs: Vec<String>,
    pub cloud: Vec<Line>,
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
    /// What the daemon's daily `smartctl` check found (`smart.json`).
    pub smart: &'a crate::daemon::smart_check::SmartFile,
    /// What the daily check found about file sharing (`shares.json`).
    pub shares: &'a crate::daemon::shares_check::SharesFile,
    /// What the storage check found: datasets near their quota (`storage.json`).
    pub storage: &'a crate::daemon::storage_check::StorageFile,
    /// The check that is running now, if any (a fresh marker).
    pub running: Option<&'a crate::daemon::checks::Marker>,
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

/// `remote_width` is the width of the widest remote, so the columns after it line up.
fn cloud_line(row: &cloud::Row, view: &JobsView, inputs: &Inputs, remote_width: usize) -> Line {
    let cells = cloud::cells(row, view, inputs.cloud_sizes, inputs.cloud_caps, inputs.now);
    // Columns, two spaces apart: the provider's name, then the remote (each padded so the next column lines up).
    let name = format!("{:<16}  {:<remote_width$}", cloud::provider_name(row.kind.as_deref()), cloud_label(row));
    let name = name.as_str();
    if row.job_ids.is_empty() {
        return Line { level: Level::Unknown, text: format!("{name}  no jobs") };
    }
    let bucket = if cells[5] == "-" { String::new() } else { format!("  {}", cells[5]) };
    let level = if cloud::failed(row, view) {
        Level::Issue
    } else {
        match cloud::row_fill(row, inputs.cloud_sizes, inputs.cloud_caps) {
            Some(Fill::Full) => Level::Issue,
            Some(Fill::Warn) => Level::Watch,
            _ => Level::Fine,
        }
    };
    Line { level, text: format!("{name}  {} job{}  last {}{bucket}", cells[3], if cells[3] == "1" { "" } else { "s" }, cells[4]) }
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
    for found in crate::daemon::smart_check::problems(inputs.smart).into_iter().filter(|p| reached(&p.host)) {
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
    let daemon = daemon_line(inputs.jobs, now);
    let (jobs, failed_jobs) = jobs_line(inputs.jobs);
    let remote_width = inputs.cloud_rows.iter().map(|r| cloud_label(r).chars().count()).max().unwrap_or(0);
    let cloud: Vec<Line> = inputs.cloud_rows.iter().map(|r| cloud_line(r, inputs.jobs, inputs, remote_width)).collect();
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
    attention.sort_by(|a, b| b.level.cmp(&a.level).then_with(|| a.subject.cmp(&b.subject)));

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
    Dashboard { verdict, attention, hosts, daemon, jobs, failed_jobs, cloud, alerts, running: inputs.running.map(running_notice) }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Thresholds;
    use crate::zfs::host_health::{assess, HostProbe, PoolProbe, ScanSummary};
    use crate::zfs::host_resources::Resources;
    use crate::zfs::pool::PoolHealth;
    use chrono::{DateTime, Duration, Utc};

    const GIB: u64 = 1 << 30;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-17T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    /// `zpool status`'s own wording for a scrub that finished `days` ago (host time = UTC here).
    fn when(days: i64) -> String {
        (now() - Duration::days(days)).format("%a %b %e %H:%M:%S %Y").to_string()
    }

    fn pool(name: &str, capacity: u8, errors: u64, last_scrub: Option<String>) -> PoolProbe {
        let scan = Some(last_scrub.as_ref().map_or(ScanSummary::Never, |w| ScanSummary::Finished { when: w.clone(), errors: 0 }));
        PoolProbe { name: name.into(), health: PoolHealth::Online, capacity_percent: capacity, frag_percent: None, errors: Some(errors), scan, last_scrub }
    }

    fn reached(pools: Vec<PoolProbe>) -> HostProbe {
        HostProbe::Reached { rtt_ms: 3, pools, kind: None, os: None, os_name: None }
    }

    fn reading(free_gib: u64, arc_gib: u64, busy: u64, idle: u64) -> Resources {
        Resources {
            cores: 4,
            physical: 20 * GIB,
            free: free_gib * GIB,
            inactive: 0,
            arc: arc_gib * GIB,
            load: Some([0.05, 0.1, 0.1]),
            cpu_ticks: Some(crate::zfs::host::CpuTimes { user: busy, nice: 0, sys: 0, intr: 0, idle }),
            swap: None,
        }
    }

    /// A host whose probe says `probe`, with the given resource readings (newest, previous), judged with the defaults.
    fn row(probe: &HostProbe, now_r: Option<&Resources>, before: Option<&Resources>, t: &Thresholds) -> HostRow {
        let input = HostInput { name: "h", assessment: assess(Some(probe), false), probe: Some(probe), resources: now_r.map(|n| (n, before)), utc_offset_secs: Some(0), os_name: None };
        host_row(&input, t, now())
    }

    fn default_row(probe: &HostProbe, now_r: Option<&Resources>) -> HostRow {
        row(probe, now_r, None, &Thresholds::default())
    }

    fn levels(r: &HostRow) -> [Level; 8] {
        [r.pools.level, r.space.level, r.memory.level, r.cpu.level, r.load.level, r.scrub.level, r.errors.level, r.state.level]
    }

    #[test]
    fn levels_sort_worst_first() {
        let mut all = [Level::Fine, Level::Issue, Level::Unknown, Level::Watch];
        all.sort_by(|a, b| b.cmp(a));
        assert_eq!(all, [Level::Issue, Level::Watch, Level::Unknown, Level::Fine]);
        assert_eq!((Level::Issue.rank(), Level::Fine.rank()), (3, 0));
    }

    #[test]
    fn space_is_fine_below_80_watch_from_80_and_an_issue_from_90() {
        let at = |c: u8| default_row(&reached(vec![pool("tank", c, 0, Some(when(1)))]), Some(&reading(10, 0, 0, 100)));
        assert_eq!((at(79).space.level, at(80).space.level, at(89).space.level, at(90).space.level), (Level::Fine, Level::Watch, Level::Watch, Level::Issue));
        assert_eq!((at(43).space.text.as_str(), at(43).space.gauge), ("43%", Some(43)));
    }

    #[test]
    fn memory_is_fine_below_90_watch_from_90_and_an_issue_from_97() {
        // 20 GiB, nothing free or cached: used% is 100 minus the free share.
        let at = |free_tenths: u64| {
            let r = Resources { free: 20 * GIB * free_tenths / 100, ..reading(0, 0, 0, 100) };
            default_row(&reached(vec![]), Some(&r)).memory
        };
        assert_eq!((at(11).level, at(10).level, at(4).level, at(3).level, at(2).level), (Level::Fine, Level::Watch, Level::Watch, Level::Issue, Level::Issue));
        assert_eq!(at(2).text, "98%");
        assert_eq!(at(2).gauge, Some(98));
    }

    #[test]
    fn a_host_whose_cache_fills_the_ram_is_not_under_memory_pressure() {
        let r = reading(1, 19, 0, 100);
        let memory = default_row(&reached(vec![]), Some(&r)).memory;
        assert_eq!((memory.level, memory.text.as_str()), (Level::Fine, "0%"));
    }

    #[test]
    fn swap_in_use_raises_the_memory_cell_and_names_itself_only_when_it_matters() {
        let with_swap = |used_pct: u64| Resources { swap: Some(crate::zfs::host::Swap { total: 100 * GIB, used: used_pct * GIB }), ..reading(10, 0, 0, 100) };
        let at = |p: u64| default_row(&reached(vec![]), Some(&with_swap(p))).memory;
        assert_eq!((at(49).level, at(49).text.as_str()), (Level::Fine, "50%"), "memory alone is 50% here and below its own watch line; swap at 49 is quiet");
        assert_eq!(at(50).level, Level::Watch);
        assert!(at(50).text.contains("swap 50%"), "{}", at(50).text);
        assert_eq!(at(90).level, Level::Issue);
        assert_eq!(default_row(&reached(vec![]), Some(&reading(10, 0, 0, 100))).memory.level, Level::Fine, "no swap at all is fine");
    }

    #[test]
    fn cpu_is_a_watch_only_from_95_between_two_readings_and_never_an_issue() {
        let t = Thresholds::default();
        let before = reading(10, 0, 0, 1000);
        let cpu = |busy: u64, idle: u64| row(&reached(vec![]), Some(&reading(10, 0, busy, 1000 + idle)), Some(&before), &t).cpu;
        assert_eq!(cpu(94, 6).level, Level::Fine);
        assert_eq!((cpu(95, 5).level, cpu(95, 5).text.as_str()), (Level::Watch, "95%"));
        assert_eq!(cpu(100, 0).level, Level::Watch, "CPU alone is never red");
        assert_eq!(cpu(40, 60).gauge, Some(40));
        let one = row(&reached(vec![]), Some(&reading(10, 0, 0, 1000)), None, &t).cpu;
        assert_eq!((one.level, one.text.as_str()), (Level::Unknown, "?"), "the first reading has nothing to compare with");
    }

    #[test]
    fn a_scrub_is_fine_until_35_days_a_watch_from_35_and_an_issue_from_90() {
        let at = |days: i64| default_row(&reached(vec![pool("tank", 10, 0, Some(when(days)))]), Some(&reading(10, 0, 0, 100))).scrub;
        assert_eq!((at(34).level, at(35).level, at(89).level, at(90).level), (Level::Fine, Level::Watch, Level::Watch, Level::Issue));
        assert_eq!(at(8).text, "8 d ago");
        assert_eq!(at(0).text, "today");
        assert!(at(40).note.as_deref().unwrap().contains("tank") && at(40).note.as_deref().unwrap().contains("40"), "{:?}", at(40).note);
    }

    #[test]
    fn a_pool_never_scrubbed_is_a_watch_unless_it_is_ignored_and_a_resilver_is_not_a_scrub() {
        let never = default_row(&reached(vec![pool("zroot", 10, 0, None)]), Some(&reading(10, 0, 0, 100))).scrub;
        assert_eq!((never.level, never.text.as_str()), (Level::Watch, "never"));
        assert!(never.note.as_deref().unwrap().contains("zroot never scrubbed"));
        let t = Thresholds { scrub_ignore: vec!["zroot".into()], ..Thresholds::default() };
        let ignored = row(&reached(vec![pool("zroot", 10, 0, None)]), Some(&reading(10, 0, 0, 100)), None, &t).scrub;
        assert_eq!(ignored.level, Level::Fine, "a pool the owner said not to judge");
        // `scan` says a scan finished (a resilver), but there was never a scrub.
        let resilvered = PoolProbe { scan: Some(ScanSummary::Finished { when: when(1), errors: 0 }), ..pool("tpool", 10, 0, None) };
        assert_eq!(default_row(&reached(vec![resilvered]), Some(&reading(10, 0, 0, 100))).scrub.level, Level::Watch);
    }

    #[test]
    fn a_running_or_paused_scan_is_not_judged_and_no_utc_offset_means_unknown_age() {
        let running = PoolProbe { scan: Some(ScanSummary::Running { percent: Some(34), resilver: false }), ..pool("tank", 10, 0, None) };
        let r = default_row(&reached(vec![running]), Some(&reading(10, 0, 0, 100))).scrub;
        assert_eq!((r.level, r.text.as_str()), (Level::Fine, "running 34%"));
        let probe = reached(vec![pool("tank", 10, 0, Some(when(50)))]);
        let input = HostInput { name: "h", assessment: assess(Some(&probe), false), probe: Some(&probe), resources: None, utc_offset_secs: None, os_name: None };
        let no_offset = host_row(&input, &Thresholds::default(), now()).scrub;
        assert_eq!((no_offset.level, no_offset.text.as_str()), (Level::Unknown, "?"), "an age needs the host's UTC offset");
    }

    #[test]
    fn disk_errors_are_counted_over_the_pools_and_a_missing_count_is_unknown() {
        let r = default_row(&reached(vec![pool("a", 10, 2, Some(when(1))), pool("b", 10, 3, Some(when(1)))]), Some(&reading(10, 0, 0, 100)));
        assert_eq!((r.errors.level, r.errors.text.as_str()), (Level::Watch, "5"));
        let clean = default_row(&reached(vec![pool("a", 10, 0, Some(when(1)))]), Some(&reading(10, 0, 0, 100)));
        assert_eq!((clean.errors.level, clean.errors.text.as_str()), (Level::Fine, "0"));
        let blind = PoolProbe { errors: None, ..pool("a", 10, 0, Some(when(1))) };
        assert_eq!(default_row(&reached(vec![blind]), Some(&reading(10, 0, 0, 100))).errors.level, Level::Unknown, "zpool status was unavailable");
    }

    #[test]
    fn the_state_starts_from_the_hosts_assessment_and_is_only_ever_raised() {
        let fine = default_row(&reached(vec![pool("tank", 10, 0, Some(when(1)))]), Some(&reading(10, 0, 0, 100)));
        assert_eq!((fine.state.level, fine.state.text.as_str()), (Level::Fine, "ok"));
        // memory 98% on a host assess() calls fine: the state becomes an issue and says why.
        let hot = Resources { free: 20 * GIB * 2 / 100, ..reading(0, 0, 0, 100) };
        let raised = default_row(&reached(vec![pool("tank", 10, 0, Some(when(1)))]), Some(&hot));
        assert_eq!((raised.state.level, raised.state.text.as_str()), (Level::Issue, "memory 98%"));
        // an issue from assess() stays an issue whatever the readings say.
        let degraded = PoolProbe { health: PoolHealth::Degraded, ..pool("tank", 10, 0, Some(when(1))) };
        let r = default_row(&reached(vec![degraded]), Some(&reading(10, 0, 0, 100)));
        assert_eq!((r.state.level, r.state.text.as_str()), (Level::Issue, "tank DEGRADED"));
        // a Watch from assess() is raised by an Issue reading but not lowered by a fine one.
        let full = pool("tank", 85, 0, Some(when(1)));
        assert_eq!(default_row(&reached(vec![full.clone()]), Some(&reading(10, 0, 0, 100))).state.level, Level::Watch);
        assert_eq!(default_row(&reached(vec![full]), Some(&hot)).state.level, Level::Issue);
    }

    #[test]
    fn a_host_that_cannot_be_reached_or_inspected_has_unknown_readings_never_fine_ones() {
        for probe in [HostProbe::Disconnected, HostProbe::Unavailable { reason: "sudo: a password is required".into() }] {
            let r = default_row(&probe, None);
            assert_ne!(r.state.level, Level::Fine, "{probe:?}");
            for cell in [&r.pools, &r.space, &r.memory, &r.cpu, &r.load, &r.scrub, &r.errors] {
                assert_eq!((cell.level, cell.text.as_str()), (Level::Unknown, "?"), "{probe:?}");
            }
        }
        let never_probed = host_row(&HostInput { name: "h", assessment: assess(None, false), probe: None, resources: None, utc_offset_secs: None, os_name: None }, &Thresholds::default(), now());
        assert_eq!((never_probed.state.level, never_probed.state.text.as_str()), (Level::Unknown, "probing"));
    }

    #[test]
    fn a_host_that_answered_but_has_no_resource_reading_shows_unknown_memory_cpu_and_load() {
        let r = default_row(&reached(vec![pool("tank", 10, 0, Some(when(1)))]), None);
        assert_eq!((r.memory.level, r.cpu.level, r.load.level), (Level::Unknown, Level::Unknown, Level::Unknown));
        assert_eq!(r.space.level, Level::Fine, "what the probe did tell is still shown");
    }

    #[test]
    fn a_host_with_no_pools_is_fine_with_dashes() {
        let r = default_row(&reached(vec![]), Some(&reading(10, 0, 0, 100)));
        for cell in [&r.pools, &r.space, &r.scrub, &r.errors] {
            assert_eq!((cell.level, cell.text.as_str()), (Level::Fine, "-"));
        }
        assert_eq!(levels(&r)[7], Level::Fine);
    }

    #[test]
    fn the_load_is_shown_as_the_one_minute_average_and_never_judged() {
        let r = default_row(&reached(vec![]), Some(&reading(10, 0, 0, 100)));
        assert_eq!((r.load.text.as_str(), r.load.level), ("0.05", Level::Fine));
        let busy = Resources { load: Some([40.0, 30.0, 20.0]), ..reading(10, 0, 0, 100) };
        assert_eq!(default_row(&reached(vec![]), Some(&busy)).load.level, Level::Fine, "the spec has no load threshold");
    }

    #[test]
    fn attention_lists_every_non_fine_cell_once_and_the_assessments_own_verdict_unless_a_cell_says_it() {
        let hot = Resources { free: 20 * GIB * 2 / 100, ..reading(0, 0, 0, 100) };
        // space 85% (assess says "tank 85% full"), never scrubbed, memory 98%.
        let r = default_row(&reached(vec![pool("tank", 85, 0, None)]), Some(&hot));
        let items = host_attention(&r);
        let texts: Vec<&str> = items.iter().map(|(_, w)| w.as_str()).collect();
        assert!(texts.contains(&"memory 98%") && texts.contains(&"tank 85% full") && texts.contains(&"tank never scrubbed"), "{texts:?}");
        assert_eq!(texts.iter().filter(|t| t.contains("85% full")).count(), 1, "the space cell and assess() say the same thing once");
        let degraded = PoolProbe { health: PoolHealth::Degraded, ..pool("tank", 10, 0, Some(when(1))) };
        let r = default_row(&reached(vec![degraded]), Some(&hot));
        let texts: Vec<String> = host_attention(&r).into_iter().map(|(_, w)| w).collect();
        assert!(texts.contains(&"tank DEGRADED".to_string()) && texts.contains(&"memory 98%".to_string()), "a cell must not hide the pool state: {texts:?}");
        assert!(host_attention(&default_row(&reached(vec![pool("tank", 10, 0, Some(when(1)))]), Some(&reading(10, 0, 0, 100)))).is_empty());
        let unreachable = default_row(&HostProbe::Disconnected, None);
        assert_eq!(host_attention(&unreachable), vec![(Level::Issue, "unreachable".to_string())], "unknown cells are not items of their own");
    }

    // ---- the other components, attention and the verdict ----

    use crate::daemon::heartbeat::Heartbeat;
    use crate::daemon::history::{Event, EventKind, History, RunResult};
    use crate::jobs::Job;
    use crate::ui::jobs::JobsView;
    use crate::ui::state::Section;
    use std::collections::HashMap;

    fn mirror(id: &str, remote: &str) -> Job {
        Job::test_mirror(id, "tank/data", "hourly", remote)
    }

    fn scrub_job(id: &str) -> Job {
        let mut j = Job::test_snapshot(id, "tank/data", "hourly", 3);
        j.kind = crate::jobs::KIND_SCRUB.into();
        j.pool = Some("tank".into());
        j
    }

    /// A jobs view with these jobs and these finished runs `(job, result, message)`, all two hours before `now()`.
    fn view(jobs: Vec<Job>, runs: &[(&str, RunResult, &str)], heartbeat: Option<Heartbeat>) -> JobsView {
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(dir.path().join("history.jsonl"));
        for (i, (job, result, message)) in runs.iter().enumerate() {
            let when = now() - Duration::hours(2);
            let ev = |kind, result| Event { job_id: (*job).into(), run_id: format!("{job}-{i}"), event: kind, at: when, scheduled_for: None, result, message: (*message).into(), catch_up: false, summary: None };
            history.append(&ev(EventKind::Started, None)).unwrap();
            history.append(&ev(EventKind::Finished, Some(*result))).unwrap();
        }
        JobsView { jobs, runs: crate::daemon::history::runs(&history.read()), heartbeat, loaded: true, ..JobsView::default() }
    }

    fn beat(seconds_ago: i64) -> Option<Heartbeat> {
        Some(Heartbeat { pid: 4242, started_at: now() - Duration::days(1), last_tick: now() - Duration::seconds(seconds_ago) })
    }

    fn alerts(mail: bool) -> crate::ui::alerts::AlertsView {
        let mut a = crate::ui::alerts::AlertsView { loaded: true, ..Default::default() };
        if mail {
            let cfg = crate::config::NotifyConfig { mail_to: Some("me@example.com".into()), ..Default::default() };
            a.saved = cfg.clone();
            a.settings = cfg.resolve().ok();
        }
        a
    }

    struct Fixture {
        view: JobsView,
        alerts: crate::ui::alerts::AlertsView,
        cloud_rows: Vec<crate::ui::cloud::Row>,
        sizes: HashMap<String, crate::ui::cloud::Size>,
        caps: crate::ui::cloud::Caps,
        thresholds: Thresholds,
        smart: crate::daemon::smart_check::SmartFile,
        shares: crate::daemon::shares_check::SharesFile,
        storage: crate::daemon::storage_check::StorageFile,
        marker: Option<crate::daemon::checks::Marker>,
    }

    impl Fixture {
        fn new(view: JobsView) -> Fixture {
            Fixture { view, alerts: alerts(true), cloud_rows: vec![], sizes: HashMap::new(), caps: HashMap::new(), thresholds: Thresholds::default(), smart: Default::default(), shares: Default::default(), storage: Default::default(), marker: None }
        }

        fn build(&self, hosts: Vec<HostInput>) -> Dashboard {
            build(&Inputs { hosts, jobs: &self.view, cloud_rows: self.cloud_rows.clone(), cloud_sizes: &self.sizes, cloud_caps: &self.caps, alerts: &self.alerts, thresholds: &self.thresholds, smart: &self.smart, shares: &self.shares, storage: &self.storage, running: self.marker.as_ref(), now: now() })
        }
    }

    fn fine_host<'a>(name: &'a str, probe: &'a HostProbe, r: &'a Resources, before: &'a Resources) -> HostInput<'a> {
        HostInput { name, assessment: assess(Some(probe), false), probe: Some(probe), resources: Some((r, Some(before))), utc_offset_secs: Some(0), os_name: None }
    }

    #[test]
    fn the_daemon_line_says_running_with_the_heartbeat_age_or_that_nothing_will_fire() {
        let on = Fixture::new(view(vec![], &[], beat(3))).build(vec![]);
        assert_eq!((on.daemon.level, on.daemon.text.as_str()), (Level::Fine, "running (pid 4242), heartbeat 3s ago"));
        for heartbeat in [None, beat(600)] {
            let off = Fixture::new(view(vec![], &[], heartbeat)).build(vec![]);
            assert_eq!(off.daemon.level, Level::Issue);
            assert!(off.daemon.text.contains("not running"), "{}", off.daemon.text);
            assert!(off.attention.iter().any(|i| i.level == Level::Issue && i.subject == "daemon" && i.goto == Section::Jobs), "{:?}", off.attention);
        }
    }

    #[test]
    fn the_jobs_line_counts_enabled_jobs_by_kind_and_names_the_failed_ones() {
        let mut disabled = scrub_job("old-scrub");
        disabled.enabled = false;
        let jobs = vec![scrub_job("scrub-a"), disabled, Job::test_backup("backup-a", "tank/data", "hourly", "/r", "/pw"), mirror("mirror-a", "b2:bkt/p")];
        let runs = [("scrub-a", RunResult::Ok, "ok"), ("backup-a", RunResult::Failed, "password file not readable\nmore"), ("old-scrub", RunResult::Failed, "ignored: disabled")];
        let d = Fixture::new(view(jobs, &runs, beat(1))).build(vec![]);
        assert_eq!(d.jobs.level, Level::Issue);
        assert!(d.jobs.text.contains("scrub 1/1 ok") && d.jobs.text.contains("backup 0/1 FAILED") && d.jobs.text.contains("cloud 1/1 ok"), "{}", d.jobs.text);
        assert_eq!(d.failed_jobs, ["backup-a"], "a disabled job is left out");
        let item = d.attention.iter().find(|i| i.subject == "backup-a").expect("the failed job is in Needs attention");
        assert_eq!((item.level, item.goto), (Level::Issue, Section::Jobs));
        assert!(item.what.contains("failed 2h ago") && item.what.contains("password file not readable") && !item.what.contains("more"), "{}", item.what);
    }

    #[test]
    fn no_jobs_at_all_is_unknown_not_fine() {
        let d = Fixture::new(view(vec![], &[], beat(1))).build(vec![]);
        assert_eq!((d.jobs.level, d.jobs.text.as_str()), (Level::Unknown, "no jobs scheduled"));
        assert!(d.failed_jobs.is_empty());
    }

    #[test]
    fn cloud_lines_follow_the_cloud_screen_and_never_trigger_a_measurement() {
        let jobs = vec![mirror("m1", "b2:zfsctl/photos")];
        let mut known = crate::ui::cloud::Known::new();
        known.insert("local".into(), vec![("b2".into(), "b2".into()), ("t1".into(), "sftp".into())]);
        let mut fx = Fixture::new(view(jobs.clone(), &[("m1", RunResult::Ok, "mirrored")], beat(1)));
        fx.cloud_rows = crate::ui::cloud::rows(&jobs, &known);
        let d = fx.build(vec![]);
        assert_eq!(d.cloud.len(), 2);
        assert_eq!(d.cloud[0].level, Level::Fine);
        assert!(d.cloud[0].text.contains("b2") && d.cloud[0].text.contains("last ok 2h ago") && !d.cloud[0].text.contains("MiB"), "{}", d.cloud[0].text);
        assert_eq!((d.cloud[1].level, d.cloud[1].text.contains("no jobs")), (Level::Unknown, true), "a remote nobody uses");
        // measured this session: the size shows, with the cap when there is one.
        fx.sizes.insert("m1".into(), crate::ui::cloud::Size { current: Some((3, 1_992_294)), old: None, at: now(), problem: None });
        fx.caps.insert("b2".into(), Ok(10_000_000_000));
        assert!(fx.build(vec![]).cloud[0].text.contains("1.9 MiB of 10 GB (0.02%)"));
        // a plan at 85% is a Watch line and an item that goes to Cloud.
        fx.caps.insert("b2".into(), Ok(2_300_000));
        let d = fx.build(vec![]);
        assert_eq!(d.cloud[0].level, Level::Watch);
        assert!(d.attention.iter().any(|i| i.level == Level::Watch && i.subject == "b2" && i.goto == Section::Cloud), "{:?}", d.attention);
    }

    #[test]
    fn a_cloud_line_starts_with_the_providers_name_and_then_the_remote() {
        let jobs = vec![mirror("m1", "b2:zfsctl/photos")];
        let mut known = crate::ui::cloud::Known::new();
        known.insert("local".into(), vec![("b2".into(), "b2".into()), ("t1".into(), "sftp".into())]);
        known.insert("fbsd16".into(), vec![("aws".into(), "s3".into())]);
        let mut fx = Fixture::new(view(jobs.clone(), &[("m1", RunResult::Ok, "ok")], beat(1)));
        fx.cloud_rows = crate::ui::cloud::rows(&jobs, &known);
        let d = fx.build(vec![]);
        let texts: Vec<&str> = d.cloud.iter().map(|l| l.text.as_str()).collect();
        // two spaces between the columns; the remote column is as wide as the longest remote (`fbsd16:aws`, 10)
        let cols = |provider: &str, remote: &str, rest: &str| format!("{provider:<16}  {remote:<10}  {rest}");
        assert!(texts[0].starts_with(&cols("Backblaze B2", "b2", "1 job  last ok")), "{texts:?}");
        assert!(texts.iter().any(|t| t.starts_with(&cols("SFTP", "t1", "no jobs"))), "{texts:?}");
        assert!(texts.iter().any(|t| t.starts_with(&cols("Amazon S3 (etc.)", "fbsd16:aws", "no jobs"))), "{texts:?}");
        assert!(texts.iter().all(|t| !t.contains("(b2)") && !t.contains("(sftp)")), "the raw type in parentheses is gone: {texts:?}");
    }

    #[test]
    fn a_failed_cloud_job_is_one_item_not_two_and_its_cloud_line_is_red() {
        let jobs = vec![mirror("m1", "b2:zfsctl/photos")];
        let mut known = crate::ui::cloud::Known::new();
        known.insert("local".into(), vec![("b2".into(), "b2".into())]);
        let mut fx = Fixture::new(view(jobs.clone(), &[("m1", RunResult::Failed, "the keys were refused")], beat(1)));
        fx.cloud_rows = crate::ui::cloud::rows(&jobs, &known);
        let d = fx.build(vec![]);
        assert_eq!(d.cloud[0].level, Level::Issue);
        assert_eq!(d.attention.iter().filter(|i| i.subject == "m1" || i.subject == "b2").count(), 1, "{:?}", d.attention);
    }

    #[test]
    fn a_resilvering_pool_is_one_yellow_line_and_a_scrub_or_a_finished_resilver_is_none() {
        let fx = Fixture::new(view(vec![], &[], beat(1)));
        let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
        let items = |scan: ScanSummary, health: PoolHealth| {
            let probe = reached(vec![PoolProbe { scan: Some(scan), health, last_scrub: Some(when(1)), ..pool("tank", 10, 0, Some(when(1))) }]);
            fx.build(vec![fine_host("h", &probe, &r, &b)]).attention.into_iter().filter(|i| i.what.contains("resilver")).collect::<Vec<_>>()
        };
        let one = items(ScanSummary::Running { percent: Some(42), resilver: true }, PoolHealth::Online);
        assert_eq!(one.len(), 1, "{one:?}");
        assert_eq!((one[0].level, one[0].subject.as_str(), one[0].what.as_str(), one[0].goto, one[0].target.as_deref()), (Level::Watch, "h", "tank resilvering 42%", Section::Pools, Some("h")));
        assert_eq!(items(ScanSummary::Running { percent: None, resilver: true }, PoolHealth::Online)[0].what, "tank resilvering");
        assert_eq!(items(ScanSummary::Paused { resilver: true }, PoolHealth::Online)[0].what, "tank resilver paused");
        assert!(items(ScanSummary::Running { percent: Some(42), resilver: false }, PoolHealth::Online).is_empty(), "a scrub gets no line");
        assert!(items(ScanSummary::Finished { when: when(1), errors: 0 }, PoolHealth::Online).is_empty(), "a finished resilver gets no line");
        let degraded = items(ScanSummary::Running { percent: Some(5), resilver: true }, PoolHealth::Degraded);
        assert_eq!(degraded.len(), 1, "the resilver line shows once next to the pool's own health line");
    }

    fn smart_file(entries: &[(&str, i64, &[(&str, crate::smart::Severity, &str)])]) -> crate::daemon::smart_check::SmartFile {
        use crate::daemon::smart_check::{HostResult, SmartFile};
        let mut file = SmartFile::default();
        for (host, hours_ago, disks) in entries {
            let mut list: Vec<crate::smart::Disk> = Vec::new();
            for (dev, severity, text) in *disks {
                match list.iter_mut().find(|d| d.dev == *dev) {
                    Some(d) => d.problems.push(crate::smart::Problem { severity: *severity, text: (*text).to_string() }),
                    None => list.push(crate::smart::Disk { dev: (*dev).to_string(), problems: vec![crate::smart::Problem { severity: *severity, text: (*text).to_string() }] }),
                }
            }
            file.hosts.insert((*host).to_string(), HostResult { checked_at: now() - Duration::hours(*hours_ago), disks: list, error: None });
        }
        file
    }

    fn disk_items(fx: &Fixture, names: &[&str]) -> Vec<Item> {
        let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
        let probe = reached(vec![]);
        let hosts: Vec<HostInput> = names.iter().map(|n| fine_host(n, &probe, &r, &b)).collect();
        fx.build(hosts).attention.into_iter().filter(|i| i.goto == Section::Disks || i.subject == "checks").collect()
    }

    #[test]
    fn a_host_with_a_failing_disk_is_one_red_line_and_a_healthy_host_none() {
        use crate::smart::Severity::{Issue, Watch};
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        fx.smart = smart_file(&[("h", 3, &[("ada0", Issue, "SMART health FAILED")]), ("ok", 3, &[])]);
        let items = disk_items(&fx, &["h", "ok"]);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!((items[0].level, items[0].subject.as_str(), items[0].goto, items[0].target.as_deref()), (Level::Issue, "h", Section::Disks, Some("h")));
        assert!(items[0].what.contains("ada0 SMART health FAILED") && items[0].what.contains("checked 3h ago"), "{}", items[0].what);
        fx.smart = smart_file(&[("h", 3, &[("ada0", Watch, "reallocated sectors 8"), ("ada4", Watch, "SSD wear 87%")])]);
        let items = disk_items(&fx, &["h"]);
        assert_eq!((items.len(), items[0].level), (1, Level::Watch), "several disks share one yellow line");
        assert!(items[0].what.contains("ada0 reallocated sectors 8; ada4 SSD wear 87%"), "{}", items[0].what);
    }

    #[test]
    fn the_last_good_result_stays_after_a_failed_check_with_its_real_age() {
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        fx.smart = smart_file(&[("h", 30, &[("ada0", crate::smart::Severity::Watch, "pending sectors 2")])]);
        fx.smart.hosts.get_mut("h").unwrap().error = Some("no route".into());
        let items = disk_items(&fx, &["h"]);
        assert_eq!(items.len(), 1);
        assert!(items[0].what.contains("checked 1d ago"), "{}", items[0].what);
    }

    #[test]
    fn a_check_older_than_three_days_while_the_daemon_runs_adds_one_yellow_line() {
        let problem: &[(&str, crate::smart::Severity, &str)] = &[("ada0", crate::smart::Severity::Watch, "pending sectors 2")];
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        fx.smart = smart_file(&[("h", 24 * 4, problem)]);
        let stale: Vec<Item> = disk_items(&fx, &["h"]).into_iter().filter(|i| i.subject == "checks").collect();
        assert_eq!(stale.len(), 1);
        assert_eq!((stale[0].level, stale[0].what.as_str()), (Level::Watch, "disk and shares check is 4 days old"));
        fx.smart.ran_at = Some(now() - Duration::hours(1));
        let still: Vec<Item> = disk_items(&fx, &["h"]).into_iter().filter(|i| i.subject == "checks").collect();
        assert_eq!(still.len(), 1, "the daemon ran an hour ago but no host has answered for 4 days: the check is broken, not fine");
        fx.smart.hosts.get_mut("h").unwrap().checked_at = now() - Duration::hours(24);
        assert!(disk_items(&fx, &["h"]).iter().all(|i| i.subject != "checks"), "a host answered yesterday");
        fx.smart.hosts.get_mut("h").unwrap().checked_at = now() - Duration::days(4);
        fx.smart.ran_at = Some(now() - Duration::days(5));
        let mut off = Fixture::new(view(vec![], &[], None));
        off.smart = fx.smart.clone();
        assert!(disk_items(&off, &["h"]).iter().all(|i| i.subject != "checks"), "the daemon is not running: its own line says so, and this one would only repeat it");
    }

    fn shares_entry(hours_ago: i64, nfs_running: bool, nfs_shares: usize, samba_running: bool, smb_shares: usize, smb_error: Option<&str>) -> crate::daemon::shares_check::SharesResult {
        crate::daemon::shares_check::SharesResult { checked_at: now() - Duration::hours(hours_ago), nfs_running, samba_running, nfs_shares, smb_shares, smb_error: smb_error.map(str::to_string), error: None, nfs_name: None, smb_name: None }
    }

    fn share_items(fx: &Fixture, names: &[&str]) -> Vec<Item> {
        let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
        let probe = reached(vec![]);
        let hosts: Vec<HostInput> = names.iter().map(|n| fine_host(n, &probe, &r, &b)).collect();
        fx.build(hosts).attention.into_iter().filter(|i| i.goto == Section::Shares).collect()
    }

    fn quota_items(fx: &Fixture, names: &[&str]) -> Vec<Item> {
        let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
        let probe = reached(vec![]);
        let hosts: Vec<HostInput> = names.iter().map(|n| fine_host(n, &probe, &r, &b)).collect();
        fx.build(hosts).attention.into_iter().filter(|i| i.goto == Section::Datasets).collect()
    }

    fn storage_entry(minutes_ago: i64, name: &str, used_percent: u64) -> crate::daemon::storage_check::StorageResult {
        use crate::daemon::storage_check::{LimitedDataset, StorageResult};
        StorageResult { checked_at: now() - Duration::minutes(minutes_ago), datasets: vec![LimitedDataset { name: name.into(), used: used_percent, referenced: used_percent, quota: 100, refquota: 0 }], pools: vec![], zfs_version: None, locked: vec![], integrity: Default::default(), integrity_stale: false, error: None }
    }

    #[test]
    fn a_dataset_near_its_quota_is_a_line_that_goes_to_datasets_and_an_unreached_host_has_none() {
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        fx.storage.hosts.insert("h".into(), storage_entry(4, "tank/home", 94));
        fx.storage.hosts.insert("red".into(), storage_entry(4, "tank/db", 99));
        fx.storage.hosts.insert("gone".into(), storage_entry(4, "tank/x", 99));
        let items = quota_items(&fx, &["h", "red"]);
        assert_eq!(items.len(), 2, "{items:?}");
        let by = |s: &str| items.iter().find(|i| i.subject == s).unwrap();
        assert_eq!((by("h").level, by("red").level), (Level::Watch, Level::Issue));
        assert!(by("h").what.starts_with("tank/home 94% of its 100 quota") && by("h").what.contains("checked 4m ago"), "{}", by("h").what);
        assert_eq!(by("h").target.as_deref(), Some("h"));
    }

    #[test]
    fn checksum_off_is_a_watch_line_that_goes_to_datasets_for_reached_hosts_only() {
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        let mut entry = storage_entry(2, "tank/x", 10);
        entry.integrity.checksum_off = vec!["tank/scratch".into()];
        fx.storage.hosts.insert("h".into(), entry.clone());
        fx.storage.hosts.insert("gone".into(), entry);
        let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
        let probe = reached(vec![]);
        let items: Vec<Item> = fx.build(vec![fine_host("h", &probe, &r, &b)]).attention.into_iter().filter(|i| i.what.contains("checksum is off")).collect();
        assert_eq!((items.len(), items[0].level, items[0].goto), (1, Level::Watch, Section::Datasets));
        assert!(items[0].what.starts_with("tank/scratch checksum is off"), "{}", items[0].what);
    }

    #[test]
    fn a_locked_dataset_that_a_job_uses_is_a_watch_line_that_goes_to_datasets() {
        let mut job = Job::test_backup("b", "tank/secure", "hourly", "/r", "/p");
        job.host = "h".into();
        let mut fx = Fixture::new(view(vec![job], &[], beat(1)));
        let mut entry = storage_entry(2, "tank/x", 10);
        entry.locked = vec![crate::daemon::storage_check::LockedDataset { name: "tank/secure".into(), shared: false }, crate::daemon::storage_check::LockedDataset { name: "tank/idle".into(), shared: false }];
        fx.storage.hosts.insert("h".into(), entry.clone());
        fx.storage.hosts.insert("gone".into(), entry);
        let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
        let probe = reached(vec![]);
        let items: Vec<Item> = fx.build(vec![fine_host("h", &probe, &r, &b)]).attention.into_iter().filter(|i| i.what.contains("is locked")).collect();
        assert_eq!((items.len(), items[0].level, items[0].goto), (1, Level::Watch, Section::Datasets));
        assert!(items[0].what.starts_with("tank/secure is locked (a job uses it)   (checked 2m ago)") && !items[0].what.contains("idle"), "{}", items[0].what);
    }

    #[test]
    fn a_pool_with_a_lone_special_disk_is_a_red_line_that_goes_to_pools() {
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        let mut entry = storage_entry(2, "tank/x", 10);
        entry.pools = vec![crate::zfs::pool_classes::PoolClasses { name: "tank".into(), unprotected: vec!["special".into()], log_bad: false, cache_bad: false }];
        fx.storage.hosts.insert("h".into(), entry);
        let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
        let probe = reached(vec![]);
        let items: Vec<Item> = fx.build(vec![fine_host("h", &probe, &r, &b)]).attention.into_iter().filter(|i| i.goto == Section::Pools && i.what.contains("special")).collect();
        assert_eq!((items.len(), items[0].level), (1, Level::Issue));
        assert!(items[0].what.starts_with("tank special vdev has no redundancy"), "{}", items[0].what);
    }

    #[test]
    fn a_quota_check_that_stopped_running_is_one_yellow_line_but_only_while_the_daemon_runs() {
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        fx.storage.ran_at = Some(now() - Duration::minutes(12));
        let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
        let probe = reached(vec![]);
        let stale = |fx: &Fixture| -> Vec<Item> { fx.build(vec![fine_host("h", &probe, &r, &b)]).attention.into_iter().filter(|i| i.what.contains("quota check")).collect() };
        assert!(stale(&fx).is_empty(), "12 minutes is within the 10-minute cadence's slack");
        fx.storage.ran_at = Some(now() - Duration::minutes(45));
        let items = stale(&fx);
        assert_eq!((items.len(), items[0].level), (1, Level::Watch));
        assert!(items[0].what.contains("45m"), "{}", items[0].what);
        let mut down = Fixture::new(view(vec![], &[], beat(1000)));
        down.storage.ran_at = Some(now() - Duration::minutes(45));
        assert!(stale(&down).is_empty(), "no daemon, no complaint about the daemon's check");
    }

    #[test]
    fn the_quota_thresholds_come_from_the_dashboard_settings() {
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        fx.storage.hosts.insert("h".into(), storage_entry(1, "tank/home", 85));
        assert!(quota_items(&fx, &["h"]).is_empty(), "85% is under the default 90");
        fx.thresholds.quota = (80, 95);
        assert_eq!(quota_items(&fx, &["h"]).len(), 1);
    }

    #[test]
    fn a_stopped_nfsd_with_shares_is_one_red_line_that_goes_to_shares_and_a_healthy_host_none() {
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        fx.shares.hosts.insert("h".into(), shares_entry(3, false, 2, true, 0, None));
        fx.shares.hosts.insert("ok".into(), shares_entry(3, true, 2, true, 1, None));
        let items = share_items(&fx, &["h", "ok"]);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!((items[0].level, items[0].subject.as_str(), items[0].target.as_deref()), (Level::Issue, "h", Some("h")));
        assert!(items[0].what.starts_with("NFS: 2 shares but nfsd is not running") && items[0].what.contains("checked 3h ago"), "{}", items[0].what);
    }

    #[test]
    fn an_unreadable_smb_block_is_one_yellow_line() {
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        fx.shares.hosts.insert("h".into(), shares_entry(1, true, 0, true, 0, Some("SMB: the managed block has no end")));
        let items = share_items(&fx, &["h"]);
        assert_eq!((items.len(), items[0].level), (1, Level::Watch));
        assert!(items[0].what.starts_with("Samba: "), "{}", items[0].what);
    }

    #[test]
    fn the_last_good_share_result_stays_after_a_failed_check_with_its_real_age_and_an_unreachable_host_has_no_line() {
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        let mut old = shares_entry(30, false, 1, true, 0, None);
        old.error = Some("no route".into());
        fx.shares.hosts.insert("h".into(), old);
        assert!(share_items(&fx, &["h"])[0].what.contains("checked 1d ago"));
        let down = HostProbe::Disconnected;
        let input = HostInput { name: "h", assessment: assess(Some(&down), false), probe: Some(&down), resources: None, utc_offset_secs: Some(0), os_name: None };
        assert!(fx.build(vec![input]).attention.iter().all(|i| i.goto != Section::Shares), "the Hosts row already says it is unreachable");
    }

    #[test]
    fn the_stale_line_covers_both_files_and_a_missing_file_is_not_stale() {
        let problem: &[(&str, crate::smart::Severity, &str)] = &[("ada0", crate::smart::Severity::Watch, "pending sectors 2")];
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        fx.smart = smart_file(&[("h", 1, problem)]);
        let stale = |fx: &Fixture| disk_items(fx, &["h"]).into_iter().filter(|i| i.subject == "checks").count();
        assert_eq!(stale(&fx), 0, "a fresh smart.json and no shares.json at all: nothing is stale");
        fx.smart = smart_file(&[("h", 24 * 4, problem)]);
        assert_eq!(stale(&fx), 1);
        fx.shares.hosts.insert("h".into(), shares_entry(2, true, 1, true, 0, None));
        assert_eq!(stale(&fx), 0, "the shares check answered 2 hours ago: the checks work");
        let none = Fixture::new(view(vec![], &[], beat(1)));
        assert_eq!(stale(&none), 0, "no data in either file: no line");
    }

    #[test]
    fn a_running_check_shows_a_red_notice_that_is_not_counted_in_the_verdict() {
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
        let probe = reached(vec![]);
        assert_eq!(fx.build(vec![fine_host("h", &probe, &r, &b)]).running, None);
        fx.marker = Some(crate::daemon::checks::Marker { by: "daemon".into(), since: now(), beat: now(), host: None });
        let d = fx.build(vec![fine_host("h", &probe, &r, &b)]);
        assert!(d.running.as_deref().unwrap_or("").contains("daily disk and shares check since"), "{:?}", d.running);
        assert_eq!(d.verdict.text, "all quiet", "a running check is a notice, not a problem");
        fx.marker = Some(crate::daemon::checks::Marker { by: "tui".into(), since: now(), beat: now(), host: Some("fbsd16".into()) });
        assert!(fx.build(vec![]).running.unwrap().starts_with("checking fbsd16: disks and shares since"));
    }

    #[test]
    fn an_unreachable_host_and_a_missing_file_show_no_disk_line_and_the_verdict_counts_the_rest() {
        let mut fx = Fixture::new(view(vec![], &[], beat(1)));
        assert!(disk_items(&fx, &["h"]).is_empty(), "no smart.json");
        fx.smart = smart_file(&[("h", 3, &[("ada0", crate::smart::Severity::Issue, "SMART health FAILED")])]);
        let down = HostProbe::Disconnected;
        let input = HostInput { name: "h", assessment: assess(Some(&down), false), probe: Some(&down), resources: None, utc_offset_secs: Some(0), os_name: None };
        let d = fx.build(vec![input]);
        assert!(d.attention.iter().all(|i| i.goto != Section::Disks), "the Hosts row already says it is unreachable: {:?}", d.attention);
        let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
        let probe = reached(vec![]);
        let d = fx.build(vec![fine_host("h", &probe, &r, &b)]);
        assert!(d.verdict.text.contains("1 issue"), "{}", d.verdict.text);
    }

    #[test]
    fn the_alerts_line_says_whether_mail_can_go_out_and_when_the_last_one_did() {
        let on = Fixture::new(view(vec![], &[], beat(1)));
        let d = on.build(vec![]);
        assert_eq!(d.alerts.level, Level::Fine);
        assert!(d.alerts.text.contains("monitor on"), "{}", d.alerts.text);
        let mut with_report = Fixture::new(view(vec![], &[], beat(1)));
        with_report.alerts.last_report = Some("2026-10-17".into());
        with_report.alerts.events = vec![crate::daemon::health_text::HealthEvent { at: now() - Duration::hours(3), host: "fbsd16".into(), kind: "alert".into(), text: "zroot 86% full".into() }];
        let text = with_report.build(vec![]).alerts.text;
        assert!(text.contains("last alert") && text.contains("fbsd16") && text.contains("report 2026-10-17"), "{text}");
        // configured but the daemon is down: a watch.
        let down = Fixture::new(view(vec![], &[], None)).build(vec![]).alerts;
        assert_eq!(down.level, Level::Watch);
        assert!(down.text.contains("daemon"), "{}", down.text);
        // not configured: unknown, with a hint; an unusable section: watch, with the reason.
        let mut off = Fixture::new(view(vec![], &[], beat(1)));
        off.alerts = alerts(false);
        assert_eq!(off.build(vec![]).alerts.level, Level::Unknown);
        off.alerts.problem = Some("[notify] check_interval_minutes must be at least 1".into());
        let bad = off.build(vec![]).alerts;
        assert_eq!(bad.level, Level::Watch);
        assert!(bad.text.contains("check_interval_minutes"), "{}", bad.text);
    }

    #[test]
    fn the_verdict_says_checking_until_every_host_has_answered_then_counts_issues_and_watches() {
        let fx = Fixture::new(view(vec![scrub_job("s")], &[("s", RunResult::Ok, "ok")], beat(1)));
        let probe = reached(vec![pool("tank", 10, 0, Some(when(1)))]);
        let (r, before) = (reading(10, 0, 10, 1100), reading(10, 0, 0, 1000)); // about 9% busy
        let unanswered = HostInput { name: "fbsd16", assessment: assess(None, false), probe: None, resources: None, utc_offset_secs: None, os_name: None };
        let checking = fx.build(vec![fine_host("local", &probe, &r, &before), unanswered]).verdict;
        assert_eq!((checking.level, checking.text.as_str()), (Level::Unknown, "checking..."));
        let quiet = fx.build(vec![fine_host("local", &probe, &r, &before)]);
        assert_eq!((quiet.verdict.level, quiet.verdict.text.as_str()), (Level::Fine, "all quiet"));
        assert!(quiet.attention.is_empty());
        // one watch (pool 85% full) and one issue (daemon down).
        let busy = reached(vec![pool("tank", 85, 0, Some(when(1)))]);
        let mixed = Fixture::new(view(vec![scrub_job("s")], &[("s", RunResult::Ok, "ok")], None)).build(vec![fine_host("local", &busy, &r, &before)]);
        assert_eq!((mixed.verdict.level, mixed.verdict.text.as_str()), (Level::Issue, "1 issue, 1 to watch"));
        let only_watch = fx.build(vec![fine_host("local", &busy, &r, &before)]).verdict;
        assert_eq!((only_watch.level, only_watch.text.as_str()), (Level::Watch, "1 to watch"));
    }

    #[test]
    fn a_host_that_is_unreachable_is_an_issue_and_one_that_cannot_be_inspected_a_watch() {
        let fx = Fixture::new(view(vec![scrub_job("s")], &[("s", RunResult::Ok, "ok")], beat(1)));
        let down = HostProbe::Disconnected;
        let denied = HostProbe::Unavailable { reason: "sudo: a password is required".into() };
        let input = |name, probe: &'static HostProbe| HostInput { name, assessment: assess(Some(probe), false), probe: Some(probe), resources: None, utc_offset_secs: None, os_name: None };
        let down: &'static HostProbe = Box::leak(Box::new(down));
        let denied: &'static HostProbe = Box::leak(Box::new(denied));
        let d = fx.build(vec![input("fbsd16", down), input("fbsd16t1", denied)]);
        let level_of = |subject: &str| d.attention.iter().find(|i| i.subject == subject).map(|i| i.level);
        assert_eq!((level_of("fbsd16"), level_of("fbsd16t1")), (Some(Level::Issue), Some(Level::Watch)));
        assert_eq!(d.verdict.text, "1 issue, 1 to watch");
    }

    #[test]
    fn attention_is_sorted_worst_first_then_by_subject_and_everything_builds_with_nothing() {
        let jobs = vec![scrub_job("zz-scrub"), scrub_job("aa-scrub")];
        let runs = [("zz-scrub", RunResult::Failed, "boom"), ("aa-scrub", RunResult::Failed, "bang")];
        let busy = reached(vec![pool("tank", 85, 0, Some(when(1)))]);
        let (r, before) = (reading(10, 0, 0, 1000), reading(10, 0, 0, 1000));
        let d = Fixture::new(view(jobs, &runs, beat(1))).build(vec![fine_host("local", &busy, &r, &before)]);
        let order: Vec<(Level, &str)> = d.attention.iter().map(|i| (i.level, i.subject.as_str())).collect();
        assert_eq!(order, [(Level::Issue, "aa-scrub"), (Level::Issue, "zz-scrub"), (Level::Watch, "local")]);
        let empty = Fixture::new(view(vec![], &[], beat(1))).build(vec![]);
        assert_eq!(empty.verdict.level, Level::Fine, "no hosts to doubt");
        let no_daemon = Fixture::new(JobsView::default()).build(vec![]);
        assert_eq!(no_daemon.verdict.level, Level::Issue, "nothing has ever run the daemon: that is the one problem");
        assert!(empty.hosts.is_empty() && empty.cloud.is_empty());
    }

    #[test]
    fn an_unreadable_status_a_resilver_and_scrub_errors_are_not_misreported() {
        let t = Thresholds::default();
        let mut unreadable = pool("a", 10, 0, None);
        unreadable.scan = None;
        assert_eq!(pool_scrub(&unreadable, Some(0), &t, now()).0, Level::Unknown, "no zpool status is unknown, not 'never scrubbed'");
        let mut resilvered = pool("b", 10, 0, None);
        resilvered.scan = Some(ScanSummary::Finished { when: when(1), errors: 0 });
        let (level, _, _, note) = pool_scrub(&resilvered, Some(0), &t, now());
        assert_eq!(level, Level::Watch);
        assert!(!note.unwrap().contains("never scrubbed"));
        let mut bad = pool("c", 10, 0, Some(when(2)));
        bad.scan = Some(ScanSummary::Finished { when: when(2), errors: 3 });
        let (level, _, _, note) = pool_scrub(&bad, Some(0), &t, now());
        assert_eq!(level, Level::Watch);
        assert_eq!(note.unwrap(), "c last scrub found 3 errors");
    }
}
