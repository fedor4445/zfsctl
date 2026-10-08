//! Decides which jobs are due. Pure: it never reads a clock, runs a job or
//! touches a file; the caller passes `now` and acts on what comes back.

use crate::daemon::cron::Schedule;
use crate::jobs::{self, Job};
use chrono::{DateTime, TimeZone, Utc};
use std::collections::HashMap;

/// A job that should start now.
#[derive(Debug, Clone, PartialEq)]
pub struct Due {
    pub job: Job,
    /// The scheduled slot this run is for.
    pub scheduled_for: DateTime<Utc>,
    /// True when this run makes up a slot missed while the daemon was down.
    pub catch_up: bool,
}

/// A job the daemon will not run, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    pub id: String,
    pub reason: String,
}

struct Entry {
    job: Job,
    schedule: Schedule,
    /// Slots at or before this have been dealt with (or predate the job).
    baseline: DateTime<Utc>,
    next: Option<DateTime<Utc>>,
}

pub struct Scheduler<Tz: TimeZone> {
    tz: Tz,
    entries: Vec<Entry>,
}

impl<Tz: TimeZone> Scheduler<Tz> {
    pub fn new(tz: Tz) -> Self {
        Scheduler { tz, entries: Vec::new() }
    }

    fn next_after(&self, schedule: &Schedule, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        schedule.next_after(&after.with_timezone(&self.tz)).map(|t| t.with_timezone(&Utc))
    }

    /// Replaces the job set. A job whose definition is unchanged keeps its
    /// pending time; a new or edited one starts from `now`. Disabled jobs are
    /// left out silently; jobs the daemon cannot run are returned with the reason.
    pub fn reload(&mut self, jobs: Vec<Job>, hosts: &[String], last_scheduled: &HashMap<String, DateTime<Utc>>, now: DateTime<Utc>) -> Vec<Unsupported> {
        let mut old = std::mem::take(&mut self.entries);
        let mut unsupported = Vec::new();
        let clashes = jobs::duplicate_prefix_reasons(&jobs);
        for job in jobs {
            let schedule = match jobs::resolve(&job, hosts) {
                Ok(s) => s,
                Err(reason) => {
                    unsupported.push(Unsupported { id: job.id.clone(), reason });
                    continue;
                }
            };
            if let Some(reason) = clashes.get(&job.id) {
                unsupported.push(Unsupported { id: job.id.clone(), reason: reason.clone() });
                continue;
            }
            if !job.enabled {
                continue;
            }
            if let Some(i) = old.iter().position(|e| e.job == job) {
                self.entries.push(old.swap_remove(i));
                continue;
            }
            let baseline = [job.created_at, last_scheduled.get(&job.id).copied()].into_iter().flatten().max().unwrap_or(now);
            let next = self.next_after(&schedule, now);
            self.entries.push(Entry { job, schedule, baseline, next });
        }
        unsupported
    }

    /// Run once when the daemon starts: every catch-up job whose latest
    /// scheduled slot is newer than its baseline is due now, however many
    /// slots were missed.
    pub fn catch_up(&mut self, now: DateTime<Utc>) -> Vec<Due> {
        let mut due = Vec::new();
        for i in 0..self.entries.len() {
            let Some(entry) = self.entries.get(i) else { continue };
            if !entry.job.catch_up {
                continue;
            }
            let latest = entry.schedule.latest_at_or_before(&now.with_timezone(&self.tz)).map(|t| t.with_timezone(&Utc));
            if let Some(slot) = latest.filter(|slot| *slot > entry.baseline) {
                due.push(Due { job: entry.job.clone(), scheduled_for: slot, catch_up: true });
            }
        }
        due
    }

    /// The jobs whose scheduled time has arrived. Each fires at most once per
    /// slot: afterwards its next time is computed from the later of `now` and
    /// the slot, so a stalled tick fires once (not once per missed slot) and a
    /// clock stepped backwards cannot fire a slot twice.
    pub fn tick(&mut self, now: DateTime<Utc>) -> Vec<Due> {
        let mut due = Vec::new();
        for i in 0..self.entries.len() {
            let Some(entry) = self.entries.get(i) else { continue };
            let Some(slot) = entry.next.filter(|slot| *slot <= now) else { continue };
            due.push(Due { job: entry.job.clone(), scheduled_for: slot, catch_up: false });
            let next = self.next_after(&entry.schedule, now.max(slot));
            if let Some(entry) = self.entries.get_mut(i) {
                entry.next = next;
            }
        }
        due
    }

