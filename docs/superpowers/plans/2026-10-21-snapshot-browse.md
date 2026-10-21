# Snapshot browse (slice 3 of snapshot recovery) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** From the Snapshots section, `f` opens a read-only folder browser of one snapshot: each entry shows type, size, modified, mode and owner in the list and created, changed, group and link target for the selected entry; entries can be filtered by name, modified date, size and type. Metadata only, never file contents, never following a symlink.

**Architecture:** A new sudoers feature `browse` (off by default) registers one privileged command, `ls`, with pinned argument forms that all end in `-- *` (so no option can be added after them). A pure module builds the directory path (`<mountpoint>/.zfs/snapshot/<snapshot>/<relative>`) after validating every part, builds the per-OS `ls` argument lists, and parses `ls -l` lines; a bounded, cancellable reader runs two or three `ls` calls per folder (modified; changed; created where the OS records it) and joins them by line; a browse view sits in place of the Snapshots list exactly as the slice 2 changes view does.

**Tech Stack:** Rust, ratatui, chrono (already a dependency), `CommandRunner::command` + `daemon::transfer::run_lines` (as `read_changes` does), the `FakeRunner` / `DiffRunner`-style test seams.

**Spec:** `docs/superpowers/specs/2026-10-20-snapshot-recovery-design.md` (slice 3 and Security). **Stacked on slices 1 and 2:** this branch is based on `snapshot-timeline` (unmerged at the time of writing). It reuses `dataset_mountpoint`, `sanitise_for_display`, `truncate_with_ellipsis`, `run_lines`, the changes view's key-routing, stale-result and host-switch patterns (`clear_host_data`).

## Global Constraints

- Security and data retention first: nothing in this slice writes, deletes, moves, opens or reads the bytes of a file. The only new privileged command is `ls`, behind the off-by-default `browse` feature.
- Every `ls` argument list ends with `--` and then exactly one path. Options first, always (illumos getopt).
- Every path is validated in zfsctl before it is sent: relative, no `..`, no empty or `.` component, no NUL, no newline (a name with a control character is shown but never opened), at most 4096 bytes. The snapshot name part follows ZFS name rules (`A-Za-z0-9_.:- ` and space, 1 to 255 bytes, no `/`, no `@`, not `.` or `..`).
- Names that come back from a host are displayed (after `sanitise_for_display`), never executed or interpolated into a command except through the validator above.
- Symlinks are shown as symlinks and never followed (`ls -l` without `-L`/`-H`).
- A browse can never hang the TUI or fill the hub's memory: at most 20,000 entries per folder, 30 seconds, cancellable with `Esc` (the child is killed).
- Plain `cargo build --release` must succeed after every task (not only `cargo test` and clippy). Clippy must stay at or below the baseline of 177 warnings. `cargo test` must stay green (2,276 unit + 7 example tests at the start).
- Rust only, no new dependencies. Same look as the rest of the program; hint bar and Help text updated; keys reference, glossary and the sudoers chapters in the book updated.

## Verified facts (checked on the real hosts on 2026-10-21)

- All three OSes accept `ls -lA ... -- <dir>`. `ls` of an unreadable or missing directory exits 1 (FreeBSD) or 2 (Linux, illumos) with the reason on stderr (`ls: /nonexistent: No such file or directory`, `/root: Permission denied`).
- Time columns, name-sorted in every form (the time option changes only what is printed, not the order):
  - **FreeBSD** `ls -lA -D %s -- DIR` prints epoch seconds; `-lAc` ctime, `-lAU` birth time (verified different values). Example: `drwxr-xr-x  2 root wheel        2 1784933440 X11` and `lrwxr-xr-x  1 root wheel       12 1790619566 aliases -> mail/aliases`. Pinned `ls` is `/bin/ls`.
  - **Linux** (GNU coreutils 9.4) `ls -lA --time-style=+%s -- DIR` prints epoch seconds; add `--time=ctime` or `--time=birth` (birth verified). Example: `lrwxrwxrwx 1 root root  23 1770683684 arptables -> /usr/sbin/arptables-nft`. Pinned `ls` is `/usr/bin/ls`. (Column widths differ: the size field is not right-aligned the same way, so split on runs of whitespace.)
  - **illumos** (OmniOS) `ls -lAE -- DIR` prints `2026-10-20 05:25:30.508633786 -0400` (date, time with nanoseconds, UTC offset); `-lAEc` is ctime. **No birth time**: `ls -/ c|v` shows attribute flags only. Pinned `ls` is `/usr/bin/ls`. Example: `lrwxrwxrwx   1 root     root          22 2026-10-20 05:25:35.059896862 -0400 .syslog_door -> ../var/run/syslog_door`.
