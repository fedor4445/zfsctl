# Replication Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `replicate` job kind that copies the newest snapshot with a prefix from a dataset on one configured host to a dataset on another (`zfs send | zfs receive`), incrementally, resumably, with progress in the Jobs section and a cancel key.

**Architecture:** `CommandRunner` hands out an unspawned `Command`; the hub spawns `zfs send` (source runner) and `zfs receive -s -u` (destination runner) and a pump thread copies the bytes, counting them for a progress file in the state dir. Selection of what to send is a pure function over parsed `zfs list` output. Cancel is a request file the daemon's one-second loop picks up. The job is flat optional fields on `jobs::Job`, dispatched by `kind` next to scrub and snapshot.

**Tech Stack:** Rust, ratatui, chrono, serde/toml/serde_json. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-06-replication-design.md` (this is plan 1 of 2; file targets are plan 2, so this plan rejects `dest_host = "file"` in `resolve` with a clear reason that plan 2 removes).

## Global Constraints

- Only `/sbin/zfs` is used on managed hosts: no new sudoers entries, no `sh -c` anywhere in replication.
- The receive is always `zfs receive -s -u [-F] -- <dest>` (resumable, never mounted); `-F` only when the job's `force` is true.
- Replication never creates, destroys or rolls back a snapshot on either side.
- The snapshot sent is the newest whose name is exactly `<prefix>-<YYYYMMDDTHHMMSSZ>` (`daemon::snapshot::is_own`); the incremental base is the newest snapshot present on both sides **by guid**.
- A cancelled run is recorded with `RunResult::Cancelled`, which the failed-jobs indicator and the notify hook ignore.
- Progress files live in `<state>/progress/<run_id>.json`, cancel requests in `<state>/cancel/<name>` where `<name>` is the job id with every character outside `A-Za-z0-9_.-` replaced by `-`.
- `jobs.toml` with scrub and snapshot jobs only must serialize exactly as before (new fields skipped when unset).
- Match the surrounding code (comment density, naming). `cargo clippy --all-targets` must not gain warnings beyond the 4 pre-existing dead-code ones (it is strict: indexing/slicing, `format!` in iterators, casts; fix what it reports in new code). Task 9 removes three of those four.
- Every code step is TDD: failing test first, watch it fail for the right reason, then minimal code.

## Review Focus

- A hand-edited `jobs.toml` replicate job with a dataset, `dest_dataset` or `dest_dir` like `-rf`, `tank/a@b`, `../x`, a relative path, or with spaces/`;` must be *unsupported*, never passed to `zfs` or opened as a path. Pinned in Task 3.
- Source snapshot pruned between runs: the base must be chosen by guid among what both sides still have (a prune costs a longer incremental or a full send, never a wrong one); a destination with snapshots but no common one fails with zfs's message rather than being force-overwritten. Pinned in Task 4.
- A destination that has changed since the base fails with zfs's own message and is not rolled back unless `force`. Pinned in Task 6.
- Cancel mid-transfer kills **both** children (no stray `zfs send` left reading), records `Cancelled`, does not notify, does not light the failed-jobs indicator, and the next run resumes from the token. Pinned in Tasks 5, 6, 1.
- A daemon killed mid-transfer: the unfinished run is closed as lost (never resumed in place), stale progress and cancel files are removed at startup. Pinned in Task 7.
- Receive exits early (bad destination) while send is still producing: the run must fail promptly with the receive's message, not hang with `zfs send` blocked on a full pipe. Pinned in Task 5.

---

### Task 0: Verify the zfs behaviour live

**Files:**
- Modify: `docs/superpowers/specs/2026-10-06-replication-design.md` (append `### Live results` under Testing)

Everything in Tasks 4-6 assumes the output formats and flag behaviour below. They were taken from OpenZFS documentation, not observed on these hosts, and the spec says to check them first (flags have bitten this project before). Do this before writing code.

- [ ] **Step 1: Run on the hub** (15.1-RELEASE; `scratch` is the disposable file-backed pool; check `sudo zpool list -H -o name,free scratch` shows at least 1 GiB free first):

  ```sh
  ssh fb@172.16.10.90 'bash -s' <<'EOF'
  S=scratch/repl-src; D=scratch/repl-dst
  A=hourly-20261006T100000Z; B=hourly-20261006T110000Z; C=hourly-20261006T120000Z
  sudo zfs create $S
  sudo dd if=/dev/urandom of=/$S/big bs=1m count=100 2>/dev/null
  sudo zfs snapshot $S@$A
  echo "--- send -nvP (stdout):";  sudo zfs send -nvP -- $S@$A 2>/dev/null
  echo "--- send -nvP (stderr):";  sudo zfs send -nvP -- $S@$A 2>&1 >/dev/null
  echo "--- list guid:";           sudo zfs list -t snapshot -H -p -o name,guid,createtxg -d 1 $S
  echo "--- token on a missing dataset:"; sudo zfs get -H -p -o value receive_resume_token $D 2>&1
  echo "--- list on a missing dataset:";  sudo zfs list -t snapshot -H -p -o name,guid,createtxg -d 1 $D 2>&1
  echo "--- full receive:";        sudo sh -c "zfs send -- $S@$A | zfs receive -s -u -- $D"
  echo "--- token / mounted after:"; sudo zfs get -H -p -o value receive_resume_token,mounted $D
  sudo dd if=/dev/urandom of=/$S/big2 bs=1m count=100 2>/dev/null
  sudo zfs snapshot $S@$B
  echo "--- incremental -nvP:";    sudo zfs send -nvP -i @$A -- $S@$B 2>/dev/null
  echo "--- interrupted receive:"; sudo sh -c "zfs send -i @$A -- $S@$B | head -c 30000000 | zfs receive -s -u -- $D" 2>&1
  echo "--- token after:";         sudo zfs get -H -p -o value receive_resume_token $D
  T=$(sudo zfs get -H -p -o value receive_resume_token $D)
  echo "--- send -t -nvP:";        sudo zfs send -nvP -t "$T" 2>/dev/null
  echo "--- resume:";              sudo sh -c "zfs send -t $T | zfs receive -s -u -- $D"
  echo "--- token / snapshots after resume:"; sudo zfs get -H -p -o value receive_resume_token $D; sudo zfs list -t snapshot -H -o name -d 1 $D
  echo "--- modified destination:"
  sudo zfs snapshot $D@extra
  sudo dd if=/dev/urandom of=/$S/big3 bs=1m count=10 2>/dev/null
  sudo zfs snapshot $S@$C
  sudo sh -c "zfs send -i @$B -- $S@$C | zfs receive -s -u -- $D" 2>&1
  echo "--- discarding a token (-A) on a dataset with none:"; sudo zfs receive -A $D 2>&1
  sudo zfs destroy -r $S; sudo zfs destroy -r $D
  echo cleaned
  EOF
  ```
  Record, for each `---` block, the exact text printed. Expected (OpenZFS): `send -nvP` prints `full\t<snap>\t<bytes>` and `size\t<bytes>` lines on **stdout** (the `-v` chatter goes to stderr); the `-i @name -- dataset@snap` form is accepted; `zfs list ... name,guid,createtxg` prints three tab-separated fields; a missing dataset says `dataset does not exist`; the token is `-` when there is none, a long string after the interrupted receive; `mounted` is `no` after `-u`; the stale-destination receive fails with `cannot receive incremental stream: destination ... has been modified since most recent snapshot`.

- [ ] **Step 2: Repeat on the 16.0-CURRENT VM** (`fbsd@172.16.10.60`; it has only `zroot`, so use `S=zroot/repl-src; D=zroot/repl-dst` and the mountpoint `sudo zfs get -H -o value mountpoint $S` instead of `/$S` in the `dd` lines).

- [ ] **Step 3: Reconcile.** If anything differs from the Expected lines above (flag order, `--` rejected after `-i`, the size line format, error wording, token format), **stop and change the affected code in Tasks 4-6 to match what was observed** (`parse_send_size`, `send_args`, the `contains("does not exist")` checks, the test fixtures) and say so in the ledger. Append the observed outputs (trimmed) to the spec under `### Live results`, and commit (`docs: live results for the zfs send/receive behaviour replication relies on`).

---

### Task 1: `RunResult::Cancelled`

**Files:**
- Modify: `src/daemon/history.rs` (enum)
- Modify: `src/ui/jobs.rs` (`run_summary`, `last_result_text`; tests)
- Modify: `src/daemon/mod.rs` (test only)

**Interfaces:**
- Produces: `RunResult::Cancelled` (serde `"cancelled"`). `failed_count`, `Shared::notify` and the failed-jobs indicator are unchanged and therefore ignore it.

