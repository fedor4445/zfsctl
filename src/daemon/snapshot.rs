//! A scheduled snapshot run: take the snapshot, then prune the job's own old ones.
//!
//! Unlike a scrub it finishes in seconds, so there is nothing to watch or resume.

use super::history::{EventKind, RunResult};
use super::scrub::{event, finish, Context, Outcome, Slot};
use crate::error::AppError;
use crate::exec::CommandRunner;
use crate::jobs::{effective_prefix, Job};
use crate::zfs::snapshot::{create_snapshot_with, destroy_snapshot_with, list_snapshots_of};
use chrono::{DateTime, Utc};

/// `<prefix>-<UTC YYYYMMDDTHHMMSSZ>`: no colons, sorts chronologically as a string, and does not
/// depend on the hub's local time zone.
pub fn snapshot_name(prefix: &str, at: DateTime<Utc>) -> String {
    format!("{prefix}-{}", at.format("%Y%m%dT%H%M%SZ"))
}

/// Whether `name` is exactly `<prefix>-<YYYYMMDDTHHMMSSZ>`. Retention destroys nothing else, so
/// a manual snapshot or another job's can never be pruned by name collision.
pub fn is_own(prefix: &str, name: &str) -> bool {
    let Some(stamp) = name.strip_prefix(prefix).and_then(|rest| rest.strip_prefix('-')) else { return false };
    let Some((date, rest)) = stamp.split_at_checked(8) else { return false };
    let Some(time) = rest.strip_prefix('T').and_then(|t| t.strip_suffix('Z')) else { return false };
    let digits = |part: &str, len: usize| part.len() == len && part.bytes().all(|b| b.is_ascii_digit());
    digits(date, 8) && digits(time, 6)
}

/// The lock a snapshot run holds. It includes the prefix, so an hourly and a daily job on one
/// dataset that fall due in the same tick both run; `@` cannot start a pool name, so the key never
/// collides with a scrub's `(host, pool)` lock.
pub fn lock_key(dataset: &str, prefix: &str) -> String {
    format!("@{dataset}@{prefix}")
}

