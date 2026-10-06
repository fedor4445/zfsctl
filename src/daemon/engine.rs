//! The daemon's brain, without threads or sleeping: given the clock it says
//! what work to start. The runtime loop in `mod.rs` executes that work.

use crate::daemon::history::{self, History, Run};
use crate::daemon::scheduler::{Due, Scheduler, Unsupported};
use crate::jobs::{self, Job};
use chrono::{DateTime, Duration, TimeZone, Utc};
use std::path::PathBuf;
use std::time::SystemTime;

/// Runs kept per job when the history is pruned.
pub const KEEP_RUNS: usize = 500;
const PRUNE_EVERY: Duration = Duration::hours(24);

/// Where the daemon keeps its state.
#[derive(Debug, Clone)]
pub struct Paths {
    pub dir: PathBuf,
}

pub const DEFAULT_STATE_DIR: &str = "/var/db/zfsctl";

impl Paths {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Paths { dir: dir.into() }
    }
    pub fn jobs(&self) -> PathBuf {
        self.dir.join("jobs.toml")
    }
    pub fn history(&self) -> PathBuf {
        self.dir.join("history.jsonl")
    }
    pub fn heartbeat(&self) -> PathBuf {
        self.dir.join("daemon.json")
    }
    /// Live progress of running transfers, one file per run.
    pub fn progress_dir(&self) -> PathBuf {
        self.dir.join("progress")
    }
    /// Cancel requests dropped by the TUI, one file per job.
    pub fn cancel_dir(&self) -> PathBuf {
        self.dir.join("cancel")
    }

    /// Creates the state directory, readable by root only.
    pub fn ensure_dir(&self) -> std::io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir)?;
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))
    }
}

/// One piece of work for the runtime to start on its own thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Work {
    /// A scheduled (or catch-up) run.
    Run(Due),
    /// A scrub started earlier and never seen to finish: keep watching it.
    /// `then` is a catch-up for the same job, run only after the resumed run
    /// ends: started on its own it would race the resumed run for the pool.
    Resume { job: Box<Job>, run: Box<Run>, then: Option<Box<Due>> },
    /// A running run whose job no longer exists or can no longer run.
    Orphan(Run),
}

pub struct Engine<Tz: TimeZone> {
    paths: Paths,
    hosts: Vec<String>,
    scheduler: Scheduler<Tz>,
    jobs_mtime: Option<SystemTime>,
    last_prune: DateTime<Utc>,
    /// Runs found unfinished at startup that have not been handed out yet. A
    /// `jobs.toml` that cannot be read must not condemn them as lost.
    pending: Vec<Run>,
    /// Whether the startup catch-up has been done (it waits for a readable `jobs.toml`).
    caught_up: bool,
    /// Jobs that cannot run, with the reason, for the log and the TUI.
    pub unsupported: Vec<Unsupported>,
    /// The last problem reading `jobs.toml`, if the file is currently bad.
    pub load_error: Option<String>,
}

fn mtime(paths: &Paths) -> Option<SystemTime> {
    std::fs::metadata(paths.jobs()).and_then(|m| m.modified()).ok()
}

impl<Tz: TimeZone> Engine<Tz> {
    pub fn new(paths: Paths, tz: Tz, hosts: Vec<String>, now: DateTime<Utc>) -> Self {
        Engine { paths, hosts, scheduler: Scheduler::new(tz), jobs_mtime: None, last_prune: now, pending: Vec::new(), caught_up: false, unsupported: Vec::new(), load_error: None }
    }

    /// Reads `jobs.toml` and swaps the job set in. A bad file keeps the job
    /// set already in place (it never empties the schedule). Returns the jobs
    /// that were read, or `None` when the file could not be read.
    fn reload(&mut self, now: DateTime<Utc>, last_scheduled: &std::collections::HashMap<String, DateTime<Utc>>) -> Option<Vec<Job>> {
        self.jobs_mtime = mtime(&self.paths);
        match jobs::load(&self.paths.jobs()) {
            Ok(list) => {
                self.load_error = None;
                self.unsupported = self.scheduler.reload(list.clone(), &self.hosts, last_scheduled, now);
                Some(list)
            }
            Err(e) => {
                self.load_error = Some(e.to_string());
                None
            }
        }
    }

