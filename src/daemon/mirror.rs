//! A scheduled cloud mirror: clone the snapshot to read from (the same source choices as a backup job), check
//! it holds files, `rclone sync` it to `<remote>/current` with everything it replaces or deletes moved to
//! `<remote>/old/<run time>/`, and destroy the clone on every exit path. Nothing is ever deleted outright:
//! zfsctl runs no rclone command but `sync` (with `--backup-dir`) and read-only ones.

use super::backup::{choose_snapshot, clone_name_for, make_clone, mountpoint, remove_stale_clone, target_dir, SourceEnd};
use super::history::{EventKind, RunResult};
use super::progress::{self, ProgressWriter};
use super::replicate::Env;
use super::scrub::{event, finish, Outcome, Slot};
use super::transfer::run_lines_merged;
use crate::exec::CommandRunner;
use crate::jobs::Job;
use crate::rclone::{self, Line};
use chrono::{DateTime, Utc};

/// The clone a mirror job reads from.
const CLONE_PREFIX: &str = "zit-mirror";

enum End {
    Done(u64),
    Cancelled(u64),
    Failed(String),
}

fn rclone_query(runner: &dyn CommandRunner, remote: &str, args: &[String]) -> std::result::Result<String, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match runner.run("rclone", &refs) {
        Ok(out) if out.success() => Ok(out.stdout),
        Ok(out) => Err(rclone::failure_text(None, &out.stderr, remote)),
        Err(e) => Err(rclone::redact(&e.to_string(), remote)),
    }
}

/// Every part of the job's `path` must be a real folder inside the clone. rclone runs as root and follows a
/// symlink given as its source, so `docs -> /` would otherwise upload the host's own files. A listing does not
/// show symlinks, so a part missing from its parent's listing is a symlink (or not a folder) and ends the run.
fn path_stays_inside(runner: &dyn CommandRunner, remote: &str, mount: &str, path: &str) -> std::result::Result<(), String> {
    let mut parent = mount.to_string();
    for part in path.split('/').filter(|p| !p.is_empty() && *p != ".") {
        let listing = rclone_query(runner, remote, &rclone::lsf_dirs_args(&parent))?;
        if !listing.lines().any(|l| l.strip_suffix('/') == Some(part)) {
            return Err(format!("the path '{path}' is not a plain folder inside the snapshot ('{part}' is a symlink, a file or missing); nothing was changed"));
        }
        parent = format!("{parent}/{part}");
    }
    Ok(())
}

/// `<remote>/old/<stamp>` must not exist yet: a second run with the same stamp (two hosts in one second, the
/// clock stepping back) would overwrite the first run's old versions. Missing is the normal answer.
fn stamp_is_free(runner: &dyn CommandRunner, remote: &str, stamp: &str) -> std::result::Result<(), String> {
    let args = rclone::lsf_args(&rclone::join(&rclone::join(remote, "old"), stamp));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match runner.run("rclone", &refs) {
        Ok(out) if out.success() && out.stdout.trim().is_empty() => Ok(()),
        Ok(out) if out.success() => Err(format!("old/{stamp} already exists in the bucket; nothing was changed, the next run uses a new name")),
        Ok(out) if out.status == 3 => Ok(()),
        Ok(out) => Err(rclone::failure_text(None, &out.stderr, remote)),
        Err(e) => Err(rclone::redact(&e.to_string(), remote)),
    }
}

