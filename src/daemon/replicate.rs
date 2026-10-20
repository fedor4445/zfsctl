//! A scheduled host-to-host replication run: pick what to send, then pipe `zfs send` on the
//! source into `zfs receive -s -u` on the destination, reporting progress and honouring cancel.

use super::history::{EventKind, RunResult};
use super::progress::{self, ProgressWriter};
use super::scrub::{event, finish, Context, Outcome, Slot};
use super::transfer::pump;
use crate::exec::CommandRunner;
use crate::error::AppError;
use crate::jobs::{effective_prefix, valid_dataset, valid_snapshot_name, Job, DEST_FILE};
use crate::zfs::replication::{list_snapshot_infos, list_snapshot_infos_or_empty, parse_send_size, plan, prune_selection, receive_args, resume_send_args, resume_token, send_args, Plan};
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
    /// Destination snapshots the prune must not destroy whatever their age: the base of this transfer and
    /// the snapshot it brings (or, when up to date, the one both sides share).
    protect: Vec<String>,
}

/// A command's stderr as one line: its lines joined, without the sender's `warning:` noise (the
/// send reports "signal received" whenever the receive stops first).
pub(super) fn message_of(stderr: &str) -> String {
    stderr.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("warning:")).collect::<Vec<_>>().join(" ")
}

/// What `zfs destroy` says (as `zfs` 15 prints it) when a snapshot is held or has a clone: the
/// snapshot is left for a later run, and that is not an error.
const HELD: &str = "it's being held";
const CLONED: &str = "dependent clones";

struct Pruned {
    destroyed: usize,
    /// The job's own snapshots left on the destination (including any that could not be destroyed).
    remaining: usize,
    /// Refused because of a hold or a clone.
    refused: usize,
    failures: Vec<String>,
}

/// Destroys the job's own snapshots on the destination beyond the newest `keep`, one
/// `zfs destroy dataset@snap` each, oldest first. `Err` means nothing was destroyed (the listing
/// failed or could not be parsed, or a name did not validate).
fn prune_destination(dst: &dyn CommandRunner, dest_ds: &str, prefix: &str, keep: u32, protect: &[String]) -> std::result::Result<Pruned, String> {
    if !valid_dataset(dest_ds) {
        return Err(format!("not a valid dataset name: {dest_ds}"));
    }
    let listing = list_snapshot_infos_or_empty(dst, dest_ds).map_err(|e| e.to_string())?;
    let own = listing.iter().filter(|s| crate::daemon::snapshot::is_own(prefix, &s.name)).count();
    // The newest by txg is not always the base: after a rollback on the source the shared snapshot can be older.
    let doomed: Vec<String> = prune_selection(&listing, prefix, keep).into_iter().filter(|n| !protect.contains(n)).collect();
    if let Some(bad) = doomed.iter().find(|n| !valid_snapshot_name(n)) {
        return Err(format!("not a valid snapshot name: {bad}"));
    }
    let mut pruned = Pruned { destroyed: 0, remaining: own, refused: 0, failures: Vec::new() };
    for name in &doomed {
        let target = format!("{dest_ds}@{name}");
        // The same guard as every other destroy: a hold, or a hold check that cannot run, leaves it alone.
        if crate::zfs::snapshot::refuse_if_held(dst, dest_ds, name, false).is_err() {
            pruned.refused += 1;
            continue;
        }
        match crate::zfs::run_checked(dst, "zfs", &["destroy", "--", &target], &format!("zfs destroy {target}")) {
            Ok(_) => pruned.destroyed += 1,
            Err(AppError::CommandFailed { stderr, .. }) if stderr.contains(HELD) || stderr.contains(CLONED) => pruned.refused += 1,
            Err(e) => pruned.failures.push(format!("{name}: {e}")),
        }
    }
    pruned.remaining -= pruned.destroyed;
    Ok(pruned)
}