- [ ] **Step 1: Write the failing tests.** In `src/ui/jobs.rs` tests (use the module's existing `event`/`runs` helpers and `at`-style time helper; model on `last_result_text_covers_never_running_ok_failed_and_skipped`):

  ```rust
  #[test]
  fn a_cancelled_run_reads_as_cancelled_and_is_not_a_failure() {
      let dir = tempfile::tempdir().unwrap();
      jobs::save(&dir.path().join("jobs.toml"), &[job("a")]).unwrap();
      let h = History::new(dir.path().join("history.jsonl"));
      h.append(&event("a", "r1", EventKind::Finished, "2026-10-06T09:30:00Z", Some(RunResult::Cancelled))).unwrap();
      let view = load(dir.path(), &hosts(), None);
      assert_eq!(view.failed_count(), 0, "a deliberate cancel is not a failure");
      let run = view.last_run("a").unwrap();
      assert!(last_result_text(Some(run), at("2026-10-06T09:35:00Z")).starts_with("cancelled"));
      assert!(run_summary(run, &Utc).contains("cancelled"));
  }
  ```
  (`event`, `at` and `hosts()` are the test module's existing helpers; `job("a")` there builds a scrub job, so use it instead of `Job::test_snapshot` if the `unsupported` map matters. `run_summary` takes a time zone: pass `&Utc`.) In `src/daemon/mod.rs` tests, next to `a_skipped_run_does_not_notify`:

  ```rust
  #[test]
  fn a_cancelled_run_does_not_notify() {
      let dir = tempfile::tempdir().unwrap();
      let out = dir.path().join("notified");
      let shared = shared(&dir, FakeRunner::new(""), Some(format!("touch {}", out.display())));
      let outcome = Outcome { job_id: "j".into(), result: RunResult::Cancelled, message: "cancelled".into() };
      shared.notify(&job(), &outcome);
      assert!(!out.exists());
  }
  ```

- [ ] **Step 2: Run to see them fail:** `cargo test cancelled 2>&1 | grep -E "^error" -A4` — Expected: `no variant named Cancelled`.

- [ ] **Step 3: Implement.** In `src/daemon/history.rs` add `Cancelled,` after `Skipped,` in `RunResult`. In `src/ui/jobs.rs` add `Some(RunResult::Cancelled) => "cancelled",` to `run_summary`'s match and `RunResult::Cancelled => "cancelled",` to `last_result_text`'s match. (`history.rs:95` `e.result.or(Some(RunResult::Ok))` stays.)

- [ ] **Step 4: Run:** `cargo test 2>&1 | grep -E "test result|FAILED|panicked"` — Expected: all pass. Commit (`feat: a Cancelled run result that is not a failure`, with the co-author trailer used throughout this repo).

---

### Task 2: `CommandRunner::command`

**Files:**
- Modify: `src/exec/mod.rs`, `src/exec/local.rs`, `src/exec/ssh.rs`

**Interfaces:**
- Produces:
  ```rust
  // trait CommandRunner
  fn command(&self, cmd: &str, args: &[&str]) -> Result<std::process::Command>;
  // default: Err(AppError::Config("this runner cannot hand out a command to spawn")).
  // LocalRunner: Command::new(cmd).args(args). SshRunner: Command::new("ssh").args(self.build_args(cmd, args)).
  ```

- [ ] **Step 1: Write the failing tests.** `src/exec/local.rs` tests:

  ```rust
  #[test]
  fn command_is_the_unspawned_command_itself() {
      let cmd = LocalRunner.command("echo", &["hi"]).unwrap();
      assert_eq!(cmd.get_program(), "echo");
      assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["hi"]);
  }
  ```
  `src/exec/ssh.rs` tests:

  ```rust
  #[test]
  fn command_is_ssh_with_the_same_arguments_and_quoting_as_run() {
      let runner = SshRunner::new("backup-host");
      let cmd = runner.command("zfs", &["send", "--", "tank/a b@s"]).unwrap();
      assert_eq!(cmd.get_program(), "ssh");
      let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
      assert_eq!(args, runner.build_args("zfs", &["send", "--", "tank/a b@s"]));
      assert!(args.last().unwrap().starts_with("sudo -n zfs send"));
  }
  ```
  And in `src/exec/mod.rs` tests (the `FakeRunner` keeps the default): `assert!(FakeRunner::new("").command("zfs", &[]).is_err());`.

- [ ] **Step 2: Run to see them fail:** `cargo test command_is 2>&1 | tail` — Expected: `no method named command`.

- [ ] **Step 3: Implement.** In the trait (add `use crate::error::AppError;` to `exec/mod.rs`):

  ```rust
      /// The command this runner would run, unspawned, so a caller can wire its own pipes
      /// (replication pipes one host's `zfs send` into another's `zfs receive`). The default
      /// refuses, so a runner that cannot do this (a test fake) says so rather than guessing.
      fn command(&self, cmd: &str, args: &[&str]) -> Result<std::process::Command> {
          let _ = (cmd, args);
          Err(AppError::Config("this runner cannot hand out a command to spawn".to_string()))
      }
  ```
  `LocalRunner`: `fn command(&self, cmd: &str, args: &[&str]) -> Result<Command> { let mut c = Command::new(cmd); c.args(args); Ok(c) }`. `SshRunner`: `fn command(...) -> Result<Command> { let mut c = Command::new("ssh"); c.args(self.build_args(cmd, args)); Ok(c) }`.

- [ ] **Step 4: Run:** `cargo test 2>&1 | grep -E "test result|FAILED"` — Expected: pass. Commit (`feat: CommandRunner::command hands out an unspawned command`).

---

### Task 3: The replicate job model and validation

**Files:**
- Modify: `src/jobs.rs`; every `Job { .. }` literal under `src/` (four new fields)

**Interfaces:**
- Produces:
  ```rust
  pub const KIND_REPLICATE: &str = "replicate";
  pub const DEST_FILE: &str = "file";
  // Job gains: pub dest_host: Option<String>, pub dest_dataset: Option<String>, pub dest_dir: Option<String>, pub force: bool
  pub fn valid_dest_dir(s: &str) -> bool;
  pub fn clash_reason(earlier: &Job, job: &Job) -> Option<String>;   // snapshot-prune clash or shared destination
  impl Job { #[cfg(test)] pub fn test_replicate(id: &str, dataset: &str, prefix: &str, dest_host: &str, dest_dataset: &str) -> Job }
  ```
  `resolve`, `clashing_job` and `duplicate_prefix_reasons` handle the new kind; their signatures are unchanged.

- [ ] **Step 1: Write the failing tests** in `src/jobs.rs`'s test module:

  ```rust
  #[test]
  fn a_replicate_job_round_trips_and_other_kinds_serialize_without_its_fields() {
      let dir = tempfile::tempdir().unwrap();
      let path = dir.path().join("jobs.toml");
      let mut repl = Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data");
      repl.force = true;
      let snap = Job::test_snapshot("s", "tank/data", "hourly", 3);
      save(&path, &[repl.clone(), snap.clone()]).unwrap();
      assert_eq!(load(&path).unwrap(), vec![repl, snap.clone()]);
      let text = toml::to_string_pretty(&JobFile { jobs: vec![snap, scrub("x")] }).unwrap();
      for absent in ["dest_host", "dest_dataset", "dest_dir", "force"] {
          assert!(!text.contains(absent), "{absent} leaked:\n{text}");
      }
  }

  #[test]
  fn resolve_accepts_a_good_replicate_job_and_rejects_each_bad_field() {
      let hosts = vec!["local".to_string(), "fbsd16".to_string()];
      assert!(resolve(&Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data"), &hosts).is_ok());
      let bad = |f: &dyn Fn(&mut Job)| {
          let mut j = Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data");
          f(&mut j);
          resolve(&j, &hosts).unwrap_err()
      };
      assert!(bad(&|j| j.dataset = None).contains("source dataset"));
      for evil in ["-rf", "tank/a@b", "tank/a b", "tank;x", "/tank"] {
          assert!(bad(&|j| j.dataset = Some(evil.into())).contains("source dataset"), "{evil}");
          assert!(bad(&|j| j.dest_dataset = Some(evil.into())).contains("destination dataset"), "{evil}");
      }
      assert!(bad(&|j| j.prefix = Some("a b".into())).contains("prefix"));
      assert!(bad(&|j| j.dest_host = None).contains("destination host"));
      assert!(bad(&|j| j.dest_host = Some("nope".into())).contains("host"));
      assert!(bad(&|j| j.dest_dataset = None).contains("destination dataset"));
      assert!(bad(&|j| j.dest_dir = Some("/mnt/x".into())).contains("dest_dir"));
      assert!(bad(&|j| { j.host = "fbsd16".into(); j.dest_dataset = j.dataset.clone(); }).contains("same dataset"), "source and destination must differ");
      assert!(bad(&|j| j.schedule = "x".into()).contains("schedule"));
  }

  #[test]
  fn a_file_destination_is_rejected_by_this_build_with_a_clear_reason() {
      let mut j = Job::test_replicate("r", "tank/data", "hourly", "file", "x");
      j.dest_dataset = None;
      j.dest_dir = Some("/mnt/usb/zfsctl".into());
      let reason = resolve(&j, &["local".to_string()]).unwrap_err();
      assert!(reason.contains("not supported"), "{reason}");
  }

  #[test]
  fn dest_dir_must_be_a_plain_absolute_path() {
      for good in ["/mnt/usb", "/mnt/usb/zfsctl", "/var/backups/my disk"] {
          assert!(valid_dest_dir(good), "{good}");
      }
      for bad in ["", "/", "mnt/usb", "-x", "/mnt/../etc", "/mnt/..", "/mnt/a;b", "/mnt/a\nb", "/mnt/$x", "/mnt/a\\b"] {
          assert!(!valid_dest_dir(bad), "{bad:?}");
      }
  }

  #[test]
  fn two_enabled_replicate_jobs_with_one_destination_clash_and_other_destinations_do_not() {
      let a = Job::test_replicate("a", "tank/one", "hourly", "fbsd16", "backup/data");
      let b = Job::test_replicate("b", "tank/two", "hourly", "fbsd16", "backup/data");
      let other_dataset = Job::test_replicate("c", "tank/two", "hourly", "fbsd16", "backup/other");
      let other_host = Job::test_replicate("d", "tank/two", "hourly", "local", "backup/data");
      let reasons = duplicate_prefix_reasons(&[a.clone(), b.clone(), other_dataset.clone(), other_host.clone()]);
      assert_eq!(reasons.len(), 1, "{reasons:?}");
      assert!(reasons["b"].contains("same destination") && reasons["b"].contains("'a'"), "{reasons:?}");
      assert_eq!(clashing_job(&[a.clone()], &b).map(|j| j.id.as_str()), Some("a"));
      assert!(clashing_job(&[a.clone()], &other_dataset).is_none());
      assert!(clash_reason(&a, &b).unwrap().contains("same destination"));
      let snap = Job::test_snapshot("s", "tank/one", "hourly", 3);
      assert!(clash_reason(&a, &snap).is_none(), "different kinds never clash");
  }
  ```

- [ ] **Step 2: Run to see them fail:** `cargo test jobs:: 2>&1 | grep -E "^error" -A3 | head` — Expected: `test_replicate`, `valid_dest_dir`, `KIND_REPLICATE` not found.

- [ ] **Step 3: Implement** in `src/jobs.rs`.

  Constants after `DEFAULT_PREFIX`:
  ```rust
  pub const KIND_REPLICATE: &str = "replicate";
  /// `dest_host` value meaning "write stream files to `dest_dir` on the hub".
  pub const DEST_FILE: &str = "file";
  ```
  `Job` fields after `keep`:
  ```rust
      /// A replication job's destination: a configured host name, or `file`.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub dest_host: Option<String>,
      /// The destination dataset on `dest_host`.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub dest_dataset: Option<String>,
      /// For `dest_host = "file"`: an absolute directory on the hub.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub dest_dir: Option<String>,
      /// Receive with `-F`, rolling the destination back to the incremental base.
      #[serde(default, skip_serializing_if = "std::ops::Not::not")]
      pub force: bool,
  ```
  Validation helpers (next to `valid_prefix`):
  ```rust
  /// An absolute hub directory with no `..` component and only plain path characters.
  pub fn valid_dest_dir(s: &str) -> bool {
      s.len() > 1 && s.starts_with('/') && s.split('/').all(|c| c != "..") && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ' '))
  }

  fn check_replicate_fields(job: &Job) -> std::result::Result<(), String> {
      if !job.dataset.as_deref().is_some_and(valid_dataset) {
          return Err("a replication job needs a source dataset like tank/data".to_string());
      }
      if !valid_prefix(effective_prefix(job)) {
          return Err("the snapshot prefix may only use letters, digits, _ - and .".to_string());
      }
      match job.dest_host.as_deref() {
          None | Some("") => Err("a replication job needs a destination host (or 'file')".to_string()),
          Some(DEST_FILE) => {
              if job.dest_dataset.is_some() {
                  return Err("a file destination takes a dest_dir, not a destination dataset".to_string());
              }
              if !job.dest_dir.as_deref().is_some_and(valid_dest_dir) {
                  return Err("a file destination needs an absolute dest_dir on the hub".to_string());
              }
              Ok(())
          }
          Some(_) => {
              if !job.dest_dataset.as_deref().is_some_and(valid_dataset) {
                  return Err("a replication job needs a destination dataset like backup/data".to_string());
              }
              if job.dest_dir.is_some() {
                  return Err("dest_dir is only for a file destination".to_string());
              }
              if job.dest_host.as_deref() == Some(job.host.as_str()) && job.dest_dataset == job.dataset {
                  return Err("the source and destination are the same dataset".to_string());
              }
              Ok(())
          }
      }
  }
  ```
  In `resolve`'s `match`, add the arm and the later destination-host check (after the existing host check):
  ```rust
          KIND_REPLICATE => check_replicate_fields(job)?,
  ```
  ```rust
      if job.kind == KIND_REPLICATE {
          match job.dest_host.as_deref() {
              // Plan 2 (file targets) removes this refusal.
              Some(DEST_FILE) => return Err("file destinations are not supported by this build".to_string()),
              Some(dest) if !known_hosts.iter().any(|h| h == dest) => return Err(format!("destination host '{dest}' is not in config.toml")),
              _ => {}
          }
      }
  ```
  Destination clash, and routing it through the existing functions. Replace `clashing_job` and `duplicate_prefix_reasons` with:
  ```rust
  /// Whether two enabled, well-formed replicate jobs write to the same place: their incrementals
  /// would corrupt each other.
  fn dest_clashes(a: &Job, b: &Job) -> bool {
      let live = |j: &Job| j.enabled && j.kind == KIND_REPLICATE && check_replicate_fields(j).is_ok();
      if !live(a) || !live(b) || a.dest_host != b.dest_host {
          return false;
      }
      match a.dest_host.as_deref() {
          Some(DEST_FILE) => a.dest_dir == b.dest_dir,
          _ => a.dest_dataset == b.dest_dataset,
      }
  }

  /// Why `job` cannot run alongside the enabled `earlier` job, if it cannot.
  pub fn clash_reason(earlier: &Job, job: &Job) -> Option<String> {
      if clashes(earlier, job) {
          Some(format!("same host and prefix as job '{}' on an overlapping dataset: they would prune each other", earlier.id))
      } else if dest_clashes(earlier, job) {
          Some(format!("same destination as job '{}': their incrementals would corrupt each other", earlier.id))
      } else {
          None
      }
  }

  /// Another job in `jobs` that `candidate` would clash with, if any.
  pub fn clashing_job<'a>(jobs: &'a [Job], candidate: &Job) -> Option<&'a Job> {
      jobs.iter().find(|j| j.id != candidate.id && clash_reason(j, candidate).is_some())
  }

  /// For each job that clashes with an *earlier* job that can itself run, why it cannot run. A
  /// flagged job never blocks a later one, so only the first of a group runs.
  pub fn duplicate_prefix_reasons(jobs: &[Job]) -> std::collections::HashMap<String, String> {
      let mut accepted: Vec<&Job> = Vec::new();
      let mut reasons = std::collections::HashMap::new();
      for job in jobs {
          match accepted.iter().find_map(|earlier| clash_reason(earlier, job)) {
              Some(reason) => {
                  reasons.insert(job.id.clone(), reason);
              }
              None => accepted.push(job),
          }
      }
      reasons
  }
  ```
  Test helper (next to `test_snapshot`):
  ```rust
      pub fn test_replicate(id: &str, dataset: &str, prefix: &str, dest_host: &str, dest_dataset: &str) -> Job {
          Job { id: id.into(), host: "local".into(), kind: KIND_REPLICATE.into(), pool: None, dataset: Some(dataset.into()), recursive: false, prefix: Some(prefix.into()), keep: None, dest_host: Some(dest_host.into()), dest_dataset: Some(dest_dataset.into()), dest_dir: None, force: false, schedule: "30 * * * *".into(), catch_up: true, enabled: true, created_at: None }
      }
  ```

- [ ] **Step 4: Fix the existing literals.** Every literal ends `keep: None }` or `keep: Some(..) }`:

  ```sh
  grep -rlE "keep: (None|Some\()" src | xargs sed -i -E 's/keep: (None|Some\([^}]*\)) \}/keep: \1, dest_host: None, dest_dataset: None, dest_dir: None, force: false }/'
  cargo build --all-targets 2>&1 | grep -E "^error" -A8 | head -40
  ```
  `test_snapshot` is a `keep: Some(keep), schedule: ...` literal, not matched; add the four fields to it by hand, and to the multi-line literals the compiler reports (scheduler.rs test `job()`, jobs.rs `scrub()`, state.rs `build_job_action` and `build_snapshot_job_action` `None =>` arms).

- [ ] **Step 5: Run:** `cargo test 2>&1 | grep -E "test result|FAILED|panicked"`; `cargo clippy --all-targets 2>&1 | grep -c "never used"` — Expected: all pass; at most the 4 pre-existing dead-code warnings plus transient ones for the not-yet-used new helpers. Commit (`feat: the replicate job kind in the job model, with validation and the shared-destination rule`).

---

### Task 4: Pure replication logic: listings, selection, argument lists

**Files:**
- Create: `src/zfs/replication.rs`
- Modify: `src/zfs/mod.rs` (add `pub mod replication;`)

**Interfaces:**
- Consumes: `daemon::snapshot::is_own`, `zfs::run_checked`.
- Produces:
  ```rust
  pub struct SnapInfo { pub name: String, pub guid: u64, pub createtxg: u64 }   // name is the part after '@'
  pub fn parse_snapshot_infos(output: &str, dataset: &str) -> Result<Vec<SnapInfo>>;
  pub fn list_snapshot_infos(runner: &dyn CommandRunner, dataset: &str) -> Result<Vec<SnapInfo>>;
  pub fn list_snapshot_infos_or_empty(runner: &dyn CommandRunner, dataset: &str) -> Result<Vec<SnapInfo>>; // missing dataset -> empty
  pub fn resume_token(runner: &dyn CommandRunner, dataset: &str) -> Result<Option<String>>;               // missing dataset or "-" -> None
  pub fn parse_send_size(output: &str) -> Option<u64>;
  pub enum Plan { NoSnapshots, UpToDate(String), Send { new: String, base: Option<String> } }
  pub fn plan(source: &[SnapInfo], dest: &[SnapInfo], prefix: &str) -> Plan;
  pub fn send_args(dataset: &str, new: &str, base: Option<&str>, dry: bool) -> Vec<String>;
  pub fn resume_send_args(token: &str, dry: bool) -> Vec<String>;
  pub fn receive_args(dest: &str, force: bool) -> Vec<String>;
  ```

- [ ] **Step 0: Check the live format first.** Task 0's live results (the spec's `### Live results`) are the source of truth for the exact `zfs send -nvP`, `zfs get` and `zfs list` output; if any fixture below differs from what the hosts printed, use the host's output for the fixture and adjust the parser.

- [ ] **Step 1: Write the failing tests** (create the file with only this test module plus `use` lines, add `pub mod replication;`, and confirm the compile failure):

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::exec::test_support::FakeRunner;

      fn info(name: &str, guid: u64, txg: u64) -> SnapInfo {
          SnapInfo { name: name.to_string(), guid, createtxg: txg }
      }

      #[test]
      fn parses_name_guid_and_txg_and_ignores_other_datasets() {
          let out = "tank/data@a\t111\t10\ntank/data@b\t222\t20\ntank/data/child@a\t333\t11\n";
          assert_eq!(parse_snapshot_infos(out, "tank/data").unwrap(), vec![info("a", 111, 10), info("b", 222, 20)]);
          assert!(parse_snapshot_infos("tank/data@a\t111\n", "tank/data").is_err());
          assert!(parse_snapshot_infos("tank/data@a\tx\t1\n", "tank/data").is_err());
          assert!(parse_snapshot_infos("", "tank/data").unwrap().is_empty());
      }

      #[test]
      fn a_missing_destination_has_no_snapshots_and_no_token() {
          let gone = FakeRunner::failing("cannot open 'backup/x': dataset does not exist", 1);
          assert!(list_snapshot_infos_or_empty(&gone, "backup/x").unwrap().is_empty());
          assert_eq!(resume_token(&gone, "backup/x").unwrap(), None);
          let broken = FakeRunner::failing("permission denied", 1);
          assert!(list_snapshot_infos_or_empty(&broken, "backup/x").is_err());
          assert!(resume_token(&broken, "backup/x").is_err());
      }

      #[test]
      fn a_dash_is_no_token_and_anything_else_is_the_token() {
          assert_eq!(resume_token(&FakeRunner::new("-\n"), "d").unwrap(), None);
          assert_eq!(resume_token(&FakeRunner::new("1-abc-def\n"), "d").unwrap(), Some("1-abc-def".to_string()));
      }

      #[test]
      fn the_send_size_is_the_size_line() {
          let out = "full\ttank/data@a\t1234567\nsize\t1234567\n";
          assert_eq!(parse_send_size(out), Some(1_234_567));
          assert_eq!(parse_send_size("incremental\t@a\ttank/data@b\t99\nsize\t99\n"), Some(99));
          assert_eq!(parse_send_size("nothing useful"), None);
      }

      #[test]
      fn the_newest_prefixed_snapshot_is_sent_in_full_when_the_destination_has_nothing() {
          let src = vec![info("hourly-20261006T100000Z", 1, 10), info("hourly-20261006T110000Z", 2, 20), info("manual", 9, 30), info("daily-20261006T120000Z", 8, 40)];
          assert_eq!(plan(&src, &[], "hourly"), Plan::Send { new: "hourly-20261006T110000Z".to_string(), base: None });
      }

      #[test]
      fn the_base_is_the_newest_snapshot_both_sides_have_by_guid() {
          let src = vec![info("hourly-20261006T100000Z", 1, 10), info("hourly-20261006T110000Z", 2, 20), info("hourly-20261006T120000Z", 3, 30)];
          // The destination has 1 and 2 (names may differ; only the guid counts) but not 3.
          let dst = vec![info("renamed-one", 1, 5), info("hourly-20261006T110000Z", 2, 6)];
          assert_eq!(plan(&src, &dst, "hourly"), Plan::Send { new: "hourly-20261006T120000Z".to_string(), base: Some("hourly-20261006T110000Z".to_string()) });
      }

      #[test]
      fn a_source_snapshot_pruned_since_the_last_run_costs_a_longer_incremental_not_a_wrong_one() {
          // 11:00 was pruned from the source; the destination still has 10:00 (guid 1) and 11:00 (guid 2).
          let src = vec![info("hourly-20261006T100000Z", 1, 10), info("hourly-20261006T120000Z", 3, 30)];
          let dst = vec![info("hourly-20261006T100000Z", 1, 5), info("hourly-20261006T110000Z", 2, 6)];
          assert_eq!(plan(&src, &dst, "hourly"), Plan::Send { new: "hourly-20261006T120000Z".to_string(), base: Some("hourly-20261006T100000Z".to_string()) });
      }

      #[test]
      fn a_destination_with_snapshots_but_none_in_common_gets_no_base() {
          let src = vec![info("hourly-20261006T100000Z", 1, 10)];
          let dst = vec![info("unrelated", 77, 5)];
          assert_eq!(plan(&src, &dst, "hourly"), Plan::Send { new: "hourly-20261006T100000Z".to_string(), base: None });
      }

      #[test]
      fn nothing_to_send_when_up_to_date_or_when_no_snapshot_matches() {
          let src = vec![info("hourly-20261006T100000Z", 1, 10)];
          assert_eq!(plan(&src, &[info("whatever", 1, 3)], "hourly"), Plan::UpToDate("hourly-20261006T100000Z".to_string()));
          assert_eq!(plan(&src, &[], "daily"), Plan::NoSnapshots);
          assert_eq!(plan(&[], &[], "hourly"), Plan::NoSnapshots);
      }

      #[test]
      fn the_argument_lists_are_exactly_what_was_verified_live() {
          assert_eq!(send_args("tank/data", "h-2", None, false), ["send", "--", "tank/data@h-2"]);
          assert_eq!(send_args("tank/data", "h-2", Some("h-1"), false), ["send", "-i", "@h-1", "--", "tank/data@h-2"]);
          assert_eq!(send_args("tank/data", "h-2", Some("h-1"), true), ["send", "-nvP", "-i", "@h-1", "--", "tank/data@h-2"]);
          assert_eq!(resume_send_args("1-abc", false), ["send", "-t", "1-abc"]);
          assert_eq!(resume_send_args("1-abc", true), ["send", "-nvP", "-t", "1-abc"]);
          assert_eq!(receive_args("backup/data", false), ["receive", "-s", "-u", "--", "backup/data"]);
          assert_eq!(receive_args("backup/data", true), ["receive", "-s", "-u", "-F", "--", "backup/data"]);
      }
  }
  ```

- [ ] **Step 2: Run to see them fail:** `cargo test zfs::replication 2>&1 | grep -E "^error" | sort | uniq -c | head` — Expected: `cannot find function`/`type` errors.

- [ ] **Step 3: Implement** above the tests:

  ```rust
  //! The pure parts of replication: reading the listings a transfer is planned from, choosing what
  //! to send, and building the `zfs send`/`zfs receive` argument lists.

  use crate::daemon::snapshot::is_own;
  use crate::error::{AppError, Result};
  use crate::exec::CommandRunner;

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct SnapInfo {
      /// The part after `@`.
      pub name: String,
      pub guid: u64,
      pub createtxg: u64,
  }

  const LIST_ARGS: &[&str] = &["list", "-t", "snapshot", "-H", "-p", "-o", "name,guid,createtxg", "-d", "1"];

  /// Parses `zfs list -t snapshot -H -p -o name,guid,createtxg`, keeping only `dataset`'s own snapshots.
  pub fn parse_snapshot_infos(output: &str, dataset: &str) -> Result<Vec<SnapInfo>> {
      let mut out = Vec::new();
      for line in output.lines().filter(|l| !l.trim().is_empty()) {
          let bad = || AppError::Parse(format!("bad snapshot line: {line:?}"));
          let fields: Vec<&str> = line.split('\t').collect();
          let [full, guid, txg] = fields.as_slice() else { return Err(bad()) };
          let (ds, name) = full.split_once('@').ok_or_else(bad)?;
          if ds != dataset {
              continue;
          }
          out.push(SnapInfo { name: name.to_string(), guid: guid.parse().map_err(|_| bad())?, createtxg: txg.parse().map_err(|_| bad())? });
      }
      Ok(out)
  }

  pub fn list_snapshot_infos(runner: &dyn CommandRunner, dataset: &str) -> Result<Vec<SnapInfo>> {
      let mut args: Vec<&str> = LIST_ARGS.to_vec();
      args.push(dataset);
      let output = super::run_checked(runner, "zfs", &args, &format!("zfs list -t snapshot {dataset}"))?;
      parse_snapshot_infos(&output.stdout, dataset)
  }

  /// Like `list_snapshot_infos`, but a dataset that does not exist (yet) has no snapshots.
  pub fn list_snapshot_infos_or_empty(runner: &dyn CommandRunner, dataset: &str) -> Result<Vec<SnapInfo>> {
      match list_snapshot_infos(runner, dataset) {
          Err(AppError::CommandFailed { stderr, .. }) if stderr.contains("does not exist") => Ok(Vec::new()),
          other => other,
      }
  }

  /// The `receive_resume_token` of `dataset`: what an interrupted `zfs receive -s` left behind.
  pub fn resume_token(runner: &dyn CommandRunner, dataset: &str) -> Result<Option<String>> {
      let args = ["get", "-H", "-p", "-o", "value", "receive_resume_token", dataset];
      match super::run_checked(runner, "zfs", &args, &format!("zfs get receive_resume_token {dataset}")) {
          Ok(output) => {
              let value = output.stdout.trim();
              Ok((!value.is_empty() && value != "-").then(|| value.to_string()))
          }
          Err(AppError::CommandFailed { stderr, .. }) if stderr.contains("does not exist") => Ok(None),
          Err(e) => Err(e),
      }
  }

  /// The stream size from `zfs send -nvP` output (the `size<TAB>N` line).
  pub fn parse_send_size(output: &str) -> Option<u64> {
      output.lines().find_map(|l| l.strip_prefix("size\t")).and_then(|v| v.trim().parse().ok())
  }

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Plan {
      /// No source snapshot matches the prefix.
      NoSnapshots,
      /// The destination already has the newest matching snapshot.
      UpToDate(String),
      /// Send `new`, incrementally from `base` when the two sides have one in common.
      Send { new: String, base: Option<String> },
  }

  /// Chooses what to send: the newest source snapshot named `<prefix>-<timestamp>`, incrementally
  /// from the newest *older* snapshot both sides have, matched by guid (names can differ, and a
  /// snapshot pruned from the source costs a longer increment, never a wrong one).
  pub fn plan(source: &[SnapInfo], dest: &[SnapInfo], prefix: &str) -> Plan {
      let Some(newest) = source.iter().filter(|s| is_own(prefix, &s.name)).max_by(|a, b| a.name.cmp(&b.name)) else {
          return Plan::NoSnapshots;
      };
      if dest.iter().any(|d| d.guid == newest.guid) {
          return Plan::UpToDate(newest.name.clone());
      }
      let base = source.iter().filter(|s| s.createtxg < newest.createtxg && dest.iter().any(|d| d.guid == s.guid)).max_by_key(|s| s.createtxg).map(|s| s.name.clone());
      Plan::Send { new: newest.name.clone(), base }
  }

  /// `zfs send [-nvP] [-i @base] -- dataset@new`; `dry` only reports the size.
  pub fn send_args(dataset: &str, new: &str, base: Option<&str>, dry: bool) -> Vec<String> {
      let mut args = vec!["send".to_string()];
      if dry {
          args.push("-nvP".to_string());
      }
      if let Some(base) = base {
          args.push("-i".to_string());
          args.push(format!("@{base}"));
      }
      args.push("--".to_string());
      args.push(format!("{dataset}@{new}"));
      args
  }

  /// `zfs send [-nvP] -t <token>`: continue an interrupted receive.
  pub fn resume_send_args(token: &str, dry: bool) -> Vec<String> {
      let mut args = vec!["send".to_string()];
      if dry {
          args.push("-nvP".to_string());
      }
      args.push("-t".to_string());
      args.push(token.to_string());
      args
  }

  /// `zfs receive -s -u [-F] -- dest`: resumable, never mounted, rolled back only when forced.
  pub fn receive_args(dest: &str, force: bool) -> Vec<String> {
      let mut args = vec!["receive".to_string(), "-s".to_string(), "-u".to_string()];
      if force {
          args.push("-F".to_string());
      }
      args.push("--".to_string());
      args.push(dest.to_string());
      args
  }
  ```

- [ ] **Step 4: Run:** `cargo test zfs::replication 2>&1 | grep -E "test result|FAILED|panicked"`; then `cargo test 2>&1 | grep "test result"` — Expected: pass. Commit (`feat: replication planning: listings, newest-with-prefix selection by guid, argument lists`).

---

### Task 5: Progress files, cancel requests and the byte pump

**Files:**
- Create: `src/daemon/progress.rs`, `src/daemon/cancel.rs`, `src/daemon/transfer.rs`
- Modify: `src/daemon/mod.rs` (`pub mod progress; pub mod cancel; pub mod transfer;`)

**Interfaces:**
- Produces:
  ```rust
  // progress.rs
  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  pub struct Progress { pub job_id: String, pub bytes: u64, pub total: Option<u64>, pub started_at: DateTime<Utc>, pub updated_at: DateTime<Utc>, pub rate: Option<u64> }
  pub fn write(dir: &Path, run_id: &str, p: &Progress) -> std::io::Result<()>;   // atomic, dir created 0700
  pub fn read_all(dir: &Path) -> Vec<Progress>;                                    // unreadable files ignored
  pub fn remove(dir: &Path, run_id: &str);
  pub fn clear_all(dir: &Path);
  pub fn human_bytes(n: u64) -> String;
  pub fn progress_line(p: &Progress) -> String;                                    // "sent 12.3 GiB of 40.0 GiB (30%)  85.0 MiB/s  eta 5m 00s"
  pub struct ProgressWriter { .. }
  impl ProgressWriter { pub fn new(dir: PathBuf, run_id: String, job_id: String, total: Option<u64>, started: DateTime<Utc>) -> Self; pub fn update(&mut self, bytes: u64, now: DateTime<Utc>); }  // writes at most once a second
  // cancel.rs
  pub fn file_name(job_id: &str) -> String;                    // id with chars outside [A-Za-z0-9_.-] replaced by '-'
  pub fn request(dir: &Path, job_id: &str) -> std::io::Result<()>;
  pub fn take_requests(dir: &Path) -> Vec<String>;             // the file names found; every one is deleted
  pub fn clear_all(dir: &Path);
  // transfer.rs
  pub struct Transfer { pub bytes: u64, pub cancelled: bool, pub send_ok: bool, pub recv_ok: bool, pub send_stderr: String, pub recv_stderr: String }
  pub fn pump(send: Command, recv: Command, cancel: &AtomicBool, on_progress: impl FnMut(u64)) -> std::io::Result<Transfer>;
  ```

- [ ] **Step 1: Write the failing tests.** `src/daemon/progress.rs` (tests only first):

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      fn at(s: &str) -> DateTime<Utc> {
          DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
      }

      fn sample(bytes: u64, total: Option<u64>, rate: Option<u64>) -> Progress {
          Progress { job_id: "j".into(), bytes, total, started_at: at("2026-10-06T10:00:00Z"), updated_at: at("2026-10-06T10:00:05Z"), rate }
      }

      #[test]
      fn human_bytes_picks_a_sensible_unit() {
          assert_eq!(human_bytes(512), "512 B");
          assert_eq!(human_bytes(2048), "2.0 KiB");
          assert_eq!(human_bytes(13_194_139_533), "12.3 GiB");
      }

      #[test]
      fn the_line_shows_bytes_total_percent_rate_and_eta_when_known() {
          let gib = 1024 * 1024 * 1024;
          let line = progress_line(&sample(12 * gib, Some(40 * gib), Some(100 * 1024 * 1024)));
          assert_eq!(line, "sent 12.0 GiB of 40.0 GiB (30%)  100.0 MiB/s  eta 4m 46s");
          assert_eq!(progress_line(&sample(5, None, None)), "sent 5 B");
          assert!(progress_line(&sample(10, Some(5), Some(1))).contains("(100%)"), "never over 100%");
      }

      #[test]
      fn files_round_trip_atomically_and_bad_ones_are_ignored() {
          let dir = tempfile::tempdir().unwrap();
          let sub = dir.path().join("progress");
          write(&sub, "r1", &sample(1, Some(2), None)).unwrap();
          std::fs::write(sub.join("junk.json"), "not json").unwrap();
          std::fs::write(sub.join("note.txt"), "x").unwrap();
          assert_eq!(read_all(&sub), vec![sample(1, Some(2), None)]);
          assert!(read_all(&dir.path().join("missing")).is_empty());
          remove(&sub, "r1");
          assert!(read_all(&sub).is_empty());
          write(&sub, "r2", &sample(1, None, None)).unwrap();
          clear_all(&sub);
          assert!(std::fs::read_dir(&sub).map_or(true, |mut d| d.next().is_none()));
      }

      #[test]
      fn the_writer_writes_at_most_once_a_second_and_works_out_the_rate() {
          let dir = tempfile::tempdir().unwrap();
          let mut w = ProgressWriter::new(dir.path().to_path_buf(), "r1".into(), "j".into(), Some(1000), at("2026-10-06T10:00:00Z"));
          w.update(100, at("2026-10-06T10:00:00Z"));
          assert_eq!(read_all(dir.path())[0].bytes, 100, "the first sample is written at once");
          w.update(150, at("2026-10-06T10:00:00.400Z"));
          assert_eq!(read_all(dir.path())[0].bytes, 100, "within a second nothing is rewritten");
          w.update(1100, at("2026-10-06T10:00:02Z"));
          let p = &read_all(dir.path())[0];
          assert_eq!((p.bytes, p.rate), (1100, Some(500)), "1000 bytes over 2 s");
      }
  }
  ```
  `src/daemon/cancel.rs`:

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn the_file_name_is_the_id_made_safe() {
          assert_eq!(file_name("repl-tank-data"), "repl-tank-data");
          assert_eq!(file_name("a/b c"), "a-b-c");
          assert_eq!(file_name("../x"), "..-x");
      }

      #[test]
      fn requests_are_collected_once_and_removed() {
          let dir = tempfile::tempdir().unwrap();
          let sub = dir.path().join("cancel");
          assert!(take_requests(&sub).is_empty(), "no directory is no requests");
          request(&sub, "job-a").unwrap();
          request(&sub, "job b").unwrap();
          let mut got = take_requests(&sub);
          got.sort();
          assert_eq!(got, ["job-a", "job-b"]);
          assert!(take_requests(&sub).is_empty());
      }
  }
  ```
  `src/daemon/transfer.rs` (real processes via `sh`; `head`, `cat`, `sleep`, `yes` exist on Linux and FreeBSD):

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use std::sync::atomic::AtomicBool;
      use std::sync::Arc;
      use std::time::{Duration, Instant};

      fn sh(script: &str) -> Command {
          let mut c = Command::new("sh");
          c.args(["-c", script]);
          c
      }

      #[test]
      fn the_bytes_arrive_intact_and_are_counted() {
          let dir = tempfile::tempdir().unwrap();
          let out = dir.path().join("out");
          let cancel = AtomicBool::new(false);
          let mut last = 0;
          let t = pump(sh("head -c 3000000 /dev/zero; printf END"), sh(&format!("cat > {}", out.display())), &cancel, |b| last = b).unwrap();
          assert!(t.send_ok && t.recv_ok && !t.cancelled, "{t:?}");
          assert_eq!(t.bytes, 3_000_003);
          let written = std::fs::read(&out).unwrap();
          assert_eq!((written.len(), written.ends_with(b"END")), (3_000_003, true));
          assert!(last <= t.bytes);
      }

      #[test]
      fn a_failing_send_is_reported_with_its_stderr() {
          let cancel = AtomicBool::new(false);
          let t = pump(sh("echo boom >&2; exit 3"), sh("cat > /dev/null"), &cancel, |_| {}).unwrap();
          assert!(!t.send_ok && t.recv_ok, "{t:?}");
          assert!(t.send_stderr.contains("boom"), "{t:?}");
      }

      #[test]
      fn a_receive_that_exits_early_fails_the_run_promptly_with_its_message() {
          let cancel = AtomicBool::new(false);
          let started = Instant::now();
          let t = pump(sh("yes | head -c 500000000"), sh("echo nope >&2; exit 4"), &cancel, |_| {}).unwrap();
          assert!(!t.recv_ok, "{t:?}");
          assert!(t.recv_stderr.contains("nope"), "{t:?}");
          assert!(started.elapsed() < Duration::from_secs(10), "the send must not be left blocked on a full pipe");
      }

      #[test]
      fn cancel_kills_both_children_and_returns_cancelled() {
          let cancel = Arc::new(AtomicBool::new(false));
          let flag = Arc::clone(&cancel);
          let setter = std::thread::spawn(move || {
              std::thread::sleep(Duration::from_millis(400));
              flag.store(true, Ordering::SeqCst);
          });
          let started = Instant::now();
          let t = pump(sh("sleep 60"), sh("cat > /dev/null"), &cancel, |_| {}).unwrap();
          setter.join().unwrap();
          assert!(t.cancelled, "{t:?}");
          assert!(started.elapsed() < Duration::from_secs(10));
      }

      #[test]
      fn a_command_that_cannot_start_is_an_error_and_leaves_nothing_running() {
          let cancel = AtomicBool::new(false);
          let missing = Command::new("/nonexistent/zfsctl-test-binary");
          assert!(pump(sh("sleep 60"), missing, &cancel, |_| {}).is_err());
      }
  }
  ```