    /// When `id` next fires.
    #[cfg(test)]
    pub fn next_time(&self, id: &str) -> Option<DateTime<Utc>> {
        self.entries.iter().find(|e| e.job.id == id).and_then(|e| e.next)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::KIND_SCRUB;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn job(id: &str, schedule: &str) -> Job {
        Job {
            id: id.to_string(),
            host: "local".to_string(),
            kind: KIND_SCRUB.to_string(),
            pool: Some("tank".to_string()),
            schedule: schedule.to_string(),
            catch_up: true,
            enabled: true,
            created_at: None,
            dataset: None,
            recursive: false,
            prefix: None,
            keep: None,
            dest_host: None,
            dest_dataset: None,
            dest_dir: None,
            force: false, repository: None, password_file: None, retention: None,
        }
    }

    fn hosts() -> Vec<String> {
        vec!["local".to_string()]
    }

    fn loaded(jobs: Vec<Job>, last: &[(&str, &str)], now: &str) -> Scheduler<Utc> {
        let mut s = Scheduler::new(Utc);
        let last: HashMap<String, DateTime<Utc>> = last.iter().map(|(k, v)| ((*k).to_string(), at(v))).collect();
        assert!(s.reload(jobs, &hosts(), &last, at(now)).is_empty());
        s
    }

    #[test]
    fn reload_reports_what_it_cannot_run_and_silently_drops_disabled_jobs() {
        let mut bad_host = job("bad-host", "0 3 * * *");
        bad_host.host = "gone".to_string();
        let mut kind = job("kind", "0 3 * * *");
        kind.kind = "restic".to_string();
        let mut off = job("off", "0 3 * * *");
        off.enabled = false;
        let mut s = Scheduler::new(Utc);
        let report = s.reload(vec![job("ok", "0 3 * * *"), bad_host, kind, off], &hosts(), &HashMap::new(), at("2026-10-06T00:00:00Z"));
        assert_eq!(report.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(), vec!["bad-host", "kind"]);
        assert_eq!(s.len(), 1, "only the enabled, runnable job is scheduled");
    }

    #[test]
    fn a_job_fires_once_at_its_slot_and_then_waits_for_the_next() {
        let mut s = loaded(vec![job("j", "0 3 * * *")], &[], "2026-10-06T02:00:00Z");
        assert!(s.tick(at("2026-10-06T02:59:59Z")).is_empty());
        let due = s.tick(at("2026-10-06T03:00:00Z"));
        assert_eq!((due.len(), due[0].scheduled_for, due[0].catch_up), (1, at("2026-10-06T03:00:00Z"), false));
        assert!(s.tick(at("2026-10-06T03:00:01Z")).is_empty(), "not twice for one slot");
        assert_eq!(s.next_time("j"), Some(at("2026-10-07T03:00:00Z")));
    }

    #[test]
    fn a_stalled_tick_fires_once_not_once_per_missed_slot() {
        let mut s = loaded(vec![job("j", "0 * * * *")], &[], "2026-10-06T00:30:00Z");
        let due = s.tick(at("2026-10-06T09:15:00Z"));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].scheduled_for, at("2026-10-06T01:00:00Z"));
        assert_eq!(s.next_time("j"), Some(at("2026-10-06T10:00:00Z")), "the next slot comes from now, not from the old slot");
    }

    #[test]
    fn a_clock_stepped_backwards_cannot_fire_a_slot_twice() {
        let mut s = loaded(vec![job("j", "0 3 * * *")], &[], "2026-10-06T02:00:00Z");
        assert_eq!(s.tick(at("2026-10-06T03:00:00Z")).len(), 1);
        assert!(s.tick(at("2026-10-06T02:30:00Z")).is_empty(), "the clock went back an hour");
        assert!(s.tick(at("2026-10-06T03:00:00Z")).is_empty(), "and forward again to the same slot");
    }

    #[test]
    fn an_unchanged_job_keeps_its_pending_time_across_a_reload_and_an_edited_one_restarts() {
        let mut s = loaded(vec![job("j", "0 3 * * *")], &[], "2026-10-06T02:00:00Z");
        let _ = s.reload(vec![job("j", "0 3 * * *")], &hosts(), &HashMap::new(), at("2026-10-06T02:59:30Z"));
        assert_eq!(s.tick(at("2026-10-06T03:00:00Z")).len(), 1, "the slot was not lost to a reload");
        let _ = s.reload(vec![job("j", "30 4 * * *")], &hosts(), &HashMap::new(), at("2026-10-06T03:10:00Z"));
        assert_eq!(s.next_time("j"), Some(at("2026-10-06T04:30:00Z")));
    }

    #[test]
    fn catch_up_runs_a_missed_slot_once_however_many_were_missed() {
        // Weekly on Sunday 03:00; last run was three weeks ago; the daemon starts on a Wednesday.
        let mut s = loaded(vec![job("j", "0 3 * * 0")], &[("j", "2026-09-13T03:00:00Z")], "2026-10-07T12:00:00Z");
        let due = s.catch_up(at("2026-10-07T12:00:00Z"));
        assert_eq!(due.len(), 1, "one catch-up run, not three");
        assert_eq!((due[0].scheduled_for, due[0].catch_up), (at("2026-10-04T03:00:00Z"), true));
    }

    #[test]
    fn catch_up_does_nothing_when_the_latest_slot_was_already_handled() {
        let mut s = loaded(vec![job("j", "0 3 * * 0")], &[("j", "2026-10-04T03:00:00Z")], "2026-10-07T12:00:00Z");
        assert!(s.catch_up(at("2026-10-07T12:00:00Z")).is_empty());
    }

    #[test]
    fn a_brand_new_job_never_catches_up_for_a_slot_before_it_existed() {
        let mut no_history = loaded(vec![job("j", "0 3 * * *")], &[], "2026-10-07T12:00:00Z");
        assert!(no_history.catch_up(at("2026-10-07T12:00:00Z")).is_empty(), "no history and no created_at: baseline is the load time");
        let mut created = job("c", "0 3 * * *");
        created.created_at = Some(at("2026-10-07T09:00:00Z"));
        let mut s = loaded(vec![created], &[], "2026-10-07T12:00:00Z");
        assert!(s.catch_up(at("2026-10-07T12:00:00Z")).is_empty(), "created after this morning's 03:00 slot");
        let mut older = job("o", "0 3 * * *");
        older.created_at = Some(at("2026-10-05T00:00:00Z"));
        let mut s = loaded(vec![older], &[], "2026-10-07T12:00:00Z");
        let due = s.catch_up(at("2026-10-07T12:00:00Z"));
        assert_eq!(due[0].scheduled_for, at("2026-10-07T03:00:00Z"), "created before it, never ran: catch up");
    }

    #[test]
    fn catch_up_respects_the_per_job_toggle_and_disabled_jobs() {
        let mut no_catch_up = job("n", "0 3 * * *");
        no_catch_up.catch_up = false;
        no_catch_up.created_at = Some(at("2026-10-01T00:00:00Z"));
        let mut disabled = job("d", "0 3 * * *");
        disabled.enabled = false;
        disabled.created_at = Some(at("2026-10-01T00:00:00Z"));
        let mut s = loaded(vec![no_catch_up, disabled], &[], "2026-10-07T12:00:00Z");
        assert!(s.catch_up(at("2026-10-07T12:00:00Z")).is_empty());
    }

    #[test]
    fn catch_up_does_not_stop_the_normal_schedule() {
        let mut s = loaded(vec![job("j", "0 3 * * 0")], &[("j", "2026-09-13T03:00:00Z")], "2026-10-07T12:00:00Z");
        let _ = s.catch_up(at("2026-10-07T12:00:00Z"));
        assert_eq!(s.next_time("j"), Some(at("2026-10-11T03:00:00Z")));
        assert_eq!(s.tick(at("2026-10-11T03:00:00Z")).len(), 1);
    }

    #[test]
    fn the_later_of_two_snapshot_jobs_with_one_host_dataset_and_prefix_is_unsupported() {
        let mut s = Scheduler::new(Utc);
        let a = Job::test_snapshot("a", "tank/data", "hourly", 3);
        let b = Job::test_snapshot("b", "tank/data", "hourly", 5);
        let unsupported = s.reload(vec![a, b], &hosts(), &HashMap::new(), at("2026-10-06T00:00:00Z"));
        assert_eq!(unsupported.len(), 1);
        assert_eq!(unsupported[0].id, "b");
        assert!(unsupported[0].reason.contains("prune each other"), "{}", unsupported[0].reason);
        assert_eq!(s.len(), 1);
    }
}
