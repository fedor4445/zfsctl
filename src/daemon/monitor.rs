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
    remind.map(|d| d.max(Duration::from_hours(24)))
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
mod tests;