- [ ] **Step 2: Run to see them fail:** add the three `pub mod` lines to `src/daemon/mod.rs`, then `cargo test daemon::progress daemon::cancel daemon::transfer 2>&1 | grep -E "^error" | sort | uniq -c | head` (run each module filter separately if cargo rejects multiple filters) — Expected: unresolved items.

- [ ] **Step 3: Implement.**

  `src/daemon/progress.rs` (above the tests):

  ```rust
  //! Live progress of a running transfer: a small JSON file per run in `<state>/progress/`, written
  //! by the daemon once a second and read by the TUI. Files are written atomically, so a reader
  //! never sees half of one.

  use chrono::{DateTime, Utc};
  use serde::{Deserialize, Serialize};
  use std::io::Write;
  use std::os::unix::fs::OpenOptionsExt;
  use std::path::{Path, PathBuf};

  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  pub struct Progress {
      pub job_id: String,
      pub bytes: u64,
      pub total: Option<u64>,
      pub started_at: DateTime<Utc>,
      pub updated_at: DateTime<Utc>,
      /// Bytes per second over the last sample.
      pub rate: Option<u64>,
  }

  fn file(dir: &Path, run_id: &str) -> PathBuf {
      let safe: String = run_id.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '-' }).collect();
      dir.join(format!("{safe}.json"))
  }

  pub fn write(dir: &Path, run_id: &str, p: &Progress) -> std::io::Result<()> {
      std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
      let path = file(dir, run_id);
      let tmp = path.with_extension("json.tmp");
      let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
      f.write_all(serde_json::to_string(p).map_err(std::io::Error::other)?.as_bytes())?;
      std::fs::rename(&tmp, &path)
  }

  /// Every readable progress file; a file that does not parse is ignored.
  pub fn read_all(dir: &Path) -> Vec<Progress> {
      let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
      entries
          .filter_map(Result::ok)
          .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
          .filter_map(|e| std::fs::read_to_string(e.path()).ok())
          .filter_map(|text| serde_json::from_str(&text).ok())
          .collect()
  }

  pub fn remove(dir: &Path, run_id: &str) {
      let _ = std::fs::remove_file(file(dir, run_id));
  }

  /// Removes every progress file: the daemon does this at startup, since none of them can belong
  /// to a transfer that is still running.
  pub fn clear_all(dir: &Path) {
      if let Ok(entries) = std::fs::read_dir(dir) {
          for e in entries.filter_map(Result::ok) {
              let _ = std::fs::remove_file(e.path());
          }
      }
  }

  /// `512 B`, `2.0 KiB`, `12.3 GiB`.
  #[allow(clippy::cast_precision_loss)]
  pub fn human_bytes(n: u64) -> String {
      const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
      let mut value = n as f64;
      let mut unit = 0;
      while value >= 1024.0 && unit < UNITS.len() - 1 {
          value /= 1024.0;
          unit += 1;
      }
      match UNITS.get(unit) {
          Some(name) if unit > 0 => format!("{value:.1} {name}"),
          _ => format!("{n} B"),
      }
  }

  /// `sent 12.3 GiB of 40.0 GiB (30%)  85.0 MiB/s  eta 5m 00s`, with whatever is known.
  pub fn progress_line(p: &Progress) -> String {
      let mut line = format!("sent {}", human_bytes(p.bytes));
      if let Some(total) = p.total.filter(|t| *t > 0) {
          line.push_str(&format!(" of {} ({}%)", human_bytes(total), p.bytes.min(total) * 100 / total));
      }
      if let Some(rate) = p.rate.filter(|r| *r > 0) {
          line.push_str(&format!("  {}/s", human_bytes(rate)));
          if let Some(total) = p.total {
              let secs = total.saturating_sub(p.bytes) / rate;
              line.push_str(&format!("  eta {}", crate::ui::jobs::duration_text(i64::try_from(secs).unwrap_or(i64::MAX))));
          }
      }
      line
  }

  /// Rewrites a run's progress file at most once a second.
  pub struct ProgressWriter {
      dir: PathBuf,
      run_id: String,
      job_id: String,
      total: Option<u64>,
      started_at: DateTime<Utc>,
      last: Option<(DateTime<Utc>, u64)>,
  }

  impl ProgressWriter {
      pub fn new(dir: PathBuf, run_id: String, job_id: String, total: Option<u64>, started_at: DateTime<Utc>) -> Self {
          ProgressWriter { dir, run_id, job_id, total, started_at, last: None }
      }

      pub fn update(&mut self, bytes: u64, now: DateTime<Utc>) {
          let rate = match self.last {
              Some((then, _)) if (now - then).num_milliseconds() < 1000 => return,
              Some((then, before)) => {
                  let ms = u64::try_from((now - then).num_milliseconds()).unwrap_or(1).max(1);
                  Some(bytes.saturating_sub(before).saturating_mul(1000) / ms)
              }
              None => None,
          };
          self.last = Some((now, bytes));
          let p = Progress { job_id: self.job_id.clone(), bytes, total: self.total, started_at: self.started_at, updated_at: now, rate };
          let _ = write(&self.dir, &self.run_id, &p);
      }
  }
  ```
  (If clippy flags `format!` appended with `push_str`, use `use std::fmt::Write as _;` and `write!(line, ...)`; if `serde_json` is not yet a dependency of this crate, it is already used by `history.rs`.)

  `src/daemon/cancel.rs`:

  ```rust
  //! Cancel requests: the TUI drops a file in `<state>/cancel/` and the daemon's one-second loop
  //! picks it up. There is no socket; the state directory is the whole interface.

  use std::path::Path;

  /// The job id made safe for a file name: anything outside `[A-Za-z0-9_.-]` becomes `-`.
  pub fn file_name(job_id: &str) -> String {
      job_id.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '-' }).collect()
  }

  pub fn request(dir: &Path, job_id: &str) -> std::io::Result<()> {
      use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
      std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
      std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(dir.join(file_name(job_id))).map(|_| ())
  }

  /// The names of the pending requests; every file found is deleted (a request for a job that is
  /// not running is simply discarded by the caller).
  pub fn take_requests(dir: &Path) -> Vec<String> {
      let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
      let mut names = Vec::new();
      for e in entries.filter_map(Result::ok) {
          if let Some(name) = e.file_name().to_str() {
              names.push(name.to_string());
          }
          let _ = std::fs::remove_file(e.path());
      }
      names
  }

  pub fn clear_all(dir: &Path) {
      let _ = take_requests(dir);
  }
  ```
  `src/daemon/transfer.rs`:

  ```rust
  //! Pipes one child's stdout into another's stdin and counts the bytes: the hub-side half of
  //! `zfs send | zfs receive`, with no shell involved. A cancel flag kills both children.

  use std::io::{Read, Write};
  use std::process::{Child, Command, Stdio};
  use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
  use std::sync::Arc;
  use std::time::Duration;

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Transfer {
      pub bytes: u64,
      pub cancelled: bool,
      pub send_ok: bool,
      pub recv_ok: bool,
      pub send_stderr: String,
      pub recv_stderr: String,
  }

  const BUFFER: usize = 256 * 1024;
  const TICK: Duration = Duration::from_millis(200);

  fn drain_stderr(child: &mut Child) -> std::thread::JoinHandle<String> {
      let pipe = child.stderr.take();
      std::thread::spawn(move || {
          let mut text = String::new();
          if let Some(mut p) = pipe {
              let _ = p.read_to_string(&mut text);
          }
          text
      })
  }

  /// Runs `send` and `recv`, copying send's stdout to recv's stdin until it ends. `on_progress` is
  /// called with the byte count about every 200 ms. Setting `cancel` kills both children.
  pub fn pump(mut send: Command, mut recv: Command, cancel: &AtomicBool, mut on_progress: impl FnMut(u64)) -> std::io::Result<Transfer> {
      send.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
      recv.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
      let mut sender = send.spawn()?;
      let mut receiver = match recv.spawn() {
          Ok(child) => child,
          Err(e) => {
              let _ = sender.kill();
              let _ = sender.wait();
              return Err(e);
          }
      };
      let (Some(mut from), Some(mut to)) = (sender.stdout.take(), receiver.stdin.take()) else {
          let _ = sender.kill();
          let _ = receiver.kill();
          let _ = sender.wait();
          let _ = receiver.wait();
          return Err(std::io::Error::other("could not open the pipes"));
      };
      let send_err = drain_stderr(&mut sender);
      let recv_err = drain_stderr(&mut receiver);

      let counted = Arc::new(AtomicU64::new(0));
      let counter = Arc::clone(&counted);
      let copier = std::thread::spawn(move || {
          let mut buf = vec![0_u8; BUFFER];
          loop {
              match from.read(&mut buf) {
                  Ok(0) | Err(_) => break,
                  Ok(n) => {
                      if buf.get(..n).is_none_or(|chunk| to.write_all(chunk).is_err()) {
                          break;
                      }
                      counter.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
                  }
              }
          }
          // Dropping `to` closes the receive's stdin, which is how it learns the stream ended.
      });

      let mut cancelled = false;
      while !copier.is_finished() {
          if cancel.load(Ordering::SeqCst) {
              cancelled = true;
              let _ = sender.kill();
              let _ = receiver.kill();
              break;
          }
          on_progress(counted.load(Ordering::Relaxed));
          std::thread::sleep(TICK);
      }
      let _ = copier.join();
      on_progress(counted.load(Ordering::Relaxed));

      let recv_status = receiver.wait()?;
      if !recv_status.success() {
          // The receive is gone: whatever the send is still doing is pointless.
          let _ = sender.kill();
      }
      let send_status = sender.wait()?;
      Ok(Transfer {
          bytes: counted.load(Ordering::Relaxed),
          cancelled,
          send_ok: send_status.success(),
          recv_ok: recv_status.success(),
          send_stderr: send_err.join().unwrap_or_default(),
          recv_stderr: recv_err.join().unwrap_or_default(),
      })
  }
  ```
  A killed send is simply not ok; callers check `cancelled` first, and when the receive failed they report the receive's message first.

