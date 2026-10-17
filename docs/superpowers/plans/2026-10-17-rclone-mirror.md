# Cloud Mirror (rclone) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new `rclone` job kind that mirrors a ZFS snapshot clone to an rclone remote as plain files (`current/`), moving anything replaced or deleted into `old/<UTC run time>/`, with a speed limit, an F5 remote check, and a book guide for AWS S3 and Backblaze B2.

**Architecture:** Reuse the restic backup job's machinery: the same source choices (`new` / `snapshot` / `prefix` + `path`), clone-and-mount lifecycle, cancel flag, progress files, history, alerts, F5 check plumbing and opt-in sudoers feature. Pure rclone logic (validation, argument building, output parsing, overlap) lives in a new `src/rclone.rs`; the run lives in a new `src/daemon/mirror.rs`; shared clone/source helpers are extracted from `src/daemon/backup.rs` without changing its behaviour.

**Tech Stack:** Rust, `CommandRunner` fakes, ratatui TUI, serde/toml `jobs.toml`, rclone (user-installed).

**Spec:** `docs/superpowers/specs/2026-10-17-rclone-mirror-design.md`

## Global Constraints

- Kind string `"rclone"` (`KIND_MIRROR`); the TUI calls it a cloud mirror job. New `jobs.toml` keys `remote` and `bwlimit`; the kind reuses `dataset`, `source`, `snapshot`, `prefix`, `keep`, `path`, `schedule`, `catch_up`. Old files load unchanged.
- Layout in the bucket: `<remote>/current` is the mirror, `<remote>/old/<YYYYMMDDTHHMMSSZ>/` holds what a run replaced or deleted (`rclone sync --backup-dir`); `old/` and `current/` are siblings. zfsctl never expires or deletes anything in `old/`.
- zfsctl only ever runs `rclone sync` (with `--backup-dir`) and read-only `lsf` / `size` / `config file`-style commands. Never `delete`, `purge`, `deletefile`, `cleanup`, `move`, `--delete-excluded`, `--links`, filters, or a provider-specific flag (no `--b2-hard-delete`).
- Refuse the run, touching nothing, when the clone/mount failed or the source tree has no files (`rclone size` count 0). Regular files that cannot be read fail the run. Skipped symlinks/sockets/devices/empty directories are **not** mentioned in the run message or the F5 check (user decision); the book states the limitation.
- A remote is `name:path`: name from letters, digits, `_`, `-`; must not start with `-` or `:`; no `,` before the first `:`; no credentials. Credentials live only in the running user's `rclone.conf` on the host. Run messages are redacted of the remote's path like the restic job redacts its repository.
- Overlap rule, per host: an enabled mirror's remote path may not equal, contain or sit inside another enabled mirror's remote path on the same host, and may not contain or equal the remote path of an enabled restic backup job's `rclone:<remote>` repository on that host.
- Speed limit: optional `bwlimit` = digits with optional single `K`/`M`/`G` suffix (bytes per second, rclone `--bwlimit`); blank = none.
- `rclone` is an opt-in sudoers feature `Feature::Rclone` (name `rclone`), listed like `restic` (`--with rclone`), not in the default set.
- Cancel (`c`) stops within seconds, destroys the clone, records `cancelled`, never failed.
- Data safety first: every ambiguity resolves toward not destroying and not deleting.

## Review Focus

1. A dataset whose files are all symlinks or empty directories: the run is refused (count 0), nothing in the bucket is moved to `old/`.
2. Remote strings that smuggle credentials or options: `:s3,access_key_id=X:b/p`, `-x:b`, `a,b:c`, `aws:` with spaces/newlines, an empty path.
3. A remote path or dataset path with spaces or unicode: every value is one argv element (no shell), never split or re-quoted.
4. rclone missing on the host or sudo entry missing: a clear failed run, clone destroyed, nothing synced.
5. A remote that overlaps a restic job's `rclone:` repository or another mirror, including a parent/child path and a trailing slash or `name:` bucket-root spelling.

