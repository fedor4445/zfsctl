//! The health monitor: probes every host, feeds the alert tracker, queues and retries what must be sent, and
//! sends the daily report. `observe` does everything after the probing, so it is tested without threads or ssh.

use super::deliver::Message;
use super::health::{Alert, AlertKind, Tracker};
use super::health_text::{ago, alert_message, dur, protection_lines, protection_message, report_message, EventLog, HealthEvent, HostRow, ProtectionSection};
use super::history::{Run, RunResult};
use super::protection::{judge, Tier, Unprotected, GRACE, STALE_AFTER};
use super::storage_check::{self, StorageFile};
use crate::config::{EmailTiming, NotifySettings, ProtectionSettings};
use crate::exec::CommandRunner;
use crate::jobs::Job;
use crate::zfs::host_health::{assess, probe_host_guarded, HostKind, HostProbe, Level};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Probes that must agree before an alert is sent.
pub const CONFIRM: u32 = 2;
/// Sends tried for one message (the first and the retries) before it is dropped.
pub const MAX_ATTEMPTS: u32 = 6;
/// How long a host may take to answer a probe.
pub const PROBE_DEADLINE: Duration = Duration::from_mins(1);

pub struct HostInfo {
    pub name: String,
    pub address: String,
    pub tag: String,
    pub runner: Arc<dyn CommandRunner>,
}

/// What `judge_reachable` found: the unprotected datasets of the judged hosts, those hosts' names, and each configured
/// host that was not judged with why (`t2 (unreachable)`), in the order of the hosts.
pub struct Judged<'a> {
    pub found: Vec<Unprotected>,
    pub known: HashSet<&'a str>,
    pub unchecked: Vec<String>,
}

impl Judged<'_> {
    /// The daily report's section; `bad_since` gives the `(for ...)` of a dataset key (`host\tdataset`) when known.
    pub fn section(&self, bad_since: &dyn Fn(&str) -> Option<DateTime<Utc>>, now: DateTime<Utc>) -> ProtectionSection {
        ProtectionSection { lines: protection_lines(&self.found, bad_since, now), judged: self.known.len(), unchecked: self.unchecked.clone() }
    }
}

/// The datasets judged unprotected, only for configured hosts that answered this pass and have a fresh dataset list
/// (unknown is not a verdict), those hosts' names, and the hosts left out with why. Shared by the monitor and the
/// report preview so the two cannot drift apart.
pub fn judge_reachable<'a>(file: &StorageFile, probed: &[Probed], rows: &'a [HostRow], jobs: &[Job], runs: &[Run], settings: &ProtectionSettings, now: DateTime<Utc>) -> Judged<'a> {
    let answered = |name: &str| probed.iter().any(|p| p.name == name && p.probe.is_some());
    let mut known: HashSet<&str> = HashSet::new();
    let mut unchecked = Vec::new();
    for r in rows {
        let why = if matches!(r.level, Level::Disconnected | Level::Probing) || !answered(&r.name) {
            Some("unreachable".to_string())
        } else {
            match file.hosts.get(&r.name) {
                None => Some("no dataset list yet".to_string()),
                Some(h) if now - h.checked_at > STALE_AFTER => Some(format!("no dataset list for {}", dur((now - h.checked_at).to_std().unwrap_or_default()))),
                Some(_) => None,
            }
        };
        match why {
            Some(why) => unchecked.push(format!("{} ({why})", r.name)),
            None => {
                known.insert(r.name.as_str());
            }
        }
    }
    let mut found = judge(file, jobs, runs, settings, now);
    found.retain(|u| known.contains(u.host.as_str()));
    Judged { found, known, unchecked }
}

/// One host's probe; `None` means it did not answer before the deadline.
pub struct Probed {
    pub name: String,
    pub probe: Option<HostProbe>,
}

/// The hosts whose probe is still running. A probe that blocks (a pool stuck in I/O) is never given a second
/// thread, ssh and `zpool` process while the first is alive, so a hung host costs one of each, not one per tick.
pub type Busy = Arc<Mutex<HashSet<String>>>;