- [ ] **Step 4: Run:** `cargo test 2>&1 | grep -E "test result|FAILED|panicked"`; `cargo clippy --all-targets 2>&1 | grep -E "^warning" | grep -v "never used\|generated"` — Expected: pass; fix any new lint. Commit (`feat: progress files, cancel requests and the send-to-receive byte pump`).

---

### Task 6: The host-to-host replication run

**Files:**
- Create: `src/daemon/replicate.rs`
- Modify: `src/daemon/mod.rs` (`pub mod replicate;`)

**Interfaces:**
- Consumes: `scrub::{Context, Outcome, event, finish, Slot}`, `zfs::replication::*`, `transfer::pump`, `progress::{ProgressWriter, remove}`, `jobs::{effective_prefix, Job}`, `CommandRunner::command`.
- Produces:
  ```rust
  pub struct Env<'a> { pub ctx: &'a Context<'a>, pub progress_dir: &'a Path, pub cancel: &'a AtomicBool }
  pub fn run_replicate(env: &Env, src: &dyn CommandRunner, dst: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome;
  ```
  Lock keys: `(job.host, "#src@<dataset>")` and `(dest_host, "#dst@<dest_dataset>")`.

- [ ] **Step 1: Write the failing tests** (file with only the tests module first). The fake: `Scripted` implements `run` for the zfs queries (`get`, `list`, and `send -nvP`) from configured strings, and `command` returning a `sh -c` process: for `send` a script that prints the configured payload (or exits non-zero), for `receive` a script that copies stdin to a file (or fails), recording every call's arguments.

  ```rust
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
          fn run(&self, src: &Host, dst: &Host, job: &Job) -> (Outcome, Vec<crate::daemon::history::Run>) {
              let history = History::new(self.dir.path().join("history.jsonl"));
              let (clock, stop) = (FixedClock(now()), AtomicBool::new(false));
              let ctx = Context { history: &history, clock: &clock, locks: &self.locks, poll: Duration::from_secs(1), stop: &stop };
              let progress = self.dir.path().join("progress");
              let env = Env { ctx: &ctx, progress_dir: &progress, cancel: &self.cancel };
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
      fn force_adds_dash_capital_f_to_the_receive_and_nothing_else_does() {
          let rig = Rig::new();
          let mut forced = job();
          forced.force = true;
          let (src, dst) = (Host::new(&src_listing()), Host::new(""));
          rig.run(&src, &dst, &forced);
          assert_eq!(dst.commands(), vec![vec!["receive", "-s", "-u", "-F", "--", "backup/data"]]);
      }

      #[test]
      fn a_receive_failure_is_failed_with_the_receives_message() {
          let rig = Rig::new();
          let src = Host::new(&src_listing());
          let mut dst = Host::new("");
          dst.recv_script = "cat > /dev/null; echo \"cannot receive incremental stream: destination backup/data has been modified since most recent snapshot\" >&2; exit 1".into();
          let (outcome, history) = rig.run(&src, &dst, &job());
          assert_eq!(outcome.result, RunResult::Failed);
          assert!(outcome.message.contains("has been modified"), "{}", outcome.message);
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
  ```
  (`FakeRunner`-style `CommandOutput` has `status: i32`; `run_checked` turns non-zero into `AppError::CommandFailed { stderr, .. }`.)

