# Unprotected Datasets Check Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tell the operator, on the Dashboard, by email and in the daily report, which datasets lack snapshots or a copy on another machine.

**Architecture:** The storage check starts storing every dataset with its creation time. A pure function (`daemon/protection.rs`) judges those datasets against the jobs and their run history. The Dashboard turns the verdicts into Needs-attention items; the health monitor feeds them to its existing alert tracker (one tracker key per dataset, one email per host per pass) and the daily report prints them in a new section.

**Tech Stack:** Rust (bin crate `zfsctl`, tests are in-module `#[cfg(test)]`), serde/serde_json, chrono. Run a test with `cargo test <name>`; the whole suite with `cargo test`; lints with `cargo clippy --all-targets` (the baseline has about 176 warnings in test code: add none).

**Spec:** `docs/superpowers/specs/2026-10-23-unprotected-datasets-design.md` (approved, merged).

## Global Constraints

- Reads only: the feature never changes a host, a job or a dataset.
- Plain TUI: items look like every other Needs-attention item (coloured dot, no background bars, banners or logo).
- "Current" means the Dashboard's overdue rule: last finished run succeeded and is at most 2 schedule intervals old; never-run, disabled, failed or unparseable-schedule jobs are not current.
- Grace: a dataset created less than 24 hours before `now` is not judged. Stored lists older than 2 days are not judged.
- Level mapping: your data = Issue (Dashboard) / `Level::Critical` (alerts); OS data = Watch / `Level::Warning`.
- Option order of `zfs list`: options before operands (OmniOS rejects the reverse).
- Every commit message ends with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.
- Work on a new branch `unprotected-check` from `origin/main`. Pull requests are opened by hand by the operator.

## Review Focus

1. A job on host A must never protect a dataset of the same name on host B (`tank/data` on `local` and on `fbsd16`).
2. Prefix traps: a recursive snapshot job on `tank/data` covers `tank/data/x` but not `tank/data2`; a job on a child never covers its parent.
3. Only snapshot jobs cover descendants: the daemon's replicate, backup and mirror code never reads `recursive` (verified), so they cover exactly their own dataset.
4. A replication destination (`dest_host` + `dest_dataset`) is covered by its source; `dest_host = "file"` has no dataset to cover.
5. The boundaries: created 23 h vs 25 h ago, last success at 2 vs 2.1 intervals, a storage list at 47 h vs 49 h old, a creation time in the future.
6. A storage result with thousands of datasets must judge in well under a frame (no per-dataset allocation beyond the output).

## File Structure

- Modify `src/daemon/cron.rs`: the schedule-gap and the "current within 2 intervals" constant (moved out of `ui/overdue.rs`).
- Modify `src/ui/overdue.rs`: use them.
- Modify `src/daemon/storage_check.rs`: collect and store every dataset with its creation time.
- Create `src/daemon/protection.rs`: the judging (pure, no I/O); register in `src/daemon/mod.rs`.
- Modify `src/ui/dashboard.rs`: Needs-attention items.
- Modify `src/daemon/monitor.rs`, `src/daemon/health_text.rs`: alert tracker, messages, report section.
- Modify docs: the book chapters that describe Needs attention and the daily report; retake the Dashboard capture.

---

### Task 1: One definition of "current"

**Files:**
- Modify: `src/daemon/cron.rs`, `src/ui/overdue.rs`
- Test: in both files' test modules

**Interfaces:**
- Produces in `daemon/cron.rs`: `pub const CURRENT_WITHIN_INTERVALS: i32 = 2;` and `pub fn schedule_gap(schedule: &str, now: DateTime<Utc>) -> Option<Duration>` (chrono `Duration`; the body of `ui::overdue::interval`, unchanged, including `LOOKAHEAD`).
- `ui/overdue.rs` keeps its public behaviour: `YELLOW_AFTER` becomes `cron::CURRENT_WITHIN_INTERVALS` and `interval` calls `cron::schedule_gap`.

