//! The scrub job: start `zpool scrub`, then watch `zpool status` until the
//! scrub finishes, recording each step in the history.

use crate::daemon::history::{Event, EventKind, History, Run, RunResult};
use crate::error::AppError;
use crate::exec::CommandRunner;
use crate::jobs::Job;
use crate::zfs::pool::{get_pool_vdevs, scrub_pool, ScanStatus};
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// How long a host may stay unreachable while a scrub is being watched before
/// the run is given up on.
const LOST_AFTER: chrono::Duration = chrono::Duration::minutes(10);
/// Polls that must show the old completion summary before we conclude the scrub never started.
const DID_NOT_START_POLLS: u32 = 3;

pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
    fn sleep(&self, d: Duration);
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// One look at the pool's scan state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    InProgress,
    Completed { summary: String, errors: bool },
    NeverScanned,
    Unreachable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    Done { ok: bool, message: String },
    DidNotStart,
    Lost(String),
}

/// Decides, poll by poll, whether a scrub we started has finished.
pub struct Watch {
    /// The scan summary before the scrub began, if the pool had one.
    before: Option<String>,
    unchanged: u32,
    unreachable_since: Option<DateTime<Utc>>,
}

impl Watch {
    pub fn new(before: Option<String>) -> Self {
        Watch { before, unchanged: 0, unreachable_since: None }
    }

    pub fn observe(&mut self, obs: Observation, now: DateTime<Utc>) -> Verdict {
        if let Observation::Unreachable(why) = &obs {
            let since = *self.unreachable_since.get_or_insert(now);
            return if now - since >= LOST_AFTER { Verdict::Lost(why.clone()) } else { Verdict::Keep };
        }
        self.unreachable_since = None;
        match obs {
            Observation::InProgress => {
                self.unchanged = 0;
                Verdict::Keep
            }
            // The completion line carries a timestamp, so a new scrub's differs from the old one's
            // even when it finished before our first poll.
            Observation::Completed { summary, errors } if self.before.as_ref() != Some(&summary) => classify_new_scan(summary, errors),
            Observation::Completed { .. } | Observation::NeverScanned => {
                self.unchanged += 1;
                if self.unchanged >= DID_NOT_START_POLLS { Verdict::DidNotStart } else { Verdict::Keep }
            }
            Observation::Unreachable(_) => Verdict::Keep,
        }
    }
}

/// What a scan line that differs from the one before the scrub means. Only a
/// completed scrub (`scrub repaired ...`) is a result; a cancelled scrub or a
/// resilver that took its place is a failure, and a paused scrub is unfinished.
fn classify_new_scan(summary: String, errors: bool) -> Verdict {
    if summary.starts_with("scrub repaired") {
        Verdict::Done { ok: !errors, message: summary }
    } else if summary.starts_with("scrub paused") {
        Verdict::Keep
    } else if summary.starts_with("scrub canceled") {
        Verdict::Done { ok: false, message: summary }
    } else {
        Verdict::Done { ok: false, message: format!("the scrub was replaced by another scan and not completed: {summary}") }
    }
}

/// At most one job per `(host, pool)` at a time.
#[derive(Clone, Default)]
pub struct Locks(Arc<Mutex<HashSet<(String, String)>>>);

pub struct LockGuard {
    locks: Locks,
    key: (String, String),
}

impl Locks {
    pub fn try_acquire(&self, host: &str, pool: &str) -> Option<LockGuard> {
        let key = (host.to_string(), pool.to_string());
        let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        held.insert(key.clone()).then(|| LockGuard { locks: self.clone(), key })
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        self.locks.0.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.key);
    }
}

/// What a finished run reports to the caller (which decides about notifying).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub job_id: String,
    pub result: RunResult,
    pub message: String,
}

pub struct Context<'a> {
    pub history: &'a History,
    pub clock: &'a dyn Clock,
    pub locks: &'a Locks,
    pub poll: Duration,
    /// Set at shutdown: a watcher returns without recording, so a restart re-attaches.
    pub stop: &'a AtomicBool,
}

