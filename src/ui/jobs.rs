//! The Jobs section's data and the pure helpers its table and form use.
//!
//! Everything here reads or computes; nothing here draws or handles keys.

use crate::daemon::cron::Schedule;
use crate::daemon::heartbeat::{self, Heartbeat};
use crate::daemon::history::{self, History, Run, RunResult};
use crate::jobs::{self, Job};
use chrono::{DateTime, TimeZone, Utc};
use std::collections::HashMap;
use std::path::Path;

/// What a new job starts with: weekly, Sunday at 03:00.
pub const DEFAULT_SCHEDULE: &str = "0 3 * * 0";

const PRESETS: [(&str, &str); 4] = [
    ("0 * * * *", "Hourly"),
    ("0 2 * * *", "Daily, 02:00"),
    (DEFAULT_SCHEDULE, "Weekly, Sunday 03:00"),
    ("0 3 1 * *", "Monthly, 1st 03:00"),
];

/// The `F2` list for the Schedule field: the cron text, then its label.
pub fn preset_values() -> Vec<String> {
    PRESETS.iter().map(|(cron, label)| format!("{cron}  ({label})")).collect()
}

/// What a picked value puts into the field: the cron part, without the label.
pub fn insert_text(picked: &str) -> &str {
    picked.split_once("  (").map_or(picked, |(cron, _)| cron)
}

/// The preset's name for `expr`, or `custom schedule`.
pub fn describe(expr: &str) -> String {
    let expr = expr.trim();
    PRESETS.iter().find(|(cron, _)| *cron == expr).map_or_else(|| "custom schedule".to_string(), |(_, label)| (*label).to_string())
}

/// `45s`, `3m 05s`, `2h 10m`.
pub fn duration_text(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3_600, secs % 3_600 / 60)
    }
}

/// One line of a job's run list: when it started (in `tz`), how it ended, how long it took, and
/// the first line of its message.
pub fn run_summary<Tz: TimeZone>(run: &Run, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let when = run.started_at.with_timezone(tz).format("%m-%d %H:%M");
    let word = match run.result {
        None => "running",
        Some(RunResult::Ok) => "ok",
        Some(RunResult::Failed) => "failed",
        Some(RunResult::Skipped) => "skipped",
        Some(RunResult::Cancelled) => "cancelled",
    };
    let duration = run.finished_at.map_or_else(|| "-".to_string(), |end| duration_text((end - run.started_at).num_seconds()));
    let mut line = format!("{when}  {word:<7}  {duration}");
    let message = run.message.lines().next().unwrap_or("").trim();
    if !message.is_empty() {
        line.push_str("  ");
        line.push_str(message);
    }
    line
}

/// The table's schedule column: a preset's name, or the cron text of a custom schedule.
pub fn schedule_label(expr: &str) -> String {
    let described = describe(expr);
    if described == "custom schedule" { expr.trim().to_string() } else { described }
}

/// The table's next-run column: day and time in `now`'s zone, or `never`.
pub fn next_run_text<Tz: TimeZone>(expr: &str, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    Schedule::parse(expr).ok().and_then(|s| s.next_after(now)).map_or_else(|| "never".to_string(), |t| t.format("%a %m-%d %H:%M").to_string())
}

/// The form's preview under the Schedule field: what the schedule is called
/// and its next three run times in `now`'s zone, or why it does not parse.
pub fn preview_lines<Tz: TimeZone>(expr: &str, now: &DateTime<Tz>) -> Vec<String>
where
    Tz::Offset: std::fmt::Display,
{
    let schedule = match Schedule::parse(expr) {
        Ok(s) => s,
        Err(e) => return vec![format!("Schedule error: {e}")],
    };
    let mut times = Vec::new();
    let mut cursor = now.clone();
    for _ in 0..3 {
        let Some(next) = schedule.next_after(&cursor) else { break };
        times.push(next.format("%a %Y-%m-%d %H:%M").to_string());
        cursor = next;
    }
    let next = if times.is_empty() { "never".to_string() } else { times.join(", ") };
    vec![describe(expr), format!("Next: {next}")]
}

