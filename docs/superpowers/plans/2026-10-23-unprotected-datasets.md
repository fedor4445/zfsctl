# Unprotected Datasets Check Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tell the operator, on the Dashboard, by email and in the daily report, which datasets fall short of the protection level the operator chose.

**Architecture:** The storage check starts storing every dataset with its creation time. A `[protection]` config table and a Settings group hold the operator's choices (level, how recent, email timing). A pure function (`daemon/protection.rs`) judges the datasets against the jobs, their run history and those choices. The Dashboard turns the verdicts into Needs-attention items; the health monitor feeds them to its alert tracker (one key per dataset, one email per host per pass); the daily report prints them in a new section.

**Tech Stack:** Rust (bin crate `zfsctl`, tests are in-module `#[cfg(test)]`), serde/serde_json, toml, chrono. Run a test with `cargo test <name>`; the whole suite with `cargo test`; lints with `cargo clippy --all-targets` (the baseline has about 176 warnings in test code: add none).

**Spec:** `docs/superpowers/specs/2026-10-23-unprotected-datasets-design.md` (read its "Levels" section first).

## Global Constraints

- Reads only: the feature never changes a host, a job or a dataset.
- Plain TUI: items look like every other Needs-attention item (coloured dot, no background bars, banners or logo).
- Defaults: level `basic`, recent `normal`, email `immediate`. Levels: Basic = at least 1 copy on another machine, 0 online; Careful = at least 3 copies, at least 1 online; Custom = `copies` 1 to 3 (default 1), `online` 0 to 2 (default 0, never more than `copies`). Recent: `strict` = 1, `normal` = 2, `relaxed` = 4 schedule intervals.
- "Current": enabled, last finished run succeeded, last success at most `tolerance` schedule intervals old; never-run, disabled, failed or unparseable-schedule jobs are not current.
- Grace: a dataset created less than 24 hours before `now` is not judged. A stored list older than 2 days is not judged.
- Levels on the screen: your data = Issue (Dashboard) / `Level::Critical` (alerts); OS data = Watch / `Level::Warning`. OS data always needs only Basic's copies.
- Option order of `zfs list`: options before operands (OmniOS rejects the reverse).
- Every commit message ends with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.
- Work on a new branch `unprotected-check` from `origin/main`. Pull requests are opened by hand by the operator.

## Review Focus

1. A job on host A must never protect a dataset of the same name on host B (`tank/data` on `local` and on `fbsd16`).
2. Prefix traps: a recursive snapshot job on `tank/data` covers `tank/data/x` but not `tank/data2`; a job on a child never covers its parent.
3. Only snapshot jobs cover descendants: the daemon's replicate, backup and mirror code never reads `recursive` (verified), so they cover exactly their own dataset.
4. Two jobs that write to the same place are one copy; a replication destination is covered by its source; `dest_host = "file"` has no dataset to cover.
5. The boundaries: created 23 h vs 25 h ago, last success at 2 vs 2.1 intervals (and at each recency step), a list 47 h vs 49 h old, a creation time in the future.
6. Switching the email timing must neither flood (one message per host) nor silently lose the current list; a wrong `[protection]` value must give a clear error, not a panic or a silent default.
7. A storage result with thousands of datasets must judge in well under a frame.

## File Structure

- Modify `src/daemon/cron.rs`, `src/ui/overdue.rs`: the shared schedule gap.
- Modify `src/daemon/storage_check.rs`: store every dataset with its creation time.
- Modify `src/config.rs`: `[protection]` and `ProtectionSettings`.
- Create `src/daemon/protection.rs` (register in `src/daemon/mod.rs`): what a job gives, the judging, the words.
- Modify `src/ui/settings.rs` (and the key handling that saves rows, found through `Setting`): the Data protection group.
- Modify `src/ui/dashboard.rs`; `src/daemon/monitor.rs`, `src/daemon/health_text.rs`.
- Modify the book chapters; retake the Dashboard capture.

---

### Task 1: One definition of "current"

**Files:** Modify `src/daemon/cron.rs`, `src/ui/overdue.rs`; tests in both.