    fn last_scheduled(&self) -> std::collections::HashMap<String, DateTime<Utc>> {
        history::last_scheduled(&history::runs(&History::new(self.paths.history()).read()))
    }

    /// Hands out the runs that were unfinished at startup, now that the jobs are
    /// known: a run whose job still exists is resumed (with its catch-up queued
    /// behind it), one whose job is gone is closed as lost.
    fn hand_out_pending(&mut self, jobs: &[Job], catch_ups: &mut Vec<Due>) -> Vec<Work> {
        let mut work = Vec::new();
        for run in std::mem::take(&mut self.pending) {
            match jobs.iter().find(|j| j.id == run.job_id && j.kind == jobs::KIND_SCRUB && jobs::resolve(j, &self.hosts).is_ok()) {
                Some(job) => {
                    let then = catch_ups.iter().position(|d| d.job.id == job.id).map(|i| catch_ups.remove(i));
                    work.push(Work::Resume { job: Box::new(job.clone()), run: Box::new(run), then: then.map(Box::new) });
                }
                None => work.push(Work::Orphan(run)),
            }
        }
        work
    }

    /// Everything to do after a successful read of `jobs.toml`: hand out
    /// unfinished runs, and on the first successful read catch up missed slots.
    fn after_load(&mut self, jobs: &[Job], now: DateTime<Utc>) -> Vec<Work> {
        let mut catch_ups = if self.caught_up { Vec::new() } else { self.scheduler.catch_up(now) };
        self.caught_up = true;
        let mut work = self.hand_out_pending(jobs, &mut catch_ups);
        work.extend(catch_ups.into_iter().map(Work::Run));
        work
    }

    /// Everything to do when the daemon starts: re-attach to unfinished runs
    /// and catch up on missed slots.
    pub fn start(&mut self, now: DateTime<Utc>) -> Vec<Work> {
        let _ = History::new(self.paths.history()).prune(KEEP_RUNS);
        self.pending = history::runs(&History::new(self.paths.history()).read()).into_iter().filter(|r| r.result.is_none()).collect();
        let last = self.last_scheduled();
        match self.reload(now, &last) {
            Some(jobs) => self.after_load(&jobs, now),
            None => Vec::new(),
        }
    }

    /// One tick: pick up an edited `jobs.toml`, fire what is due, prune daily.
    pub fn step(&mut self, now: DateTime<Utc>) -> Vec<Work> {
        let mut work = Vec::new();
        let current = mtime(&self.paths);
        if current != self.jobs_mtime {
            let last = self.last_scheduled();
            if let Some(jobs) = self.reload(now, &last) {
                work = self.after_load(&jobs, now);
            }
        }
        if now - self.last_prune >= PRUNE_EVERY {
            self.last_prune = now;
            let _ = History::new(self.paths.history()).prune(KEEP_RUNS);
        }
        work.extend(self.scheduler.tick(now).into_iter().map(Work::Run));
        work
    }

    /// Replaces the known host names (after `config.toml` changed) and makes the
    /// next tick re-read the jobs, so a job that was waiting for a host can start.
    pub fn set_hosts(&mut self, hosts: Vec<String>) {
        self.hosts = hosts;
        self.jobs_mtime = None;
    }

