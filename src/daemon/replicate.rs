//! A scheduled host-to-host replication run: pick what to send, then pipe `zfs send` on the
//! source into `zfs receive -s -u` on the destination, reporting progress and honouring cancel.

use super::history::{EventKind, RunResult};
use super::progress::{self, ProgressWriter};
use super::scrub::{event, finish, Context, Outcome, Slot};
use super::transfer::pump;
use crate::exec::CommandRunner;
use crate::jobs::{effective_prefix, Job};
use crate::zfs::replication::{list_snapshot_infos, list_snapshot_infos_or_empty, parse_send_size, plan, receive_args, resume_send_args, resume_token, send_args, Plan};
use chrono::{DateTime, Utc};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub struct Env<'a> {
    pub ctx: &'a Context<'a>,
    pub progress_dir: &'a Path,
    pub cancel: &'a AtomicBool,
    /// `replication.json`: what each file-target job last wrote.
    pub replication_state: &'a Path,
}

/// What a run decided to send.
struct Chosen {
    send: Vec<String>,
    dry: Vec<String>,
    label: String,
    resumed: bool,
    /// Sent from a base snapshot, so `force` has a base to roll the destination back to.
    incremental: bool,
}

/// A command's stderr as one line: its lines joined, without the sender's `warning:` noise (the
/// send reports "signal received" whenever the receive stops first).
pub(super) fn message_of(stderr: &str) -> String {
    stderr.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("warning:")).collect::<Vec<_>>().join(" ")
}