- [ ] **Step 2: Run to see them fail:** add `pub mod replicate;`, then `cargo test daemon::replicate 2>&1 | grep -E "^error" | sort | uniq -c | head` — Expected: unresolved `run_replicate`, `Env`.

- [ ] **Step 3: Implement** above the tests:

  ```rust
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
  use std::path::Path;
  use std::sync::atomic::AtomicBool;

  pub struct Env<'a> {
      pub ctx: &'a Context<'a>,
      pub progress_dir: &'a Path,
      pub cancel: &'a AtomicBool,
  }

  /// What a run decided to send.
  struct Chosen {
      send: Vec<String>,
      dry: Vec<String>,
      label: String,
      resumed: bool,
  }

  pub fn run_replicate(env: &Env, src: &dyn CommandRunner, dst: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome {
      let ctx = env.ctx;
      let dataset = job.dataset.clone().unwrap_or_default();
      let dest_host = job.dest_host.clone().unwrap_or_default();
      let dest = job.dest_dataset.clone().unwrap_or_default();
      let prefix = effective_prefix(job).to_string();
      let slot = Slot { scheduled_for: Some(scheduled_for), catch_up };
      let failed = |msg: String| finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, msg, slot);
      let skipped = |msg: String| finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, msg, slot);

      let Some(_src_guard) = ctx.locks.try_acquire(&job.host, &format!("#src@{dataset}")) else { return skipped("skipped: already running".to_string()) };
      let Some(_dst_guard) = ctx.locks.try_acquire(&dest_host, &format!("#dst@{dest}")) else { return skipped("skipped: the destination is busy".to_string()) };

      let token = match resume_token(dst, &dest) {
          Ok(t) => t,
          Err(e) => return failed(e.to_string()),
      };
      let chosen = match token {
          Some(token) => Chosen { send: resume_send_args(&token, false), dry: resume_send_args(&token, true), label: "resumed an interrupted transfer".to_string(), resumed: true },
          None => {
              let source = match list_snapshot_infos(src, &dataset) {
                  Ok(s) => s,
                  Err(e) => return failed(e.to_string()),
              };
              let destination = match list_snapshot_infos_or_empty(dst, &dest) {
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
                  },
              }
          }
      };

      // The size is only for the progress display; a failed or unparseable estimate is not an error.
      let dry: Vec<&str> = chosen.dry.iter().map(String::as_str).collect();
      let total = src.run("zfs", &dry).ok().and_then(|out| parse_send_size(&out.stdout));

      let send_args_ref: Vec<&str> = chosen.send.iter().map(String::as_str).collect();
      let recv = receive_args(&dest, job.force);
      let recv_args_ref: Vec<&str> = recv.iter().map(String::as_str).collect();
      let (send_cmd, recv_cmd) = match (src.command("zfs", &send_args_ref), dst.command("zfs", &recv_args_ref)) {
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
          let first = |text: &str| text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
          let (side, text) = if transfer.recv_ok { ("send", first(&transfer.send_stderr)) } else { ("receive", first(&transfer.recv_stderr)) };
          let mut message = if text.is_empty() { format!("{side} failed") } else { format!("{side} failed: {text}") };
          if chosen.resumed {
              message.push_str(&format!(". If the transfer cannot be continued, discard it with `zfs receive -A {dest}` on {dest_host}"));
          }
          return failed(message);
      }
      finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, format!("{}, {}", chosen.label, progress::human_bytes(transfer.bytes)), slot)
  }
  ```
  Tests expect `Skipped` for a busy destination and `Failed` with `recv`/`send` text as written above; `"send failed: cannot send: no such snapshot"` and `"receive failed: cannot receive incremental stream: destination backup/data has been modified since most recent snapshot"` satisfy the assertions.