    #[cfg(test)]
    pub fn next_time(&self, id: &str) -> Option<DateTime<Utc>> {
        self.scheduler.next_time(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::history::{Event, EventKind, RunResult};
    use crate::jobs::KIND_SCRUB;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn job(id: &str, schedule: &str) -> Job {
        Job { id: id.into(), host: "local".into(), kind: KIND_SCRUB.into(), pool: Some("tank".into()), schedule: schedule.into(), catch_up: true, enabled: true, created_at: Some(at("2026-09-01T00:00:00Z")), dataset: None, recursive: false, prefix: None, keep: None, dest_host: None, dest_dataset: None, dest_dir: None, force: false }
    }

    fn engine(dir: &tempfile::TempDir, now: &str) -> Engine<Utc> {
        Engine::new(Paths::new(dir.path()), Utc, vec!["local".to_string()], at(now))
    }

    fn started(job_id: &str, run_id: &str, when: &str) -> Event {
        Event { job_id: job_id.into(), run_id: run_id.into(), event: EventKind::Started, at: at(when), scheduled_for: Some(at(when)), result: None, message: String::new(), catch_up: false, summary: Some("old".into()) }
    }

    #[test]
    fn paths_live_under_one_private_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path().join("zfsctl"));
        p.ensure_dir().unwrap();
        p.ensure_dir().unwrap();
        assert_eq!(std::fs::metadata(&p.dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!((p.jobs().file_name().unwrap(), p.history().file_name().unwrap(), p.heartbeat().file_name().unwrap()), ("jobs.toml".as_ref(), "history.jsonl".as_ref(), "daemon.json".as_ref()));
    }

    #[test]
    fn start_catches_up_a_missed_slot_once_and_a_second_start_does_not_repeat_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        jobs::save(&paths.jobs(), &[job("j", "0 3 * * 0")]).unwrap();
        let work = engine(&dir, "2026-10-07T12:00:00Z").start(at("2026-10-07T12:00:00Z"));
        let [Work::Run(due)] = work.as_slice() else { panic!("{work:?}") };
        assert_eq!((due.scheduled_for, due.catch_up), (at("2026-10-04T03:00:00Z"), true));
        // The runtime records the catch-up run; the next start sees that slot as handled.
        let h = History::new(paths.history());
        let mut done = started("j", "r1", "2026-10-04T03:00:00Z");
        done.event = EventKind::Finished;
        done.result = Some(RunResult::Ok);
        h.append(&done).unwrap();
        assert!(engine(&dir, "2026-10-07T12:05:00Z").start(at("2026-10-07T12:05:00Z")).is_empty());
    }

    #[test]
    fn start_resumes_an_unfinished_run_and_orphans_one_whose_job_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        jobs::save(&paths.jobs(), &[job("j", "0 3 * * *")]).unwrap();
        let h = History::new(paths.history());
        h.append(&started("j", "r1", "2026-10-07T03:00:00Z")).unwrap();
        h.append(&started("deleted", "r2", "2026-10-07T03:00:00Z")).unwrap();
        let work = engine(&dir, "2026-10-07T04:00:00Z").start(at("2026-10-07T04:00:00Z"));
        assert!(work.iter().any(|w| matches!(w, Work::Resume { job, run, .. } if job.id == "j" && run.run_id == "r1")), "{work:?}");
        assert!(work.iter().any(|w| matches!(w, Work::Orphan(run) if run.run_id == "r2")), "{work:?}");
    }