**Interfaces:**
- Produces in `daemon/cron.rs`: `pub const CURRENT_WITHIN_INTERVALS: i32 = 2;` and `pub fn schedule_gap(schedule: &str, now: DateTime<Utc>) -> Option<Duration>` (chrono `Duration`; the body of `ui::overdue::interval`, unchanged, including `LOOKAHEAD`). `ui/overdue.rs` keeps its behaviour: `YELLOW_AFTER` is the new constant, `interval` calls `schedule_gap`.

- [ ] **Step 1: Failing test** in `cron.rs`: `schedule_gap_is_the_longest_gap_among_the_next_fires` (hourly gives 1 h; `0 3 * * 1-5` gives at least 24 h; an unparseable schedule gives `None`).
- [ ] **Step 2: Run** `cargo test schedule_gap`: FAIL, not defined.
- [ ] **Step 3: Move** `interval` and `LOOKAHEAD` to `cron.rs` as `schedule_gap`; point `overdue.rs` at them.
- [ ] **Step 4: Run** `cargo test overdue schedule_gap`: PASS (existing overdue tests untouched).
- [ ] **Step 5: Commit** `refactor: the schedule gap lives in cron.rs`.

### Task 2: The storage check keeps every dataset with its creation time

**Files:** Modify `src/daemon/storage_check.rs`; tests in it.

**Interfaces:**
- Produces: `pub struct SeenDataset { pub name: String, pub volume: bool, pub created: Option<i64> }` (serde; `created` is epoch seconds, `None` when the column is missing) and `StorageResult.seen: Vec<SeenDataset>` with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`.
- `LIST_ARGS` gains a last column: `["list","-H","-p","-t","filesystem,volume","-o","name,used,referenced,quota,refquota,type,creation"]`. `fn parse_seen(output: &str) -> Vec<SeenDataset>` accepts 6 or 7 tab-separated columns (6: `created = None`; type `volume` sets `volume`).

- [ ] **Step 1: Failing tests:** `parse_seen_reads_all_datasets_with_creation_times` (a filesystem, a volume, a 6-column line, a junk line skipped); `a_check_stores_every_dataset_not_only_limited_ones`; `a_failed_pass_keeps_the_last_seen_list`.
- [ ] **Step 2: Run** `cargo test parse_seen a_check_stores a_failed_pass_keeps`: FAIL.
- [ ] **Step 3: Implement** where `datasets` is set (one `zfs list`, parsed into both). Existing 6-column fixtures must keep passing untouched.
- [ ] **Step 4: Run** `cargo test storage_check`: PASS.
- [ ] **Step 5: Verify on real hosts** (read-only; checked on 2026-10-23): the new `zfs list` line, `| head -3`, on the hub, ub24 and omnios. Expected: last column is epoch seconds on all three.
- [ ] **Step 6: Commit** `feat: the storage check stores every dataset with its creation time`.

### Task 3: The `[protection]` config

**Files:** Modify `src/config.rs`; tests in it.

**Interfaces:**
- Produces: `pub struct ProtectionConfig { pub level: Option<String>, pub copies: Option<u8>, pub online: Option<u8>, pub recent: Option<String>, pub email: Option<String> }` (serde, `Default`, skipped when default, as `DashboardConfig` and `NotifyConfig` are) as `Config.protection`; `pub enum EmailTiming { Immediate, Report, Off }`; `pub struct ProtectionSettings { pub copies: u8, pub online: u8, pub tolerance: i32, pub email: EmailTiming }` (`Clone, Debug, PartialEq, Eq`, `Default` = Basic, 2, Immediate); `impl ProtectionConfig { pub fn resolve(&self) -> Result<ProtectionSettings, String> }`.
- `resolve` errors name the key and the allowed values, e.g. `[protection] level must be basic, careful or custom`; `copies` outside 1..=3, `online` outside 0..=2 or above `copies` are errors; `copies`/`online` are ignored unless the level is `custom`.

- [ ] **Step 1: Failing tests:** `protection_defaults_are_basic_normal_immediate`; `careful_means_three_copies_one_online`; `custom_reads_copies_and_online_and_ignores_them_for_other_levels`; `recent_steps_are_one_two_and_four_intervals`; `a_wrong_value_names_the_key` (level `paranoid`, copies 4, online 3, online above copies, recent `fast`, email `daily`); `an_empty_section_is_not_written_back` (round trip through the config writer, if it serialises).
- [ ] **Step 2: Run** `cargo test protection_ careful_means custom_reads recent_steps a_wrong_value an_empty_section`: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test config`: PASS.
- [ ] **Step 5: Commit** `feat: the [protection] config table`.