fn observe(runner: &dyn CommandRunner, pool: &str) -> Observation {
    match get_pool_vdevs(runner, pool) {
        Ok((_, Some(ScanStatus::InProgress { .. }))) => Observation::InProgress,
        Ok((_, Some(ScanStatus::Completed { summary, errors }))) => Observation::Completed { summary, errors },
        Ok((_, None)) => Observation::NeverScanned,
        Err(e) => Observation::Unreachable(e.to_string()),
    }
}

fn event(job: &Job, run_id: &str, kind: EventKind, at: DateTime<Utc>) -> Event {
    Event { job_id: job.id.clone(), run_id: run_id.to_string(), event: kind, at, scheduled_for: None, result: None, message: String::new(), catch_up: false, summary: None }
}

const NO_SLOT: Slot = Slot { scheduled_for: None, catch_up: false };

/// The scheduled slot a run is for, recorded on its closing event.
#[derive(Clone, Copy)]
struct Slot {
    scheduled_for: Option<DateTime<Utc>>,
    catch_up: bool,
}

/// Records the end of a run and returns what to report.
fn finish(ctx: &Context, job: &Job, run_id: &str, kind: EventKind, result: RunResult, message: String, slot: Slot) -> Outcome {
    let mut e = event(job, run_id, kind, ctx.clock.now());
    e.result = Some(result);
    e.message.clone_from(&message);
    e.scheduled_for = slot.scheduled_for;
    e.catch_up = slot.catch_up;
    let _ = ctx.history.append(&e);
    Outcome { job_id: job.id.clone(), result, message }
}

fn watch(ctx: &Context, runner: &dyn CommandRunner, job: &Job, pool: &str, run_id: &str, mut watch: Watch) -> Option<Outcome> {
    loop {
        ctx.clock.sleep(ctx.poll);
        if ctx.stop.load(Ordering::SeqCst) {
            return None;
        }
        match watch.observe(observe(runner, pool), ctx.clock.now()) {
            Verdict::Keep => {}
            Verdict::Done { ok, message } => {
                return Some(finish(ctx, job, run_id, EventKind::Finished, if ok { RunResult::Ok } else { RunResult::Failed }, message, NO_SLOT));
            }
            Verdict::DidNotStart => return Some(finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, "scrub did not start".to_string(), NO_SLOT)),
            Verdict::Lost(why) => return Some(finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, format!("lost: host unreachable for 10 minutes ({why})"), NO_SLOT)),
        }
    }
}

/// Runs one scheduled scrub from start to finish. `None` means the daemon is
/// shutting down mid-watch and nothing further was recorded.
pub fn run_scrub(ctx: &Context, runner: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Option<Outcome> {
    let pool = job.pool.clone().unwrap_or_default();
    let slot = Slot { scheduled_for: Some(scheduled_for), catch_up };
    let Some(_guard) = ctx.locks.try_acquire(&job.host, &pool) else {
        return Some(finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, "skipped: already running".to_string(), slot));
    };
    let before = match get_pool_vdevs(runner, &pool) {
        Err(e) => return Some(finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, e.to_string(), slot)),
        Ok((_, Some(ScanStatus::InProgress { .. }))) => {
            return Some(finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, "skipped: already scanning".to_string(), slot));
        }
        Ok((_, Some(ScanStatus::Completed { summary, .. }))) => Some(summary),
        Ok((_, None)) => None,
    };
    match scrub_pool(runner, &pool) {
        Ok(()) => {}
        // A scrub or resilver that began between our check and our command.
        Err(AppError::CommandFailed { stderr, .. }) if stderr.contains("currently scrubbing") || stderr.contains("currently resilvering") => {
            return Some(finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, "skipped: already scanning".to_string(), slot));
        }
        Err(e) => return Some(finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, e.to_string(), slot)),
    }
    let mut started = event(job, run_id, EventKind::Started, ctx.clock.now());
    started.scheduled_for = Some(scheduled_for);
    started.catch_up = catch_up;
    started.summary.clone_from(&before);
    let _ = ctx.history.append(&started);
    watch(ctx, runner, job, &pool, run_id, Watch::new(before))
}

