# Replication Target Retention Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A host-to-host replication job keeps only the newest N of its own snapshots on the destination dataset (`dest_keep`), pruned automatically after each run, with a preview-and-confirm when saving a setting that would destroy snapshots.

**Architecture:** A pure `prune_selection` in `src/zfs/replication.rs` is the single source of truth; the daemon calls it as the last step of `run_replicate` (under the destination lock, after a send and after an "up to date" skip), and the TUI form calls the same function on a destination listing fetched in a worker thread (same queue-and-runner pattern as the backup form's F5 preflight) before it saves.

**Tech Stack:** Rust, existing `CommandRunner` fakes, ratatui TUI, serde/toml `jobs.toml`.

**Spec:** `docs/superpowers/specs/2026-10-16-replication-target-retention-design.md`

## Global Constraints

- `dest_keep` is optional in `jobs.toml`; absent = keep everything; minimum 1. An old `jobs.toml` loads unchanged.
- Valid only on host-target replication jobs (`dest_host != "file"`); rejected on file-target replication, snapshot, scrub and backup jobs.
- Destroy only snapshots that pass `is_own(prefix, name)` (`src/daemon/snapshot.rs`), are outside the newest N by `createtxg` (never by name), and are not the newest own snapshot.
- Scope: destination dataset only, depth 1. No `-r`, no wildcards, no `--` before names (OmniOS treats `--` as a dataset operand): validate the dataset with `jobs::valid_dataset` and the snapshot name with `jobs::valid_snapshot_name` instead, and run one `zfs destroy <dataset>@<snap>` per snapshot.
- Nothing is pruned when the transfer failed, was cancelled or resumed; when the destination listing fails or any line is unparsable; or when there are no more than N own snapshots.
- A destroy refused for a hold or a dependent clone is "kept", not an error. Any other destroy failure marks the run Failed (message says the transfer succeeded) and the remaining snapshots are still tried.
- Data safety first: every ambiguity resolves toward not destroying.

## Review Focus

1. Snapshot names that look like the prefix but are not (`hourly-latest`, `hourly-20261006T1000Z`, `hourlyx-2026…`, hand-made `hourly-20261006T100000Z-keep`): never selected.
2. Names that sort differently than they were created (clock stepped back, `createtxg` order disagrees with name order): `createtxg` decides.
3. `dest_keep` set on a job that later gets edited to a file target: rejected by `resolve`, never silently ignored.
4. Destination dataset does not exist yet (first run, `list_snapshot_infos_or_empty`): prune sees an empty list and does nothing.
5. A stale preview: the form shows N to destroy, then the target gains/loses snapshots before the run; the daemon re-selects and obeys only the rules above.

---

### Task 1: The `dest_keep` job field and its validation

**Files:**
- Modify: `src/jobs.rs` (struct `Job` ~line 31, `check_replicate_fields`, `resolve`, the `test_*` constructors ~392-401, tests module)
- Modify: every other `Job { .. }` literal the compiler flags (e.g. `src/config.rs:505`, `src/ui/state/forms.rs` job builders): add `dest_keep: None` (the replication builder gets the real value in Task 4).

**Interfaces:**
- Produces: `Job.dest_keep: Option<u32>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`, doc comment: "A host-target replication job's retention on the destination: keep this many of the job's own snapshots there; absent keeps everything").

- [ ] **Step 1: Write failing tests** in `src/jobs.rs` tests (use the existing `bad(&|j| ..)` helper style next to the `dest_host` tests ~line 777):
  - `dest_keep_of_zero_is_rejected`: a host-target replicate job with `dest_keep = Some(0)` → `resolve` error containing `"dest_keep"`.
  - `dest_keep_is_accepted_on_a_host_target_replication_and_nowhere_else`: `Some(3)` is `Ok` on `Job::test_replicate("r","tank/data","hourly","fbsd16","backup/data")`; error containing `"dest_keep"` for the same job with `dest_host = Some("file")` (and `dest_dataset = None`, `dest_dir = Some("/var/streams")`), for `test_snapshot`, a scrub job, and `test_backup`.
  - `dest_keep_round_trips_and_an_old_file_loads`: a job with `Some(5)` survives toml serialize/parse; the `jobs.toml` text of a job without it contains no `dest_keep` line and loads with `None` (extend the "absent keys" test at ~line 757 to list `dest_keep`).
