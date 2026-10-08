//! A scheduled restic backup: clone the newest `<prefix>-<timestamp>` snapshot to a fixed read-only
//! mountpoint (restic matches files against its parent backup by path, so the path must not change
//! between runs), run `restic backup` on it, destroy the clone on every exit path, then optionally
//! `restic forget --prune` this job's own backups. The source's snapshots are never touched.

use super::history::{EventKind, RunResult};
use super::progress::{self, ProgressWriter};
use super::replicate::Env;
use super::scrub::{event, finish, Outcome, Slot};
use super::snapshot::is_own;
use super::transfer::run_lines;
use crate::exec::CommandRunner;
use crate::jobs::{effective_prefix, Job};
use crate::restic::{self, Line, Summary};
use crate::zfs::replication::list_snapshot_infos;
use chrono::{DateTime, Utc};

/// Where a job's clone is mounted: the same path every run, so restic finds its parent backup.
pub const MOUNT_ROOT: &str = "/var/run/zfsctl";

/// `<pool>/zit-restic-<job id>`.
pub fn clone_name(dataset: &str, job_id: &str) -> String {
    let pool = dataset.split('/').next().unwrap_or(dataset);
    format!("{pool}/zit-restic-{job_id}")
}

pub fn mountpoint(job_id: &str) -> String {
    format!("{MOUNT_ROOT}/{job_id}")
}

