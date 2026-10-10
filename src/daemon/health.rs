//! When to tell the operator about a host: the alert state machine, with no clock and no I/O of its own.

use crate::zfs::host_health::Level;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Ok,
    Warning,
    Critical,
}

/// `None` for a host whose first probe has not finished: it says nothing either way.
pub fn severity(level: Level) -> Option<Severity> {
    match level {
        Level::Probing => None,
        Level::Ok => Some(Severity::Ok),
        Level::Warning => Some(Severity::Warning),
        Level::Critical | Level::Disconnected => Some(Severity::Critical),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Alert,
    Escalation,
    Reminder,
    Recovery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub host: String,
    pub kind: AlertKind,
    pub severity: Severity,
    pub reason: String,
    /// How long the host has been bad (escalations, reminders and recoveries).
    pub bad_for: Option<Duration>,
}

#[derive(Debug, Clone)]
struct Alerted {
    severity: Severity,
    since: DateTime<Utc>,
    last_sent: DateTime<Utc>,
}

#[derive(Debug, Default, Clone)]
struct HostState {
    streak_of: Option<Severity>,
    streak: u32,
    alerted: Option<Alerted>,
}

pub struct Tracker {
    confirm: u32,
    remind: Option<Duration>,
    hosts: HashMap<String, HostState>,
}

fn elapsed(from: DateTime<Utc>, to: DateTime<Utc>) -> Duration {
    (to - from).to_std().unwrap_or_default()
}

impl Tracker {
    pub fn new(confirm: u32, remind: Option<Duration>) -> Tracker {
        Tracker { confirm: confirm.max(1), remind, hosts: HashMap::new() }
    }

    /// When the alert that is still open for `host` was first sent.
    pub fn bad_since(&self, host: &str) -> Option<DateTime<Utc>> {
        self.hosts.get(host).and_then(|s| s.alerted.as_ref()).map(|a| a.since)
    }

    pub fn forget_except(&mut self, hosts: &[String]) {
        self.hosts.retain(|name, _| hosts.contains(name));
    }

    /// Feeds one probe result; returns the message it calls for, if any.
    pub fn observe(&mut self, host: &str, level: Level, reason: &str, now: DateTime<Utc>) -> Option<Alert> {
        let sev = severity(level)?;
        let (confirm, remind) = (self.confirm, self.remind);
        let state = self.hosts.entry(host.to_string()).or_default();
        if state.streak_of == Some(sev) {
            state.streak += 1;
        } else {
            state.streak_of = Some(sev);
            state.streak = 1;
        }
        let confirmed = state.streak >= confirm;
        let alert = |kind, severity, bad_for| Alert { host: host.to_string(), kind, severity, reason: reason.to_string(), bad_for };
        if sev == Severity::Ok {
            if confirmed {
                if let Some(a) = state.alerted.take() {
                    return Some(alert(AlertKind::Recovery, Severity::Ok, Some(elapsed(a.since, now))));
                }
            }
            return None;
        }
        match state.alerted.as_mut() {
            None if confirmed => {
                state.alerted = Some(Alerted { severity: sev, since: now, last_sent: now });
                Some(alert(AlertKind::Alert, sev, None))
            }
            None => None,
            Some(a) if sev > a.severity && confirmed => {
                a.severity = sev;
                a.last_sent = now;
                Some(alert(AlertKind::Escalation, sev, Some(elapsed(a.since, now))))
            }
            Some(a) => {
                if sev < a.severity && confirmed {
                    a.severity = sev;
                }
                match remind {
                    Some(every) if elapsed(a.last_sent, now) >= every => {
                        a.last_sent = now;
                        Some(alert(AlertKind::Reminder, a.severity, Some(elapsed(a.since, now))))
                    }
                    _ => None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(min: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-11T00:00:00Z").unwrap().with_timezone(&Utc) + chrono::Duration::minutes(min)
    }

    fn tracker() -> Tracker {
        Tracker::new(2, Some(Duration::from_secs(6 * 3600)))
    }

    #[test]
    fn a_single_bad_probe_never_alerts_but_two_in_a_row_do() {
        let mut tr = tracker();
        assert!(tr.observe("a", Level::Critical, "tank DEGRADED", t(0)).is_none());
        assert!(tr.observe("a", Level::Ok, "ok", t(5)).is_none(), "the blip is over");
        assert!(tr.observe("a", Level::Critical, "tank DEGRADED", t(10)).is_none(), "the streak restarted");
        let alert = tr.observe("a", Level::Critical, "tank DEGRADED", t(15)).unwrap();
        assert_eq!((alert.kind, alert.severity, alert.reason.as_str()), (AlertKind::Alert, Severity::Critical, "tank DEGRADED"));
    }

    #[test]
    fn disconnected_counts_as_critical_and_probing_counts_for_nothing() {
        let mut tr = tracker();
        assert!(tr.observe("a", Level::Probing, "probing", t(0)).is_none());
        assert!(tr.observe("a", Level::Disconnected, "unreachable", t(1)).is_none());
        assert!(tr.observe("a", Level::Probing, "probing", t(2)).is_none(), "does not break the streak");
        assert_eq!(tr.observe("a", Level::Disconnected, "unreachable", t(3)).unwrap().severity, Severity::Critical);
    }

    #[test]
    fn no_repeat_until_the_reminder_is_due_then_one_reminder_that_says_how_long() {
        let mut tr = tracker();
        tr.observe("a", Level::Warning, "tank 85% full", t(0));
        assert!(tr.observe("a", Level::Warning, "tank 85% full", t(5)).is_some());
        for m in [10, 60, 300] {
            assert!(tr.observe("a", Level::Warning, "tank 85% full", t(m)).is_none(), "minute {m}");
        }
        let r = tr.observe("a", Level::Warning, "tank 86% full", t(5 + 361)).unwrap();
        assert_eq!((r.kind, r.reason.as_str()), (AlertKind::Reminder, "tank 86% full"));
        assert_eq!(r.bad_for, Some(Duration::from_secs(361 * 60)));
        assert!(tr.observe("a", Level::Warning, "tank 86% full", t(5 + 362)).is_none(), "the clock restarts after a reminder");
    }

    #[test]
    fn a_reminder_interval_of_none_never_reminds() {
        let mut tr = Tracker::new(2, None);
        tr.observe("a", Level::Warning, "x", t(0));
        tr.observe("a", Level::Warning, "x", t(5));
        assert!(tr.observe("a", Level::Warning, "x", t(60 * 48)).is_none());
    }

    #[test]
    fn escalation_needs_its_own_two_probes_and_a_changed_reason_alone_is_silent() {
        let mut tr = tracker();
        tr.observe("a", Level::Warning, "tank 85% full", t(0));
        tr.observe("a", Level::Warning, "tank 85% full", t(5));
        assert!(tr.observe("a", Level::Warning, "tank 3 errors", t(10)).is_none(), "same level, new reason: silent");
        assert!(tr.observe("a", Level::Critical, "tank DEGRADED", t(15)).is_none());
        let e = tr.observe("a", Level::Critical, "tank DEGRADED", t(20)).unwrap();
        assert_eq!((e.kind, e.severity), (AlertKind::Escalation, Severity::Critical));
    }

    #[test]
    fn recovery_needs_two_good_probes_and_an_earlier_alert() {
        let mut tr = tracker();
        assert!(tr.observe("never", Level::Ok, "ok", t(0)).is_none() && tr.observe("never", Level::Ok, "ok", t(5)).is_none(), "no alert, so no recovery");
        tr.observe("a", Level::Critical, "x", t(0));
        tr.observe("a", Level::Critical, "x", t(5));
        assert!(tr.observe("a", Level::Ok, "ok", t(10)).is_none());
        let r = tr.observe("a", Level::Ok, "ok", t(15)).unwrap();
        assert_eq!((r.kind, r.bad_for), (AlertKind::Recovery, Some(Duration::from_secs(10 * 60))));
        assert!(tr.observe("a", Level::Ok, "ok", t(20)).is_none(), "once");
        assert!(tr.bad_since("a").is_none());
    }

    #[test]
    fn a_good_blip_while_alerted_does_not_recover_and_a_return_to_bad_does_not_realert() {
        let mut tr = tracker();
        tr.observe("a", Level::Critical, "x", t(0));
        tr.observe("a", Level::Critical, "x", t(5));
        assert!(tr.observe("a", Level::Ok, "ok", t(10)).is_none());
        assert!(tr.observe("a", Level::Critical, "x", t(15)).is_none());
        assert!(tr.observe("a", Level::Critical, "x", t(20)).is_none(), "still the same alert");
        assert_eq!(tr.bad_since("a"), Some(t(5)));
    }

    #[test]
    fn hosts_are_tracked_independently_and_removed_hosts_are_forgotten() {
        let mut tr = tracker();
        tr.observe("a", Level::Critical, "x", t(0));
        tr.observe("b", Level::Critical, "y", t(0));
        assert!(tr.observe("a", Level::Critical, "x", t(5)).is_some());
        tr.forget_except(&["b".to_string()]);
        assert!(tr.bad_since("a").is_none());
        assert!(tr.observe("b", Level::Critical, "y", t(5)).is_some());
    }

    #[test]
    fn a_de_escalation_is_silent_and_remembered() {
        let mut tr = tracker();
        tr.observe("a", Level::Critical, "x", t(0));
        tr.observe("a", Level::Critical, "x", t(5));
        tr.observe("a", Level::Warning, "w", t(10));
        assert!(tr.observe("a", Level::Warning, "w", t(15)).is_none());
        tr.observe("a", Level::Critical, "x", t(20));
        let e = tr.observe("a", Level::Critical, "x", t(25)).unwrap();
        assert_eq!(e.kind, AlertKind::Escalation, "going back up is an escalation again");
    }
}