/// Picks up a scrub this daemon (or a previous one) started and never saw finish.
pub fn resume_scrub(ctx: &Context, runner: &dyn CommandRunner, job: &Job, run: &Run) -> Option<Outcome> {
    let pool = job.pool.clone().unwrap_or_default();
    let _guard = ctx.locks.try_acquire(&job.host, &pool);
    watch(ctx, runner, job, &pool, &run.run_id, Watch::new(run.summary.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::history::runs;
    use crate::error::Result;
    use crate::exec::CommandOutput;
    use crate::jobs::KIND_SCRUB;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    const NEVER: &str = "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\ttank  ONLINE  0 0 0\n";
    fn status(scan: &str) -> String {
        format!("  pool: tank\n state: ONLINE\n  scan: {scan}\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\ttank  ONLINE  0 0 0\n")
    }
    fn running() -> String {
        status("scrub in progress since Tue Oct  6 03:00:01 2026\n\t10.0G / 20.0G scanned, 50.00% done, 00:10:00 to go")
    }
    fn done(errors: u32) -> String {
        status(&format!("scrub repaired 0B in 00:20:00 with {errors} errors on Tue Oct  6 03:20:01 2026"))
    }
    fn old_done() -> String {
        status("scrub repaired 0B in 00:20:00 with 0 errors on Sun Sep  6 03:20:01 2026")
    }

    /// Answers `zpool status` from a queue (the last answer repeats) and `zpool scrub` with a fixed result.
    struct Seq {
        statuses: Mutex<VecDeque<std::result::Result<String, String>>>,
        scrub: std::result::Result<(), String>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl Seq {
        fn new(statuses: Vec<std::result::Result<String, String>>, scrub: std::result::Result<(), String>) -> Self {
            Seq { statuses: Mutex::new(statuses.into()), scrub, calls: Mutex::new(Vec::new()) }
        }
    }

    impl CommandRunner for Seq {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(std::iter::once(cmd).chain(args.iter().copied()).map(str::to_string).collect());
            let fail = |stderr: &str| Ok(CommandOutput { stdout: String::new(), stderr: stderr.to_string(), status: 1 });
            let ok = |stdout: String| Ok(CommandOutput { stdout, stderr: String::new(), status: 0 });
            match args.first().copied() {
                Some("status") => {
                    let mut q = self.statuses.lock().unwrap();
                    let next = if q.len() > 1 { q.pop_front().unwrap() } else { q.front().cloned().unwrap() };
                    match next {
                        Ok(text) => ok(text),
                        Err(e) => fail(&e),
                    }
                }
                Some("scrub") => match &self.scrub {
                    Ok(()) => ok(String::new()),
                    Err(e) => fail(e),
                },
                _ => ok(String::new()),
            }
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _stdin: &str) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
    }

    /// A clock that only moves when something sleeps.
    struct FakeClock(Mutex<DateTime<Utc>>);

    impl FakeClock {
        fn new() -> Self {
            FakeClock(Mutex::new(DateTime::parse_from_rfc3339("2026-10-06T03:00:00Z").unwrap().with_timezone(&Utc)))
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
        fn sleep(&self, d: Duration) {
            *self.0.lock().unwrap() += chrono::Duration::from_std(d).unwrap();
        }
    }

    fn job() -> Job {
        Job { id: "scrub-tank".into(), host: "local".into(), kind: KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * *".into(), catch_up: true, enabled: true, created_at: None }
    }

    struct Rig {
        dir: tempfile::TempDir,
        clock: FakeClock,
        locks: Locks,
        stop: AtomicBool,
    }

    impl Rig {
        fn new() -> Self {
            Rig { dir: tempfile::tempdir().unwrap(), clock: FakeClock::new(), locks: Locks::default(), stop: AtomicBool::new(false) }
        }
        fn history(&self) -> History {
            History::new(self.dir.path().join("history.jsonl"))
        }
        fn run(&self, runner: &Seq) -> Option<Outcome> {
            let history = self.history();
            let ctx = Context { history: &history, clock: &self.clock, locks: &self.locks, poll: Duration::from_secs(30), stop: &self.stop };
            run_scrub(&ctx, runner, &job(), self.clock.now(), false, "run-1")
        }
    }

    fn t(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-06T03:00:00Z").unwrap().with_timezone(&Utc) + chrono::Duration::minutes(minutes)
    }

    #[test]
    fn watch_keeps_going_while_in_progress_and_finishes_on_a_new_completion() {
        let fresh = "scrub repaired 0B in 00:20:00 with 0 errors on Tue Oct  6 04:00:00 2026";
        let mut w = Watch::new(Some("scrub repaired 0B in 00:20:00 with 0 errors on Sun Sep  6 03:20:01 2026".into()));
        assert_eq!(w.observe(Observation::InProgress, t(0)), Verdict::Keep);
        let v = w.observe(Observation::Completed { summary: fresh.into(), errors: false }, t(1));
        assert_eq!(v, Verdict::Done { ok: true, message: fresh.into() });
        let with_errors = "scrub repaired 12K in 00:20:00 with 2 errors on Tue Oct  6 04:00:00 2026";
        let mut w = Watch::new(None);
        assert_eq!(w.observe(Observation::Completed { summary: with_errors.into(), errors: true }, t(0)), Verdict::Done { ok: false, message: with_errors.into() });
    }

    #[test]
    fn a_cancelled_scrub_is_a_failed_run_not_a_green_one() {
        let mut w = Watch::new(Some("scrub repaired 0B in 00:20:00 with 0 errors on Sun Sep  6 03:20:01 2026".into()));
        let v = w.observe(Observation::Completed { summary: "scrub canceled on Tue Oct  6 03:05:00 2026".into(), errors: false }, t(0));
        assert_eq!(v, Verdict::Done { ok: false, message: "scrub canceled on Tue Oct  6 03:05:00 2026".into() });
    }

    #[test]
    fn a_paused_scrub_is_still_unfinished() {
        let mut w = Watch::new(None);
        let paused = || Observation::Completed { summary: "scrub paused since Tue Oct  6 03:05:00 2026".into(), errors: false };
        for i in 0..5 {
            assert_eq!(w.observe(paused(), t(i)), Verdict::Keep, "a paused scrub neither finished nor failed to start");
        }
        let v = w.observe(Observation::Completed { summary: "scrub repaired 0B in 00:20:00 with 0 errors on Tue Oct  6 04:00:00 2026".into(), errors: false }, t(6));
        assert!(matches!(v, Verdict::Done { ok: true, .. }), "{v:?}");
    }

    #[test]
    fn a_resilver_that_replaced_the_scrub_is_a_failed_run_naming_it() {
        let mut w = Watch::new(Some("old".into()));
        let v = w.observe(Observation::Completed { summary: "resilvered 500M in 00:00:30 with 0 errors on Tue Oct  6 03:10:00 2026".into(), errors: false }, t(0));
        let Verdict::Done { ok, message } = v else { panic!("{v:?}") };
        assert!(!ok);
        assert!(message.contains("resilver") && message.contains("not completed"), "{message}");
    }

    #[test]
    fn a_scrub_that_repaired_data_with_errors_still_reports_them() {
        let mut w = Watch::new(None);
        let v = w.observe(Observation::Completed { summary: "scrub repaired 12K in 00:20:00 with 3 errors on Tue Oct  6 04:00:00 2026".into(), errors: true }, t(0));
        assert!(matches!(v, Verdict::Done { ok: false, .. }), "{v:?}");
    }

    #[test]
    fn watch_treats_the_old_completion_line_as_not_finished_until_three_polls_agree() {
        let mut w = Watch::new(Some("old".into()));
        let same = || Observation::Completed { summary: "old".into(), errors: false };
        assert_eq!(w.observe(same(), t(0)), Verdict::Keep);
        assert_eq!(w.observe(same(), t(1)), Verdict::Keep);
        assert_eq!(w.observe(same(), t(2)), Verdict::DidNotStart);
        let mut w = Watch::new(Some("old".into()));
        let _ = w.observe(same(), t(0));
        assert_eq!(w.observe(Observation::InProgress, t(1)), Verdict::Keep, "seeing it run resets the count");
        assert_eq!(w.observe(same(), t(2)), Verdict::Keep);
    }

    #[test]
    fn a_pool_that_never_gets_a_scan_line_is_a_scrub_that_did_not_start() {
        let mut w = Watch::new(None);
        for i in 0..2 {
            assert_eq!(w.observe(Observation::NeverScanned, t(i)), Verdict::Keep);
        }
        assert_eq!(w.observe(Observation::NeverScanned, t(2)), Verdict::DidNotStart);
    }

    #[test]
    fn an_unreachable_host_is_lost_only_after_ten_minutes_and_recovery_resets_the_clock() {
        let mut w = Watch::new(None);
        let down = || Observation::Unreachable("ssh: timeout".into());
        assert_eq!(w.observe(down(), t(0)), Verdict::Keep);
        assert_eq!(w.observe(down(), t(9)), Verdict::Keep);
        assert_eq!(w.observe(Observation::InProgress, t(9)), Verdict::Keep);
        assert_eq!(w.observe(down(), t(15)), Verdict::Keep, "the outage restarted at minute 15");
        assert_eq!(w.observe(down(), t(25)), Verdict::Lost("ssh: timeout".into()));
    }

    #[test]
    fn locks_allow_one_holder_per_pool_and_free_it_on_drop() {
        let locks = Locks::default();
        let a = locks.try_acquire("local", "tank").unwrap();
        assert!(locks.try_acquire("local", "tank").is_none());
        assert!(locks.try_acquire("local", "other").is_some(), "another pool is independent");
        assert!(locks.try_acquire("fbsd16", "tank").is_some(), "another host is independent");
        drop(a);
        assert!(locks.try_acquire("local", "tank").is_some());
    }

    #[test]
    fn a_scrub_that_runs_then_completes_cleanly_records_started_then_ok() {
        let rig = Rig::new();
        let runner = Seq::new(vec![Ok(old_done()), Ok(running()), Ok(running()), Ok(done(0))], Ok(()));
        let out = rig.run(&runner).unwrap();
        assert_eq!((out.result, out.job_id.as_str()), (RunResult::Ok, "scrub-tank"));
        let all = runs(&rig.history().read());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].result, Some(RunResult::Ok));
        assert!(all[0].message.contains("with 0 errors"));
        assert_eq!(all[0].summary.as_deref(), Some("scrub repaired 0B in 00:20:00 with 0 errors on Sun Sep  6 03:20:01 2026"));
        let calls = runner.calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == &["zpool", "scrub", "--", "tank"]), "{calls:?}");
        assert!(rig.locks.try_acquire("local", "tank").is_some(), "the lock is released afterwards");
    }

    #[test]
    fn a_scrub_that_finds_errors_is_a_failed_run_carrying_the_summary() {
        let rig = Rig::new();
        let runner = Seq::new(vec![Ok(NEVER.into()), Ok(running()), Ok(done(3))], Ok(()));
        let out = rig.run(&runner).unwrap();
        assert_eq!(out.result, RunResult::Failed);
        assert!(out.message.contains("with 3 errors"), "{}", out.message);
    }

    #[test]
    fn a_scrub_that_finished_before_the_first_poll_is_still_recognised() {
        let rig = Rig::new();
        let runner = Seq::new(vec![Ok(old_done()), Ok(done(0))], Ok(()));
        assert_eq!(rig.run(&runner).unwrap().result, RunResult::Ok);
    }

    #[test]
    fn a_pool_already_scanning_is_skipped_without_starting_anything() {
        let rig = Rig::new();
        let runner = Seq::new(vec![Ok(running())], Ok(()));
        let out = rig.run(&runner).unwrap();
        assert_eq!(out.result, RunResult::Skipped);
        assert!(out.message.contains("already scanning"));
        assert!(!runner.calls.lock().unwrap().iter().any(|c| c.get(1).is_some_and(|a| a == "scrub")), "no scrub command was sent");
    }

    #[test]
    fn a_scrub_that_races_another_is_skipped_not_failed() {
        let rig = Rig::new();
        let runner = Seq::new(vec![Ok(old_done())], Err("cannot scrub tank: currently scrubbing; use 'zpool scrub -s' to cancel".into()));
        assert_eq!(rig.run(&runner).unwrap().result, RunResult::Skipped);
    }

    #[test]
    fn a_failing_start_or_unreadable_pool_is_a_failed_run_with_the_error() {
        let rig = Rig::new();
        let out = rig.run(&Seq::new(vec![Err("cannot open 'tank': no such pool".into())], Ok(()))).unwrap();
        assert_eq!(out.result, RunResult::Failed);
        assert!(out.message.contains("no such pool"), "{}", out.message);
        let out = rig.run(&Seq::new(vec![Ok(old_done())], Err("permission denied".into()))).unwrap();
        assert_eq!(out.result, RunResult::Failed);
    }

    #[test]
    fn a_held_lock_skips_the_run() {
        let rig = Rig::new();
        let _held = rig.locks.try_acquire("local", "tank").unwrap();
        let out = rig.run(&Seq::new(vec![Ok(old_done())], Ok(()))).unwrap();
        assert_eq!(out.result, RunResult::Skipped);
        assert!(out.message.contains("already running"));
    }

    #[test]
    fn the_run_gives_up_as_lost_after_ten_minutes_of_an_unreachable_host() {
        let rig = Rig::new();
        let runner = Seq::new(vec![Ok(old_done()), Err("ssh: connect timed out".into())], Ok(()));
        let out = rig.run(&runner).unwrap();
        assert_eq!(out.result, RunResult::Failed);
        assert!(out.message.starts_with("lost:"), "{}", out.message);
    }

    #[test]
    fn shutdown_mid_watch_records_nothing_more_so_a_restart_can_resume() {
        let rig = Rig::new();
        rig.stop.store(true, Ordering::SeqCst);
        let runner = Seq::new(vec![Ok(old_done()), Ok(running())], Ok(()));
        assert!(rig.run(&runner).is_none());
        let all = runs(&rig.history().read());
        assert_eq!((all.len(), all[0].result), (1, None), "started, never finished: still running");
    }

    #[test]
    fn resume_finishes_a_run_using_the_summary_recorded_at_its_start() {
        let rig = Rig::new();
        let history = rig.history();
        let ctx = Context { history: &history, clock: &rig.clock, locks: &rig.locks, poll: Duration::from_secs(30), stop: &rig.stop };
        let run = Run {
            job_id: "scrub-tank".into(),
            run_id: "run-1".into(),
            started_at: t(0),
            scheduled_for: Some(t(0)),
            finished_at: None,
            result: None,
            message: String::new(),
            catch_up: false,
            summary: Some("scrub repaired 0B in 00:20:00 with 0 errors on Sun Sep  6 03:20:01 2026".into()),
        };
        // Still showing the old completion: not finished. Then a new one: finished.
        let runner = Seq::new(vec![Ok(old_done()), Ok(done(0))], Ok(()));
        let out = resume_scrub(&ctx, &runner, &job(), &run).unwrap();
        assert_eq!(out.result, RunResult::Ok);
        let all = runs(&history.read());
        assert_eq!((all.len(), all[0].run_id.as_str(), all[0].result), (1, "run-1", Some(RunResult::Ok)));
    }
}
