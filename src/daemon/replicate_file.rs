//! A scheduled replication to stream files in a directory on the hub. The send runs through the
//! source host's runner; the hub (root) writes `<name>@<snap>.zfs` itself, as `.partial` first and
//! renamed once complete and fsynced, so a half-written file is never mistaken for a stream.
//! `replication.json` remembers what each job last wrote, which is all a file target knows about
//! "the other side": a file target cannot resume, a failed or cancelled run just starts again.

use super::history::{EventKind, RunResult};
use super::progress::{self, ProgressWriter};
use super::replicate::{message_of, Env};
use super::scrub::{event, finish, Outcome, Slot};
use super::transfer::pump_to_file;
use crate::exec::CommandRunner;
use crate::jobs::{effective_prefix, Job};
use crate::zfs::replication::{list_snapshot_infos, parse_send_size, plan, send_args, Plan, SnapInfo};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

/// What a job last wrote. The guid (not just the name) tells a snapshot destroyed and recreated
/// under the same name from the one that was written, and `dir` keeps an edited `dest_dir` from
/// getting an increment with no full stream beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    pub snapshot: String,
    pub guid: u64,
    pub dir: String,
}

/// Two file jobs can finish at the same moment; their read-modify-write of the state file must not interleave.
static STATE_LOCK: Mutex<()> = Mutex::new(());

/// The recorded state; a missing or unreadable file counts as nothing recorded (the next run is a full stream).
pub fn read_state(path: &Path) -> BTreeMap<String, Sent> {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

pub fn record(path: &Path, job_id: &str, sent: Sent) -> std::io::Result<()> {
    let _guard = STATE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut state = read_state(path);
    state.insert(job_id.to_string(), sent);
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(serde_json::to_string_pretty(&state).map_err(std::io::Error::other)?.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// `tank-data@s1.zfs` (full) or `tank-data@s1..s2.zfs` (incremental from s1).
pub fn stream_name(dataset: &str, new: &str, base: Option<&str>) -> String {
    let flat = dataset.replace('/', "-");
    match base {
        Some(base) => format!("{flat}@{base}..{new}.zfs"),
        None => format!("{flat}@{new}.zfs"),
    }
}

/// Whether `dir` holds a stream that ends at `snapshot`: the full `<flat>@<snapshot>.zfs` or an increment
/// `<flat>@<from>..<snapshot>.zfs`. A recorded snapshot without one is as good as not recorded (a swapped or
/// unmounted disk, a deleted job re-added, an over-pruned directory), and the next stream must then be full.
fn stream_exists(dir: &str, flat: &str, snapshot: &str) -> bool {
    let (full, head, tail) = (format!("{flat}@{snapshot}.zfs"), format!("{flat}@"), format!("..{snapshot}.zfs"));
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.filter_map(std::result::Result::ok).any(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name == full || (name.starts_with(&head) && name.ends_with(&tail))
        })
    })
}