fn sync(env: &Env, runner: &dyn CommandRunner, job: &Job, remote: &str, mount: &str, target: &str, run_id: &str) -> End {
    if let Some(path) = job.path.as_deref() {
        if let Err(e) = path_stays_inside(runner, remote, mount, path) {
            return End::Failed(e);
        }
    }
    // A tree with no files (an empty dataset, only symlinks, a wrong `path`) must never look like "everything was
    // deleted": refuse before the sync can move anything.
    match rclone_query(runner, remote, &rclone::size_args(target)).and_then(|json| rclone::parse_size_count(&json)) {
        Ok(0) => return End::Failed(format!("the source holds no files to copy ({target} is empty); nothing was changed")),
        Ok(_) => {}
        Err(e) => return End::Failed(e),
    }
    let clock = env.ctx.clock;
    let stamp = rclone::run_stamp(clock.now());
    if let Err(e) = stamp_is_free(runner, remote, &stamp) {
        return End::Failed(e);
    }
    let args = rclone::sync_args(target, remote, &stamp, job.bwlimit.as_deref());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cmd = match runner.command("rclone", &refs) {
        Ok(c) => c,
        Err(e) => return End::Failed(rclone::redact(&e.to_string(), remote)),
    };
    let mut writer = ProgressWriter::new(env.progress_dir.to_path_buf(), run_id.to_string(), job.id.clone(), None, clock.now());
    let (mut fatal, mut bytes) = (None, 0_u64);
    let result = run_lines_merged(cmd, env.cancel, |line| match rclone::parse_line(line) {
        Line::Stats { bytes: done, total } => {
            if total > 0 {
                writer.set_total(total);
            }
            bytes = done;
            writer.update(done, clock.now());
        }
        Line::Error(m) => {
            fatal.get_or_insert(m);
        }
        Line::Other => {}
    });
    progress::remove(env.progress_dir, run_id);
    let lines = match result {
        Ok(l) => l,
        Err(e) => return End::Failed(format!("could not run rclone: {}", rclone::redact(&e.to_string(), remote))),
    };
    if lines.cancelled {
        return End::Cancelled(bytes);
    }
    match lines.code {
        Some(0) => End::Done(bytes),
        code => End::Failed(format!("rclone failed (exit {}): {}", code.map_or_else(|| "signal".to_string(), |c| c.to_string()), rclone::failure_text(fatal.as_deref(), &lines.stderr, remote))),
    }
}

