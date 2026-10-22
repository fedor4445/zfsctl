# Snapshot restore as a copy (slice 4 of snapshot recovery) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (native) or superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax. Part A and Part B each end in something that can be pushed and merged on its own.

**Goal:** `R` on a regular file in the snapshot browser copies that file out of the snapshot to a new path next to the original (`photo.jpg` becomes `photo.restored-<snapshot>.jpg`, editable), after a two-key confirm that shows the exact command. It never overwrites, never restores a folder, and checks the result.

**Architecture:** Part A replaces slice 3's raw `ls` listing forms with escaped, one-line-per-entry forms (`ls -b` on FreeBSD and illumos, `ls -Q` on GNU), because a restore may only trust a listing in which a crafted file name cannot forge extra lines. Part B adds an off-by-default sudoers feature `restore` (one pinned `cp` per OS), a pure planning module that validates the source and destination, a worker that checks, copies and verifies, and the `R` key with its confirm.

**Tech Stack:** Rust, ratatui; the slice 3 modules (`snapshot_browse`, `ui/browse`), `Modal::Form` and `Modal::Confirm`, `MutationCall`, `FakeRunner`-style test runners.

**Spec:** `docs/superpowers/specs/2026-10-20-snapshot-recovery-design.md`, slice 4 and the "Hard rules for slice 4" paragraph. **Builds on:** slices 1 to 3 and 3b (merged into `main`), and the `browse-polish` branch (name filter).

## Decisions made by the user (2026-10-21)

1. **Default destination:** the file's own folder with a suffix (`photo.jpg` becomes `photo.restored-<snapshot>.jpg`), editable. (Not a recovery folder, so no `mkdir` privilege.)
2. **Browse forms:** the raw `ls` forms are **replaced** by escaped ones. Hosts re-install the `browse` sudoers entry once (the hub needs nothing).
3. **Privilege:** a `restore` sudoers feature with a root `cp` is accepted, **off by default**, with its limits stated in the printed file, README and book.

## Global Constraints

- Data safety first. The restore never overwrites, never deletes, never touches a folder, never follows a symlink for the source, and never writes anywhere but one new file.
- **Hard rules from the spec:** (1) listings used by a restore are one line per entry (Part A); (2) the source path has no symlink in it (every step was listed as a real directory, the last as a regular file); (3) a name with a control character or U+FFFD is never sent; (4) a dataset given to a zone is **refused** for restore ("do it inside the zone"): never read or write through `<zone path>/root`.
- Every `cp` and `ls` argument list ends `-- ` and operands; options first (illumos getopt). Argument lists are built from the registry's pinned forms (`pinned_forms`), so what is sent and what sudoers allows cannot drift.
- The confirm shows the exact command, the source path and the destination, and needs two keys (`y`/`n`, then `Enter`).
- Plain `cargo build --release` after every task; `cargo test` green (2,379 + 7 at the start); clippy at most 177 warnings.
- Rust only, no new dependencies. Hints, Help, keys reference, glossary, sudoers chapters and README updated.

## Verified facts (probed on 2026-10-21 on the real hosts, scratch files under /tmp only)

- **Escaped one-line listings.**
  - FreeBSD `ls -lAb -D %s`: newline `\n`, tab `\t`, quote `\"`, **backslash `\\`**, non-ASCII byte `\351` (octal), space raw; a symlink prints `name -> target` with the target escaped the same way. Unambiguous.
  - GNU `ls -lAQ --time-style +%s`: every name (and a symlink target) in double quotes, with `\"`, `\\`, `\n`, `\t` and octal `\351`; spaces raw inside the quotes. Unambiguous. (GNU `-b` would also escape spaces, so `-Q` is used.)
  - illumos `ls -lAbE`: newline `\012`, tab `\011`, non-ASCII `\351`, space raw, **but a backslash is printed single and quotes unescaped**, so `back\slash` and a literal `\351` look alike. Rule: on illumos a name containing a backslash is shown but never opened or restored.
- **No-clobber copy.** FreeBSD `cp -n` leaves an existing file alone and exits 1. GNU `cp --update=none` (9.4) and illumos native `/usr/bin/cp -p -n` leave it alone and exit **0** (silent). So the exit status cannot say "skipped": the plan checks that the destination is absent before, and checks the result after. Native illumos `cp -P` is not valid for a plain file copy; the source is a regular file from an immutable snapshot, so `-P` is not needed.
- `cp -p` as root keeps mode, owner, group and times. `ls -lA <path-to-a-file>` prints that one file's line (and errors for a missing path), so the existing `ls` forms also serve the existence check and the result check.
- Zone-delegated datasets are read through `<zone path>/root` (slice 3b) and are therefore refused here.