pub fn run_replicate_file(env: &Env, src: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome {
    let ctx = env.ctx;
    let dataset = job.dataset.clone().unwrap_or_default();
    let dir = job.dest_dir.clone().unwrap_or_default();
    let prefix = effective_prefix(job).to_string();
    let slot = Slot { scheduled_for: Some(scheduled_for), catch_up };
    let failed = |msg: String| finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, msg, slot);
    let skipped = |msg: String| finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, msg, slot);

    // One writer per directory (the source is not locked, as for host-to-host). `#` cannot appear in
    // a pool name, so the key cannot collide with a scrub's, a snapshot's or another replication's.
    let Some(_guard) = ctx.locks.try_acquire("local", &format!("#dir@{dir}")) else { return skipped("skipped: already running".to_string()) };

    if let Err(msg) = super::guard::check_source(src, &dataset, super::guard::Need::NotEncrypted) {
        return failed(msg);
    }
    let source = match list_snapshot_infos(src, &dataset) {
        Ok(s) => s,
        Err(e) => return failed(e.to_string()),
    };
    // The recorded snapshot stands in for "what the other side has": `plan` then gives exactly the
    // rule we want (an increment only from a snapshot that still exists on the source, by guid).
    let flat = dataset.replace('/', "-");
    let recorded = read_state(env.replication_state).remove(&job.id).filter(|s| s.dir == dir && stream_exists(&dir, &flat, &s.snapshot));
    let known: Vec<SnapInfo> = recorded.iter().map(|s| SnapInfo { name: s.snapshot.clone(), guid: s.guid, createtxg: 0 }).collect();
    let (new, base) = match plan(&source, &known, &prefix) {
        Plan::NoSnapshots => return skipped(format!("skipped: no snapshots with prefix '{prefix}' on {dataset}")),
        Plan::UpToDate(name) => return skipped(format!("skipped: up to date ({name})")),
        Plan::Send { new, base } => (new, base),
    };
    let Some(new_guid) = source.iter().find(|s| s.name == new).map(|s| s.guid) else { return failed(format!("snapshot {new} vanished from {dataset}")) };

    let name = stream_name(&dataset, &new, base.as_deref());
    let partial = Path::new(&dir).join(format!("{name}.partial"));
    let target = Path::new(&dir).join(&name);
    if let Err(e) = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir) {
        return failed(format!("cannot create {dir}: {e}"));
    }
    // Streams hold a whole dataset and the daemon writes as root: never into a directory others can write to.
    match std::fs::metadata(&dir) {
        Ok(m) if m.is_dir() && m.permissions().mode() & 0o022 == 0 => {}
        Ok(m) if m.is_dir() => return failed(format!("{dir} is writable by group or others; refusing to write streams there (chmod go-w it)")),
        Ok(_) => return failed(format!("{dir} is not a directory")),
        Err(e) => return failed(format!("cannot read {dir}: {e}")),
    }
    // Unlink first (it never follows a symlink) and create exclusively: a file or link somebody put at this
    // predictable name can neither be truncated through nor have its permissions inherited.
    let _ = std::fs::remove_file(&partial);
    let file = match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&partial) {
        Ok(f) => f,
        Err(e) => return failed(format!("cannot create {}: {e}", partial.display())),
    };
    let discard = |msg: String| {
        let _ = std::fs::remove_file(&partial);
        failed(msg)
    };

    // The size is only for the progress display; a failed or unparseable estimate is not an error.
    let dry = send_args(&dataset, &new, base.as_deref(), true);
    let dry_refs: Vec<&str> = dry.iter().map(String::as_str).collect();
    let total = src.run("zfs", &dry_refs).ok().and_then(|out| parse_send_size(&out.stdout));
    let send = send_args(&dataset, &new, base.as_deref(), false);
    let send_refs: Vec<&str> = send.iter().map(String::as_str).collect();
    let send_cmd = match src.command("zfs", &send_refs) {
        Ok(c) => c,
        Err(e) => return discard(e.to_string()),
    };

    let mut started = event(job, run_id, EventKind::Started, ctx.clock.now());
    started.scheduled_for = Some(scheduled_for);
    started.catch_up = catch_up;
    let _ = ctx.history.append(&started);

    let mut writer = ProgressWriter::new(env.progress_dir.to_path_buf(), run_id.to_string(), job.id.clone(), total, ctx.clock.now());
    let result = pump_to_file(send_cmd, file, env.cancel, |bytes| writer.update(bytes, ctx.clock.now()));
    progress::remove(env.progress_dir, run_id);

    let transfer = match result {
        Ok(t) => t,
        Err(e) => return discard(format!("could not run the transfer: {e}")),
    };
    if transfer.cancelled {
        let _ = std::fs::remove_file(&partial);
        return finish(ctx, job, run_id, EventKind::Finished, RunResult::Cancelled, format!("cancelled after {}; the next run starts again", progress::human_bytes(transfer.bytes)), slot);
    }
    if let Some(err) = transfer.write_error {
        return discard(format!("could not write {}: {err}", partial.display()));
    }
    if !transfer.send_ok {
        let text = message_of(&transfer.send_stderr);
        return discard(if text.is_empty() { "send failed".to_string() } else { format!("send failed: {text}") });
    }
    // A complete stream is never destroyed: one already under this name (a snapshot recreated under the
    // same name, or a run that crashed after its rename) is kept as `<name>.old-N`.
    let mut kept = None;
    if std::fs::symlink_metadata(&target).is_ok() {
        let aside = (1_u32..10_000).map(|n| Path::new(&dir).join(format!("{name}.old-{n}"))).find(|p| std::fs::symlink_metadata(p).is_err());
        let Some(aside) = aside else { return discard(format!("{name} exists and so do 9999 earlier copies of it; remove some of them")) };
        if let Err(e) = std::fs::rename(&target, &aside) {
            return discard(format!("cannot keep the earlier {name}: {e}"));
        }
        kept = Some(aside);
    }
    if let Err(e) = std::fs::rename(&partial, &target) {
        return discard(format!("cannot move {} into place: {e}", partial.display()));
    }
    // Best effort: make the rename itself durable.
    let _ = std::fs::File::open(&dir).and_then(|d| d.sync_all());
    let label = match &base {
        Some(base) => format!("incremental from {base}"),
        None => "full".to_string(),
    };
    if let Err(e) = record(env.replication_state, &job.id, Sent { snapshot: new.clone(), guid: new_guid, dir: dir.clone() }) {
        return failed(format!("wrote {name} but could not record it in replication.json ({e}); the next run sends a full stream"));
    }
    let note = kept.and_then(|p| p.file_name().map(|n| format!("; kept the earlier file as {}", n.to_string_lossy()))).unwrap_or_default();
    finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, format!("wrote {name} ({label}), {}{note}", progress::human_bytes(transfer.bytes)), slot)
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
    use std::time::Duration;

    struct FixedClock(DateTime<Utc>);
    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
        fn sleep(&self, _: Duration) {}
    }

    /// The source host: answers the zfs queries from fixed strings and hands out a scripted shell for `send`.
    struct Host {
        listing: String,
        send_script: String,
        /// The source's `keystatus` when it is encrypted.
        keystatus: Option<&'static str>,
        commands: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl Host {
        fn new(listing: &str) -> Self {
            Host { listing: listing.into(), send_script: "printf 'STREAMDATA'".into(), keystatus: None, commands: std::sync::Mutex::new(vec![]) }
        }
        fn commands(&self) -> Vec<Vec<String>> {
            self.commands.lock().unwrap().clone()
        }
    }

    impl CommandRunner for Host {
        fn run(&self, _cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            let ok = |stdout: &str| Ok(CommandOutput { stdout: stdout.to_string(), stderr: String::new(), status: 0 });
            match args.first().copied() {
                Some("get") => ok(&crate::daemon::guard::fake_answer("tank/data", self.keystatus)),
                Some("list") => ok(&self.listing),
                Some("send") => ok("size\t10\n"),
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
            let mut c = Command::new("sh");
            c.args(["-c", &self.send_script]);
            Ok(c)
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-08T19:30:00Z").unwrap().with_timezone(&Utc)
    }

    const A: &str = "hourly-20261008T100000Z";
    const B: &str = "hourly-20261008T110000Z";
    const C: &str = "hourly-20261008T120000Z";

    fn listing(snaps: &[(&str, u64, u64)]) -> String {
        snaps.iter().map(|(n, g, t)| format!("tank/data@{n}\t{g}\t{t}\n")).collect()
    }

    struct Rig {
        state: tempfile::TempDir,
        dest: tempfile::TempDir,
        locks: Locks,
        cancel: AtomicBool,
    }

    impl Rig {
        fn new() -> Self {
            Rig { state: tempfile::tempdir().unwrap(), dest: tempfile::tempdir().unwrap(), locks: Locks::default(), cancel: AtomicBool::new(false) }
        }
        fn dir(&self) -> String {
            self.dest.path().join("streams").display().to_string()
        }
        fn job(&self, dir: &str) -> Job {
            let mut j = Job::test_replicate("r", "tank/data", "hourly", "file", "x");
            j.dest_dataset = None;
            j.dest_dir = Some(dir.to_string());
            j
        }
        fn state_file(&self) -> std::path::PathBuf {
            self.state.path().join("replication.json")
        }
        fn run(&self, src: &dyn CommandRunner, job: &Job) -> (Outcome, Vec<crate::daemon::history::Run>) {
            let history = History::new(self.state.path().join("history.jsonl"));
            let (clock, stop) = (FixedClock(now()), AtomicBool::new(false));
            let ctx = Context { history: &history, clock: &clock, locks: &self.locks, poll: Duration::from_secs(1), stop: &stop };
            let progress = self.state.path().join("progress");
            let state = self.state_file();
            let env = Env { ctx: &ctx, progress_dir: &progress, cancel: &self.cancel, replication_state: &state };
            let outcome = run_replicate_file(&env, src, job, now(), false, "run-1");
            (outcome, runs(&history.read()))
        }
        fn files(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.dir()).map(|d| d.filter_map(std::result::Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
            names.sort();
            names
        }
    }

    #[test]
    fn stream_names_flatten_the_dataset_and_mark_an_increment_with_two_dots() {
        assert_eq!(stream_name("tank/data/sub", "s2", None), "tank-data-sub@s2.zfs");
        assert_eq!(stream_name("tank/data", "s2", Some("s1")), "tank-data@s1..s2.zfs");
    }

    #[test]
    fn a_first_run_writes_a_full_stream_file_records_it_and_leaves_no_partial() {
        let rig = Rig::new();
        let src = Host::new(&listing(&[(A, 1, 10), (B, 2, 20)]));
        let (outcome, history) = rig.run(&src, &rig.job(&rig.dir()));
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(outcome.message.contains(B) && outcome.message.contains("full"), "{}", outcome.message);
        assert_eq!(rig.files(), vec![format!("tank-data@{B}.zfs")]);
        assert_eq!(std::fs::read_to_string(format!("{}/tank-data@{B}.zfs", rig.dir())).unwrap(), "STREAMDATA");
        assert_eq!(src.commands(), vec![vec!["send".to_string(), "--".into(), format!("tank/data@{B}")]]);
        let state = read_state(&rig.state_file());
        assert_eq!(state.get("r"), Some(&Sent { snapshot: B.to_string(), guid: 2, dir: rig.dir() }));
        assert_eq!((history.len(), history[0].result), (1, Some(RunResult::Ok)));
        assert!(crate::daemon::progress::read_all(&rig.state.path().join("progress")).is_empty());
    }

    #[test]
    fn an_encrypted_source_is_refused_and_no_stream_file_is_written() {
        let rig = Rig::new();
        let mut src = Host::new(&listing(&[(A, 1, 10), (B, 2, 20)]));
        src.keystatus = Some("available");
        let (outcome, _) = rig.run(&src, &rig.job(&rig.dir()));
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.starts_with("tank/data is encrypted"), "{}", outcome.message);
        assert!(src.commands().is_empty());
        assert!(rig.files().is_empty());
    }

    #[test]
    fn the_directory_is_created_private_and_the_files_are_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let rig = Rig::new();
        rig.run(&Host::new(&listing(&[(A, 1, 10)])), &rig.job(&rig.dir()));
        let mode = |p: &str| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&rig.dir()), 0o700);
        assert_eq!(mode(&format!("{}/tank-data@{A}.zfs", rig.dir())), 0o600);
    }

    #[test]
    fn a_later_run_writes_an_incremental_from_the_recorded_snapshot() {
        let rig = Rig::new();
        let job = rig.job(&rig.dir());
        rig.run(&Host::new(&listing(&[(A, 1, 10), (B, 2, 20)])), &job);
        let src = Host::new(&listing(&[(A, 1, 10), (B, 2, 20), (C, 3, 30)]));
        let (outcome, _) = rig.run(&src, &job);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(outcome.message.contains(&format!("incremental from {B}")), "{}", outcome.message);
        assert_eq!(src.commands()[0], vec!["send".to_string(), "-i".into(), format!("@{B}"), "--".into(), format!("tank/data@{C}")]);
        assert_eq!(rig.files(), vec![format!("tank-data@{B}..{C}.zfs"), format!("tank-data@{B}.zfs")]);
        assert_eq!(read_state(&rig.state_file()).get("r").map(|s| s.snapshot.clone()), Some(C.to_string()));
    }

    #[test]
    fn an_up_to_date_run_and_a_source_without_matching_snapshots_are_skipped_and_write_nothing() {
        let rig = Rig::new();
        let job = rig.job(&rig.dir());
        let same = listing(&[(A, 1, 10), (B, 2, 20)]);
        rig.run(&Host::new(&same), &job);
        let src = Host::new(&same);
        let (outcome, _) = rig.run(&src, &job);
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(outcome.message.contains("up to date"), "{}", outcome.message);
        assert!(src.commands().is_empty());
        let rig = Rig::new();
        let (outcome, _) = rig.run(&Host::new("tank/data@manual\t9\t1\n"), &rig.job(&rig.dir()));
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(outcome.message.contains("no snapshots"), "{}", outcome.message);
        assert!(rig.files().is_empty());
    }

    #[test]
    fn a_recorded_snapshot_that_was_pruned_from_the_source_means_a_full_stream() {
        let rig = Rig::new();
        let job = rig.job(&rig.dir());
        rig.run(&Host::new(&listing(&[(A, 1, 10), (B, 2, 20)])), &job);
        let src = Host::new(&listing(&[(C, 3, 30)]));
        let (outcome, _) = rig.run(&src, &job);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(outcome.message.contains("full"), "{}", outcome.message);
        assert_eq!(src.commands()[0], vec!["send".to_string(), "--".into(), format!("tank/data@{C}")]);
    }

    #[test]
    fn a_snapshot_destroyed_and_recreated_under_the_same_name_is_not_mistaken_for_the_recorded_one() {
        let rig = Rig::new();
        let job = rig.job(&rig.dir());
        rig.run(&Host::new(&listing(&[(A, 1, 10), (B, 2, 20)])), &job);
        // Same name, new guid: an increment from "B" would be built on the wrong base.
        let src = Host::new(&listing(&[(A, 1, 10), (B, 99, 25)]));
        let (outcome, _) = rig.run(&src, &job);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(outcome.message.contains("full"), "{}", outcome.message);
        assert_eq!(read_state(&rig.state_file()).get("r").map(|s| s.guid), Some(99));
    }

    #[test]
    fn an_edited_directory_starts_with_a_full_stream_not_an_orphan_increment() {
        let rig = Rig::new();
        rig.run(&Host::new(&listing(&[(A, 1, 10), (B, 2, 20)])), &rig.job(&rig.dir()));
        let other = rig.dest.path().join("elsewhere").display().to_string();
        let src = Host::new(&listing(&[(A, 1, 10), (B, 2, 20), (C, 3, 30)]));
        let (outcome, _) = rig.run(&src, &rig.job(&other));
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert_eq!(src.commands()[0], vec!["send".to_string(), "--".into(), format!("tank/data@{C}")]);
        assert!(std::path::Path::new(&format!("{other}/tank-data@{C}.zfs")).exists());
    }

    #[test]
    fn a_corrupt_state_file_means_a_full_stream_and_is_rewritten_valid() {
        let rig = Rig::new();
        std::fs::write(rig.state_file(), "{ not json").unwrap();
        let (outcome, _) = rig.run(&Host::new(&listing(&[(A, 1, 10)])), &rig.job(&rig.dir()));
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert_eq!(read_state(&rig.state_file()).get("r").map(|s| s.snapshot.clone()), Some(A.to_string()));
    }

    #[test]
    fn a_failing_send_leaves_no_file_and_records_nothing() {
        let rig = Rig::new();
        let mut src = Host::new(&listing(&[(A, 1, 10)]));
        src.send_script = "printf partial; echo 'cannot send: no such snapshot' >&2; exit 1".into();
        let (outcome, history) = rig.run(&src, &rig.job(&rig.dir()));
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("no such snapshot"), "{}", outcome.message);
        assert!(rig.files().is_empty(), "{:?}", rig.files());
        assert!(read_state(&rig.state_file()).is_empty());
        assert_eq!(history[0].result, Some(RunResult::Failed));
    }

    #[test]
    fn a_cancelled_run_is_recorded_cancelled_and_leaves_no_file_or_progress() {
        let rig = Rig::new();
        let mut src = Host::new(&listing(&[(A, 1, 10)]));
        src.send_script = "sleep 60".into();
        rig.cancel.store(true, Ordering::SeqCst);
        let (outcome, history) = rig.run(&src, &rig.job(&rig.dir()));
        assert_eq!(outcome.result, RunResult::Cancelled, "{}", outcome.message);
        assert_eq!(history[0].result, Some(RunResult::Cancelled));
        assert!(rig.files().is_empty() && read_state(&rig.state_file()).is_empty());
        assert!(crate::daemon::progress::read_all(&rig.state.path().join("progress")).is_empty());
    }

    #[test]
    fn a_directory_that_cannot_be_created_fails_the_run_before_sending() {
        let rig = Rig::new();
        let blocker = rig.dest.path().join("afile");
        std::fs::write(&blocker, "x").unwrap();
        let src = Host::new(&listing(&[(A, 1, 10)]));
        let (outcome, _) = rig.run(&src, &rig.job(&format!("{}/sub", blocker.display())));
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("cannot create"), "{}", outcome.message);
        assert!(src.commands().is_empty());
    }

    #[test]
    fn a_busy_directory_is_skipped_and_touches_nothing() {
        let rig = Rig::new();
        let dir = rig.dir();
        let _held = rig.locks.try_acquire("local", &format!("#dir@{dir}")).unwrap();
        let src = Host::new(&listing(&[(A, 1, 10)]));
        let (outcome, _) = rig.run(&src, &rig.job(&dir));
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(src.commands().is_empty() && rig.files().is_empty());
    }

    #[test]
    fn two_jobs_recording_at_once_do_not_lose_each_others_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replication.json");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || record(&path, &format!("job{i}"), Sent { snapshot: format!("s{i}"), guid: i, dir: "/d".into() }).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(read_state(&path).len(), 8);
    }

    #[test]
    fn a_symlink_planted_where_the_partial_file_goes_is_not_followed() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let rig = Rig::new();
        std::fs::create_dir_all(rig.dir()).unwrap();
        std::fs::set_permissions(rig.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let victim = rig.state.path().join("victim");
        std::fs::write(&victim, "PRECIOUS").unwrap();
        symlink(&victim, format!("{}/tank-data@{A}.zfs.partial", rig.dir())).unwrap();
        let (outcome, _) = rig.run(&Host::new(&listing(&[(A, 1, 10)])), &rig.job(&rig.dir()));
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "PRECIOUS", "the file the link pointed at is untouched");
        let written = format!("{}/tank-data@{A}.zfs", rig.dir());
        assert!(!std::fs::symlink_metadata(&written).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&written).unwrap(), "STREAMDATA");
    }

    #[test]
    fn a_directory_writable_by_group_or_others_is_refused_before_anything_is_sent() {
        use std::os::unix::fs::PermissionsExt;
        for mode in [0o777, 0o770, 0o707, 0o775] {
            let rig = Rig::new();
            std::fs::create_dir_all(rig.dir()).unwrap();
            std::fs::set_permissions(rig.dir(), std::fs::Permissions::from_mode(mode)).unwrap();
            let src = Host::new(&listing(&[(A, 1, 10)]));
            let (outcome, _) = rig.run(&src, &rig.job(&rig.dir()));
            assert_eq!(outcome.result, RunResult::Failed, "{mode:o}");
            assert!(outcome.message.contains("writable"), "{mode:o}: {}", outcome.message);
            assert!(src.commands().is_empty() && rig.files().is_empty());
        }
    }

    #[test]
    fn an_increment_is_only_written_when_its_base_stream_is_still_in_the_directory() {
        let rig = Rig::new();
        let job = rig.job(&rig.dir());
        rig.run(&Host::new(&listing(&[(A, 1, 10), (B, 2, 20)])), &job);
        // The directory lost its files (a swapped or unmounted disk, a pruned backup): "up to date" would be a lie.
        for f in rig.files() {
            std::fs::remove_file(format!("{}/{f}", rig.dir())).unwrap();
        }
        let src = Host::new(&listing(&[(A, 1, 10), (B, 2, 20)]));
        let (outcome, _) = rig.run(&src, &job);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(outcome.message.contains("full"), "{}", outcome.message);
        assert_eq!(rig.files(), vec![format!("tank-data@{B}.zfs")]);
        // And the newest file can be the increment that ends at the recorded snapshot.
        rig.run(&Host::new(&listing(&[(A, 1, 10), (B, 2, 20), (C, 3, 30)])), &job);
        std::fs::remove_file(format!("{}/tank-data@{B}.zfs", rig.dir())).unwrap(); // only the full is gone
        let src = Host::new(&listing(&[(A, 1, 10), (B, 2, 20), (C, 3, 30), ("hourly-20261008T130000Z", 4, 40)]));
        let (outcome, _) = rig.run(&src, &job);
        assert!(outcome.message.contains(&format!("incremental from {C}")), "the record's snapshot still has a file: {}", outcome.message);
    }

    #[test]
    fn a_complete_stream_is_never_overwritten_it_is_kept_aside() {
        let rig = Rig::new();
        let job = rig.job(&rig.dir());
        rig.run(&Host::new(&listing(&[(A, 1, 10), (B, 2, 20)])), &job);
        let mut again = Host::new(&listing(&[(A, 1, 10), (B, 99, 25)])); // B destroyed and recreated
        again.send_script = "printf 'NEWDATA'".into();
        let (outcome, _) = rig.run(&again, &job);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        let read = |f: &str| std::fs::read_to_string(format!("{}/{f}", rig.dir())).unwrap();
        assert_eq!(read(&format!("tank-data@{B}.zfs")), "NEWDATA");
        assert_eq!(read(&format!("tank-data@{B}.zfs.old-1")), "STREAMDATA", "the earlier full stream survives");
        assert!(outcome.message.contains(".old-1"), "{}", outcome.message);
        // A third time takes the next free number.
        let mut third = Host::new(&listing(&[(A, 1, 10), (B, 100, 26)]));
        third.send_script = "printf 'THIRD'".into();
        rig.run(&third, &job);
        assert_eq!(read(&format!("tank-data@{B}.zfs.old-2")), "NEWDATA");
    }
}