- [ ] **Step 2:** Run `cargo test jobs:: -- dest_keep` → FAIL (field missing / does not compile).
- [ ] **Step 3: Implement** the field; add the `dest_keep` checks in `resolve` (not only `check_replicate_fields`, so non-replicate kinds are covered): reject `Some(0)` anywhere, and reject `Some(_)` unless `kind == KIND_REPLICATE && dest_host != Some(DEST_FILE)`. Messages: `"dest_keep must be at least 1"`, `"dest_keep is only for a replication job to a host, not to a file"`.
- [ ] **Step 4:** Run `cargo test` → all PASS (fix every flagged `Job` literal).
- [ ] **Step 5: Commit** `feat: dest_keep job field and validation`.

---

### Task 2: `prune_selection` (pure)

**Files:**
- Modify: `src/zfs/replication.rs` (function + tests)

**Interfaces:**
- Consumes: `SnapInfo { name, guid, createtxg }`, `is_own`.
- Produces: `pub fn prune_selection(listing: &[SnapInfo], prefix: &str, keep: u32) -> Vec<String>` — snapshot names (the part after `@`) to destroy, oldest first. Unparsable input is already an `Err` from `parse_snapshot_infos`, so this function takes parsed data and cannot guess.

- [ ] **Step 1: Write failing tests** (named after the behaviour):
  - `only_exact_prefix_timestamp_names_are_selected`: listing with `hourly-20261006T100000Z`(txg 10), `hourly-20261006T110000Z`(20), `hourly-20261006T120000Z`(30), plus look-alikes `hourly-latest`, `hourly-20261006T1000Z`, `hourlyx-20261006T090000Z`, `manual`, `hourly-20261006T090000Z-keep` at the lowest txgs; keep 1 → exactly the first two names, oldest first.
  - `order_is_by_createtxg_not_name`: names sort opposite to txg; keep 1 destroys the lower-txg ones.
  - `the_newest_is_kept_at_keep_one_and_nothing_is_selected_when_there_are_n_or_fewer`: keep 1 with one snapshot → empty; keep 3 with three → empty; keep 3 with four → the single oldest.
  - `keep_zero_selects_nothing`: defensive — `keep == 0` returns empty (never destroys everything).
- [ ] **Step 2:** `cargo test zfs::replication -- prune` → FAIL.
- [ ] **Step 3: Implement** `prune_selection`: filter `is_own`, sort by `(createtxg, name)` ascending, return all but the last `keep` (empty when `keep == 0`).
- [ ] **Step 4:** run → PASS.
- [ ] **Step 5: Commit** `feat: prune_selection for replication targets`.

---

### Task 3: Pruning in the daemon run

**Files:**
- Modify: `src/daemon/replicate.rs` (`run_replicate`, new `prune_destination`, tests incl. extending the fake `Host` to answer `destroy`)
- Modify: `src/zfs/replication.rs` only if a destroy helper is wanted there (put `destroy_args`-style helpers next to `receive_args`)

**Interfaces:**
- Consumes: `Job.dest_keep` (Task 1), `prune_selection` (Task 2), `list_snapshot_infos`, `jobs::{valid_dataset, valid_snapshot_name}`.
- Produces: `struct Pruned { destroyed: usize, kept: usize, failures: Vec<String> }` and `fn prune_destination(dst: &dyn CommandRunner, dest_ds: &str, prefix: &str, keep: u32) -> std::result::Result<Pruned, String>` (`Err` = listing failed or dataset/name invalid; nothing was destroyed). Message suffix helper `fn prune_note(p: &Pruned, dest_host: &str) -> String` → `"; pruned 12, kept 6 on t2"` (kept = own snapshots remaining on the target, i.e. survivors, not the refused ones; refused ones are reported as `", 1 held or cloned"`; empty string when nothing was selected and `dest_keep` is unset).

