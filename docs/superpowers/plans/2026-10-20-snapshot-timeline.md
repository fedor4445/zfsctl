# Snapshot timeline (slice 2 of snapshot recovery) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show what changed between a snapshot and the one before it (or the live dataset): a `Written` column in the Snapshots list, and a read-only "changes" view, grouped by folder, with file type and change time.

**Architecture:** `zfs diff -FHt` is streamed (never collected) through the existing cancellable line reader, parsed line by line into entries, and folded into a bounded summary (counts per folder plus a capped entry list). A new view inside the Snapshots section shows the summary; it is read-only and never changes data. Slice 1's hold (`H`) is reused to protect the older snapshot from the view.

**Tech Stack:** Rust, ratatui, chrono (already a dependency), the existing `CommandRunner` / `daemon::transfer::run_lines` / `FakeRunner` seams.

**Spec:** `docs/superpowers/specs/2026-10-20-snapshot-recovery-design.md` (slice 2). **Stacked on slice 1:** this branch is based on `snapshot-hold` (unmerged at the time of writing); it uses `Snapshot.holds`, `HoldSnapshot`, `AppError::Held`/`HoldCheck` and `crate::display_text::sanitise_for_display`.

## Global Constraints

- Read-only: nothing here may change data on a host. The only mutating action reachable from the view is slice 1's `HoldSnapshot` (one y/n confirm).
- **No new sudoers entry:** `zfs` is already allowed in full. Commands are run through `runner.command("zfs", ...)` (privileged).
- Argument lists are options-first with `--` before names where the command accepts it (`zfs diff -FHt -- <older> <newer>`: verified on FreeBSD and OmniOS; `zfs list`/`zfs get` take the dataset last without `--`, as the surrounding code does). Never `zfs destroy -d`.
- Limits (the spec's starting points, exact values): 5,000 entries listed per group; 2,000,000 diff lines read; 120 seconds; at most 100,000 entries kept in memory (counts are always complete up to the line limit). At a limit the read stops, the view says `stopped after N changes` and still shows what it has. Esc cancels at any time.
- A line that does not match the expected shape is counted as unreadable and shown in the header; it is never guessed at.
- Every path, name and error text from a host is sanitised (`sanitise_for_display`) before it is drawn, and never used to build a command.
- Default grouping depth 2 below the dataset root, changeable 1 to 4 with `[` and `]`.
- Works on FreeBSD, Linux, illumos. FreeBSD and OmniOS verified 2026-10-20 (`zfs diff -FHt` output, `-s createtxg`, `-o written`, diff against the live dataset); Linux is verified in Task 6.
- Rust only, no new dependencies; `cargo clippy --all-targets` warnings must not exceed 174 (the baseline); the book's tests stay green (every Help key is in the keys reference; new terms are in the glossary).

## Review Focus

1. A very large diff: reading stops at the line limit or the time limit, the view says so, memory stays bounded (entries capped), the UI never freezes, Esc cancels and kills the child.
2. Paths with spaces, `\040` octal escapes, tabs, newlines, ESC and bidi characters: decoded, then sanitised for display; a rename line has a fifth field.
3. Zoned or delegated datasets (OmniOS): diff paths come through the zone root (`/rpool/zones/syncthing/root/omni30/syncthing/...`) while the mountpoint is `/omni30/syncthing`: shown relative to the dataset; if the mountpoint is `legacy`, `none` or not found in the path, the raw path is shown and the header says "paths as the host prints them".
4. The oldest snapshot has no previous one: the view says so and offers `C`; it is not an error.
5. A diff that fails (snapshot destroyed meanwhile, dataset not mounted, no permission): the error text is shown sanitised; no panic; partial results are not presented as complete.
6. A second `c` while a read is running, and Esc while it runs: nothing starts twice; Esc kills the child and returns to the list.
7. The dataset's own root directory appears as a change line (`M / /scratch/data/`): grouped under the dataset root, not dropped.

---

## File Structure

- `src/zfs/snapshot.rs`: `Snapshot.written`, list args (Task 1).
- `src/zfs/snapshot_diff.rs` (new): parse one diff line, decode paths, strip the mountpoint (Task 2).
- `src/zfs/snapshot_changes.rs` (new): `ChangeSummary` (pure folding and grouping), the commands, the streaming reader (Tasks 3 and 4).
- `src/ui/changes.rs` (new): the view's state and drawing (Task 5).
- `src/ui/state.rs`, `src/ui/state/worker.rs`, `src/ui/layout.rs`, `src/ui/hints.rs`, `src/help.rs`: wiring (Task 5).
- `docs/book/10-snapshots.md`, `20-reference-keys.md`, `23-glossary.md`, `docs/book/captures/10-changes.txt` (Task 6).

---

### Task 1: The Written column

**Files:** Modify `src/zfs/snapshot.rs`, `src/ui/layout.rs` (`draw_snapshots`), and every `Snapshot { .. }` literal (add `written: 0`). Test: `snapshot.rs` tests, a layout render test.

**Interfaces:** Produces `Snapshot { dataset, name, used, holds, written: u64 }`; `SNAPSHOT_LIST_ARGS` ends `"name,used,userrefs,written"`.

- [ ] **Step 1: Failing tests** in `snapshot.rs`: `parses_the_written_column` (`"tank/data@a\t10\t0\t4096\n"` gives `written: 4096`); `a_dash_for_written_is_zero` (`"tank/data@a\t10\t0\t-\n"` gives 0); `rejects_a_line_with_three_fields`; `list_snapshots_asks_for_written` (args end `name,used,userrefs,written`). In the layout tests: `the_snapshot_list_has_a_written_column` (a snapshot with `written: 2048` renders `2.0K` under a `Written` header).
- [ ] **Step 2: Run** `cargo test snapshot` — Expected: FAIL (compile errors).
- [ ] **Step 3: Implement** the field, the four-field parse (`-` maps to 0 via `parse_numeric_or_dash`), the args, and the `Written` column after `Used` (width like `SIZE_COL_W`; the existing `Hold` column keeps working). Update the existing 3-field fixtures to 4 fields (about 15 tests across `snapshot.rs`, `daemon/snapshot.rs`, `ui/state/tests.rs`; the compiler and failing tests list them).
- [ ] **Step 4: Run** `cargo test` — Expected: all pass; clippy count unchanged.
- [ ] **Step 5: Commit** `feat: Written column (bytes since the previous snapshot)`.

### Task 2: Parsing a diff line

**Files:** Create `src/zfs/snapshot_diff.rs`; modify `src/zfs/mod.rs` (declare it). Test: in the new file.

**Interfaces:** Produces
`pub enum Change { Added, Removed, Modified, Renamed }` with `pub fn word(self) -> &'static str` (`added`, `removed`, `modified`, `renamed`);
`pub enum FileKind { File, Directory, Symlink, Socket, Door, Pipe, BlockDevice, CharDevice, EventPort, Other }` with `pub fn word(self) -> &'static str` (`file`, `directory`, `symlink`, `socket`, `door`, `pipe`, `block device`, `character device`, `event port`, `other`);
`pub struct DiffEntry { pub changed_at: u64, pub change: Change, pub kind: FileKind, pub path: String, pub renamed_to: Option<String> }`;
`pub fn parse_diff_line(line: &str) -> Option<DiffEntry>`; `pub fn decode_zfs_path(s: &str) -> String`; `pub fn relative_to_mount(path: &str, mountpoint: &str) -> Option<String>`.

- [ ] **Step 1: Failing tests** (assertions exact):
  - `parses_an_added_file`: `"1788654605.572189000\t+\tF\t/scratch/data/test.txt"` gives `changed_at 1788654605, Added, File, path "/scratch/data/test.txt", renamed_to None`.
  - `parses_modified_removed_and_a_directory`: `M` with `/` is `Modified, Directory`; `-` is `Removed`.
  - `a_rename_line_has_a_fifth_field`: `"1788654605.5\tR\tF\t/a/old\t/a/new"` gives `Renamed`, path `/a/old`, `renamed_to Some("/a/new")`.
  - `every_type_letter_maps`: `F / @ = > | B C P` give File, Directory, Symlink, Socket, Door, Pipe, BlockDevice, CharDevice, EventPort; an unknown letter gives `Other`.
  - `garbage_is_none`: empty line, three fields, a non-numeric time, change letter `X`, a rename with only four fields: all `None`.
  - `octal_escapes_are_decoded`: `decode_zfs_path("/a/b\\040c")` is `"/a/b c"`; `"\\134"` is a backslash; `"\\011"` a tab; a lone trailing backslash or `\\9zz` is kept as written; multi-byte UTF-8 given as escaped bytes (`\\303\\251`) decodes to `é`.
  - `relative_to_mount_strips_a_plain_prefix`: `("/scratch/data/test.txt", "/scratch/data")` is `Some("test.txt")`; `("/scratch/data/", "/scratch/data")` is `Some("")` (the dataset root); `("/scratch/database/x", "/scratch/data")` is `None` (no false prefix match).
  - `relative_to_mount_finds_the_mountpoint_behind_a_zone_root`: `("/rpool/zones/syncthing/root/omni30/syncthing/state/x", "/omni30/syncthing")` is `Some("state/x")`; mountpoint `legacy`, `none`, `-` or empty gives `None`.
- [ ] **Step 2: Run** `cargo test snapshot_diff` — Expected: FAIL (module missing).
- [ ] **Step 3: Implement** the three functions and types. `parse_diff_line`: split on tab; fields `time change type path [new path]`; time is seconds before the first `.`; paths go through `decode_zfs_path`. `relative_to_mount`: when the path equals or starts with `mountpoint` followed by `/` or end, strip it; otherwise search for `mountpoint` preceded by `/`-boundary and followed by `/` or end (the zone-root case, first match); trailing and leading `/` trimmed from the result.
- [ ] **Step 4: Run** `cargo test` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: parse zfs diff lines (types, change letters, octal escapes, zone-root paths)`.

### Task 3: Folding entries into a summary

**Files:** Create `src/zfs/snapshot_changes.rs` (summary part); declare in `src/zfs/mod.rs`. Test: in the file.

**Interfaces:** Consumes `DiffEntry`, `relative_to_mount`. Produces
`pub struct Limits { pub max_lines: u64, pub max_seconds: u64, pub max_entries: usize }` with `impl Default` (2_000_000, 120, 100_000);
`pub struct Counts { pub added: u64, pub removed: u64, pub modified: u64, pub renamed: u64 }` with `pub fn total(&self) -> u64`;
`pub enum Stop { Lines, Time, Cancelled }`;
`pub struct ChangeSummary` with `pub fn new(limits: &Limits, mountpoint: Option<String>) -> Self`, `pub fn add(&mut self, entry: DiffEntry)`, `pub fn add_unreadable(&mut self)`, `pub fn totals(&self) -> Counts`, `pub fn unreadable(&self) -> u64`, `pub fn time_range(&self) -> Option<(u64, u64)>`, `pub fn stopped(&self) -> Option<Stop>`, `pub fn set_stopped(&mut self, s: Stop)`, `pub fn lines(&self) -> u64`, `pub fn paths_relative(&self) -> bool` (false when any path could not be made relative), `pub fn groups(&self, depth: usize) -> Vec<(String, Counts)>` (sorted by `total()` descending, ties by name), `pub fn entries_in(&self, group: &str, depth: usize, limit: usize) -> (Vec<&Row>, usize)` (the rows and how many more exist), `pub fn entries_dropped(&self) -> u64`; `pub struct Row { pub entry: DiffEntry, pub shown_path: String }`.

- [ ] **Step 1: Failing tests**:
  - `counts_each_kind_and_the_total`: add 3 added, 2 removed, 4 modified, 1 renamed: `totals()` matches, `total() == 10`.
  - `groups_by_the_folder_at_the_depth_asked_for`: entries `/ds/a/b/c/f1`, `/ds/a/b/c/f2`, `/ds/a/b/g`, `/ds/a/h`, `/ds/top` with mountpoint `/ds`: at depth 1 groups are `a/` (4) and `/` (1); at depth 2 `a/b/` (3), `a/` (1), `/` (1); at depth 4 `a/b/c/` (2), `a/b/` (1), `a/` (1), `/` (1). (A folder is the entry's parent directory cut to the depth, shown with a trailing `/`; a root-level entry is `/`.)
  - `the_dataset_root_directory_line_counts_under_slash`: `M / /ds/` with mountpoint `/ds` lands in group `/`.
  - `zone_root_paths_group_by_their_dataset_relative_folder`: path `/rpool/zones/z/root/ds/state/x` with mountpoint `/ds` is group `state/`.
  - `a_path_that_cannot_be_made_relative_keeps_the_raw_path_and_flags_it`: no mountpoint: group names are raw parent folders (cut to depth) and `paths_relative()` is false.
  - `entries_are_capped_but_counts_are_not`: `max_entries = 3`, add 5: `entries_dropped() == 2`, `totals().total() == 5`.
  - `entries_in_returns_the_rows_of_one_group_and_how_many_more`: 10 entries in `a/`, limit 4 gives 4 rows and 6 more; rows are sorted newest change first.
  - `time_range_is_min_and_max_of_the_change_times`; `unreadable_lines_are_counted`.
- [ ] **Step 2: Run** `cargo test snapshot_changes` — Expected: FAIL.
- [ ] **Step 3: Implement.** Keep a `BTreeMap<String, Counts>` of folders at the maximum depth 4 and derive shallower depths by truncating keys (counts are additive), so a depth change never re-reads the diff; keep `Vec<Row>` up to `max_entries`. `shown_path` is the relative path (or the raw path when it cannot be made relative).
- [ ] **Step 4: Run** `cargo test` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: fold diff entries into a bounded, depth-groupable summary`.

### Task 4: The commands and the streaming reader

**Files:** Modify `src/zfs/snapshot_changes.rs`. Test: in the file, using a small fake runner whose `command()` returns `sh -c '<script>'` (follow the `send_script` fake in `src/daemon/replicate.rs` tests).

**Interfaces:** Consumes Tasks 2-3, `crate::daemon::transfer::run_lines(cmd, cancel: &AtomicBool, on_line)`, `crate::zfs::not_an_option`. Produces `pub fn previous_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<Option<String>>` (full name of the snapshot created just before `dataset@name`, by `createtxg`: `zfs list -H -t snapshot -d 1 -o name -s createtxg <dataset>`); `pub fn dataset_mountpoint(runner, dataset) -> Result<Option<String>>` (`zfs get -H -o value mountpoint <dataset>`; `legacy`, `none`, `-` give `None`); `pub fn read_changes(runner: &dyn CommandRunner, older: &str, newer: &str, mountpoint: Option<String>, limits: &Limits, cancel: &AtomicBool) -> Result<ChangeSummary>` (`older` is a full snapshot name, `newer` a full snapshot name or a dataset name).

- [ ] **Step 1: Failing tests**:
  - `previous_snapshot_is_the_one_before_by_creation_order`: listing `tank/d@a\ntank/d@b\ntank/d@c` gives `Some("tank/d@a")` for `b`, `None` for `a`, and an error for a name not in the list; the recorded call is `zfs list -H -t snapshot -d 1 -o name -s createtxg tank/d`.
  - `a_dataset_name_starting_with_a_dash_is_refused` for both functions and `read_changes` (no command runs).
  - `mountpoint_words_that_are_not_paths_become_none` (`legacy`, `none`, `-`).
  - `read_changes_folds_a_streamed_diff`: the fake streams 5 valid lines and 1 garbage line: totals, `unreadable() == 1`, groups right; the recorded command is `zfs diff -FHt -- <older> <newer>`.
  - `the_line_limit_stops_the_read_and_says_so`: `max_lines = 3`, the fake streams 10: `stopped() == Some(Stop::Lines)`, `lines() <= 4` (a few more may already be in flight), the child is killed (the fake script would otherwise run to completion: make it `sleep` after the first lines and assert the call returns well before the sleep ends).
  - `the_time_limit_stops_the_read`: `max_seconds = 1`, the fake sleeps 30 s after one line: returns within about 5 s with `Stop::Time`.
  - `cancel_stops_the_read`: set the flag from another thread: `Stop::Cancelled`, partial summary returned.
  - `a_failing_diff_is_an_error_with_the_first_stderr_line`: exit 1 with `cannot open 'x': dataset does not exist` gives `Err(CommandFailed)` containing that text; no summary.
  - `a_runner_that_cannot_hand_out_a_command_is_an_error`.
- [ ] **Step 2: Run** `cargo test snapshot_changes` — Expected: FAIL.
- [ ] **Step 3: Implement.** `read_changes` builds the command with `runner.command("zfs", ...)`, runs it with `run_lines` and an internal `stop` flag; the `on_line` callback parses, adds, counts lines and sets `stop` at the line limit; a watcher thread sets `stop` when the deadline passes or the caller's `cancel` is set (record which, as `Stop`). A non-zero exit that was not caused by our own stop is an error.
- [ ] **Step 4: Run** `cargo test` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: previous snapshot, mountpoint and a bounded, cancellable diff reader`.

### Task 5: The changes view

**Files:** Create `src/ui/changes.rs` (view state, rows, drawing helpers); modify `src/ui/state.rs` (field `changes_view: Option<ChangesView>`, `c`/`C` keys, view keys), `src/ui/state/worker.rs` (new `WorkerMsg::Changes(usize, u64, Result<ChangeSummary>)`: host, request id, result), `src/ui/layout.rs` (draw the view in place of the snapshot table), `src/ui/hints.rs`, `src/help.rs`, `docs/book/20-reference-keys.md` (the key rows, for book_check parity). Test: `src/ui/changes.rs`, `src/ui/state/tests.rs`, layout and hints tests.

**Interfaces:** Consumes Tasks 1-4, slice 1's `MutationCall::HoldSnapshot`, `sanitise_for_display`, `truncate_with_ellipsis`. Produces `ChangesView` with a `Phase { Reading, NoPrevious, Failed(String), Ready(ChangeSummary) }`, `depth: usize` (default 2), `selected: usize`, `open_group: Option<String>`, `cancel: Arc<AtomicBool>`, `request: u64`, `older: String`, `newer: String` and a title.

- [ ] **Step 1: Failing tests** (state tests use `FakeRunner`/the fake of Task 4 where a spawn is needed; follow how the other worker-message tests inject results):
  - `c_on_a_snapshot_with_a_previous_one_opens_a_reading_view_and_asks_the_worker` and `c_on_the_oldest_snapshot_opens_a_view_that_says_there_is_no_previous_one_and_offers_capital_c`.
  - `capital_c_compares_with_the_live_dataset` (the view's `newer` is the dataset name).
  - `c_with_nothing_selected_does_nothing`; `c_while_a_read_is_running_starts_no_second_one`.
  - `esc_while_reading_sets_the_cancel_flag_and_closes_the_view`; `esc_in_a_group_list_returns_to_the_groups_then_closes`.
  - `a_result_for_an_older_request_is_ignored` (request id); `a_result_for_another_host_is_ignored`.
  - `up_down_select_groups_and_enter_lists_entries_of_the_group`; `brackets_change_the_depth_between_1_and_4_and_clamp`.
  - `capital_h_in_the_view_holds_the_older_snapshot_with_a_y_n_confirm` (opens `Modal::Confirm` with `HoldSnapshot` for the OLDER snapshot) and `capital_h_does_nothing_when_there_is_no_older_snapshot`.
  - `a_failed_diff_shows_the_error_sanitised` (an error text with ESC characters shows none) and `a_path_with_control_characters_is_drawn_sanitised`.
  - `the_header_reports_counts_the_time_range_unreadable_lines_and_a_stop` (render test: contains `2,505 removed`-style counts with thousands separators, `stopped after`, `unreadable`) and `paths_as_the_host_prints_them_note_appears_when_paths_are_not_relative`.
  - `hints_in_the_view_offer_enter_brackets_hold_and_esc`; the Help text lists `c`, `C`; book_check parity (the keys reference has the rows).
- [ ] **Step 2: Run** `cargo test ui::` — Expected: FAIL.
- [ ] **Step 3: Implement.** `c`/`C` run on a worker thread: `previous_snapshot` (for `c`), `dataset_mountpoint`, then `read_changes` with the default `Limits` and the view's `cancel` flag, sending `WorkerMsg::Changes`. A request counter makes stale results harmless. While `Reading` the view shows `reading changes... (Esc cancels)`. The groups table shows folder, `+added -removed ~modified` counts; the entry list shows change (word), type (word), changed time (`YYYY-MM-DD HH:MM:SS`, hub local time via `chrono::Local`), and the sanitised path, at most 5,000 per group with `... N more`. All host text goes through `sanitise_for_display`. Key routing: a new chunk before the others so the view's keys win while it is open; `q`, `?`, `A` and `!` keep working as everywhere.
- [ ] **Step 4: Run** `cargo test` — Expected: all pass; clippy unchanged.
- [ ] **Step 5: Commit** `feat: the changes view (c and C in Snapshots)`.

### Task 6: Book and live checks

**Files:** `docs/book/10-snapshots.md` ("What changed between snapshots"), `docs/book/20-reference-keys.md`, `docs/book/23-glossary.md` (`changes view` only if a new term is needed), `docs/book/captures/10-changes.txt` and its embedding.

- [ ] **Step 1 (implementer): book text** for the view: what it shows, the keys, the limits and what "stopped after N changes" means, that it only reads, that paths on a delegated dataset are shown relative to it, and that the older snapshot can be held from the view. Beginner voice, existing callout labels only; no captures yet. Run `cargo test book_check`.
- [ ] **Step 2 (controller, with the user's yes): live checks.** On the hub scratch pool create a throwaway snapshot, change files (add, modify, rename, delete, a name with spaces, a symlink), take a second snapshot, then run the view with `c` and `C` through the capture tool from the build tree. On Ubuntu `ztest/data` and OmniOS `tank/photos` run the exact `zfs diff -FHt --` and `zfs list ... -s createtxg` forms by hand (Linux output is not yet verified). Never on `omni30`. Remove every throwaway snapshot and file afterwards.
- [ ] **Step 3 (controller): capture** `10-changes` with the capture tool (it scrubs real names), embed it in chapter 10, run `cargo test book_check`.
- [ ] **Step 4: Commit** `docs: what changed between snapshots (chapter 10, keys, capture)`.

## Self-Review notes

- Spec slice 2 coverage: `Written` column (Task 1), `c` previous / `C` live (Task 5), header counts and folder groups with depth `[`/`]` (Tasks 3 and 5), entry list with change, type in words and change time (Tasks 2 and 5), hold the older snapshot from the view (Task 5), limits and cancel (Tasks 3-5), unreadable lines (Tasks 2-4), nothing written to disk (no persistence anywhere), live verification including untried Linux (Task 6).
- Two calls of mine to check: entry storage is capped at 100,000 in memory (spec lists 5,000 per group shown and 2,000,000 lines read); `previous_snapshot` orders by `createtxg`, not by name.
- Not in this plan: browse and restore (slices 3 and 4), and the deferred slice 1 items.