fn safe_key(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '-' }).collect()
}

/// `base`, with `-2`, `-3`, ... added until it is not in `existing`.
fn unique(base: &str, existing: &[String]) -> String {
    let mut candidate = base.to_string();
    let mut n = 1_u32;
    while existing.contains(&candidate) {
        n += 1;
        candidate = format!("{base}-{n}");
    }
    candidate
}

/// `scrub-<host>-<pool>`, made safe for a file key, with `-2`, `-3`, ... added if taken.
pub fn new_job_id(host: &str, pool: &str, existing: &[String]) -> String {
    unique(&format!("scrub-{}-{}", safe_key(host), safe_key(pool)), existing)
}

/// `repl-<host>-<dataset>-to-<dest host>-<dest>`, made safe the same way.
pub fn new_replicate_job_id(host: &str, dataset: &str, dest_host: &str, dest: &str, existing: &[String]) -> String {
    unique(&format!("repl-{}-{}-to-{}-{}", safe_key(host), safe_key(dataset), safe_key(dest_host), safe_key(dest)), existing)
}

/// `backup-<host>-<dataset>`, made safe the same way.
pub fn new_backup_job_id(host: &str, dataset: &str, existing: &[String]) -> String {
    unique(&format!("backup-{}-{}", safe_key(host), safe_key(dataset)), existing)
}

/// How a backup job's Source field reads: `new`, `@snapshot`, or the prefix.
pub fn source_text(job: &Job) -> String {
    match jobs::backup_source(job) {
        jobs::BackupSource::Fresh => jobs::SOURCE_NEW.to_string(),
        jobs::BackupSource::Pinned(name) => format!("@{name}"),
        jobs::BackupSource::Prefix(prefix) => prefix.to_string(),
    }
}

/// What the Jobs panel says a backup job backs up, e.g. `prefix hourly`, `snapshot snap4` or
/// `new snapshot each run, keep 3`, then its path if it has one.
pub fn source_summary(job: &Job) -> String {
    let what = match jobs::backup_source(job) {
        jobs::BackupSource::Fresh => format!("new snapshot each run, keep {}", job.keep.unwrap_or(jobs::DEFAULT_FRESH_KEEP)),
        jobs::BackupSource::Pinned(name) => format!("snapshot {name}"),
        jobs::BackupSource::Prefix(prefix) => format!("prefix {prefix}"),
    };
    match job.path.as_deref() {
        Some(path) => format!("{what}  path {path}"),
        None => what,
    }
}

/// `(source, snapshot, prefix)` of a backup job.
pub type SourceFields = (Option<String>, Option<String>, Option<String>);

/// The three job fields a backup form's Source text stands for: `(source, snapshot, prefix)`. `new` takes
/// a fresh snapshot each run, `@name` pins one snapshot, anything else is the prefix of the snapshots to
/// use (blank is the default prefix).
pub fn parse_source(text: &str) -> std::result::Result<SourceFields, String> {
    let text = text.trim();
    if text == jobs::SOURCE_NEW {
        return Ok((Some(jobs::SOURCE_NEW.to_string()), None, None));
    }
    if let Some(name) = text.strip_prefix('@') {
        if !jobs::valid_snapshot_name(name) {
            return Err("Source @name: a snapshot name may only use letters, digits, _ - . and :".to_string());
        }
        return Ok((Some(jobs::SOURCE_SNAPSHOT.to_string()), Some(name.to_string()), None));
    }
    let prefix = if text.is_empty() { jobs::DEFAULT_PREFIX } else { text };
    if !jobs::valid_prefix(prefix) {
        return Err("Source: use new, a prefix (letters, digits, _ - and .), or @snapshot".to_string());
    }
    Ok((None, None, Some(prefix.to_string())))
}

/// `snap-<host>-<dataset>-<prefix>`, made safe the same way.
pub fn new_snapshot_job_id(host: &str, dataset: &str, prefix: &str, existing: &[String]) -> String {
    unique(&format!("snap-{}-{}-{}", safe_key(host), safe_key(dataset), safe_key(prefix)), existing)
}

