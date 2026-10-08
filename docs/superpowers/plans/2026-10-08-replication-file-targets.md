# Replication File Targets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A replication job with `dest_host = "file"` writes `zfs send` streams as files in a hub directory (full, then incremental), with progress, cancel and cleanup, instead of being refused.

**Architecture:** Plan 1 already built the validation, the form, the Target column, progress/cancel plumbing and the shared-destination clash rule for file targets; only `jobs::resolve` still refuses them. This plan adds a hub-side `pump_to_file` (the file twin of `pump`), a new `src/daemon/replicate_file.rs` that selects what to send with the existing pure `plan`, remembers what it last wrote in `replication.json`, and writes `.partial` then renames; then wires it into the daemon and cleans `.partial` files at startup.

**Tech Stack:** Rust, `serde_json`, `chrono`, `tempfile` (tests), the existing `FakeRunner`/scripted-shell test doubles.

**Spec:** `docs/superpowers/specs/2026-10-06-replication-design.md` (sections "A file run", "Cancel", "Restart", "Edge cases", "Testing", delivery item 2).

## Global Constraints

- Hub only: the hub (root) writes the files itself; no remote host gets a new sudoers entry and no `sh -c` is used anywhere.
- File names: `<name>@<snap>.zfs` (full) and `<name>@<from>..<to>.zfs` (incremental) where `<name>` is the source dataset with `/` replaced by `-`; written as `<file>.partial` first, fsynced, then renamed.
- The directory is created with mode 0700, files with 0600.
- A cancelled or failed run removes its `.partial` and records nothing in `replication.json`; `replication.json` changes only on success.
- File targets cannot resume: they restart from scratch.
- Only the destination (the directory) is locked, as in plan 1's ruling: two jobs may send from one dataset to different destinations. The lock key uses `#` (never part of a pool name).
- Nothing is created, destroyed or rolled back on the source.
- Lints are `pedantic` plus `unwrap_used`/`indexing_slicing` warnings: no `unwrap()`/indexing in non-test code. Baseline: `cargo test` = 1019 passed, `cargo clippy` = exactly 1 pre-existing warning (`method get is never used`).
- Commit messages end with: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. Work on branch `replication-file-targets`; merge into `master` with `--no-ff`; no remote.

## Rulings made while planning (the spec is silent or loose)

1. **`replication.json` stores `{snapshot, guid, dir}` per job, not just a name.** A name alone cannot tell a snapshot that was destroyed and recreated (same name, different guid) from the one that was written, and an incremental from the wrong base is silently corrupt on restore. `dir` makes an edited `dest_dir` start with a full stream instead of an incremental that has no full file beside it. An unreadable or corrupt file counts as "nothing recorded" (full send, then rewritten).
2. **`plan()` is reused unchanged**: the recorded snapshot is presented to it as the only snapshot "on the other side" (`SnapInfo { name, guid, createtxg: 0 }`), which yields exactly the spec's rule (incremental only if the recorded snapshot still exists on the source by guid and is older than the new one; up to date if it is the newest).
3. **Updates of `replication.json` are serialized** with a process-wide mutex and written atomically, because two file jobs can finish at the same moment.
4. **Startup cleanup** removes `*.zfs.partial` from the `dest_dir` of every file job in `jobs.toml` (the daemon cannot know other directories).

## Review Focus

The spec implies these but a happy-path test would not exercise them; each has a test in the named task.

1. Disk full or an unwritable/impossible directory mid-run: no `.partial` left, nothing recorded — Tasks 1 (write error) and 2 (cannot create dir).
2. A source snapshot destroyed and recreated with the same name — must be a full stream, never an incremental from the wrong base — Task 2.
3. `dest_dir` edited, or `replication.json` corrupt — full stream, never an orphan incremental — Task 2.
4. Daemon killed mid-write — startup removes the stale `.partial` and nothing else in the directory — Task 3.
5. Two file jobs finishing together do not lose each other's `replication.json` entry — Task 2.

## File Structure