## Review Focus

- A file or folder name with a newline, a forged `-rw-r--r--` line, a quote, a backslash, a non-UTF-8 byte, a leading `-`: the listing stays one line per entry, the name is shown escaped or cleaned, and nothing unsafe is sent back.
- A source path that passes through a symlink (planted in the dataset before the snapshot): refused, never copied.
- A live destination folder replaced by a symlink after the snapshot (the classic root-writes-elsewhere trap): refused, because every folder of the destination path is checked to be a real directory right before the copy. The remaining race (a local user swapping a folder between the check and the copy) is documented, not hidden.
- A destination that already exists (a race, or a name chosen by the user): the copy does not overwrite, and the result check says `not copied` instead of claiming success.
- A zoned dataset, a dataset not mounted, a file that vanished, a file that is not regular (symlink, device, socket, folder): refused with a plain message before any command runs.
- The `restore` sudoers entry not installed on a remote host: a message naming `print-sudoers HOST --with restore`; the hub (root, no sudo) works without it.

---

## Part A: one line per entry

### Task 1: Escaped browse forms in the registry

**Files:** Modify `src/exec/privilege.rs` (`BROWSE_FORMS`, `pinned_forms`), `src/zfs/snapshot_browse.rs` (`ls_args` tests), `README.md`, `docs/book/05-remote-hosts.md`, `docs/book/10-snapshots.md`, `docs/book/22-reference-commands.md`. Test in the same files.

**Interfaces:** Produces the forms, in the order modified, changed, created, each ending `-- *`:
FreeBSD `-lAb -D %s`, `-lAbc -D %s`, `-lAbU -D %s`; Linux `-lAQ --time-style +%s`, `-lAQ --time-style +%s --time ctime`, `-lAQ --time-style +%s --time birth`; illumos `-lAbE`, `-lAbEc`. `pinned_forms(entry, os)` and `ls_args` keep their signatures.

- [ ] **Step 1: Write the failing tests:** `the_ls_forms_are_the_escaped_ones` (exact lists per OS; every form has a `b` (BSD/illumos) or `Q` (GNU) in its option word; none has `-R -L -H -e -d`); the existing `ls_args_match_the_pinned_forms_exactly` and the README gap test updated; `print_sudoers_with_browse_*` expects the new text.
- [ ] **Step 2: Run** `cargo test privilege sudoers snapshot_browse` — Expected: FAIL.
- [ ] **Step 3: Implement** the new `BROWSE_FORMS`; update the docs' form lists (README paragraph, book table row).
- [ ] **Step 4: Run** `cargo test` and `cargo build --release` — Expected: pass.
- [ ] **Step 5: Commit** `feat: browse lists with escaped, one-line-per-entry ls forms`.

### Task 2: Decode escaped names (`src/zfs/snapshot_browse.rs`)

**Files:** Modify `src/zfs/snapshot_browse.rs`; add fixtures `src/zfs/snapshot_browse_fixtures/{freebsd,linux,illumos}_escaped.txt` from the probes above (backslash, quote, tab, newline, non-ASCII byte, space, symlink with a spaced target).

**Interfaces:** Produces `decode_name(os: HostOs, text: &str) -> Decoded` where `Decoded { text: String, ambiguous: bool }`: BSD and illumos decode `\\ \" \a \b \f \n \r \t \v \ooo`; GNU first strips one pair of double quotes then decodes the same escapes (a quote inside is `\"`); bytes from octal escapes are collected and converted lossily (so a non-UTF-8 name contains U+FFFD and is never opened); `ambiguous` is true for an illumos name that contains a backslash. `parse_ls_line` uses it for the name and for a symlink target (split at the first ` -> ` outside the quotes for GNU). `ListEntry` gains `ambiguous: bool` (false elsewhere).