- [ ] **Step 4: Run:** `cargo test daemon::replicate 2>&1 | grep -E "test result|FAILED|panicked" -A6`; then the full suite and clippy — Expected: all pass; fix new lints. Commit (`feat: the host-to-host replication run: plan, resume, send|receive, progress, cancel`).

---

### Task 7: Daemon wiring, cancel requests and restart cleanup

**Files:**
- Modify: `src/daemon/engine.rs` (`Paths`), `src/daemon/mod.rs` (`Shared`, `execute`, loop, startup, tests)

**Interfaces:**
- Consumes: Task 6 `replicate::{Env, run_replicate}`, Task 5 `cancel`, `progress`.
- Produces: `Paths::progress_dir()`, `Paths::cancel_dir()`; `Shared { progress_dir: PathBuf, cancels: Mutex<HashMap<String, Arc<AtomicBool>>>, .. }`.

- [ ] **Step 1: Write the failing tests.** `src/daemon/engine.rs` tests (helpers `started`, `engine`, `at` exist; `"local"` is a host there, and a same-host destination with a different dataset is valid):

  ```rust
  #[test]
  fn an_unfinished_replicate_run_is_closed_as_lost_not_resumed() {
      let dir = tempfile::tempdir().unwrap();
      let paths = Paths::new(dir.path());
      jobs::save(&paths.jobs(), &[Job::test_replicate("repl", "tank/data", "hourly", "local", "backup/data")]).unwrap();
      History::new(paths.history()).append(&started("repl", "r1", "2026-10-07T03:00:00Z")).unwrap();
      let work = engine(&dir, "2026-10-07T04:00:00Z").start(at("2026-10-07T04:00:00Z"));
      assert!(work.iter().any(|w| matches!(w, Work::Orphan(run) if run.run_id == "r1")), "{work:?}");
      assert!(!work.iter().any(|w| matches!(w, Work::Resume { .. })), "{work:?}");
  }
  ```
  `src/daemon/mod.rs` tests (the module's `shared(&dir, runner, notify)` helper gains the two new fields, set to `dir.path().join("progress")` and an empty map):

  ```rust
  #[test]
  fn a_due_replicate_job_whose_destination_host_is_not_configured_is_recorded_as_failed() {
      let dir = tempfile::tempdir().unwrap();
      let shared = shared(&dir, FakeRunner::new(""), None);
      let job = Job::test_replicate("repl", "tank/data", "hourly", "gone", "backup/data");
      let due = Due { job, scheduled_for: shared.clock.now(), catch_up: false };
      shared.execute(Work::Run(due));
      let runs = history::runs(&shared.history.read());
      assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Failed)));
      assert!(runs[0].message.contains("gone"), "{}", runs[0].message);
  }

  #[test]
  fn a_cancel_request_sets_the_flag_of_the_running_job_only_and_is_always_consumed() {
      let dir = tempfile::tempdir().unwrap();
      let shared = shared(&dir, FakeRunner::new(""), None);
      let running = Arc::new(AtomicBool::new(false));
      shared.cancels.lock().unwrap().insert(cancel::file_name("repl-a"), Arc::clone(&running));
      let cancel_dir = dir.path().join("cancel");
      cancel::request(&cancel_dir, "repl-a").unwrap();
      cancel::request(&cancel_dir, "not-running").unwrap();
      shared.apply_cancel_requests(&cancel_dir);
      assert!(running.load(Ordering::SeqCst));
      assert!(cancel::take_requests(&cancel_dir).is_empty(), "every request file is consumed");
  }

  #[test]
  fn startup_removes_progress_and_cancel_files_left_by_a_previous_daemon() {
      let dir = tempfile::tempdir().unwrap();
      let paths = Paths::new(dir.path());
      let p = progress::Progress { job_id: "j".into(), bytes: 1, total: None, started_at: Utc::now(), updated_at: Utc::now(), rate: None };
      progress::write(&paths.progress_dir(), "old", &p).unwrap();
      cancel::request(&paths.cancel_dir(), "j").unwrap();
      clear_runtime_files(&paths);
      assert!(progress::read_all(&paths.progress_dir()).is_empty());
      assert!(cancel::take_requests(&paths.cancel_dir()).is_empty());
  }
  ```

- [ ] **Step 2: Run to see them fail:** `cargo test 2>&1 | grep -E "^error" | sort | uniq -c | head` — Expected: `no method progress_dir`, `cancels`, `apply_cancel_requests`, `clear_runtime_files`.

- [ ] **Step 3: Implement.**
  - `Paths` (engine.rs): `pub fn progress_dir(&self) -> PathBuf { self.dir.join("progress") }` and `pub fn cancel_dir(&self) -> PathBuf { self.dir.join("cancel") }`.
  - `Shared`: add `progress_dir: PathBuf` and `cancels: Mutex<HashMap<String, Arc<AtomicBool>>>`; set them in `run()` (`paths.progress_dir()`, `Mutex::default()`) and in the tests' `shared()` helper (`dir.path().join("progress")`, empty map). `use std::collections::HashMap;`.
  - A free function in `src/daemon/mod.rs`:
    ```rust
    /// Progress and cancel files can only belong to a transfer of a previous daemon: remove them.
    fn clear_runtime_files(paths: &Paths) {
        progress::clear_all(&paths.progress_dir());
        cancel::clear_all(&paths.cancel_dir());
    }
    ```
    Call it in `run()` right after `paths.ensure_dir()`... before `engine.start` (note `paths` is moved into `Engine::new`; call it before that, using a reference).
  - `Shared::apply_cancel_requests(&self, dir: &Path)`:
    ```rust
    /// Hands each pending cancel request to the run it names; a request for a job that is not
    /// running has nothing to cancel and is dropped.
    fn apply_cancel_requests(&self, dir: &Path) {
        let running = self.cancels.lock().unwrap_or_else(PoisonError::into_inner);
        for name in cancel::take_requests(dir) {
            if let Some(flag) = running.get(&name) {
                log(&format!("cancelling {name}"));
                flag.store(true, Ordering::SeqCst);
            }
        }
    }
    ```
    and in `run()`'s one-second loop, before `engine.step`: `shared.apply_cancel_requests(&cancel_dir);` (`let cancel_dir = paths.cancel_dir();` taken before `paths` moves).
  - `execute`'s `Work::Run(due)` arm becomes:
    ```rust
    Work::Run(due) => {
        let run_id = format!("{}-{}", due.job.id, self.clock.now().timestamp_millis());
        log(&format!("running {} (slot {}{})", due.job.id, due.scheduled_for.to_rfc3339(), if due.catch_up { ", catch-up" } else { "" }));
        let outcome = match due.job.kind.as_str() {
            crate::jobs::KIND_REPLICATE => Some(self.replicate(&ctx, &due, &run_id)),
            kind => {
                let Some(runner) = self.runner(&due.job.host) else { return };
                if kind == crate::jobs::KIND_SNAPSHOT {
                    Some(snapshot::run_snapshot(&ctx, runner.as_ref(), &due.job, due.scheduled_for, due.catch_up, &run_id))
                } else {
                    scrub::run_scrub(&ctx, runner.as_ref(), &due.job, due.scheduled_for, due.catch_up, &run_id)
                }
            }
        };
        if let Some(outcome) = outcome {
            log(&format!("{} {:?}: {}", outcome.job_id, outcome.result, outcome.message));
            self.notify(&due.job, &outcome);
        }
    }
    ```
    and the method:
    ```rust
    /// Runs a replication with its own cancel flag registered under the job's cancel-file name, so a
    /// request file can reach it. A host that is no longer configured is a recorded failure.
    fn replicate(&self, ctx: &Context, due: &scheduler::Due, run_id: &str) -> Outcome {
        let job = &due.job;
        let dest_host = job.dest_host.clone().unwrap_or_default();
        let (Some(src), Some(dst)) = (self.runner(&job.host), self.runner(&dest_host)) else {
            let slot = scrub::Slot { scheduled_for: Some(due.scheduled_for), catch_up: due.catch_up };
            return scrub::finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, format!("host '{}' or '{dest_host}' is not configured", job.host), slot);
        };
        let flag = Arc::new(AtomicBool::new(false));
        let key = cancel::file_name(&job.id);
        self.cancels.lock().unwrap_or_else(PoisonError::into_inner).insert(key.clone(), Arc::clone(&flag));
        let env = replicate::Env { ctx, progress_dir: &self.progress_dir, cancel: &flag };
        let outcome = replicate::run_replicate(&env, src.as_ref(), dst.as_ref(), job, due.scheduled_for, due.catch_up, run_id);
        self.cancels.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        outcome
    }
    ```
    (`use std::collections::HashMap;` and `std::sync::Mutex` as needed.)
  - `Engine`'s `hand_out_pending` already restricts `Resume` to `KIND_SCRUB`; no change.

- [ ] **Step 4: Run:** `cargo test 2>&1 | grep -E "test result|FAILED|panicked" -A5`; clippy. Commit (`feat: the daemon runs replicate jobs, takes cancel requests and clears stale runtime files`).

---

### Task 8: The replication form, keys, pickers and display

**Files:**
- Modify: `src/ui/modal.rs`, `src/ui/state.rs`, `src/ui/picker.rs`, `src/ui/jobs.rs`, `src/ui/layout.rs`, `src/ui/hints.rs`

**Interfaces:**
- Consumes: Task 3 (`resolve`, `clash_reason`, `clashing_job`, `DEST_FILE`), Task 5 (`progress::{read_all, progress_line}`, `cancel::request`), existing live-name picker machinery (`LiveNames`, `pending_pool_fetch`).
- Produces:
  ```rust
  // modal.rs: fields = [source host, source dataset, prefix, destination host (or file), destination dataset or directory, force (y/n), schedule, catch up (y/n)]
  FormKind::ReplicateJob { hosts: Vec<String>, existing: Option<Box<crate::jobs::Job>>, jobs: Vec<crate::jobs::Job> }
  // ui/jobs.rs
  pub fn new_replicate_job_id(host: &str, dataset: &str, dest_host: &str, dest: &str, existing: &[String]) -> String;  // repl-<host>-<dataset>-to-<dest_host>-<dest>[-N]
  // JobsView gains: pub progress: Vec<crate::daemon::progress::Progress>   (read in `load` from <state>/progress)
  impl JobsView { pub fn progress_of(&self, job_id: &str) -> Option<&Progress>; pub fn is_running(&self, job_id: &str) -> bool }  // last run has no result yet
  // state.rs (private): fn replicate_job_form(&self, existing: Option<Job>) -> Modal; fn build_replicate_job_action(kind, fields, state_dir, now) -> Result<PendingAction, String>
  ```

- [ ] **Step 1: Write the failing tests** (mirror the snapshot-form tests added in the previous plan; helpers `app_with_jobs`, `form_values`, `at_utc`, `snap_fields`-style builders already exist in `src/ui/state.rs`'s tests):

  ```rust
  fn repl_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
      FormKind::ReplicateJob { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), jobs }
  }
  fn repl_fields(values: [&str; 8]) -> Vec<FormField> {
      values.iter().map(|v| FormField { label: String::new(), value: (*v).to_string() }).collect()
  }
  fn repl_err(values: [&str; 8], jobs: Vec<crate::jobs::Job>) -> String {
      build_replicate_job_action(&repl_kind(None, jobs), &repl_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap_err()
  }
  const GOOD: [&str; 8] = ["local", "tank/data", "hourly", "fbsd16", "backup/data", "n", "30 * * * *", "y"];

  #[test]
  fn big_r_in_jobs_opens_a_blank_replication_form_with_defaults() {
      let (mut app, _dir) = app_with_jobs(&[]);
      app.handle_key(KeyCode::Char('R'));
      assert_eq!(form_values(&app), ["local", "", "auto", "", "", "n", "30 * * * *", "y"]);
  }

  #[test]
  fn a_valid_replication_form_builds_a_save_with_a_fresh_id_and_creation_time() {
      let now = at_utc("2026-10-06T12:00:00Z");
      let action = build_replicate_job_action(&repl_kind(None, vec![]), &repl_fields(GOOD), std::path::Path::new("/state"), now).unwrap();
      let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
      assert_eq!((job.kind.as_str(), job.host.as_str(), job.dataset.as_deref(), job.prefix.as_deref(), job.dest_host.as_deref(), job.dest_dataset.as_deref(), job.force), (crate::jobs::KIND_REPLICATE, "local", Some("tank/data"), Some("hourly"), Some("fbsd16"), Some("backup/data"), false));
      assert_eq!(job.id, "repl-local-tank-data-to-fbsd16-backup-data");
      assert_eq!(job.created_at, Some(now));
      assert!(job.enabled && job.catch_up && job.pool.is_none() && job.keep.is_none());
  }

  #[test]
  fn the_replication_form_refuses_what_the_daemon_would_refuse() {
      let with = |i: usize, v: &str| {
          let mut vals = GOOD;
          vals[i] = v;
          vals
      };
      assert!(repl_err(with(0, "nope"), vec![]).contains("host"));
      assert!(repl_err(with(1, "-rf"), vec![]).contains("source dataset"));
      assert!(repl_err(with(2, "a b"), vec![]).contains("prefix"));
      assert!(repl_err(with(3, ""), vec![]).contains("destination host"));
      assert!(repl_err(with(3, "nope"), vec![]).contains("host"));
      assert!(repl_err(with(4, "tank/a@b"), vec![]).contains("destination dataset"));
      assert!(repl_err(with(5, "maybe"), vec![]).contains("Force"));
      assert!(repl_err(with(6, "bogus"), vec![]).contains("Schedule"));
      assert!(repl_err(with(7, "q"), vec![]).contains("Catch up"));
      assert!(repl_err(["fbsd16", "backup/data", "hourly", "fbsd16", "backup/data", "n", "30 * * * *", "y"], vec![]).contains("same dataset"));
      let mut to_file = GOOD;
      to_file[3] = "file";
      to_file[4] = "/mnt/usb";
      assert!(repl_err(to_file, vec![]).contains("not supported"), "this build has no file destinations yet");
  }

  #[test]
  fn the_replication_form_refuses_a_destination_another_job_already_uses() {
      let other = crate::jobs::Job::test_replicate("other", "tank/elsewhere", "hourly", "fbsd16", "backup/data");
      assert!(repl_err(GOOD, vec![other.clone()]).contains("other"));
      // Editing that same job is fine.
      let kind = repl_kind(Some(other.clone()), vec![other]);
      assert!(build_replicate_job_action(&kind, &repl_fields(GOOD), std::path::Path::new("/x"), chrono::Utc::now()).is_ok());
  }

  #[test]
  fn enter_on_a_replicate_job_edits_it_in_the_replication_form() {
      let (mut app, _dir) = app_with_jobs(&[crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data")]);
      app.hosts = vec!["local".to_string(), "fbsd16".to_string()];
      app.refresh_jobs();
      app.handle_key(KeyCode::Down);
      app.handle_key(KeyCode::Enter);
      assert_eq!(form_values(&app), ["local", "tank/data", "hourly", "fbsd16", "backup/data", "n", "30 * * * *", "y"]);
  }

  #[test]
  fn f2_on_the_replication_forms_fields_opens_the_right_pickers() {
      let (mut app, _dir) = app_with_jobs(&[]);
      app.hosts = vec!["local".to_string(), "fbsd16".to_string()];
      app.handle_key(KeyCode::Char('R'));
      app.handle_modal_key(KeyCode::F(2));
      assert_eq!(app.picker.as_ref().unwrap().values, vec!["local", "fbsd16"], "source host");
      app.picker = None;
      app.handle_modal_key(KeyCode::Tab);
      app.handle_modal_key(KeyCode::F(2));
      assert_eq!(app.pending_pool_fetch, vec![(0, crate::ui::picker::LiveNames::Datasets)], "source dataset: the source host's datasets");
      app.picker = None;
      app.pending_pool_fetch.clear();
      app.handle_modal_key(KeyCode::Tab); // prefix: free text
      app.handle_modal_key(KeyCode::F(2));
      assert!(app.picker.is_none());
      app.handle_modal_key(KeyCode::Tab); // destination host
      app.handle_modal_key(KeyCode::F(2));
      assert_eq!(app.picker.as_ref().unwrap().values, vec!["local", "fbsd16", "file"], "destination host also offers file");
      app.picker = None;
      // Type the destination host, then F2 on the destination dataset lists *that* host's datasets.
      for c in "fbsd16".chars() {
          app.handle_modal_key(KeyCode::Char(c));
      }
      app.handle_modal_key(KeyCode::Tab);
      app.handle_modal_key(KeyCode::F(2));
      assert_eq!(app.pending_pool_fetch, vec![(1, crate::ui::picker::LiveNames::Datasets)]);
  }

  #[test]
  fn space_refuses_to_enable_a_replicate_job_that_clashes_on_its_destination() {
      let mut a = crate::jobs::Job::test_replicate("a", "tank/one", "hourly", "fbsd16", "backup/data");
      a.enabled = false;
      let b = crate::jobs::Job::test_replicate("b", "tank/two", "hourly", "fbsd16", "backup/data");
      let (mut app, _dir) = app_with_jobs(&[a, b]);
      app.refresh_jobs();
      app.handle_key(KeyCode::Down);
      app.handle_key(KeyCode::Char(' '));
      assert!(app.pending_mutations.is_empty());
      assert!(app.status_message.clone().unwrap_or_default().contains("same destination"));
  }

  #[test]
  fn c_cancels_a_running_replication_and_is_inert_otherwise() {
      let repl = crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data");
      let (mut app, dir) = app_with_jobs(&[repl]);
      app.refresh_jobs();
      app.handle_key(KeyCode::Down);
      app.handle_key(KeyCode::Char('c'));
      assert!(app.status_message.is_none(), "not running: nothing happens");
      assert!(crate::daemon::cancel::take_requests(&dir.path().join("cancel")).is_empty());
      let h = crate::daemon::history::History::new(dir.path().join("history.jsonl"));
      h.append(&crate::daemon::history::Event { job_id: "r".into(), run_id: "r1".into(), event: crate::daemon::history::EventKind::Started, at: chrono::Utc::now(), scheduled_for: None, result: None, message: String::new(), catch_up: false, summary: None }).unwrap();
      app.refresh_jobs();
      app.handle_key(KeyCode::Char('c'));
      assert_eq!(crate::daemon::cancel::take_requests(&dir.path().join("cancel")), ["r"]);
      assert!(app.status_message.clone().unwrap_or_default().contains("Cancel requested"));
  }
  ```
  `src/ui/jobs.rs` tests:

  ```rust
  #[test]
  fn replicate_job_ids_are_safe_and_unique() {
      assert_eq!(new_replicate_job_id("local", "tank/data", "fbsd16", "backup/data", &[]), "repl-local-tank-data-to-fbsd16-backup-data");
      let taken = vec!["repl-local-tank-data-to-fbsd16-backup-data".to_string()];
      assert_eq!(new_replicate_job_id("local", "tank/data", "fbsd16", "backup/data", &taken), "repl-local-tank-data-to-fbsd16-backup-data-2");
  }

  #[test]
  fn load_reads_progress_files_and_knows_which_jobs_are_running() {
      let dir = tempfile::tempdir().unwrap();
      jobs::save(&dir.path().join("jobs.toml"), &[Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data")]).unwrap();
      let p = crate::daemon::progress::Progress { job_id: "r".into(), bytes: 5, total: Some(10), started_at: Utc::now(), updated_at: Utc::now(), rate: None };
      crate::daemon::progress::write(&dir.path().join("progress"), "run1", &p).unwrap();
      let view = load(dir.path(), &["local".to_string(), "fbsd16".to_string()], None);
      assert_eq!(view.progress_of("r").map(|p| p.bytes), Some(5));
      assert!(view.progress_of("other").is_none());
      assert!(!view.is_running("r"), "no run in the history yet");
  }
  ```
  `src/ui/layout.rs` tests (use `jobs_app`/`render`): `a_replicate_job_shows_its_source_as_the_target_and_destination_prefix_and_progress_in_the_panel` (job + a progress file → text contains `to fbsd16:backup/data`, `prefix hourly`, `sent 5 B of 10 B (50%)`) and `the_replication_forms_preview_follows_its_schedule_field` (`R` then render: contains `New replication job`, `Next:`; schedule field index 6). `src/ui/hints.rs`: extend the Jobs hint test to assert `R` ("New replication job") is advertised and that `c` ("Cancel transfer") appears only for a running replicate job (the existing invariant tests then enforce that `R` and `c` do something when advertised and nothing when not).

- [ ] **Step 2: Run to see them fail:** `cargo test 2>&1 | grep -E "^error" | sort | uniq -c | head` — Expected: unresolved `ReplicateJob`, `build_replicate_job_action`, `new_replicate_job_id`, `progress_of`, `is_running`.

- [ ] **Step 3: Implement.**

  `src/ui/modal.rs`, after `SnapshotJob`:
  ```rust
      /// Fields: `[source host, source dataset, prefix, destination host (or file), destination dataset (or directory), force (y/n), schedule, catch_up (y/n)]`.
      ReplicateJob { hosts: Vec<String>, existing: Option<Box<crate::jobs::Job>>, jobs: Vec<crate::jobs::Job> },
  ```
  `src/ui/jobs.rs`: add `new_replicate_job_id` using the existing `unique` and `safe_key` helpers:
  ```rust
  /// `repl-<host>-<dataset>-to-<dest host>-<dest>`, made safe the same way.
  pub fn new_replicate_job_id(host: &str, dataset: &str, dest_host: &str, dest: &str, existing: &[String]) -> String {
      unique(&format!("repl-{}-{}-to-{}-{}", safe_key(host), safe_key(dataset), safe_key(dest_host), safe_key(dest)), existing)
  }
  ```
  `JobsView` gains `pub progress: Vec<crate::daemon::progress::Progress>`; `load` fills it with `crate::daemon::progress::read_all(&state_dir.join("progress"))`; and
  ```rust
      pub fn progress_of(&self, job_id: &str) -> Option<&crate::daemon::progress::Progress> {
          self.progress.iter().find(|p| p.job_id == job_id)
      }

      /// Whether the job's latest run has started and not finished.
      pub fn is_running(&self, job_id: &str) -> bool {
          self.last_run(job_id).is_some_and(|r| r.result.is_none())
      }
  ```
  (`JobsView` derives `Default`/`PartialEq`: `Progress` derives both `Clone` and `PartialEq`, so adding the field is fine; update the few `JobsView { .. }` literals the compiler reports.)

  `src/ui/state.rs`: add `replicate_job_form` and `build_replicate_job_action` next to the snapshot ones.
  ```rust
  impl App {
      fn replicate_job_form(&self, existing: Option<crate::jobs::Job>) -> Modal {
          let yn = |b: bool| if b { "y" } else { "n" }.to_string();
          let (title, host, dataset, prefix, dest_host, dest, force, schedule, catch_up) = match &existing {
              Some(j) => (
                  format!("Edit replication job '{}'", j.id),
                  j.host.clone(),
                  j.dataset.clone().unwrap_or_default(),
                  crate::jobs::effective_prefix(j).to_string(),
                  j.dest_host.clone().unwrap_or_default(),
                  j.dest_dataset.clone().or_else(|| j.dest_dir.clone()).unwrap_or_default(),
                  j.force,
                  j.schedule.clone(),
                  j.catch_up,
              ),
              None => ("New replication job".to_string(), self.active_host_name().to_string(), String::new(), crate::jobs::DEFAULT_PREFIX.to_string(), String::new(), String::new(), false, "30 * * * *".to_string(), true),
          };
          Modal::Form {
              title,
              fields: vec![
                  FormField { label: "Source host".to_string(), value: host },
                  FormField { label: "Source dataset".to_string(), value: dataset },
                  FormField { label: "Prefix (snapshots to send)".to_string(), value: prefix },
                  FormField { label: "Destination host (or file)".to_string(), value: dest_host },
                  FormField { label: "Destination dataset (or directory)".to_string(), value: dest },
                  FormField { label: "Force: roll back the destination (y/n)".to_string(), value: yn(force) },
                  FormField { label: "Schedule".to_string(), value: schedule },
                  FormField { label: "Catch up (y/n)".to_string(), value: yn(catch_up) },
              ],
              focused: 0,
              description: Some("Sends the newest <prefix>-<timestamp> snapshot, incrementally; nothing is destroyed or mounted.".to_string()),
              kind: FormKind::ReplicateJob { hosts: self.hosts.clone(), existing: existing.map(Box::new), jobs: self.jobs_view.jobs.clone() },
          }
      }
  }

  /// Turns a submitted replication form into a save, or says what is wrong. The job is validated
  /// by `jobs::resolve`, exactly as the daemon will validate it, plus the shared-destination rule.
  fn build_replicate_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
      let FormKind::ReplicateJob { hosts, existing, jobs } = kind else { return Err("not a replication job form".to_string()) };
      let yes_no = |label: &str, raw: &str, blank: bool| match raw.trim().to_lowercase().as_str() {
          "" => Ok(blank),
          "y" | "yes" => Ok(true),
          "n" | "no" => Ok(false),
          other => Err(format!("{label} must be y or n, not '{other}'")),
      };
      let host = field_value(fields, 0).trim();
      if !hosts.iter().any(|h| h == host) {
          return Err(format!("Unknown host '{host}'. Configured hosts: {}", hosts.join(", ")));
      }
      let dataset = field_value(fields, 1).trim();
      let prefix = match field_value(fields, 2).trim() {
          "" => crate::jobs::DEFAULT_PREFIX,
          p => p,
      };
      let dest_host = field_value(fields, 3).trim();
      let dest = field_value(fields, 4).trim();
      let force = yes_no("Force", field_value(fields, 5), false)?;
      let schedule = field_value(fields, 6).trim();
      crate::daemon::cron::Schedule::parse(schedule).map_err(|e| format!("Schedule: {e}"))?;
      let catch_up = yes_no("Catch up", field_value(fields, 7), true)?;
      let to_file = dest_host == crate::jobs::DEST_FILE;
      let taken: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
      let base = crate::jobs::Job {
          id: String::new(),
          host: host.to_string(),
          kind: crate::jobs::KIND_REPLICATE.to_string(),
          pool: None,
          dataset: Some(dataset.to_string()),
          recursive: false,
          prefix: Some(prefix.to_string()),
          keep: None,
          dest_host: Some(dest_host.to_string()),
          dest_dataset: (!to_file).then(|| dest.to_string()),
          dest_dir: to_file.then(|| dest.to_string()),
          force,
          schedule: schedule.to_string(),
          catch_up,
          enabled: true,
          created_at: Some(now),
      };
      let job = match existing {
          Some(old) => crate::jobs::Job { id: old.id.clone(), enabled: old.enabled, created_at: old.created_at, ..base },
          None => crate::jobs::Job { id: crate::ui::jobs::new_replicate_job_id(host, dataset, dest_host, dest, &taken), ..base },
      };
      crate::jobs::resolve(&job, hosts)?;
      if let Some(other) = crate::jobs::clashing_job(jobs, &job) {
          return Err(format!("Job '{}' cannot run alongside this one: {}", other.id, crate::jobs::clash_reason(other, &job).unwrap_or_default()));
      }
      Ok(PendingAction { host: 0, section: Section::Jobs, call: MutationCall::SaveJob { state_dir: state_dir.to_path_buf(), job: Box::new(job) } })
  }
  ```
  (`resolve`'s messages are what the tests assert: "source dataset", "prefix", "destination host", "destination dataset", "same dataset", "not supported"; the unknown-host message contains "host". `Force`/`Catch up`/`Schedule` come from the helpers above.)

  Keys (in `handle_key`, beside the other Jobs keys):
  ```rust
  KeyCode::Char('R') if self.current_section() == Section::Jobs => {
      self.modal = Some(self.replicate_job_form(None));
  }
  KeyCode::Char('c') if self.current_section() == Section::Jobs => {
      if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)) {
          if job.kind == crate::jobs::KIND_REPLICATE && self.jobs_view.is_running(&job.id) {
              match crate::daemon::cancel::request(&self.state_dir.join("cancel"), &job.id) {
                  Ok(()) => self.status_message = Some(format!("Cancel requested for '{}'; it stops within a second or two", job.id)),
                  Err(e) => self.set_error(format!("Could not request the cancel: {e}")),
              }
          }
      }
  }
  ```
  Extend the Jobs `Enter` arm with the replicate case, the submit chain with a `FormKind::ReplicateJob` branch (copy the `SnapshotJob` branch, calling `build_replicate_job_action`), and the `None` arm of the form-action match (`... | FormKind::SnapshotJob { .. } | FormKind::ReplicateJob { .. } => None`). The Space guard from the snapshot work already calls `clashing_job`; change its message to use the reason: `format!("Cannot enable '{}': it clashes with enabled job '{}' ({}): ...", ...)` built from `crate::jobs::clash_reason(other, &as_enabled).unwrap_or_default()` so replicate clashes read "same destination" and snapshot clashes still read "prune each other" (the existing snapshot test keeps passing).

  `src/ui/picker.rs`: extend the job-form handling in `build_picker_state`: for `FormKind::ReplicateJob { hosts, .. }`: field 0 hosts (as now); field 3 hosts plus the keyword `file` (`let mut values = hosts.clone(); values.push(crate::jobs::DEST_FILE.to_string());`); fields 1 and 4 the Loading live-datasets picker (`pool_host: Some(0)`); field 6 schedule presets; others `None`. State's `open_pool_picker` takes the host from the right field: the F2 handler passes `field_value(&fields, host_field)` where `host_field` is `3` when `kind` is `ReplicateJob` and `focused == 4`, else `0`; `open_pool_picker`'s `match kind` adds `FormKind::ReplicateJob { hosts, .. } => (hosts, LiveNames::Datasets)`, and for `host_name == "file"` it fails the picker with `PickerLoad::Failed("the destination is a directory on the hub: type its path".to_string())` instead of queueing a fetch.

  `src/ui/layout.rs`: `form_preview` gets `FormKind::ReplicateJob { .. } => preview_lines(fields.get(6)...)`. In `draw_jobs`'s row builder the Target cell stays `pool or dataset` (the source). In `draw_jobs_panel`, after the snapshot line:
  ```rust
  if job.kind == crate::jobs::KIND_REPLICATE {
      let dest = match job.dest_host.as_deref() {
          Some(crate::jobs::DEST_FILE) => format!("file:{}", job.dest_dir.as_deref().unwrap_or("")),
          Some(host) => format!("{host}:{}", job.dest_dataset.as_deref().unwrap_or("")),
          None => String::new(),
      };
      info.push(Line::from(format!("to {dest}  prefix {}{}", crate::jobs::effective_prefix(job), if job.force { "  force" } else { "" })));
      if let Some(p) = app.jobs_view.progress_of(&job.id) {
          info.push(Line::from(crate::daemon::progress::progress_line(p)));
      }
  }
  ```
  Empty-state text: add `'R' for a replication job`. `src/ui/hints.rs`, Jobs arm: `v.push(h("R", "New replication job", 3));` and, when the selected job is a replicate job that `is_running`, `v.push(h("c", "Cancel transfer", 1).tone(Tone::Warn));`. Help text in `state.rs`: lines for `R` ("new replication job") and `c` ("cancel the selected replication while it runs"), and add "Source and Destination host, Source and Destination dataset" to the F2 line.

- [ ] **Step 4: Run:** `cargo test 2>&1 | grep -E "test result|FAILED|panicked" -A5`; clippy; Expected: all pass (the repo's hint-invariant tests prove `R` and `c` do something exactly when advertised). Commit (`feat: the replication job form, R and c keys, pickers and progress display`).

---

### Task 9: Retire `replicate_snapshot`, document, deploy and verify live

**Files:**
- Modify: `src/zfs/snapshot.rs` (remove `is_safe_zfs_target`, `is_safe_host`, `replicate_snapshot` and their tests), `README.md`, the spec (live results)

- [ ] **Step 1: Remove the dead pipeline.** Delete `is_safe_zfs_target`, `is_safe_host`, `replicate_snapshot` and the four `replicate_snapshot_*` tests from `src/zfs/snapshot.rs`; delete the README Status paragraph's mention that replication "is implemented and tested at the library layer but is not yet wired into the UI" and the sudoers-section sentence about `sh -c` (replace with "Replication needs no `sh` entry: the hub pipes the two `zfs` commands itself."). Run `cargo test` and `cargo clippy --all-targets 2>&1 | grep -c "never used"` — Expected: all pass, **one** remaining pre-existing dead-code warning (`hosts.rs` `get`).

- [ ] **Step 2: README.** Under "Scheduled jobs": a **Replication** subsection with the host-to-host example job from the spec, the rules (newest `<prefix>-<timestamp>`; incremental from the newest snapshot both sides have by guid, else full; received `-s -u`, so resumable and not mounted; nothing destroyed or rolled back unless `force = true`; a changed destination fails with zfs's message; an interrupted transfer resumes on the next run, and a stale token is discarded by hand with `zfs receive -A`; two jobs cannot share a destination; file destinations are not supported yet), the keys `R` and `c` in the Jobs list, and where progress and cancel files live (`<state>/progress`, `<state>/cancel`). Update the "There are two job kinds" sentence to three. Commit (`docs: replication`).

- [ ] **Step 3: Deploy to the hub** (tar `Cargo.toml Cargo.lock README.md src contrib docs` with `rm -rf src` first on the hub side, `cargo build --release`, `sudo install -m 755 target/release/zfsctl /usr/local/bin/zfsctl`, `sudo service zfsctl restart`; check `daemon.json` resumes).

- [ ] **Step 4: Live end-to-end (a clean `zfs list` round trip proves nothing; check the data and the states).** Use scratch datasets on the hub's `scratch` pool as the source and the VM `fbsd16` (a host in the hub's `config.toml`) or the hub itself as destination; keep every test dataset under a `repl-test` name and destroy them at the end.
  1. Source `scratch/repl-src` with ~300 MB of random data and snapshots `hourly-<UTC stamp>` taken by hand (or by a snapshot job). Job: `host=local`, `dest_host=local`, `dest_dataset=scratch/repl-dst`, every minute. Expect `Ok: sent hourly-... (full), 300 MiB`, then `Skipped: up to date`, then after a new snapshot `incremental from ...`. Verify `zfs list -t snapshot scratch/repl-dst`, that `scratch/repl-dst` is **not mounted** (`zfs get mounted`), and that a checksum of the data matches after mounting it read-only on a throwaway mountpoint.
  2. Cancel: write enough new data that a transfer lasts a while, press `c` in the Jobs panel while the progress line shows; expect `Cancelled` (not counted in the failed indicator, no notify), no `zfs send`/`zfs receive` processes left (`pgrep -fl "zfs (send|receive)"`), `receive_resume_token` set on the destination; the next run reports `resumed an interrupted transfer` and ends `Ok`.
  3. Modified destination: take a snapshot on the destination, then add a source snapshot; expect `Failed` with zfs's "has been modified" text and the destination unchanged; set `force = true` and expect it to roll back and succeed.
  4. Host to host across SSH: repeat 1 with `dest_host=fbsd16` (the VM needs a `scratch`-like pool or use `zroot/repl-test`), and a pull (source on the VM, destination on the hub).
  5. In the TUI: `R`, F2 on each field, save, `Enter` re-open, duplicate-destination refusal, progress line while running, `c`.
  6. Restart the daemon mid-transfer: the run is closed as lost and the next run resumes; progress and cancel files are cleaned at startup.
  7. Clean up (jobs, datasets, snapshots) and record what you saw, plus any bug found with its fix, under a `### Live results` heading in the spec's Testing section. Commit (`docs: live results for replication`).