- Modify `src/jobs.rs` — `resolve` stops refusing file destinations (+ its test).
- Modify `src/daemon/transfer.rs` — `FileTransfer`, `pump_to_file` (+ tests).
- Create `src/daemon/replicate_file.rs` — state file, names, `run_replicate_file` (+ tests).
- Modify `src/daemon/replicate.rs` — `Env` gains `replication_state`; `message_of` becomes `pub(super)`; test rig updated.
- Modify `src/daemon/engine.rs` — `Paths::replication_state()`.
- Modify `src/daemon/mod.rs` — `mod replicate_file;`, `Shared.replication_state`, dispatch in `replicate()`, `clear_partial_files` at startup (+ tests).
- Modify `README.md`, and the spec's delivery note.

---

### Task 1: `resolve` accepts file destinations; `pump_to_file`

**Files:**
- Modify: `src/jobs.rs:196-203` (the `resolve` match) and its test `a_file_destination_is_rejected_by_this_build_with_a_clear_reason` (~line 603)
- Modify: `src/daemon/transfer.rs` (new type + fn above `#[cfg(test)]`; tests inside the existing `mod tests`, which already has the `sh(script)` helper)

**Interfaces:**
- Produces: `pub struct FileTransfer { pub bytes: u64, pub cancelled: bool, pub send_ok: bool, pub send_stderr: String, pub write_error: Option<String> }` and `pub fn pump_to_file(send: Command, file: std::fs::File, cancel: &AtomicBool, on_progress: impl FnMut(u64)) -> std::io::Result<FileTransfer>`.

- [ ] **Step 1: Create the branch**

```bash
git switch -c replication-file-targets
```

- [ ] **Step 2: Write the failing tests**

In `src/jobs.rs`, replace the test `a_file_destination_is_rejected_by_this_build_with_a_clear_reason` with:

```rust
    #[test]
    fn a_file_destination_is_accepted_and_still_needs_a_plain_absolute_directory() {
        let mut j = Job::test_replicate("r", "tank/data", "hourly", "file", "x");
        j.dest_dataset = None;
        j.dest_dir = Some("/mnt/usb/zfsctl".into());
        assert!(resolve(&j, &["local".to_string()]).is_ok());
        j.dest_dir = Some("relative/dir".into());
        assert!(resolve(&j, &["local".to_string()]).unwrap_err().contains("dest_dir"));
        j.dest_dir = Some("/a/../etc".into());
        assert!(resolve(&j, &["local".to_string()]).is_err());
    }
```

Append inside `mod tests` of `src/daemon/transfer.rs`:

```rust
    #[test]
    fn pump_to_file_writes_the_stream_counts_the_bytes_and_reports_a_clean_send() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out");
        let file = std::fs::File::create(&path).unwrap();
        let t = pump_to_file(sh("printf 'STREAMDATA'"), file, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!((t.bytes, t.cancelled, t.send_ok, t.write_error), (10, false, true, None));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "STREAMDATA");
    }

    #[test]
    fn pump_to_file_reports_a_failing_send_with_its_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("out")).unwrap();
        let t = pump_to_file(sh("echo oops >&2; printf abc; exit 3"), file, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(!t.send_ok && !t.cancelled && t.write_error.is_none());
        assert_eq!(t.bytes, 3);
        assert!(t.send_stderr.contains("oops"), "{}", t.send_stderr);
    }

    #[test]
    fn pump_to_file_kills_the_send_when_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("out")).unwrap();
        let cancel = AtomicBool::new(true);
        let started = Instant::now();
        let t = pump_to_file(sh("sleep 60"), file, &cancel, |_| {}).unwrap();
        assert!(t.cancelled && !t.send_ok);
        assert!(started.elapsed() < Duration::from_secs(10), "the sleep was killed, not waited for");
    }

    #[test]
    fn pump_to_file_stops_and_reports_a_write_error_such_as_a_full_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out");
        std::fs::write(&path, "").unwrap();
        let read_only = std::fs::File::open(&path).unwrap(); // every write to it fails, like ENOSPC
        let started = Instant::now();
        let t = pump_to_file(sh("head -c 1000000 /dev/zero; sleep 30"), read_only, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(t.write_error.is_some(), "{t:?}");
        assert!(!t.send_ok, "the send is killed once the write fails");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test pump_to_file a_file_destination 2>&1 | grep -E "^error" | head -3` (run the two filters separately if cargo rejects two).