- [ ] **Step 1: Write the failing test** in `cron.rs`: `schedule_gap_is_the_longest_gap_among_the_next_fires` (hourly gives 1 h; `0 3 * * 1-5` gives at least 24 h over a weekend lookahead; an unparseable schedule gives `None`).
- [ ] **Step 2: Run it** (`cargo test schedule_gap`): expected FAIL, function not defined.
- [ ] **Step 3: Move** `interval` and `LOOKAHEAD` from `ui/overdue.rs` to `cron.rs` as `schedule_gap`; make `YELLOW_AFTER` use the new constant.
- [ ] **Step 4: Run** `cargo test overdue schedule_gap`: PASS (all existing overdue tests unchanged).
- [ ] **Step 5: Commit** `refactor: the schedule gap and the "current within 2 intervals" rule live in cron.rs`.

### Task 2: The storage check keeps every dataset with its creation time

**Files:**
- Modify: `src/daemon/storage_check.rs`
- Test: same file

**Interfaces:**
- Produces: `pub struct SeenDataset { pub name: String, pub volume: bool, pub created: Option<i64> }` (serde, `Clone, PartialEq, Eq`; `created` is epoch seconds, `None` when the column is missing) and `StorageResult.seen: Vec<SeenDataset>` with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`.
- `LIST_ARGS` gains a last column `creation`: `["list","-H","-p","-t","filesystem,volume","-o","name,used,referenced,quota,refquota,type,creation"]`.
- `parse_limited` stays; add `fn parse_seen(output: &str) -> Vec<SeenDataset>` that accepts 6 or 7 tab-separated columns (6: `created = None`), `type == "volume"` sets `volume`.

- [ ] **Step 1: Write failing tests:** `parse_seen_reads_all_datasets_with_creation_times` (a filesystem, a volume, a line with 6 columns, a junk line skipped); `a_check_stores_every_dataset_not_only_limited_ones` (FakeRunner `zfs list` with a quota-less dataset: `result.seen` has it, `result.datasets` does not); `a_failed_pass_keeps_the_last_seen_list` (apply `Outcome::Failed` after a checked result: `seen` still there, as the other fields).
- [ ] **Step 2: Run** `cargo test parse_seen a_check_stores a_failed_pass_keeps`: FAIL.
- [ ] **Step 3: Implement** the type, the parser, the new column, and set `seen` in the same place `datasets` is set (the `zfs list` output is parsed once into both). Existing tests that feed 6-column fixtures must still pass untouched.
- [ ] **Step 4: Run** `cargo test storage_check`: PASS.
- [ ] **Step 5: Verify on real hosts** (read-only; the output shape was checked on 2026-10-23): `zfs list -H -p -t filesystem,volume -o name,used,referenced,quota,refquota,type,creation | head -3` on the hub, on ub24 and on omnios (ssh through the hub as the existing routine does). Expected: last column is epoch seconds on all three.
- [ ] **Step 6: Commit** `feat: the storage check stores every dataset with its creation time`.

### Task 3: What a job gives a dataset

**Files:**
- Create: `src/daemon/protection.rs` (register `pub mod protection;` in `src/daemon/mod.rs`)
- Test: same file

**Interfaces:**
- Consumes: `crate::jobs::{Job, KIND_SNAPSHOT, KIND_REPLICATE, KIND_BACKUP, KIND_MIRROR}`, `daemon::history::{Run, RunResult}`, `cron::{schedule_gap, CURRENT_WITHIN_INTERVALS}`.
- Produces:
  - `pub(crate) enum JobState { Current, Stopped { last_ok: DateTime<Utc> }, Failing, NeverRan }`
  - `pub(crate) fn job_state(job: &Job, runs: &[Run], now: DateTime<Utc>) -> JobState` (Failing when the latest finished run failed; Stopped when the last success is older than `CURRENT_WITHIN_INTERVALS` gaps, or the schedule does not parse (then `last_ok` is the last success or `job.created_at`); NeverRan when there is no success and no failure).
  - `pub(crate) fn gives_snapshots(job: &Job) -> bool` (enabled snapshot job).
  - `pub(crate) fn gives_copy(job: &Job) -> bool` (enabled and: replicate with `dest_host` a host other than `job.host`, or `file` when `job.host != "local"`; backup whose `repository` is remote: starts with one of `sftp:`, `s3:`, `rclone:`, `b2:`, `rest:`, `azure:`, `gs:`, `swift:`; mirror whose `remote` contains `:`, does not start with `/` and does not start with `:local:`).
  - `pub(crate) fn covers(job: &Job, host: &str, dataset: &str) -> bool` (same host; exact dataset; or, for a snapshot job with `recursive`, `dataset` starts with `job.dataset + "/"`).

- [ ] **Step 1: Write failing tests**, with the spec's values in the assertions: `job_state_follows_the_overdue_rule` (hourly job: success 119 min ago is Current, 121 min ago is Stopped, latest run failed is Failing, no runs is NeverRan, bad schedule is Stopped); `only_remote_destinations_are_a_copy` (local path repo no; `sftp:h:/r` yes; `rclone:b2:x` yes; replicate to the same host no; to another host yes; to `file` yes only when the job's host is not `local`; mirror `b2:bucket/x` yes, `/mnt/x` no, `:local:/x` no; disabled never); `covers_respects_host_prefix_and_recursion` (`tank/data` vs `tank/data2`, child never covers parent, a recursive replicate job does NOT cover children, a recursive snapshot job does, another host never).
- [ ] **Step 2: Run** `cargo test protection`: FAIL.
- [ ] **Step 3: Implement** the five functions. Latest finished run = max `finished_at` among runs with `result` set for that job id.
- [ ] **Step 4: Run** `cargo test protection`: PASS.
- [ ] **Step 5: Commit** `feat: what a job gives a dataset (current, snapshots, copy on another machine, coverage)`.

### Task 4: Judging the datasets

**Files:**
- Modify: `src/daemon/protection.rs`
- Test: same file

**Interfaces:**
- Consumes: Task 2's `StorageFile` / `SeenDataset`, Task 3's functions.
- Produces:
  - `pub const GRACE: chrono::Duration` (24 h), `pub const STALE_AFTER: chrono::Duration` (2 days).
  - `pub enum Tier { Data, Os }` (`Ord`: `Data` before `Os`).
  - `pub enum Gap { Missing, Stopped { last_ok: DateTime<Utc> }, Failing }`.
  - `pub struct Unprotected { pub host: String, pub dataset: String, pub tier: Tier, pub snapshots: Option<Gap>, pub copy: Option<Gap> }` (`Clone, Debug, PartialEq, Eq`; at least one of the two is `Some`).
  - `pub fn tier_of(dataset: &str, volume: bool) -> Tier`: `Os` when any `/`-separated step after the pool is `ROOT`, or the path below the pool is `tmp`, `var` or under `var/`, or `usr/ports`, `usr/src`, `usr/obj` (and below), or a volume whose last step is `swap` or `dump`; everything else `Data` (`usr/home` is `Data`).
  - `pub fn judge(file: &StorageFile, jobs: &[Job], runs: &[Run], now: DateTime<Utc>) -> Vec<Unprotected>`: every `seen` dataset of every host entry that is not stale (`now - checked_at <= STALE_AFTER`) and older than `GRACE` (a missing or future `created` counts as old) gets its snapshot side and copy side from the jobs that `covers` it; a dataset that is the `dest_dataset` of an enabled replicate job with `dest_host == host` whose state is Current is protected on both sides. Sorted `Tier`, then host, then dataset.

- [ ] **Step 1: Write failing tests:** `tier_of_recognises_the_os_datasets` (`zroot/ROOT/default`, `zroot/tmp`, `zroot/var/log`, `zroot/usr/ports`, `rpool/swap` volume are Os; `zroot/usr/home`, `tank/photos`, `tank/var2` are Data); `a_dataset_with_both_current_jobs_is_protected`; `each_missing_half_is_named` (snapshots only: copy is Missing; a stopped copy job gives `Stopped` with its last success; a failing snapshot job gives Failing; neither: both Some); `a_replica_is_protected_by_its_current_source_job` (and not when that job is Stopped); `grace_and_staleness_boundaries` (23 h old is not judged, 25 h is; a list checked 47 h ago is judged, 49 h ago says nothing; a future creation time is judged); `a_stale_or_empty_host_says_nothing` (a host entry whose `checked_at` is 49 h old, and one with no `seen` list); `output_is_sorted_data_first`.
- [ ] **Step 2: Run** `cargo test protection`: FAIL.
- [ ] **Step 3: Implement.** A host entry whose `checked_at` is older than `STALE_AFTER` is skipped; an entry with an `error` but a recent `checked_at` is judged from its last good list. Build, once per call, the list of enabled jobs of the four kinds and filter per dataset; do not clone the jobs.
- [ ] **Step 4: Run** `cargo test protection`: PASS, plus a test `judging_five_thousand_datasets_is_fast` (5000 seen datasets, 40 jobs; finishes in under 200 ms in a debug build).
- [ ] **Step 5: Commit** `feat: judge every dataset: snapshots plus a copy on another machine`.

### Task 5: The Dashboard lines

**Files:**
- Modify: `src/ui/dashboard.rs`
- Test: same file (fixtures `Fixture`, `storage_entry`, and `fx.storage.hosts.insert(..)` are the existing way)

**Interfaces:**
- Consumes: `protection::{judge, Unprotected, Gap, Tier}`.
- Produces: one `Item` per `Unprotected`: `level` Issue for `Tier::Data`, Watch for `Tier::Os`; `subject` `dataset` for host `local`, else `host:dataset` (like `cloud_label`); `what` joins the missing halves with `; `: `no snapshots` / `snapshots stopped {age}` / `snapshot job failing`, `no copy on another machine` / `copy on another machine stopped {age}` / `copy job failing` (`{age}` from `crate::ui::jobs::age_text`); `goto: Section::Jobs`, `target: None`. Added where the other storage problems are added (the block that uses `reached`), only for hosts `reached` accepts, and only while the daemon line is Fine like the overdue items. The inputs are `&inputs.storage`, `&inputs.jobs.jobs`, `&inputs.jobs.runs` and `inputs.now`.

- [ ] **Step 1: Write failing tests:** `an_unprotected_dataset_is_a_red_line_and_an_os_one_amber` (assert level, subject, text exactly as above for a `local` data dataset with no jobs and an OS dataset on `h`); `a_host_that_is_not_reached_adds_no_protection_lines`; `the_verdict_counts_the_protection_lines` (one Issue gives `1 issue`); `protected_datasets_add_nothing`.
- [ ] **Step 2: Run** `cargo test an_unprotected the_verdict_counts protected_datasets`: FAIL.
- [ ] **Step 3: Implement** the items; the sort that already follows orders them with the rest.
- [ ] **Step 4: Run** `cargo test dashboard`: PASS.
- [ ] **Step 5: Commit** `feat: the Dashboard lists unprotected datasets in Needs attention`.

### Task 6: Email when a dataset becomes unprotected, and when it recovers

**Files:**
- Modify: `src/daemon/monitor.rs`, `src/daemon/health_text.rs`
- Test: `monitor.rs` tests (use the existing `FakeSink`)

**Interfaces:**
- Consumes: `protection::judge`, `StorageFile::load(&state_dir.join(storage_check::FILE))`, `health::Tracker`.
- Produces: in `Monitor`: a second `Tracker` field `protection` built like `tracker` (same confirm count and reminder interval, updated by `set_settings`); in `observe`, after the host rows: judge the datasets of the configured, reachable hosts, feed `protection.observe(key, level, reason, now)` once per dataset with `key = format!("{host}\t{dataset}")`, `Level::Critical` for `Tier::Data`, `Level::Warning` for `Tier::Os`, `reason` the same words as the Dashboard; `Monitor` keeps `protection_keys: HashSet<String>` (the keys fed last pass). A key that is no longer unprotected but whose dataset is still in a `seen` list is fed `Level::Ok`, so it recovers ("protected again"); a key whose dataset has disappeared from every `seen` list (destroyed, host removed) is forgotten silently with `forget_except`, with no recovery message. The alerts of one pass are grouped by host and kind: `pub fn protection_message(host: &str, kind: AlertKind, items: &[(String, Alert)], now: DateTime<Utc>, hub: &str) -> Message` in `health_text.rs` (subject `[zfsctl] {host}: {n} dataset(s) not protected` / `...still not protected (for 3d)` / `...protected again`; kinds `health-alert`, `health-reminder`, `health-recovered`; body one line per dataset with its reason; ASCII through `ascii_line`). A message that cannot be delivered is retried like the others, and after the maximum attempts `protection.forget_host`-style forgetting applies to those keys so the next pass alerts again.

- [ ] **Step 1: Write failing tests:** `six_new_unprotected_datasets_on_one_host_send_one_message` (one `health-alert`, six lines); `a_dataset_in_its_grace_sends_nothing`; `recovery_is_one_message_per_host`; `a_second_pass_sends_no_duplicate`; `a_failed_delivery_is_retried_then_alerts_again`; `an_unreachable_host_sends_no_protection_mail`; `a_destroyed_dataset_recovers_silently`.
- [ ] **Step 2: Run** `cargo test protection_message six_new a_dataset_in_its_grace recovery_is_one`: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test monitor health_text`: PASS.
- [ ] **Step 5: Commit** `feat: email when datasets become unprotected and when they recover`.