- [ ] **Step 0 (verify before coding — never assume CLI text):** on the hub, with a throwaway dataset, capture the exact `zfs destroy` stderr for (a) a held snapshot and (b) a snapshot with a clone, on FreeBSD (`fb@172.16.10.90`, test VMs), via ssh + sudo. Record the substrings in a `const` list in `replicate.rs` (the snapshot job uses `"dependent clones"`; confirm that and find the hold wording). Do not touch real data.
- [ ] **Step 1: Write failing tests** in `replicate.rs` tests (extend `Host`: a `destroy_fail: Vec<(String /*target*/, String /*stderr*/)>`; `run` arm `Some("destroy")` records and answers; add `destroyed()` helper returning the last arg of each destroy query). Destination listing in each test has e.g. six own snapshots `backup/data@hourly-…` with txgs, job has `dest_keep = Some(3)`:
  - `prunes_the_destination_after_a_send`: after an incremental send, exactly the three oldest are destroyed, each as one query `["destroy", "backup/data@<name>"]` (assert no `-r`, no `--`); message contains `"pruned 3"`.
  - `prunes_after_an_up_to_date_skip`: destination already has the source's newest; result stays `Skipped` with "up to date" and the prune suffix; destroys happened.
  - `prunes_nothing_when_the_transfer_failed_was_cancelled_or_resumed`: three runs (receive failure, cancel, token present) → `destroyed()` empty each time.
  - `prunes_nothing_when_the_destination_listing_cannot_be_read`: unparsable line in the post-run listing → no destroys, run `Failed`, message says the transfer succeeded and the prune was skipped.
  - `a_held_or_cloned_snapshot_is_kept_and_is_not_an_error`: one destroy answers the held stderr and one the clone stderr → run `Ok`, others destroyed, suffix mentions them.
  - `another_destroy_failure_fails_the_run_but_the_rest_are_still_pruned`: result `Failed`, message contains "transfer succeeded"-style wording plus the snapshot and error, remaining targets destroyed.
  - `no_dest_keep_means_no_listing_and_no_destroy`: existing behaviour untouched (no extra queries).
  - `look_alike_names_on_the_target_are_never_destroyed`.
- [ ] **Step 2:** `cargo test daemon::replicate` → FAIL.
- [ ] **Step 3: Implement** `prune_destination` + wiring in `run_replicate`: only when `job.dest_keep` is `Some(k)` and `dest_host != DEST_FILE`; call after the successful-transfer check (before the final `finish`) and in the `Plan::UpToDate` arm (restructure so that arm calls the prune and then `finish`es as `Skipped`, or `Failed` on prune failure). Refuse (Err, no destroys) if `valid_dataset(dest_ds)` fails or a selected name fails `valid_snapshot_name`. Destroy oldest first; refusals classified by the Step 0 substrings.
- [ ] **Step 4:** `cargo test` → PASS.
- [ ] **Step 5: Commit** `feat: prune old snapshots on the replication target`.

---

### Task 4: Form field and preview-then-confirm

**Files:**
- Modify: `src/ui/state/forms.rs` (`replicate_job_form`, `build_replicate_job_action`, new `prune_prompt`)
- Modify: `src/ui/state.rs` (`PruneCheck` state, `WorkerMsg::PrunePreview`, request/run/fail fns beside `request_backup_preflight`/`run_backup_preflight`/`fail_backup_preflight`), `src/ui/state/worker.rs` (`on_prune_preview`), `src/ui/state/modal_keys.rs` (`submit_job_form` replicate branch), `src/ui/mod.rs` (`apply_pending_prune_checks`, called in `event_loop` next to `apply_pending_preflights`)
- Test: `src/ui/state/tests.rs`, `src/ui/mod.rs` tests

**Interfaces:**
- Consumes: `Job.dest_keep`, `prune_selection`, `list_snapshot_infos_or_empty`.
- Produces: form field `Keep on target (blank = keep everything)` inserted after "Force" (shifting Schedule/Catch-up indices; update every positional index and existing test); `fn prune_prompt(dest_host: &str, dest_ds: &str, doomed: &[String], kept: usize) -> String`; `PendingPruneCheck { host: usize, host_name: String, dest_ds: String, prefix: String, keep: u32 }`; `WorkerMsg::PrunePreview(usize /*host*/, String /*dest_ds*/, Result<Vec<SnapInfo>>)`.

- [ ] **Step 1: Write failing tests** (`tests.rs`, driving the form like the existing replication form tests and the preflight tests at ~7621-7680):
  - `blank_keep_on_target_saves_with_no_check`; `keep_on_target_of_zero_or_text_is_refused_with_a_message`; `keep_on_target_on_a_file_destination_is_refused`.
  - `a_keep_on_target_queues_a_destination_check_and_does_not_save_yet`: submit → `pending_prune_checks` has one entry for the destination host/dataset/prefix/keep; no `PendingAction` returned; status shows "Checking snapshots on <host>…".
  - `an_empty_selection_saves_without_a_confirm` (worker message with ≤N own snapshots → the `SaveJob` action is handed out, no modal).
  - `a_selection_opens_a_confirm_naming_count_oldest_and_newest_destroyed_and_kept`: text matches the spec: `This will destroy N snapshots on <host> <dataset>: oldest <name> … newest destroyed <name>; K kept`, `Modal::Confirm` with `choice: None` (second keystroke), and declining returns the form with its values intact (`confirm_restore`).
  - `a_failed_listing_refuses_the_save_with_the_reason_and_keeps_the_form`.
  - `a_stale_answer_is_dropped` (message for a different dataset/keep than the pending check).
  - `apply_pending_prune_checks_runs_on_the_destination_hosts_runner` and `..._reports_an_unknown_host` (mirror the two preflight tests in `src/ui/mod.rs`).
  - `editing_without_touching_keep_still_checks`: an existing job with `dest_keep` re-saved runs the same check (selection empty → saves silently).