- First character of the mode field is the type: `-` file, `d` directory, `l` symlink, `b` block, `c` character, `p` pipe, `s` socket, `D` door, `P` event port. Block and character devices print `major, minor` in place of the size.
- **A ZFS directory's size column is its number of entries plus 2** (hub `snap4` holds three files and shows `5`; the `.zfs/snapshot` directory with four snapshots shows `6`; an empty one shows `2`): a folder's entry count is `size - 2`, with no extra listing.
- `.zfs/snapshot/<name>` exists for a mounted dataset in the global zone on all three OSes (hub `/scratch/data`, OmniOS `/tank/photos/.zfs/snapshot`, Ubuntu `/ztest/data/.zfs/snapshot`). It does **not** exist for an OmniOS dataset delegated to a zone, as slice 2 found (the diff showed zone-root paths): that case is reported as "this snapshot cannot be browsed here", never worked around by cloning.
- `ls` of a large directory sorts before it prints, so a cap of N lines is the first N names.
- sudoers matches the command line with `*` wildcards that also match spaces and `/`, so the pinned path cannot be confined to `.zfs/snapshot`. The `--` before the path keeps options out; what remains is the metadata of any path. The account already holds `zfs` and `zpool` in full (which can read every byte of every dataset), so the entry is least privilege in spirit rather than a new boundary; the printed sudoers file says so, as it does for `smartctl`.

## Review Focus

- A directory with 100,000+ entries: the list is capped at 20,000 with a visible "first 20,000 of a larger folder" note, the UI stays responsive, `Esc` kills `ls`.
- Hostile names from the host: newline, ESC sequences, a leading `-`, backslashes, ` -> ` inside a name, a name with spaces: displayed safely, parsed without panicking, and a name with a control character is never sent back in a command.
- `browse` not enabled on a remote host (`sudo: a password is required`, or a refusal): a message that names `print-sudoers HOST --with browse`, not a raw error; the same host's other sections keep working.
- The zone-delegated OmniOS dataset and an unmounted or `legacy`/`none` mountpoint: a clear "cannot be browsed here" (and why), no command run against a guessed path.
- A symlink that points outside the snapshot (`-> /etc`, `-> ../../..`): shown, never entered, and Enter on it does nothing.
- Host switch, section change or a second `f` while a folder is being read: the read is cancelled and a late answer is dropped (the slice 2 stuck-"reading" bug must not return).

---

### Task 1: The `browse` feature and the pinned `ls` entry