pub fn run_replicate(env: &Env, src: &dyn CommandRunner, dst: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome {
    let ctx = env.ctx;
    let dataset = job.dataset.clone().unwrap_or_default();
    let dest_host = job.dest_host.clone().unwrap_or_default();
    let dest_ds = job.dest_dataset.clone().unwrap_or_default();
    let prefix = effective_prefix(job).to_string();
    let slot = Slot { scheduled_for: Some(scheduled_for), catch_up };
    let failed = |msg: String| finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, msg, slot);
    let skipped = |msg: String| finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, msg, slot);

    // Only the destination is locked: two jobs may send from one dataset to different destinations
    // (ZFS allows concurrent sends), while an overlapping slot of the same job, or two jobs aimed at
    // one destination, collide here. `#` cannot appear in a pool name, so the key never collides
    // with a scrub's or a snapshot's lock key.
    let Some(_dst_guard) = ctx.locks.try_acquire(&dest_host, &format!("#dst@{dest_ds}")) else { return skipped("skipped: already running".to_string()) };

    let token = match resume_token(dst, &dest_ds) {
        Ok(t) => t,
        Err(e) => return failed(e.to_string()),
    };
    let chosen = if let Some(token) = token {
        Chosen { send: resume_send_args(&token, false), dry: resume_send_args(&token, true), label: "resumed an interrupted transfer".to_string(), resumed: true, incremental: false }
    } else {
        let source = match list_snapshot_infos(src, &dataset) {
            Ok(s) => s,
            Err(e) => return failed(e.to_string()),
        };
        let destination = match list_snapshot_infos_or_empty(dst, &dest_ds) {
            Ok(d) => d,
            Err(e) => return failed(e.to_string()),
        };
        match plan(&source, &destination, &prefix) {
            Plan::NoSnapshots => return skipped(format!("skipped: no snapshots with prefix '{prefix}' on {dataset}")),
            Plan::UpToDate(name) => return skipped(format!("skipped: up to date ({name})")),
            Plan::Send { new, base } => Chosen {
                send: send_args(&dataset, &new, base.as_deref(), false),
                dry: send_args(&dataset, &new, base.as_deref(), true),
                label: match &base {
                    Some(base) => format!("sent {new} (incremental from {base})"),
                    None => format!("sent {new} (full)"),
                },
                resumed: false,
                incremental: base.is_some(),
            },
        }
    };

    // The size is only for the progress display; a failed or unparseable estimate is not an error.
    let dry: Vec<&str> = chosen.dry.iter().map(String::as_str).collect();
    let total = src.run("zfs", &dry).ok().and_then(|out| parse_send_size(&out.stdout));

    let send_refs: Vec<&str> = chosen.send.iter().map(String::as_str).collect();
    // `-F` rolls a destination back to the incremental base; on a full stream it would instead
    // overwrite whatever dataset is already there, so a full send is never forced.
    let recv = receive_args(&dest_ds, job.force && (chosen.incremental || chosen.resumed));
    let recv_refs: Vec<&str> = recv.iter().map(String::as_str).collect();
    let (send_cmd, recv_cmd) = match (src.command("zfs", &send_refs), dst.command("zfs", &recv_refs)) {
        (Ok(s), Ok(r)) => (s, r),
        (Err(e), _) | (_, Err(e)) => return failed(e.to_string()),
    };

    let mut started = event(job, run_id, EventKind::Started, ctx.clock.now());
    started.scheduled_for = Some(scheduled_for);
    started.catch_up = catch_up;
    let _ = ctx.history.append(&started);

    let mut writer = ProgressWriter::new(env.progress_dir.to_path_buf(), run_id.to_string(), job.id.clone(), total, ctx.clock.now());
    let result = pump(send_cmd, recv_cmd, env.cancel, |bytes| writer.update(bytes, ctx.clock.now()));
    progress::remove(env.progress_dir, run_id);

    let transfer = match result {
        Ok(t) => t,
        Err(e) => return failed(format!("could not run the transfer: {e}")),
    };
    if transfer.cancelled {
        return finish(ctx, job, run_id, EventKind::Finished, RunResult::Cancelled, format!("cancelled after {}; the next run resumes", progress::human_bytes(transfer.bytes)), slot);
    }
    if !transfer.recv_ok || !transfer.send_ok {
        // When the send failed with something to say, that is the cause: the receive then only
        // complains about a stream that was cut off.
        let send_text = message_of(&transfer.send_stderr);
        let (side, text) = if !transfer.send_ok && (transfer.recv_ok || !send_text.is_empty()) { ("send", send_text) } else { ("receive", message_of(&transfer.recv_stderr)) };
        let mut message = if text.is_empty() { format!("{side} failed") } else { format!("{side} failed: {text}") };
        if chosen.resumed {
            let _ = write!(message, ". If the transfer cannot be continued, discard it with `zfs receive -A {dest_ds}` on {dest_host}");
        }
        return failed(message);
    }
    finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, format!("{}, {}", chosen.label, progress::human_bytes(transfer.bytes)), slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::history::{runs, History};
    use crate::daemon::scrub::{Clock, Locks};
    use crate::error::Result;
    use crate::exec::CommandOutput;
    use std::process::Command;
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;
    use std::time::Duration;

    struct FixedClock(DateTime<Utc>);
    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
        fn sleep(&self, _: Duration) {}
    }

    /// One host: answers the zfs queries from fixed strings and hands out scripted shells for
    /// `send` and `receive`, recording every call.
    struct Host {
        listing: String,
        token: String,
        size_output: String,
        send_script: String,
        recv_script: String,
        queries: Mutex<Vec<Vec<String>>>,
        commands: Mutex<Vec<Vec<String>>>,
    }

    impl Host {
        fn new(listing: &str) -> Self {
            Host { listing: listing.into(), token: "-".into(), size_output: "size\t11\n".into(), send_script: "printf 'STREAMDATA'".into(), recv_script: "cat > /dev/null".into(), queries: Mutex::new(vec![]), commands: Mutex::new(vec![]) }
        }
        fn commands(&self) -> Vec<Vec<String>> {
            self.commands.lock().unwrap().clone()
        }
    }

    impl CommandRunner for Host {
        fn run(&self, _cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.queries.lock().unwrap().push(args.iter().map(|a| (*a).to_string()).collect());
            let ok = |stdout: &str| Ok(CommandOutput { stdout: stdout.to_string(), stderr: String::new(), status: 0 });
            match args.first().copied() {
                Some("list") => ok(&self.listing),
                Some("get") => ok(&self.token),
                Some("send") => ok(&self.size_output),
                other => panic!("unexpected query {other:?}"),
            }
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _: &str) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn command(&self, _cmd: &str, args: &[&str]) -> Result<Command> {
            self.commands.lock().unwrap().push(args.iter().map(|a| (*a).to_string()).collect());
            let script = if args.first() == Some(&"send") { &self.send_script } else { &self.recv_script };
            let mut c = Command::new("sh");
            c.args(["-c", script]);
            Ok(c)
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-06T19:30:00Z").unwrap().with_timezone(&Utc)
    }

    const A: &str = "hourly-20261006T100000Z";
    const B: &str = "hourly-20261006T110000Z";

    fn src_listing() -> String {
        format!("tank/data@{A}\t1\t10\ntank/data@{B}\t2\t20\n")
    }

    struct Rig {
        dir: tempfile::TempDir,
        locks: Locks,
        cancel: AtomicBool,
    }

    impl Rig {
        fn new() -> Self {
            Rig { dir: tempfile::tempdir().unwrap(), locks: Locks::default(), cancel: AtomicBool::new(false) }
        }
        fn run(&self, src: &dyn CommandRunner, dst: &dyn CommandRunner, job: &Job) -> (Outcome, Vec<crate::daemon::history::Run>) {
            let history = History::new(self.dir.path().join("history.jsonl"));
            let (clock, stop) = (FixedClock(now()), AtomicBool::new(false));
            let ctx = Context { history: &history, clock: &clock, locks: &self.locks, poll: Duration::from_secs(1), stop: &stop };
            let progress = self.dir.path().join("progress");
            let state = self.dir.path().join("replication.json");
            let env = Env { ctx: &ctx, progress_dir: &progress, cancel: &self.cancel, replication_state: &state };
            let outcome = run_replicate(&env, src, dst, job, now(), false, "run-1");
            (outcome, runs(&history.read()))
        }
    }

    fn job() -> Job {
        Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data")
    }

    #[test]
    fn a_first_run_sends_the_newest_snapshot_in_full_and_receives_it_resumable_and_unmounted() {
        let rig = Rig::new();
        let (src, dst) = (Host::new(&src_listing()), Host::new(""));
        let (outcome, history) = rig.run(&src, &dst, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(outcome.message.contains(B) && outcome.message.contains("full"), "{}", outcome.message);
        assert_eq!(src.commands(), vec![vec!["send".to_string(), "--".into(), format!("tank/data@{B}")]]);
        assert_eq!(dst.commands(), vec![vec!["receive", "-s", "-u", "--", "backup/data"]]);
        assert_eq!((history.len(), history[0].result), (1, Some(RunResult::Ok)));
        assert!(crate::daemon::progress::read_all(&rig.dir.path().join("progress")).is_empty(), "the progress file is removed at the end");
    }

    #[test]
    fn a_later_run_sends_incrementally_from_the_newest_common_snapshot() {
        let rig = Rig::new();
        let (src, dst) = (Host::new(&src_listing()), Host::new(&format!("backup/data@{A}\t1\t5\n")));
        let (outcome, _) = rig.run(&src, &dst, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(outcome.message.contains(&format!("incremental from {A}")), "{}", outcome.message);
        assert_eq!(src.commands()[0], vec!["send".to_string(), "-i".into(), format!("@{A}"), "--".into(), format!("tank/data@{B}")]);
    }

    #[test]
    fn an_up_to_date_destination_and_a_source_without_matching_snapshots_are_skipped() {
        let rig = Rig::new();
        let (src, dst) = (Host::new(&src_listing()), Host::new(&format!("backup/data@{B}\t2\t6\n")));
        let (outcome, _) = rig.run(&src, &dst, &job());
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(outcome.message.contains("up to date"), "{}", outcome.message);
        assert!(src.commands().is_empty() && dst.commands().is_empty());
        let (outcome, _) = rig.run(&Host::new("tank/data@manual\t9\t1\n"), &Host::new(""), &job());
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(outcome.message.contains("no snapshots"), "{}", outcome.message);
    }

    #[test]
    fn an_interrupted_receive_is_resumed_from_its_token_without_choosing_a_snapshot() {
        let rig = Rig::new();
        let src = Host::new("");
        let mut dst = Host::new("");
        dst.token = "1-abc-def\n".into();
        let (outcome, _) = rig.run(&src, &dst, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(outcome.message.contains("resumed"), "{}", outcome.message);
        assert_eq!(src.commands(), vec![vec!["send", "-t", "1-abc-def"]]);
    }

    #[test]
    fn force_adds_dash_capital_f_to_an_incremental_receive_and_nothing_else_does() {
        let rig = Rig::new();
        let mut forced = job();
        forced.force = true;
        let (src, dst) = (Host::new(&src_listing()), Host::new(&format!("backup/data@{A}\t1\t5\n")));
        rig.run(&src, &dst, &forced);
        assert_eq!(dst.commands(), vec![vec!["receive", "-s", "-u", "-F", "--", "backup/data"]]);
        let (src, dst) = (Host::new(&src_listing()), Host::new(&format!("backup/data@{A}\t1\t5\n")));
        rig.run(&src, &dst, &job());
        assert_eq!(dst.commands(), vec![vec!["receive", "-s", "-u", "--", "backup/data"]], "no force, no -F");
    }

    #[test]
    fn force_never_turns_a_full_send_into_an_overwrite_of_an_existing_dataset() {
        let rig = Rig::new();
        let mut forced = job();
        forced.force = true;
        let (src, dst) = (Host::new(&src_listing()), Host::new(""));
        rig.run(&src, &dst, &forced);
        assert_eq!(dst.commands(), vec![vec!["receive", "-s", "-u", "--", "backup/data"]]);
    }

    #[test]
    fn two_jobs_from_one_source_dataset_do_not_block_each_other() {
        let rig = Rig::new();
        let _another_job_sending_from_the_same_dataset = rig.locks.try_acquire("local", "#src@tank/data");
        let (src, dst) = (Host::new(&src_listing()), Host::new(""));
        let (outcome, _) = rig.run(&src, &dst, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    }

    #[test]
    fn when_the_send_fails_first_its_message_wins_over_the_receives_complaint_about_a_cut_off_stream() {
        let rig = Rig::new();
        let mut src = Host::new("");
        src.send_script = "echo \"cannot resume send: 'x' is no longer the same snapshot\" >&2; exit 1".into();
        let mut dst = Host::new("");
        dst.token = "1-abc\n".into();
        dst.recv_script = "cat > /dev/null; echo 'cannot receive: failed to read from stream' >&2; exit 1".into();
        let (outcome, _) = rig.run(&src, &dst, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("no longer the same snapshot"), "{}", outcome.message);
        assert!(outcome.message.contains("zfs receive -A backup/data"), "{}", outcome.message);
    }

    #[test]
    fn a_receive_failure_is_failed_with_the_receives_whole_message_not_just_its_first_line() {
        let rig = Rig::new();
        let src = Host::new(&src_listing());
        let mut dst = Host::new("");
        // zfs wraps this message over two lines; the run keeps both and drops the sender's noise.
        dst.recv_script = "cat > /dev/null; printf 'cannot receive incremental stream: destination backup/data has been modified\\nsince most recent snapshot\\nwarning: cannot send x: signal received\\n' >&2; exit 1".into();
        let (outcome, history) = rig.run(&src, &dst, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("has been modified since most recent snapshot"), "{}", outcome.message);
        assert!(!outcome.message.contains("signal received"), "{}", outcome.message);
        assert_eq!(history[0].result, Some(RunResult::Failed));
    }

    #[test]
    fn a_send_failure_is_failed_with_the_sends_message() {
        let rig = Rig::new();
        let mut src = Host::new(&src_listing());
        src.send_script = "echo \"cannot send: no such snapshot\" >&2; exit 2".into();
        let (outcome, _) = rig.run(&src, &Host::new(""), &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("no such snapshot"), "{}", outcome.message);
    }

    #[test]
    fn a_failing_resume_says_how_to_discard_the_token() {
        let rig = Rig::new();
        let mut src = Host::new("");
        src.send_script = "echo \"cannot resume send: 'x' is no longer the same snapshot\" >&2; exit 1".into();
        let mut dst = Host::new("");
        dst.token = "1-abc\n".into();
        let (outcome, _) = rig.run(&src, &dst, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("zfs receive -A backup/data"), "{}", outcome.message);
    }

    #[test]
    fn cancel_records_cancelled_and_leaves_no_progress_file() {
        let rig = Rig::new();
        let mut src = Host::new(&src_listing());
        src.send_script = "sleep 60".into();
        rig.cancel.store(true, Ordering::SeqCst);
        let (outcome, history) = rig.run(&src, &Host::new(""), &job());
        assert_eq!(outcome.result, RunResult::Cancelled, "{}", outcome.message);
        assert_eq!(history[0].result, Some(RunResult::Cancelled));
        assert!(crate::daemon::progress::read_all(&rig.dir.path().join("progress")).is_empty());
    }

    #[test]
    fn a_run_whose_source_or_destination_is_busy_is_skipped_and_touches_nothing() {
        let rig = Rig::new();
        let (src, dst) = (Host::new(&src_listing()), Host::new(""));
        let _held = rig.locks.try_acquire("fbsd16", "#dst@backup/data").unwrap();
        let (outcome, _) = rig.run(&src, &dst, &job());
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(src.commands().is_empty() && dst.commands().is_empty() && dst.queries.lock().unwrap().is_empty());
    }

    #[test]
    fn an_unreadable_destination_fails_the_run_without_sending() {
        struct Broken;
        impl CommandRunner for Broken {
            fn run(&self, _: &str, _: &[&str]) -> Result<CommandOutput> {
                Ok(CommandOutput { stdout: String::new(), stderr: "permission denied".into(), status: 1 })
            }
            fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> Result<CommandOutput> {
                self.run(c, a)
            }
            fn run_unprivileged(&self, c: &str, a: &[&str]) -> Result<CommandOutput> {
                self.run(c, a)
            }
        }
        let rig = Rig::new();
        let src = Host::new(&src_listing());
        let (outcome, _) = rig.run(&src, &Broken, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("permission denied"), "{}", outcome.message);
        assert!(src.commands().is_empty());
    }
}