- [ ] **Step 2:** `cargo test ui::` → FAIL.
- [ ] **Step 3: Implement.** Form builder parses the new field (blank → `None`; `u32 ≥ 1`; error messages in the style of the existing parsers) and runs `crate::jobs::resolve` as today. In `submit_job_form`, for a replicate action whose `dest_keep` is `Some` and `dest_host != file`: stash `(form Modal, action)` in a `prune_check` slot, push the pending request, return `None`. `on_prune_preview` computes `prune_selection`; empty → store the action in a ready slot that the event loop takes and executes exactly like a key-returned `PendingAction` (find how `event_loop` dispatches one; add `ready_action: Option<PendingAction>` taken next to `drain_worker_messages` if nothing equivalent exists); non-empty → `Modal::Confirm` with `prune_prompt`, `confirm_restore = Some((prompt, form))`; `Err` → `reject` the form with the reason. Runner is the destination host's, resolved in `apply_pending_prune_checks` by index of `dest_host` in `app.hosts`.
- [ ] **Step 4:** `cargo test` → PASS; `cargo clippy --all-targets` clean as the repo requires.
- [ ] **Step 5: Commit** `feat: Keep on target in the replication form, with preview and confirm`.

---

### Task 5: Display, docs and help

**Files:**
- Modify: `src/ui/jobs.rs` (job detail/row text: show `keep N on target` for replication jobs with `dest_keep`), `src/ui/hints.rs` if the form hint list needs the field
- Modify: `README.md`, `docs/book/14-jobs.md` (replication chapter + a Safety note), `docs/book/21-reference-config.md` (row `dest_keep | integer | absent | replicate to a host: keep this many of the job's own snapshots on the destination`), `src/help.rs` only if it lists job fields
- Test: `src/ui/jobs.rs` tests, `src/book_check.rs` (stays green)

- [ ] **Step 1: Write failing test:** the jobs detail for a replication job with `dest_keep = Some(7)` contains `"7"` and "on target"; one without shows no such line.
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3: Implement** the display and write the docs. Book text must say what is destroyed (own `<prefix>-<timestamp>` snapshots only), that file targets are not pruned, held/cloned snapshots are left, and that the preview appears when saving. Regenerate any book screen capture of the replication form if the repo's capture tool in `docs/book/tools` covers it.
- [ ] **Step 4:** `cargo test` (includes the help/book heading check) → PASS.
- [ ] **Step 5: Commit** `docs: replication target retention`.

---

### Task 6: Live verification on the hub (no code unless it finds a bug)

Per `feedback_verify_feature_works_end_to_end` and `feedback_smoke_scripts_must_not_send_mutating_keys`: throwaway datasets only, never real jobs.

- [ ] **Step 1:** Install the build on the hub; create `tpool1/zt-scratch-src` with a few `@scratch-<UTC>` snapshots and a scratch replication job to t2 with `dest_keep = 2` (via the TUI, pexpect/pyte per `reference_driving_tui_with_pexpect_pyte`).
- [ ] **Step 2:** Verify in the TUI: lowering `dest_keep` shows the confirm with count and names; Esc returns to the form with values intact; raising/blank saves silently.
- [ ] **Step 3:** Run the job until the target holds more than 2 own snapshots; confirm on t2 (`zfs list -t snapshot`) that exactly the oldest were destroyed, the newest and the last-replicated base remain, a hand-made snapshot and a held snapshot (`zfs hold`) survive, and the next incremental still succeeds after a prune.
- [ ] **Step 4:** Check the history message suffix (`; pruned N, kept M on t2`), then remove the scratch job and datasets on both sides.
- [ ] **Step 5:** Fix anything found test-first, then merge per `finishing-a-development-branch`.