**Files:**
- Modify: `src/exec/privilege.rs` (Feature::Browse, ALL, name, works_on, select/select_for_os defaults, `LS` entry kind, the full-list test string), `src/exec/sudoers.rs` (a comment line in `render` when `ls` is present, like smartctl's), `docs/book/22-reference-commands.md` and `docs/book/05-remote-hosts.md` (the drift test compares the book with `privileged_names()`; add the `ls` row and the `--with browse` option text).
- Test: the existing tests in those files plus new ones.

**Interfaces:**
- Produces: `Feature::Browse` (name `"browse"`, on every OS, off by default everywhere), registry entry `Entry { name: "ls", features: &[Feature::Browse], kind: Kind::Privileged { pinned: BROWSE_FORMS, probe: "" } }` where `BROWSE_FORMS` is exactly these eight argument lists, each ending `"--", "*"`: `["-lA","-D","%s"]`, `["-lAc","-D","%s"]`, `["-lAU","-D","%s"]` (FreeBSD); `["-lA","--time-style=+%s"]`, `["-lA","--time-style=+%s","--time=ctime"]`, `["-lA","--time-style=+%s","--time=birth"]` (Linux); `["-lAE"]`, `["-lAEc"]` (illumos).

- [ ] **Step 1: Write the failing tests** in `privilege.rs`/`sudoers.rs` test modules:
  - `browse_is_a_known_feature_and_off_by_default_on_every_os`: `select(&[], &[])` and `select_for_os` for FreeBSD, Linux and illumos do not contain `Browse`; with `with = ["browse"]` they do (Linux and illumos accept it; no "not available" error).
  - `the_ls_entry_is_privileged_and_only_pinned_forms_with_a_double_dash`: `lookup("ls")` is `Privileged` with 8 forms; every form's last two items are `--` and `*`; no form contains `-R`, `-L`, `-H`, `-e` or `-d`.
  - `print_sudoers_with_browse_lists_every_ls_form_and_without_it_lists_none`: render for FreeBSD with `--with browse` contains `/bin/ls -lA -D %s -- *`, `/bin/ls -lAU -D %s -- *` and the explanatory comment line; without the feature the text contains no `ls`.
  - the existing "full list" string test and the book drift test expect the new entry (update them as part of this task).
- [ ] **Step 2: Run** `cargo test privilege sudoers` — Expected: FAIL (no `Browse`, no `ls`).
- [ ] **Step 3: Implement** the feature and entry. `Feature::ALL` grows to 11; `select` excludes `Browse` by default like `Restic|Rclone|Smart|Beadm`; `works_on` is true for `Browse` on every OS. In `render`, when an `ls` entry is present add: `# ls: the -- * forms cannot stop an extra path (sudo's * also matches spaces and /); zfsctl only reads .zfs/snapshot folders with them, and this account already holds zfs and zpool.`
- [ ] **Step 4: Run** `cargo test` and `cargo build --release` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: the browse sudoers feature (a pinned, read-only ls)`.

### Task 2: Paths and command lines (`src/zfs/snapshot_browse.rs`, part 1)

**Files:**
- Create: `src/zfs/snapshot_browse.rs`; modify `src/zfs/mod.rs` (declare it).
- Test: in the same file.

**Interfaces:**
- Consumes: `HostOs` (from `zfs::host_health`), `super::not_an_option`.
- Produces:
  - `pub fn valid_component(name: &str) -> bool` and `pub fn join_relative(parts: &[&str]) -> Option<String>` (None when any part is invalid, e.g. `..`, `.`, empty, contains `/`, NUL, newline or any other control character, or the joined result exceeds 4096 bytes).
  - `pub fn valid_snapshot_name(name: &str) -> bool` (the ZFS rule in Global Constraints).
  - `pub fn snapshot_dir(mountpoint: &str, snapshot: &str, relative: &[&str]) -> Result<String>`: `Err(AppError::Config(..))` unless the mountpoint is absolute (starts with `/`, not `/` alone is allowed? no: reject `/` and reject `none`, `legacy`, `-` with the message `not mounted`), the snapshot name and every component are valid; result `"<mountpoint>/.zfs/snapshot/<snapshot>[/<a>/<b>]"` with no doubled slash.
  - `pub enum Times { Modified, Changed, Created }` and `pub fn times_for(os: HostOs) -> &'static [Times]` (illumos: Modified, Changed; others all three).
  - `pub fn ls_args(os: HostOs, times: Times, dir: &str) -> Option<Vec<String>>`: the exact forms from the pinned list plus `dir` last, preceded by `--`; `None` for `Created` on illumos. `HostOs::Other` uses the FreeBSD forms (as the sudoers defaults do).

- [ ] **Step 1: Write the failing tests:**
  - `rejects_hostile_components`: `""`, `"."`, `".."`, `"a/b"`, `"a\nb"`, `"a\0b"`, `"a\x1b[2J"`, a 300-byte name is accepted only if it is under the 255-byte component limit (reject above), and a 5,000-byte joined path is `None`.
  - `accepts_ordinary_names`: `"photo 1.jpg"`, `"-leading-dash"`, `"naïve"`, `".hidden"`, `"a b c"`.
  - `snapshot_names_follow_zfs_rules`: `"daily-2026-10-20"`, `"keep 1"`, `"a:b"` ok; `""`, `"a/b"`, `"a@b"`, `".."`, `"x\ny"` rejected.
  - `snapshot_dir_builds_the_path_and_refuses_the_rest`: `("/scratch/data", "snap4", &["sub","x y"])` gives `/scratch/data/.zfs/snapshot/snap4/sub/x y`; mountpoint `/` , `none`, `legacy`, `-`, `relative/path`, `/a/../b` are errors (the last because `..` as a mountpoint component is refused).
  - `ls_args_match_the_pinned_forms_exactly`: for each OS and each `Times`, `ls_args` equals the pinned list entry plus `dir` and every produced list begins with one of the eight pinned prefixes from the registry (read from `privilege::lookup("ls")`, so the two cannot drift); `ls_args(Illumos, Created, ..)` is `None`.
- [ ] **Step 2: Run** `cargo test snapshot_browse` — Expected: FAIL (module missing).
- [ ] **Step 3: Implement** the functions above. Component limits: 1 to 255 bytes, no control characters (`char::is_control`), no `/`, not `.` or `..`.
- [ ] **Step 4: Run** `cargo test` and `cargo build --release` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: validated snapshot paths and the per-OS ls command lines`.

### Task 3: Parsing `ls -l` lines (`src/zfs/snapshot_browse.rs`, part 2)

**Files:**
- Modify: `src/zfs/snapshot_browse.rs`; add real output as fixtures under `src/zfs/snapshot_browse_fixtures/` (`freebsd.txt`, `linux.txt`, `illumos.txt`, copied from the verified outputs: header `total N` line, a directory, a regular file, a symlink with ` -> `, a name with spaces if available; the illumos fixture uses the `-E` format).
- Test: in the same file.

**Interfaces:**
- Produces:
  - `pub enum EntryKind { File, Directory, Symlink, Block, Character, Pipe, Socket, Door, Port, Other }` with `word(self) -> &'static str` (`file`, `folder`, `symlink`, ...).
  - `pub struct ListEntry { pub name: String, pub kind: EntryKind, pub mode: String, pub owner: String, pub group: String, pub size: u64, pub time: Option<i64>, pub link_target: Option<String> }` (`time` is UTC epoch seconds, `size` is the byte count of a file or the raw ZFS size of a directory).
  - `pub fn parse_ls_line(os: HostOs, line: &str) -> Option<ListEntry>`: `None` for the `total` line, blank lines and anything that does not parse. Split the mode, link count, owner, group, size and time fields on runs of whitespace; the time is 1 token (epoch) on FreeBSD/Linux and 3 tokens (date, time, offset) on illumos, converted with chrono to UTC epoch seconds; the rest of the line is the name, split once at the first ` -> ` for a symlink. Block/character lines (`major, minor`) take size 0.
  - `impl ListEntry { pub fn folder_entries(&self) -> Option<u64> }`: `Some(size.saturating_sub(2))` for a directory, else `None`.

- [ ] **Step 1: Write the failing tests:** one per fixture asserting the first directory, the first file (size, owner, group, time as an exact epoch number) and the symlink (`name`, `link_target`); `a_name_with_spaces_and_an_arrow_in_it` (`"-rw-r--r-- 1 u g 5 1700000000 a -> b -> c"` as a file is a symlink only if the mode starts with `l`, otherwise the whole text is the name); `illumos_offsets_become_utc` (`2026-10-20 05:25:30.508633786 -0400` is `1792488330`); `garbage_and_total_lines_are_none` (empty, `total 541`, `drwx`, a line with a non-numeric size); `a_device_line_has_no_size_and_does_not_panic`; `a_hostile_name_is_kept_as_text` (ESC bytes survive parsing; drawing sanitises them, covered in Task 6); `folder_entries_is_size_minus_two` (5 gives 3, 2 gives 0, 0 gives 0, a file gives None).
- [ ] **Step 2: Run** `cargo test snapshot_browse` — Expected: FAIL.
- [ ] **Step 3: Implement** with no panics (use `get`, `splitn`-style helpers, `checked` arithmetic).
- [ ] **Step 4: Run** `cargo test` and `cargo build --release` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: parse ls -l output from FreeBSD, Linux and illumos`.

### Task 4: The bounded, cancellable folder reader (`src/zfs/snapshot_browse.rs`, part 3)

**Files:**
- Modify: `src/zfs/snapshot_browse.rs`, `src/error.rs` only if a new variant is unavoidable (prefer `AppError::CommandFailed`/`Config` with a clear text).
- Test: in the same file, with a runner that spawns `sh -c` scripts (copy the pattern of `DiffRunner` in `snapshot_changes.rs`).

**Interfaces:**
- Consumes: Tasks 2 and 3; `runner.command("ls", &args)`; `run_lines`.
- Produces:
  - `pub struct BrowseLimits { pub max_entries: usize, pub max_seconds: u64 }`, default 20,000 and 30.
  - `pub struct Listing { pub entries: Vec<Row>, pub truncated: bool, pub unreadable: usize, pub has_created: bool }` and `pub struct Row { pub entry: ListEntry, pub changed: Option<i64>, pub created: Option<i64> }` (modified is `entry.time`).
  - `pub fn read_folder(runner: &dyn CommandRunner, os: HostOs, dir: &str, limits: &BrowseLimits, cancel: &AtomicBool) -> Result<Listing>`: runs the Modified listing first (this defines the entries and decides success), then Changed and Created (when `times_for` has it) and zips them by line number, setting `changed`/`created` only when the entry names match; a mismatch or a short list leaves them `None`. All calls share the entry cap and the time budget. A failing first call returns `Err` carrying the first stderr line.
  - `pub fn explain(os_known: bool, snapshot_root_missing: bool, error_text: &str) -> String`: maps `a password is required`/`not allowed`/`sorry` to `browse is not enabled for this host: run zfsctl print-sudoers HOST --with browse, install the file and try again` (HOST is filled in by the caller), `No such file or directory` on the snapshot root to `this snapshot cannot be browsed here (its .zfs/snapshot folder is not visible: for example a dataset given to a zone); zfsctl does not clone to work around that`, and anything else to the sanitised first line.

- [ ] **Step 1: Write the failing tests:**
  - `read_folder_joins_modified_changed_and_created` (script prints the same three names for each call with different epochs; check the three times per row and the exact spawned argument lists, options first, ending `--` and the directory).
  - `illumos_has_no_created_time` (`has_created == false`, `created` is `None`, only two spawns).
  - `the_entry_cap_stops_the_read_and_says_so` (script prints 10 lines then sleeps 30; `max_entries = 3`: `truncated`, 3 entries, returns well under 10 s).
  - `the_time_limit_stops_the_read` and `cancel_stops_the_read_and_keeps_nothing_half_joined`.
  - `a_failing_first_call_is_an_error_with_the_first_stderr_line` (exit 2, `ls: /x: Permission denied`).
  - `a_mismatched_second_listing_leaves_changed_empty_not_wrong` (names differ on line 2).
  - `unparsable_lines_are_counted_not_shown`.
  - `explain_names_the_sudoers_command_when_sudo_refuses` and `explain_handles_the_missing_snapshot_folder`.
  - `no_command_is_built_from_an_invalid_directory`: `read_folder` is only ever given a string from `snapshot_dir`; a doc test is not enough, so assert the UI-facing constructor `snapshot_dir` is the sole producer by making `read_folder` re-check `dir` contains `/.zfs/snapshot/` and no `\n`/NUL and return `Err(Config)` otherwise.
- [ ] **Step 2: Run** `cargo test snapshot_browse` — Expected: FAIL.
- [ ] **Step 3: Implement** reusing the watcher-thread pattern of `read_changes` (stop flag, deadline, cancel). `ls` that exits non-zero because we killed it is not an error.
- [ ] **Step 4: Run** `cargo test` and `cargo build --release` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: a bounded, cancellable folder reader for snapshots`.

### Task 5: Filters and ordering (`src/zfs/snapshot_browse.rs`, part 4)

**Files:** Modify `src/zfs/snapshot_browse.rs`. Test: in the same file.

**Interfaces:**
- Produces:
  - `pub struct Filter { pub name: String, pub modified_from: Option<i64>, pub modified_to: Option<i64>, pub size_min: Option<u64>, pub size_max: Option<u64>, pub kind: KindFilter }` and `pub enum KindFilter { All, Files, Folders, Links }`, `Default` is no filtering.
  - `impl Filter { pub fn is_empty(&self) -> bool; pub fn matches(&self, row: &Row) -> bool }` (name is a case-insensitive substring; folders are never excluded by size or date, only by name and kind, so the user can still navigate).
  - `pub fn parse_size_range(text: &str) -> Option<(Option<u64>, Option<u64>)>`: `10M..`, `..1G`, `1K..1M`, `500` (exactly 500 bytes up to 500), units `K M G T` powers of 1024, optional `B`, case-insensitive; `None` for garbage or reversed ranges.
  - `pub fn parse_date_range(text: &str, utc_offset_secs: i32) -> Option<(Option<i64>, Option<i64>)>`: `2026-10-01..2026-10-20`, `2026-10-01..`, `..2026-10-20`, a single `2026-10-05` (that whole day); `to` is inclusive to the end of its day; local to the host's offset; `None` for garbage or reversed.
  - `pub fn sort_rows(rows: &mut [Row])`: folders first, then name, case-insensitive then bytewise.
- [ ] **Step 1: Write the failing tests** for each function including: `1k..1m` equals `1K..1M`, `..` alone is `None`, `5G..1G` is `None`, `2026-02-30` is `None`, a one-day range includes 23:59:59 and excludes the next second, a size filter keeps folders, kind filter `Links` keeps only symlinks, a name filter is case-insensitive and treats `%`, `*` and `\` as plain characters.
- [ ] **Step 2: Run** `cargo test snapshot_browse` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test` and `cargo build --release` — Expected: all pass.
- [ ] **Step 5: Commit** `feat: name, date, size and type filters for the snapshot browser`.

### Task 6: The browse view

**Files:**
- Create: `src/ui/browse.rs` (view state: `BrowseView { request, host, dataset, snapshot, mountpoint, os, path: Vec<String>, phase, rows (filtered, sorted, cached), selected, filter, input mode, cancel flag }`, `draw`, helpers).
- Modify: `src/ui/state.rs` (field `browse_view`, `f` in the Snapshots chunk, `key_browse_view`, `WorkerMsg::Browse`, `start_browse`, `clear_host_data` cancels and drops the view), `src/ui/state/worker.rs` (`on_browse`: apply only when `request` and `host` match), `src/ui/mod.rs` (apply pending), `src/ui/layout.rs` (draw in place of the list; new layout test), `src/ui/hints.rs` (hints and the invariant test's fingerprint), Help text, `docs/book/20-reference-keys.md`.
- Test: `src/ui/browse.rs`, `src/ui/state/tests.rs`, `src/ui/layout.rs`.

**Interfaces:**
- Consumes: Tasks 2 to 5, `dataset_mountpoint`, the host's `HostOs` and UTC offset from the probe state (fall back to FreeBSD forms and offset 0 when unknown), `sanitise_for_display`, `truncate_with_ellipsis`.
- Produces: `f` on a selected snapshot starts the browser at the snapshot root; keys in the view: Up/Down move; `Enter` or Right opens a folder (a symlink or any other kind does nothing; a name that fails `valid_component` shows "unusual name, not opened"); Left or Backspace goes up one level (at the root it does nothing); `Esc` cancels a read or closes the view; `/` edits the name filter; `m` edits the modified-date range; `s` the size range; `t` cycles All/Files/Folders/Links; `x` clears all filters; invalid input keeps the prompt open with the reason. While the view is open `c`, `C`, `d`, `b`, `H`, `f` do nothing (the hidden list's keys cannot act). `q ? A ! Tab` work as everywhere.
- Drawing: a header with `dataset@snapshot  /path/inside`, filters in force, "first 20,000 of a larger folder" and "N entries could not be read" notes; columns `Type Size Modified Mode Owner Name` (a folder's size column shows `N items`); a detail line or panel for the selected row: `Created`, `Changed`, `Group`, `Link target`, with `not recorded on this OS` for created on illumos and `-` when unknown; everything host-supplied goes through `sanitise_for_display`.

- [ ] **Step 1: Write the failing tests:**
  - view tests: `ready_view_lists_folders_first_with_item_counts`; `enter_opens_a_folder_and_left_goes_back_to_the_same_row`; `enter_on_a_symlink_does_nothing`; `a_name_with_a_control_character_is_shown_sanitised_and_not_opened`; `filters_narrow_the_list_and_x_clears_them`; `bad_filter_text_keeps_the_prompt_and_says_why`; `the_list_notes_truncation_and_unreadable_entries`; `created_shows_not_recorded_on_illumos`.
  - state tests: `f_starts_a_read_with_the_snapshot_directory_and_the_pinned_listing_forms` (fake pending request carries `snapshot_dir(...)`); `f_without_a_mountpoint_says_not_mounted_and_runs_nothing`; `keys_d_b_c_C_H_f_do_nothing_inside_the_view` (modal stays `None`, no second request: this also closes the slice 2 deferred minor for the changes view's `d`/`b`); `a_late_answer_for_another_request_is_dropped`; `a_browse_view_does_not_survive_a_host_switch_and_its_read_is_cancelled` (the slice 2 bug, pinned for this view); `esc_cancels_a_running_read`.
  - hints/layout: the hints invariant test's `Fingerprint` gains `browse_view` and every advertised key does something; a layout test draws the view at 100x30 and 70x20 without panic and shows the header.
- [ ] **Step 2: Run** `cargo test` — Expected: FAIL (compile errors, then behaviour).
- [ ] **Step 3: Implement.** Use the changes view as the template for request ids, the cancel flag, the `Phase` enum and the key-routing chunk. Cache the filtered, sorted rows when the folder arrives or a filter changes (never recompute per frame; slice 2's Important finding). The worker calls `read_folder` with `snapshot_dir(mountpoint, snapshot, &path)`; an `Err` is mapped through `explain` with the host name filled in.
- [ ] **Step 4: Run** `cargo test`, `cargo build --release` and `cargo clippy --all-targets` (count must be at most 177) — Expected: all pass.
- [ ] **Step 5: Commit** `feat: browse a snapshot's folders (f) with filters and details`.

### Task 7: Book, sudoers chapters and captures

**Files:**
- Modify: `docs/book/10-snapshots.md` (a section "Looking inside a snapshot": what you see, the keys, the filters, "metadata only, never contents", why it needs the `browse` feature, what "cannot be browsed here" means; recapture the stale list screen with the Written column and the `c`/`C`/`f` hints, closing the slice 2 deferred minor), `docs/book/05-remote-hosts.md` and `docs/book/22-reference-commands.md` (the `ls` row, `--with browse`, the honest limit about the `*` wildcard), `docs/book/18-safety.md` (one sentence: browsing only lists names and metadata), `docs/book/20-reference-keys.md`, `docs/book/23-glossary.md` ("browse"), `docs/book/19-troubleshooting.md` ("browse is not enabled for this host").
- Create: `docs/book/captures/10-browse.txt`, `10-browse-details.txt`, refreshed `10-snapshots.txt`, taken with `examples/capture.rs` from the hub (throwaway files in `scratch/data` and a throwaway snapshot, removed afterwards).
- Test: the book's drift tests (`cargo test`) and the capture tool's own tests.

- [ ] **Step 1:** write the prose and run `cargo test` — Expected: the drift tests fail until the `ls` row and `--with browse` text exist, then pass.
- [ ] **Step 2:** take the captures on the hub (see Task 8 for the throwaway data); real names must not appear (the capture tool scrubs them).
- [ ] **Step 3: Run** `cargo test` and `cargo build --release` — Expected: pass.
- [ ] **Step 4: Commit** `docs: looking inside a snapshot; the browse sudoers feature; recaptured snapshot list`.

### Task 8: Live checks (no code; ask before touching a host that is not the hub)

- [ ] Hub (FreeBSD, root, no sudo): build with `git archive HEAD | ssh fb@172.16.10.90 'cd ~/zfsctl-browse && tar xf -'` (create the directory first) and `cargo build --release`; run the branch binary as root. On `scratch/data` make a throwaway tree (folders, a file with spaces, a symlink to `/etc`, a file with a newline in its name via `touch "$(printf 'a\nb')"`, a 30,000-file folder), snapshot it as `zt-brw-a`, open it with `f`, check every key, the item counts, the 20,000 cap and note, `Esc` during a read, the filters, and that the symlink cannot be entered. Remove all throwaway data and the snapshot afterwards and show the hub is back to `snap1 snap2 snap4 sphrase1`.
- [ ] Ubuntu `ub24` and OmniOS: with the user's go-ahead, install the printed sudoers file (`zfsctl print-sudoers ub24 --with browse`, `check-host ub24 --with browse`; the user installs it as root, never over ssh as root), then browse a throwaway snapshot of `ztest/data` and of `tank/photos` (never `omni30`). Confirm: Linux shows a created time, OmniOS shows "not recorded", and a missing `.zfs` folder gives the "cannot be browsed here" message. If the user declines to install the entry, record that these two OSes are verified only by the raw `ls` forms above.
- [ ] Remove the `browse` entry again if the user wants hosts back to their previous sudoers.

---

## Self-review

- **Spec coverage:** type/size/created/modified/changed/mode/owner (list + details; created is "not recorded" where the OS has none), folders show an entry count (size - 2, verified), `/` name filter and date, size, type filters (Task 5/6), `browse` sudoers feature off by default and metadata only (Task 1), `.zfs/snapshot` or "cannot be browsed here" without cloning (Tasks 2, 4), validated paths (Task 2), symlinks never followed (Tasks 3, 6), names displayed not executed (Task 6), hub-never-hangs limits (Task 4), book and sudoers chapters (Task 7).
- **Spec deviations (to be ledgered as rulings when executing):** the spec says each entry shows all seven facts; this plan shows five in the list and three more for the selected entry, because a wide table does not fit 80 columns. Date filtering is on the modified time only (the spec says "date range" without naming which). The spec's "single pinned listing helper per OS" is eight pinned `ls` forms in one registry entry, which print-sudoers renders for every OS (an unused FreeBSD form on a Linux host is inert); the registry has no per-OS pinning today.
- **Type consistency:** `Times`, `ListEntry`, `Row`, `Listing`, `BrowseLimits`, `Filter`, `KindFilter` are defined in Tasks 2 to 5 and used unchanged in Task 6; `snapshot_dir` is the only producer of directory strings and `read_folder` re-checks them.
- **Out of scope here (slice 4):** restoring a file. Slice 4 will need plain names only; this slice marks names with control characters as "not opened" so slice 4 can rely on `valid_component`.