Expected: compile error `cannot find function pump_to_file`; the jobs test would fail on "not supported".

- [ ] **Step 4: Implement**

In `src/jobs.rs`, in `resolve`, replace the match:

```rust
    if job.kind == KIND_REPLICATE {
        match job.dest_host.as_deref() {
            Some(DEST_FILE) => {}
            Some(dest) if !known_hosts.iter().any(|h| h == dest) => return Err(format!("destination host '{dest}' is not in config.toml")),
            _ => {}
        }
    }
```

In `src/daemon/transfer.rs`, after `pump` (before `#[cfg(test)]`), add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransfer {
    pub bytes: u64,
    pub cancelled: bool,
    pub send_ok: bool,
    pub send_stderr: String,
    /// A write or fsync error on the destination file (disk full, ...), if there was one.
    pub write_error: Option<String>,
}

/// Runs `send` and writes its stdout to `file` (fsynced at the end), counting the bytes: the hub-side
/// half of a replication to a stream file. `on_progress` is called about every 200 ms. Setting
/// `cancel` kills the child; a failed write stops the copy and kills it too.
pub fn pump_to_file(mut send: Command, mut file: std::fs::File, cancel: &AtomicBool, mut on_progress: impl FnMut(u64)) -> std::io::Result<FileTransfer> {
    send.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut sender = send.spawn()?;
    let Some(mut from) = sender.stdout.take() else {
        let _ = sender.kill();
        let _ = sender.wait();
        return Err(std::io::Error::other("could not open the pipe"));
    };
    let send_log = drain_stderr(&mut sender);

    let total_bytes = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&total_bytes);
    let copier = std::thread::spawn(move || -> Option<String> {
        let mut buf = vec![0_u8; BUFFER];
        loop {
            match from.read(&mut buf) {
                Ok(0) => break,
                Err(e) => return Some(format!("reading the stream failed: {e}")),
                Ok(n) => {
                    let Some(chunk) = buf.get(..n) else { break };
                    if let Err(e) = file.write_all(chunk) {
                        return Some(e.to_string());
                    }
                    counter.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
                }
            }
        }
        file.sync_all().err().map(|e| format!("syncing failed: {e}"))
    });

    let mut cancelled = false;
    while !copier.is_finished() {
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            let _ = sender.kill();
            break;
        }
        on_progress(total_bytes.load(Ordering::Relaxed));
        std::thread::sleep(TICK);
    }
    let write_error = copier.join().unwrap_or_else(|_| Some("the writer thread panicked".to_string()));
    on_progress(total_bytes.load(Ordering::Relaxed));
    if write_error.is_some() {
        // Nothing can be stored any more: whatever the send is still doing is pointless.
        let _ = sender.kill();
    }
    let send_status = sender.wait()?;
    Ok(FileTransfer {
        bytes: total_bytes.load(Ordering::Relaxed),
        cancelled,
        send_ok: send_status.success(),
        send_stderr: send_log.join().unwrap_or_default(),
        write_error,
    })
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"`
Expected: all pass (1019 + 4 new `pump_to_file` tests = 1023; the jobs test replaces the old one). Then `cargo clippy 2>&1 | tail -3`: `pump_to_file`/`FileTransfer` show dead-code warnings until Task 2 uses them; that is expected.

- [ ] **Step 6: Commit**