fn zfs(runner: &dyn CommandRunner, args: &[&str]) -> std::result::Result<String, String> {
    match runner.run("zfs", args) {
        Ok(out) if out.success() => Ok(out.stdout),
        Ok(out) => Err(out.stderr.trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}

fn restic_query(runner: &dyn CommandRunner, args: &[String]) -> std::result::Result<String, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match runner.run("restic", &refs) {
        Ok(out) if out.success() => Ok(out.stdout),
        Ok(out) => Err(restic::error_text(&out.stdout, &out.stderr)),
        Err(e) => Err(e.to_string()),
    }
}

/// The snapshot's creation time as restic's `--time` wants it: in the host's own local time zone.
/// `None` when the host cannot say, and the backup then carries the run time.
fn snapshot_time(runner: &dyn CommandRunner, dataset: &str, snapshot: &str) -> Option<String> {
    let epoch = zfs(runner, &["get", "-H", "-p", "-o", "value", "creation", "--", &format!("{dataset}@{snapshot}")]).ok()?;
    let epoch = epoch.trim();
    if epoch.is_empty() || !epoch.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let out = runner.run_unprivileged("date", &["-r", epoch, "+%Y-%m-%d %H:%M:%S"]).ok().filter(crate::exec::CommandOutput::success)?;
    let text = out.stdout.trim().to_string();
    (text.len() == 19).then_some(text)
}

enum End {
    Done(Option<Summary>),
    Cancelled(u64),
    Failed(String),
}

#[allow(clippy::too_many_arguments)]
fn backup(env: &Env, runner: &dyn CommandRunner, job: &Job, repo: &str, password_file: &str, snapshot: &str, time: Option<&str>, mount: &str, run_id: &str) -> End {
    let args = restic::backup_args(repo, password_file, &job.host, &job.id, snapshot, time, mount);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cmd = match runner.command("restic", &refs) {
        Ok(c) => c,
        Err(e) => return End::Failed(e.to_string()),
    };
    let clock = env.ctx.clock;
    let mut writer = ProgressWriter::new(env.progress_dir.to_path_buf(), run_id.to_string(), job.id.clone(), None, clock.now());
    let (mut summary, mut fatal, mut bytes) = (None, None, 0_u64);
    let result = run_lines(cmd, env.cancel, |line| match restic::parse_line(line) {
        Line::Status { bytes_done, total_bytes } => {
            if total_bytes > 0 {
                writer.set_total(total_bytes);
            }
            bytes = bytes_done;
            writer.update(bytes_done, clock.now());
        }
        Line::Summary(s) => summary = Some(s),
        Line::Error(m) => fatal = Some(m),
        Line::Other => {}
    });
    progress::remove(env.progress_dir, run_id);
    let lines = match result {
        Ok(l) => l,
        Err(e) => return End::Failed(format!("could not run restic: {e}")),
    };
    if lines.cancelled {
        return End::Cancelled(bytes);
    }
    match lines.code {
        Some(0) => End::Done(summary),
        code => {
            let text = restic::failure_text(fatal.as_deref(), &lines.stderr);
            let what = if code == Some(3) { "restic saved the backup but could not read some files (exit 3)".to_string() } else { format!("restic failed (exit {})", code.map_or_else(|| "signal".to_string(), |c| c.to_string())) };
            End::Failed(format!("{what}: {text}"))
        }
    }
}

pub fn run_backup(env: &Env, runner: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome {
    let ctx = env.ctx;
    let dataset = job.dataset.clone().unwrap_or_default();
    let repo = job.repository.clone().unwrap_or_default();
    let password_file = job.password_file.clone().unwrap_or_default();
    let prefix = effective_prefix(job).to_string();
    let slot = Slot { scheduled_for: Some(scheduled_for), catch_up };
    let failed = |msg: String| finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, msg, slot);
    let skipped = |msg: String| finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, msg, slot);

    // `#` cannot appear in a pool name, so the key cannot collide with another kind's lock.
    let Some(_guard) = ctx.locks.try_acquire(&job.host, &format!("#bak@{}", job.id)) else { return skipped("skipped: already running".to_string()) };
    let clone = clone_name(&dataset, &job.id);
    let mount = mountpoint(&job.id);

    // A clone left by a crashed run would pin its snapshot and block the new clone.
    if zfs(runner, &["list", "-H", "-o", "name", "--", &clone]).is_ok() {
        if let Err(e) = zfs(runner, &["destroy", "--", &clone]) {
            return failed(format!("cannot remove the clone {clone} left by an earlier run: {e}"));
        }
    }

    let source = match list_snapshot_infos(runner, &dataset) {
        Ok(s) => s,
        Err(e) => return failed(e.to_string()),
    };
    let Some(newest) = source.iter().filter(|s| is_own(&prefix, &s.name)).max_by(|a, b| a.name.cmp(&b.name)) else {
        return skipped(format!("skipped: no snapshots with prefix '{prefix}' on {dataset}"));
    };
    let snapshot = newest.name.clone();

    match restic_query(runner, &restic::snapshots_args(&repo, &password_file, &job.id, &snapshot)).and_then(|json| restic::snapshot_count(&json)) {
        Ok(0) => {}
        Ok(_) => return skipped(format!("skipped: up to date ({snapshot})")),
        Err(e) => return failed(e),
    }

    let time = snapshot_time(runner, &dataset, &snapshot);
    let origin = format!("{dataset}@{snapshot}");
    if let Err(e) = zfs(runner, &["clone", "-o", &format!("mountpoint={mount}"), "-o", "readonly=on", "--", &origin, &clone]) {
        return failed(format!("cannot clone {origin}: {e}"));
    }

    let mut started = event(job, run_id, EventKind::Started, ctx.clock.now());
    started.scheduled_for = Some(scheduled_for);
    started.catch_up = catch_up;
    let _ = ctx.history.append(&started);

    let end = backup(env, runner, job, &repo, &password_file, &snapshot, time.as_deref(), &mount, run_id);
    // Always, whatever happened: the clone pins the snapshot and holds a mountpoint.
    let leftover = match zfs(runner, &["destroy", "--", &clone]) {
        Ok(_) => String::new(),
        Err(e) => format!("; could not remove the clone {clone} ({e}), the next run retries"),
    };
    match end {
        End::Cancelled(bytes) => finish(ctx, job, run_id, EventKind::Finished, RunResult::Cancelled, format!("cancelled after {}; the next run starts again{leftover}", progress::human_bytes(bytes)), slot),
        End::Failed(msg) => failed(format!("{msg}{leftover}")),
        End::Done(summary) => {
            let message = match summary {
                Some(s) => format!("backed up {snapshot} ({} new, {} changed, {} unmodified), {} added", s.new, s.changed, s.unmodified, progress::human_bytes(s.added)),
                None => format!("backed up {snapshot}"),
            };
            if !leftover.is_empty() {
                return failed(format!("{message}{leftover}"));
            }
            let policy = job.retention.as_deref().and_then(|t| restic::parse_retention(t).ok()).filter(|r| !r.is_empty());
            if let Some(policy) = policy {
                let forget_args = restic::forget_args(&repo, &password_file, &job.host, &job.id, &policy);
                let mut forgot = restic_query(runner, &forget_args);
                // A cancelled or killed run leaves a stale repository lock that blocks `forget --prune`.
                // `restic unlock` removes only stale locks, so clear them once and retry once.
                if forgot.as_ref().is_err_and(|e| e.contains("already locked")) && restic_query(runner, &restic::unlock_args(&repo, &password_file)).is_ok() {
                    forgot = restic_query(runner, &forget_args);
                }
                if let Err(e) = forgot {
                    return failed(format!("backed up {snapshot} but forget failed: {e}"));
                }
            }
            finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, message, slot)
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

    const A: &str = "hourly-20261008T100000Z";
    const B: &str = "hourly-20261008T110000Z";
    const OK_SCRIPT: &str = r#"echo '{"message_type":"status","percent_done":0.5,"total_bytes":1000,"bytes_done":500}'; echo '{"message_type":"summary","files_new":3,"files_changed":2,"files_unmodified":40,"data_added":2048}'"#;

    /// One host: answers zfs and restic queries from fixed strings, remembers whether the clone
    /// exists, hands out a scripted shell for `restic backup`, and records every call.
    struct Host {
        listing: String,
        restic_snapshots: std::result::Result<String, String>,
        backup_script: String,
        date_works: bool,
        clone_fails: bool,
        destroy_fails: bool,
        forget_fails: bool,
        /// While true `restic forget` fails like a repository with a stale lock; `restic unlock` clears it.
        forget_locked: Mutex<bool>,
        unlock_fails: bool,
        clone_exists: Mutex<bool>,
        calls: Mutex<Vec<String>>,
        backup_args: Mutex<Vec<Vec<String>>>,
    }

    impl Host {
        fn new() -> Self {
            Host {
                listing: format!("tank/data@{A}\t1\t10\ntank/data@{B}\t2\t20\n"),
                restic_snapshots: Ok("[]\n".to_string()),
                backup_script: OK_SCRIPT.to_string(),
                date_works: true,
                clone_fails: false,
                destroy_fails: false,
                forget_fails: false,
                forget_locked: Mutex::new(false),
                unlock_fails: false,
                clone_exists: Mutex::new(false),
                calls: Mutex::new(vec![]),
                backup_args: Mutex::new(vec![]),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn called(&self, prefix: &str) -> bool {
            self.calls().iter().any(|c| c.starts_with(prefix))
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
                ("zfs", Some("list")) if args.contains(&"snapshot") => ok(&self.listing),
                ("zfs", Some("list")) => {
                    if self.clone_exists() { ok("tank/zit-restic-job\n") } else { fail("dataset does not exist") }
                }
                ("zfs", Some("get")) => ok("1791500000\n"),
                ("zfs", Some("clone")) => {
                    if self.clone_fails {
                        return fail("cannot mount '/var/run/zfsctl/job': failed to create mountpoint");
                    }
                    *self.clone_exists.lock().unwrap() = true;
                    ok("")
                }
                ("zfs", Some("destroy")) => {
                    if self.destroy_fails {
                        return fail("dataset is busy");
                    }
                    *self.clone_exists.lock().unwrap() = false;
                    ok("")
                }
                ("restic", Some("snapshots")) => match &self.restic_snapshots {
                    Ok(json) => ok(json),
                    Err(e) => Ok(CommandOutput { stdout: String::new(), stderr: e.clone(), status: 12 }),
                },
                ("restic", Some("forget")) => {
                    if *self.forget_locked.lock().unwrap() {
                        return fail("unable to create lock in backend: repository is already locked by PID 14568 on fb by root");
                    }
                    if self.forget_fails { fail("Fatal: unable to save snapshot") } else { ok("") }
                }
                ("restic", Some("unlock")) => {
                    if self.unlock_fails {
                        return fail("Fatal: unlock broke");
                    }
                    *self.forget_locked.lock().unwrap() = false;
                    ok("")
                }
                other => panic!("unexpected call {other:?}"),
            }
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _: &str) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(format!("{cmd} {}", args.join(" ")));
            assert_eq!(cmd, "date");
            let status = i32::from(!self.date_works);
            Ok(CommandOutput { stdout: if self.date_works { "2026-10-08 19:33:20\n".to_string() } else { String::new() }, stderr: String::new(), status })
        }
        fn command(&self, cmd: &str, args: &[&str]) -> Result<Command> {
            assert_eq!(cmd, "restic");
            self.calls.lock().unwrap().push(format!("restic {}", args.join(" ")));
            self.backup_args.lock().unwrap().push(args.iter().map(|a| (*a).to_string()).collect());
            let mut c = Command::new("sh");
            c.args(["-c", &self.backup_script]);
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
            let outcome = run_backup(&env, host, job, now(), false, "run-1");
            (outcome, runs(&history.read()))
        }
    }

    fn job() -> Job {
        Job::test_backup("job", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw")
    }

    #[test]
    fn names_are_derived_from_the_pool_and_the_job_id() {
        assert_eq!(clone_name("tank/data/sub", "backup-1"), "tank/zit-restic-backup-1");
        assert_eq!(clone_name("tank", "b"), "tank/zit-restic-b");
        assert_eq!(mountpoint("backup-1"), "/var/run/zfsctl/backup-1");
    }

    #[test]
    fn a_run_clones_the_newest_snapshot_backs_up_the_clone_destroys_it_and_reports_the_counts() {
        let rig = Rig::new();
        let host = Host::new();
        let (outcome, history) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert_eq!(outcome.message, format!("backed up {B} (3 new, 2 changed, 40 unmodified), 2.0 KiB added"));
        let calls = host.calls();
        let at = |prefix: &str| calls.iter().position(|c| c.starts_with(prefix)).unwrap_or_else(|| panic!("no call starting {prefix:?} in {calls:?}"));
        assert!(at("restic snapshots") < at("zfs clone") && at("zfs clone") < at("restic backup") && at("restic backup") < at("zfs destroy"));
        assert!(calls.contains(&format!("zfs clone -o mountpoint=/var/run/zfsctl/job -o readonly=on -- tank/data@{B} tank/zit-restic-job")), "{calls:?}");
        assert!(calls.contains(&"zfs destroy -- tank/zit-restic-job".to_string()));
        let backup = host.backup_args.lock().unwrap()[0].clone();
        assert!(backup.contains(&format!("--tag=zfsctl-snapshot:{B}")) && backup.contains(&"--tag=zfsctl:job".to_string()));
        assert!(backup.contains(&"--time=2026-10-08 19:33:20".to_string()), "{backup:?}");
        assert_eq!(backup.last().map(String::as_str), Some("/var/run/zfsctl/job"));
        assert!(!host.clone_exists());
        assert!(!host.called("restic forget"), "no retention, no forget");
        assert_eq!((history.len(), history[0].result), (1, Some(RunResult::Ok)));
        assert!(crate::daemon::progress::read_all(&rig.state.path().join("progress")).is_empty());
    }

    #[test]
    fn without_a_working_date_the_backup_runs_without_dash_dash_time() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.date_works = false;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(!host.backup_args.lock().unwrap()[0].iter().any(|a| a.starts_with("--time")));
    }

    #[test]
    fn a_source_without_matching_snapshots_is_skipped_and_nothing_is_created() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.listing = "tank/data@manual\t9\t1\n".to_string();
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(outcome.message.contains("no snapshots"), "{}", outcome.message);
        assert!(!host.called("zfs clone") && !host.called("restic"));
    }

    #[test]
    fn a_snapshot_already_in_the_repository_is_skipped_before_any_clone() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.restic_snapshots = Ok("[{\"id\":\"abc\"}]".to_string());
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(outcome.message.contains("up to date") && outcome.message.contains(B), "{}", outcome.message);
        assert!(!host.called("zfs clone"));
    }

    #[test]
    fn an_unreachable_or_wrongly_locked_repository_fails_the_run_before_cloning() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.restic_snapshots = Err("Fatal: wrong password or no key found".to_string());
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("wrong password"), "{}", outcome.message);
        assert!(!host.called("zfs clone"));
    }

    #[test]
    fn a_leftover_clone_from_a_crashed_run_is_destroyed_first() {
        let rig = Rig::new();
        let host = Host::new();
        *host.clone_exists.lock().unwrap() = true;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        let calls = host.calls();
        let first_destroy = calls.iter().position(|c| c.starts_with("zfs destroy")).unwrap();
        let clone = calls.iter().position(|c| c.starts_with("zfs clone")).unwrap();
        assert!(first_destroy < clone, "{calls:?}");
    }

    #[test]
    fn a_leftover_clone_that_cannot_be_removed_fails_the_run_without_backing_up() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.destroy_fails = true;
        *host.clone_exists.lock().unwrap() = true;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("left by an earlier run") && outcome.message.contains("busy"), "{}", outcome.message);
        assert!(!host.called("zfs clone") && !host.called("restic backup"));
    }

    #[test]
    fn a_failed_clone_is_a_failed_run_with_zfss_message_and_no_backup() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.clone_fails = true;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("failed to create mountpoint"), "{}", outcome.message);
        assert!(!host.called("restic backup"));
    }

    #[test]
    fn a_failing_backup_is_failed_with_resticss_message_destroys_the_clone_and_never_forgets() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.backup_script = r#"echo '{"message_type":"summary","files_new":1,"files_changed":0,"files_unmodified":0,"data_added":1}'; echo '{"message_type":"exit_error","code":1,"message":"Fatal: unable to save snapshot: disk full"}'; exit 1"#.to_string();
        let mut with_policy = job();
        with_policy.retention = Some("7d".to_string());
        let (outcome, history) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Failed, "{}", outcome.message);
        assert!(outcome.message.contains("disk full"), "{}", outcome.message);
        assert!(!host.clone_exists());
        assert!(!host.called("restic forget"));
        assert_eq!(history[0].result, Some(RunResult::Failed));
    }

    #[test]
    fn exit_code_three_means_the_snapshot_exists_but_files_were_unreadable_and_is_a_failure() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.backup_script = format!("{OK_SCRIPT}; echo 'error: permission denied' >&2; exit 3");
        let mut with_policy = job();
        with_policy.retention = Some("7d".to_string());
        let (outcome, _) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("could not read some files"), "{}", outcome.message);
        assert!(!host.clone_exists() && !host.called("restic forget"));
    }

    #[test]
    fn a_cancelled_run_is_recorded_cancelled_destroys_the_clone_and_does_not_forget() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.backup_script = "sleep 60".to_string();
        rig.cancel.store(true, Ordering::SeqCst);
        let mut with_policy = job();
        with_policy.retention = Some("7d".to_string());
        let (outcome, history) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Cancelled, "{}", outcome.message);
        assert_eq!(history[0].result, Some(RunResult::Cancelled));
        assert!(!host.clone_exists() && !host.called("restic forget"));
        assert!(crate::daemon::progress::read_all(&rig.state.path().join("progress")).is_empty());
    }

    #[test]
    fn a_successful_backup_with_a_policy_forgets_only_this_jobs_backups_afterwards() {
        let rig = Rig::new();
        let host = Host::new();
        let mut with_policy = job();
        with_policy.retention = Some("7d 4w".to_string());
        let (outcome, _) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        let calls = host.calls();
        let forget = calls.iter().find(|c| c.starts_with("restic forget")).expect("forget ran");
        assert_eq!(forget, "restic forget --repo=/mnt/usb/restic --password-file=/root/pw --prune --tag=zfsctl:job --host=local --keep-daily=7 --keep-weekly=4");
        assert!(calls.iter().position(|c| c.starts_with("zfs destroy")) < calls.iter().position(|c| c.starts_with("restic forget")), "the clone is gone before the slow prune");
    }

    #[test]
    fn a_blank_policy_forgets_nothing() {
        let rig = Rig::new();
        let host = Host::new();
        let mut blank = job();
        blank.retention = Some("   ".to_string());
        rig.run(&host, &blank);
        assert!(!host.called("restic forget"));
    }

    #[test]
    fn a_failed_forget_is_a_failed_run_that_says_the_backup_exists() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.forget_fails = true;
        let mut with_policy = job();
        with_policy.retention = Some("7d".to_string());
        let (outcome, _) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains(&format!("backed up {B}")) && outcome.message.contains("forget failed"), "{}", outcome.message);
    }

    #[test]
    fn a_clone_that_cannot_be_destroyed_is_reported_and_the_next_run_retries() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.destroy_fails = true;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("could not remove the clone") && outcome.message.contains("next run"), "{}", outcome.message);
    }

    #[test]
    fn the_repository_never_appears_in_a_run_message() {
        let rig = Rig::new();
        let host = Host::new();
        let mut secretive = job();
        secretive.repository = Some("sftp:me:hunter2@host:/srv/restic".to_string());
        let (ok, history) = rig.run(&host, &secretive);
        assert!(!ok.message.contains("hunter2") && !format!("{history:?}").contains("hunter2"));
        let mut failing = Host::new();
        failing.clone_fails = true;
        let (bad, _) = rig.run(&failing, &secretive);
        assert!(!bad.message.contains("hunter2"), "{}", bad.message);
    }

    #[test]
    fn a_busy_job_is_skipped_and_touches_nothing() {
        let rig = Rig::new();
        let host = Host::new();
        let _held = rig.locks.try_acquire("local", "#bak@job").unwrap();
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(outcome.message.contains("already running"), "{}", outcome.message);
        assert!(host.calls().is_empty());
    }

    #[test]
    fn two_jobs_for_one_dataset_use_separate_clones_and_mountpoints() {
        let mut other = job();
        other.id = "job-two".to_string();
        assert_ne!(clone_name("tank/data", &job().id), clone_name("tank/data", &other.id));
        assert_ne!(mountpoint(&job().id), mountpoint(&other.id));
        let rig = Rig::new();
        let _held = rig.locks.try_acquire("local", "#bak@job");
        let host = Host::new();
        let (outcome, _) = rig.run(&host, &other);
        assert_ne!(outcome.result, RunResult::Skipped, "the other job's lock does not block this one: {}", outcome.message);
    }

    #[test]
    fn a_stale_lock_left_by_an_earlier_cancelled_run_is_cleared_once_and_forget_is_retried() {
        let rig = Rig::new();
        let host = Host::new();
        *host.forget_locked.lock().unwrap() = true;
        let mut with_policy = job();
        with_policy.retention = Some("1l".to_string());
        let (outcome, _) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        let calls = host.calls();
        let forgets: Vec<usize> = calls.iter().enumerate().filter(|(_, c)| c.starts_with("restic forget")).map(|(i, _)| i).collect();
        let unlock = calls.iter().position(|c| c.starts_with("restic unlock")).expect("unlock ran");
        assert_eq!(forgets.len(), 2, "{calls:?}");
        assert!(forgets[0] < unlock && unlock < forgets[1], "{calls:?}");
        assert_eq!(calls[unlock], "restic unlock --repo=/mnt/usb/restic --password-file=/root/pw", "only stale locks: no --remove-all");
    }

    #[test]
    fn a_forget_that_fails_for_another_reason_does_not_unlock_anything() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.forget_fails = true;
        let mut with_policy = job();
        with_policy.retention = Some("1l".to_string());
        let (outcome, _) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(!host.called("restic unlock"));
    }

    #[test]
    fn a_failing_unlock_is_reported_with_the_original_lock_error() {
        let rig = Rig::new();
        let mut host = Host::new();
        *host.forget_locked.lock().unwrap() = true;
        host.unlock_fails = true;
        let mut with_policy = job();
        with_policy.retention = Some("1l".to_string());
        let (outcome, _) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("already locked") && outcome.message.contains("forget failed"), "{}", outcome.message);
    }
}
