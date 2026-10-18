//! The words of an alert and of the daily report, built from plain data (no clock, no I/O but the event log).

use super::deliver::{ascii_line, Message};
use super::health::{Alert, AlertKind, Severity};
use super::history::{Run, RunResult};
use crate::jobs::Job;
use crate::zfs::host_health::{jobs_cell, pools_cell, scan_cell, used_cell, HostKind, Level, PoolProbe};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

/// How many entries of one report list are shown before `(+N more)`.
const LIST_MAX: usize = 40;
/// The event log keeps this many lines, trimmed back to it once it holds `EVENTS_TRIM_AT`.
const EVENTS_KEEP: usize = 2000;
const EVENTS_TRIM_AT: usize = 2200;

/// What the monitor knows about one host at one probe.
#[derive(Debug, Clone)]
pub struct HostRow {
    pub name: String,
    pub address: String,
    pub level: Level,
    pub reason: String,
    pub kind: Option<HostKind>,
    pub tag: String,
    pub pools: Vec<PoolProbe>,
    pub rtt_ms: Option<u64>,
    pub jobs_failed: usize,
    pub jobs_total: usize,
}

fn level_word(level: Level) -> &'static str {
    match level {
        Level::Ok => "ok",
        Level::Warning => "warning",
        Level::Critical | Level::Disconnected => "critical",
        Level::Probing => "unknown",
    }
}

fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Ok => "ok",
        Severity::Warning => "warning",
        Severity::Critical => "critical",
    }
}