pub fn run_snapshot(ctx: &Context, runner: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome {
    let dataset = job.dataset.clone().unwrap_or_default();
    let prefix = effective_prefix(job).to_string();
    let keep = job.keep.unwrap_or(1).max(1) as usize;
    let slot = Slot { scheduled_for: Some(scheduled_for), catch_up };
    let Some(_guard) = ctx.locks.try_acquire(&job.host, &lock_key(&dataset, &prefix)) else {
        return finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, "skipped: already running".to_string(), slot);
    };
    let name = snapshot_name(&prefix, ctx.clock.now());
    match create_snapshot_with(runner, &dataset, &name, job.recursive) {
        Ok(()) => {}
        Err(AppError::CommandFailed { stderr, .. }) if stderr.contains("already exists") => {
            return finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, format!("skipped: {name} already exists"), slot);
        }
        Err(e) => return finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, e.to_string(), slot),
    }
    let mut started = event(job, run_id, EventKind::Started, ctx.clock.now());
    started.scheduled_for = Some(scheduled_for);
    started.catch_up = catch_up;
    let _ = ctx.history.append(&started);

    let listed = match list_snapshots_of(runner, &dataset, job.recursive) {
        Ok(listed) => listed,
        Err(e) => return finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, format!("created {name}; could not list snapshots: {e}"), slot),
    };
    // Newest first. Only names on the job's own dataset count (a recursive listing also returns
    // descendants' snapshots, which may belong to another job), and the snapshot just taken is never
    // a candidate, even if a clock that stepped backwards makes another name sort after it.
    let mut own: Vec<String> = listed.into_iter().filter(|s| s.dataset == dataset && s.name != name && is_own(&prefix, &s.name)).map(|s| s.name).collect();
    own.sort_unstable_by(|a, b| b.cmp(a));
    own.dedup();
    let (mut pruned, mut failures) = (0_usize, Vec::new());
    // Oldest first, so a failure part-way leaves the newest of the excess in place.
    for old in own.iter().skip(keep.saturating_sub(1)).rev() {
        match destroy_snapshot_with(runner, &dataset, old, job.recursive) {
            Ok(()) => pruned += 1,
            Err(e) => failures.push(format!("{old}: {e}")),
        }
    }
    if failures.is_empty() {
        finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, format!("created {name}; pruned {pruned}"), slot)
    } else {
        finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, format!("created {name}; pruned {pruned}; could not destroy {}", failures.join("; ")), slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::history::{runs, History};
    use crate::daemon::scrub::{Clock, Locks};
    use crate::error::Result;
    use crate::exec::CommandOutput;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;
    use std::time::Duration;

    struct FixedClock(DateTime<Utc>);
    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
        fn sleep(&self, _: Duration) {}
    }

    /// Answers `zfs list` with a fixed table, fails `snapshot` and `destroy` for configured
    /// targets, and records every call.
    struct Zfs {
        listing: String,
        snapshot_error: Option<String>,
        destroy_fails: Vec<String>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl Zfs {
        fn new(existing: &[&str]) -> Self {
            let listing = existing.iter().map(|n| format!("{n}\t1\n")).collect::<Vec<_>>().concat();
            Zfs { listing, snapshot_error: None, destroy_fails: Vec::new(), calls: Mutex::new(Vec::new()) }
        }
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
        fn destroyed(&self) -> Vec<String> {
            self.calls().into_iter().filter(|c| c[0] == "destroy").map(|c| c.last().unwrap().clone()).collect()
        }
    }

    impl CommandRunner for Zfs {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(args.iter().map(|a| (*a).to_string()).collect());
            assert_eq!(cmd, "zfs");
            let fail = |stderr: &str| Ok(CommandOutput { stdout: String::new(), stderr: stderr.to_string(), status: 1 });
            let ok = |stdout: String| Ok(CommandOutput { stdout, stderr: String::new(), status: 0 });
            match args[0] {
                "snapshot" => self.snapshot_error.as_deref().map_or_else(|| ok(String::new()), fail),
                "list" => ok(self.listing.clone()),
                "destroy" => {
                    let target = args.last().unwrap();
                    if self.destroy_fails.iter().any(|t| t == target) { fail("snapshot has holds") } else { ok(String::new()) }
                }
                other => panic!("unexpected zfs {other}"),
            }
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _: &str) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-06T19:30:00Z").unwrap().with_timezone(&Utc)
    }

    /// Runs one job against `zfs` and returns the outcome and the history of the run.
    fn run(zfs: &Zfs, job: &Job) -> (Outcome, Vec<crate::daemon::history::Run>) {
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(dir.path().join("history.jsonl"));
        let (clock, locks, stop) = (FixedClock(now()), Locks::default(), AtomicBool::new(false));
        let ctx = Context { history: &history, clock: &clock, locks: &locks, poll: Duration::from_secs(1), stop: &stop };
        let outcome = run_snapshot(&ctx, zfs, job, now(), false, "run-1");
        (outcome, runs(&history.read()))
    }

    const NEW: &str = "hourly-20261006T193000Z";

    #[test]
    fn names_are_the_prefix_and_a_utc_timestamp() {
        assert_eq!(snapshot_name("hourly", now()), NEW);
    }

    #[test]
    fn only_exact_prefix_timestamp_names_are_the_jobs_own() {
        assert!(is_own("hourly", "hourly-20261006T193000Z"));
        for not_own in ["hourly-keep-forever", "hourly-20261006T1930Z", "hourly-x-20261006T193000Z", "daily-20261006T193000Z", "hourly20261006T193000Z", "hourly-20261006T193000Zjunk", "xhourly-20261006T193000Z", "hourly-2026100aT193000Z"] {
            assert!(!is_own("hourly", not_own), "{not_own}");
        }
    }

    #[test]
    fn a_run_takes_the_snapshot_and_prunes_the_oldest_beyond_keep() {
        let zfs = Zfs::new(&[
            "tank/data@hourly-20261006T163000Z",
            "tank/data@hourly-20261006T173000Z",
            "tank/data@hourly-20261006T183000Z",
            &format!("tank/data@{NEW}"),
        ]);
        let (outcome, history) = run(&zfs, &Job::test_snapshot("s", "tank/data", "hourly", 2));
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert_eq!(zfs.destroyed(), vec!["tank/data@hourly-20261006T163000Z", "tank/data@hourly-20261006T173000Z"]);
        assert!(outcome.message.contains(NEW) && outcome.message.contains("pruned 2"), "{}", outcome.message);
        assert_eq!(zfs.calls()[0], vec!["snapshot", "--", &format!("tank/data@{NEW}")]);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].result, Some(RunResult::Ok));
    }

    #[test]
    fn snapshots_that_only_look_like_the_jobs_own_survive_a_prune() {
        let zfs = Zfs::new(&[
            "tank/data@hourly-keep-forever",
            "tank/data@hourly-20261006T1930Z",
            "tank/data@daily-20250101T000000Z",
            "tank/data@manual",
            &format!("tank/data@{NEW}"),
        ]);
        let (outcome, _) = run(&zfs, &Job::test_snapshot("s", "tank/data", "hourly", 1));
        assert_eq!(outcome.result, RunResult::Ok);
        assert!(zfs.destroyed().is_empty(), "{:?}", zfs.destroyed());
    }

    const FIVE: [&str; 5] = [
        "tank/data@hourly-20261006T100000Z",
        "tank/data@hourly-20261006T110000Z",
        "tank/data@hourly-20261006T120000Z",
        "tank/data@hourly-20261006T130000Z",
        "tank/data@hourly-20261006T140000Z",
    ];

    /// What `zfs list` shows after the run's own snapshot was taken: the five old ones plus `NEW`.
    fn with_new() -> Vec<String> {
        FIVE.iter().map(|s| (*s).to_string()).chain(std::iter::once(format!("tank/data@{NEW}"))).collect()
    }

    #[test]
    fn raising_keep_prunes_nothing_and_lowering_it_prunes_the_excess_oldest_first() {
        let listing = with_new();
        let refs: Vec<&str> = listing.iter().map(String::as_str).collect();
        let roomy = Zfs::new(&refs);
        run(&roomy, &Job::test_snapshot("s", "tank/data", "hourly", 10));
        assert!(roomy.destroyed().is_empty());
        // keep = 3 counts the new snapshot: it and the two newest old ones stay.
        let tight = Zfs::new(&refs);
        run(&tight, &Job::test_snapshot("s", "tank/data", "hourly", 3));
        assert_eq!(tight.destroyed(), vec!["tank/data@hourly-20261006T100000Z", "tank/data@hourly-20261006T110000Z", "tank/data@hourly-20261006T120000Z"]);
    }

    #[test]
    fn a_recursive_job_snapshots_and_destroys_with_dash_r_and_prunes_each_name_once() {
        let zfs = Zfs::new(&[
            "tank/data@hourly-20261006T173000Z",
            "tank/data/child@hourly-20261006T173000Z",
            &format!("tank/data@{NEW}"),
            &format!("tank/data/child@{NEW}"),
        ]);
        let mut job = Job::test_snapshot("s", "tank/data", "hourly", 1);
        job.recursive = true;
        let (outcome, _) = run(&zfs, &job);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert_eq!(zfs.calls()[0][..2], ["snapshot", "-r"]);
        let destroys: Vec<Vec<String>> = zfs.calls().into_iter().filter(|c| c[0] == "destroy").collect();
        assert_eq!(destroys, vec![vec!["destroy", "-r", "--", "tank/data@hourly-20261006T173000Z"]]);
    }

    #[test]
    fn a_failed_snapshot_is_failed_and_prunes_nothing() {
        let mut zfs = Zfs::new(&["tank/data@hourly-20250101T000000Z"]);
        zfs.snapshot_error = Some("dataset does not exist".to_string());
        let (outcome, _) = run(&zfs, &Job::test_snapshot("s", "tank/data", "hourly", 1));
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("does not exist"), "{}", outcome.message);
        assert!(zfs.calls().iter().all(|c| c[0] == "snapshot"), "no list or destroy after a failed snapshot: {:?}", zfs.calls());
    }

    #[test]
    fn a_snapshot_that_already_exists_is_skipped_not_failed() {
        let mut zfs = Zfs::new(&[]);
        zfs.snapshot_error = Some("cannot create snapshot 'tank/data@x': dataset already exists".to_string());
        let (outcome, _) = run(&zfs, &Job::test_snapshot("s", "tank/data", "hourly", 1));
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(zfs.destroyed().is_empty());
    }

    #[test]
    fn a_destroy_that_fails_is_reported_but_the_rest_are_still_pruned() {
        let mut zfs = Zfs::new(&[
            "tank/data@hourly-20261006T163000Z",
            "tank/data@hourly-20261006T173000Z",
            &format!("tank/data@{NEW}"),
        ]);
        zfs.destroy_fails = vec!["tank/data@hourly-20261006T163000Z".to_string()];
        let (outcome, _) = run(&zfs, &Job::test_snapshot("s", "tank/data", "hourly", 1));
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains(NEW) && outcome.message.contains("hourly-20261006T163000Z") && outcome.message.contains("holds"), "{}", outcome.message);
        assert_eq!(zfs.destroyed().len(), 2, "the second old snapshot was still attempted");
    }

    #[test]
    fn a_job_already_running_on_the_dataset_is_skipped() {
        let zfs = Zfs::new(&[]);
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(dir.path().join("history.jsonl"));
        let (clock, locks, stop) = (FixedClock(now()), Locks::default(), AtomicBool::new(false));
        let _held = locks.try_acquire("local", &lock_key("tank/data", "hourly")).unwrap();
        let ctx = Context { history: &history, clock: &clock, locks: &locks, poll: Duration::from_secs(1), stop: &stop };
        let outcome = run_snapshot(&ctx, &zfs, &Job::test_snapshot("s", "tank/data", "hourly", 1), now(), false, "r");
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(zfs.calls().is_empty());
    }

    #[test]
    fn the_lock_is_per_dataset_and_prefix_so_hourly_and_daily_tiers_do_not_block_each_other() {
        assert_ne!(lock_key("tank/data", "hourly"), lock_key("tank/data", "daily"));
        assert_ne!(lock_key("tank/data", "hourly"), lock_key("tank/other", "hourly"));
        assert_eq!(lock_key("tank/data", "hourly"), lock_key("tank/data", "hourly"));
        assert!(lock_key("tank", "x").starts_with('@'), "never collides with a scrub's pool-name key");
    }

    #[test]
    fn the_snapshot_just_created_is_never_pruned_even_if_an_own_name_sorts_after_it() {
        let zfs = Zfs::new(&["tank/data@hourly-20991231T000000Z", &format!("tank/data@{NEW}")]);
        let (outcome, _) = run(&zfs, &Job::test_snapshot("s", "tank/data", "hourly", 1));
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(!zfs.destroyed().iter().any(|d| d.ends_with(NEW)), "{:?}", zfs.destroyed());
    }

    #[test]
    fn a_recursive_job_counts_and_prunes_only_names_that_exist_on_its_own_dataset() {
        let zfs = Zfs::new(&[
            "tank@hourly-20261006T100000Z",
            "tank@hourly-20261006T110000Z",
            // Another job's snapshots, present only on a child, newer than the parent's own.
            "tank/data@hourly-20261006T180000Z",
            "tank/data@hourly-20261006T190000Z",
            &format!("tank@{NEW}"),
        ]);
        let mut job = Job::test_snapshot("s", "tank", "hourly", 2);
        job.recursive = true;
        let (outcome, _) = run(&zfs, &job);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert_eq!(zfs.destroyed(), vec!["tank@hourly-20261006T100000Z"], "child-only names are neither counted nor destroyed");
    }
}