/// Runs the prune a finished (or up-to-date) run is due, and says how it went: the history suffix
/// when it worked, the failure text when it did not.
fn prune_after(dst: &dyn CommandRunner, job: &Job, dest_host: &str, dest_ds: &str, prefix: &str, protect: &[String], sent: bool) -> std::result::Result<String, String> {
    let Some(keep) = job.dest_keep.filter(|_| dest_host != DEST_FILE) else { return Ok(String::new()) };
    match prune_destination(dst, dest_ds, prefix, keep, protect) {
        Err(e) => Err(format!("{}{dest_host} was not pruned: {e}", if sent { "the transfer succeeded, but " } else { "" })),
        Ok(p) => {
            let left = if p.refused == 0 { String::new() } else { format!(", {} held or cloned left alone", p.refused) };
            let note = format!("; pruned {}, kept {} on {dest_host}{left}", p.destroyed, p.remaining);
            if p.failures.is_empty() {
                Ok(note)
            } else {
                Err(format!("{}{note}; could not destroy {}", if sent { "the transfer succeeded" } else { "nothing needed sending" }, p.failures.join("; ")))
            }
        }
    }
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

    // A raw job sends an encrypted source as it is; any other job would deliver it decrypted.
    if !job.raw {
        if let Err(msg) = super::guard::check_source(src, &dataset, super::guard::Need::NotEncryptedUnlessRaw) {
            return failed(msg);
        }
    }

    let token = match resume_token(dst, &dest_ds) {
        Ok(t) => t,
        Err(e) => return failed(e.to_string()),
    };
    let chosen = if let Some(token) = token {
        // A raw job never completes a plain send's leftovers: the rest of an encrypted dataset would go out decrypted.
        if job.raw && !crate::zfs::replication::token_is_raw(src, &token).unwrap_or(false) {
            match crate::zfs::encryption::query(src, &dataset) {
                Ok(None) => {}
                Ok(Some(_)) => return failed(format!("{dest_ds} holds an interrupted transfer that was not sent raw, and {dataset} is encrypted: resuming it would send the rest decrypted. Discard it with `zfs receive -A {dest_ds}` on {dest_host}, then the next run starts again raw")),
                Err(e) => return failed(format!("could not check whether {dataset} is encrypted: {e}")),
            }
        }
        Chosen { send: resume_send_args(&token, false), dry: resume_send_args(&token, true), label: "resumed an interrupted transfer".to_string(), resumed: true, incremental: false, protect: Vec::new() }
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
            Plan::UpToDate(name) => {
                return match prune_after(dst, job, &dest_host, &dest_ds, &prefix, &[name.clone()], false) {
                    Ok(note) => skipped(format!("skipped: up to date ({name}){note}")),
                    Err(e) => failed(format!("up to date ({name}); {e}")),
                };
            }
            Plan::Send { new, base } => Chosen {
                send: send_args(&dataset, &new, base.as_deref(), false, job.raw),
                dry: send_args(&dataset, &new, base.as_deref(), true, job.raw),
                label: match &base {
                    Some(base) => format!("sent {new} (incremental from {base})"),
                    None => format!("sent {new} (full)"),
                },
                resumed: false,
                incremental: base.is_some(),
                protect: base.iter().cloned().chain([new.clone()]).collect(),
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
    // A resumed transfer completes an earlier slot's send; the next ordinary run prunes.
    if chosen.resumed {
        return finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, format!("{}, {}", chosen.label, progress::human_bytes(transfer.bytes)), slot);
    }
    let sent = format!("{}, {}", chosen.label, progress::human_bytes(transfer.bytes));
    match prune_after(dst, job, &dest_host, &dest_ds, &prefix, &chosen.protect, true) {
        Ok(note) => finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, format!("{sent}{note}"), slot),
        Err(e) => failed(format!("{sent}; {e}")),
    }
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
        /// Answers the second and later `zfs list` calls instead of `listing` (the prune's, after the plan's).
        later_listing: Option<String>,
        /// `(snapshot target, stderr)` pairs that `zfs destroy` refuses.
        destroy_fails: Vec<(String, String)>,
        /// Snapshots (full names) that report one hold.
        held: Vec<String>,
        /// The source's `keystatus` when it is encrypted.
        keystatus: Option<&'static str>,
        /// What `zfs send -nv -t <token>` prints (the decoded resume token).
        token_decode: String,
        queries: Mutex<Vec<Vec<String>>>,
        commands: Mutex<Vec<Vec<String>>>,
    }

    impl Host {
        fn new(listing: &str) -> Self {
            Host { listing: listing.into(), token: "-".into(), size_output: "size\t11\n".into(), send_script: "printf 'STREAMDATA'".into(), recv_script: "cat > /dev/null".into(), later_listing: None, destroy_fails: vec![], held: vec![], keystatus: None, token_decode: String::new(), queries: Mutex::new(vec![]), commands: Mutex::new(vec![]) }
        }
        fn commands(&self) -> Vec<Vec<String>> {
            self.commands.lock().unwrap().clone()
        }
        /// Every `zfs destroy` query, as the arguments after `destroy`.
        fn destroys(&self) -> Vec<Vec<String>> {
            self.queries.lock().unwrap().iter().filter(|q| q[0] == "destroy").map(|q| q[1..].to_vec()).collect()
        }
    }

    impl CommandRunner for Host {
        fn run(&self, _cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.queries.lock().unwrap().push(args.iter().map(|a| (*a).to_string()).collect());
            let ok = |stdout: &str| Ok(CommandOutput { stdout: stdout.to_string(), stderr: String::new(), status: 0 });
            let lists = self.queries.lock().unwrap().iter().filter(|q| q[0] == "list").count();
            match args.first().copied() {
                Some("list") if lists > 1 && self.later_listing.is_some() => ok(self.later_listing.as_deref().unwrap()),
                Some("list") => ok(&self.listing),
                Some("destroy") => {
                    let target = args.last().unwrap();
                    match self.destroy_fails.iter().find(|(t, _)| t == target) {
                        Some((_, stderr)) => Ok(CommandOutput { stdout: String::new(), stderr: stderr.clone(), status: 1 }),
                        None => ok(""),
                    }
                }
                Some("get") if args.contains(&"userrefs") => {
                    let rows: Vec<String> = [Some(&self.listing), self.later_listing.as_ref()].into_iter().flatten().flat_map(|l| l.lines()).filter_map(|l| l.split('\t').next()).map(|n| format!("{n}\t{}\n", u8::from(self.held.iter().any(|h| h == n)))).collect();
                    ok(&rows.concat())
                }
                Some("holds") => ok(""),
                Some("get") if args.contains(&"name,property,value") => ok(&crate::daemon::guard::fake_answer("tank/data", self.keystatus)),
                Some("get") => ok(&self.token),
                Some("send") if args.contains(&"-nv") => ok(&self.token_decode),
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

    fn older(hour: u32) -> String {
        format!("hourly-20261006T{hour:02}0000Z")
    }

    /// The destination's listing: five older own snapshots (hours 05-09, txg 1-5), the base `A`, and any extras.
    fn target_with_history(extra: &str) -> String {
        let older: String = (5..=9).map(|h| format!("backup/data@{}\t{}\t{}\n", older(h), 100 + h, h - 4)).collect();
        format!("{older}backup/data@{A}\t1\t6\n{extra}")
    }

    fn keeping(n: u32) -> Job {
        let mut j = job();
        j.dest_keep = Some(n);
        j
    }

    fn doomed(hours: &[u32]) -> Vec<Vec<String>> {
        hours.iter().map(|h| vec!["--".to_string(), format!("backup/data@{}", older(*h))]).collect()
    }

    #[test]
    fn a_dest_keep_prunes_the_destination_after_a_send_with_one_plain_destroy_per_snapshot() {
        let rig = Rig::new();
        let (src, mut dst) = (Host::new(&src_listing()), Host::new(&target_with_history("")));
        dst.later_listing = Some(target_with_history(&format!("backup/data@{B}\t2\t7\n")));
        let (outcome, history) = rig.run(&src, &dst, &keeping(3));
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert_eq!(dst.destroys(), doomed(&[5, 6, 7, 8]), "oldest first, no -r, `--` before the name");
        assert!(outcome.message.contains("pruned 4, kept 3 on fbsd16"), "{}", outcome.message);
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn it_also_prunes_after_an_up_to_date_skip() {
        let rig = Rig::new();
        let src = Host::new(&src_listing());
        let dst = Host::new(&target_with_history(&format!("backup/data@{B}\t2\t7\n")));
        let (outcome, _) = rig.run(&src, &dst, &keeping(3));
        assert_eq!(outcome.result, RunResult::Skipped, "{}", outcome.message);
        assert!(outcome.message.contains("up to date") && outcome.message.contains("pruned 4"), "{}", outcome.message);
        assert_eq!(dst.destroys(), doomed(&[5, 6, 7, 8]));
    }

    #[test]
    fn nothing_is_pruned_after_a_failed_cancelled_or_resumed_transfer() {
        let rig = Rig::new();
        let (src, mut dst) = (Host::new(&src_listing()), Host::new(&target_with_history("")));
        dst.recv_script = "cat > /dev/null; echo boom >&2; exit 1".into();
        let (outcome, _) = rig.run(&src, &dst, &keeping(1));
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(dst.destroys().is_empty());

        let mut slow = Host::new(&src_listing());
        slow.send_script = "sleep 60".into();
        rig.cancel.store(true, Ordering::SeqCst);
        let dst = Host::new(&target_with_history(""));
        let (outcome, _) = rig.run(&slow, &dst, &keeping(1));
        assert_eq!(outcome.result, RunResult::Cancelled);
        assert!(dst.destroys().is_empty());
        rig.cancel.store(false, Ordering::SeqCst);

        let mut resumed = Host::new(&target_with_history(""));
        resumed.token = "1-abc-def\n".into();
        let (outcome, _) = rig.run(&Host::new(""), &resumed, &keeping(1));
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(resumed.destroys().is_empty());
    }

    #[test]
    fn an_unreadable_listing_after_the_transfer_prunes_nothing_and_fails_the_run() {
        let rig = Rig::new();
        let (src, mut dst) = (Host::new(&src_listing()), Host::new(&target_with_history("")));
        dst.later_listing = Some("not a listing\n".into());
        let (outcome, _) = rig.run(&src, &dst, &keeping(1));
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("transfer succeeded") && outcome.message.contains("not pruned"), "{}", outcome.message);
        assert!(dst.destroys().is_empty());
    }

    #[test]
    fn a_held_or_cloned_snapshot_is_left_alone_and_is_not_an_error() {
        let rig = Rig::new();
        let (src, mut dst) = (Host::new(&src_listing()), Host::new(&target_with_history("")));
        dst.destroy_fails = vec![
            (format!("backup/data@{}", older(5)), "cannot destroy snapshot backup/data@x: it's being held. Run 'zfs holds -r backup/data@x' to see holders.".into()),
            (format!("backup/data@{}", older(6)), "cannot destroy 'backup/data@y': snapshot has dependent clones\nuse '-R' to destroy the following datasets:\n".into()),
        ];
        let (outcome, _) = rig.run(&src, &dst, &keeping(3));
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert_eq!(dst.destroys().len(), 3, "all three were attempted");
        assert!(outcome.message.contains("pruned 1") && outcome.message.contains("2 held or cloned"), "{}", outcome.message);
    }

    #[test]
    fn the_destination_pruner_skips_a_held_snapshot_on_the_target_host() {
        let rig = Rig::new();
        let (src, mut dst) = (Host::new(&src_listing()), Host::new(&target_with_history("")));
        dst.later_listing = Some(target_with_history(&format!("backup/data@{B}\t2\t7\n")));
        dst.held = vec![format!("backup/data@{}", older(5))];
        let (outcome, _) = rig.run(&src, &dst, &keeping(3));
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert_eq!(dst.destroys(), doomed(&[6, 7, 8]), "the held one is never destroyed, the later excess ones still are");
        assert!(outcome.message.contains("pruned 3, kept 4 on fbsd16") && outcome.message.contains("1 held or cloned left alone"), "{}", outcome.message);
    }

    #[test]
    fn another_destroy_failure_fails_the_run_but_the_rest_are_still_pruned() {
        let rig = Rig::new();
        let (src, mut dst) = (Host::new(&src_listing()), Host::new(&target_with_history("")));
        dst.destroy_fails = vec![(format!("backup/data@{}", older(5)), "cannot destroy: permission denied".into())];
        let (outcome, _) = rig.run(&src, &dst, &keeping(3));
        assert_eq!(outcome.result, RunResult::Failed);
        assert_eq!(dst.destroys().len(), 3);
        assert!(outcome.message.contains("transfer succeeded") && outcome.message.contains(&older(5)) && outcome.message.contains("permission denied"), "{}", outcome.message);
    }

    #[test]
    fn the_snapshot_both_sides_share_is_never_pruned_even_when_it_is_not_the_newest_on_the_target() {
        // The source was rolled back to A; the target still has a newer snapshot that no longer exists on the source.
        let rig = Rig::new();
        let src = Host::new(&format!("tank/data@{A}\t1\t10\n"));
        let dst = Host::new(&target_with_history("backup/data@hourly-20261006T120000Z\t500\t99\n"));
        let (outcome, _) = rig.run(&src, &dst, &keeping(1));
        assert_eq!(outcome.result, RunResult::Skipped, "{}", outcome.message);
        let names: Vec<String> = dst.destroys().into_iter().map(|d| d[1].clone()).collect();
        assert!(!names.contains(&format!("backup/data@{A}")), "the shared snapshot is the next incremental's base: {names:?}");
        assert!(!names.iter().any(|n| n.contains("120000Z")), "the newest is kept: {names:?}");
        assert_eq!(names.len(), 5, "{names:?}");
        assert!(!outcome.message.contains("transfer succeeded"), "{}", outcome.message);
    }

    #[test]
    fn without_dest_keep_there_is_no_extra_listing_and_no_destroy() {
        let rig = Rig::new();
        let (src, dst) = (Host::new(&src_listing()), Host::new(&target_with_history("")));
        let (outcome, _) = rig.run(&src, &dst, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(dst.destroys().is_empty());
        assert_eq!(dst.queries.lock().unwrap().iter().filter(|q| q[0] == "list").count(), 1);
        assert!(!outcome.message.contains("pruned"));
    }

    #[test]
    fn look_alike_names_on_the_target_are_never_destroyed() {
        let rig = Rig::new();
        let (src, mut dst) = (Host::new(&src_listing()), Host::new(&target_with_history("")));
        dst.later_listing = Some(target_with_history("backup/data@hourly-latest\t900\t1\nbackup/data@manual\t901\t1\nbackup/data@hourly-20261006T0900Z\t902\t1\n"));
        rig.run(&src, &dst, &keeping(1));
        let names: Vec<String> = dst.destroys().into_iter().map(|d| d[1].clone()).collect();
        assert_eq!(names.len(), 5);
        assert!(names.iter().all(|n| n.contains("hourly-2026") && !n.contains("0900Z")), "{names:?}");
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
    fn an_encrypted_source_is_refused_before_any_snapshot_is_listed_or_sent() {
        let rig = Rig::new();
        for status in ["available", "unavailable"] {
            let mut src = Host::new(&src_listing());
            src.keystatus = Some(status);
            let dst = Host::new("");
            let (outcome, _) = rig.run(&src, &dst, &job());
            assert_eq!(outcome.result, RunResult::Failed);
            assert!(outcome.message.starts_with("tank/data is encrypted"), "{}", outcome.message);
            assert!(src.commands().is_empty() && dst.commands().is_empty(), "nothing was sent or received");
            assert!(!src.queries.lock().unwrap().iter().any(|q| q[0] == "list" || q[0] == "send"), "{:?}", src.queries.lock().unwrap());
        }
    }

    #[test]
    fn a_raw_job_sends_an_encrypted_source_with_dash_w_and_never_asks_whether_it_is_encrypted() {
        let rig = Rig::new();
        for status in ["available", "unavailable"] {
            let mut src = Host::new(&src_listing());
            src.keystatus = Some(status);
            let dst = Host::new("");
            let mut raw = job();
            raw.raw = true;
            let (outcome, _) = rig.run(&src, &dst, &raw);
            assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
            assert!(src.commands().iter().any(|c| c[0] == "send" && c[1] == "-w"), "{:?}", src.commands());
            assert!(src.queries.lock().unwrap().iter().any(|q| q[0] == "send" && q[1] == "-w"), "the size estimate is raw too");
            assert!(!src.queries.lock().unwrap().iter().any(|q| q[0] == "get" && q.iter().any(|a| a.contains("keystatus"))));
        }
    }

    #[test]
    fn a_job_that_is_not_raw_is_told_to_turn_raw_on() {
        let rig = Rig::new();
        let mut src = Host::new(&src_listing());
        src.keystatus = Some("available");
        let (outcome, _) = rig.run(&src, &Host::new(""), &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("Turn Raw on"), "{}", outcome.message);
        assert!(src.commands().is_empty());
    }

    const RAW_TOKEN: &str = "resume token contents:\nnvlist version: 0\n\tobject = 0x2\n\ttoname = tank/data@h\n\tcompressok = 1\n\trawok = 1\n";
    const PLAIN_TOKEN: &str = "resume token contents:\nnvlist version: 0\n\tobject = 0x2\n\ttoname = tank/data@h\n";

    #[test]
    fn a_raw_job_never_completes_a_plain_sends_leftovers_of_an_encrypted_source() {
        let rig = Rig::new();
        let mut src = Host::new(&src_listing());
        src.keystatus = Some("available");
        src.token_decode = PLAIN_TOKEN.into();
        let mut dst = Host::new("");
        dst.token = "1-abc-def".into();
        let mut raw = job();
        raw.raw = true;
        let (outcome, _) = rig.run(&src, &dst, &raw);
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("not sent raw") && outcome.message.contains("zfs receive -A backup/data"), "{}", outcome.message);
        assert!(src.commands().is_empty() && dst.commands().is_empty(), "nothing was sent or received");
    }

    #[test]
    fn a_raw_job_resumes_a_raw_token_and_a_plain_token_of_a_plain_source() {
        let rig = Rig::new();
        for (keystatus, decode) in [(Some("available"), RAW_TOKEN), (None, PLAIN_TOKEN)] {
            let mut src = Host::new(&src_listing());
            src.keystatus = keystatus;
            src.token_decode = decode.into();
            let mut dst = Host::new("");
            dst.token = "1-abc-def".into();
            let mut raw = job();
            raw.raw = true;
            let (outcome, _) = rig.run(&src, &dst, &raw);
            assert_eq!(outcome.result, RunResult::Ok, "{keystatus:?}: {}", outcome.message);
            assert_eq!(src.commands()[0], ["send", "-t", "1-abc-def"], "resumed by token");
        }
    }

    #[test]
    fn a_raw_job_that_cannot_tell_whether_the_source_is_encrypted_does_not_resume() {
        let rig = Rig::new();
        let src = Host::new(&src_listing()); // token_decode is blank: not raw
        struct Down(Host);
        impl CommandRunner for Down {
            fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
                if args.first() == Some(&"get") && args.contains(&"name,property,value") {
                    return Ok(CommandOutput { stdout: String::new(), stderr: "Connection closed".into(), status: 255 });
                }
                self.0.run(cmd, args)
            }
            fn run_with_stdin(&self, cmd: &str, a: &[&str], s: &str) -> Result<CommandOutput> { self.0.run_with_stdin(cmd, a, s) }
            fn run_unprivileged(&self, cmd: &str, a: &[&str]) -> Result<CommandOutput> { self.0.run_unprivileged(cmd, a) }
            fn command(&self, cmd: &str, a: &[&str]) -> Result<Command> { self.0.command(cmd, a) }
        }
        let src = Down(src);
        let mut dst = Host::new("");
        dst.token = "1-abc-def".into();
        let mut raw = job();
        raw.raw = true;
        let (outcome, _) = rig.run(&src, &dst, &raw);
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("could not check"), "{}", outcome.message);
        assert!(src.0.commands().is_empty());
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