### Task 4: What a job gives a dataset

**Files:** Create `src/daemon/protection.rs` (add `pub mod protection;` to `src/daemon/mod.rs`); tests in it.

**Interfaces:**
- Consumes: `crate::jobs::{Job, KIND_SNAPSHOT, KIND_REPLICATE, KIND_BACKUP, KIND_MIRROR}`, `daemon::history::{Run, RunResult}`, `cron::schedule_gap`.
- Produces:
  - `pub(crate) enum JobState { Current, Stopped { last_ok: DateTime<Utc> }, Failing, NeverRan }` and `pub(crate) fn job_state(job: &Job, runs: &[Run], tolerance: i32, now: DateTime<Utc>) -> JobState` (Failing when the latest finished run failed; Stopped when the last success is older than `tolerance` gaps or the schedule does not parse (`last_ok` then the last success or `job.created_at`); NeverRan when there is neither success nor failure).
  - `pub(crate) fn gives_snapshots(job: &Job) -> bool` (enabled snapshot job).
  - `pub(crate) enum CopyKind { OnPremises, Online }` and `pub(crate) fn copy_of(job: &Job) -> Option<(CopyKind, String)>`: `None` unless enabled and a copy on another machine; the string identifies where it writes. Replicate to a host other than `job.host`: `OnPremises`, `host/dest_dataset`; to `file` when `job.host != "local"`: `OnPremises`, `file/dest_dir`. Backup: `repository` starting `s3:`, `b2:`, `azure:`, `gs:`, `swift:` or `rclone:` is `Online`; `sftp:` or `rest:` is `OnPremises`; anything else `None`. Mirror: `remote` containing `:`, not starting with `/` or `:local:` is `Online`, else `None`. The identifying string for backup and mirror is the repository or remote itself.
  - `pub(crate) fn covers(job: &Job, host: &str, dataset: &str) -> bool` (same host; exact dataset; or, for a snapshot job with `recursive`, `dataset` starts with `job.dataset + "/"`).