```bash
git add src/jobs.rs src/daemon/transfer.rs
git commit -m "feat: file destinations pass validation; pump_to_file

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: The file run

**Files:**
- Create: `src/daemon/replicate_file.rs`
- Modify: `src/daemon/mod.rs` (add `pub mod replicate_file;` next to `pub mod replicate;` at line ~11)
- Modify: `src/daemon/replicate.rs` — `Env` struct (add field), `message_of` visibility, the test `Rig::run`'s `Env { .. }` literal.

**Interfaces:**
- Consumes: `transfer::pump_to_file`/`FileTransfer` (Task 1); existing `scrub::{event, finish, Slot, Outcome}`, `progress::{ProgressWriter, remove, human_bytes}`, `zfs::replication::{list_snapshot_infos, parse_send_size, plan, send_args, Plan, SnapInfo}`, `jobs::effective_prefix`.
- Produces: `Env.replication_state: &Path` (path of `replication.json`); `pub struct Sent { pub snapshot: String, pub guid: u64, pub dir: String }`; `pub fn read_state(&Path) -> BTreeMap<String, Sent>`; `pub fn record(&Path, job_id: &str, Sent) -> std::io::Result<()>`; `pub fn stream_name(dataset: &str, new: &str, base: Option<&str>) -> String`; `pub fn run_replicate_file(env: &Env, src: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome`.

- [ ] **Step 1: Wire `Env` and write the failing tests**

In `src/daemon/replicate.rs`: add to `pub struct Env<'a>` the field

```rust
    /// `replication.json`: what each file-target job last wrote.
    pub replication_state: &'a Path,
```

change `fn message_of` to `pub(super) fn message_of`, and in the test `Rig::run` change the `Env` literal to

```rust
            let state = self.dir.path().join("replication.json");
            let env = Env { ctx: &ctx, progress_dir: &progress, cancel: &self.cancel, replication_state: &state };
```

In `src/daemon/mod.rs` add `pub mod replicate_file;` after `pub mod replicate;`, and at the one place that builds `replicate::Env` (line ~135, in `fn replicate`) add `replication_state: &self.replication_state,`. Add to `struct Shared` the field `replication_state: PathBuf,` (doc: `/// What each file-target replication last wrote.`), and set it where `Shared` is built: in `run()` `replication_state: paths.replication_state(),` (put it next to `progress_dir: paths.progress_dir(),` BEFORE `paths` is moved into `Engine::new`) and in the test helper `shared()` `replication_state: dir.path().join("replication.json"),`. In `src/daemon/engine.rs` add next to `progress_dir`:

```rust
    /// What each file-target replication last wrote.
    pub fn replication_state(&self) -> PathBuf {
        self.dir.join("replication.json")
    }
```

Create `src/daemon/replicate_file.rs` containing only this test module (plus `use super::*;` is not available yet — the file starts with the imports listed in Step 3; for the RED run put just `#![allow(unused)]`-free minimal stubs: simply create the file with the test module below and the `use` lines from Step 3, and the run fails to compile because the items do not exist):

```rust
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
        commands: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl Host {
        fn new(listing: &str) -> Self {
            Host { listing: listing.into(), send_script: "printf 'STREAMDATA'".into(), commands: std::sync::Mutex::new(vec![]) }
        }
        fn commands(&self) -> Vec<Vec<String>> {
            self.commands.lock().unwrap().clone()
        }
    }

    impl CommandRunner for Host {
        fn run(&self, _cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            let ok = |stdout: &str| Ok(CommandOutput { stdout: stdout.to_string(), stderr: String::new(), status: 0 });
            match args.first().copied() {
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
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test replicate_file 2>&1 | grep -E "^error" | sort | uniq -c | head -5`
Expected: compile errors (`stream_name`, `read_state`, `record`, `Sent`, `run_replicate_file` not found).

- [ ] **Step 3: Implement** — put this above the test module in `src/daemon/replicate_file.rs`:

```rust
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
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
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

    let source = match list_snapshot_infos(src, &dataset) {
        Ok(s) => s,
        Err(e) => return failed(e.to_string()),
    };
    // The recorded snapshot stands in for "what the other side has": `plan` then gives exactly the
    // rule we want (an increment only from a snapshot that still exists on the source, by guid).
    let recorded = read_state(env.replication_state).remove(&job.id).filter(|s| s.dir == dir);
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
    let file = match std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&partial) {
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
    finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, format!("wrote {name} ({label}), {}", progress::human_bytes(transfer.bytes)), slot)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"`
Expected: all pass. `cargo clippy 2>&1 | tail -3`: only the daemon wiring (Task 3) is missing, so `run_replicate_file` may warn as unused until then.

- [ ] **Step 5: Commit**

```bash
git add src/daemon
git commit -m "feat: replication to stream files on the hub

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Daemon dispatch and startup cleanup

**Files:**
- Modify: `src/daemon/mod.rs` — `fn replicate` (~line 116), `run()` (~line 80 `clear_runtime_files(&paths)`), new `clear_partial_files`, tests.

**Interfaces:**
- Consumes: `replicate_file::run_replicate_file`, `Shared.replication_state` (added in Task 2), `crate::jobs::{load, Job, KIND_REPLICATE, DEST_FILE}`.
- Produces: `fn clear_partial_files(jobs: &[crate::jobs::Job])`.

- [ ] **Step 1: Write the failing tests** — append in `mod tests` of `src/daemon/mod.rs`:

```rust
    fn file_job(id: &str, dir: &Path) -> Job {
        let mut j = Job::test_replicate(id, "tank/data", "hourly", "file", "x");
        j.dest_dataset = None;
        j.dest_dir = Some(dir.display().to_string());
        j
    }

    #[test]
    fn a_due_file_replication_needs_no_destination_host() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let due = Due { job: file_job("repl-file", &dir.path().join("out")), scheduled_for: shared.clock.now(), catch_up: false };
        shared.execute(Work::Run(due));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Skipped)), "{:?}", runs[0].message);
        assert!(runs[0].message.contains("no snapshots"), "it reached the file run: {}", runs[0].message);
    }

    #[test]
    fn a_due_file_replication_from_an_unconfigured_source_host_is_recorded_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let mut job = file_job("repl-file", &dir.path().join("out"));
        job.host = "gone".into();
        shared.execute(Work::Run(Due { job, scheduled_for: shared.clock.now(), catch_up: false }));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Failed)));
        assert!(runs[0].message.contains("gone"), "{}", runs[0].message);
    }

    #[test]
    fn startup_removes_partial_stream_files_of_file_jobs_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("tank-data@s2.zfs.partial"), "half").unwrap();
        std::fs::write(out.join("tank-data@s1.zfs"), "complete").unwrap();
        std::fs::write(out.join("notes.partial"), "not ours").unwrap();
        let host_to_host = Job::test_replicate("h", "tank/data", "hourly", "fbsd16", "backup/data");
        clear_partial_files(&[file_job("f", &out), host_to_host]);
        assert!(!out.join("tank-data@s2.zfs.partial").exists());
        assert!(out.join("tank-data@s1.zfs").exists() && out.join("notes.partial").exists());
    }

    #[test]
    fn a_missing_file_directory_is_not_an_error_at_startup() {
        clear_partial_files(&[file_job("f", Path::new("/nonexistent/zfsctl-test-dir"))]);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test file_replication 2>&1 | grep -E "^error|FAILED" | head`; `cargo test startup_removes_partial 2>&1 | grep -E "^error" | head -3`
Expected: the first fails (the dispatch treats `file` as an unconfigured host: message `... or 'file' is not configured`), the cleanup tests do not compile.

- [ ] **Step 3: Implement**

Replace the first half of `fn replicate` (up to where `flag` is created) with:

```rust
    fn replicate(&self, ctx: &Context, due: &scheduler::Due, run_id: &str) -> Outcome {
        let job = &due.job;
        let dest_host = job.dest_host.clone().unwrap_or_default();
        let to_file = dest_host == crate::jobs::DEST_FILE;
        let src = self.runner(&job.host);
        let dst = if to_file { None } else { self.runner(&dest_host) };
        let Some(src) = src.filter(|_| to_file || dst.is_some()) else {
            let slot = scrub::Slot { scheduled_for: Some(due.scheduled_for), catch_up: due.catch_up };
            let which = if to_file { format!("host '{}' is", job.host) } else { format!("host '{}' or '{dest_host}' is", job.host) };
            return scrub::finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, format!("{which} not configured"), slot);
        };
```

and replace the line that calls `replicate::run_replicate` with:

```rust
        let outcome = match &dst {
            Some(dst) => replicate::run_replicate(&env, src.as_ref(), dst.as_ref(), job, due.scheduled_for, due.catch_up, run_id),
            None => replicate_file::run_replicate_file(&env, src.as_ref(), job, due.scheduled_for, due.catch_up, run_id),
        };
```

Add next to `clear_runtime_files`:

```rust
/// A `.partial` stream file can only belong to a write of a previous daemon. The daemon knows the
/// directories from `jobs.toml`; nothing but `*.zfs.partial` is touched.
fn clear_partial_files(jobs: &[crate::jobs::Job]) {
    for job in jobs.iter().filter(|j| j.kind == crate::jobs::KIND_REPLICATE && j.dest_host.as_deref() == Some(crate::jobs::DEST_FILE)) {
        let Some(dir) = job.dest_dir.as_deref() else { continue };
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().ends_with(".zfs.partial")) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
```

and in `run()` right after `clear_runtime_files(&paths);` add:

```rust
    if let Ok(jobs) = crate::jobs::load(&paths.jobs()) {
        clear_partial_files(&jobs);
    }
```

- [ ] **Step 4: Run everything**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"; cargo clippy 2>&1 | tail -2`
Expected: all pass; clippy back to exactly the 1 pre-existing warning.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/mod.rs
git commit -m "feat: daemon runs file replications; startup removes stale .partial files

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Docs and the live checks

**Files:**
- Modify: `README.md` (the Replication subsection, ~line 287-306)
- Modify: `docs/superpowers/specs/2026-10-06-replication-design.md` (append live results; ruling on `replication.json` shape)

- [ ] **Step 1: README.** In the replication paragraph replace the last bullet's final sentence ``... `dest_host = "file"` (stream files in a hub directory) is not supported yet.`` with nothing, and add after the **Rules** bullet:

````markdown
**A file target** writes the stream to files in a directory on the hub instead of receiving it anywhere:

```toml
[[jobs]]
id = "repl-tank-data-to-files"
host = "local"
kind = "replicate"
dataset = "tank/data"
prefix = "hourly"
dest_host = "file"
dest_dir = "/mnt/backup/zfs-streams"   # absolute, on the hub
schedule = "30 3 * * *"
```

- Each run writes `tank-data@<snap>.zfs` (a full stream) or `tank-data@<from>..<to>.zfs` (an increment), the dataset name with `/` replaced by `-`. The first run, a run after the recorded snapshot was pruned or recreated on the source, one after `dest_dir` was changed, and one after `replication.json` was lost are full streams; otherwise it is an increment from the snapshot the previous run wrote. `<state>/replication.json` records what each job last wrote (snapshot, guid and directory); it changes only after a file is complete.
- A file is written as `<name>.partial`, fsynced and renamed when complete, so a file without `.partial` is a complete stream. A failed or cancelled run removes its `.partial` (the daemon also removes leftovers at startup, only `*.zfs.partial` in the jobs' directories). File targets cannot resume: the next run starts again. The directory is created private (mode 0700) and so are the files (0600); nothing is created, destroyed or rolled back on the source, and no remote host needs a new sudoers entry.
- **Restoring.** The files are plain `zfs send` streams. Receive the full file, then each increment in order, into a dataset that does not exist yet: `zfs receive pool/restored < tank-data@hourly-A.zfs`, then `zfs receive pool/restored < tank-data@hourly-A..hourly-B.zfs`, and so on. There is no UI for restoring, and `zfsctl` never deletes old stream files: prune them yourself, keeping every increment back to a full stream you still want to restore from.
````

- [ ] **Step 2: Live checks on the hub** (deploy per [[project-zfsctl-freebsd-test-box]]: `scp` the changed `src/` files and `README.md` to `fb@172.16.10.90:/home/fb/zfsctl/`, then `cd /home/fb/zfsctl && cargo test && cargo build --release`; the daemon runs as `service zfsctl`, so `sudo service zfsctl restart` picks up the new binary — check how the rc script finds the binary first with `grep -n command /usr/local/etc/rc.d/zfsctl`). Use only throwaway datasets (`scratch/fsrc`, `scratch/frestore`; the `scratch` pool has ~18 MB free) and the directory `/var/tmp/zit-streams`:
  1. Sanity first: `zfs send -nvP`, `zfs list -t snapshot -H -p -o name,guid,createtxg -d 1 scratch/fsrc` print what the code parses.
  2. `zfs create scratch/fsrc; dd if=/dev/urandom of=/scratch/fsrc/d1 bs=1m count=3; zfs snapshot scratch/fsrc@hourly-20261008T100000Z`. In the TUI (Jobs, `R`): host `local`, dataset `scratch/fsrc`, prefix `hourly`, destination host `file`, directory `/var/tmp/zit-streams`, schedule `* * * * *`. Within about two minutes: `ls -l /var/tmp/zit-streams` shows `scratch-fsrc@hourly-…Z.zfs` of roughly 3 MB and no `.partial`; `cat <state>/replication.json` shows the entry; the Jobs table shows the Ok result (its Target column shows only the source, a known deferred minor).
  3. `dd ... of=/scratch/fsrc/d2 count=1; zfs snapshot scratch/fsrc@hourly-20261008T110000Z`: the next run writes `scratch-fsrc@hourly-…A..hourly-…B.zfs` (about 1 MB). Wait one more minute: `skipped: up to date`.
  4. **Restore:** `zfs receive scratch/frestore < full`, then `zfs receive scratch/frestore < increment`; `sha256 /scratch/fsrc/d1 /scratch/fsrc/d2 /scratch/frestore/d1 /scratch/frestore/d2` must match pairwise.
  5. Destroy and recreate the newest snapshot under the same name with different content (`zfs destroy`, change a file, `zfs snapshot` again): the next run writes a *full* stream (guid changed), not an increment.
  6. **Cancel mid-transfer through the real SSH runner** (the only check that exercises `pump_to_file` over `ssh ... sudo -n zfs send`): on `fbsd16t1` (`fbsd@172.16.10.61`, 4.6 GB free on `zroot`) create `zroot/zit-big` and write about 600 MB of incompressible data into it as `fbsd` (the sudoers there does not allow `sh`: run `ssh ... sudo zfs create`, then `dd if=/dev/urandom of=/zroot/zit-big/x bs=1m count=600` needs write access, so `sudo chown fbsd /zroot/zit-big` first), snapshot it with the `hourly-` prefix, add a file job for host `fbsd16t1` to `/var/tmp/zit-streams`, and press `c` in Jobs while the progress line shows it moving. Expect: `cancelled`, no `.partial` left, nothing new in `replication.json`; the next run completes and the file's size equals the dry-run estimate printed by `zfs send -nvP` (within the stream header). `df /var/tmp` stays above 20 GB free.
  7. Kill the daemon mid-write (`sudo service zfsctl stop` while step 6's second run is in progress, or `kill -9` it): the `.partial` is removed by the next start, the interrupted run is closed `failed: lost`, and the run after that succeeds.
  8. Clean up everything: disable and delete the test jobs from the TUI, `zfs destroy -r scratch/fsrc scratch/frestore`, destroy `zroot/zit-big` on the VM, `rm -r /var/tmp/zit-streams`, and remove the test entries from `replication.json`.

- [ ] **Step 3: Record results in the spec.** Append a `### File targets (live, 2026-10-08)` subsection under the spec's live-results section with what steps 2-7 showed (sizes, messages, hash comparison), and add to the spec's "A file run" section a note: "`replication.json` stores `{snapshot, guid, dir}` per job (planning ruling: a name alone cannot detect a recreated snapshot or an edited directory)."

- [ ] **Step 4: Commit, whole-suite check and finish**

```bash
cargo test 2>&1 | grep -E "^test result|FAILED"
git add README.md docs
git commit -m "docs: replication to stream files; live results

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

Then one fresh whole-branch review on the most capable model (the plan's Review Focus verbatim), a single fix pass for Critical/Important findings, update the memory notes, and merge `replication-file-targets` into `master` with `--no-ff` only after the user chooses to (superpowers:finishing-a-development-branch).
