//! The health monitor: probes every host, feeds the alert tracker, queues and retries what must be sent, and
//! sends the daily report. `observe` does everything after the probing, so it is tested without threads or ssh.

use super::deliver::Message;
use super::health::{AlertKind, Tracker};
use super::health_text::{alert_message, report_message, EventLog, HealthEvent, HostRow};
use super::history::{Run, RunResult};
use crate::config::NotifySettings;
use crate::exec::CommandRunner;
use crate::jobs::Job;
use crate::zfs::host_health::{assess, probe_host_guarded, HostKind, HostProbe, Level};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Probes that must agree before an alert is sent.
pub const CONFIRM: u32 = 2;
/// Sends tried for one message (the first and the retries) before it is dropped.
pub const MAX_ATTEMPTS: u32 = 6;
/// How long a host may take to answer a probe.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(60);

pub struct HostInfo {
    pub name: String,
    pub address: String,
    pub tag: String,
    pub runner: Arc<dyn CommandRunner>,
}

/// One host's probe; `None` means it did not answer before the deadline.
pub struct Probed {
    pub name: String,
    pub probe: Option<HostProbe>,
}

/// Probes every host at once and waits until `deadline` for the answers. A host that has not answered by then
/// comes back as `None`; its thread is left to finish by itself (as the TUI's probe is). `detect` says whether to
/// read a host's type (done once per host).
pub fn probe_all(hosts: &[HostInfo], detect: &dyn Fn(&str) -> bool, deadline: Duration) -> Vec<Probed> {
    let (tx, rx) = std::sync::mpsc::channel();
    for host in hosts {
        let (tx, runner, name, kind) = (tx.clone(), Arc::clone(&host.runner), host.name.clone(), detect(&host.name));
        std::thread::spawn(move || {
            let probe = probe_host_guarded(runner.as_ref(), kind);
            let _ = tx.send((name, probe));
        });
    }
    drop(tx);
    let started = Instant::now();
    let mut answers: HashMap<String, HostProbe> = HashMap::new();
    while answers.len() < hosts.len() {
        let Some(left) = deadline.checked_sub(started.elapsed()) else { break };
        match rx.recv_timeout(left) {
            Ok((name, probe)) => {
                answers.insert(name, probe);
            }
            Err(_) => break,
        }
    }
    hosts.iter().map(|h| Probed { name: h.name.clone(), probe: answers.remove(&h.name) }).collect()
}

/// One row per host from its probe and its jobs: a host whose probe did not answer is a warning, and an enabled job
/// whose newest run failed makes a host a warning too (as in the Hosts table).
pub fn build_rows(hosts: &[HostInfo], probed: &[Probed], kinds: &HashMap<String, HostKind>, jobs: &[Job], runs: &[Run]) -> Vec<HostRow> {
    hosts
        .iter()
        .map(|h| {
            let mine: Vec<&Job> = jobs.iter().filter(|j| j.enabled && j.host == h.name).collect();
            let failed = mine.iter().filter(|j| runs.iter().filter(|r| r.job_id == j.id).max_by_key(|r| r.started_at).is_some_and(|r| r.result == Some(RunResult::Failed))).count();
            let probe = probed.iter().find(|p| p.name == h.name).and_then(|p| p.probe.as_ref());
            let (level, reason) = match probe {
                None => (Level::Warning, format!("no answer for {}s", PROBE_DEADLINE.as_secs())),
                Some(p) => {
                    let a = assess(Some(p), failed > 0);
                    (a.level, a.reason)
                }
            };
            let (pools, rtt_ms) = match probe {
                Some(HostProbe::Reached { pools, rtt_ms, .. }) => (pools.clone(), Some(*rtt_ms)),
                _ => (Vec::new(), None),
            };
            HostRow { name: h.name.clone(), address: h.address.clone(), level, reason, kind: kinds.get(&h.name).cloned(), tag: h.tag.clone(), pools, rtt_ms, jobs_failed: failed, jobs_total: mine.len() }
        })
        .collect()
}

/// Where a delivery goes; `true` means at least one channel took the message.
pub trait Sink {
    fn send(&mut self, msg: &Message) -> bool;
}

struct Pending {
    msg: Message,
    attempts: u32,
}

pub struct Monitor {
    settings: NotifySettings,
    hub: String,
    state_dir: PathBuf,
    tracker: Tracker,
    pending: Vec<Pending>,
    last_report: Option<NaiveDate>,
    report_attempts: u32,
    last_report_at: Option<DateTime<Utc>>,
    kinds: HashMap<String, HostKind>,
    events: EventLog,
}