- [ ] **Step 1: Write the failing tests:** one per fixture line (exact decoded names: `back\slash`, `quo"te`, `tab` + TAB + `tab`, `nl` + LF + `x`, `caf` + U+FFFD, `sp ace`, symlink `lnk` to `target with space`); `a_forged_entry_inside_a_name_stays_one_entry` (the three hosts' escaped form of a name holding a newline and a fake `-rw-r--r-- 1 u g 5 100 evil` line parses to ONE entry whose name contains the newline, and `valid_component` rejects it); `illumos_backslash_names_are_ambiguous` (true) and `bsd_backslash_names_are_not` (false); `gnu_quotes_with_arrows_inside_do_not_confuse_the_link_split` (`"a -> b"` is a name, `"x" -> "y"` is a link).
- [ ] **Step 2: Run** `cargo test snapshot_browse` — Expected: FAIL.
- [ ] **Step 3: Implement** with no panics (checked indexing, `from_utf8_lossy`).
- [ ] **Step 4: Run** `cargo test` and `cargo build --release` — Expected: pass.
- [ ] **Step 5: Commit** `feat: decode the escaped names of ls -b and ls -Q`.

### Task 3: The browser uses them

**Files:** Modify `src/zfs/snapshot_browse.rs` (`list_once`: a line that does not parse is now counted, the continuation-gluing is removed because it cannot occur; keep the unit test that proves a raw continuation line is counted, not glued), `src/ui/browse.rs` (`ambiguous` names are shown and never opened: `enter_selected` refuses with `unusual name, not opened (a backslash is ambiguous on this host)`), `src/ui/hints.rs` (no `Enter Open` for them), `docs/book/10-snapshots.md` (the honesty paragraph about crafted names is replaced: listings are one line per entry now; the remaining limit is the zone root).

- [ ] **Step 1: Write the failing tests:** `list_once_counts_a_stray_line_instead_of_gluing_it`, `enter_on_an_ambiguous_name_is_refused`, `open_is_not_offered_on_an_ambiguous_name`, the existing newline-glue test replaced by `a_name_with_a_newline_arrives_escaped_as_one_entry` using a script that prints the escaped form.
- [ ] **Step 2: Run** `cargo test` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test`, `cargo build --release`, clippy — Expected: pass, clippy at most 177.
- [ ] **Step 5: Commit** `feat: the browser trusts one line per entry`.
- [ ] **Step 6 (live, read-only):** build on the hub; the user re-installs the `browse` sudoers entry on `ub24` and `omnios` (`print-sudoers HOST --with browse`); browse a throwaway tree there that holds a newline name, a quote, a backslash and a non-UTF-8 name, and look at the screens. Record what each OS shows.

---

## Part B: restore

### Task 4: The `restore` feature and the pinned `cp`

**Files:** Modify `src/exec/privilege.rs` (`Feature::Restore`, off by default on every OS and available on all; entry `cp` privileged with `RESTORE_FORMS`, `pinned_forms` extended), `src/exec/sudoers.rs` (comment line), README, book chapters 5 and 22. Test in the same files.

**Interfaces:** Produces `Feature::Restore` (name `restore`) and entry `Entry { name: "cp", features: &[Feature::Restore], kind: Privileged { pinned: RESTORE_FORMS, probe: "" } }` with the forms, by OS: FreeBSD `-p -n -- * *`; Linux `-p --update=none -- * *`; illumos `-p -n -- * *`. `pinned_forms(cp, os)` returns that OS's single form.

- [ ] **Step 1: Write the failing tests:** `restore_is_a_known_feature_off_by_default_on_every_os`; `the_cp_entry_is_privileged_and_its_forms_never_allow_overwriting` (no `-f`, `-i`, `-r`, `-R`, `-a`, `-l`, `-s`, `-u` word in any form, each form has the no-clobber word, each ends `-- * *`); `print_sudoers_with_restore_lists_only_the_hosts_cp_form_and_says_what_it_allows` (the comment states plainly: "cp as root: with this entry the account can copy any file the host can read to any new path, which zfs and zpool already allow; sudo cannot confine the paths"); the README gap test and book drift test updated.
- [ ] **Step 2: Run** `cargo test` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test` and `cargo build --release` — Expected: pass, clippy at most 177.
- [ ] **Step 5: Commit** `feat: the restore sudoers feature (a pinned cp that never overwrites)`.

### Task 5: The restore plan (`src/zfs/snapshot_restore.rs`, pure)

**Files:** Create `src/zfs/snapshot_restore.rs`; modify `src/zfs/mod.rs`. Test in the same file.

**Interfaces:**
- Produces:
  - `pub struct RestoreSource { pub dataset: String, pub snapshot: String, pub mountpoint: String, pub folder: Vec<String>, pub entry: ListEntry, pub zone: Option<String>, pub os: HostOs }`
  - `pub struct RestorePlan { pub source: String, pub destination: String, pub dest_folder: String, pub dest_name: String, pub command: Vec<String>, pub size: u64, pub mode: String }`
  - `pub fn default_destination_name(file: &str, snapshot: &str) -> String` (`photo.jpg` and `daily-1` give `photo.restored-daily-1.jpg`; no extension gives `name.restored-daily-1`; a leading-dot file keeps its dot; spaces in the snapshot name become `_`; the result is cut to 255 bytes at a character boundary, keeping the extension).
  - `pub fn plan_restore(src: &RestoreSource, dest_name: &str) -> Result<RestorePlan>`: refuses with a plain message when `zone.is_some()` ("restore from a dataset given to a zone must be done inside the zone"), when the entry is not `EntryKind::File`, is ambiguous, or its name or `dest_name` fails `valid_component` (so also control characters and U+FFFD), when any folder step is invalid, or when `dest_name` equals the source name; builds `source = snapshot_dir(mountpoint, snapshot, folder + name)`, `destination = <mountpoint>/<folder>/<dest_name>` (the **live** path: no `.zfs`), and `command` from `pinned_forms(cp, os)` minus the trailing `*` `*`, plus the two operands.
- Consumes: `snapshot_dir`, `valid_component`, `pinned_forms`, `ListEntry`.

- [ ] **Step 1: Write the failing tests:** the default-name table above; each refusal; `the_command_is_the_pinned_form_with_the_two_paths` for the three OSes (exact lists); `the_destination_is_in_the_live_tree_not_in_dot_zfs`; `a_destination_name_with_a_slash_or_dotdot_is_refused`.
- [ ] **Step 2: Run** `cargo test snapshot_restore` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test` and `cargo build --release` — Expected: pass.
- [ ] **Step 5: Commit** `feat: plan a restore as a copy (pure validation and the exact command)`.

### Task 6: Check, copy, verify (`src/zfs/snapshot_restore.rs`)

**Files:** Modify `src/zfs/snapshot_restore.rs`. Test with an `sh -c` fake runner like slice 3's.

**Interfaces:** Produces `pub fn run_restore(runner: &dyn CommandRunner, plan: &RestorePlan, src: &RestoreSource, cancel: &AtomicBool) -> Result<Restored>` with `Restored { destination: String, size: u64, mode: String, owner: String }`. Steps, each an error with a clear message when it fails and each using the existing escaped `ls` forms through `read_folder`:
1. **Source:** list the source's parent folder (inside the snapshot) and require the entry to be there as a regular file of the planned size (`the file in the snapshot changed` is impossible for a snapshot, but a missing one is reported).
2. **Destination chain:** for each step of the live destination folder from the mountpoint down, list the parent and require the step to be an entry of kind Directory (a symlink or a missing step is refused: `a folder on the way to the destination is a symlink, or is gone`); list the destination folder and require `dest_name` to be absent.
3. **Copy:** `runner.run("cp", args)`; a failure is an error with the first stderr line (sudo refusal becomes `restore is not enabled for this host: run zfsctl print-sudoers HOST --with restore ...`).
4. **Verify:** list the destination folder; require `dest_name` to be present, a regular file, of the planned size and mode; otherwise report `not copied` (this is what catches a silent no-clobber skip) or `copied but size/mode differ`. Return the owner, size and mode.
`cancel` is checked between the steps; the copy itself is not interrupted.

- [ ] **Step 1: Write the failing tests:** `a_restore_runs_the_checks_then_cp_then_verifies` (the exact calls in order); `a_symlink_in_the_destination_chain_is_refused_and_cp_never_runs`; `an_existing_destination_is_refused_and_cp_never_runs`; `a_silent_skip_is_reported_as_not_copied` (cp exits 0 but the destination is absent after); `a_size_mismatch_is_reported`; `sudo_refusal_names_the_restore_feature`; `a_missing_source_is_reported`; `cancel_between_steps_stops_before_cp`.
- [ ] **Step 2: Run** `cargo test snapshot_restore` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test` and `cargo build --release` — Expected: pass.
- [ ] **Step 5: Commit** `feat: check, copy and verify one restored file`.

### Task 7: The `R` key

**Files:** Modify `src/ui/modal.rs` (`MutationCall::RestoreFile { plan, source, host_name }`, its title `RESTORE AS A COPY`, success text `Restored <destination> (<size>)`, `run` calls `run_restore`; never offered as a "recursive" or "force" anything), `src/ui/browse.rs` (selected row access for `R`), `src/ui/state.rs` (`R` in the browser: only on a regular file, only when the browser is Ready; opens a `Modal::Form` with one field, `Restore as`, prefilled with `default_destination_name`; on submit builds the plan and opens `Modal::Confirm` whose prompt shows the source, the destination and the exact command; a refusal from `plan_restore` is shown as an error and nothing opens), `src/ui/hints.rs` (`R Restore` only on a regular file), `src/ui/state.rs` Help text, `docs/book/20-reference-keys.md`. Test in `src/ui/state/tests.rs`, `src/ui/hints.rs`, `src/ui/modal.rs`.

- [ ] **Step 1: Write the failing tests:** `r_on_a_regular_file_opens_the_destination_form_with_the_default_name`; `r_on_a_folder_a_symlink_or_an_ambiguous_name_does_nothing`; `submitting_the_form_opens_a_confirm_that_shows_source_destination_and_command`; `the_confirm_needs_two_keys` (y then Enter; Enter alone does nothing); `a_refused_plan_shows_the_reason_and_opens_no_confirm` (zoned, bad name); `r_is_not_offered_in_the_hints_unless_it_works` (hints invariant fingerprint extended); `the_changes_view_has_no_r` (restore from the changes list is a later decision; the key is swallowed there).
- [ ] **Step 2: Run** `cargo test` — Expected: FAIL.
- [ ] **Step 3: Implement.** After a successful restore, the browser re-reads its folder so the new file appears.
- [ ] **Step 4: Run** `cargo test`, `cargo build --release`, clippy — Expected: pass, clippy at most 177.
- [ ] **Step 5: Commit** `feat: R restores a file from a snapshot as a copy`.

### Task 8: Book, sudoers chapters and captures

**Files:** Modify `docs/book/10-snapshots.md` (a section "Getting a file back": the key, the default name, the confirm, what is checked, what is refused and why, the honest limits: the root `cp` entry, the swap race, zone datasets refused, restore into a synced folder will sync), `docs/book/05-remote-hosts.md`, `docs/book/18-safety.md` ("a restore never overwrites"), `docs/book/19-troubleshooting.md`, `docs/book/23-glossary.md`, `docs/book/20-reference-keys.md`, README. Create `docs/book/captures/10-restore-form.txt` and `10-restore-confirm.txt` taken with `examples/capture.rs` on the hub.

- [ ] **Step 1:** write the prose; run `cargo test` — Expected: the drift tests need the `cp` row and `--with restore` text, then pass.
- [ ] **Step 2:** take the captures on the hub with throwaway files in `scratch/data` (removed afterwards); real names must not appear.
- [ ] **Step 3: Run** `cargo test` and `cargo build --release` — Expected: pass.
- [ ] **Step 4: Commit** `docs: getting a file back from a snapshot; the restore sudoers feature`.

### Task 9: Live checks (ask before touching hosts other than the hub)

- [ ] **Hub (FreeBSD, root, no sudo):** in `scratch/data` make a throwaway tree (a photo-like file, a name with spaces, a folder, a symlink folder), snapshot it as `zt-rst-a`, change the live file, then `R` the snapshot copy: check the default name, the confirm text, the copy, the size/mode/owner/time against the original, then press `R` again for the same name: it must be refused (exists) and the existing copy untouched. Replace a live folder by a symlink to `/tmp/zt-elsewhere` and confirm the restore into it is refused and nothing lands in `/tmp`. Remove all throwaway files, the snapshot and the temporary symlink target afterwards and show the hub is back to its snapshots.
- [ ] **`ub24` and `omnios`:** after the user installs `print-sudoers HOST --with restore` (and the new `--with browse`) as root, repeat the happy path, the exists-refusal and the symlink-folder refusal with throwaway data (`ztest/data`, `tank/photos`, never `omni30`); the user removes the test trees.
- [ ] **`omni30/syncthing`:** only if the user asks, and then only the refusal: the zoned dataset must say "restore from a dataset given to a zone must be done inside the zone" and run nothing.
- [ ] Record each result in the ledger and the final message; anything not run is said plainly.

---

## Self-review

- **Spec coverage:** restore one file as a copy with an editable default name (Task 5, 7); never overwrites (Tasks 4 to 6); two-key confirm with the exact command (Task 7); post-copy size and mode check (Task 6); `restore` sudoers feature, off by default, with a pinned copy that preserves times and mode and refuses to overwrite (Task 4); the four hard rules (Task 2/3 for rule 1, Task 5/6 for rules 2 to 4).
- **Deviation from the spec text to ledger when executing:** the spec says the confirm checks the result "against the source"; the plan checks size and mode against the snapshot listing (a content hash would need reading both files as root and is not done; stated in the book).
- **Type consistency:** `ListEntry.ambiguous` (Task 2) is read in Tasks 3, 5 and 7; `RestorePlan.command` (Task 5) is what Task 7's confirm prints and Task 6 sends; `pinned_forms` (Task 1 and 4) is the single source of argument lists.
- **Out of scope:** folder restore, restoring from the changes list, restoring over an existing file (never), restoring through a zone root (refused), and a search across a whole snapshot.