    #[test]
    fn a_bad_jobs_file_at_startup_leaves_unfinished_runs_alone_until_the_file_is_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::write(paths.jobs(), "this is [[[ not toml").unwrap();
        let h = History::new(paths.history());
        h.append(&started("j", "r1", "2026-10-07T03:00:00Z")).unwrap();
        h.append(&started("gone", "r2", "2026-10-07T03:00:00Z")).unwrap();
        let mut e = engine(&dir, "2026-10-07T04:00:00Z");
        assert!(e.start(at("2026-10-07T04:00:00Z")).is_empty(), "an unreadable file is not proof that the jobs are gone");
        assert!(history::runs(&h.read()).iter().all(|r| r.result.is_none()), "no healthy run was recorded as lost");
        jobs::save(&paths.jobs(), &[job("j", "0 3 * * *")]).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(paths.jobs()).unwrap().set_modified(later).unwrap();
        let work = e.step(at("2026-10-07T04:00:05Z"));
        assert!(work.iter().any(|w| matches!(w, Work::Resume { job, run, .. } if job.id == "j" && run.run_id == "r1")), "{work:?}");
        assert!(work.iter().any(|w| matches!(w, Work::Orphan(run) if run.run_id == "r2")), "{work:?}");
        assert!(e.step(at("2026-10-07T04:00:06Z")).is_empty(), "each unfinished run is handed out once");
    }

    #[test]
    fn a_resumed_run_and_a_catch_up_for_the_same_job_are_one_sequential_piece_of_work() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        jobs::save(&paths.jobs(), &[job("j", "0 3 * * 0")]).unwrap();
        History::new(paths.history()).append(&started("j", "r1", "2026-09-27T03:00:00Z")).unwrap();
        let work = engine(&dir, "2026-10-07T12:00:00Z").start(at("2026-10-07T12:00:00Z"));
        let [Work::Resume { job, run, then }] = work.as_slice() else { panic!("expected a single Resume, got {work:?}") };
        assert_eq!((job.id.as_str(), run.run_id.as_str()), ("j", "r1"));
        let due = then.as_ref().expect("the missed slot is caught up after the resumed run ends");
        assert_eq!((due.scheduled_for, due.catch_up), (at("2026-10-04T03:00:00Z"), true));
    }

    #[test]
    fn the_catch_up_after_a_bad_start_happens_once_the_file_is_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::write(paths.jobs(), "[[[").unwrap();
        let mut e = engine(&dir, "2026-10-07T12:00:00Z");
        assert!(e.start(at("2026-10-07T12:00:00Z")).is_empty());
        let mut j = job("j", "0 3 * * 0");
        j.created_at = Some(at("2026-09-01T00:00:00Z"));
        jobs::save(&paths.jobs(), &[j]).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(paths.jobs()).unwrap().set_modified(later).unwrap();
        let work = e.step(at("2026-10-07T12:00:05Z"));
        let [Work::Run(due)] = work.as_slice() else { panic!("{work:?}") };
        assert!(due.catch_up);
        assert!(e.step(at("2026-10-07T12:00:06Z")).is_empty(), "the catch-up is offered once");
    }

    #[test]
    fn step_fires_due_jobs_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = job("j", "0 3 * * *");
        j.created_at = Some(at("2026-10-07T01:00:00Z"));
        jobs::save(&Paths::new(dir.path()).jobs(), &[j]).unwrap();
        let mut e = engine(&dir, "2026-10-07T02:00:00Z");
        assert!(e.start(at("2026-10-07T02:00:00Z")).is_empty());
        assert!(e.step(at("2026-10-07T02:59:59Z")).is_empty());
        assert_eq!(e.step(at("2026-10-07T03:00:00Z")).len(), 1);
        assert!(e.step(at("2026-10-07T03:00:01Z")).is_empty());
        assert_eq!(e.next_time("j"), Some(at("2026-10-08T03:00:00Z")));
    }

    #[test]
    fn step_picks_up_an_edited_jobs_file_and_a_bad_edit_keeps_the_old_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let mut e = engine(&dir, "2026-10-07T02:00:00Z");
        assert!(e.start(at("2026-10-07T02:00:00Z")).is_empty());
        jobs::save(&paths.jobs(), &[job("j", "0 3 * * *")]).unwrap();
        assert!(e.step(at("2026-10-07T02:10:00Z")).is_empty());
        assert_eq!(e.next_time("j"), Some(at("2026-10-07T03:00:00Z")), "the new job is scheduled");
        std::fs::write(paths.jobs(), "this is [[[ not toml").unwrap();
        // A different mtime is needed to notice the change; bump it explicitly.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(paths.jobs()).unwrap().set_modified(later).unwrap();
        assert_eq!(e.step(at("2026-10-07T03:00:00Z")).len(), 1, "the previous job set still fires");
        assert!(e.load_error.as_deref().is_some_and(|m| m.contains("parsing")), "{:?}", e.load_error);
    }

    #[test]
    fn a_job_for_a_host_added_later_becomes_runnable_once_the_host_list_is_updated() {
        let dir = tempfile::tempdir().unwrap();
        let mut later = job("j", "0 3 * * *");
        later.host = "later".into();
        later.created_at = Some(at("2026-10-07T01:00:00Z"));
        jobs::save(&Paths::new(dir.path()).jobs(), &[later]).unwrap();
        let mut e = engine(&dir, "2026-10-07T02:00:00Z");
        assert!(e.start(at("2026-10-07T02:00:00Z")).is_empty());
        assert_eq!(e.unsupported.len(), 1, "the host is not known yet");
        e.set_hosts(vec!["local".to_string(), "later".to_string()]);
        assert!(e.step(at("2026-10-07T02:00:05Z")).is_empty());
        assert!(e.unsupported.is_empty(), "{:?}", e.unsupported);
        assert_eq!(e.next_time("j"), Some(at("2026-10-07T03:00:00Z")));
    }

    #[test]
    fn unsupported_jobs_are_reported_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut bad = job("bad", "0 3 * * *");
        bad.host = "gone".into();
        jobs::save(&Paths::new(dir.path()).jobs(), &[bad]).unwrap();
        let mut e = engine(&dir, "2026-10-07T02:00:00Z");
        assert!(e.start(at("2026-10-07T04:00:00Z")).is_empty());
        assert_eq!(e.unsupported.len(), 1);
        assert!(e.unsupported[0].reason.contains("'gone'"));
    }

    #[test]
    fn step_prunes_the_history_once_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let h = History::new(paths.history());
        for i in 0..(KEEP_RUNS + 5) {
            let mut e = started("j", &format!("r{i:04}"), "2026-10-01T03:00:00Z");
            e.event = EventKind::Finished;
            e.result = Some(RunResult::Ok);
            h.append(&e).unwrap();
        }
        let mut e = engine(&dir, "2026-10-07T00:00:00Z");
        e.last_prune = at("2026-10-07T00:00:00Z");
        let _ = e.step(at("2026-10-07T12:00:00Z"));
        assert_eq!(history::runs(&h.read()).len(), KEEP_RUNS + 5, "not yet a day");
        let _ = e.step(at("2026-10-08T00:00:01Z"));
        assert_eq!(history::runs(&h.read()).len(), KEEP_RUNS);
    }

    #[test]
    fn an_unfinished_snapshot_run_is_closed_as_lost_not_resumed_as_a_scrub() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        jobs::save(&paths.jobs(), &[Job::test_snapshot("snap", "tank/data", "hourly", 3)]).unwrap();
        History::new(paths.history()).append(&started("snap", "r1", "2026-10-07T03:00:00Z")).unwrap();
        let work = engine(&dir, "2026-10-07T04:00:00Z").start(at("2026-10-07T04:00:00Z"));
        assert!(work.iter().any(|w| matches!(w, Work::Orphan(run) if run.run_id == "r1")), "{work:?}");
        assert!(!work.iter().any(|w| matches!(w, Work::Resume { .. })), "{work:?}");
    }

    #[test]
    fn an_unfinished_replicate_run_is_closed_as_lost_not_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        jobs::save(&paths.jobs(), &[Job::test_replicate("repl", "tank/data", "hourly", "local", "backup/data")]).unwrap();
        History::new(paths.history()).append(&started("repl", "r1", "2026-10-07T03:00:00Z")).unwrap();
        let work = engine(&dir, "2026-10-07T04:00:00Z").start(at("2026-10-07T04:00:00Z"));
        assert!(work.iter().any(|w| matches!(w, Work::Orphan(run) if run.run_id == "r1")), "{work:?}");
        assert!(!work.iter().any(|w| matches!(w, Work::Resume { .. })), "{work:?}");
    }
}