### Task 7: The daily report section

**Files:**
- Modify: `src/daemon/health_text.rs` (and its call in `monitor.rs`)
- Test: `health_text.rs` tests

**Interfaces:**
- `report_message` gains a last parameter `unprotected: &[String]`; produced by `pub fn protection_lines(items: &[Unprotected], bad_since: &dyn Fn(&str) -> Option<DateTime<Utc>>, now: DateTime<Utc>) -> Vec<String>`: `  {host}:{dataset} (your data|OS data): {what} (for 3d)` with the duration from the protection tracker's `bad_since(key)` when known. The report prints, after NEEDS ATTENTION, a section `UNPROTECTED DATA` with these lines (cut by `cut_list`), or `  All datasets are protected.` when none.

- [ ] **Step 1: Write failing tests:** `the_report_lists_unprotected_data_first_with_how_long` and `the_report_says_all_protected_when_none` (assert the exact section text; and that the subject line is unchanged).
- [ ] **Step 2: Run** `cargo test the_report_`: FAIL.
- [ ] **Step 3: Implement**; update the other `report_message` callers and tests with the new argument.
- [ ] **Step 4: Run** `cargo test`: PASS (the whole suite).
- [ ] **Step 5: Commit** `feat: the daily report has an Unprotected data section`.

### Task 8: Book, capture, live check

