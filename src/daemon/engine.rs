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
    Resume { job: Job, run: Run },
    /// A running run whose job no longer exists or can no longer run.
    Orphan(Run),
}

pub struct Engine<Tz: TimeZone> {
    paths: Paths,
    hosts: Vec<String>,
    scheduler: Scheduler<Tz>,
    jobs_mtime: Option<SystemTime>,
    last_prune: DateTime<Utc>,
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
        Engine { paths, hosts, scheduler: Scheduler::new(tz), jobs_mtime: None, last_prune: now, unsupported: Vec::new(), load_error: None }
    }

    /// Reads `jobs.toml` and swaps the job set in. A bad file keeps the job
    /// set already in place (it never empties the schedule). Returns the jobs
    /// that were read so the caller can look runs up by id.
    fn reload(&mut self, now: DateTime<Utc>, last_scheduled: &std::collections::HashMap<String, DateTime<Utc>>) -> Vec<Job> {
        self.jobs_mtime = mtime(&self.paths);
        match jobs::load(&self.paths.jobs()) {
            Ok(list) => {
                self.load_error = None;
                self.unsupported = self.scheduler.reload(list.clone(), &self.hosts, last_scheduled, now);
                list
            }
            Err(e) => {
                self.load_error = Some(e.to_string());
                Vec::new()
            }
        }
    }

    fn last_scheduled(&self) -> std::collections::HashMap<String, DateTime<Utc>> {
        history::last_scheduled(&history::runs(&History::new(self.paths.history()).read()))
    }

    /// Everything to do when the daemon starts: re-attach to unfinished runs
    /// and catch up on missed slots.
    pub fn start(&mut self, now: DateTime<Utc>) -> Vec<Work> {
        let _ = History::new(self.paths.history()).prune(KEEP_RUNS);
        let last = self.last_scheduled();
        let loaded = self.reload(now, &last);
        let mut work = Vec::new();
        let unfinished: Vec<Run> = history::runs(&History::new(self.paths.history()).read()).into_iter().filter(|r| r.result.is_none()).collect();
        for run in unfinished {
            match loaded.iter().find(|j| j.id == run.job_id && jobs::resolve(j, &self.hosts).is_ok()) {
                Some(job) => work.push(Work::Resume { job: job.clone(), run }),
                None => work.push(Work::Orphan(run)),
            }
        }
        work.extend(self.scheduler.catch_up(now).into_iter().map(Work::Run));
        work
    }

    /// One tick: pick up an edited `jobs.toml`, fire what is due, prune daily.
    pub fn step(&mut self, now: DateTime<Utc>) -> Vec<Work> {
        let current = mtime(&self.paths);
        if current != self.jobs_mtime {
            let last = self.last_scheduled();
            self.reload(now, &last);
        }
        if now - self.last_prune >= PRUNE_EVERY {
            self.last_prune = now;
            let _ = History::new(self.paths.history()).prune(KEEP_RUNS);
        }
        self.scheduler.tick(now).into_iter().map(Work::Run).collect()
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
        Job { id: id.into(), host: "local".into(), kind: KIND_SCRUB.into(), pool: Some("tank".into()), schedule: schedule.into(), catch_up: true, enabled: true, created_at: Some(at("2026-09-01T00:00:00Z")) }
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
        assert!(work.iter().any(|w| matches!(w, Work::Resume { job, run } if job.id == "j" && run.run_id == "r1")), "{work:?}");
        assert!(work.iter().any(|w| matches!(w, Work::Orphan(run) if run.run_id == "r2")), "{work:?}");
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
}