/// What the Jobs table's Target column shows: the pool or dataset, and for a replication or a backup where
/// its data goes (a host and dataset, a hub directory, or the restic repository, which by contract holds no
/// credentials).
pub fn job_target(job: &Job) -> String {
    let source = job.pool.clone().or_else(|| job.dataset.clone()).unwrap_or_default();
    match job.kind.as_str() {
        jobs::KIND_REPLICATE => match job.dest_host.as_deref() {
            Some(jobs::DEST_FILE) => format!("{source} → file:{}", job.dest_dir.as_deref().unwrap_or("")),
            Some(host) => format!("{source} → {host}:{}", job.dest_dataset.as_deref().unwrap_or("")),
            None => source,
        },
        jobs::KIND_BACKUP => format!("{source} → {}", job.repository.as_deref().unwrap_or("")),
        _ => source,
    }
}

/// `just now`, `45m ago`, `3h ago`, `2d ago`. A time in the future reads as `just now`.
pub fn age_text(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// The last-result column: `never`, `running`, or the result and its age.
pub fn last_result_text(run: Option<&Run>, now: DateTime<Utc>) -> String {
    let Some(run) = run else { return "never".to_string() };
    let Some(result) = run.result else { return "running".to_string() };
    let word = match result {
        RunResult::Ok => "ok",
        RunResult::Failed => "failed",
        RunResult::Skipped => "skipped",
        RunResult::Cancelled => "cancelled",
    };
    format!("{word} {}", age_text(now, run.finished_at.unwrap_or(run.started_at)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonState {
    Running { pid: u32 },
    NotRunning,
}

/// Everything the Jobs section shows, read from the daemon's state directory.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct JobsView {
    pub jobs: Vec<Job>,
    pub runs: Vec<Run>,
    pub heartbeat: Option<Heartbeat>,
    /// Why a job cannot run, by job id.
    pub unsupported: HashMap<String, String>,
    /// Set when `jobs.toml` could not be read; `jobs` then holds the last good list.
    pub error: Option<String>,
    /// False until the first load, so the section does not claim "no jobs" before it has looked.
    pub loaded: bool,
    /// Live progress of running transfers, read from `<state>/progress`.
    pub progress: Vec<crate::daemon::progress::Progress>,
}

/// Reads `jobs.toml`, `history.jsonl` and `daemon.json` from `state_dir`. If
/// `jobs.toml` cannot be read the list from `previous` is kept and the error recorded.
pub fn load(state_dir: &Path, hosts: &[String], previous: Option<&JobsView>) -> JobsView {
    let (jobs, error) = match jobs::load(&state_dir.join("jobs.toml")) {
        Ok(list) => (list, None),
        Err(e) => (previous.map(|p| p.jobs.clone()).unwrap_or_default(), Some(e.to_string())),
    };
    let clashes = jobs::duplicate_prefix_reasons(&jobs);
    let unsupported = jobs.iter().filter_map(|j| jobs::resolve(j, hosts).err().or_else(|| clashes.get(&j.id).cloned()).map(|reason| (j.id.clone(), reason))).collect();
    JobsView {
        runs: history::runs(&History::new(state_dir.join("history.jsonl")).read()),
        heartbeat: heartbeat::read(&state_dir.join("daemon.json")),
        jobs,
        unsupported,
        error,
        loaded: true,
        progress: crate::daemon::progress::read_all(&state_dir.join("progress")),
    }
}

impl JobsView {
    /// The live progress of the job's running transfer, if it has one.
    pub fn progress_of(&self, job_id: &str) -> Option<&crate::daemon::progress::Progress> {
        self.progress.iter().find(|p| p.job_id == job_id)
    }

    /// Whether any run of the job has started and not finished. (Not just the latest: a slot that
    /// falls due during a long transfer is recorded as skipped and would otherwise hide it.)
    pub fn is_running(&self, job_id: &str) -> bool {
        self.runs.iter().any(|r| r.job_id == job_id && r.result.is_none())
    }

    /// The most recently started run of `id`.
    pub fn last_run(&self, id: &str) -> Option<&Run> {
        self.runs.iter().filter(|r| r.job_id == id).max_by_key(|r| r.started_at)
    }

    /// Up to `n` runs of `id`, newest first.
    pub fn runs_of(&self, id: &str, n: usize) -> Vec<&Run> {
        let mut runs: Vec<&Run> = self.runs.iter().filter(|r| r.job_id == id).collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.started_at));
        runs.truncate(n);
        runs
    }

    /// Enabled jobs whose latest run failed.
    pub fn failed_count(&self) -> usize {
        self.jobs.iter().filter(|j| j.enabled && self.last_run(&j.id).is_some_and(|r| r.result == Some(RunResult::Failed))).count()
    }

    pub fn daemon(&self, now: DateTime<Utc>) -> DaemonState {
        self.heartbeat.as_ref().filter(|h| h.is_fresh(now)).map_or(DaemonState::NotRunning, |h| DaemonState::Running { pid: h.pid })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::heartbeat::Heartbeat;
    use crate::daemon::history::{Event, EventKind, History, RunResult};
    use crate::jobs::{self, Job, KIND_SCRUB};
    use chrono::FixedOffset;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn job(id: &str) -> Job {
        Job { id: id.into(), host: "local".into(), kind: KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * 0".into(), catch_up: true, enabled: true, created_at: None, dataset: None, recursive: false, prefix: None, keep: None, dest_host: None, dest_dataset: None, dest_dir: None, force: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None }
    }

    fn event(job_id: &str, run_id: &str, kind: EventKind, when: &str, result: Option<RunResult>) -> Event {
        Event { job_id: job_id.into(), run_id: run_id.into(), event: kind, at: at(when), scheduled_for: Some(at(when)), result, message: "msg".into(), catch_up: false, summary: None }
    }

    fn hosts() -> Vec<String> {
        vec!["local".to_string(), "fbsd16".to_string()]
    }

    #[test]
    fn the_source_text_maps_to_the_job_fields_and_back() {
        assert_eq!(parse_source("new"), Ok((Some("new".into()), None, None)));
        assert_eq!(parse_source(" @snap4 "), Ok((Some("snapshot".into()), Some("snap4".into()), None)));
        assert_eq!(parse_source("hourly"), Ok((None, None, Some("hourly".into()))));
        assert_eq!(parse_source(""), Ok((None, None, Some("auto".into()))));
        for bad in ["@", "@a b", "@-x", "@a/b", "a b", "a@b"] {
            assert!(parse_source(bad).is_err(), "{bad:?}");
        }
        let mut j = Job::test_backup("b", "tank/data", "hourly", "/r", "/p");
        assert_eq!(source_text(&j), "hourly");
        j.source = Some("new".into());
        assert_eq!(source_text(&j), "new");
        j.source = Some("snapshot".into());
        j.snapshot = Some("snap4".into());
        assert_eq!(source_text(&j), "@snap4");
    }

    #[test]
    fn the_panel_summary_names_the_source_and_the_path() {
        let mut j = Job::test_backup("b", "tank/data", "hourly", "/r", "/p");
        assert_eq!(source_summary(&j), "prefix hourly");
        j.path = Some("photos".into());
        assert_eq!(source_summary(&j), "prefix hourly  path photos");
        j.source = Some("new".into());
        assert_eq!(source_summary(&j), "new snapshot each run, keep 3  path photos");
        j.keep = Some(5);
        j.path = None;
        assert_eq!(source_summary(&j), "new snapshot each run, keep 5");
        j.source = Some("snapshot".into());
        j.snapshot = Some("snap4".into());
        assert_eq!(source_summary(&j), "snapshot snap4");
    }

    #[test]
    fn presets_carry_a_label_and_the_cron_part_is_what_gets_inserted() {
        let values = preset_values();
        assert_eq!(values[2], "0 3 * * 0  (Weekly, Sunday 03:00)");
        assert_eq!(insert_text(&values[2]), "0 3 * * 0");
        assert_eq!(insert_text("0 5 * * *"), "0 5 * * *", "a plain value is inserted as typed");
        assert!(values.iter().all(|v| crate::daemon::cron::Schedule::parse(insert_text(v)).is_ok()), "every preset is a valid schedule");
        assert!(crate::daemon::cron::Schedule::parse(DEFAULT_SCHEDULE).is_ok());
    }

    #[test]
    fn describe_names_a_preset_and_calls_anything_else_custom() {
        assert_eq!(describe("0 3 * * 0"), "Weekly, Sunday 03:00");
        assert_eq!(describe("  0 3 * * 0  "), "Weekly, Sunday 03:00", "surrounding space does not matter");
        assert_eq!(describe("*/10 * * * *"), "custom schedule");
    }

    #[test]
    fn the_preview_lists_the_next_three_run_times_in_the_given_zone() {
        let est = FixedOffset::west_opt(5 * 3600).unwrap();
        let now = at("2026-10-06T10:00:00Z").with_timezone(&est); // Tue 05:00 local
        let lines = preview_lines("0 3 * * 0", &now);
        assert_eq!(lines[0], "Weekly, Sunday 03:00");
        assert_eq!(lines[1], "Next: Sun 2026-10-11 03:00, Sun 2026-10-18 03:00, Sun 2026-10-25 03:00");
    }

    #[test]
    fn the_preview_reports_a_bad_schedule_and_an_impossible_one() {
        let now = at("2026-10-06T10:00:00Z");
        let bad = preview_lines("61 * * * *", &now);
        assert_eq!(bad.len(), 1);
        assert!(bad[0].starts_with("Schedule error:") && bad[0].contains("outside 0-59"), "{bad:?}");
        let never = preview_lines("0 0 31 2 *", &now);
        assert_eq!(never[1], "Next: never");
        assert!(preview_lines("", &now)[0].starts_with("Schedule error:"));
    }

    #[test]
    fn new_job_ids_are_readable_and_never_collide() {
        assert_eq!(new_job_id("local", "tank", &[]), "scrub-local-tank");
        let taken = vec!["scrub-local-tank".to_string(), "scrub-local-tank-2".to_string()];
        assert_eq!(new_job_id("local", "tank", &taken), "scrub-local-tank-3");
        assert_eq!(new_job_id("fbsd16", "my pool", &[]), "scrub-fbsd16-my-pool", "anything outside [A-Za-z0-9_.-] becomes a dash");
    }

    #[test]
    fn ages_read_naturally() {
        let now = at("2026-10-06T12:00:00Z");
        assert_eq!(age_text(now, at("2026-10-06T11:59:30Z")), "just now");
        assert_eq!(age_text(now, at("2026-10-06T11:15:00Z")), "45m ago");
        assert_eq!(age_text(now, at("2026-10-06T09:00:00Z")), "3h ago");
        assert_eq!(age_text(now, at("2026-10-03T12:00:00Z")), "3d ago");
        assert_eq!(age_text(now, at("2026-10-06T13:00:00Z")), "just now", "a time in the future is not negative");
    }

    #[test]
    fn load_reads_jobs_runs_and_the_heartbeat_and_explains_jobs_that_cannot_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut bad = job("bad");
        bad.host = "gone".into();
        jobs::save(&dir.path().join("jobs.toml"), &[job("ok"), bad]).unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        h.append(&event("ok", "r1", EventKind::Finished, "2026-10-05T03:00:00Z", Some(RunResult::Ok))).unwrap();
        crate::daemon::heartbeat::write(&dir.path().join("daemon.json"), &Heartbeat { pid: 42, started_at: at("2026-10-01T00:00:00Z"), last_tick: at("2026-10-06T12:00:00Z") }).unwrap();
        let view = load(dir.path(), &hosts(), None);
        assert_eq!(view.jobs.len(), 2);
        assert!(view.error.is_none() && view.loaded);
        assert_eq!(view.runs.len(), 1);
        assert!(view.unsupported["bad"].contains("'gone' is not in config.toml"));
        assert!(!view.unsupported.contains_key("ok"));
        assert_eq!(view.daemon(at("2026-10-06T12:00:05Z")), DaemonState::Running { pid: 42 });
        assert_eq!(view.daemon(at("2026-10-06T12:01:00Z")), DaemonState::NotRunning, "a heartbeat older than ten seconds is stale");
    }

    #[test]
    fn a_missing_heartbeat_means_the_daemon_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let view = load(dir.path(), &hosts(), None);
        assert_eq!(view.daemon(at("2026-10-06T12:00:00Z")), DaemonState::NotRunning);
        assert!(view.jobs.is_empty() && view.error.is_none());
    }

    #[test]
    fn an_unreadable_jobs_file_keeps_the_previous_data_and_reports_the_error() {
        let dir = tempfile::tempdir().unwrap();
        jobs::save(&dir.path().join("jobs.toml"), &[job("a")]).unwrap();
        let first = load(dir.path(), &hosts(), None);
        std::fs::write(dir.path().join("jobs.toml"), "this is [[[ not toml").unwrap();
        let second = load(dir.path(), &hosts(), Some(&first));
        assert_eq!(second.jobs, first.jobs, "the list does not vanish because of a typo");
        assert!(second.error.as_deref().is_some_and(|e| e.contains("parsing")), "{:?}", second.error);
    }

    #[test]
    fn last_run_runs_of_and_the_failed_count_use_the_newest_run_of_each_enabled_job() {
        let dir = tempfile::tempdir().unwrap();
        let mut off = job("off");
        off.enabled = false;
        jobs::save(&dir.path().join("jobs.toml"), &[job("a"), job("b"), job("c"), off]).unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        h.append(&event("a", "a1", EventKind::Finished, "2026-10-01T03:00:00Z", Some(RunResult::Failed))).unwrap();
        h.append(&event("a", "a2", EventKind::Finished, "2026-10-02T03:00:00Z", Some(RunResult::Ok))).unwrap();
        h.append(&event("b", "b1", EventKind::Finished, "2026-10-01T03:00:00Z", Some(RunResult::Ok))).unwrap();
        h.append(&event("b", "b2", EventKind::Finished, "2026-10-02T03:00:00Z", Some(RunResult::Failed))).unwrap();
        h.append(&event("off", "o1", EventKind::Finished, "2026-10-02T03:00:00Z", Some(RunResult::Failed))).unwrap();
        let view = load(dir.path(), &hosts(), None);
        assert_eq!(view.last_run("a").unwrap().run_id, "a2");
        assert!(view.last_run("c").is_none());
        assert_eq!(view.failed_count(), 1, "only b: a recovered, c never ran, off is disabled");
        assert_eq!(view.runs_of("a", 8).iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(), vec!["a2", "a1"], "newest first");
        assert_eq!(view.runs_of("a", 1).len(), 1);
    }

    #[test]
    fn last_result_text_covers_never_running_ok_failed_and_skipped() {
        let now = at("2026-10-06T12:00:00Z");
        let dir = tempfile::tempdir().unwrap();
        jobs::save(&dir.path().join("jobs.toml"), &[job("a")]).unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        assert_eq!(last_result_text(None, now), "never");
        h.append(&event("a", "r1", EventKind::Started, "2026-10-06T09:00:00Z", None)).unwrap();
        let running = load(dir.path(), &hosts(), None);
        assert_eq!(last_result_text(running.last_run("a"), now), "running");
        h.append(&event("a", "r1", EventKind::Finished, "2026-10-06T09:30:00Z", Some(RunResult::Ok))).unwrap();
        let ok = load(dir.path(), &hosts(), None);
        assert_eq!(last_result_text(ok.last_run("a"), now), "ok 2h ago");
        h.append(&event("a", "r2", EventKind::Skipped, "2026-10-06T11:00:00Z", Some(RunResult::Skipped))).unwrap();
        assert_eq!(last_result_text(load(dir.path(), &hosts(), None).last_run("a"), now), "skipped 1h ago");
        h.append(&event("a", "r3", EventKind::Finished, "2026-10-04T11:00:00Z", Some(RunResult::Failed))).unwrap();
        assert_eq!(last_result_text(load(dir.path(), &hosts(), None).last_run("a"), now), "skipped 1h ago", "the newest by start time wins, not by file order");
    }

    #[test]
    fn the_table_shows_a_presets_name_or_the_cron_text_of_a_custom_schedule() {
        assert_eq!(schedule_label("0 3 * * 0"), "Weekly, Sunday 03:00");
        assert_eq!(schedule_label(" */10 * * * * "), "*/10 * * * *");
    }

    #[test]
    fn the_next_run_column_gives_a_day_and_time_or_never() {
        let est = FixedOffset::west_opt(5 * 3600).unwrap();
        let now = at("2026-10-06T10:00:00Z").with_timezone(&est);
        assert_eq!(next_run_text("0 3 * * 0", &now), "Sun 10-11 03:00");
        assert_eq!(next_run_text("0 0 31 2 *", &now), "never");
        assert_eq!(next_run_text("bogus", &now), "never");
    }

    #[test]
    fn durations_read_in_the_two_largest_units() {
        assert_eq!(duration_text(0), "0s");
        assert_eq!(duration_text(45), "45s");
        assert_eq!(duration_text(185), "3m 05s");
        assert_eq!(duration_text(7_800), "2h 10m");
        assert_eq!(duration_text(-5), "0s", "a clock that stepped back is not negative");
    }

    #[test]
    fn a_run_summary_shows_when_how_it_ended_how_long_it_took_and_why() {
        let est = FixedOffset::west_opt(5 * 3600).unwrap();
        let dir = tempfile::tempdir().unwrap();
        jobs::save(&dir.path().join("jobs.toml"), &[job("a")]).unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        let mut started = event("a", "r1", EventKind::Started, "2026-10-06T08:00:00Z", None);
        started.message = String::new();
        h.append(&started).unwrap();
        let running = load(dir.path(), &hosts(), None);
        assert_eq!(run_summary(running.last_run("a").unwrap(), &est), "10-06 03:00  running  -");
        let mut done = event("a", "r1", EventKind::Finished, "2026-10-06T08:03:05Z", Some(RunResult::Failed));
        done.message = "scrub repaired 12K with 3 errors\nsecond line".into();
        h.append(&done).unwrap();
        let finished = load(dir.path(), &hosts(), None);
        assert_eq!(run_summary(finished.last_run("a").unwrap(), &est), "10-06 03:00  failed   3m 05s  scrub repaired 12K with 3 errors");
    }

    #[test]
    fn load_reports_the_prefix_clash_as_a_reason_on_the_later_job() {
        let dir = tempfile::tempdir().unwrap();
        jobs::save(&dir.path().join("jobs.toml"), &[Job::test_snapshot("a", "tank/data", "hourly", 3), Job::test_snapshot("b", "tank/data", "hourly", 5)]).unwrap();
        let view = load(dir.path(), &["local".to_string()], None);
        assert!(!view.unsupported.contains_key("a"));
        assert!(view.unsupported["b"].contains("prune each other"));
    }

    #[test]
    fn snapshot_job_ids_are_safe_file_keys_and_unique() {
        assert_eq!(new_snapshot_job_id("local", "tank/data", "hourly", &[]), "snap-local-tank-data-hourly");
        let taken = vec!["snap-local-tank-data-hourly".to_string()];
        assert_eq!(new_snapshot_job_id("local", "tank/data", "hourly", &taken), "snap-local-tank-data-hourly-2");
    }

    #[test]
    fn a_cancelled_run_reads_as_cancelled_and_is_not_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        jobs::save(&dir.path().join("jobs.toml"), &[job("a")]).unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        h.append(&event("a", "r1", EventKind::Finished, "2026-10-06T09:30:00Z", Some(RunResult::Cancelled))).unwrap();
        let view = load(dir.path(), &hosts(), None);
        assert_eq!(view.failed_count(), 0, "a deliberate cancel is not a failure");
        let run = view.last_run("a").unwrap();
        assert!(last_result_text(Some(run), at("2026-10-06T09:35:00Z")).starts_with("cancelled"));
        assert!(run_summary(run, &Utc).contains("cancelled"));
    }

    #[test]
    fn replicate_job_ids_are_safe_and_unique() {
        assert_eq!(new_replicate_job_id("local", "tank/data", "fbsd16", "backup/data", &[]), "repl-local-tank-data-to-fbsd16-backup-data");
        let taken = vec!["repl-local-tank-data-to-fbsd16-backup-data".to_string()];
        assert_eq!(new_replicate_job_id("local", "tank/data", "fbsd16", "backup/data", &taken), "repl-local-tank-data-to-fbsd16-backup-data-2");
    }

    #[test]
    fn load_reads_progress_files_and_knows_which_jobs_are_running() {
        let dir = tempfile::tempdir().unwrap();
        jobs::save(&dir.path().join("jobs.toml"), &[Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data")]).unwrap();
        let p = crate::daemon::progress::Progress { job_id: "r".into(), bytes: 5, total: Some(10), started_at: Utc::now(), updated_at: Utc::now(), rate: None };
        crate::daemon::progress::write(&dir.path().join("progress"), "run1", &p).unwrap();
        let view = load(dir.path(), &["local".to_string(), "fbsd16".to_string()], None);
        assert_eq!(view.progress_of("r").map(|p| p.bytes), Some(5));
        assert!(view.progress_of("other").is_none());
        assert!(!view.is_running("r"), "no run in the history yet");
        let h = History::new(dir.path().join("history.jsonl"));
        h.append(&event("r", "r1", EventKind::Started, "2026-10-06T09:00:00Z", None)).unwrap();
        assert!(load(dir.path(), &["local".to_string(), "fbsd16".to_string()], None).is_running("r"));
    }

    #[test]
    fn a_job_is_running_while_any_of_its_runs_is_unfinished_even_if_a_later_slot_was_skipped() {
        let dir = tempfile::tempdir().unwrap();
        jobs::save(&dir.path().join("jobs.toml"), &[Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data")]).unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        h.append(&event("r", "r1", EventKind::Started, "2026-10-06T09:00:00Z", None)).unwrap();
        h.append(&event("r", "r2", EventKind::Skipped, "2026-10-06T10:00:00Z", Some(RunResult::Skipped))).unwrap();
        let view = load(dir.path(), &["local".to_string(), "fbsd16".to_string()], None);
        assert!(view.is_running("r"), "the 09:00 transfer is still going");
        h.append(&event("r", "r1", EventKind::Finished, "2026-10-06T11:00:00Z", Some(RunResult::Ok))).unwrap();
        assert!(!load(dir.path(), &["local".to_string(), "fbsd16".to_string()], None).is_running("r"));
    }

    #[test]
    fn backup_job_ids_are_safe_and_unique() {
        assert_eq!(new_backup_job_id("local", "tank/data", &[]), "backup-local-tank-data");
        assert_eq!(new_backup_job_id("local", "tank/data", &["backup-local-tank-data".to_string()]), "backup-local-tank-data-2");
        assert!(new_backup_job_id("my host", "tank/we ird", &[]).chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')));
    }


    #[test]
    fn the_target_names_where_the_data_goes() {
        assert_eq!(job_target(&Job::test_snapshot("s", "tank/data", "auto", 3)), "tank/data");
        assert_eq!(job_target(&job("x")), "tank");
        assert_eq!(job_target(&Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data")), "tank/data → fbsd16:backup/data");
        let mut file = Job::test_replicate("r", "tank/data", "hourly", "file", "x");
        file.dest_dataset = None;
        file.dest_dir = Some("/mnt/usb/streams".to_string());
        assert_eq!(job_target(&file), "tank/data → file:/mnt/usb/streams");
        assert_eq!(job_target(&Job::test_backup("b", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw")), "tank/data → /mnt/usb/restic");
    }
}
