# Zone-aware snapshot browse (slice 3b) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (chosen) or superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Let the snapshot browser (slice 3) open a snapshot of a dataset that is given to an illumos zone (`zoned=on`), such as `omni30/syncthing`, by reading it through the zone's root directory, with no new sudoers power.

**Architecture:** A `zoned=on` dataset has no `.zfs` folder in the global zone, but `<zonepath>/root<mountpoint>/.zfs/snapshot/<name>` is listable by root from the global zone (verified on OmniOS on 2026-10-21: all eight `omni30/syncthing` snapshots listed through `/rpool/zones/syncthing/root/omni30/syncthing/.zfs/snapshot`). The zone that owns the dataset is found with two unprivileged commands: `zoneadm list -cp` (zone names, states, zone paths) and `zonecfg -z NAME info dataset` (the delegated dataset names). The existing pinned `ls` forms already reach that path.

**Tech Stack:** Rust; the slice 3 code in `src/zfs/snapshot_browse.rs`, `src/ui/browse.rs`, `src/ui/state.rs`.

**Spec:** `docs/superpowers/specs/2026-10-20-snapshot-recovery-design.md` (slice 3; the zone case the spec said to report as "cannot be browsed here" is now resolved, never by cloning). **Builds on:** `docs/superpowers/plans/2026-10-21-snapshot-browse.md` (same branch).

## Global Constraints

- Security first: only read-only commands; `zoneadm` and `zonecfg` are registered **Unprivileged** (they run without sudo; both worked for the unprivileged `omni` account) under `Feature::Browse`; no new sudoers line.
- A zone name and a zone path come from the host: validate them before they reach a command or a path (zone name: ASCII letters, digits, `_ . -`, 1 to 64 bytes, not starting with `-`; zone path: absolute, every step passes `valid_component`).
- The zone must be `running` and must be the **only** zone whose delegated datasets include the dataset (equal to it, or an ancestor of it). Zero or several matches are refused with a message, never guessed.
- The header says which zone the folders were read through.
- `cargo build --release` plain, `cargo test` green (2,335 + 7 at the start), clippy at or below 177.

## Review Focus

- A zone whose name or path contains odd characters, a path with `..`, or a `zonecfg` answer that names a similar dataset (`omni30/syncthing2`, `omni30/sync`): exact or ancestor-by-slash match only.
- Two zones that both delegate the dataset (or an ancestor): refuse.
- A stopped zone: refuse with a clear message (its datasets are not mounted in its root).
- A non-illumos host or `zoned=off`: unchanged behaviour.
- The cached base path of a browsing session is reused for folders below, not re-resolved, and never contains the zone's content, only the validated prefix.

---

### Task 1: Find the zone (`src/zfs/snapshot_browse.rs`)

**Files:** Modify `src/exec/privilege.rs` (two Unprivileged entries `zoneadm`, `zonecfg` under `Feature::Browse`), `src/zfs/snapshot_browse.rs`. Test in the same files.

**Interfaces:**
- Produces:
  - `pub struct BrowseBase { pub path: String, pub zone: Option<String> }`
  - `pub fn zone_for_dataset(runner: &dyn CommandRunner, dataset: &str) -> Result<(String, String)>`: `(zone name, zone path)`; runs `zoneadm list -cp` then `zonecfg -z NAME info dataset` for each running zone.
  - `pub fn browse_mountpoint(runner, os, dataset) -> Result<BrowseBase>` replaces the slice 3 version: unchanged for everything except `zoned=on` on illumos, which now resolves the zone and returns `path = "<zonepath>/root<mountpoint>"`, `zone = Some(name)`.
- Consumes: `valid_component`, the slice 3 `GetRunner`-style test runner (extended to answer by command name).

- [ ] **Step 1: Write the failing tests:**
  - `zoneadm_output_is_parsed_and_only_running_zones_are_candidates`.
  - `the_zone_that_delegates_the_dataset_is_found` (`name: omni30/syncthing` in `syncthing`'s answer; path `/rpool/zones/syncthing`).
  - `a_dataset_below_a_delegated_one_is_found_but_a_lookalike_name_is_not` (`omni30/syncthing/Camera` matches `omni30/syncthing`; `omni30/syncthing2` and `omni30/sync` do not).
  - `two_zones_delegating_the_dataset_is_refused`, `no_zone_delegating_it_is_refused_with_a_message`, `a_stopped_zone_is_refused_with_its_name`.
  - `zone_names_and_paths_from_the_host_are_validated` (a name `-x`, `a b`, `a;b`; a path `/x/../y`, relative, with a newline: refused, nothing run for that zone).
  - `browse_mountpoint_for_a_zoned_dataset_returns_the_zone_root_path` (the exact commands run: `zoneadm list -cp`, `zonecfg -z syncthing info dataset`, options first) and `a_zoned_dataset_on_a_non_illumos_host_is_still_refused` (the slice 3 behaviour).
  - privilege: `zoneadm` and `zonecfg` are registered Unprivileged for `Browse`, and the drift/README tests stay green.
- [ ] **Step 2: Run** `cargo test snapshot_browse privilege` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test` and `cargo build --release` — Expected: pass; clippy at most 177.
- [ ] **Step 5: Commit** `feat: find the zone that holds a zoned dataset (zoneadm, zonecfg)`.

### Task 2: Use it in the view, the book and the live check

**Files:** Modify `src/ui/browse.rs` (field `zone: Option<String>` on `BrowseView` and `PendingBrowse`, shown in the header as `(zone NAME)`), `src/ui/state.rs` (`start_browse` uses `BrowseBase`, `BrowseOutcome` carries `zone`), `docs/book/10-snapshots.md`, `docs/book/19-troubleshooting.md`, `docs/book/05-remote-hosts.md` (the two unprivileged commands). Test in `src/ui/browse.rs`, `src/ui/state/tests.rs`.

- [ ] **Step 1: Write the failing tests:** `the_header_names_the_zone_a_folder_was_read_through`; `the_zone_is_kept_when_a_folder_is_opened_and_reused_without_asking_again` (the next pending read carries the cached base and zone); `a_non_zone_browse_shows_no_zone_text`.
- [ ] **Step 2: Run** `cargo test` — Expected: FAIL.
- [ ] **Step 3: Implement,** then update the book (the zone case is no longer "cannot be browsed"; say how it is read and that the zone must be running).
- [ ] **Step 4: Run** `cargo test`, `cargo build --release`, clippy — Expected: pass, clippy at most 177.
- [ ] **Step 5: Commit** `feat: browse a snapshot of a dataset given to a zone`.
- [ ] **Step 6 (live, read-only):** build on the hub, open `omni30/syncthing@keep-20261020-before-recovery` from the TUI and look at `Camera`; only keys that navigate, nothing that writes. Record what the user sees (counts of folders and files).
