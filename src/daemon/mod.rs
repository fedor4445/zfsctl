//! `zfsctl daemon`: runs scheduled jobs on the hub, unattended.

pub mod cron;
pub mod engine;
pub mod heartbeat;
pub mod history;
pub mod notify;
pub mod scheduler;
pub mod scrub;

use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use crate::hosts::HostInventory;
use chrono::Utc;
use engine::{Engine, Paths, Work};
use history::{Event, EventKind, History, RunResult};
use scrub::{Clock, Context, Locks, Outcome, SystemClock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How often a running scrub is polled for completion.
const POLL: Duration = Duration::from_secs(30);

fn log(message: &str) {
    eprintln!("{} {message}", Utc::now().to_rfc3339());
}

/// What every job thread shares.
struct Shared {
    history: History,
    hosts: Vec<(String, Arc<dyn CommandRunner>)>,
    locks: Locks,
    clock: Arc<dyn Clock + Send + Sync>,
    poll: Duration,
    notify_command: Option<String>,
    stop: Arc<AtomicBool>,
}

impl Shared {
    fn runner(&self, host: &str) -> Option<Arc<dyn CommandRunner>> {
        self.hosts.iter().find(|(name, _)| name == host).map(|(_, r)| Arc::clone(r))
    }

    /// Runs the notify command for a failed run; a failing hook is only logged.
    fn notify(&self, job: &crate::jobs::Job, outcome: &Outcome) {
        if outcome.result != RunResult::Failed {
            return;
        }
        let Some(command) = &self.notify_command else { return };
        let notice = notify::Notice { job: job.id.clone(), host: job.host.clone(), kind: job.kind.clone(), result: "failed".to_string(), message: outcome.message.clone() };
        match notify::run(command, &notice, notify::TIMEOUT) {
            Ok(()) => log(&format!("notified about {}", job.id)),
            Err(e) => log(&format!("notify command for {} {e}", job.id)),
        }
    }

    /// Runs one piece of work to completion on the calling thread.
    fn execute(&self, work: Work) {
        let ctx = Context { history: &self.history, clock: self.clock.as_ref(), locks: &self.locks, poll: self.poll, stop: &self.stop };
        match work {
            Work::Run(due) => {
                let Some(runner) = self.runner(&due.job.host) else { return };
                let run_id = format!("{}-{}", due.job.id, self.clock.now().timestamp_millis());
                log(&format!("running {} (slot {}{})", due.job.id, due.scheduled_for.to_rfc3339(), if due.catch_up { ", catch-up" } else { "" }));
                if let Some(outcome) = scrub::run_scrub(&ctx, runner.as_ref(), &due.job, due.scheduled_for, due.catch_up, &run_id) {
                    log(&format!("{} {:?}: {}", outcome.job_id, outcome.result, outcome.message));
                    self.notify(&due.job, &outcome);
                }
            }
            Work::Resume { job, run, then } => {
                let Some(runner) = self.runner(&job.host) else { return };
                log(&format!("re-attaching to {} (run {})", job.id, run.run_id));
                if let Some(outcome) = scrub::resume_scrub(&ctx, runner.as_ref(), &job, &run) {
                    log(&format!("{} {:?}: {}", outcome.job_id, outcome.result, outcome.message));
                    self.notify(&job, &outcome);
                    // The catch-up waits for the resumed run to end, so the two never race for the pool.
                    if let Some(due) = then {
                        self.execute(Work::Run(*due));
                    }
                }
            }
            Work::Orphan(run) => {
                log(&format!("run {} of {} has no runnable job any more", run.run_id, run.job_id));
                let _ = self.history.append(&Event {
                    job_id: run.job_id,
                    run_id: run.run_id,
                    event: EventKind::Finished,
                    at: self.clock.now(),
                    scheduled_for: None,
                    result: Some(RunResult::Failed),
                    message: "lost: the job was removed or can no longer run".to_string(),
                    catch_up: false,
                    summary: None,
                });
            }
        }
    }
}

fn dispatch(shared: &Arc<Shared>, work: Vec<Work>) {
    for item in work {
        let shared = Arc::clone(shared);
        if let Err(e) = std::thread::Builder::new().name("job".to_string()).spawn(move || shared.execute(item)) {
            log(&format!("could not start a job thread: {e}"));
        }
    }
}

/// Runs the scheduler until `stop` is set. Refuses to start if another daemon is alive.
pub fn run(inventory: &HostInventory, notify_command: Option<String>, paths: Paths, stop: &Arc<AtomicBool>) -> Result<()> {
    paths.ensure_dir().map_err(|e| AppError::Config(format!("creating {}: {e}", paths.dir.display())))?;
    let now = Utc::now();
    let heartbeat_path = paths.heartbeat();
    if let Some(pid) = heartbeat::live_other_instance(heartbeat::read(&heartbeat_path).as_ref(), now, std::process::id(), heartbeat::process_alive) {
        return Err(AppError::Config(format!("another zfsctl daemon is already running (pid {pid})")));
    }
    let hosts: Vec<(String, Arc<dyn CommandRunner>)> = inventory.hosts.iter().map(|h| (h.name.clone(), Arc::clone(&h.runner))).collect();
    let names: Vec<String> = hosts.iter().map(|(n, _)| n.clone()).collect();
    let shared = Arc::new(Shared { history: History::new(paths.history()), hosts, locks: Locks::default(), clock: Arc::new(SystemClock), poll: POLL, notify_command, stop: Arc::clone(stop) });
    let mut engine = Engine::new(paths, chrono::Local, names, now);
    let started_at = now;

    let work = engine.start(Utc::now());
    log(&format!("zfsctl daemon started (pid {})", std::process::id()));
    report(&engine);
    dispatch(&shared, work);

    let mut last_problem: Option<String> = None;
    while !stop.load(Ordering::SeqCst) {
        let now = Utc::now();
        if let Err(e) = heartbeat::write(&heartbeat_path, &heartbeat::Heartbeat { pid: std::process::id(), started_at, last_tick: now }) {
            let problem = format!("cannot write the heartbeat: {e}");
            if last_problem.as_deref() != Some(problem.as_str()) {
                log(&problem);
            }
            last_problem = Some(problem);
        }
        let before = engine.load_error.clone();
        let work = engine.step(now);
        if engine.load_error != before {
            report(&engine);
        }
        dispatch(&shared, work);
        std::thread::sleep(Duration::from_secs(1));
    }
    let _ = std::fs::remove_file(&heartbeat_path);
    log("zfsctl daemon stopping");
    Ok(())
}

fn report(engine: &Engine<chrono::Local>) {
    if let Some(e) = &engine.load_error {
        log(&format!("jobs file problem, keeping the previous schedule: {e}"));
    }
    for u in &engine.unsupported {
        log(&format!("job {} will not run: {}", u.id, u.reason));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;
    use crate::jobs::{Job, KIND_SCRUB};
    use scheduler::Due;
    use std::sync::Mutex;

    struct FixedClock(Mutex<chrono::DateTime<Utc>>);

    impl Clock for FixedClock {
        fn now(&self) -> chrono::DateTime<Utc> {
            *self.0.lock().unwrap()
        }
        fn sleep(&self, d: Duration) {
            *self.0.lock().unwrap() += chrono::Duration::from_std(d).unwrap();
        }
    }

    fn job() -> Job {
        Job { id: "scrub-tank".into(), host: "local".into(), kind: KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * *".into(), catch_up: true, enabled: true, created_at: None }
    }

    fn shared(dir: &tempfile::TempDir, runner: FakeRunner, notify: Option<String>) -> Shared {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-07T03:00:00Z").unwrap().with_timezone(&Utc);
        Shared {
            history: History::new(dir.path().join("history.jsonl")),
            hosts: vec![("local".to_string(), Arc::new(runner) as Arc<dyn CommandRunner>)],
            locks: Locks::default(),
            clock: Arc::new(FixedClock(Mutex::new(now))),
            poll: Duration::from_secs(30),
            notify_command: notify,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    fn due() -> Due {
        Due { job: job(), scheduled_for: chrono::DateTime::parse_from_rfc3339("2026-10-07T03:00:00Z").unwrap().with_timezone(&Utc), catch_up: false }
    }

    #[test]
    fn a_failed_run_is_recorded_and_runs_the_notify_command_with_the_job_in_its_environment() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("notified");
        let cmd = format!("printf '%s|%s' \"$ZFSCTL_JOB\" \"$ZFSCTL_RESULT\" > {}", out.display());
        let shared = shared(&dir, FakeRunner::failing("cannot open 'tank': no such pool", 1), Some(cmd));
        shared.execute(Work::Run(due()));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Failed)));
        assert!(runs[0].message.contains("no such pool"));
        assert_eq!(std::fs::read_to_string(out).unwrap(), "scrub-tank|failed");
    }

    #[test]
    fn a_failing_notify_command_does_not_disturb_the_recorded_run() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::failing("no such pool", 1), Some("exit 9".to_string()));
        shared.execute(Work::Run(due()));
        assert_eq!(history::runs(&shared.history.read())[0].result, Some(RunResult::Failed));
    }

    #[test]
    fn a_skipped_run_does_not_notify() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("notified");
        let shared = shared(&dir, FakeRunner::new(""), Some(format!("touch {}", out.display())));
        let _held = shared.locks.try_acquire("local", "tank").unwrap();
        shared.execute(Work::Run(due()));
        assert_eq!(history::runs(&shared.history.read())[0].result, Some(RunResult::Skipped));
        assert!(!out.exists());
    }

    #[test]
    fn an_orphaned_run_is_closed_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let started = Event { job_id: "gone".into(), run_id: "r1".into(), event: EventKind::Started, at: shared.clock.now(), scheduled_for: None, result: None, message: String::new(), catch_up: false, summary: None };
        shared.history.append(&started).unwrap();
        let run = history::runs(&shared.history.read()).remove(0);
        shared.execute(Work::Orphan(run));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Failed)));
        assert!(runs[0].message.starts_with("lost:"));
    }

    #[test]
    fn a_resumed_run_finishes_before_its_queued_catch_up_starts() {
        let dir = tempfile::tempdir().unwrap();
        // Every `zpool` call fails, so the resumed run is closed as failed and the catch-up fails at its pre-check.
        let shared = shared(&dir, FakeRunner::failing("cannot open 'tank': no such pool", 1), None);
        let started = Event { job_id: "scrub-tank".into(), run_id: "old-run".into(), event: EventKind::Started, at: shared.clock.now(), scheduled_for: None, result: None, message: String::new(), catch_up: false, summary: Some("old".into()) };
        shared.history.append(&started).unwrap();
        let run = history::runs(&shared.history.read()).remove(0);
        let mut catch_up = due();
        catch_up.catch_up = true;
        shared.execute(Work::Resume { job: Box::new(job()), run: Box::new(run), then: Some(Box::new(catch_up)) });
        let order: Vec<String> = shared.history.read().into_iter().filter(|e| e.event != EventKind::Started).map(|e| e.run_id).collect();
        assert_eq!(order.len(), 2, "{order:?}");
        assert_eq!(order[0], "old-run", "the resumed run is closed first");
        assert_ne!(order[1], "old-run");
        let runs = history::runs(&shared.history.read());
        assert!(runs.iter().any(|r| r.catch_up && r.result == Some(RunResult::Failed)), "the catch-up then ran: {runs:?}");
    }

    #[test]
    fn work_for_an_unknown_host_is_ignored_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let mut d = due();
        d.job.host = "gone".into();
        shared.execute(Work::Run(d));
        assert!(shared.history.read().is_empty());
    }

    #[test]
    fn a_second_daemon_refuses_to_start_while_the_first_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        // A heartbeat from this very process, as if another daemon were running here.
        let other = heartbeat::Heartbeat { pid: 1, started_at: Utc::now(), last_tick: Utc::now() };
        heartbeat::write(&paths.heartbeat(), &other).unwrap();
        let inventory = HostInventory { hosts: Vec::new() };
        let err = run(&inventory, None, paths, &Arc::new(AtomicBool::new(true))).unwrap_err();
        assert!(err.to_string().contains("another zfsctl daemon is already running (pid 1)"), "{err}");
    }

    #[test]
    fn the_daemon_writes_a_heartbeat_while_it_runs_and_removes_it_on_a_clean_stop() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let hb = paths.heartbeat();
        let stop = Arc::new(AtomicBool::new(false));
        let inventory = HostInventory { hosts: Vec::new() };
        let stopper = Arc::clone(&stop);
        let watch_path = hb.clone();
        let watcher = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut seen = None;
            while std::time::Instant::now() < deadline && seen.is_none() {
                seen = heartbeat::read(&watch_path);
                std::thread::sleep(Duration::from_millis(50));
            }
            stopper.store(true, Ordering::SeqCst);
            seen
        });
        run(&inventory, None, paths, &stop).unwrap();
        let seen = watcher.join().unwrap().expect("a heartbeat appeared while the daemon ran");
        assert_eq!(seen.pid, std::process::id());
        assert!(!hb.exists(), "removed on a clean stop, so the TUI shows 'not running' at once");
    }
}