**Files:**
- Modify: the book chapters that describe Needs attention and the daily report (`grep -n "NEEDS ATTENTION\|daily report" docs/book/*.md`; at least `docs/book/16-alerts.md`), `docs/book/captures/06-dashboard.txt` (regenerated with the capture tool, `examples/capture.rs`).

- [ ] **Step 1: Write the book text:** what protected means (snapshots plus a copy on another machine), the two colours, the one-day grace, what counts as another machine, that no dataset can be excluded, and the email behaviour. Run the book's drift tests (`cargo test book`) and fix what they report.
- [ ] **Step 2: Full checks:** `cargo test`, `cargo clippy --all-targets` (no new warnings), `cargo build --release`.
- [ ] **Step 3: Live check on the hub** (read-only: do not create, change or delete anything on the production hub): install by the usual routine, run `sudo zfsctl check-host`, open the TUI Dashboard. Compare each listed dataset with `Jobs`: a dataset the operator knows is covered must not be listed; one with no replication or backup must be.
- [ ] **Step 4: Live check on a test box** only after asking the operator which box and getting a go-ahead. The operator creates a throwaway dataset there. It must not be listed for 24 hours (the grace is proved by the unit tests; never change a host's clock to hurry it). After a day it must appear as unprotected, and adding a snapshot job and a replication job for it must clear the line.
- [ ] **Step 5: Commit** `docs: the unprotected-datasets check in the book; a fresh Dashboard capture`.