---

### Task 1: `src/rclone.rs` — pure rclone logic (and a real-output spike)

**Files:**
- Create: `src/rclone.rs` (declare `mod rclone;` in `src/main.rs`)
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Produces (all `pub`):
  - `fn valid_remote(s: &str) -> bool` and `fn valid_bwlimit(s: &str) -> bool` per Global Constraints.
  - `fn join(remote: &str, tail: &str) -> String` — `join("aws:bkt/photos","current")` = `"aws:bkt/photos/current"`; `join("aws:","current")` = `"aws:current"`; trailing slashes on `remote` are trimmed.
  - `fn run_stamp(at: DateTime<Utc>) -> String` — `YYYYMMDDTHHMMSSZ`.
  - `fn sync_args(source: &str, remote: &str, stamp: &str, bwlimit: Option<&str>) -> Vec<String>` — `["sync", source, join(remote,"current"), "--backup-dir", join(remote, "old/<stamp>"), ("--bwlimit", x)?, "--use-json-log", "--stats", "1s", "--stats-log-level", "NOTICE", "--log-level", "NOTICE"]` (exact stats flags per the spike below; the first six elements are fixed).
  - `fn size_args(path: &str) -> Vec<String>` (`size --json <path>`), `fn parse_size_count(json: &str) -> Result<u64, String>` (the `count`).
  - `fn lsf_args(remote: &str) -> Vec<String>` (read-only listing, `--max-depth 1`, `--contimeout`/`--timeout` short).
  - `enum Line { Stats { bytes: u64, total: u64 }, Error(String), Other }`, `fn parse_line(line: &str) -> Line`.
  - `fn redact(text: &str, remote: &str) -> String`, `fn failure_text(stderr: &str, remote: &str) -> String`.
  - `enum Check { Ok, Problem(String) }`, `fn classify_check(status: i32, stdout: &str, stderr: &str, remote: &str) -> Check` — plain-words problems: remote name not configured ("didn't find section in config file"), bucket/directory not found, access denied/forbidden, other.
  - `fn overlaps(a: &str, b: &str) -> bool` — same remote name and one path (split on `/`, empty segments ignored) is a prefix of the other; different remote names never overlap.

- [ ] **Step 0: Spike on the hub (read-only for cloud, throwaway locally).** On the hub (`fb@172.16.10.90`, NOPASSWD sudo) confirm `rclone` is installed (`pkg install rclone` if not; ask before installing), create a throwaway local remote with `sudo rclone config create ztlocal local`, and with a scratch directory capture the **real** output of: `rclone sync SRC ztlocal:/tmp/zt-x/current --backup-dir ztlocal:/tmp/zt-x/old/STAMP --use-json-log --stats 1s --stats-log-level NOTICE --log-level NOTICE`, `rclone size --json DIR`, `rclone lsf --max-depth 1 ztlocal:/nonexistent`, `rclone lsf` on an unknown remote name, and `rclone config file`. Save each as a fixture string in the tests below. Confirm whether `--bwlimit 1M` is accepted and whether `--` is understood. Do not guess formats: these captures are the test inputs.
- [ ] **Step 1: Write failing tests** for every function above with the captured fixtures: valid/invalid remotes (`aws:bkt/p`, `b2:bkt`, `aws:`, `my-remote_1:a/b c` valid; `:s3,access_key_id=X:b/p`, `-x:b`, `a,b:c`, `nocolon`, `aws:b\nx`, empty invalid), bwlimit (`5M`, `500K`, `1G`, `100` valid; `5MB`, `M`, `-1`, `1.5M`, `0x10` invalid), `join` cases, `sync_args` exact prefix and optional bwlimit, `parse_size_count`, `parse_line` on a real stats line and an error line, `classify_check` for ok / unknown remote / missing bucket / access denied, `overlaps` (`aws:b/p` vs `aws:b/p/q` true, vs `aws:b/pq` false, vs `b2:b/p` false, trailing slash, `aws:` vs `aws:b` true).
- [ ] **Step 2:** `cargo test rclone::` → FAIL (module missing).
- [ ] **Step 3: Implement** the functions above in `src/rclone.rs` (signatures fixed here; bodies are yours).
- [ ] **Step 4:** `cargo test rclone::` → PASS, `cargo test` green.
- [ ] **Step 5: Commit** `feat: pure rclone logic for the cloud mirror job`.

