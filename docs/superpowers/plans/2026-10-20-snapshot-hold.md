# Snapshot hold (slice 1 of snapshot recovery) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user protect a snapshot with a ZFS hold (`H` in Snapshots), see which snapshots are held, and make every destroy path (UI, jobs, replication pruning) refuse or skip a held snapshot.

**Architecture:** The hold is ZFS's own (`zfs hold zfsctl <snap>`, `zfs release`, `userrefs`). `zfs` is already allowed in full on hosts, so no sudoers change. A single guard function in `src/zfs/snapshot.rs` checks `userrefs` before any destroy; the UI, `destroy_snapshot_with`, the dataset destroys and the three job pruners all go through it, so no path can forget it.

**Tech Stack:** Rust, ratatui, the existing `CommandRunner`/`FakeRunner` test seam.

**Spec:** `docs/superpowers/specs/2026-10-20-snapshot-recovery-design.md` (slice 1). This plan changes only what slice 1 needs; slices 2 to 4 are separate plans.

## Global Constraints

- Data retention and security first. Nothing may delete, overwrite or move data, or make a snapshot easier to lose. The hold tag is the literal `zfsctl`; only that tag is ever released.
- Never use `zfs destroy -d` (deferred destroy).
- Every argument list is options-first (illumos `zfs` uses strict getopt) and uses `--` before a dataset or snapshot name, as the surrounding code does.
- Taking a hold: one y/n confirm (`Modal::Confirm`, two keys). Releasing: `Modal::ConfirmTyped` with the snapshot's full name.
- Works on FreeBSD, Linux and illumos. FreeBSD and OmniOS were verified on 2026-10-20: `zfs list -t snapshot -H -p -o name,used,userrefs` and `zfs get -r -H -p -t snapshot -o name,value userrefs <dataset>` both work; Linux is not yet verified.
- Rust only, no new dependencies. `cargo clippy --all-targets` warning count must not rise above 174 (the baseline on `master`).
- The book's tests stay green: every key in the Help screen is in the keys reference and every new term is in the glossary.

## Review Focus

