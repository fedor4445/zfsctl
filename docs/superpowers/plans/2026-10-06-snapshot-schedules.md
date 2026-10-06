# Snapshot Schedules and Retention Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `snapshot` job kind: the daemon takes a ZFS snapshot of one dataset on a cron schedule and prunes the job's own snapshots beyond the newest N; the Jobs section (and `J` in Datasets) manages them.

**Architecture:** Flat optional fields (`dataset`, `recursive`, `prefix`, `keep`) on the existing `jobs::Job`; a new synchronous `daemon::snapshot::run_snapshot` dispatched by `kind` next to `run_scrub`; the existing cron, catch-up, history, notify and Jobs UI are reused. Retention only ever destroys names matching exactly `<prefix>-<YYYYMMDDTHHMMSSZ>`.

**Tech Stack:** Rust, ratatui, chrono, serde/toml. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-06-snapshot-schedules-design.md`

## Global Constraints

- Snapshot name format is exactly `<prefix>-<UTC YYYYMMDDTHHMMSSZ>` (e.g. `hourly-20261006T193000Z`): no colons, UTC.
- `prefix` charset `A-Za-z0-9_-.`, default `auto`; `dataset` charset `A-Za-z0-9_-:./`, no `@`, no leading `-`; `keep >= 1`.
- Retention destroys only names that match `^<prefix>-\d{8}T\d{6}Z$` exactly, and never when the new snapshot failed.
- Two enabled snapshot jobs on the same host, dataset and prefix are rejected (they would prune each other).
- No new sudoers entries: only `/sbin/zfs` is used.
- A `jobs.toml` with scrub jobs only must serialize exactly as before (new fields skipped when unset).
- Match the surrounding code: comment density, naming, `Result<T, String>` for `resolve`-style validation. `cargo clippy --all-targets` must not gain warnings (4 pre-existing dead-code warnings stay).
- Every code step is TDD: failing test first, then the minimal code.

## Review Focus

- A hand-edited `jobs.toml` with a dataset like `-rf`, `tank/a@b`, or one with spaces must be *unsupported*, never passed to `zfs` (argument injection). Pinned in Task 1.
- Snapshots named like the job's prefix but not exactly the pattern (`hourly-keep-forever`, `hourly-20261006T1930Z`, `hourly-x-20261006T193000Z`, another job's `daily-...`) must survive a prune. Pinned in Task 3.
- `keep` lowered on an existing job prunes the excess on the next run; raised prunes nothing. Pinned in Task 3.
- The dataset was destroyed or the host is unreachable: the run is `Failed` with the zfs error and nothing is pruned. Pinned in Task 3.
- A catch-up landing in the same second as a scheduled run (same snapshot name) is `Skipped`, not `Failed`. Pinned in Task 3.
- A daemon killed mid-snapshot-run leaves a `started` event; on restart that run is closed as lost, never resumed as if it were a scrub. Pinned in Task 4.

---

### Task 1: Live check of the ZFS flags, then the job model and validation

**Files:**
- Modify: `src/jobs.rs` (struct, `resolve`, new helpers, tests)
- Modify: every `Job { .. }` literal under `src/` (add the four new fields)
- Modify: `docs/superpowers/specs/2026-10-06-snapshot-schedules-design.md` (append the live results under Testing)

**Interfaces:**
- Produces (used by Tasks 2-6):
  ```rust
  pub const KIND_SNAPSHOT: &str = "snapshot";
  pub const DEFAULT_PREFIX: &str = "auto";
  // Job gains: pub dataset: Option<String>, pub recursive: bool, pub prefix: Option<String>, pub keep: Option<u32>
  pub fn valid_dataset(s: &str) -> bool;
  pub fn valid_prefix(s: &str) -> bool;
  pub fn effective_prefix(job: &Job) -> &str;               // prefix or DEFAULT_PREFIX
  pub fn clashing_job<'a>(jobs: &'a [Job], candidate: &Job) -> Option<&'a Job>;
  pub fn duplicate_prefix_reasons(jobs: &[Job]) -> std::collections::HashMap<String, String>; // job id -> reason
  impl Job { #[cfg(test)] pub fn test_snapshot(id: &str, dataset: &str, prefix: &str, keep: u32) -> Job }
  ```

- [ ] **Step 1: Verify the zfs behaviour live (do not assume).** On both test boxes (`fb@172.16.10.90` is 15.1-RELEASE; the 16.0-CURRENT VM is `fbsd16`, see the test-box memory for its address), run against the disposable `scratch/data`:

  ```sh
  ssh fb@172.16.10.90 'sudo zfs snapshot -r -- scratch/data@plan-t1 \
    && echo "--- -d 1:" && sudo zfs list -t snapshot -H -p -o name,used -d 1 scratch/data \
    && echo "--- -r:" && sudo zfs list -t snapshot -H -p -o name,used -r scratch/data \
    && echo "--- dup:" ; sudo zfs snapshot -- scratch/data@plan-t1 ; \
    sudo zfs hold keep scratch/data@plan-t1 && echo "--- held:" ; sudo zfs destroy -r -- scratch/data@plan-t1 ; \
    sudo zfs release keep scratch/data@plan-t1 && sudo zfs destroy -r -- scratch/data@plan-t1 && echo cleaned'
  ```

  Expected: `-d 1` lists only `scratch/data@plan-t1`; `-r` lists it (and any child's); the duplicate snapshot fails with text containing `already exists`; destroying the held snapshot fails with text mentioning the hold. **If any of these differ** (flag order, `--` rejected by `zfs list`, different error wording), stop and change the affected later task's code to match what was observed. Paste the observed error texts into the spec under a new `### Live results` heading in the Testing section, and commit.

- [ ] **Step 2: Write the failing tests** in `src/jobs.rs`'s test module (add `use std::collections::HashMap;` if absent):

  ```rust
  #[test]
  fn a_snapshot_job_round_trips_and_a_scrub_job_serializes_exactly_as_before() {
      let dir = tempfile::tempdir().unwrap();
      let path = dir.path().join("jobs.toml");
      let mut snap = Job::test_snapshot("s", "tank/data", "hourly", 24);
      snap.recursive = true;
      let scrub = Job { id: "x".into(), host: "local".into(), kind: KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * 0".into(), catch_up: true, enabled: true, created_at: None, dataset: None, recursive: false, prefix: None, keep: None };
      save(&path, &[scrub.clone(), snap.clone()]).unwrap();
      assert_eq!(load(&path).unwrap(), vec![scrub.clone(), snap]);
      let text = toml::to_string_pretty(&JobFile { jobs: vec![scrub] }).unwrap();
      for absent in ["dataset", "recursive", "prefix", "keep"] {
          assert!(!text.contains(absent), "{absent} leaked into a scrub job:\n{text}");
      }
  }

  #[test]
  fn resolve_accepts_a_good_snapshot_job_and_rejects_each_bad_field() {
      let hosts = vec!["local".to_string()];
      assert!(resolve(&Job::test_snapshot("s", "tank/data", "hourly", 3), &hosts).is_ok());
      let bad = |f: &dyn Fn(&mut Job)| {
          let mut j = Job::test_snapshot("s", "tank/data", "hourly", 3);
          f(&mut j);
          resolve(&j, &hosts).unwrap_err()
      };
      assert!(bad(&|j| j.dataset = None).contains("dataset"));
      assert!(bad(&|j| j.dataset = Some(String::new())).contains("dataset"));
      for evil in ["-rf", "tank/a@b", "tank/a b", "tank/data;x", "/tank", "tank/"] {
          assert!(bad(&|j| j.dataset = Some(evil.to_string())).contains("dataset"), "{evil}");
      }
      assert!(bad(&|j| j.prefix = Some("a b".into())).contains("prefix"));
      assert!(bad(&|j| j.prefix = Some(String::new())).contains("prefix"));
      assert!(bad(&|j| j.keep = Some(0)).contains("keep"));
      assert!(bad(&|j| j.keep = None).contains("keep"));
      assert!(bad(&|j| j.host = "nope".into()).contains("host"));
      assert!(bad(&|j| j.schedule = "x".into()).contains("schedule"));
  }

  #[test]
  fn a_missing_prefix_means_auto() {
      let mut j = Job::test_snapshot("s", "tank/data", "hourly", 3);
      j.prefix = None;
      assert_eq!(effective_prefix(&j), "auto");
      assert!(resolve(&j, &["local".to_string()]).is_ok());
  }

  #[test]
  fn two_enabled_snapshot_jobs_on_one_host_dataset_and_prefix_clash_but_other_combinations_do_not() {
      let a = Job::test_snapshot("a", "tank/data", "hourly", 3);
      let b = Job::test_snapshot("b", "tank/data", "hourly", 5);
      let other_prefix = Job::test_snapshot("c", "tank/data", "daily", 5);
      let other_dataset = Job::test_snapshot("d", "tank/other", "hourly", 5);
      let mut disabled = Job::test_snapshot("e", "tank/data", "hourly", 5);
      disabled.enabled = false;
      let all = vec![a.clone(), b.clone(), other_prefix, other_dataset, disabled];
      let reasons = duplicate_prefix_reasons(&all);
      assert_eq!(reasons.len(), 1, "{reasons:?}");
      assert!(reasons["b"].contains("'a'"), "{reasons:?}");
      assert_eq!(clashing_job(&all, &b).map(|j| j.id.as_str()), Some("a"));
      let mut edited = a.clone();
      edited.keep = Some(9);
      assert!(clashing_job(&[a.clone()], &edited).is_none(), "a job never clashes with itself");
  }
  ```

- [ ] **Step 3: Run to see them fail**

  Run: `cargo test jobs:: 2>&1 | tail -20` — Expected: compile errors (`test_snapshot`, `KIND_SNAPSHOT`, new fields not defined).

- [ ] **Step 4: Implement.** In `src/jobs.rs`:

  Add after `KIND_SCRUB`:
  ```rust
  pub const KIND_SNAPSHOT: &str = "snapshot";
  /// The snapshot name prefix a job without one uses.
  pub const DEFAULT_PREFIX: &str = "auto";
  ```
  Add to `Job`, after the `pool` field:
  ```rust
      /// The dataset a snapshot job snapshots.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub dataset: Option<String>,
      /// `zfs snapshot -r`, and pruning across the whole tree.
      #[serde(default, skip_serializing_if = "std::ops::Not::not")]
      pub recursive: bool,
      /// The snapshot name prefix; only snapshots named `<prefix>-<timestamp>` are ever pruned.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub prefix: Option<String>,
      /// How many of the job's own snapshots to keep.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub keep: Option<u32>,
  ```
  Replace the body of `resolve` and add the helpers:
  ```rust
  /// Whether `s` can be a dataset name for a job: ZFS name characters only, no `@`, no leading
  /// `-` or `/`, no trailing `/`. Checked here so a hand-edited `jobs.toml` can never put an
  /// option or a shell-looking string on a `zfs` command line.
  pub fn valid_dataset(s: &str) -> bool {
      !s.is_empty() && !s.starts_with(['-', '/']) && !s.ends_with('/') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.' | '/'))
  }

  pub fn valid_prefix(s: &str) -> bool {
      !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
  }

  pub fn effective_prefix(job: &Job) -> &str {
      job.prefix.as_deref().unwrap_or(DEFAULT_PREFIX)
  }

  fn check_snapshot_fields(job: &Job) -> std::result::Result<(), String> {
      if !job.dataset.as_deref().is_some_and(valid_dataset) {
          return Err("a snapshot job needs a dataset like tank/data".to_string());
      }
      if !valid_prefix(effective_prefix(job)) {
          return Err("the snapshot prefix may only use letters, digits, _ - and .".to_string());
      }
      if job.keep.is_none_or(|k| k == 0) {
          return Err("a snapshot job needs keep of at least 1".to_string());
      }
      Ok(())
  }

  /// Whether the daemon can run `job`: its schedule if so, or the reason it cannot.
  pub fn resolve(job: &Job, known_hosts: &[String]) -> std::result::Result<Schedule, String> {
      match job.kind.as_str() {
          KIND_SCRUB => {
              if job.pool.as_deref().is_none_or(str::is_empty) {
                  return Err("a scrub job needs a pool".to_string());
              }
          }
          KIND_SNAPSHOT => check_snapshot_fields(job)?,
          other => return Err(format!("unsupported job kind '{other}'")),
      }
      if !known_hosts.iter().any(|h| h == &job.host) {
          return Err(format!("host '{}' is not in config.toml", job.host));
      }
      Schedule::parse(&job.schedule).map_err(|e| format!("bad schedule: {e}"))
  }

  /// The identity two snapshot jobs must not share: they would prune each other's snapshots.
  /// `None` for a job that is disabled, not a snapshot job, or has bad fields.
  fn clash_key(job: &Job) -> Option<(&str, &str, &str)> {
      (job.enabled && job.kind == KIND_SNAPSHOT && check_snapshot_fields(job).is_ok()).then(|| (job.host.as_str(), job.dataset.as_deref().unwrap_or(""), effective_prefix(job)))
  }

  /// Another job in `jobs` that `candidate` would clash with, if any.
  pub fn clashing_job<'a>(jobs: &'a [Job], candidate: &Job) -> Option<&'a Job> {
      let key = clash_key(candidate)?;
      jobs.iter().find(|j| j.id != candidate.id && clash_key(j) == Some(key))
  }

  /// For each snapshot job that clashes with an *earlier* one in `jobs`, why it cannot run.
  pub fn duplicate_prefix_reasons(jobs: &[Job]) -> std::collections::HashMap<String, String> {
      let mut first: std::collections::HashMap<(&str, &str, &str), &str> = std::collections::HashMap::new();
      let mut reasons = std::collections::HashMap::new();
      for job in jobs {
          let Some(key) = clash_key(job) else { continue };
          match first.get(&key) {
              Some(earlier) => {
                  reasons.insert(job.id.clone(), format!("same host, dataset and prefix as job '{earlier}': they would prune each other"));
              }
              None => {
                  first.insert(key, job.id.as_str());
              }
          }
      }
      reasons
  }

  #[cfg(test)]
  impl Job {
      pub fn test_snapshot(id: &str, dataset: &str, prefix: &str, keep: u32) -> Job {
          Job { id: id.into(), host: "local".into(), kind: KIND_SNAPSHOT.into(), pool: None, dataset: Some(dataset.into()), recursive: false, prefix: Some(prefix.into()), keep: Some(keep), schedule: "0 * * * *".into(), catch_up: true, enabled: true, created_at: None }
      }
  }
  ```
  Remove the stale comment above `save` that says the daemon only reads jobs only if it is now wrong; leave it otherwise.

- [ ] **Step 5: Fix the existing literals.** Most end in `created_at: None }` or `created_at: Some(...) }`:

  ```sh
  grep -rlE "created_at: (None|Some\()" src | xargs sed -i -E 's/created_at: (None|Some\([^}]*\)) \}/created_at: \1, dataset: None, recursive: false, prefix: None, keep: None }/'
  cargo build --all-targets 2>&1 | grep -E "^error" -A6 | head -40
  ```
  Fix whatever the compiler still reports by hand (the multi-line literal in `src/ui/state.rs` `build_job_action`'s `None =>` arm gets `dataset: None, recursive: false, prefix: None, keep: None,` after `created_at: Some(now),`).

- [ ] **Step 6: Run the suite**

  Run: `cargo test 2>&1 | grep -E "test result|FAILED|panicked"` — Expected: all pass (the four new tests included).

- [ ] **Step 7: Commit**

  ```sh
  git add -A && git commit -m "feat: the snapshot job kind in the job model, with validation and the prefix-clash rule

  Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
  ```

---

### Task 2: ZFS helpers for recursive create, destroy and per-dataset listing

**Files:**
- Modify: `src/zfs/snapshot.rs` (functions + tests)

**Interfaces:**
- Consumes: `run_checked`, `parse_snapshot_list`, `Snapshot` (existing).
- Produces:
  ```rust
  pub fn create_snapshot_with(runner: &dyn CommandRunner, dataset: &str, name: &str, recursive: bool) -> Result<()>;
  pub fn destroy_snapshot_with(runner: &dyn CommandRunner, dataset: &str, name: &str, recursive: bool) -> Result<()>;
  pub fn list_snapshots_of(runner: &dyn CommandRunner, dataset: &str, recursive: bool) -> Result<Vec<Snapshot>>;
  ```
  `create_snapshot` and `destroy_snapshot` keep their signatures and delegate with `recursive = false`.

- [ ] **Step 1: Write the failing tests** in `src/zfs/snapshot.rs`'s test module:

  ```rust
  fn args_of(runner: &FakeRunner) -> Vec<Vec<String>> {
      runner.calls.lock().unwrap().iter().map(|(_, a)| a.clone()).collect()
  }

  #[test]
  fn create_and_destroy_add_dash_r_only_when_recursive() {
      let r = FakeRunner::new("");
      create_snapshot_with(&r, "tank/data", "h-1", false).unwrap();
      create_snapshot_with(&r, "tank/data", "h-1", true).unwrap();
      destroy_snapshot_with(&r, "tank/data", "h-1", false).unwrap();
      destroy_snapshot_with(&r, "tank/data", "h-1", true).unwrap();
      assert_eq!(
          args_of(&r),
          vec![
              vec!["snapshot", "--", "tank/data@h-1"],
              vec!["snapshot", "-r", "--", "tank/data@h-1"],
              vec!["destroy", "--", "tank/data@h-1"],
              vec!["destroy", "-r", "--", "tank/data@h-1"],
          ]
      );
  }

  #[test]
  fn listing_one_datasets_snapshots_is_depth_one_unless_recursive() {
      let r = FakeRunner::new("tank/data@a\t1\n");
      let flat = list_snapshots_of(&r, "tank/data", false).unwrap();
      let tree = list_snapshots_of(&r, "tank/data", true).unwrap();
      assert_eq!(flat[0].name, "a");
      assert_eq!(tree.len(), 1);
      let calls = args_of(&r);
      assert_eq!(calls[0], vec!["list", "-t", "snapshot", "-H", "-p", "-o", "name,used", "-d", "1", "tank/data"]);
      assert_eq!(calls[1], vec!["list", "-t", "snapshot", "-H", "-p", "-o", "name,used", "-r", "tank/data"]);
  }
  ```
  (Adjust the expected argument lists to whatever Task 1 step 1 proved the real `zfs` accepts.)

- [ ] **Step 2: Run to see them fail:** `cargo test zfs::snapshot 2>&1 | tail` — Expected: `cannot find function`.

- [ ] **Step 3: Implement.** Replace `create_snapshot` and `destroy_snapshot` and add the list function:

  ```rust
  /// The `--` before `target` stops `zfs` from treating a dataset name
  /// that starts with `-` as an option flag (argument injection).
  pub fn create_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
      create_snapshot_with(runner, dataset, name, false)
  }

  /// `zfs snapshot [-r] -- dataset@name`; `-r` snapshots the dataset and every descendant atomically.
  pub fn create_snapshot_with(runner: &dyn CommandRunner, dataset: &str, name: &str, recursive: bool) -> Result<()> {
      let target = format!("{dataset}@{name}");
      let mut args = vec!["snapshot"];
      if recursive {
          args.push("-r");
      }
      args.extend(["--", &target]);
      super::run_checked(runner, "zfs", &args, &format!("zfs snapshot {target}"))?;
      Ok(())
  }

  pub fn destroy_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
      destroy_snapshot_with(runner, dataset, name, false)
  }

  /// `zfs destroy [-r] -- dataset@name`; `-r` also destroys the same-named snapshot of each descendant.
  pub fn destroy_snapshot_with(runner: &dyn CommandRunner, dataset: &str, name: &str, recursive: bool) -> Result<()> {
      let target = format!("{dataset}@{name}");
      let mut args = vec!["destroy"];
      if recursive {
          args.push("-r");
      }
      args.extend(["--", &target]);
      super::run_checked(runner, "zfs", &args, &format!("zfs destroy {target}"))?;
      Ok(())
  }

  /// The snapshots of `dataset` alone (`-d 1`), or of its whole tree when `recursive`.
  pub fn list_snapshots_of(runner: &dyn CommandRunner, dataset: &str, recursive: bool) -> Result<Vec<Snapshot>> {
      let mut args: Vec<&str> = SNAPSHOT_LIST_ARGS.to_vec();
      if recursive {
          args.push("-r");
      } else {
          args.extend(["-d", "1"]);
      }
      args.push(dataset);
      let output = super::run_checked(runner, "zfs", &args, &format!("zfs list -t snapshot {dataset}"))?;
      parse_snapshot_list(&output.stdout)
  }
  ```

- [ ] **Step 4: Run:** `cargo test zfs::snapshot 2>&1 | grep -E "test result|FAILED"` — Expected: pass; then `cargo test 2>&1 | grep "test result"`.

- [ ] **Step 5: Commit** (`feat: recursive snapshot create/destroy and per-dataset snapshot listing`, with the co-author trailer).

---

### Task 3: The snapshot run and retention

**Files:**
- Create: `src/daemon/snapshot.rs`
- Modify: `src/daemon/mod.rs` (add `mod snapshot;`)
- Modify: `src/daemon/scrub.rs` (make `event`, `finish`, `Slot` and its fields `pub(super)`)

**Interfaces:**
- Consumes: `scrub::{Context, Outcome, event, finish, Slot}`; `jobs::{effective_prefix, Job}`; Task 2's zfs functions; `History`, `EventKind`, `RunResult`.
- Produces:
  ```rust
  pub fn snapshot_name(prefix: &str, at: DateTime<Utc>) -> String;
  pub fn is_own(prefix: &str, name: &str) -> bool;
  pub fn run_snapshot(ctx: &Context, runner: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome;
  ```

- [ ] **Step 1: Expose the scrub helpers.** In `src/daemon/scrub.rs` change `fn event(` to `pub(super) fn event(`, `fn finish(` to `pub(super) fn finish(`, `struct Slot {` to `pub(super) struct Slot {` with `pub(super) scheduled_for` and `pub(super) catch_up` fields. Run `cargo build 2>&1 | grep -E "^error"` — Expected: no output.

- [ ] **Step 2: Write the file with tests first.** Create `src/daemon/snapshot.rs` containing only the `#[cfg(test)] mod tests` below plus `use` lines and `todo!()`-free empty stubs is not allowed, so write the tests and the implementation in one file but run the tests *before* adding the implementation body by temporarily commenting it out: simplest is to create the file with just the tests module and the module line `mod snapshot;` and confirm the compile failure, then add the implementation. Test module:

  ```rust
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
              let listing = existing.iter().map(|n| format!("{n}\t1\n")).collect();
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
              self.calls.lock().unwrap().push(std::iter::once(args[0]).chain(args[1..].iter().copied()).map(str::to_string).collect());
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

      #[test]
      fn raising_keep_prunes_nothing_and_lowering_it_prunes_the_excess_oldest_first() {
          let roomy = Zfs::new(&FIVE);
          run(&roomy, &Job::test_snapshot("s", "tank/data", "hourly", 10));
          assert!(roomy.destroyed().is_empty());
          let tight = Zfs::new(&FIVE);
          run(&tight, &Job::test_snapshot("s", "tank/data", "hourly", 2));
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
          let _held = locks.try_acquire("local", "@tank/data").unwrap();
          let ctx = Context { history: &history, clock: &clock, locks: &locks, poll: Duration::from_secs(1), stop: &stop };
          let outcome = run_snapshot(&ctx, &zfs, &Job::test_snapshot("s", "tank/data", "hourly", 1), now(), false, "r");
          assert_eq!(outcome.result, RunResult::Skipped);
          assert!(zfs.calls().is_empty());
      }
  }
  ```
- [ ] **Step 3: Run to see them fail:** `cargo test daemon::snapshot 2>&1 | tail` — Expected: compile errors for `run_snapshot`, `snapshot_name`, `is_own`.

- [ ] **Step 4: Implement** (top of `src/daemon/snapshot.rs`, above the tests):

  ```rust
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
      let b = stamp.as_bytes();
      b.len() == 16 && b[8] == b'T' && b[15] == b'Z' && b[..8].iter().all(u8::is_ascii_digit) && b[9..15].iter().all(u8::is_ascii_digit)
  }

  pub fn run_snapshot(ctx: &Context, runner: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome {
      let dataset = job.dataset.clone().unwrap_or_default();
      let prefix = effective_prefix(job).to_string();
      let keep = job.keep.unwrap_or(1).max(1) as usize;
      let slot = Slot { scheduled_for: Some(scheduled_for), catch_up };
      // `@` cannot start a pool name, so this key never collides with a scrub's `(host, pool)` lock.
      let Some(_guard) = ctx.locks.try_acquire(&job.host, &format!("@{dataset}")) else {
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
      // Newest first. A recursive listing repeats each name once per descendant.
      let mut own: Vec<String> = listed.into_iter().filter(|s| is_own(&prefix, &s.name)).map(|s| s.name).collect();
      own.sort_unstable_by(|a, b| b.cmp(a));
      own.dedup();
      let (mut pruned, mut failures) = (0_usize, Vec::new());
      for old in own.iter().skip(keep) {
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
  ```
  Add `mod snapshot;` to `src/daemon/mod.rs` next to `mod scrub;` (the daemon's `execute` uses it in Task 4; until then add `#[allow(dead_code)]` is NOT needed because Task 4 follows immediately — if clippy warns in between, ignore until Task 4).
  If `finish`'s `ctx.history.append` or `Slot` need `Copy`, they already are (`Slot` is `Clone, Copy`).

- [ ] **Step 5: Run:** `cargo test daemon::snapshot 2>&1 | grep -E "test result|FAILED|panicked"` — Expected: all pass. Then `cargo test 2>&1 | grep "test result"`.

- [ ] **Step 6: Commit** (`feat: the snapshot run: take a snapshot, then prune the job's own beyond keep`).

---

### Task 4: Daemon dispatch, the prefix-clash rule at load, and restart handling

**Files:**
- Modify: `src/daemon/mod.rs` (`execute`)
- Modify: `src/daemon/engine.rs` (`hand_out_pending` + test)
- Modify: `src/daemon/scheduler.rs` (`reload` + test)
- Modify: `src/ui/jobs.rs` (`load`'s `unsupported` + test)

**Interfaces:**
- Consumes: Task 1 `jobs::{KIND_SNAPSHOT, duplicate_prefix_reasons}`; Task 3 `snapshot::run_snapshot`.

- [ ] **Step 1: Write the failing tests.**

  In `src/daemon/engine.rs` tests (helpers `started`, `engine`, `at` already exist there; `Job::test_snapshot` makes the job):

  ```rust
  #[test]
  fn an_unfinished_snapshot_run_is_closed_as_lost_not_resumed_as_a_scrub() {
      let dir = tempfile::tempdir().unwrap();
      let paths = Paths::new(dir.path());
      jobs::save(&paths.jobs(), &[Job::test_snapshot("snap", "tank/data", "hourly", 3)]).unwrap();
      History::new(paths.history()).append(&started("snap", "r1", "2026-10-07T03:00:00Z")).unwrap();
      let work = engine(&dir, "2026-10-07T04:00:00Z").start(at("2026-10-07T04:00:00Z"));
      assert!(work.iter().any(|w| matches!(w, Work::Orphan(run) if run.run_id == "r1")), "{work:?}");
      assert!(!work.iter().any(|w| matches!(w, Work::Resume { .. })), "{work:?}");
  }
  ```
  In `src/daemon/scheduler.rs` tests (the module already has `at`, `hosts()`, `HashMap` and `Utc`):

  ```rust
  #[test]
  fn the_later_of_two_snapshot_jobs_with_one_host_dataset_and_prefix_is_unsupported() {
      let mut s = Scheduler::new(Utc);
      let a = Job::test_snapshot("a", "tank/data", "hourly", 3);
      let b = Job::test_snapshot("b", "tank/data", "hourly", 5);
      let unsupported = s.reload(vec![a, b], &hosts(), &HashMap::new(), at("2026-10-06T00:00:00Z"));
      assert_eq!(unsupported.len(), 1);
      assert_eq!(unsupported[0].id, "b");
      assert!(unsupported[0].reason.contains("prune each other"), "{}", unsupported[0].reason);
      assert_eq!(s.len(), 1);
  }
  ```
  

  In `src/ui/jobs.rs` tests:

  ```rust
  #[test]
  fn load_reports_the_prefix_clash_as_a_reason_on_the_later_job() {
      let dir = tempfile::tempdir().unwrap();
      jobs::save(&dir.path().join("jobs.toml"), &[Job::test_snapshot("a", "tank/data", "hourly", 3), Job::test_snapshot("b", "tank/data", "hourly", 5)]).unwrap();
      let view = load(dir.path(), &["local".to_string()], None);
      assert!(!view.unsupported.contains_key("a"));
      assert!(view.unsupported["b"].contains("prune each other"));
  }
  ```

- [ ] **Step 2: Run to see them fail:** `cargo test an_unfinished_snapshot the_later_of_two load_reports_the_prefix 2>&1 | tail -20` — Expected: failures (Resume handed out; no clash reason).

- [ ] **Step 3: Implement.**

  `src/daemon/engine.rs`, `hand_out_pending`: the `find` predicate becomes
  ```rust
  jobs.iter().find(|j| j.id == run.job_id && j.kind == jobs::KIND_SCRUB && jobs::resolve(j, &self.hosts).is_ok())
  ```
  (a snapshot job's unfinished run falls to `None` and is closed as lost).

  `src/daemon/scheduler.rs`, `reload`: before the `for`, `let clashes = jobs::duplicate_prefix_reasons(&jobs);`, and right after the `resolve` match (before the `enabled` check):
  ```rust
  if let Some(reason) = clashes.get(&job.id) {
      unsupported.push(Unsupported { id: job.id.clone(), reason: reason.clone() });
      continue;
  }
  ```
  `src/ui/jobs.rs`, `load`: compute `let clashes = jobs::duplicate_prefix_reasons(&jobs);` and change the `unsupported` expression to
  ```rust
  let unsupported = jobs
      .iter()
      .filter_map(|j| jobs::resolve(j, hosts).err().or_else(|| clashes.get(&j.id).cloned()).map(|reason| (j.id.clone(), reason)))
      .collect();
  ```
  `src/daemon/mod.rs`, `execute`, the `Work::Run(due)` arm: replace the `if let Some(outcome) = scrub::run_scrub(...)` with
  ```rust
  let outcome = if due.job.kind == crate::jobs::KIND_SNAPSHOT {
      Some(snapshot::run_snapshot(&ctx, runner.as_ref(), &due.job, due.scheduled_for, due.catch_up, &run_id))
  } else {
      scrub::run_scrub(&ctx, runner.as_ref(), &due.job, due.scheduled_for, due.catch_up, &run_id)
  };
  if let Some(outcome) = outcome {
      log(&format!("{} {:?}: {}", outcome.job_id, outcome.result, outcome.message));
      self.notify(&due.job, &outcome);
  }
  ```

- [ ] **Step 4: Run:** `cargo test 2>&1 | grep -E "test result|FAILED|panicked"`; `cargo clippy --all-targets 2>&1 | grep -c "^warning"` — Expected: all pass; warning count not above the baseline (5 lines including the summary).

- [ ] **Step 5: Add a daemon-level test** in `src/daemon/mod.rs` tests, next to `an_orphaned_run_is_closed_as_failed` (it uses that module's `shared` and `FakeRunner`; `FakeRunner::new("")` answers every zfs call with success and empty output, so the listing is empty and nothing is pruned). The module's `FixedClock` starts at 2026-10-07T03:00:00Z:

  ```rust
  #[test]
  fn a_due_snapshot_job_runs_through_the_snapshot_runner_and_is_recorded() {
      let dir = tempfile::tempdir().unwrap();
      let shared = shared(&dir, FakeRunner::new(""), None);
      let due = Due { job: Job::test_snapshot("snap", "tank/data", "hourly", 3), scheduled_for: shared.clock.now(), catch_up: false };
      shared.execute(Work::Run(due));
      let runs = history::runs(&shared.history.read());
      assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Ok)));
      assert!(runs[0].message.starts_with("created hourly-20261007T030000Z"), "{}", runs[0].message);
  }
  ```
  Run it (`cargo test a_due_snapshot_job`), then commit (`feat: the daemon runs snapshot jobs, rejects prefix clashes and never resumes one as a scrub`).

---

### Task 5: The snapshot job form in the TUI (open, validate, save, edit)

**Files:**
- Modify: `src/ui/modal.rs` (`FormKind::SnapshotJob`)
- Modify: `src/ui/state.rs` (form builder, action builder, keys `N` and Datasets `J`, Enter dispatch, submit branch, the `build_pending`-style `None` arm near line 2490)
- Modify: `src/ui/jobs.rs` (`new_snapshot_job_id`, shared unique-id helper)

**Interfaces:**
- Consumes: Task 1 `jobs::{resolve, clashing_job, valid_dataset, valid_prefix, DEFAULT_PREFIX}`.
- Produces:
  ```rust
  // modal.rs: fields = [host, dataset, recursive (y/n), prefix, keep, schedule, catch_up (y/n)]
  FormKind::SnapshotJob { hosts: Vec<String>, existing: Option<Box<crate::jobs::Job>>, jobs: Vec<crate::jobs::Job> }
  // ui/jobs.rs
  pub fn new_snapshot_job_id(host: &str, dataset: &str, prefix: &str, existing: &[String]) -> String; // snap-<host>-<dataset>-<prefix>[-N]
  // state.rs (private)
  fn snapshot_job_form(&self, existing: Option<Job>, dataset: &str) -> Modal;
  fn build_snapshot_job_action(kind: &FormKind, fields: &[FormField], state_dir: &Path, now: DateTime<Utc>) -> Result<PendingAction, String>;
  ```

- [ ] **Step 1: Write the failing tests.** In `src/ui/jobs.rs` tests:

  ```rust
  #[test]
  fn snapshot_job_ids_are_safe_file_keys_and_unique() {
      assert_eq!(new_snapshot_job_id("local", "tank/data", "hourly", &[]), "snap-local-tank-data-hourly");
      let taken = vec!["snap-local-tank-data-hourly".to_string()];
      assert_eq!(new_snapshot_job_id("local", "tank/data", "hourly", &taken), "snap-local-tank-data-hourly-2");
  }
  ```
  In `src/ui/state.rs` tests (helpers `app_with_jobs`, `form_values`, `at_utc` already exist):

  ```rust
  #[test]
  fn n_in_jobs_opens_a_blank_snapshot_form_with_sensible_defaults() {
      let (mut app, _dir) = app_with_jobs(&[]);
      app.handle_key(KeyCode::Char('N'));
      assert_eq!(form_values(&app), ["local", "", "n", "auto", "24", "0 * * * *", "y"]);
  }

  #[test]
  fn j_on_a_dataset_prefills_the_snapshot_form() {
      let mut app = datasets_app_with_mount(Some("/tank/data"));
      app.handle_key(KeyCode::Char('J'));
      let values = form_values(&app);
      assert_eq!(values[1], app.selected_dataset_name().unwrap());
  }

  #[test]
  fn a_valid_snapshot_form_builds_a_save_of_a_new_job_with_a_fresh_id_and_creation_time() {
      let now = at_utc("2026-10-06T12:00:00Z");
      let action = build_snapshot_job_action(&snapshot_kind(None, vec![]), &snap_fields(["local", " tank/data ", "y", "hourly", "24", "0 * * * *", "y"]), std::path::Path::new("/state"), now).unwrap();
      assert_eq!(action.section, Section::Jobs);
      let MutationCall::SaveJob { state_dir, job } = action.call else { panic!("{:?}", action.call) };
      assert_eq!(state_dir, std::path::PathBuf::from("/state"));
      assert_eq!((job.id.as_str(), job.kind.as_str(), job.dataset.as_deref(), job.recursive, job.prefix.as_deref(), job.keep), ("snap-local-tank-data-hourly", crate::jobs::KIND_SNAPSHOT, Some("tank/data"), true, Some("hourly"), Some(24)));
      assert!(job.pool.is_none() && job.catch_up && job.enabled);
      assert_eq!(job.created_at, Some(now));
  }

  #[test]
  fn blank_prefix_and_blank_yes_no_fields_take_their_defaults() {
      let now = at_utc("2026-10-06T12:00:00Z");
      let action = build_snapshot_job_action(&snapshot_kind(None, vec![]), &snap_fields(["local", "tank", "", "", "3", "0 * * * *", ""]), std::path::Path::new("/s"), now).unwrap();
      let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
      assert_eq!((job.recursive, job.prefix.as_deref(), job.catch_up), (false, Some("auto"), true));
  }

  #[test]
  fn an_edited_snapshot_job_keeps_its_id_creation_time_and_enabled_flag() {
      let mut existing = crate::jobs::Job::test_snapshot("keep-me", "tank/data", "hourly", 3);
      existing.enabled = false;
      existing.created_at = Some(at_utc("2026-01-01T00:00:00Z"));
      let kind = snapshot_kind(Some(existing.clone()), vec![existing]);
      let action = build_snapshot_job_action(&kind, &snap_fields(["local", "tank/other", "n", "daily", "9", "0 2 * * *", "n"]), std::path::Path::new("/s"), at_utc("2026-10-06T12:00:00Z")).unwrap();
      let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
      assert_eq!((job.id.as_str(), job.enabled, job.created_at), ("keep-me", false, Some(at_utc("2026-01-01T00:00:00Z"))));
      assert_eq!((job.dataset.as_deref(), job.prefix.as_deref(), job.keep, job.catch_up), (Some("tank/other"), Some("daily"), Some(9), false));
  }
  ```

  And the validation tests, via `build_snapshot_job_action` directly (the helpers `snapshot_kind`, `snap_fields`, `snap_err` below are shared by every test in this task; `at_utc` already exists in the test module):

  ```rust
  fn snapshot_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
      FormKind::SnapshotJob { hosts: vec!["local".to_string()], existing: existing.map(Box::new), jobs }
  }
  fn snap_fields(values: [&str; 7]) -> Vec<FormField> {
      values.iter().map(|v| FormField { label: String::new(), value: (*v).to_string() }).collect()
  }
  fn snap_err(values: [&str; 7], jobs: Vec<crate::jobs::Job>) -> String {
      build_snapshot_job_action(&snapshot_kind(None, jobs), &snap_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).err().expect("should be refused")
  }

  #[test]
  fn the_snapshot_form_refuses_what_the_daemon_would_refuse() {
      let good = ["local", "tank/data", "n", "hourly", "24", "0 * * * *", "y"];
      assert!(build_snapshot_job_action(&snapshot_kind(None, vec![]), &snap_fields(good), std::path::Path::new("/x"), chrono::Utc::now()).is_ok());
      assert!(snap_err(["nope", "tank/data", "n", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Unknown host"));
      assert!(snap_err(["local", "", "n", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Dataset"));
      assert!(snap_err(["local", "-rf", "n", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Dataset"));
      assert!(snap_err(["local", "tank/data", "maybe", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Recursive"));
      assert!(snap_err(["local", "tank/data", "n", "a b", "24", "0 * * * *", "y"], vec![]).contains("Prefix"));
      assert!(snap_err(["local", "tank/data", "n", "hourly", "0", "0 * * * *", "y"], vec![]).contains("Keep"));
      assert!(snap_err(["local", "tank/data", "n", "hourly", "x", "0 * * * *", "y"], vec![]).contains("Keep"));
      assert!(snap_err(["local", "tank/data", "n", "hourly", "24", "bogus", "y"], vec![]).contains("Schedule"));
      assert!(snap_err(["local", "tank/data", "n", "hourly", "24", "0 * * * *", "q"], vec![]).contains("Catch up"));
  }

  #[test]
  fn the_snapshot_form_refuses_a_prefix_another_job_already_uses_on_that_dataset() {
      let other = crate::jobs::Job::test_snapshot("other", "tank/data", "hourly", 5);
      assert!(snap_err(["local", "tank/data", "n", "hourly", "24", "0 * * * *", "y"], vec![other.clone()]).contains("other"));
      // Editing that same job is fine, as is a different prefix.
      let kind = snapshot_kind(Some(other.clone()), vec![other]);
      assert!(build_snapshot_job_action(&kind, &snap_fields(["local", "tank/data", "n", "hourly", "9", "0 * * * *", "y"]), std::path::Path::new("/x"), chrono::Utc::now()).is_ok());
  }

  #[test]
  fn enter_on_a_snapshot_job_edits_it_in_the_snapshot_form() {
      let (mut app, _dir) = app_with_jobs(&[crate::jobs::Job::test_snapshot("s", "tank/data", "hourly", 7)]);
      app.refresh_jobs();
      app.handle_key(KeyCode::Down);
      app.handle_key(KeyCode::Enter);
      assert_eq!(form_values(&app), ["local", "tank/data", "n", "hourly", "7", "0 * * * *", "y"]);
  }
  ```

- [ ] **Step 2: Run to see them fail:** `cargo test snapshot_form n_in_jobs j_on_a_dataset snapshot_job 2>&1 | tail` — Expected: compile errors for `FormKind::SnapshotJob`, `build_snapshot_job_action`.

- [ ] **Step 3: Implement.**

  `src/ui/modal.rs`, in `FormKind` after `Job`:
  ```rust
      /// Fields: `[host, dataset, recursive (y/n), prefix, keep, schedule, catch_up (y/n)]`. `jobs`
      /// is every current job, so the form can refuse a prefix another job already uses on the
      /// same host and dataset and can name a new job without a collision.
      SnapshotJob { hosts: Vec<String>, existing: Option<Box<crate::jobs::Job>>, jobs: Vec<crate::jobs::Job> },
  ```
  `src/ui/jobs.rs`: replace the body of `new_job_id` with a call to a shared helper and add the new one:
  ```rust
  fn safe_key(s: &str) -> String {
      s.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '-' }).collect()
  }

  fn unique(base: String, existing: &[String]) -> String {
      let mut candidate = base.clone();
      let mut n = 1_u32;
      while existing.contains(&candidate) {
          n += 1;
          candidate = format!("{base}-{n}");
      }
      candidate
  }

  /// `scrub-<host>-<pool>`, made safe for a file key, with `-2`, `-3`, ... added if taken.
  pub fn new_job_id(host: &str, pool: &str, existing: &[String]) -> String {
      unique(format!("scrub-{}-{}", safe_key(host), safe_key(pool)), existing)
  }

  /// `snap-<host>-<dataset>-<prefix>`, made safe the same way.
  pub fn new_snapshot_job_id(host: &str, dataset: &str, prefix: &str, existing: &[String]) -> String {
      unique(format!("snap-{}-{}-{}", safe_key(host), safe_key(dataset), safe_key(prefix)), existing)
  }
  ```
  `src/ui/state.rs`: add next to `job_form`:
  ```rust
  impl App {
      fn snapshot_job_form(&self, existing: Option<crate::jobs::Job>, dataset: &str) -> Modal {
          let yn = |b: bool| if b { "y" } else { "n" }.to_string();
          let (title, host, dataset, recursive, prefix, keep, schedule, catch_up) = match &existing {
              Some(j) => (format!("Edit snapshot job '{}'", j.id), j.host.clone(), j.dataset.clone().unwrap_or_default(), j.recursive, crate::jobs::effective_prefix(j).to_string(), j.keep.unwrap_or(1), j.schedule.clone(), j.catch_up),
              None => ("New snapshot job".to_string(), self.active_host_name().to_string(), dataset.to_string(), false, crate::jobs::DEFAULT_PREFIX.to_string(), 24, "0 * * * *".to_string(), true),
          };
          Modal::Form {
              title,
              fields: vec![
                  FormField { label: "Host".to_string(), value: host },
                  FormField { label: "Dataset".to_string(), value: dataset },
                  FormField { label: "Recursive (y/n)".to_string(), value: yn(recursive) },
                  FormField { label: "Prefix".to_string(), value: prefix },
                  FormField { label: "Keep (newest N)".to_string(), value: keep.to_string() },
                  FormField { label: "Schedule".to_string(), value: schedule },
                  FormField { label: "Catch up (y/n)".to_string(), value: yn(catch_up) },
              ],
              focused: 0,
              description: Some("Only snapshots named <prefix>-<timestamp> made by this job are ever pruned.".to_string()),
              kind: FormKind::SnapshotJob { hosts: self.hosts.clone(), existing: existing.map(Box::new), jobs: self.jobs_view.jobs.clone() },
          }
      }
  }
  ```
  ```rust
  /// Turns a submitted snapshot-job form into a save, or says what is wrong. It refuses exactly
  /// what the daemon would refuse (`jobs::resolve`) plus a prefix another job already uses.
  fn build_snapshot_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
      let FormKind::SnapshotJob { hosts, existing, jobs } = kind else { return Err("not a snapshot job form".to_string()) };
      let host = field_value(fields, 0).trim();
      if !hosts.iter().any(|h| h == host) {
          return Err(format!("Unknown host '{host}'. Configured hosts: {}", hosts.join(", ")));
      }
      let dataset = field_value(fields, 1).trim();
      if !crate::jobs::valid_dataset(dataset) {
          return Err("Dataset must look like tank/data (letters, digits and _ - : . /)".to_string());
      }
      let yes_no = |label: &str, raw: &str, blank: bool| match raw.trim().to_lowercase().as_str() {
          "" => Ok(blank),
          "y" | "yes" => Ok(true),
          "n" | "no" => Ok(false),
          other => Err(format!("{label} must be y or n, not '{other}'")),
      };
      let recursive = yes_no("Recursive", field_value(fields, 2), false)?;
      let prefix = match field_value(fields, 3).trim() {
          "" => crate::jobs::DEFAULT_PREFIX,
          p => p,
      };
      if !crate::jobs::valid_prefix(prefix) {
          return Err("Prefix may only use letters, digits, _ - and .".to_string());
      }
      let keep: u32 = field_value(fields, 4).trim().parse().ok().filter(|k| *k >= 1).ok_or_else(|| "Keep must be a whole number, at least 1".to_string())?;
      let schedule = field_value(fields, 5).trim();
      crate::daemon::cron::Schedule::parse(schedule).map_err(|e| format!("Schedule: {e}"))?;
      let catch_up = yes_no("Catch up", field_value(fields, 6), true)?;
      let taken: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
      let job = match existing {
          Some(old) => crate::jobs::Job { host: host.to_string(), dataset: Some(dataset.to_string()), recursive, prefix: Some(prefix.to_string()), keep: Some(keep), schedule: schedule.to_string(), catch_up, ..(**old).clone() },
          None => crate::jobs::Job {
              id: crate::ui::jobs::new_snapshot_job_id(host, dataset, prefix, &taken),
              host: host.to_string(),
              kind: crate::jobs::KIND_SNAPSHOT.to_string(),
              pool: None,
              dataset: Some(dataset.to_string()),
              recursive,
              prefix: Some(prefix.to_string()),
              keep: Some(keep),
              schedule: schedule.to_string(),
              catch_up,
              enabled: true,
              created_at: Some(now),
          },
      };
      if let Some(other) = crate::jobs::clashing_job(jobs, &job) {
          return Err(format!("Job '{}' already snapshots {dataset} with prefix '{prefix}': they would prune each other. Use another prefix.", other.id));
      }
      Ok(PendingAction { host: 0, section: Section::Jobs, call: MutationCall::SaveJob { state_dir: state_dir.to_path_buf(), job: Box::new(job) } })
  }
  ```
  Keys, in the `handle_key` match next to the existing job keys:
  ```rust
  KeyCode::Char('N') if self.current_section() == Section::Jobs => {
      self.modal = Some(self.snapshot_job_form(None, ""));
  }
  KeyCode::Char('J') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => match self.selected_dataset_name() {
      Some(dataset) => self.modal = Some(self.snapshot_job_form(None, &dataset)),
      None => self.status_message = Some("Select a dataset first, then press J to schedule snapshots of it".to_string()),
  },
  ```
  Change the Jobs `Enter` arm to
  ```rust
  KeyCode::Enter if self.current_section() == Section::Jobs => {
      if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)).cloned() {
          self.modal = Some(if job.kind == crate::jobs::KIND_SNAPSHOT { self.snapshot_job_form(Some(job), "") } else { self.job_form(Some(job.clone()), job.pool.as_deref().unwrap_or("")) });
      }
  }
  ```
  In the form-submit chain (where `matches!(kind, FormKind::Job { .. })` handles the job form) add a sibling branch:
  ```rust
  } else if matches!(kind, FormKind::SnapshotJob { .. }) {
      match build_snapshot_job_action(&kind, &fields, &self.state_dir, chrono::Utc::now()) {
          Ok(mut action) => {
              action.host = self.selected_host;
              Some(action)
          }
          Err(msg) => {
              self.set_error(msg);
              self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
              None
          }
      }
  ```
  and at the `FormKind::AddHost | FormKind::ShareSmb { .. } | FormKind::Job { .. } => None` arm add `| FormKind::SnapshotJob { .. }`.

- [ ] **Step 4: Run:** `cargo test 2>&1 | grep -E "test result|FAILED|panicked"` — Expected: all pass. Fix any exhaustive-match compile errors the new `FormKind` variant causes (the compiler lists them; `picker.rs` and `layout.rs` get their real handling in Task 6, so give them `_ =>` fallthroughs only if the compiler demands).

- [ ] **Step 5: Commit** (`feat: the snapshot job form: N in Jobs, J on a dataset, edit with Enter`).

---

### Task 6: Pickers, table, detail pane, hints and help

**Files:**
- Modify: `src/ui/picker.rs` (`LiveNames`, snapshot-form picker fields)
- Modify: `src/ui/state.rs` (`pending_pool_fetch` carries the kind; `open_pool_picker`; `fetch_pool_names`; existing tests that compare `pending_pool_fetch`)
- Modify: `src/ui/mod.rs` (`apply_pending_pool_fetches`)
- Modify: `src/ui/layout.rs` (`form_preview`, Target column, job panel, picker "Loading" text, empty-state hint)
- Modify: `src/ui/hints.rs` (Jobs `N`, Datasets `J`)
- Modify: `src/ui/state.rs` help text (lines near `n  new scrub job` and `J  schedule a recurring scrub`)

**Interfaces:**
- Produces:
  ```rust
  // picker.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum LiveNames { Pools, Datasets }
  // state.rs: pub pending_pool_fetch: Vec<(usize, LiveNames)>
  // state.rs: pub fn fetch_pool_names(&mut self, runner: Arc<dyn CommandRunner>, host: usize, what: LiveNames)
  ```

- [ ] **Step 1: Write the failing tests.**

  `src/ui/state.rs`: update the pool-picker tests added earlier so `assert_eq!(app.pending_pool_fetch, vec![1]);` becomes `vec![(1, crate::ui::picker::LiveNames::Pools)]`, then add:

  ```rust
  #[test]
  fn f2_on_the_snapshot_forms_dataset_field_queues_a_dataset_fetch_for_the_chosen_host() {
      let (mut app, _dir) = app_with_jobs(&[]);
      app.handle_key(KeyCode::Char('N'));
      app.handle_modal_key(KeyCode::Tab);
      app.handle_modal_key(KeyCode::F(2));
      assert_eq!(app.pending_pool_fetch, vec![(0, crate::ui::picker::LiveNames::Datasets)]);
      app.tx.send(WorkerMsg::PoolNames(0, Ok(vec!["tank".to_string(), "tank/data".to_string()]))).unwrap();
      app.drain_worker_messages();
      app.handle_picker_key(KeyCode::Down);
      app.handle_picker_key(KeyCode::Enter);
      assert_eq!(form_values(&app)[1], "tank/data");
  }

  #[test]
  fn f2_on_the_snapshot_forms_host_and_schedule_fields_opens_their_pickers_and_the_rest_do_nothing() {
      let (mut app, _dir) = app_with_jobs(&[]);
      app.handle_key(KeyCode::Char('N'));
      app.handle_modal_key(KeyCode::F(2));
      assert_eq!(app.picker.as_ref().unwrap().values, vec!["local"]);
      app.picker = None;
      for _ in 0..5 {
          app.handle_modal_key(KeyCode::Tab);
      }
      app.handle_modal_key(KeyCode::F(2));
      assert_eq!(app.picker.as_ref().unwrap().values, crate::ui::jobs::preset_values());
      app.picker = None;
      app.handle_modal_key(KeyCode::Tab); // Catch up
      app.handle_modal_key(KeyCode::F(2));
      assert!(app.picker.is_none());
  }
  ```
  `src/ui/mod.rs`: update its pool-fetch test (if one exists from the earlier change) to the tuple form; add a test that `apply_pending_pool_fetches` with `(0, LiveNames::Datasets)` and a `FakeRunner` routed `("zfs","list")` returning `tank\t1\t2\t3\t-\tfilesystem\n`-style dataset output (copy the exact fixture format from `zfs::dataset::tests`) delivers dataset names.

  `src/ui/layout.rs` tests (helpers `jobs_app`, `a_job`, `render` exist): 

  ```rust
  #[test]
  fn a_snapshot_job_shows_its_dataset_as_the_target_and_its_keep_prefix_and_recursion_in_the_panel() {
      let mut snap = crate::jobs::Job::test_snapshot("s", "tank/data", "hourly", 24);
      snap.recursive = true;
      let (mut app, _dir) = jobs_app(&[snap]);
      app.handle_key(crossterm::event::KeyCode::Down);
      let text = render(&mut app, 120, 30).join("\n");
      assert!(text.contains("Target"), "{text}");
      assert!(text.contains("tank/data"), "{text}");
      assert!(text.contains("keep 24") && text.contains("hourly") && text.contains("recursive"), "{text}");
  }

  #[test]
  fn the_snapshot_forms_preview_follows_its_schedule_field() {
      let mut app = job_form_app();
      app.handle_key(crossterm::event::KeyCode::Esc);
      app.handle_key(crossterm::event::KeyCode::Char('N'));
      let text = render(&mut app, 120, 30).join("\n");
      assert!(text.contains("Hourly"), "the default 0 * * * * is the Hourly preset:\n{text}");
  }
  ```
  (`job_form_app` builds an app already on the Jobs section; if `Esc`ing its open modal is awkward, build the app the way `job_form_app` does without opening the form.)

  `src/ui/hints.rs`: extend the Jobs hint test (near line 483) to assert `N` is advertised, and add a Datasets-section test that `J` is advertised when a dataset is selected (copy the neighbouring Pools `J` hint test).

- [ ] **Step 2: Run to see them fail:** `cargo test 2>&1 | grep -E "^error|FAILED" | head` — Expected: compile errors (`LiveNames`) then failures.

- [ ] **Step 3: Implement.**

  `src/ui/picker.rs`: add
  ```rust
  /// What a live-fetched name picker lists.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum LiveNames {
      Pools,
      Datasets,
  }
  ```
  Replace the `if let FormKind::Job { hosts, .. } = kind {` block in `build_picker_state` with one that handles both forms (`host_field` 0 in both; live-name field 1 in both; schedule field 2 for `Job`, 5 for `SnapshotJob`):
  ```rust
  let job_form = match kind {
      FormKind::Job { hosts, .. } => Some((hosts, 2)),
      FormKind::SnapshotJob { hosts, .. } => Some((hosts, 5)),
      _ => None,
  };
  if let Some((hosts, schedule_field)) = job_form {
      if field_index == 1 {
          // The host is whatever the form's Host field says at F2 time; `open_pool_picker` fills it in.
          return Some(PickerState { values: Vec::new(), filter: String::new(), selected: 0, checked: std::collections::HashSet::new(), multi_select: false, load: PickerLoad::Loading, note: None, account: None, pool_host: Some(0) });
      }
      let (values, selected) = if field_index == 0 {
          (hosts.clone(), hosts.iter().position(|h| h == current_value.trim()).unwrap_or(0))
      } else if field_index == schedule_field {
          let presets = crate::ui::jobs::preset_values();
          let selected = presets.iter().position(|p| crate::ui::jobs::insert_text(p) == current_value.trim()).unwrap_or(0);
          (presets, selected)
      } else {
          return None;
      };
      return Some(PickerState { values, filter: String::new(), selected, checked: std::collections::HashSet::new(), multi_select: false, load: PickerLoad::Ready, note: None, account: None, pool_host: None });
  }
  ```
  `src/ui/state.rs`: change the field to `pub pending_pool_fetch: Vec<(usize, crate::ui::picker::LiveNames)>`; `open_pool_picker` becomes
  ```rust
  fn open_pool_picker(&mut self, picker: &mut PickerState, kind: &FormKind, host_name: &str) {
      let (hosts, what) = match kind {
          FormKind::Job { hosts, .. } => (hosts, crate::ui::picker::LiveNames::Pools),
          FormKind::SnapshotJob { hosts, .. } => (hosts, crate::ui::picker::LiveNames::Datasets),
          _ => return,
      };
      match hosts.iter().position(|h| h == host_name.trim()) {
          Some(host) => {
              picker.pool_host = Some(host);
              self.pending_pool_fetch.push((host, what));
          }
          None => picker.load = crate::ui::picker::PickerLoad::Failed(format!("unknown host '{}'", host_name.trim())),
      }
  }
  ```
  `fetch_pool_names` takes `what` and maps: `LiveNames::Pools => list_pools(..).map(|v| v.into_iter().map(|p| p.name).collect())`, `LiveNames::Datasets => crate::zfs::dataset::list_datasets(..).map(|v| v.into_iter().map(|d| d.name).collect())`. `fail_pool_fetch` is unchanged.
  `src/ui/mod.rs`: `for (host, what) in std::mem::take(&mut app.pending_pool_fetch) { ... app.fetch_pool_names(h.runner.clone(), host, what) ... }`.

  `src/ui/layout.rs`: `form_preview` gets a second arm `crate::ui::modal::FormKind::SnapshotJob { .. } => preview_lines(fields.get(5)...)`. In `draw_jobs` the row's third cell becomes `j.pool.clone().or_else(|| j.dataset.clone()).unwrap_or_default()`, the header `"Target"`, and the widths `[22, 8, 16, 20, 15, Min(10)]`. In `draw_jobs_panel`, after the `catch up` line push:
  ```rust
  if job.kind == crate::jobs::KIND_SNAPSHOT {
      info.push(Line::from(format!("keep {}  prefix {}{}", job.keep.unwrap_or(0), crate::jobs::effective_prefix(job), if job.recursive { "  recursive" } else { "" })));
  }
  ```
  The picker's "Loading accounts…"/"Could not load accounts" text: for `pool_host.is_some()` use `names` (`let what = if picker.pool_host.is_some() { "names" } else { "accounts" };`) and update any layout test that asserts "pools". The empty-state text becomes `"No scheduled jobs. Press 'n' for a scrub job or 'N' for a snapshot job, or 'J' on a pool (Pools) or a dataset (Datasets)."`.

  `src/ui/hints.rs`: in the `(Section::Jobs, _)` arm add `v.push(h("N", "New snapshot job", 3));` after the `n` hint and rename `n`'s label to `"New scrub job"`; in the Datasets arm mirror how the Pools arm advertises `J` (find it with `grep -n '"J"' src/ui/hints.rs`) to advertise `J` "Snapshot job" when a dataset is selected and the Middle pane has focus.

  Help text in `src/ui/state.rs`: after the `J  schedule a recurring scrub of the selected pool` line add `  J                (in Datasets) schedule recurring snapshots of the selected dataset`; after `n  new scrub job (or press J on a pool in Pools)` add `  N                new snapshot job (or press J on a dataset in Datasets)`. Extend the `F2` help line's list with `a snapshot job's Host, Dataset and Schedule`.

- [ ] **Step 4: Run:** `cargo test 2>&1 | grep -E "test result|FAILED|panicked"`; `cargo clippy --all-targets 2>&1 | grep -c "^warning"` — Expected: all pass; clippy count at the baseline (5).

- [ ] **Step 5: Commit** (`feat: F2 pickers, the Target column and keys/help for snapshot jobs`).

---

### Task 7: README and the live check on the hub

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-06-snapshot-schedules-design.md` (live results)

- [ ] **Step 1: Document it.** In README "Scheduled jobs" section: change "The only job kind so far is **scrub**" to name both kinds; add after the scrub job example a snapshot example (the TOML from the spec), a paragraph stating "A snapshot job names its snapshots `<prefix>-<UTC timestamp>` and keeps the newest `keep` of them; it only ever destroys snapshots matching exactly that pattern, so manual snapshots and other jobs' are never touched. Two enabled snapshot jobs cannot share a host, dataset and prefix.", and the keys: `N` (new snapshot job) in Jobs and `J` on a dataset in Datasets. Add the dataset/prefix/keep rules to the schedule paragraph where it fits.

- [ ] **Step 2: Run everything:** `cargo test 2>&1 | grep "test result"; cargo clippy --all-targets 2>&1 | grep -c "^warning"` — Expected: all pass, clippy at the baseline. Commit (`docs: snapshot jobs`).

- [ ] **Step 3: Deploy to the hub** (same pattern as before: tar `Cargo.toml Cargo.lock README.md src contrib docs`, `rm -rf src` first on the hub side, `cargo build --release`, `sudo install -m 755 target/release/zfsctl /usr/local/bin/zfsctl`, `sudo service zfsctl restart`). Confirm `daemon.json` heartbeat resumes and `/var/log/zfsctl.log` shows the start line.

- [ ] **Step 4: Live end-to-end check (a clean `zfs list` round trip proves nothing; check the real effects).**
  1. Write a job: `[[jobs]] id="live-snap" host="local" kind="snapshot" dataset="scratch/data" prefix="live" keep=3 schedule="* * * * *"` to `/var/db/zfsctl/jobs.toml` as root.
  2. `sudo zfs snapshot scratch/data@manual` first. Wait about 5 minutes. Expect exactly three `live-<timestamp>` snapshots (`sudo zfs list -t snapshot -o name -d 1 scratch/data`), the `manual` one still present, and `history.jsonl` showing `ok` finishes with "pruned" counts.
  3. Hold the oldest `live-` snapshot (`sudo zfs hold keep <snap>`), wait for a run: expect a `failed` run whose message names that snapshot and "hold", with the other excess snapshots still pruned and the Jobs section showing the failed-jobs indicator. Release the hold.
  4. In the TUI Jobs section: `N`, F2 on Dataset lists `scratch`, `scratch/data`; save a job from the form; `Enter` re-opens it in the snapshot form; the Target column and panel show dataset, keep, prefix.
  5. Try a duplicate prefix in the form and see the refusal naming the other job.
  6. Clean up: delete the test jobs (`d` in the TUI, or empty `jobs.toml`), `sudo zfs destroy` the leftover `live-*` and `manual` snapshots, and make sure nothing else on `scratch` changed.
  7. Repeat steps 1-3 on the 16.0-CURRENT VM if the Task 1 flag check showed any difference between versions; otherwise note that the Task 1 results already covered both.
  Record what you saw (and any bug found, with its fix) at the end of the spec's Testing section.

- [ ] **Step 5: Commit the spec's live results** (`docs: live results for snapshot jobs`), then merge as the user directs (the repo's default branch is `master`).