- [ ] **Step 1: Failing tests** (spec values in the assertions): `job_state_follows_the_overdue_rule_at_each_tolerance` (hourly job, tolerance 2: 119 min Current, 121 min Stopped; tolerance 1: 61 min Stopped; tolerance 4: 239 min Current; latest run failed is Failing; no runs NeverRan; bad schedule Stopped); `copy_of_classifies_destinations` (local path repo `None`; `sftp:h:/r` on-premises; `rclone:b2:x` online; replicate to the same host `None`, to another host on-premises, to `file` only when the job's host is not `local`; mirror `b2:bucket/x` online, `/mnt/x` and `:local:/x` `None`; disabled `None`); `covers_respects_host_prefix_and_recursion` (`tank/data` vs `tank/data2`, a child never covers its parent, a recursive replicate job does NOT cover children, a recursive snapshot job does, another host never).
- [ ] **Step 2: Run** `cargo test protection`: FAIL.
- [ ] **Step 3: Implement** the four functions. Latest finished run = max `finished_at` among that job's runs with `result` set.
- [ ] **Step 4: Run** `cargo test protection`: PASS.
- [ ] **Step 5: Commit** `feat: what a job gives a dataset`.

### Task 5: Judging the datasets, and the words

**Files:** Modify `src/daemon/protection.rs`; tests in it.

**Interfaces:**
- Consumes: Tasks 2, 3, 4.
- Produces:
  - `pub const GRACE: chrono::Duration` (24 h), `pub const STALE_AFTER: chrono::Duration` (2 days).
  - `pub enum Tier { Data, Os }` (`Ord`: `Data` first). `pub fn tier_of(dataset: &str, volume: bool) -> Tier`: `Os` when a step after the pool is `ROOT`, or the path below the pool is `tmp`, `var` or under `var/`, `usr/ports`, `usr/src`, `usr/obj` (and below), or a volume whose last step is `swap` or `dump`; else `Data` (`usr/home` is `Data`).
  - `pub enum Gap { Missing, Stopped { last_ok: DateTime<Utc> }, Failing }`.
  - `pub struct CopyGap { pub have: u8, pub need: u8, pub online_have: u8, pub online_need: u8, pub worst: Gap }` (`worst`: with no current copy, `Missing` when no copy job covers the dataset, else the state of the most recently stopped or failing one; with some current copies, `Missing`).
  - `pub struct Unprotected { pub host: String, pub dataset: String, pub tier: Tier, pub snapshots: Option<Gap>, pub copies: Option<CopyGap> }` (`Clone, Debug, PartialEq, Eq`; at least one `Some`).
  - `pub fn judge(file: &StorageFile, jobs: &[Job], runs: &[Run], settings: &ProtectionSettings, now: DateTime<Utc>) -> Vec<Unprotected>`: every `seen` dataset of every host entry with `now - checked_at <= STALE_AFTER` and older than `GRACE` (missing or future `created` counts as old). Snapshots: Current covering snapshot job, else a `Gap`. Copies: distinct current copies (by `copy_of`'s identity string, over covering jobs) counted as `have`, of which `Online` as `online_have`; `need`/`online_need` from `settings` for `Tier::Data`, `1`/`0` for `Tier::Os`. A dataset that is the `dest_dataset` of a Current replicate job with `dest_host == host` is protected on both sides. Sorted `Tier`, then host, then dataset.
  - `impl Unprotected { pub fn describe(&self, age: &dyn Fn(DateTime<Utc>) -> String) -> String }`: the words, joined by `; `: `no snapshots` / `snapshots stopped {age}` / `snapshot job failing`; for copies, with `have == 0` and need 1 or more: `no copy on another machine` / `copy on another machine stopped {age}` / `copy job failing`; with `0 < have < need`: `{have} of {need} copies on other machines`; then, if `online_have < online_need`: `no online copy` when `online_need == 1`, else `{online_have} of {online_need} online copies`.

- [ ] **Step 1: Failing tests:** `tier_of_recognises_the_os_datasets` (`zroot/ROOT/default`, `zroot/tmp`, `zroot/var/log`, `zroot/usr/ports`, `rpool/swap` volume are Os; `zroot/usr/home`, `tank/photos`, `tank/var2` are Data); `basic_is_one_current_snapshot_job_and_one_current_copy`; `careful_needs_three_copies_one_of_them_online` (2 on-premises copies: `2 of 3 copies on other machines; no online copy`; adding a mirror clears it); `custom_counts_what_it_is_told`; `two_jobs_to_one_destination_are_one_copy`; `each_missing_half_is_named` (the exact `describe` strings above, with a fixed `age`); `a_replica_is_protected_by_its_current_source_job` (and not when that job is Stopped); `grace_staleness_and_recency_boundaries`; `a_stale_or_empty_host_says_nothing`; `os_data_needs_only_one_copy_at_every_level`; `output_is_sorted_data_first`; `judging_five_thousand_datasets_is_fast` (5000 datasets, 40 jobs, under 200 ms in a debug build).
- [ ] **Step 2: Run** `cargo test protection`: FAIL.
- [ ] **Step 3: Implement.** Filter the enabled jobs of the four kinds once per call; do not clone jobs.
- [ ] **Step 4: Run** `cargo test protection`: PASS.
- [ ] **Step 5: Commit** `feat: judge every dataset against the chosen level`.

### Task 6: The Settings group

**Files:** Modify `src/ui/settings.rs` and wherever a `Setting` is turned into a form and saved (found with `grep -rn "Setting::Quota" src`); tests next to them.

**Interfaces:**
- Consumes: `Config.protection`, `ProtectionConfig::resolve`.
- Produces: rows in a new group `Data protection`: `Setting::ProtLevel` ("Level", `basic`/`careful`/`custom`), `Setting::ProtCopies` ("Copies on other machines (1 to 3)") and `Setting::ProtOnline` ("Online copies (0 to 2)"), shown only when the level is `custom`, `Setting::ProtRecent` ("How recent (strict, normal, relaxed)"), `Setting::ProtEmail` ("Email (immediate, report, off)"). Values equal to the default are removed from the file, as the other rows do. Validation uses `ProtectionConfig::resolve` so the screen and the config agree.

- [ ] **Step 1: Failing tests:** `the_protection_rows_show_the_effective_values` (defaults: `basic`, `normal`, `immediate`, no Custom rows); `custom_adds_its_two_rows`; `saving_a_level_writes_protection_level_and_a_default_removes_it`; `a_wrong_value_is_refused_with_the_configs_words`; follow the existing quota-row tests for the shape.
- [ ] **Step 2: Run** `cargo test protection_rows custom_adds saving_a_level a_wrong_value_is_refused`: FAIL.
- [ ] **Step 3: Implement** following `pair_changes` and the `Setting::Memory` row as the pattern.
- [ ] **Step 4: Run** `cargo test settings`: PASS.
- [ ] **Step 5: Commit** `feat: Settings has a Data protection group`.

### Task 7: The Dashboard lines

**Files:** Modify `src/ui/dashboard.rs` (and the one place that builds `Inputs` for the app: `grep -rn "thresholds:" src`); tests in `dashboard.rs`.

**Interfaces:**
- Consumes: `protection::{judge, Tier}`. `Inputs` gains `pub protection: &'a ProtectionSettings`; the test `Fixture` supplies a default.
- Produces: one `Item` per `Unprotected`: `level` Issue for `Tier::Data`, Watch for `Tier::Os`; `subject` `dataset` for host `local`, else `host:dataset`; `what` is `describe(&|t| age_text(now, t))`; `goto: Section::Jobs`, `target: None`. Added in the block that uses `reached`, only for hosts `reached` accepts and only while the daemon line is Fine; always shown whatever the email timing is (that setting controls email only). Inputs to `judge`: `inputs.storage`, `&inputs.jobs.jobs`, `&inputs.jobs.runs`, `inputs.protection`, `inputs.now`.

- [ ] **Step 1: Failing tests:** `an_unprotected_dataset_is_a_red_line_and_an_os_one_amber` (level, subject, exact text); `a_host_that_is_not_reached_adds_no_protection_lines`; `the_verdict_counts_the_protection_lines`; `protected_datasets_add_nothing`; `the_chosen_level_changes_the_verdict` (the same data is protected at Basic and has a line at Careful); `email_off_still_shows_the_dashboard_lines`.
- [ ] **Step 2: Run** `cargo test an_unprotected the_verdict_counts protected_datasets the_chosen_level email_off_still`: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test dashboard`: PASS.
- [ ] **Step 5: Commit** `feat: the Dashboard lists unprotected datasets in Needs attention`.

### Task 8: Email when a dataset becomes unprotected, and when it recovers

**Files:** Modify `src/daemon/monitor.rs`, `src/daemon/health_text.rs`; tests in `monitor.rs` (use the existing `FakeSink`).

**Interfaces:**
- Consumes: `protection::judge`, `StorageFile::load(&state_dir.join(storage_check::FILE))`, `health::Tracker`, `ProtectionSettings`.
- Produces: `Monitor::set_protection(&mut self, ProtectionSettings)`, called wherever `set_settings` is called and at construction (from `Config.protection.resolve()`; a wrong section falls back to the defaults and is reported the way a wrong `[notify]` section already is). A second `Tracker` field `protection` built like `tracker`. In `observe`, only when `email == Immediate`: judge the configured, reachable hosts' datasets and feed `protection.observe(key, level, reason, now)` per dataset, `key = format!("{host}\t{dataset}")`, `Level::Critical` for `Tier::Data`, `Level::Warning` for `Tier::Os`, `reason = describe(..)`. `Monitor` keeps `protection_keys: HashSet<String>` (keys fed last pass): a key no longer unprotected whose dataset is still in a `seen` list is fed `Level::Ok` ("protected again"); a key whose dataset has vanished from every `seen` list is forgotten silently with `forget_except`. When `email` is not `Immediate` the tracker is cleared. Alerts of one pass are grouped by host and kind: `pub fn protection_message(host: &str, kind: AlertKind, items: &[(String, Alert)], now: DateTime<Utc>, hub: &str) -> Message` in `health_text.rs` (subjects `[zfsctl] {host}: {n} dataset(s) not protected` / `... still not protected (for 3d)` / `... protected again`; message kinds `health-alert`, `health-reminder`, `health-recovered`; one body line per dataset; ASCII through `ascii_line`). An undelivered message is retried like the others; after the maximum attempts its keys are forgotten so the next pass alerts again.

- [ ] **Step 1: Failing tests:** `six_new_unprotected_datasets_on_one_host_send_one_message`; `a_dataset_in_its_grace_sends_nothing`; `recovery_is_one_message_per_host`; `a_second_pass_sends_no_duplicate`; `a_failed_delivery_is_retried_then_alerts_again`; `an_unreachable_host_sends_no_protection_mail`; `a_destroyed_dataset_recovers_silently`; `report_and_off_send_no_alerts`; `switching_to_immediate_sends_one_message_per_host_for_the_current_list`.
- [ ] **Step 2: Run** `cargo test protection_message six_new a_dataset_in_its_grace recovery_is_one report_and_off switching_to_immediate`: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test monitor health_text`: PASS.
- [ ] **Step 5: Commit** `feat: email when datasets become unprotected and when they recover`.

### Task 9: The daily report section

**Files:** Modify `src/daemon/health_text.rs` (and its caller in `monitor.rs`); tests in `health_text.rs`.

**Interfaces:**
- `report_message` gains a last parameter `unprotected: Option<&[String]>` (`None` when the email timing is `off`: no section); lines from `pub fn protection_lines(items: &[Unprotected], bad_since: &dyn Fn(&str) -> Option<DateTime<Utc>>, now: DateTime<Utc>) -> Vec<String>`: `  {host}:{dataset} (your data|OS data): {describe} (for 3d)`, the duration from the protection tracker's `bad_since(key)` when it is known. After NEEDS ATTENTION the report prints `UNPROTECTED DATA` with these lines (cut by `cut_list`), or `  All datasets are protected.`

- [ ] **Step 1: Failing tests:** `the_report_lists_unprotected_data_first_with_how_long`; `the_report_says_all_protected_when_none`; `no_section_when_email_is_off`; the subject line is unchanged.
- [ ] **Step 2: Run** `cargo test the_report_ no_section_when`: FAIL.
- [ ] **Step 3: Implement**; update the other `report_message` callers and tests.
- [ ] **Step 4: Run** `cargo test`: PASS (whole suite).
- [ ] **Step 5: Commit** `feat: the daily report has an Unprotected data section`.

### Task 10: Book, capture, live check

**Files:** the book chapters that describe Needs attention, Settings and the daily report (`grep -n "NEEDS ATTENTION\|daily report\|Dashboard thresholds" docs/book/*.md`, at least `16-alerts.md` and the Settings chapter), `docs/book/21-reference-config.md` (the new table), `docs/book/captures/06-dashboard.txt` (regenerate with `examples/capture.rs`).

- [ ] **Step 1: Write the book text:** what protected means, the three levels and what counts as a copy (and an online copy), "how recent", the email timing, the one-day grace, that no dataset can be excluded, where to set it. Run `cargo test book` and fix what the drift tests report.
- [ ] **Step 2: Full checks:** `cargo test`, `cargo clippy --all-targets` (no new warnings), `cargo build --release`.
- [ ] **Step 3: Live check on the hub** (read-only: create, change and delete nothing on the production hub): install by the usual routine, run `sudo zfsctl check-host`, open the Dashboard and Settings. Compare each listed dataset with Jobs: a dataset the operator knows is covered must not be listed, one with no copy job must be; change the level in Settings to Careful and back and watch the lines change.
- [ ] **Step 4: Live check on a test box** only after asking the operator which box and getting a go-ahead. The operator creates a throwaway dataset there. It must not be listed for 24 hours (the grace is proved by the unit tests; never change a host's clock to hurry it). After a day it must appear as unprotected; adding a snapshot job and a replication job for it must clear the line.
- [ ] **Step 5: Commit** `docs: the unprotected-datasets check and its levels in the book; a fresh Dashboard capture`.