/// Probes every host at once and waits until `deadline` for the answers. A host that has not answered by then
/// comes back as `None`; its thread is left to finish by itself (as the TUI's probe is) and the host is skipped
/// (also as `None`) until it has. `detect` says whether to read a host's type (done once per host).
pub fn probe_all(hosts: &[HostInfo], detect: &dyn Fn(&str) -> bool, deadline: Duration, busy: &Busy) -> Vec<Probed> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut asked = 0;
    for host in hosts {
        if !busy.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(host.name.clone()) {
            continue;
        }
        asked += 1;
        let (tx, runner, name, kind, busy) = (tx.clone(), Arc::clone(&host.runner), host.name.clone(), detect(&host.name), Arc::clone(busy));
        std::thread::spawn(move || {
            let probe = probe_host_guarded(runner.as_ref(), kind);
            busy.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&name);
            let _ = tx.send((name, probe));
        });
    }
    drop(tx);
    let started = Instant::now();
    let mut answers: HashMap<String, HostProbe> = HashMap::new();
    while answers.len() < asked {
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

/// Writes `text` to `path` through a temporary file and a rename, so a crash leaves the old file or the new one,
/// never half of one (a torn `notify.json` would be read as "no report sent yet" and the report sent twice).
fn write_atomic(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Where a delivery goes; `true` means at least one channel took the message.
pub trait Sink {
    fn send(&mut self, msg: &Message) -> bool;
}

/// The reminder interval of the dataset alerts: the configured one, but at most daily (the daily report lists them
/// anyway); `None` (no reminders) stays `None`.
fn protection_remind(remind: Option<Duration>) -> Option<Duration> {
    remind.map(|d| d.max(Duration::from_secs(24 * 3600)))
}

/// The dataset alerts of one host and one kind in one pass: (host, kind, [(dataset, alert)]).
type ProtectionGroup = (String, AlertKind, Vec<(String, Alert)>);

struct Pending {
    msg: Message,
    attempts: u32,
    /// The protection tracker keys (`host\tdataset`) the message is about; empty for a message about a host.
    keys: Vec<String>,
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
    protection_settings: ProtectionSettings,
    /// The alert state of each unprotected dataset of the operator's data (not OS data), keyed `host\tdataset`, fed only
    /// when the email timing is immediate.
    protection: Tracker,
    /// The keys `protection` was fed last pass and still has to follow (to a recovery, or to forget them).
    protection_keys: HashSet<String>,
    /// The datasets judged unprotected on the last pass (configured, reachable hosts only), for the daily report.
    unprotected: Vec<Unprotected>,
    /// How many configured hosts the last pass judged, and the ones it could not with why, for the daily report.
    judged_hosts: usize,
    unchecked: Vec<String>,
    /// When each dataset of `unprotected` (key `host\tdataset`) was first judged unprotected, kept whenever the email
    /// timing is not off, so the report says how long even when no alert is tracked (`report` timing, OS data).
    first_seen: HashMap<String, DateTime<Utc>>,
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
        let protection = Tracker::new(CONFIRM, protection_remind(settings.remind));
        Monitor {
            settings,
            hub,
            state_dir,
            tracker,
            pending: Vec::new(),
            last_report,
            report_attempts: 0,
            last_report_at: None,
            kinds: HashMap::new(),
            events,
            protection_settings: ProtectionSettings::default(),
            protection,
            protection_keys: HashSet::new(),
            unprotected: Vec::new(),
            judged_hosts: 0,
            unchecked: Vec::new(),
            first_seen: HashMap::new(),
        }
    }

    /// Applies new `[protection]` settings; the next pass judges with them (and drops the dataset alerts when the email
    /// timing is no longer immediate).
    pub fn set_protection(&mut self, settings: ProtectionSettings) {
        self.protection_settings = settings;
    }

    pub fn settings(&self) -> &NotifySettings {
        &self.settings
    }

    /// Applies new settings without losing anything: the alert state, the retry queue and the report window all
    /// stay (only the reminder interval of the tracker changes).
    pub fn set_settings(&mut self, settings: NotifySettings) {
        self.tracker.set_remind(settings.remind);
        self.protection.set_remind(protection_remind(settings.remind));
        self.settings = settings;
    }

    /// How long until the daily report is due, so the caller can wake for it: `None` when there is no report or it
    /// was sent today, zero when it is due now, else the time left today.
    pub fn report_wait(&self, local: NaiveDateTime) -> Option<Duration> {
        let at = self.settings.report_time?;
        if self.last_report == Some(local.date()) {
            return None;
        }
        Some((at - local.time()).to_std().unwrap_or_default())
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
            let _ = write_atomic(&self.state_dir.join("notify.json"), &format!("{{\"last_report\":\"{}\"}}\n", date.format("%Y-%m-%d")));
        }
    }

    /// One monitoring tick, after the probes: updates the alert state, sends (and retries) what is due, and sends the
    /// daily report when it is time.
    #[allow(clippy::too_many_arguments)]
    pub fn observe(&mut self, now: DateTime<Utc>, local: NaiveDateTime, hosts: &[HostInfo], probed: &[Probed], jobs: &[Job], runs: &[Run], sink: &mut dyn Sink) {
        for p in probed {
            if let Some(HostProbe::Reached { kind: Some(k), .. }) = &p.probe {
                self.kinds.insert(p.name.clone(), k.clone());
            }
        }
        let rows = build_rows(hosts, probed, &self.kinds, jobs, runs);
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
            self.pending.push(Pending { msg: alert_message(row, &alert, now, &self.hub), attempts: 0, keys: Vec::new() });
        }
        self.observe_protection(now, probed, &rows, jobs, runs);
        let queue = std::mem::take(&mut self.pending);
        for mut item in queue {
            if sink.send(&item.msg) {
                continue;
            }
            item.attempts += 1;
            if item.attempts < MAX_ATTEMPTS {
                self.pending.push(item);
            } else if item.msg.kind == "health-alert" {
                // Given up on an alert nobody received: forget the host (or the datasets) so the next bad probes alert
                // it again, instead of a later "recovered" for something the operator never heard about.
                if item.keys.is_empty() {
                    self.tracker.forget_host(&item.msg.host);
                } else {
                    for key in &item.keys {
                        self.protection.forget_host(key);
                    }
                }
            }
        }
        if Self::report_due(local, self.settings.report_time, self.last_report) {
            let since = self.last_report_at.unwrap_or(now - chrono::Duration::hours(24));
            let protection = (self.protection_settings.email != EmailTiming::Off).then(|| ProtectionSection { lines: protection_lines(&self.unprotected, &|k| self.unprotected_since(k), now), judged: self.judged_hosts, unchecked: self.unchecked.clone() });
            let msg = report_message(now, &local.format("%Y-%m-%d %H:%M").to_string(), &self.hub, &rows, &|h| self.tracker.bad_since(h), since, runs, jobs, &self.events.read(), &super::health_text::check_lines(&self.state_dir), protection.as_ref());
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

    /// Since when the dataset key has been unprotected: the earlier of its alert's start and its first-seen time.
    fn unprotected_since(&self, key: &str) -> Option<DateTime<Utc>> {
        match (self.protection.bad_since(key), self.first_seen.get(key).copied()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Drops every dataset alert and the messages still queued for them: the email timing is no longer immediate.
    fn stop_protection_alerts(&mut self) {
        if !self.protection_keys.is_empty() {
            self.protection = Tracker::new(CONFIRM, protection_remind(self.settings.remind));
            self.protection_keys.clear();
        }
        self.pending.retain(|p| p.keys.is_empty());
    }

    /// Judges the datasets once for this pass and, when the email timing is immediate, feeds the dataset alerts and queues
    /// one message per host and kind. Only configured hosts that answered this pass and have a fresh dataset list are
    /// judged; the alerts of any other configured host are left as they are (unknown is not a recovery).
    fn observe_protection(&mut self, now: DateTime<Utc>, probed: &[Probed], rows: &[HostRow], jobs: &[Job], runs: &[Run]) {
        if self.protection_settings.email == EmailTiming::Off {
            self.unprotected.clear();
            self.first_seen.clear();
            self.stop_protection_alerts();
            return;
        }
        let file = StorageFile::load(&self.state_dir.join(storage_check::FILE));
        let Judged { found, known, unchecked } = judge_reachable(&file, probed, rows, jobs, runs, &self.protection_settings, now);
        self.judged_hosts = known.len();
        self.unchecked = unchecked;
        // First-seen times: new keys start now; a key that is protected or gone is dropped, one on a host not judged
        // this pass is kept (unknown is not a verdict).
        let now_bad: HashSet<String> = found.iter().map(|u| format!("{}\t{}", u.host, u.dataset)).collect();
        self.first_seen.retain(|k, _| now_bad.contains(k) || k.split_once('\t').is_some_and(|(h, _)| !known.contains(h)));
        for key in now_bad {
            self.first_seen.entry(key).or_insert(now);
        }
        if self.protection_settings.email != EmailTiming::Immediate {
            self.unprotected = found;
            self.stop_protection_alerts();
            return;
        }

        // (host, dataset, alert) in the order they arise: data first, as `judge` sorts, then the recoveries.
        let mut alerts: Vec<(String, String, Alert)> = Vec::new();
        let mut keys: HashSet<String> = HashSet::with_capacity(found.len());
        // OS data is not mailed one by one: it stays on the Dashboard and in the daily report.
        for u in found.iter().filter(|u| u.tier == Tier::Data) {
            let key = format!("{}\t{}", u.host, u.dataset);
            if let Some(alert) = self.protection.observe(&key, Level::Critical, &u.describe(&|t| ago(now, t)), now) {
                alerts.push((u.host.clone(), u.dataset.clone(), alert));
            }
            keys.insert(key);
        }
        let configured: HashSet<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        let mut earlier: Vec<String> = self.protection_keys.drain().filter(|k| !keys.contains(k)).collect();
        earlier.sort_unstable();
        for key in earlier {
            let Some((host, dataset)) = key.split_once('\t') else { continue };
            if !configured.contains(host) {
                continue;
            }
            if !known.contains(host) {
                keys.insert(key);
                continue;
            }
            // A dataset that is gone (destroyed or renamed) is forgotten without a word; so is one of the same name made
            // again (inside its grace, or made after the alert began): it is a new dataset, not a recovery.
            let Some(seen) = file.hosts.get(host).and_then(|h| h.seen.iter().find(|s| s.name == dataset)) else { continue };
            let created = seen.created.and_then(|c| DateTime::from_timestamp(c, 0));
            if created.is_some_and(|c| (c <= now && now - c < GRACE) || self.protection.bad_since(&key).is_some_and(|since| c > since)) {
                continue;
            }
            if let Some(alert) = self.protection.observe(&key, Level::Ok, "protected again", now) {
                alerts.push((host.to_string(), dataset.to_string(), alert));
            }
            if self.protection.bad_since(&key).is_some() {
                keys.insert(key);
            }
        }
        let kept: Vec<String> = keys.iter().cloned().collect();
        self.protection.forget_except(&kept);
        self.protection_keys = keys;
        self.unprotected = found;

        let mut groups: Vec<ProtectionGroup> = Vec::new();
        for (host, dataset, alert) in alerts {
            let kind = if alert.kind == AlertKind::Escalation { AlertKind::Alert } else { alert.kind };
            match groups.iter_mut().find(|(h, k, _)| *h == host && *k == kind) {
                Some((_, _, items)) => items.push((dataset, alert)),
                None => groups.push((host, kind, vec![(dataset, alert)])),
            }
        }
        for (host, kind, items) in groups {
            let msg = protection_message(&host, kind, &items, now, &self.hub);
            let event = match kind {
                AlertKind::Alert | AlertKind::Escalation => "alert",
                AlertKind::Reminder => "reminder",
                AlertKind::Recovery => "recovery",
            };
            let text = msg.subject.split_once(": ").map_or(msg.subject.as_str(), |(_, rest)| rest).to_string();
            let _ = self.events.append(&HealthEvent { at: now, host: host.clone(), kind: event.to_string(), text });
            self.pending.push(Pending { msg, attempts: 0, keys: items.into_iter().map(|(_, a)| a.host).collect() });
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
        Probed { name: name.into(), probe: Some(HostProbe::Reached { rtt_ms: 3, pools: vec![], kind: None, os: None, os_name: None }) }
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
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        }
        assert_eq!(sink.sent.len(), 1);
        assert_eq!(sink.sent[0].subject, "[zfsctl] a: unreachable");
        m.observe(t(10), local(10), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        assert_eq!(sink.sent.len(), 1, "no repeat");
        for i in 3..5 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
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
            m.observe(t(i * 5), local(i * 5), &hosts, &[Probed { name: "a".into(), probe: None }], &[], &[], &mut sink);
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
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &jobs, &runs, &mut sink);
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
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        }
        let first = sink.sent.len();
        assert_eq!(first, 1);
        for i in 2..7 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        }
        assert_eq!(sink.sent.len() - first, 5, "six attempts in all: one at first, five retries, then dropped");
        assert!(sink.sent.iter().all(|s| s.subject == "[zfsctl] a: unreachable"));
    }

    #[test]
    fn an_alert_that_was_given_up_on_is_raised_again_instead_of_leaving_a_phantom_open_alert() {
        let mut m = monitor();
        let hosts = [host("a")];
        let mut sink = FakeSink::failing();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        }
        for i in 2..7 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        }
        let dropped_at = sink.sent.len();
        assert_eq!(dropped_at, 6, "six attempts, then dropped");
        sink.ok = true;
        m.observe(t(40), local(40), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        assert_eq!(sink.sent.len(), dropped_at, "one bad probe after forgetting is not enough");
        m.observe(t(45), local(45), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        assert_eq!(sink.sent.len(), dropped_at + 1);
        assert_eq!(sink.sent.last().unwrap().subject, "[zfsctl] a: unreachable", "the same alert, sent anew");
    }

    #[test]
    fn a_dropped_recovery_or_reminder_does_not_reopen_anything() {
        let mut m = monitor();
        let hosts = [host("a")];
        let mut ok = FakeSink::new();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut ok);
        }
        let mut failing = FakeSink::failing();
        for i in 2..4 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut failing);
        }
        for i in 4..12 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut failing);
        }
        let n = failing.sent.len();
        failing.ok = true;
        m.observe(t(70), local(70), &hosts, &[probed_ok("a")], &[], &[], &mut failing);
        assert_eq!(failing.sent.len(), n, "the recovery was given up on and nothing is re-sent for a healthy host");
    }

    #[test]
    fn notify_json_is_written_through_a_rename_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
        m.observe(t(0), at_local(7, 1), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        assert_eq!(std::fs::read_to_string(dir.path().join("notify.json")).unwrap(), "{\"last_report\":\"2026-10-11\"}\n");
        assert!(!dir.path().join("notify.json.tmp").exists() && !dir.path().join("notify.json.json.tmp").exists());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.contains(".tmp")).collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_retry_that_succeeds_is_not_sent_again() {
        let mut m = monitor();
        let hosts = [host("a")];
        let mut sink = FakeSink::failing();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        }
        sink.ok = true;
        m.observe(t(10), local(10), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        let n = sink.sent.len();
        m.observe(t(15), local(15), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
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
        m.observe(t(0), at_local(7, 1), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        assert_eq!(reports(&sink), 1);
        m.observe(t(5), at_local(7, 6), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        assert_eq!(reports(&sink), 1, "not twice");
        let mut again = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(60));
        again.observe(t(60), at_local(8, 0), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        assert_eq!(reports(&sink), 1, "a restarted monitor reads notify.json");
    }

    #[test]
    fn a_failed_report_is_retried_and_only_marked_sent_once_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = [host("a")];
        let mut sink = FakeSink::failing();
        let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
        m.observe(t(0), at_local(7, 1), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        assert!(!dir.path().join("notify.json").exists());
        sink.ok = true;
        m.observe(t(5), at_local(7, 6), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
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
            m.observe(t(i * 5), at_local(7, u32::try_from(i).unwrap()), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
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
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
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
        let probed = [Probed { name: "a".into(), probe: Some(HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: Some(HostKind::Metal), os: None, os_name: None }) }];
        m.observe(t(0), local(0), &hosts, &probed, &[], &[], &mut FakeSink::new());
        assert!(!m.needs_kind("a"));
    }

    #[test]
    fn a_host_whose_probe_is_still_running_is_not_probed_again() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Slow(Arc<AtomicUsize>);
        impl CommandRunner for Slow {
            fn run(&self, _: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
                if args.first() == Some(&"list") {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(900));
                Ok(CommandOutput { stdout: String::new(), stderr: String::new(), status: 0 })
            }
            fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
            fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let hosts = vec![HostInfo { name: "slow".into(), address: "x".into(), tag: String::new(), runner: Arc::new(Slow(Arc::clone(&calls))) }];
        let busy = Busy::default();
        for _ in 0..5 {
            let probed = probe_all(&hosts, &|_| false, Duration::from_millis(100), &busy);
            assert!(probed[0].probe.is_none());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one probe, not five stacked up");
        std::thread::sleep(Duration::from_millis(2500));
        assert!(busy.lock().unwrap().is_empty(), "the host is free again once its probe returned");
        probe_all(&hosts, &|_| false, Duration::from_millis(100), &busy);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn changing_the_settings_keeps_the_alert_state_the_retry_queue_and_does_not_log_a_new_start() {
        let dir = tempfile::tempdir().unwrap();
        let hosts = [host("a")];
        let mut sink = FakeSink::failing();
        let mut m = Monitor::new(settings_with_report("off"), "hub".into(), dir.path().to_path_buf(), t(0));
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        }
        let mut fixed = settings_with_report("off");
        fixed.remind = Some(Duration::from_secs(3600));
        m.set_settings(fixed);
        sink.ok = true;
        m.observe(t(10), local(10), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        assert_eq!(sink.sent.last().unwrap().subject, "[zfsctl] a: unreachable", "the queued alert was retried after the change");
        let n = sink.sent.len();
        m.observe(t(15), local(15), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        assert_eq!(sink.sent.len(), n, "and the host is not alerted as if it were new");
        for i in 4..6 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        assert!(sink.sent.last().unwrap().subject.contains("recovered"), "the recovery still comes");
        let starts = EventLog::new(dir.path().join("health-events.jsonl")).read().iter().filter(|e| e.kind == "daemon-started").count();
        assert_eq!(starts, 1);
    }

    use crate::config::{EmailTiming, ProtectionSettings};
    use crate::daemon::storage_check::{self, Integrity, SeenDataset, StorageFile, StorageResult};

    /// Writes `storage.json` with each host's datasets, created `created_ago` before `t(0)` and checked at `t(0)`.
    fn write_storage(dir: &std::path::Path, hosts: &[(&str, &[&str])], created_ago: chrono::Duration) {
        let mut file = StorageFile::default();
        for (h, names) in hosts {
            let seen = names.iter().map(|n| SeenDataset { name: (*n).into(), volume: false, created: Some((t(0) - created_ago).timestamp()) }).collect();
            file.hosts.insert((*h).into(), StorageResult { checked_at: t(0), datasets: vec![], seen, pools: vec![], integrity: Integrity::default(), locked: vec![], integrity_stale: false, zfs_version: None, error: None });
        }
        file.save(&dir.join(storage_check::FILE)).unwrap();
    }

    fn week() -> chrono::Duration {
        chrono::Duration::days(7)
    }

    fn guarding(email: EmailTiming) -> (Monitor, PathBuf) {
        let dir = tempfile::tempdir().unwrap().keep();
        let mut m = Monitor::new(settings_with_report("off"), "hub".into(), dir.clone(), t(0));
        m.set_protection(ProtectionSettings { email, ..ProtectionSettings::default() });
        (m, dir)
    }

    /// A current snapshot job and a current replication to another host for each dataset.
    fn protecting(host: &str, datasets: &[&str]) -> (Vec<Job>, Vec<Run>) {
        let mut jobs = Vec::new();
        for (i, d) in datasets.iter().enumerate() {
            let mut s = Job::test_snapshot(&format!("s-{host}-{i}"), d, "auto", 24);
            s.host = host.into();
            let mut r = Job::test_replicate(&format!("r-{host}-{i}"), d, "rep", "elsewhere", &format!("backup/{i}"));
            r.host = host.into();
            jobs.push(s);
            jobs.push(r);
        }
        let runs = jobs.iter().map(|j| run(&j.id, Some(RunResult::Ok), "", t(-10))).collect();
        (jobs, runs)
    }

    fn protection_mail(s: &FakeSink) -> Vec<&Message> {
        s.sent.iter().filter(|m| m.subject.contains("protected")).collect()
    }

    #[test]
    fn six_new_unprotected_datasets_on_one_host_send_one_message() {
        let (mut m, dir) = guarding(EmailTiming::Immediate);
        let names = ["tank/a", "tank/b", "tank/c", "tank/d", "tank/e", "tank/f"];
        write_storage(&dir, &[("a", &names)], week());
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        m.observe(t(0), local(0), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        assert!(protection_mail(&sink).is_empty(), "one pass is not enough, as for hosts");
        m.observe(t(5), local(5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        let mail = protection_mail(&sink);
        assert_eq!(mail.len(), 1, "{:?}", sink.sent);
        assert_eq!(mail[0].subject, "[zfsctl] a: 6 datasets not protected");
        assert_eq!((mail[0].kind.as_str(), mail[0].host.as_str(), mail[0].level.as_str()), ("health-alert", "a", "critical"));
        for n in names {
            assert_eq!(mail[0].body.lines().filter(|l| l.trim_start().starts_with(&format!("{n}:"))).count(), 1, "{n} in\n{}", mail[0].body);
        }
        assert!(mail[0].body.contains("tank/a: no snapshots; no copy on another machine"), "{}", mail[0].body);
        assert_eq!(sink.sent.len(), 1, "the healthy host itself sends nothing");
        let events = EventLog::new(dir.join("health-events.jsonl")).read();
        assert_eq!(events.iter().filter(|e| e.kind == "alert" && e.host == "a").count(), 1, "one event for the message, not six: {events:?}");
    }

    #[test]
    fn os_data_is_not_mailed_but_stays_in_the_report_list() {
        let (mut m, dir) = guarding(EmailTiming::Immediate);
        write_storage(&dir, &[("a", &["zroot/var/log", "zroot/tmp"])], week());
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        for i in 0..4 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        assert!(protection_mail(&sink).is_empty(), "{:?}", sink.sent);
        assert_eq!(m.unprotected.len(), 2, "still judged, for the report");

        let (mut m, dir) = guarding(EmailTiming::Immediate);
        write_storage(&dir, &[("a", &["tank/photos", "zroot/var/log"])], week());
        let mut sink = FakeSink::new();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        let mail = protection_mail(&sink);
        assert_eq!(mail.len(), 1);
        assert_eq!(mail[0].subject, "[zfsctl] a: 1 dataset not protected");
        assert!(!mail[0].body.contains("zroot/var/log"), "{}", mail[0].body);
    }

    #[test]
    fn protection_reminders_are_at_most_daily() {
        let (mut m, dir) = guarding(EmailTiming::Immediate);
        assert_eq!(m.settings().remind, Some(Duration::from_secs(6 * 3600)), "the host reminders stay at six hours");
        write_storage(&dir, &[("a", &["tank/x"])], week());
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        let mut at = |min: i64, m: &mut Monitor, sink: &mut FakeSink| m.observe(t(min), local(min % 1440), &hosts, &[probed_ok("a")], &[], &[], sink);
        at(0, &mut m, &mut sink);
        at(5, &mut m, &mut sink);
        assert_eq!(protection_mail(&sink).len(), 1);
        for h in [7, 13, 19, 23] {
            at(h * 60, &mut m, &mut sink);
        }
        assert_eq!(protection_mail(&sink).len(), 1, "no six-hourly reminder: {:?}", sink.sent);
        at(24 * 60 + 10, &mut m, &mut sink);
        let mail = protection_mail(&sink);
        assert_eq!(mail.len(), 2);
        assert!(mail[1].subject.starts_with("[zfsctl] a: 1 dataset still not protected"), "{}", mail[1].subject);
        // A longer configured interval is kept.
        let mut s = settings_with_report("off");
        s.remind = Some(Duration::from_secs(48 * 3600));
        m.set_settings(s);
        at(49 * 60, &mut m, &mut sink);
        assert_eq!(protection_mail(&sink).len(), 2, "48 h asked: no reminder after 24 h");
    }

    #[test]
    fn a_dataset_in_its_grace_sends_nothing() {
        let (mut m, dir) = guarding(EmailTiming::Immediate);
        write_storage(&dir, &[("a", &["tank/new"])], chrono::Duration::hours(1));
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        for i in 0..4 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        assert!(sink.sent.is_empty(), "{:?}", sink.sent);
    }

    #[test]
    fn recovery_is_one_message_per_host() {
        let (mut m, dir) = guarding(EmailTiming::Immediate);
        write_storage(&dir, &[("a", &["tank/x", "tank/y"]), ("b", &["tank/p", "tank/q"])], week());
        let hosts = [host("a"), host("b")];
        let probed = [probed_ok("a"), probed_ok("b")];
        let mut sink = FakeSink::new();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, &probed, &[], &[], &mut sink);
        }
        assert_eq!(protection_mail(&sink).len(), 2, "one alert per host");
        let (mut jobs, mut runs) = protecting("a", &["tank/x", "tank/y"]);
        let (more_jobs, more_runs) = protecting("b", &["tank/p", "tank/q"]);
        jobs.extend(more_jobs);
        runs.extend(more_runs);
        m.observe(t(10), local(10), &hosts, &probed, &jobs, &runs, &mut sink);
        assert_eq!(protection_mail(&sink).len(), 2, "one good pass does not recover");
        m.observe(t(15), local(15), &hosts, &probed, &jobs, &runs, &mut sink);
        let mail = protection_mail(&sink);
        assert_eq!(mail.len(), 4, "{:?}", sink.sent);
        let mut subjects: Vec<&str> = mail[2..].iter().map(|m| m.subject.as_str()).collect();
        subjects.sort_unstable();
        assert_eq!(subjects, ["[zfsctl] a: 2 datasets protected again", "[zfsctl] b: 2 datasets protected again"]);
        assert!(mail[2..].iter().all(|m| m.kind == "health-recovered" && m.level == "ok"));
        m.observe(t(20), local(20), &hosts, &probed, &jobs, &runs, &mut sink);
        assert_eq!(protection_mail(&sink).len(), 4, "once");
    }

    #[test]
    fn a_second_pass_sends_no_duplicate() {
        let (mut m, dir) = guarding(EmailTiming::Immediate);
        write_storage(&dir, &[("a", &["tank/x"])], week());
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        for i in 0..6 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        let mail = protection_mail(&sink);
        assert_eq!(mail.len(), 1, "{:?}", sink.sent);
        assert_eq!(mail[0].subject, "[zfsctl] a: 1 dataset not protected");
    }

    #[test]
    fn a_failed_delivery_is_retried_then_alerts_again() {
        let (mut m, dir) = guarding(EmailTiming::Immediate);
        write_storage(&dir, &[("a", &["tank/x", "tank/y"])], week());
        let hosts = [host("a")];
        let mut sink = FakeSink::failing();
        for i in 0..7 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        assert_eq!(sink.sent.len(), 6, "six attempts, then dropped: {:?}", sink.sent);
        assert!(sink.sent.iter().all(|s| s.subject == "[zfsctl] a: 2 datasets not protected"));
        sink.ok = true;
        m.observe(t(40), local(40), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        assert_eq!(sink.sent.len(), 6, "forgotten: one pass is not enough again");
        m.observe(t(45), local(45), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        assert_eq!(sink.sent.len(), 7);
        assert_eq!(sink.sent[6].subject, "[zfsctl] a: 2 datasets not protected", "the same alert, sent anew");
        // Giving up on the protection alert did not touch the host's own alert state: a bad host still alerts after two probes.
        m.observe(t(50), local(50), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        m.observe(t(55), local(55), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        assert_eq!(sink.sent.last().unwrap().subject, "[zfsctl] a: unreachable");
    }

    #[test]
    fn an_unreachable_host_sends_no_protection_mail() {
        let (mut m, dir) = guarding(EmailTiming::Immediate);
        write_storage(&dir, &[("a", &["tank/x"]), ("b", &["tank/y"])], week());
        let hosts = [host("a"), host("b")];
        let mut sink = FakeSink::new();
        for i in 0..3 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a"), Probed { name: "b".into(), probe: None }], &[], &[], &mut sink);
        }
        assert!(protection_mail(&sink).is_empty(), "{:?}", sink.sent);
        assert!(sink.sent.iter().any(|s| s.subject == "[zfsctl] a: unreachable"));

        // Alerted while reachable; while it is unreachable nothing changes, not even when its jobs are fixed.
        let (mut m, dir) = guarding(EmailTiming::Immediate);
        write_storage(&dir, &[("a", &["tank/x"])], week());
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        assert_eq!(protection_mail(&sink).len(), 1);
        let (jobs, runs) = protecting("a", &["tank/x"]);
        for i in 2..6 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &jobs, &runs, &mut sink);
        }
        assert_eq!(protection_mail(&sink).len(), 1, "no recovery while the host cannot be seen");
        for i in 6..8 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &jobs, &runs, &mut sink);
        }
        let mail = protection_mail(&sink);
        assert_eq!(mail.len(), 2, "{:?}", sink.sent);
        assert_eq!(mail[1].subject, "[zfsctl] a: 1 dataset protected again", "the open alert was kept and recovers");
    }

    #[test]
    fn a_destroyed_dataset_recovers_silently() {
        let (mut m, dir) = guarding(EmailTiming::Immediate);
        write_storage(&dir, &[("a", &["tank/x", "tank/y"])], week());
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        assert_eq!(protection_mail(&sink).len(), 1);
        write_storage(&dir, &[("a", &[])], week());
        for i in 2..6 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        assert_eq!(protection_mail(&sink).len(), 1, "no 'protected again' for datasets that are gone: {:?}", sink.sent);
        // Forgotten: a dataset of the same name made later is new.
        write_storage(&dir, &[("a", &["tank/x"])], week());
        for i in 6..8 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        let mail = protection_mail(&sink);
        assert_eq!(mail.len(), 2);
        assert_eq!(mail[1].subject, "[zfsctl] a: 1 dataset not protected");
    }

    #[test]
    fn judge_reachable_says_which_hosts_were_not_checked_and_why() {
        let dir = tempfile::tempdir().unwrap();
        write_storage(dir.path(), &[("a", &["tank/x"]), ("down", &["tank/y"]), ("old", &["tank/z"])], week());
        let mut file = StorageFile::load(&dir.path().join(storage_check::FILE));
        file.hosts.get_mut("old").unwrap().checked_at = t(0) - chrono::Duration::days(3);
        let hosts = [host("a"), host("down"), host("new"), host("old")];
        let probed = [probed_ok("a"), probed_bad("down"), probed_ok("new"), probed_ok("old")];
        let rows = build_rows(&hosts, &probed, &HashMap::new(), &[], &[]);
        let judged = judge_reachable(&file, &probed, &rows, &[], &[], &ProtectionSettings::default(), t(0));
        assert_eq!(judged.found.iter().map(|u| (u.host.as_str(), u.dataset.as_str())).collect::<Vec<_>>(), [("a", "tank/x")]);
        assert_eq!(judged.known, HashSet::from(["a"]));
        assert_eq!(judged.unchecked, ["down (unreachable)", "new (no dataset list yet)", "old (no dataset list for 3d)"]);
        let section = judged.section(&|_| None, t(0));
        assert_eq!((section.judged, section.unchecked.len(), section.lines.len()), (1, 3, 1));
        // A probe that never answered is unreachable too.
        let probed = [probed_ok("a"), Probed { name: "down".into(), probe: None }, probed_ok("new"), probed_ok("old")];
        let rows = build_rows(&hosts, &probed, &HashMap::new(), &[], &[]);
        assert_eq!(judge_reachable(&file, &probed, &rows, &[], &[], &ProtectionSettings::default(), t(0)).unchecked[0], "down (unreachable)");
    }

    #[test]
    fn the_daily_report_names_the_hosts_it_could_not_check() {
        let dir = tempfile::tempdir().unwrap().keep();
        let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.clone(), t(0));
        m.set_protection(ProtectionSettings { email: EmailTiming::Report, ..ProtectionSettings::default() });
        let (jobs, runs) = protecting("a", &["tank/x"]);
        write_storage(&dir, &[("a", &["tank/x"])], week());
        let hosts = [host("a"), host("b")];
        let mut sink = FakeSink::new();
        m.observe(t(0), at_local(7, 1), &hosts, &[probed_ok("a"), probed_bad("b")], &jobs, &runs, &mut sink);
        let report = sink.sent.iter().find(|s| s.kind == "health-report").unwrap();
        assert!(report.body.contains("UNPROTECTED DATA\n  All datasets on 1 host are protected; not checked: b (unreachable)\n"), "{}", report.body);
        assert!(!report.body.contains("All datasets are protected."), "{}", report.body);
    }

    #[test]
    fn a_dataset_destroyed_and_made_again_is_new_not_protected_again() {
        // Made again inside one storage-check gap: still listed, but in its grace.
        let (mut m, dir) = guarding(EmailTiming::Immediate);
        write_storage(&dir, &[("a", &["tank/x"])], week());
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        assert_eq!(protection_mail(&sink).len(), 1);
        write_storage(&dir, &[("a", &["tank/x"])], chrono::Duration::hours(1));
        for i in 2..6 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        assert_eq!(protection_mail(&sink).len(), 1, "a new dataset in its grace is not 'protected again': {:?}", sink.sent);
        assert!(m.protection.bad_since("a\ttank/x").is_none() && !m.protection_keys.contains("a\ttank/x"), "forgotten");

        // Made again after the alert was sent (the alert's start) and past its grace, now protected: still new, no recovery mail.
        let (mut m, dir) = guarding(EmailTiming::Immediate);
        write_storage(&dir, &[("a", &["tank/x"])], week());
        let mut sink = FakeSink::new();
        for i in 0..2 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        assert_eq!(protection_mail(&sink).len(), 1);
        write_storage(&dir, &[("a", &["tank/x"])], chrono::Duration::minutes(-10));
        let later = t(26 * 60);
        let (jobs, _) = protecting("a", &["tank/x"]);
        let runs: Vec<Run> = jobs.iter().map(|j| run(&j.id, Some(RunResult::Ok), "", later - chrono::Duration::minutes(10))).collect();
        for i in 0..3 {
            m.observe(later + chrono::Duration::minutes(i * 5), local(26 * 60 + i * 5), &hosts, &[probed_ok("a")], &jobs, &runs, &mut sink);
        }
        assert_eq!(protection_mail(&sink).len(), 1, "{:?}", sink.sent);
    }

    #[test]
    fn the_report_says_how_long_a_dataset_has_been_unprotected_in_report_timing_too() {
        let dir = tempfile::tempdir().unwrap().keep();
        let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.clone(), t(0));
        m.set_protection(ProtectionSettings { email: EmailTiming::Report, ..ProtectionSettings::default() });
        write_storage(&dir, &[("a", &["tank/x", "zroot/var/log"])], week());
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        m.observe(t(0), at_local(6, 0), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        m.observe(t(90), at_local(7, 30), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        let report = sink.sent.iter().find(|s| s.kind == "health-report").unwrap();
        assert!(report.body.contains("  a:tank/x (your data): no snapshots; no copy on another machine (for 1h 30m)\n"), "{}", report.body);
        assert!(report.body.contains("  a:zroot/var/log (OS data): no snapshots; no copy on another machine (for 1h 30m)\n"), "{}", report.body);
        // Protected or gone: forgotten, so it starts again from zero.
        write_storage(&dir, &[("a", &["tank/x"])], week());
        m.observe(t(100), at_local(7, 40), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        assert!(!m.first_seen.contains_key("a\tzroot/var/log") && m.first_seen.contains_key("a\ttank/x"));
        // An unreachable host is not a verdict: its datasets keep their first-seen time.
        m.observe(t(105), at_local(7, 45), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
        assert_eq!(m.first_seen.get("a\ttank/x"), Some(&t(0)));
        m.set_protection(ProtectionSettings { email: EmailTiming::Off, ..ProtectionSettings::default() });
        m.observe(t(110), at_local(7, 50), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        assert!(m.first_seen.is_empty(), "off keeps nothing");
    }

    #[test]
    fn report_and_off_send_no_alerts() {
        for email in [EmailTiming::Report, EmailTiming::Off] {
            let (mut m, dir) = guarding(email);
            write_storage(&dir, &[("a", &["tank/x", "tank/y"])], week());
            let hosts = [host("a")];
            let mut sink = FakeSink::new();
            for i in 0..4 {
                m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
            }
            assert!(sink.sent.is_empty(), "{email:?}: {:?}", sink.sent);
            let listed: Vec<&str> = m.unprotected.iter().map(|u| u.dataset.as_str()).collect();
            if email == EmailTiming::Report {
                assert_eq!(listed, ["tank/x", "tank/y"], "kept for the daily report");
            } else {
                assert!(listed.is_empty(), "off: not even judged");
            }
        }
    }

    #[test]
    fn switching_to_immediate_sends_one_message_per_host_for_the_current_list() {
        let (mut m, dir) = guarding(EmailTiming::Report);
        write_storage(&dir, &[("a", &["tank/1", "tank/2", "tank/3"]), ("b", &["tank/4", "tank/5", "tank/6"])], week());
        let hosts = [host("a"), host("b")];
        let probed = [probed_ok("a"), probed_ok("b")];
        let mut sink = FakeSink::new();
        for i in 0..3 {
            m.observe(t(i * 5), local(i * 5), &hosts, &probed, &[], &[], &mut sink);
        }
        assert!(sink.sent.is_empty());
        m.set_protection(ProtectionSettings { email: EmailTiming::Immediate, ..ProtectionSettings::default() });
        for i in 3..6 {
            m.observe(t(i * 5), local(i * 5), &hosts, &probed, &[], &[], &mut sink);
        }
        let mut subjects: Vec<&str> = protection_mail(&sink).iter().map(|m| m.subject.as_str()).collect();
        subjects.sort_unstable();
        assert_eq!(subjects, ["[zfsctl] a: 3 datasets not protected", "[zfsctl] b: 3 datasets not protected"]);

        // Off and on again: the old alerts are cleared, so it is again one message per host, and no recovery for the switch.
        m.set_protection(ProtectionSettings { email: EmailTiming::Off, ..ProtectionSettings::default() });
        m.observe(t(30), local(30), &hosts, &probed, &[], &[], &mut sink);
        assert_eq!(protection_mail(&sink).len(), 2, "no recovery mail for turning it off");
        m.set_protection(ProtectionSettings { email: EmailTiming::Immediate, ..ProtectionSettings::default() });
        for i in 7..9 {
            m.observe(t(i * 5), local(i * 5), &hosts, &probed, &[], &[], &mut sink);
        }
        assert_eq!(protection_mail(&sink).len(), 4, "{:?}", sink.sent);
    }

    #[test]
    fn report_wait_says_how_long_until_the_report_is_due() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
        assert_eq!(m.report_wait(at_local(6, 0)), Some(Duration::from_secs(3600)));
        assert_eq!(m.report_wait(at_local(7, 0)), Some(Duration::ZERO), "due now");
        assert_eq!(m.report_wait(at_local(15, 0)), Some(Duration::ZERO), "overdue: sent as soon as a tick runs");
        m.mark_report_sent(at_local(7, 1).date(), false);
        assert_eq!(m.report_wait(at_local(8, 0)), None, "sent today");
        let next_day = at_local(0, 5) + chrono::Duration::days(1);
        assert_eq!(m.report_wait(next_day), Some(Duration::from_secs(6 * 3600 + 55 * 60)), "after midnight the wait is for the next 07:00");
        assert_eq!(monitor().report_wait(at_local(6, 0)), None, "no report configured");
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
        let probed = probe_all(&hosts, &|_| false, Duration::from_millis(500), &Busy::default());
        assert!(t0.elapsed() < Duration::from_secs(2));
        assert_eq!(probed.len(), 2);
        assert!(probed.iter().find(|p| p.name == "slow").unwrap().probe.is_none());
        assert!(matches!(probed.iter().find(|p| p.name == "fast").unwrap().probe, Some(HostProbe::Reached { .. })));
    }
}