impl Monitor {
    pub fn new(settings: NotifySettings, hub: String, state_dir: PathBuf, now: DateTime<Utc>) -> Monitor {
        let last_report = std::fs::read_to_string(state_dir.join("notify.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.get("last_report").and_then(|d| d.as_str().map(str::to_string)))
            .and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok());
        let events = EventLog::new(state_dir.join("health-events.jsonl"));
        let _ = events.append(&HealthEvent { at: now, host: hub.clone(), kind: "daemon-started".to_string(), text: "zfsctl daemon started".to_string() });
        let tracker = Tracker::new(CONFIRM, settings.remind);
        Monitor { settings, hub, state_dir, tracker, pending: Vec::new(), last_report, report_attempts: 0, last_report_at: None, kinds: HashMap::new(), events }
    }

    pub fn settings(&self) -> &NotifySettings {
        &self.settings
    }

    /// Whether `host`'s type has not been read yet.
    pub fn needs_kind(&self, host: &str) -> bool {
        !self.kinds.contains_key(host)
    }

    /// The report goes out once per local day, from `at` on; also when the daemon was down at `at` and comes back
    /// later the same day.
    pub fn report_due(local: NaiveDateTime, at: Option<NaiveTime>, last: Option<NaiveDate>) -> bool {
        at.is_some_and(|at| local.time() >= at) && last != Some(local.date())
    }

    fn mark_report_sent(&mut self, date: NaiveDate, persist: bool) {
        self.last_report = Some(date);
        self.report_attempts = 0;
        if persist {
            let _ = std::fs::write(self.state_dir.join("notify.json"), format!("{{\"last_report\":\"{}\"}}\n", date.format("%Y-%m-%d")));
        }
    }

    /// One monitoring tick, after the probes: updates the alert state, sends (and retries) what is due, and sends the
    /// daily report when it is time.
    #[allow(clippy::too_many_arguments)]
    pub fn observe(&mut self, now: DateTime<Utc>, local: NaiveDateTime, hosts: &[HostInfo], probed: Vec<Probed>, jobs: &[Job], runs: &[Run], sink: &mut dyn Sink) {
        for p in &probed {
            if let Some(HostProbe::Reached { kind: Some(k), .. }) = &p.probe {
                self.kinds.insert(p.name.clone(), k.clone());
            }
        }
        let rows = build_rows(hosts, &probed, &self.kinds, jobs, runs);
        let names: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
        self.tracker.forget_except(&names);
        for row in &rows {
            let Some(alert) = self.tracker.observe(&row.name, row.level, &row.reason, now) else { continue };
            let kind = match alert.kind {
                AlertKind::Alert => "alert",
                AlertKind::Escalation => "escalation",
                AlertKind::Reminder => "reminder",
                AlertKind::Recovery => "recovery",
            };
            let _ = self.events.append(&HealthEvent { at: now, host: row.name.clone(), kind: kind.to_string(), text: alert.reason.clone() });
            self.pending.push(Pending { msg: alert_message(row, &alert, now, &self.hub), attempts: 0 });
        }
        let queue = std::mem::take(&mut self.pending);
        for mut item in queue {
            if sink.send(&item.msg) {
                continue;
            }
            item.attempts += 1;
            if item.attempts < MAX_ATTEMPTS {
                self.pending.push(item);
            }
        }
        if Self::report_due(local, self.settings.report_time, self.last_report) {
            let since = self.last_report_at.unwrap_or(now - chrono::Duration::hours(24));
            let msg = report_message(now, &local.format("%Y-%m-%d %H:%M").to_string(), &self.hub, &rows, &|h| self.tracker.bad_since(h), since, runs, jobs, &self.events.read());
            if sink.send(&msg) {
                self.last_report_at = Some(now);
                self.mark_report_sent(local.date(), true);
            } else {
                self.report_attempts += 1;
                if self.report_attempts >= MAX_ATTEMPTS {
                    // Give up for today, without recording it as sent.
                    self.mark_report_sent(local.date(), false);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NotifyConfig;
    use crate::exec::test_support::FakeRunner;
    use crate::exec::CommandOutput;

    struct FakeSink {
        sent: Vec<Message>,
        ok: bool,
    }

    impl FakeSink {
        fn new() -> FakeSink {
            FakeSink { sent: Vec::new(), ok: true }
        }
        fn failing() -> FakeSink {
            FakeSink { sent: Vec::new(), ok: false }
        }
    }

    impl Sink for FakeSink {
        fn send(&mut self, msg: &Message) -> bool {
            self.sent.push(msg.clone());
            self.ok
        }
    }

    fn t(min: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-11T00:00:00Z").unwrap().with_timezone(&Utc) + chrono::Duration::minutes(min)
    }

    fn local(min: i64) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 11).unwrap().and_hms_opt(0, 0, 0).unwrap() + chrono::Duration::minutes(min)
    }

    fn at_local(h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 11).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    fn settings_with_report(report: &str) -> NotifySettings {
        NotifyConfig { mail_to: Some("me@x.org".into()), report_time: Some(report.into()), ..NotifyConfig::default() }.resolve().unwrap()
    }

    fn monitor() -> Monitor {
        let dir = tempfile::tempdir().unwrap();
        Monitor::new(settings_with_report("off"), "hub".into(), dir.keep(), t(0))
    }

    fn host(name: &str) -> HostInfo {
        let runner = FakeRunner::new("").route("zpool", "list", "tank\t1000\t200\t800\t5\tONLINE\n").route("zpool", "status", "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\ttank  ONLINE  0 0 0\n");
        HostInfo { name: name.into(), address: format!("{name}.example"), tag: String::new(), runner: Arc::new(runner) }
    }

    fn probed_ok(name: &str) -> Probed {
        Probed { name: name.into(), probe: Some(HostProbe::Reached { rtt_ms: 3, pools: vec![], kind: None }) }
    }

    fn probed_bad(name: &str) -> Probed {
        Probed { name: name.into(), probe: Some(HostProbe::Disconnected) }
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
    fn two_bad_probes_alert_once_and_two_good_ones_recover() {
        let mut m = monitor();
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
        }
        assert_eq!(sink.sent.len(), 1);
        assert_eq!(sink.sent[0].subject, "[zfsctl] a: unreachable");
        m.observe(t(10), local(10), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
        assert_eq!(sink.sent.len(), 1, "no repeat");
        for i in 3..5 {
            m.observe(t(i * 5), local(i * 5), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
        }
        assert_eq!(sink.sent.len(), 2);
        assert!(sink.sent[1].subject.starts_with("[zfsctl] a: recovered after"), "{}", sink.sent[1].subject);
    }

    #[test]
    fn a_probe_that_never_answered_is_a_warning_not_a_silent_ok() {
        let mut m = monitor();
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, vec![Probed { name: "a".into(), probe: None }], &[], &[], &mut sink);
        }
        assert!(sink.sent[0].subject.contains("no answer"), "{}", sink.sent[0].subject);
        assert_eq!(sink.sent[0].level, "warning");
    }

    #[test]
    fn a_failed_job_on_the_host_makes_it_a_warning() {
        let mut m = monitor();
        let hosts = [host("a")];
        let jobs = vec![job("backup-a", "backup", "a")];
        let runs = vec![run("backup-a", Some(RunResult::Failed), "restic exited 3", t(-10))];
        let mut sink = FakeSink::new();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, vec![probed_ok("a")], &jobs, &runs, &mut sink);
        }
        assert_eq!(sink.sent.len(), 1);
        assert_eq!(sink.sent[0].level, "warning");
    }

    #[test]
    fn a_delivery_that_fails_is_retried_on_later_ticks_at_most_six_times_then_dropped() {
        let mut m = monitor();
        let hosts = [host("a")];
        let mut sink = FakeSink::failing();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
        }
        let first = sink.sent.len();
        assert_eq!(first, 1);
        for i in 2..20 {
            m.observe(t(i * 5), local(i * 5), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
        }
        assert_eq!(sink.sent.len() - first, 5, "six attempts in all: one at first, five retries, then dropped");
        assert!(sink.sent.iter().all(|s| s.subject == "[zfsctl] a: unreachable"));
    }

    #[test]
    fn a_retry_that_succeeds_is_not_sent_again() {
        let mut m = monitor();
        let hosts = [host("a")];
        let mut sink = FakeSink::failing();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
        }
        sink.ok = true;
        m.observe(t(10), local(10), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
        let n = sink.sent.len();
        m.observe(t(15), local(15), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
        assert_eq!(sink.sent.len(), n);
    }

    #[test]
    fn the_report_is_due_once_per_local_day_from_report_time_on() {
        let at = NaiveTime::from_hms_opt(7, 0, 0);
        let day = |d: u32, h: u32, mi: u32| NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(h, mi, 0).unwrap();
        let last = NaiveDate::from_ymd_opt(2026, 10, 10);
        assert!(!Monitor::report_due(day(11, 6, 59), at, last), "not yet");
        assert!(Monitor::report_due(day(11, 7, 0), at, last));
        assert!(Monitor::report_due(day(11, 15, 0), at, last), "the daemon was down at 07:00: sent on return");
        assert!(!Monitor::report_due(day(11, 15, 0), at, NaiveDate::from_ymd_opt(2026, 10, 11)), "already today");
        assert!(Monitor::report_due(day(12, 7, 0), at, NaiveDate::from_ymd_opt(2026, 10, 11)));
        assert!(!Monitor::report_due(day(11, 9, 0), None, None), "off");
    }

    fn reports(s: &FakeSink) -> usize {
        s.sent.iter().filter(|m| m.kind == "health-report").count()
    }

    #[test]
    fn the_report_is_sent_once_and_remembered_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
        m.observe(t(0), at_local(7, 1), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
        assert_eq!(reports(&sink), 1);
        m.observe(t(5), at_local(7, 6), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
        assert_eq!(reports(&sink), 1, "not twice");
        let mut again = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(60));
        again.observe(t(60), at_local(8, 0), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
        assert_eq!(reports(&sink), 1, "a restarted monitor reads notify.json");
    }

    #[test]
    fn a_failed_report_is_retried_and_only_marked_sent_once_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = [host("a")];
        let mut sink = FakeSink::failing();
        let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
        m.observe(t(0), at_local(7, 1), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
        assert!(!dir.path().join("notify.json").exists());
        sink.ok = true;
        m.observe(t(5), at_local(7, 6), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
        assert!(dir.path().join("notify.json").exists());
        assert_eq!(reports(&sink), 2);
    }

    #[test]
    fn a_report_that_keeps_failing_is_given_up_for_the_day_without_being_recorded_as_sent() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = [host("a")];
        let mut sink = FakeSink::failing();
        let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
        for i in 0..10 {
            m.observe(t(i * 5), at_local(7, u32::try_from(i).unwrap()), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
        }
        assert_eq!(reports(&sink), 6, "six attempts, then it stops asking for today");
        assert!(!dir.path().join("notify.json").exists());
    }

    #[test]
    fn alerts_and_a_start_are_written_to_the_event_log_for_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        let mut m = Monitor::new(settings_with_report("off"), "hub".into(), dir.path().to_path_buf(), t(0));
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
        }
        let events = EventLog::new(dir.path().join("health-events.jsonl")).read();
        assert!(events.iter().any(|e| e.kind == "daemon-started"), "{events:?}");
        assert!(events.iter().any(|e| e.kind == "alert" && e.host == "a"), "{events:?}");
    }

    #[test]
    fn the_type_is_remembered_so_it_is_asked_for_only_once() {
        let mut m = monitor();
        let hosts = [host("a")];
        assert!(m.needs_kind("a"));
        let probed = vec![Probed { name: "a".into(), probe: Some(HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: Some(HostKind::Metal) }) }];
        m.observe(t(0), local(0), &hosts, probed, &[], &[], &mut FakeSink::new());
        assert!(!m.needs_kind("a"));
    }

    #[test]
    fn probe_all_collects_every_host_and_a_hung_one_comes_back_as_no_answer() {
        struct Hung;
        impl CommandRunner for Hung {
            fn run(&self, _: &str, _: &[&str]) -> crate::error::Result<CommandOutput> {
                std::thread::sleep(Duration::from_secs(3));
                Ok(CommandOutput { stdout: String::new(), stderr: String::new(), status: 0 })
            }
            fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
            fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
        }
        let hosts = vec![HostInfo { name: "slow".into(), address: "x".into(), tag: String::new(), runner: Arc::new(Hung) }, host("fast")];
        let t0 = Instant::now();
        let probed = probe_all(&hosts, &|_| false, Duration::from_millis(500));
        assert!(t0.elapsed() < Duration::from_secs(2));
        assert_eq!(probed.len(), 2);
        assert!(probed.iter().find(|p| p.name == "slow").unwrap().probe.is_none());
        assert!(matches!(probed.iter().find(|p| p.name == "fast").unwrap().probe, Some(HostProbe::Reached { .. })));
    }
}