/// `6h 2m`, `1h 30m`, `45m`, `3d 2h`; a zero part is dropped.
fn dur(d: Duration) -> String {
    let minutes = d.as_secs() / 60;
    let (days, hours, mins) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (days, hours, mins) {
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, 0, _) => format!("{d}d"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

fn utc(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// `text` cut to `width` characters with an ellipsis, then padded (`left`) or right-aligned.
fn cell(text: &str, width: usize, right: bool) -> String {
    let n = text.chars().count();
    let shown: String = if n > width { text.chars().take(width.saturating_sub(1)).chain(std::iter::once('…')).collect() } else { text.to_string() };
    if right { format!("{shown:>width$}") } else { format!("{shown:<width$}") }
}

fn table_header() -> String {
    [cell("HOST", 12, false), cell("ADDRESS", 16, false), cell("STATE", 9, false), cell("TYPE", 6, false), cell("TAG", 8, false), cell("USED", 5, true), cell("POOLS", 5, true), cell("SCRUB", 13, false), cell("JOBS", 5, true)].join(" ")
}

fn table_row(r: &HostRow) -> String {
    let (used, pools, scan) = if r.pools.is_empty() { ("-".to_string(), "-".to_string(), "-".to_string()) } else { (used_cell(&r.pools).0, pools_cell(&r.pools), scan_cell(&r.pools)) };
    [
        cell(&ascii_line(&r.name), 12, false),
        cell(&ascii_line(&r.address), 16, false),
        cell(level_word(r.level), 9, false),
        cell(r.kind.as_ref().map_or("-", HostKind::word), 6, false),
        cell(&ascii_line(&r.tag), 8, false),
        cell(&used, 5, true),
        cell(&pools, 5, true),
        cell(&scan, 13, false),
        cell(&jobs_cell(r.jobs_failed, r.jobs_total).0, 5, true),
    ]
    .join(" ")
}

fn pool_lines(pools: &[PoolProbe]) -> Vec<String> {
    pools
        .iter()
        .map(|p| {
            let frag = p.frag_percent.map_or("-".to_string(), |f| format!("{f}%"));
            let errors = p.errors.map_or("-".to_string(), |e| e.to_string());
            format!("  {:<12} {}%  frag {frag}  errors {errors}  scrub {}", p.name, p.capacity_percent, scan_cell(std::slice::from_ref(p)))
        })
        .collect()
}

/// The message for one alert, escalation, reminder or recovery.
pub fn alert_message(row: &HostRow, alert: &Alert, now: DateTime<Utc>, hub: &str) -> Message {
    let host = ascii_line(&alert.host);
    let reason = ascii_line(&alert.reason);
    let bad_for = alert.bad_for.map(dur);
    let (subject, kind) = match alert.kind {
        AlertKind::Alert => (format!("[zfsctl] {host}: {reason}"), "health-alert"),
        AlertKind::Escalation => (format!("[zfsctl] {host}: now {}: {reason}", severity_word(alert.severity)), "health-alert"),
        AlertKind::Reminder => (format!("[zfsctl] {host}: still: {reason} (for {})", bad_for.clone().unwrap_or_default()), "health-reminder"),
        AlertKind::Recovery => (bad_for.clone().map_or_else(|| format!("[zfsctl] {host}: recovered"), |d| format!("[zfsctl] {host}: recovered after {d}")), "health-recovered"),
    };
    let mut body = format!("Host:     {} ({})\nHub:      {hub}\nTime:     {}\nState:    {}: {}\n", row.name, row.address, utc(now), severity_word(alert.severity), alert.reason);
    if let Some(rtt) = row.rtt_ms {
        let _ = writeln!(body, "Rtt:      {rtt}ms");
    }
    if let Some(d) = &bad_for {
        let _ = writeln!(body, "Bad for:  {d}");
    }
    let _ = write!(body, "\n{}\n{}\n", table_header(), table_row(row));
    for line in pool_lines(&row.pools) {
        body.push_str(&line);
        body.push('\n');
    }
    Message { kind: kind.to_string(), host: alert.host.clone(), level: severity_word(alert.severity).to_string(), subject, body }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthEvent {
    pub at: DateTime<Utc>,
    pub host: String,
    pub kind: String,
    pub text: String,
}

/// `<state>/health-events.jsonl`: alerts, recoveries, reminders and daemon starts, for the report.
pub struct EventLog(PathBuf);

impl EventLog {
    pub fn new(path: impl Into<PathBuf>) -> EventLog {
        EventLog(path.into())
    }

    pub fn append(&self, event: &HealthEvent) -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&self.0)?;
        writeln!(file, "{}", serde_json::to_string(event).map_err(std::io::Error::other)?)?;
        drop(file);
        let text = std::fs::read_to_string(&self.0)?;
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() >= EVENTS_TRIM_AT {
            let kept = lines.iter().skip(lines.len().saturating_sub(EVENTS_KEEP)).copied().collect::<Vec<_>>().join("\n") + "\n";
            let tmp = self.0.with_extension("jsonl.tmp");
            std::fs::write(&tmp, kept)?;
            std::fs::rename(tmp, &self.0)?;
        }
        Ok(())
    }

    pub fn read(&self) -> Vec<HealthEvent> {
        std::fs::read_to_string(&self.0).map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default()
    }
}

fn cut_list(mut lines: Vec<String>) -> Vec<String> {
    if lines.len() > LIST_MAX {
        let more = lines.len() - LIST_MAX;
        lines.truncate(LIST_MAX);
        lines.push(format!("  (+{more} more)"));
    }
    if lines.is_empty() {
        lines.push("  none".to_string());
    }
    lines
}

/// The daily report. `bad_since` says when an open alert began; `since` starts the events window.
#[allow(clippy::too_many_arguments)]
/// The daily report's lines for disks with a problem, from the daemon's `smart.json`: `fbsd16: da1 reallocated sectors 8 (checked ... UTC)`.
pub fn disk_lines(file: &crate::daemon::smart_check::SmartFile) -> Vec<String> {
    crate::daemon::smart_check::problems(file).iter().map(|p| format!("{}: {} (checked {})", p.host, p.disks, utc(p.checked_at))).collect()
}

/// All the lines the daily report adds for the daemon's checks: disks first, then shares.
pub fn check_lines(state_dir: &std::path::Path) -> Vec<String> {
    let mut lines = disk_lines(&crate::daemon::smart_check::SmartFile::load(&state_dir.join("smart.json")));
    lines.extend(share_lines(&crate::daemon::shares_check::SharesFile::load(&state_dir.join("shares.json"))));
    // The report has no config of its own: the thresholds are the user's, read from the fixed config path (defaults when absent).
    let quota = crate::config::Config::load(std::path::Path::new(crate::config::DEFAULT_CONFIG_PATH)).map(|c| c.dashboard.resolve().quota).unwrap_or_else(|_| crate::config::Thresholds::default().quota);
    lines.extend(storage_lines(&crate::daemon::storage_check::StorageFile::load(&state_dir.join(crate::daemon::storage_check::FILE)), quota));
    lines
}

/// The daily report's lines for datasets near their quota, from `storage.json`.
pub fn storage_lines(file: &crate::daemon::storage_check::StorageFile, quota: (u8, u8)) -> Vec<String> {
    crate::daemon::storage_check::problems(file, quota)
        .iter()
        .chain(crate::daemon::storage_check::pool_problems(file).iter())
        .map(|p| format!("{}: {} (checked {})", p.host, p.text, utc(p.checked_at)))
        .collect()
}

/// The daily report's lines for hosts whose file sharing has a problem, from the daemon's `shares.json`.
pub fn share_lines(file: &crate::daemon::shares_check::SharesFile) -> Vec<String> {
    crate::daemon::shares_check::problems(file).iter().map(|p| format!("{}: {} (checked {})", p.host, p.text, utc(p.checked_at))).collect()
}

#[allow(clippy::too_many_arguments)]
pub fn report_message(now: DateTime<Utc>, local: &str, hub: &str, rows: &[HostRow], bad_since: &dyn Fn(&str) -> Option<DateTime<Utc>>, since: DateTime<Utc>, runs: &[Run], jobs: &[Job], events: &[HealthEvent], disks: &[String]) -> Message {
    let count = |want: &[Level]| rows.iter().filter(|r| want.contains(&r.level)).count();
    let (ok, warning, critical) = (count(&[Level::Ok]), count(&[Level::Warning]), count(&[Level::Critical, Level::Disconnected]));
    let mut parts = vec![format!("{ok} ok")];
    if warning > 0 {
        parts.push(format!("{warning} warning"));
    }
    if critical > 0 {
        parts.push(format!("{critical} critical"));
    }
    let counts = parts.join(", ");
    let subject = if warning + critical == 0 { format!("[zfsctl] daily report: all {} hosts ok", rows.len()) } else { format!("[zfsctl] daily report: {counts}") };

    let mut body = format!("zfsctl daily report\nHub:      {hub}\nTime:     {local} (hub local time), {}\n\n{} hosts: {counts}\n\n{}\n", utc(now), rows.len(), table_header());
    for r in rows {
        body.push_str(&table_row(r));
        body.push('\n');
    }
    body.push_str("\nNEEDS ATTENTION\n");
    let mut attention: Vec<String> = rows
        .iter()
        .filter(|r| !matches!(r.level, Level::Ok | Level::Probing))
        .map(|r| match bad_since(&r.name) {
            Some(t) => format!("  {}: {} (since {})", r.name, r.reason, utc(t)),
            None => format!("  {}: {}", r.name, r.reason),
        })
        .collect();
    attention.extend(disks.iter().map(|d| format!("  {d}")));
    if attention.is_empty() {
        body.push_str("  Nothing needs attention.\n");
    } else {
        for line in attention {
            body.push_str(&line);
            body.push('\n');
        }
    }

    let _ = write!(body, "\nEVENTS since {}\n", utc(since));
    let finished: Vec<&Run> = runs.iter().filter(|r| r.started_at >= since && r.result.is_some()).collect();
    let job_of = |id: &str| jobs.iter().find(|j| j.id == id);
    let mut kinds: Vec<(String, usize, usize)> = Vec::new();
    for r in &finished {
        let kind = job_of(&r.job_id).map_or("removed jobs".to_string(), |j| j.kind.clone());
        let failed = usize::from(r.result == Some(RunResult::Failed));
        match kinds.iter_mut().find(|(k, _, _)| *k == kind) {
            Some((_, n, f)) => {
                *n += 1;
                *f += failed;
            }
            None => kinds.push((kind, 1, failed)),
        }
    }
    body.push_str("Jobs:\n");
    for line in cut_list(kinds.into_iter().map(|(k, n, f)| format!("  {k}: {n} run{}, {f} failed", if n == 1 { "" } else { "s" })).collect()) {
        body.push_str(&line);
        body.push('\n');
    }
    body.push_str("Failed runs:\n");
    let failed: Vec<String> = finished
        .iter()
        .filter(|r| r.result == Some(RunResult::Failed))
        .map(|r| {
            // A job that has since been removed from jobs.toml no longer says which host it ran on.
            let host = job_of(&r.job_id).map_or(String::new(), |j| format!(" on {}", j.host));
            format!("  failed: {}{host} at {}: {}", r.job_id, r.finished_at.unwrap_or(r.started_at).format("%H:%M"), r.message)
        })
        .collect();
    for line in cut_list(failed) {
        body.push_str(&line);
        body.push('\n');
    }
    body.push_str("Health:\n");
    let health: Vec<String> = events.iter().filter(|e| e.at >= since).map(|e| format!("  {} {}: {}", e.host, e.kind, e.text)).collect();
    for line in cut_list(health) {
        body.push_str(&line);
        body.push('\n');
    }
    Message { kind: "health-report".to_string(), host: hub.to_string(), level: if critical > 0 { "critical" } else if warning > 0 { "warning" } else { "ok" }.to_string(), subject, body }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zfs::host_health::ScanSummary;
    use crate::zfs::pool::PoolHealth;

    fn t(min: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-11T00:00:00Z").unwrap().with_timezone(&Utc) + chrono::Duration::minutes(min)
    }

    fn row(name: &str, level: Level, reason: &str) -> HostRow {
        HostRow { name: name.into(), address: "172.16.10.60".into(), level, reason: reason.into(), kind: Some(HostKind::Vm("kvm".into())), tag: String::new(), pools: vec![], rtt_ms: Some(3), jobs_failed: 0, jobs_total: 0 }
    }

    fn pool(name: &str, cap: u8, scan: Option<ScanSummary>) -> PoolProbe {
        PoolProbe { name: name.into(), health: PoolHealth::Online, capacity_percent: cap, frag_percent: Some(4), errors: Some(0), scan, last_scrub: None }
    }

    fn job(id: &str, kind: &str, host: &str) -> Job {
        let mut j = Job::test_backup(id, "tank/x", "p", "/r", "/pw");
        j.kind = kind.into();
        j.host = host.into();
        j
    }

    fn run(job: &str, result: Option<RunResult>, msg: &str, at: DateTime<Utc>) -> Run {
        Run { job_id: job.into(), run_id: format!("{job}-{}", at.timestamp()), started_at: at, scheduled_for: None, finished_at: result.map(|_| at), result, message: msg.into(), catch_up: false, summary: None }
    }

    #[test]
    fn the_report_lists_datasets_near_their_quota() {
        use crate::daemon::storage_check::{LimitedDataset, StorageFile, StorageResult};
        let mut file = StorageFile::default();
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-18T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        file.hosts.insert("fbsd16".into(), StorageResult { checked_at: at, datasets: vec![LimitedDataset { name: "tank/home".into(), used: 94, referenced: 94, quota: 100, refquota: 0 }], pools: vec![], zfs_version: None, integrity: Default::default(), error: None });
        let lines = storage_lines(&file, (90, 98));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("fbsd16: tank/home 94% of its 100 quota (checked "), "{}", lines[0]);
        assert!(storage_lines(&file, (95, 99)).is_empty());
    }

    #[test]
    fn an_alert_names_the_host_the_reason_and_the_pools() {
        let mut r = row("fbsd16", Level::Critical, "tank DEGRADED");
        r.pools = vec![pool("tank", 91, Some(ScanSummary::Never))];
        let alert = Alert { host: "fbsd16".into(), kind: AlertKind::Alert, severity: Severity::Critical, reason: "tank DEGRADED".into(), bad_for: None };
        let m = alert_message(&r, &alert, t(0), "hub");
        assert_eq!(m.subject, "[zfsctl] fbsd16: tank DEGRADED");
        assert_eq!((m.kind.as_str(), m.host.as_str(), m.level.as_str()), ("health-alert", "fbsd16", "critical"));
        for want in ["Host:     fbsd16 (172.16.10.60)", "State:    critical: tank DEGRADED", "tank", "91%"] {
            assert!(m.body.contains(want), "{want:?} in\n{}", m.body);
        }
    }

    #[test]
    fn escalations_reminders_and_recoveries_have_their_own_subjects_and_kinds() {
        let r = row("fbsd16", Level::Ok, "ok");
        let mk = |kind, sev, bad_for| Alert { host: "fbsd16".into(), kind, severity: sev, reason: "tank 90% full".into(), bad_for };
        let e = alert_message(&r, &mk(AlertKind::Escalation, Severity::Critical, Some(Duration::from_secs(3600))), t(0), "hub");
        assert_eq!((e.subject.as_str(), e.kind.as_str()), ("[zfsctl] fbsd16: now critical: tank 90% full", "health-alert"));
        let m = alert_message(&r, &mk(AlertKind::Reminder, Severity::Warning, Some(Duration::from_secs(6 * 3600 + 120))), t(0), "hub");
        assert_eq!((m.subject.as_str(), m.kind.as_str()), ("[zfsctl] fbsd16: still: tank 90% full (for 6h 2m)", "health-reminder"));
        let ok = alert_message(&r, &mk(AlertKind::Recovery, Severity::Ok, Some(Duration::from_secs(90 * 60))), t(0), "hub");
        assert_eq!((ok.subject.as_str(), ok.kind.as_str(), ok.level.as_str()), ("[zfsctl] fbsd16: recovered after 1h 30m", "health-recovered", "ok"));
    }

    #[test]
    fn durations_are_short_and_drop_a_zero_part() {
        assert_eq!(dur(Duration::from_secs(45 * 60)), "45m");
        assert_eq!(dur(Duration::from_secs(2 * 3600)), "2h");
        assert_eq!(dur(Duration::from_secs(26 * 3600)), "1d 2h");
        assert_eq!(dur(Duration::from_secs(48 * 3600)), "2d");
        assert_eq!(dur(Duration::from_secs(10)), "0m");
    }

    #[test]
    fn the_daily_report_lists_the_disk_problems_under_needs_attention() {
        let mut file = crate::daemon::smart_check::SmartFile::default();
        file.hosts.insert("fbsd16".into(), crate::daemon::smart_check::HostResult { checked_at: t(0), disks: vec![crate::smart::Disk { dev: "da1".into(), problems: vec![crate::smart::Problem { severity: crate::smart::Severity::Watch, text: "reallocated sectors 8".into() }] }], error: None });
        let lines = disk_lines(&file);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("fbsd16: da1 reallocated sectors 8 (checked "), "{}", lines[0]);
        let m = report_message(t(60), "2026-10-11 07:00", "hub", &[row("hub", Level::Ok, "ok")], &|_| None, t(0), &[], &[], &[], &lines);
        assert!(m.body.contains("  fbsd16: da1 reallocated sectors 8") && !m.body.contains("Nothing needs attention"), "{}", m.body);
        assert!(disk_lines(&crate::daemon::smart_check::SmartFile::default()).is_empty());
    }

    #[test]
    fn the_daily_report_lists_the_share_problems_after_the_disk_ones() {
        let dir = tempfile::tempdir().unwrap();
        let mut shares = crate::daemon::shares_check::SharesFile::default();
        shares.hosts.insert("fbsd16".into(), crate::daemon::shares_check::SharesResult { checked_at: t(0), nfs_running: false, samba_running: true, nfs_shares: 2, smb_shares: 0, smb_error: None, error: None, nfs_name: None, smb_name: None });
        shares.save(&dir.path().join("shares.json")).unwrap();
        let lines = check_lines(dir.path());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("fbsd16: NFS: 2 shares but nfsd is not running (checked "), "{}", lines[0]);
        let m = report_message(t(60), "2026-10-11 07:00", "hub", &[row("hub", Level::Ok, "ok")], &|_| None, t(0), &[], &[], &[], &lines);
        assert!(m.body.contains("  fbsd16: NFS: 2 shares but nfsd is not running") && !m.body.contains("Nothing needs attention"), "{}", m.body);
    }

    #[test]
    fn a_host_name_with_a_newline_cannot_break_the_subject() {
        let r = row("bad\nname", Level::Critical, "x");
        let a = Alert { host: "bad\nname".into(), kind: AlertKind::Alert, severity: Severity::Critical, reason: "x\r\ny".into(), bad_for: None };
        let m = alert_message(&r, &a, t(0), "hub");
        assert!(!m.subject.contains('\n') && !m.subject.contains('\r'), "{:?}", m.subject);
    }

    #[test]
    fn the_report_has_a_summary_a_table_and_what_needs_attention() {
        let rows = vec![row("hub", Level::Ok, "ok"), row("fbsd16", Level::Critical, "tank DEGRADED"), row("t1", Level::Warning, "tank 85% full"), row("t2", Level::Disconnected, "unreachable")];
        let m = report_message(t(60 * 7), "2026-10-11 07:00", "hub", &rows, &|h| (h == "fbsd16").then(|| t(60 * 2)), t(0), &[], &[], &[], &[]);
        assert_eq!(m.subject, "[zfsctl] daily report: 1 ok, 1 warning, 2 critical");
        assert_eq!((m.kind.as_str(), m.host.as_str()), ("health-report", "hub"));
        for want in ["4 hosts: 1 ok, 1 warning, 2 critical", "HOST", "ADDRESS", "STATE", "USED", "SCRUB", "JOBS", "fbsd16", "NEEDS ATTENTION", "fbsd16: tank DEGRADED (since 2026-10-11 02:00 UTC)", "t2: unreachable"] {
            assert!(m.body.contains(want), "{want:?} in\n{}", m.body);
        }
        let attention = m.body.split("NEEDS ATTENTION").nth(1).unwrap().split("EVENTS").next().unwrap();
        assert!(!attention.contains("hub:"), "a healthy host is not listed: {attention}");
    }

    #[test]
    fn an_all_green_report_says_so_in_the_subject_and_still_has_a_body() {
        let rows = vec![row("hub", Level::Ok, "ok"), row("t1", Level::Ok, "ok")];
        let m = report_message(t(0), "2026-10-11 07:00", "hub", &rows, &|_| None, t(-60 * 24), &[], &[], &[], &[]);
        assert_eq!(m.subject, "[zfsctl] daily report: all 2 hosts ok");
        assert!(m.body.contains("Nothing needs attention") && m.body.contains("EVENTS since"), "{}", m.body);
    }

    #[test]
    fn events_count_job_runs_since_the_window_and_list_failures_and_alerts() {
        let jobs = vec![job("backup-a", "backup", "fbsd16"), job("scrub-t", "scrub", "hub")];
        let runs = vec![
            run("backup-a", Some(RunResult::Ok), "", t(10)),
            run("backup-a", Some(RunResult::Failed), "restic exited 3", t(20)),
            run("scrub-t", Some(RunResult::Ok), "scrub repaired 0B with 0 errors", t(30)),
            run("backup-a", Some(RunResult::Failed), "too old", t(-5000)),
            run("scrub-t", None, "", t(40)),
        ];
        let events = vec![HealthEvent { at: t(15), host: "fbsd16".into(), kind: "alert".into(), text: "tank DEGRADED".into() }, HealthEvent { at: t(-9000), host: "t1".into(), kind: "alert".into(), text: "old".into() }];
        let m = report_message(t(60), "2026-10-11 07:00", "hub", &[row("hub", Level::Ok, "ok")], &|_| None, t(0), &runs, &jobs, &events, &[]);
        for want in ["backup: 2 runs, 1 failed", "scrub: 1 run, 0 failed", "failed: backup-a on fbsd16", "restic exited 3", "fbsd16 alert: tank DEGRADED"] {
            assert!(m.body.contains(want), "{want:?} in\n{}", m.body);
        }
        assert!(!m.body.contains("too old") && !m.body.contains("t1 alert"), "outside the window:\n{}", m.body);
    }

    #[test]
    fn runs_of_a_removed_job_are_still_counted_and_listed_without_a_question_mark() {
        let runs = vec![run("gone", Some(RunResult::Failed), "boom", t(10))];
        let m = report_message(t(60), "2026-10-11 07:00", "hub", &[row("hub", Level::Ok, "ok")], &|_| None, t(0), &runs, &[], &[], &[]);
        assert!(m.body.contains("removed jobs: 1 run, 1 failed") && m.body.contains("failed: gone at 00:10: boom"), "{}", m.body);
        assert!(!m.body.contains('?'), "{}", m.body);
    }

    #[test]
    fn long_event_lists_are_cut_with_a_count_and_empty_ones_say_none() {
        let jobs = vec![job("j", "snapshot", "hub")];
        let runs: Vec<Run> = (0..60).map(|i| run("j", Some(RunResult::Failed), &format!("boom {i}"), t(i))).collect();
        let m = report_message(t(120), "2026-10-11 07:00", "hub", &[row("hub", Level::Ok, "ok")], &|_| None, t(0), &runs, &jobs, &[], &[]);
        assert!(m.body.contains("(+20 more)"), "{}", m.body);
        let quiet = report_message(t(120), "2026-10-11 07:00", "hub", &[row("hub", Level::Ok, "ok")], &|_| None, t(0), &[], &[], &[], &[]);
        assert_eq!(quiet.body.matches("  none").count(), 3, "{}", quiet.body);
    }

    #[test]
    fn the_table_cuts_long_cells_instead_of_pushing_the_columns() {
        let mut r = row("a-very-long-host-name", Level::Ok, "ok");
        r.tag = "a-long-tag-here".into();
        let line = table_row(&r);
        assert_eq!(line.chars().count(), table_header().chars().count(), "{line}");
        assert!(line.contains('…'));
    }

    #[test]
    fn the_event_log_round_trips_and_keeps_only_the_newest_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::new(dir.path().join("health-events.jsonl"));
        for i in 0..2300 {
            log.append(&HealthEvent { at: t(i), host: "a".into(), kind: "alert".into(), text: format!("n{i}") }).unwrap();
        }
        let all = log.read();
        assert!(all.len() <= 2200 && all.len() >= 2000, "{}", all.len());
        assert_eq!(all.last().unwrap().text, "n2299");
        assert_eq!(EventLog::new(dir.path().join("none")).read(), Vec::<HealthEvent>::new());
    }
}