pub fn run_mirror(env: &Env, runner: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome {
    let ctx = env.ctx;
    let dataset = job.dataset.clone().unwrap_or_default();
    let remote = job.remote.clone().unwrap_or_default();
    let slot = Slot { scheduled_for: Some(scheduled_for), catch_up };
    let failed = |msg: String| finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, msg, slot);
    let skipped = |msg: String| finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, msg, slot);

    // `#` cannot appear in a pool name, so the key cannot collide with another kind's lock.
    let Some(_guard) = ctx.locks.try_acquire(&job.host, &format!("#mir@{}", job.id)) else { return skipped("skipped: already running".to_string()) };
    if let Err(msg) = super::guard::check_source(runner, &dataset, super::guard::Need::NotEncrypted) {
        return failed(msg);
    }
    let clone = clone_name_for(CLONE_PREFIX, &dataset, &job.id);
    let mount = mountpoint(&job.id);
    if let Err(e) = remove_stale_clone(runner, ctx.clock, &clone) {
        return failed(e);
    }
    let (snapshot, prune_note) = match choose_snapshot(runner, ctx.clock, job, &dataset) {
        Ok(chosen) => chosen,
        Err(SourceEnd::Skip(msg)) => return skipped(msg),
        Err(SourceEnd::Fail(msg)) => return failed(msg),
    };
    let target = target_dir(job, &mount);
    if let Err(e) = make_clone(runner, &format!("{dataset}@{snapshot}"), &clone, &mount) {
        // The clone can exist without being mounted: do not leave it pinning the snapshot.
        let _ = remove_stale_clone(runner, ctx.clock, &clone);
        return failed(e);
    }

    let mut started = event(job, run_id, EventKind::Started, ctx.clock.now());
    started.scheduled_for = Some(scheduled_for);
    started.catch_up = catch_up;
    let _ = ctx.history.append(&started);

    let end = sync(env, runner, job, &remote, &mount, &target, run_id);
    // Always, whatever happened: the clone pins the snapshot and holds a mountpoint.
    let leftover = match super::backup::destroy_clone(runner, ctx.clock, &clone) {
        Ok(()) => String::new(),
        Err(e) => format!("; could not remove the clone {clone} ({e}), the next run retries"),
    };
    match end {
        End::Cancelled(bytes) => finish(ctx, job, run_id, EventKind::Finished, RunResult::Cancelled, format!("cancelled after {}; the next run starts again{leftover}", progress::human_bytes(bytes)), slot),
        End::Failed(msg) => failed(format!("{msg}{leftover}")),
        End::Done(bytes) => {
            let name = remote.split(':').next().unwrap_or("");
            let message = format!("mirrored {snapshot} to the {name} remote, {} sent{prune_note}", progress::human_bytes(bytes));
            if leftover.is_empty() {
                finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, message, slot)
            } else {
                failed(format!("{message}{leftover}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::history::{runs, History};
    use crate::daemon::scrub::{Clock, Context, Locks};
    use crate::error::Result;
    use crate::exec::CommandOutput;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    struct FixedClock(DateTime<Utc>);
    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
        fn sleep(&self, _: Duration) {}
    }

    const B: &str = "hourly-20261008T110000Z";
    const STATS: &str = r#"echo '{"level":"notice","msg":"x","stats":{"bytes":500,"totalBytes":1000}}' >&2"#;

    /// One host: answers zfs and rclone queries from fixed strings, remembers whether the clone exists, hands out a
    /// scripted shell for `rclone sync`, and records every call.
    struct Host {
        size_json: String,
        sync_script: String,
        clone_fails: bool,
        clone_made_but_unmounted: bool,
        rclone_missing: bool,
        /// Folders (full paths) that exist for real; any other name inside a listed parent is a symlink.
        real_dirs: Vec<String>,
        /// What `rclone lsf <remote>/old/<stamp>` answers: None = no such folder (exit 3).
        old_stamp_listing: Option<String>,
        /// The source's `keystatus` when it is encrypted.
        keystatus: Option<&'static str>,
        clone_exists: Mutex<bool>,
        calls: Mutex<Vec<String>>,
        sync_args: Mutex<Vec<Vec<String>>>,
    }

    impl Host {
        fn new() -> Self {
            Host { size_json: "{\"count\":3,\"bytes\":30,\"sizeless\":0}".into(), sync_script: format!("{STATS}; exit 0"), clone_fails: false, clone_made_but_unmounted: false, rclone_missing: false, real_dirs: vec![], old_stamp_listing: None, keystatus: None, clone_exists: Mutex::new(false), calls: Mutex::new(vec![]), sync_args: Mutex::new(vec![]) }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn clone_exists(&self) -> bool {
            *self.clone_exists.lock().unwrap()
        }
    }

    impl CommandRunner for Host {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(format!("{cmd} {}", args.join(" ")));
            let ok = |s: &str| Ok(CommandOutput { stdout: s.to_string(), stderr: String::new(), status: 0 });
            let fail = |s: &str| Ok(CommandOutput { stdout: String::new(), stderr: s.to_string(), status: 1 });
            match (cmd, args.first().copied()) {
                ("zfs", Some("get")) => ok(&crate::daemon::guard::fake_answer("tank/data", self.keystatus)),
                ("zfs", Some("list")) if args.contains(&"snapshot") => ok(&format!("tank/data@{B}\t2\t20\n")),
                ("zfs", Some("list")) => {
                    if self.clone_exists() { ok("tank/zit-mirror-job\n") } else { fail("dataset does not exist") }
                }
                ("zfs", Some("clone")) => {
                    if self.clone_made_but_unmounted {
                        *self.clone_exists.lock().unwrap() = true;
                        return fail("cannot mount: failed to create mountpoint");
                    }
                    if self.clone_fails {
                        return fail("cannot mount: failed to create mountpoint");
                    }
                    *self.clone_exists.lock().unwrap() = true;
                    ok("")
                }
                ("zfs", Some("snapshot" | "destroy")) if args.last().is_some_and(|a| a.contains('@')) => ok(""),
                ("zfs", Some("destroy")) => {
                    *self.clone_exists.lock().unwrap() = false;
                    ok("")
                }
                ("rclone", Some("size")) => {
                    if self.rclone_missing { fail("sudo: a password is required") } else { ok(&self.size_json) }
                }
                ("rclone", Some("lsf")) if args.contains(&"--dirs-only") => {
                    let parent = args.last().unwrap();
                    let names: String = self.real_dirs.iter().filter_map(|d| d.strip_prefix(&format!("{parent}/"))).filter(|n| !n.contains('/')).map(|n| format!("{n}/\n")).collect();
                    ok(&names)
                }
                ("rclone", Some("lsf")) => match &self.old_stamp_listing {
                    Some(listing) => ok(listing),
                    None => Ok(CommandOutput { stdout: String::new(), stderr: "directory not found".into(), status: 3 }),
                },
                other => panic!("unexpected call {other:?}"),
            }
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _: &str) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn command(&self, cmd: &str, args: &[&str]) -> Result<Command> {
            assert_eq!(cmd, "rclone");
            self.calls.lock().unwrap().push(format!("rclone {}", args.join(" ")));
            self.sync_args.lock().unwrap().push(args.iter().map(|a| (*a).to_string()).collect());
            let mut c = Command::new("sh");
            c.args(["-c", &self.sync_script]);
            Ok(c)
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-08T23:30:00Z").unwrap().with_timezone(&Utc)
    }

    struct Rig {
        state: tempfile::TempDir,
        locks: Locks,
        cancel: AtomicBool,
    }

    impl Rig {
        fn new() -> Self {
            Rig { state: tempfile::tempdir().unwrap(), locks: Locks::default(), cancel: AtomicBool::new(false) }
        }
        fn run(&self, host: &Host, job: &Job) -> (Outcome, Vec<crate::daemon::history::Run>) {
            let history = History::new(self.state.path().join("history.jsonl"));
            let (clock, stop) = (FixedClock(now()), AtomicBool::new(false));
            let ctx = Context { history: &history, clock: &clock, locks: &self.locks, poll: Duration::from_secs(1), stop: &stop };
            let progress = self.state.path().join("progress");
            let state = self.state.path().join("replication.json");
            let env = Env { ctx: &ctx, progress_dir: &progress, cancel: &self.cancel, replication_state: &state };
            let outcome = run_mirror(&env, host, job, now(), false, "run-1");
            (outcome, runs(&history.read()))
        }
    }

    fn job() -> Job {
        Job::test_mirror("job", "tank/data", "hourly", "aws:my bkt/p")
    }

    fn position(calls: &[String], prefix: &str) -> usize {
        calls.iter().position(|c| c.starts_with(prefix)).unwrap_or_else(|| panic!("no call starting {prefix:?} in {calls:?}"))
    }

    #[test]
    fn a_run_clones_checks_the_source_syncs_with_a_backup_dir_and_destroys_the_clone() {
        let rig = Rig::new();
        let host = Host::new();
        let mut j = job();
        j.bwlimit = Some("5M".into());
        let (outcome, history) = rig.run(&host, &j);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert_eq!(outcome.message, "mirrored hourly-20261008T110000Z to the aws remote, 500 B sent");
        let calls = host.calls();
        assert!(position(&calls, "zfs clone") < position(&calls, "rclone size") && position(&calls, "rclone size") < position(&calls, "rclone sync") && position(&calls, "rclone sync") < position(&calls, "zfs destroy"));
        assert!(calls.contains(&format!("zfs clone -o mountpoint=/var/run/zfsctl/job -o readonly=on -o com.sun:auto-snapshot=false -- tank/data@{B} tank/zit-mirror-job")), "{calls:?}");
        assert_eq!(host.sync_args.lock().unwrap()[0], crate::rclone::sync_args("/var/run/zfsctl/job", "aws:my bkt/p", "20261008T233000Z", Some("5M")));
        assert!(!host.clone_exists());
        assert_eq!(history.last().unwrap().result, Some(RunResult::Ok));
        assert!(crate::daemon::progress::read_all(&rig.state.path().join("progress")).is_empty());
    }

    #[test]
    fn an_encrypted_source_is_refused_before_anything_is_cloned_or_synced() {
        let rig = Rig::new();
        for status in ["available", "unavailable"] {
            let mut host = Host::new();
            host.keystatus = Some(status);
            let (outcome, history) = rig.run(&host, &job());
            assert_eq!(outcome.result, RunResult::Failed);
            assert!(outcome.message.starts_with("tank/data is encrypted"), "{}", outcome.message);
            assert!(!host.calls().iter().any(|c| c.starts_with("zfs clone") || c.starts_with("zfs snapshot") || c.starts_with("rclone")), "{:?}", host.calls());
            assert_eq!(history.last().unwrap().result, Some(RunResult::Failed));
        }
    }

    #[test]
    fn a_remote_and_path_with_spaces_stay_single_arguments() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.real_dirs = vec!["/var/run/zfsctl/job/my photos".into()];
        let mut j = job();
        j.path = Some("my photos".into());
        let (outcome, _) = rig.run(&host, &j);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        let sync = host.sync_args.lock().unwrap()[0].clone();
        assert_eq!(sync[1], "/var/run/zfsctl/job/my photos");
        assert_eq!(sync[2], "aws:my bkt/p/current");
    }

    #[test]
    fn a_path_through_a_symlink_is_refused_before_anything_is_read_or_synced() {
        let rig = Rig::new();
        let mut host = Host::new();
        // `docs` is listed, `docs/out` is not: it is a symlink (to `/`, say), which rclone would follow as root.
        host.real_dirs = vec!["/var/run/zfsctl/job/docs".into()];
        let mut j = job();
        j.path = Some("docs/out".into());
        let (outcome, _) = rig.run(&host, &j);
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("not a plain folder") && outcome.message.contains("'out'") && outcome.message.contains("nothing was changed"), "{}", outcome.message);
        assert!(host.sync_args.lock().unwrap().is_empty());
        assert!(!host.calls().iter().any(|c| c.starts_with("rclone size")), "{:?}", host.calls());
        assert!(!host.clone_exists(), "the clone is destroyed");
    }

    #[test]
    fn a_path_of_real_folders_is_accepted_part_by_part() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.real_dirs = vec!["/var/run/zfsctl/job/docs".into(), "/var/run/zfsctl/job/docs/2026".into()];
        let mut j = job();
        j.path = Some("docs/2026/".into());
        let (outcome, _) = rig.run(&host, &j);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        let lists: Vec<String> = host.calls().into_iter().filter(|c| c.contains("--dirs-only")).collect();
        assert_eq!(lists, ["rclone lsf --dirs-only --max-depth 1 /var/run/zfsctl/job", "rclone lsf --dirs-only --max-depth 1 /var/run/zfsctl/job/docs"]);
    }

    #[test]
    fn an_old_folder_that_already_exists_is_never_written_into() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.old_stamp_listing = Some("docs/\na.txt\n".into());
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("old/20261008T233000Z already exists") && outcome.message.contains("nothing was changed"), "{}", outcome.message);
        assert!(host.sync_args.lock().unwrap().is_empty());
        assert!(!host.clone_exists());
        assert!(host.calls().iter().any(|c| c.starts_with("rclone lsf") && c.contains("aws:my bkt/p/old/20261008T233000Z")), "{:?}", host.calls());
    }

    #[test]
    fn a_clone_that_was_made_but_not_mounted_is_destroyed() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.clone_made_but_unmounted = true;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(!host.clone_exists(), "{:?}", host.calls());
        assert!(!host.calls().iter().any(|c| c.starts_with("rclone")));
    }

    #[test]
    fn an_empty_source_is_refused_and_nothing_is_synced() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.size_json = "{\"count\":0,\"bytes\":0,\"sizeless\":0}".into();
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("no files") && outcome.message.contains("nothing was changed"), "{}", outcome.message);
        assert!(host.sync_args.lock().unwrap().is_empty());
        assert!(!host.clone_exists(), "the clone is destroyed");
    }

    #[test]
    fn a_failed_clone_touches_nothing() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.clone_fails = true;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("cannot clone"), "{}", outcome.message);
        assert!(!host.calls().iter().any(|c| c.starts_with("rclone")));
    }

    #[test]
    fn a_sync_failure_is_failed_with_rclones_message_and_never_shows_the_remote() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.sync_script = r#"echo '{"level":"error","msg":"Failed to sync: aws:my bkt/p/current: AccessDenied"}' >&2; exit 1"#.into();
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("AccessDenied") && outcome.message.contains("exit 1"), "{}", outcome.message);
        assert!(!outcome.message.contains("my bkt"), "{}", outcome.message);
        assert!(!host.clone_exists());
    }

    #[test]
    fn skipped_items_are_not_mentioned() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.sync_script = format!(r#"echo '{{"level":"notice","msg":"Can'"'"'t follow symlink without -L/--copy-links","object":"link"}}' >&2; {STATS}; exit 0"#);
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        let lower = outcome.message.to_lowercase();
        assert!(!lower.contains("symlink") && !lower.contains("skipped"), "{}", outcome.message);
    }

    #[test]
    fn cancel_stops_the_sync_destroys_the_clone_and_records_cancelled() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.sync_script = "sleep 60".into();
        rig.cancel.store(true, Ordering::SeqCst);
        let (outcome, history) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Cancelled, "{}", outcome.message);
        assert_eq!(history.last().unwrap().result, Some(RunResult::Cancelled));
        assert!(!host.clone_exists());
    }

    #[test]
    fn an_existing_clone_from_a_crashed_run_is_removed_first() {
        let rig = Rig::new();
        let host = Host::new();
        *host.clone_exists.lock().unwrap() = true;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        let calls = host.calls();
        let destroys: Vec<usize> = calls.iter().enumerate().filter(|(_, c)| c.starts_with("zfs destroy -r")).map(|(i, _)| i).collect();
        assert_eq!(destroys.len(), 2, "{calls:?}");
        assert!(destroys[0] < position(&calls, "zfs clone"));
    }

    #[test]
    fn a_missing_rclone_or_sudoers_entry_is_a_clear_failed_run_that_destroys_the_clone() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.rclone_missing = true;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("password is required"), "{}", outcome.message);
        assert!(host.sync_args.lock().unwrap().is_empty() && !host.clone_exists());
    }

    #[test]
    fn a_busy_job_is_skipped_and_touches_nothing() {
        let rig = Rig::new();
        let host = Host::new();
        let _held = rig.locks.try_acquire("local", "#mir@job").unwrap();
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(host.calls().is_empty());
    }

    #[test]
    fn a_pinned_snapshot_that_is_gone_fails_and_a_missing_prefix_snapshot_is_skipped() {
        let rig = Rig::new();
        let mut pinned = job();
        pinned.source = Some("snapshot".into());
        pinned.snapshot = Some("nope".into());
        let (outcome, _) = rig.run(&Host::new(), &pinned);
        assert_eq!(outcome.result, RunResult::Failed);
        let mut other_prefix = job();
        other_prefix.prefix = Some("daily".into());
        let (outcome, _) = rig.run(&Host::new(), &other_prefix);
        assert_eq!(outcome.result, RunResult::Skipped);
    }

    #[test]
    fn a_fresh_snapshot_source_takes_a_snapshot_first() {
        let rig = Rig::new();
        let host = Host::new();
        let mut j = job();
        j.source = Some("new".into());
        let (outcome, _) = rig.run(&host, &j);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(host.calls().iter().any(|c| c.starts_with("zfs snapshot")), "{:?}", host.calls());
    }

    #[test]
    fn only_sync_and_read_only_rclone_commands_are_ever_run() {
        let rig = Rig::new();
        let host = Host::new();
        rig.run(&host, &job());
        for call in host.calls().iter().filter(|c| c.starts_with("rclone")) {
            let verb = call.split(' ').nth(1).unwrap();
            assert!(matches!(verb, "sync" | "size" | "lsf"), "{call}");
            for forbidden in ["delete", "purge", "deletefile", "cleanup", "move", "--delete-excluded", "--links"] {
                assert!(!call.split(' ').any(|a| a == forbidden), "{call}");
            }
        }
    }
}