---

### Task 2: The job kind in `jobs.rs`

**Files:**
- Modify: `src/jobs.rs` (`Job` struct, `KIND_*` consts, `resolve`, a shared source check, `clash_reason`/`clashing_job`/flagging fn, `is_cancellable`, constructors/tests), `src/config.rs` (`documented_config_keys` and its serialising test literal), every `Job { .. }` literal the compiler flags (add `remote: None, bwlimit: None` — do it with a regex across `src/` as in the previous feature).
- Modify: `docs/book/21-reference-config.md` (rows for `remote`, `bwlimit`, and `rclone` under `kind`).

**Interfaces:**
- Consumes: `rclone::{valid_remote, valid_bwlimit, overlaps}`, existing `backup_source`, `check_backup_fields` source logic.
- Produces: `pub const KIND_MIRROR: &str = "rclone"`; `Job.remote: Option<String>`, `Job.bwlimit: Option<String>` (`skip_serializing_if = "Option::is_none"`); `fn check_mirror_fields(job: &Job) -> Result<(), String>`; `Job::test_mirror(id, dataset, prefix, remote) -> Job` (test constructor like `test_backup`); `clash_reason` also returns a reason for mirror/mirror and mirror/restic overlaps.

- [ ] **Step 1: Write failing tests** in `jobs.rs`: `a_good_mirror_job_resolves`; `a_mirror_job_needs_dataset_and_a_valid_remote` (each bad remote from Task 1); `bwlimit_is_validated_and_optional`; `the_source_rules_match_the_backup_job` (new needs keep ≥ 1, pinned needs a valid snapshot name, unknown source rejected, path validated); `remote_and_bwlimit_are_rejected_on_other_kinds`; `mirror_jobs_on_one_host_with_overlapping_remotes_clash` (equal, parent, child; different host or different remote name do not); `a_mirror_whose_remote_contains_a_restic_jobs_rclone_repository_clashes` (`repository = "rclone:aws:bkt/x/current"` vs mirror `aws:bkt/x`); `disabled_jobs_never_clash`; `mirror_is_cancellable`; `a_jobs_toml_without_the_new_keys_loads_and_the_keys_round_trip`; the `documented_config_keys` test lists `remote` and `bwlimit`.
- [ ] **Step 2:** `cargo test jobs:: config::` → FAIL.
- [ ] **Step 3: Implement.** Factor the backup source checks out of `check_backup_fields` into a shared `check_source_fields(job)` used by both (backup behaviour and messages unchanged — its existing tests must pass untouched); add `check_mirror_fields` (dataset valid, source fields, `valid_remote`, optional `valid_bwlimit`, id valid like backup's since the id names the clone, no `repository`/`password_file`/`retention`); wire `KIND_MIRROR` into `resolve` and `is_cancellable`; add the overlap rules to `clash_reason`/the flagging function (per host, enabled, well-formed only).
- [ ] **Step 4:** `cargo test` → PASS.
- [ ] **Step 5: Commit** `feat: the rclone job kind, its fields, validation and overlap rule`.

---

### Task 3: `Feature::Rclone` (opt-in sudoers)

**Files:**
- Modify: `src/exec/privilege.rs` (enum, `ALL`, `name`, `ENTRIES`, the default-set filter), `src/help.rs` / `src/main.rs` if the feature list is printed, `docs/book/22-reference-commands.md`, `docs/book/04-installing.md` (wherever `restic` is listed as opt-in), README sudoers section.
- Test: `src/exec/privilege.rs` tests, help/book-check tests.

**Interfaces:**
- Produces: `Feature::Rclone` (`name() == "rclone"`), `Feature::ALL` of 8 entries, `Entry { name: "rclone", features: &[Feature::Rclone], kind: ANY }`. `rclone` is excluded from the default set unless added with `--with rclone`, exactly like `restic`.

- [ ] **Step 1: Write failing tests** next to the restic ones: `the_default_set_has_no_rclone`; `with_rclone_adds_it_and_without_removes_it`; `privileged_for(&[Feature::Rclone])` names exactly `["rclone"]`; `Feature::parse("rclone")`; the help feature list and the book's commands reference mention `rclone` (the existing help↔book tie test stays green).
- [ ] **Step 2:** `cargo test exec::privilege help book` → FAIL.
- [ ] **Step 3: Implement** by mirroring every place `Feature::Restic` appears (grep it; derive the list from the code, not memory).
- [ ] **Step 4:** `cargo test` → PASS.
- [ ] **Step 5: Commit** `feat: opt-in rclone sudoers feature`.

---

### Task 4: Extract the shared source and clone helpers from `backup.rs`

**Files:**
- Modify: `src/daemon/backup.rs`
- Test: the existing `backup.rs` tests are the safety net; add tests only for the new parameter.

**Interfaces:**
- Produces (`pub(super)` in `backup.rs`, moved out of `run_backup` unchanged in behaviour):
  - `fn clone_name_for(prefix: &str, dataset: &str, job_id: &str) -> String` (`<pool>/<prefix>-<job id>`; `clone_name` becomes `clone_name_for("zit-restic", ..)`, a mirror uses `"zit-mirror"`).
  - `enum SourceEnd { Skip(String), Fail(String) }` and `fn choose_snapshot(runner, clock, job, dataset) -> Result<(String /*snapshot*/, String /*prune note*/), SourceEnd>` — the `BackupSource::{Fresh,Pinned,Prefix}` match from `run_backup` (including `prune_fresh` before taking a fresh snapshot, and the "already exists" skip).
  - `fn make_clone(runner, snapshot_origin: &str, clone: &str, mount: &str) -> Result<(), String>` and the existing `destroy_clone` kept `pub(super)`.
  - `fn target_dir(job: &Job, mount: &str) -> String` — mount plus the optional `path`.

- [ ] **Step 1:** Run `cargo test daemon::backup` → record the passing count (the contract: identical after the refactor).
- [ ] **Step 2: Write a failing test** `clone_name_for_uses_the_given_prefix` (`clone_name_for("zit-mirror","tank/data","j") == "tank/zit-mirror-j"` and `clone_name("tank/data","j") == "tank/zit-restic-j"`).
- [ ] **Step 3: Refactor** `run_backup` to call the extracted helpers; no message text or call order may change.
- [ ] **Step 4:** `cargo test daemon::backup` → same count, all PASS; `cargo test` green.
- [ ] **Step 5: Commit** `refactor: share the source choice and clone lifecycle between backup and mirror`.

---

### Task 5: The mirror run (`src/daemon/mirror.rs`) and its dispatch

**Files:**
- Create: `src/daemon/mirror.rs` (declare in `src/daemon/mod.rs`)
- Modify: `src/daemon/mod.rs` (`execute` match: `KIND_MIRROR => Some(self.mirror(&ctx, &due, &run_id))`, a `mirror` fn like `backup` using `run_cancellable`), anywhere the daemon lists kinds for resume/notify/history (grep `KIND_BACKUP` in `src/daemon`).
- Test: `mirror.rs` tests with a scripted `Host` fake modelled on `backup.rs`'s.

**Interfaces:**
- Consumes: Task 1 (`sync_args`, `size_args`, `parse_size_count`, `parse_line`, `failure_text`, `redact`, `run_stamp`), Task 4 helpers, `Env`, `run_lines`, `ProgressWriter`.
- Produces: `pub fn run_mirror(env: &Env, runner: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome`. Lock key `#mir@<job id>`. Messages: ok `mirrored <snapshot> to <redacted remote>, <bytes> transferred`; skipped `skipped: already running`.

- [ ] **Step 1: Write failing tests** (fake runner records every call; scripted shell for `rclone sync`):
  - `a_run_clones_checks_the_source_syncs_with_a_backup_dir_and_destroys_the_clone`: call order `zfs clone` → `rclone size --json` → `rclone sync` (args equal `sync_args` with the run's UTC stamp and the job's `bwlimit`) → clone destroyed; result Ok.
  - `an_empty_source_is_refused_and_nothing_is_synced` (size count 0 → Failed, no `rclone sync`, clone destroyed).
  - `a_failed_clone_or_missing_snapshot_touches_nothing`.
  - `unreadable_files_fail_the_run` (sync exit non-zero → Failed with the redacted failure text; the remote string never appears in the message).
  - `skipped_items_are_not_mentioned` (a sync whose output contains `Can't follow symlink`/NOTICE lines and exit 0 → Ok, message has no "skipped"/"symlink").
  - `cancel_stops_the_sync_destroys_the_clone_and_records_cancelled`.
  - `the_clone_is_destroyed_on_every_exit_path` (success, failure, cancel).
  - `an_existing_clone_from_a_crashed_run_is_removed_first`; `a_remote_and_path_with_spaces_stay_single_arguments` (assert the recorded `sync` argv contains `"aws:my bkt/a b/current"` and the target dir as single elements).
  - `source_choices_new_snapshot_prefix_work_like_backup` (one test per source; `new` prunes only its own `zit-` snapshots via the shared helper).
  - `only_sync_and_read_only_rclone_commands_are_ever_run`: across all tests above, assert no call's first arg is in `{delete, purge, deletefile, cleanup, move}` and no arg is `--delete-excluded`/`--links`.
  - `a_missing_rclone_gives_a_clear_failed_run` (runner error → Failed, clone destroyed).
- [ ] **Step 2:** `cargo test daemon::mirror` → FAIL.
- [ ] **Step 3: Implement** `run_mirror` following `run_backup`'s order (lock, stale clone removal, `choose_snapshot`, clone, `Started` event, size check, sync via `run_lines` with progress from `parse_line`, destroy clone always, finish). Progress `total` from the size check's bytes.
- [ ] **Step 4:** `cargo test` → PASS.
- [ ] **Step 5: Commit** `feat: the cloud mirror run`.

---

### Task 6: The TUI form, key, confirm and display

**Files:**
- Modify: `src/ui/modal.rs` (`FormKind::MirrorJob { hosts, existing, jobs }`), `src/ui/state/forms.rs` (`mirror_job_form`, `build_mirror_job_action`, `job_save_prompt` arm), `src/ui/state/modal_keys.rs` (`submit_job_form`), `src/ui/state.rs` (new-job key, `Enter` edit dispatch at the `KIND_BACKUP` arm, `form_values` plumbing), `src/ui/picker.rs` (F2 for host/dataset/snapshot fields as the backup form does), `src/ui/jobs.rs` (`job_target`: `<source> → <remote>`; `source_summary` reuse), `src/ui/layout.rs` (job panel lines: `to <remote>  [limit X]  <source summary>` + progress), `src/ui/hints.rs`, `docs/book/20-reference-keys.md`.
- Test: `src/ui/state/tests.rs`, `layout.rs`/`hints.rs` tests.

**Interfaces:**
- Consumes: Task 2 job fields, existing backup-form helpers (copy the field order and parsing of source/path/keep from `backup_job_form`/`build_backup_job_action`; reuse, do not duplicate, any shared parser).
- Produces: field order **Host, Dataset, Path inside the dataset, Source, Fresh keep, Remote, Speed limit, Schedule, Catch up** (index of Remote = 5; keep the backup form's index logic for Source/F2); `fn build_mirror_job_action(kind: &FormKind, fields: &[FormField], state_dir: &Path, now: DateTime<Utc>) -> Result<PendingAction, String>`; new-job key **`C`** in Jobs (verify it is unused with `grep`; if taken pick another and record it as a ruling); job id `mirror-<host>-<dataset>-<tag>` via a `new_mirror_job_id` like `new_backup_job_id`.

- [ ] **Step 1: Write failing tests:** `C_in_jobs_opens_a_blank_mirror_form_with_defaults` (schedule default `30 3 * * *`, catch-up y, source `new`); `a_valid_form_builds_a_save_with_remote_and_bwlimit`; `a_bad_remote_or_bwlimit_is_refused_with_a_message_naming_the_field`; `a_blank_bwlimit_saves_none`; `a_clashing_mirror_is_refused_on_save` (overlap rule message); `enter_on_a_mirror_job_edits_it_prefilled`; `saving_a_mirror_job_asks_first_and_names_the_remote_and_says_files_are_moved_to_old` (confirm text contains the remote, "old/", "never deletes"; second keystroke pattern as the other confirms); `job_target_shows_source_arrow_remote`; the panel shows `limit 5M` only when set; the hint bar for Jobs lists `C`.
- [ ] **Step 2:** `cargo test ui::` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** `cargo test` → PASS; `cargo clippy --all-targets` shows no new warnings in touched files.
- [ ] **Step 5: Commit** `feat: cloud mirror form, key and display`.

---

### Task 7: F5 remote check in the mirror form

**Files:**
- Modify: `src/preflight.rs` (add `run_remote`), `src/ui/state.rs` (`request_backup_preflight` generalised: a `remote_check: bool` on `PreflightCheck`/`PendingPreflight`; `run_backup_preflight` branches on it), `src/ui/state/modal_keys.rs` (F5 for `FormKind::MirrorJob`), `src/ui/layout.rs` (render the result line in the mirror form as the backup form does), `src/ui/mod.rs` (no change expected; the existing `apply_pending_preflights` hands out the runner).
- Test: `preflight.rs`, `src/ui/state/tests.rs`, `src/ui/mod.rs` tests.

**Interfaces:**
- Consumes: `rclone::{lsf_args, classify_check, Check}`, existing `PreflightCheck`/`WorkerMsg::Preflight` flow (a mirror check stores the remote in `repository` and `""` in `password_file`).
- Produces: `pub fn run_remote(runner: Arc<dyn CommandRunner>, host: &str, remote: &str, deadline: Duration) -> PreflightReport` — runs `rclone lsf <remote> --max-depth 1 …` read-only, maps `Check` to `restic::Preflight`-compatible result (`Ok`/`Problem`) and adds a `file_note` from `rclone config file` + a stat of the printed path (mode/owner advice like the password-file note; unreadable → no note), plus a warning when `remote` itself ends in `old` or `current` ("this path looks like a mirror's own folder").

- [ ] **Step 1: Write failing tests:** `f5_in_the_mirror_form_queues_one_check_for_the_forms_host_and_remote`; a bad or empty remote says why without running anything; `run_remote_reports_ok_unknown_remote_missing_bucket_and_access_denied` (fixtures from Task 1); `a_slow_remote_gives_up_after_the_deadline`; `a_world_readable_rclone_conf_gets_a_chmod_note`; `no_check_runs_a_command_that_writes` (assert only `lsf`/`config`/`stat` are run); the unknown-host-index test mirrors the preflight one.
- [ ] **Step 2:** `cargo test preflight ui::` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** `cargo test` → PASS.
- [ ] **Step 5: Commit** `feat: F5 remote check for the cloud mirror form`.

---

### Task 8: Documentation and the cloud account guide

**Files:**
- Modify: `docs/book/15-backups.md` (new section "Mirror to the cloud (rclone)" — the form, the `current/` + `old/<time>/` layout, what is and is not copied (symlinks, sockets, devices, empty directories, owners and permissions are not), how to restore by browsing the bucket or `rclone copy`, a safety table, how old versions are never deleted by zfsctl and how to prune them by hand), a "Cloud account guide" subsection (AWS: IAM user with a policy limited to one bucket — `s3:ListBucket`, `s3:GetObject`, `s3:PutObject`, `s3:DeleteObject`; Backblaze: an application key restricted to one bucket with list/read/write/delete; the key needs delete because moving a file is a copy then a delete; B2 keeps every version by default so recommend the bucket lifecycle "keep only the last version" while saying "keep all versions" is a safer-but-costlier choice; how to run `rclone config` as root on the host; optional S3 bucket versioning tip; `rclone crypt` makes files unbrowsable), `docs/book/20-reference-keys.md` (the `C` key and form keys), `docs/book/21-reference-config.md` (already touched in Task 2 — verify), `docs/book/22-reference-commands.md`, `README.md` (job kinds list, the keys list, the sudoers features list), `src/help.rs` if it lists job kinds or features, a new capture `docs/book/captures/15-mirror-form.txt` embedded as a `screen` block.
- Test: `src/book_check.rs` (every embedded screen equals its capture; every config key documented; headings tied to help text).

- [ ] **Step 1: Write the docs** per the list; do not hand-edit the capture. Placeholders for the screen block are not allowed: do Task 9 step 1 first if a capture is needed, or write the section without the screen block and add it in Task 9.
- [ ] **Step 2:** `cargo test book help` → FAIL first where a new heading/key needs wiring, then PASS.
- [ ] **Step 3: Commit** `docs: cloud mirror job and the AWS/Backblaze account guide`.

---

### Task 9: Live verification on the hub (local remote; no real cloud)

No new code unless it finds a bug (fix test-first). Per `feedback_verify_feature_works_end_to_end` and `feedback_smoke_scripts_must_not_send_mutating_keys`: throwaway datasets and a throwaway local rclone remote only; install the build on the hub only after asking the user (keep the old binary as `/usr/local/bin/zfsctl.pre-mirror`).

- [ ] **Step 1:** Build on the hub (tar the tree over ssh as before), install with the user's go-ahead, `service zfsctl restart`. Drive the TUI with pexpect/pyte (venv in the scratchpad) to create a mirror job on `scratch/zt-mir-src` → `ztlocal:/tmp/zt-mir/bucket` with `Keep` 2 and a `1M` limit; capture the form screen with `docs/book/tools/capture.py` as `15-mirror-form`.
- [ ] **Step 2:** Verify with files: first run → `current/` equals the source; change one file, delete one, add one → second run: `current/` mirrors, the changed and deleted originals are under `old/<stamp>/`, nothing lost; confirm `zfs list` shows the clone gone and `zit-mirror-*` fresh snapshots pruned to `keep`.
- [ ] **Step 3:** Safety paths: an empty dataset is refused with nothing moved; a dataset of only symlinks is refused; cancel (`c`) mid-run on a large scratch file leaves the clone destroyed and `cancelled` in history; F5 reports ok / unknown remote / missing path / (chmod 644 `rclone.conf`) note; a second job overlapping the first is refused on save.
- [ ] **Step 4:** Clean up: delete the scratch jobs, datasets and `ztlocal` remote and `/tmp/zt-mir`; confirm the hub's `jobs.toml` is back to `scrub-local-tpool`.
- [ ] **Step 5:** Record in the final report that real AWS S3 and Backblaze B2 are **untested** until the user has accounts, then merge per `finishing-a-development-branch` with the user's go-ahead.