1. A recursive destroy (`zfs destroy -r tank/data@s`) where only a **descendant's** same-named snapshot is held: must be refused as a whole, naming the held one.
2. `userrefs` that is `-` (a host or snapshot that doesn't report it): treated as unknown, not as zero for destroys. A destroy where the check itself fails is **refused**, never allowed.
3. A snapshot held by someone else's tag only: `H` must not release it (`zfs release zfsctl` fails cleanly) and the screen says so; the tag list is shown.
4. A job whose only prunable snapshot is held: the run is `Ok` with "kept (held)", not `Failed`, and the held snapshot is never destroyed.
5. A snapshot name or tag containing a space, tab or newline coming back from a host: shown sanitised, never interpolated into a command.

---

## File Structure

- `src/zfs/snapshot.rs`: the data (`Snapshot.holds`), the commands (`hold_snapshot`, `release_snapshot`, `hold_tags`) and the guard (`refuse_if_held`). One responsibility: snapshots on a host.
- `src/error.rs`: one new variant, `AppError::Held`.
- `src/ui/modal.rs`, `src/ui/state.rs`, `src/ui/hints.rs`, `src/ui/layout.rs`: the mutation calls, the key, the hint and the column.
- `src/daemon/snapshot.rs`, `src/daemon/backup.rs`, `src/daemon/replicate.rs`: pruning skips held snapshots.
- `docs/book/10-snapshots.md`, `20-reference-keys.md`, `23-glossary.md`, `src/help.rs`, `docs/book/captures/10-snapshots.txt`: documentation.

---

### Task 1: Read the holds

**Files:**
- Modify: `src/zfs/snapshot.rs` (struct `Snapshot`, `SNAPSHOT_LIST_ARGS`, `parse_snapshot_list`; new fns)
- Modify: every `Snapshot { .. }` literal (about 27, in tests across `src/ui`, `src/daemon`, `src/zfs`): add `holds: 0`
- Test: `src/zfs/snapshot.rs` `mod tests`

**Interfaces:**
- Produces: `Snapshot { dataset: String, name: String, used: u64, holds: u64 }`; `pub const HOLD_TAG: &str = "zfsctl"`; `pub fn parse_holds(output: &str) -> Result<Vec<(String, String)>>` (pairs of full snapshot name and tag from `zfs holds -H`); `pub fn hold_tags(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<Vec<String>>`.

- [ ] **Step 1: Write the failing tests** in `snapshot.rs` tests:
  - `parses_the_holds_column`: `parse_snapshot_list("tank/data@a\t10\t2\ntank/data@b\t5\t0\n")` gives `holds` 2 and 0.
  - `a_dash_for_holds_is_zero_held_unknown`: `"tank/data@a\t10\t-\n"` parses with `holds: 0` (the listing is only for display; the destroy guard in Task 2 does not rely on it).
  - `rejects_a_line_with_two_fields`: `"tank/data@a\t10\n"` is an error.
  - `list_snapshots_asks_for_userrefs`: `SNAPSHOT_LIST_ARGS == ["list","-t","snapshot","-H","-p","-o","name,used,userrefs"]`.
  - `parse_holds_reads_name_tag_and_ignores_the_timestamp`: input `"tank/data@a\tzfsctl\tFri Oct 20 10:00 2026\ntank/data@a\tkeep\tFri Oct 20 10:01 2026\n"` gives `[("tank/data@a","zfsctl"),("tank/data@a","keep")]`; empty input gives an empty vec.
  - `hold_tags_runs_zfs_holds_options_first`: with `FakeRunner::new("tank/data@a\tzfsctl\tx\n")`, `hold_tags(&r,"tank/data","a")` returns `["zfsctl"]` and the recorded call is `zfs holds -H -- tank/data@a`.
- [ ] **Step 2: Run** `cargo test snapshot::tests` — Expected: compile errors / FAIL.
- [ ] **Step 3: Implement** the struct field, the 3-column list and parse (a missing or `-` third field is an error only when the field is absent; `-` maps to 0), `parse_holds`, `hold_tags`. Update the literals with `holds: 0` (compiler lists them).
- [ ] **Step 4: Run** `cargo test` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: snapshots carry their hold count (userrefs) and hold tags can be read`.

### Task 2: The guard, and hold/release commands

**Files:**
- Modify: `src/error.rs` (new variant), `src/zfs/snapshot.rs`
- Test: `src/zfs/snapshot.rs` `mod tests`

**Interfaces:**
- Consumes: `HOLD_TAG`, `hold_tags` (Task 1).
- Produces: `AppError::Held(String)` with message `"protected: {0}"` (the String is already a full sentence); `pub fn hold_snapshot(runner, dataset: &str, name: &str) -> Result<()>` (`zfs hold -- zfsctl dataset@name`); `pub fn release_snapshot(runner, dataset, name) -> Result<()>` (`zfs release -- zfsctl dataset@name`); `pub fn held_in(runner: &dyn CommandRunner, dataset: &str, recursive: bool) -> Result<Vec<(String, Option<u64>)>>` (full snapshot name and its `userrefs`, `None` when not a number; one `zfs get -r -H -p -t snapshot -o name,value userrefs -- <dataset>` call, without `-r` when not recursive); `pub fn refuse_if_held(runner: &dyn CommandRunner, dataset: &str, name: &str, recursive: bool) -> Result<()>` (built on `held_in`); `destroy_snapshot_with` now calls `refuse_if_held` first.

- [ ] **Step 1: Write the failing tests**:
  - `hold_and_release_run_the_exact_commands`: recorded calls are `zfs hold -- zfsctl tank/data@a` and `zfs release -- zfsctl tank/data@a`.
  - `refuse_if_held_allows_an_unheld_snapshot`: FakeRunner answering `zfs get -r -H -p -t snapshot -o name,value userrefs tank/data` with `tank/data@a\t0\n`; `refuse_if_held(.., "tank/data","a",false)` is `Ok`.
  - `refuse_if_held_names_the_holder_and_the_tags`: the get returns `tank/data@a\t1\n` and `zfs holds -H` returns `tank/data@a\tkeep\tx\n`; result is `Err(AppError::Held(m))` and `m` contains `tank/data@a`, `1 hold` and `keep`.
  - `a_recursive_check_looks_at_descendants_of_the_same_name_only`: the get lists `tank/data@a\t0`, `tank/data/sub@a\t2`, `tank/data/sub@b\t5`; recursive refuses naming `tank/data/sub@a` (not `@b`); non-recursive allows.
  - `if_the_check_cannot_run_the_destroy_is_refused`: `FakeRunner::failing("boom",1)`: `refuse_if_held` is `Err` and `destroy_snapshot_with` never runs `zfs destroy` (assert no `destroy` call recorded).
  - `an_unreadable_userrefs_value_refuses`: `tank/data@a\t-\n` is refused (unknown is not zero).
  - `destroy_snapshot_never_uses_the_deferred_flag`: recorded destroy args contain neither `-d` nor any flag but `-r` when recursive.
- [ ] **Step 2: Run** `cargo test snapshot::tests` — Expected: FAIL.
- [ ] **Step 3: Implement.** `refuse_if_held` runs `zfs get -r -H -p -t snapshot -o name,value userrefs -- <dataset>` once (omit `-r` when not recursive; options first), keeps rows whose name ends in `@<name>` (and whose dataset is `dataset` or, when recursive, a descendant), refuses on any non-zero or non-numeric value, and on refusal fetches tags with `hold_tags` for the message (a tag-fetch failure still refuses, with a shorter message). A failing check returns `AppError::Held` with "could not check holds: ..." (never `Ok`).
- [ ] **Step 4: Run** `cargo test` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: refuse to destroy a held snapshot; hold and release commands`.

### Task 3: Dataset destroys and the UI mutation calls

**Files:**
- Modify: `src/ui/modal.rs` (`MutationCall` variants, `tone`, `label`, `success_text`, the dispatch at ~line 236), `src/zfs/dataset.rs` (destroy functions)
- Test: `src/ui/modal.rs` tests, `src/zfs/dataset.rs` tests

**Interfaces:**
- Consumes: `refuse_if_held`, `hold_snapshot`, `release_snapshot`.
- Produces: `MutationCall::HoldSnapshot { dataset: String, name: String }` (tone like `CreateSnapshot`, label `"HOLD SNAPSHOT"`, success text `"Protected <dataset>@<name>"`) and `MutationCall::ReleaseSnapshot { dataset, name }` (tone `Tone::Warn`, label `"RELEASE HOLD"`, success text `"Released the zfsctl hold on <dataset>@<name>"`); `zfs::dataset::destroy_dataset` and `destroy_dataset_recursive` first refuse when any of their snapshots is held.

- [ ] **Step 1: Write the failing tests**: `hold_call_runs_hold_snapshot`, `release_call_runs_release_snapshot` (as the existing `destroy_snapshot_call_runs_destroy_snapshot`); `tone_and_label_of_the_new_calls`; in `dataset.rs`: `destroying_a_dataset_with_a_held_snapshot_is_refused_and_runs_no_destroy` (a held snapshot among its snapshots, plain and recursive; the non-recursive destroy checks the dataset's own snapshots, the recursive one its whole tree).
- [ ] **Step 2: Run** `cargo test modal::tests dataset::tests` — Expected: FAIL (compile errors: `tone` and `label` have no wildcard arm, so the compiler lists every place to extend).
- [ ] **Step 3: Implement** the variants and arms; in `dataset.rs` call `snapshot::held_in` (Task 2) and refuse with `AppError::Held` when any snapshot is held or its count is `None`.
- [ ] **Step 4: Run** `cargo test` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: hold and release as confirmed mutations; dataset destroy refuses held snapshots`.

### Task 4: Jobs skip held snapshots

**Files:**
- Modify: `src/daemon/snapshot.rs` (~line 70), `src/daemon/backup.rs` (`prune_fresh`, ~line 140), `src/daemon/replicate.rs` (the `dest_keep` destroy, ~line 75)
- Test: the `tests` module of each file

**Interfaces:**
- Consumes: `AppError::Held` (Task 2). `destroy_snapshot_with` already returns it, so snapshot and backup pruning only classify it; replicate's raw `zfs destroy` on the destination host calls `refuse_if_held` first.

- [ ] **Step 1: Write the failing tests**:
  - `a_held_old_snapshot_is_kept_not_failed` (snapshot job): the destroy of the oldest excess snapshot is `Held`; the run result is `Ok` and the message contains `kept 1 (held)`; no further snapshot is destroyed in a way that violates oldest-first (later excess ones still are).
  - `prune_fresh_leaves_a_held_snapshot_and_says_so` (backup): the note contains `kept (held)`.
  - `the_destination_pruner_skips_a_held_snapshot_on_the_target_host` (replicate): the held one is not destroyed and counts as kept; the run is not `Failed`.
- [ ] **Step 2: Run** `cargo test daemon::` — Expected: FAIL.
- [ ] **Step 3: Implement.** In the snapshot job, match `Err(AppError::Held(_))` next to the existing `dependent clones` arm and count it as `held`; extend the message to `kept N (held)`. In `prune_fresh` the held case appends `; kept the old snapshot {old} (held)` instead of "could not remove". In replicate, call `refuse_if_held` against the destination runner before destroying and count a refusal under the existing `remaining`/kept accounting.
- [ ] **Step 4: Run** `cargo test` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: retention, backup and replication pruning skip held snapshots`.

### Task 5: The Snapshots screen

**Files:**
- Modify: `src/ui/layout.rs` (`draw_snapshots`), `src/ui/state.rs` (`key_lists_and_snapshots`), `src/ui/hints.rs` (Snapshots arm), `src/help.rs`
- Test: `src/ui/state/tests.rs`, `src/ui/hints.rs` tests, `src/ui/layout.rs` tests

**Interfaces:**
- Consumes: `Snapshot.holds`, `MutationCall::HoldSnapshot`/`ReleaseSnapshot` (Task 3), `hold_tags` (Task 1).
- Produces: a `Hold` column (`H` when the zfsctl tag is among the snapshot's tags, `h` when held by other tags only, blank when none); key `H`; a refusal shown as the existing status error when `d` is applied to a held snapshot.

- [ ] **Step 1: Write the failing tests**:
  - `capital_h_on_an_unheld_snapshot_opens_a_y_n_confirm_for_a_hold` (modal is `Modal::Confirm` with `MutationCall::HoldSnapshot`).
  - `capital_h_on_a_held_snapshot_opens_a_typed_confirm_for_a_release` (modal is `ConfirmTyped`, `expected` is `dataset@name`, call is `ReleaseSnapshot`).
  - `capital_h_with_nothing_selected_does_nothing`.
  - `d_on_a_held_snapshot_shows_the_refusal_and_runs_no_destroy` (after confirming the typed name, the worker returns the `Held` error and the status line shows `protected:`; no `zfs destroy` was run).
  - `the_hold_column_shows_a_capital_for_ours_and_a_small_h_for_others` (render test on the draw function with three snapshots).
  - `hints_offer_the_hold_key_when_a_snapshot_is_selected` and a Help-screen test that `H` is listed.
- [ ] **Step 2: Run** `cargo test ui::` — Expected: FAIL.
- [ ] **Step 3: Implement.** Add the `Hold` column (width 4) to `draw_snapshots`. After each snapshot refresh, fetch tags for the held ones only (`hold_tags` per held snapshot, on the worker thread beside `list_snapshots`) and keep `App.snapshot_tags: HashMap<String, Vec<String>>` keyed by `dataset@name`. `H` toggles as specified, using `snapshot_tags` to choose take or release; a snapshot held only by others opens no release modal and shows "held by another tag: <tags>; zfsctl will not release it". Hint `H` "Hold" with `Tone::Safe` (or the existing tone for harmless actions). Add `H` to `src/help.rs`.
- [ ] **Step 4: Run** `cargo test` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: Hold column and H key in the Snapshots section`.

### Task 6: Book, capture and live verification

**Files:**
- Modify: `docs/book/10-snapshots.md`, `docs/book/20-reference-keys.md`, `docs/book/23-glossary.md`, `docs/book/captures/10-snapshots.txt` (and the embedding in chapter 10)
- Test: `cargo test book_check`

- [ ] **Step 1: Ask the user** before creating anything on a host. With their yes, on the hub's scratch pool only: take a throwaway snapshot `scratch/data@zt-hold-test`, hold it through the TUI, confirm `userrefs` is 1 with `zfs get userrefs`, try `d` and see the refusal, try a snapshot job prune on a held snapshot if one is convenient, release it through the TUI with the typed confirm, confirm `userrefs` is 0, destroy the throwaway snapshot. Record the exact error text ZFS gives for destroying a held snapshot on FreeBSD (it is not needed by the code, which checks first, but belongs in the book's troubleshooting chapter).
- [ ] **Step 2: Linux and OmniOS.** Ask the user before creating a throwaway snapshot on the Ubuntu VM; on OmniOS repeat the hold, refusal and release on a throwaway snapshot of a scratch dataset only if one exists (never on `omni30`). If the user declines, the plan notes Linux as "unverified" in the book's Linux limitations.
- [ ] **Step 3: Write the book text**: a section "Protecting a snapshot" in chapter 10 (what a hold is, `H`, the column, that jobs skip held snapshots, how to release, that other people's holds are shown and never released), the `H` row in the keys reference, the glossary entry "hold".
- [ ] **Step 4: Retake `10-snapshots`** with the capture tool on the hub, with a held throwaway snapshot present, then remove it; replace the capture's embedding in chapter 10 and run `cargo test book_check` — Expected: PASS.
- [ ] **Step 5: Commit** `docs: protecting a snapshot (hold)`.

## Self-Review notes

- Spec slice 1 coverage: hold/release (Task 2, 3, 5), column and `H`/`h` marker (Task 5), destroy refusal in `d`, `D` and dataset destroy (Task 2, 3, 5), jobs skip held (Task 4), tag `zfsctl` only (Task 2, 3), no `-d` (Task 2), no new sudoers (verified, `Global Constraints`), book (Task 6).
- The destroy-path mutation pass from the spec's test plan is done after Task 5: remove `refuse_if_held` from `destroy_snapshot_with` and from each dataset destroy in turn and confirm a test fails each time, then restore.
- Deviation to flag: the spec's column is `H`/`h`/blank; Task 5 implements exactly that, which needs the tag fetch. If that proves too slow on a host with many held snapshots, fall back to the count from `userrefs` and say so in the book.
