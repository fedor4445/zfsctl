# Shares Check and Manual Checks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The daily check also looks at NFS and Samba on every host and the Dashboard shows a red line for a stopped service that has shares; `c` in the Hosts section runs the disk and shares checks on the selected host on demand; the Dashboard shows a red notice while any check runs.

**Architecture:** A new `src/daemon/shares_check.rs` mirrors `smart_check.rs` (pure over `CommandRunner`, JSON file, per-host outcome). A small `src/daemon/checks.rs` holds the running marker (with heartbeat guard) and `check_one_host`, the function both the daemon's pass and the TUI's manual check call. The Dashboard reads `smart.json`, `shares.json` and the marker; the TUI never changes the Shares section.

**Tech Stack:** Rust, serde_json, chrono, ratatui; the existing `CommandRunner`, `HostInfo`, `WorkerMsg` plumbing.

**Spec:** `docs/superpowers/specs/2026-10-17-shares-check-and-manual-checks-design.md` (builds on `2026-10-17-disk-health-design.md`)

## Global Constraints

- Shares problem: NFS shares exist and the NFS server is stopped, or SMB shares exist and Samba (`smbd`) is stopped: red (`Level::Issue`), text `NFS: 2 shares but nfsd is not running` / `Samba: 1 share but smbd is not running` (singular `1 share`), then `   (checked 3h ago)`. A `smb_error` is yellow (`Level::Watch`), text `Samba: <error>`. One Dashboard line per host (problems joined with `; `), goto `Section::Shares`, target the host. Nothing for a host with no shares needing a stopped service.
- Three outcomes for a service check: exit 255 = host unreachable (Failed, keeps the old entry); a non-zero exit whose stderr starts with `sudo:` or says `not found` = not set up (silent); any other non-zero exit = stopped. Samba is "running" when stdout contains `smbd is running` (as `zfs::share::samba_available`), NFS when the exit is 0. Do not change `zfs/share.rs`.
- Success replaces a host's entry (also "no shares, all fine"); failure or silence keeps the previous entry with an `error` note; the disk and shares halves are stored separately, each as soon as done.
- Files in the state dir: `smart.json` (unchanged shape), `shares.json` (`{hosts: {name: {checked_at, nfs_running, samba_running, nfs_shares, smb_shares, smb_error?, error?}}, ran_at?}`), `checks-running.json` (`{by, since, beat, host?}`), all written atomically (temp file + rename); missing/torn/old shape = empty, never a crash.
- Marker: whoever runs writes it first, refreshes `beat` every 20 s, removes it on every exit path (guard with `Drop`), removal only if `by` and `since` are still its own; a marker whose `beat` is older than 3 minutes is stale and ignored. The TUI refuses `c` while a fresh marker exists (message `A check is already running (<who>, since HH:MM)`); the daemon skips its pass while a fresh marker exists and tries again at its next wake.
- Dashboard: a red line (`theme.err`) under the header while a fresh marker exists: `checking: daily disk and shares check since 03:30` (daemon) or `checking fbsd16: disks and shares since 14:02` (TUI); not counted in the verdict and not an attention item.
- "disk and shares check is N days old" (yellow, goto Jobs, only with the daemon running) uses the newest `checked_at` over both files; no data in either file = no line. (The text of the existing SMART line changes accordingly.)
- Config: `[daemon] smart_check` becomes `daily_check` (same values, default `03:30`), `smart_check` still accepted as an alias; the book's config reference and `documented_config_keys()` follow.
- Read-only everywhere; no new sudoers entries. Style: pure modules with fakes, comments say why, tests next to the code or in `src/ui/state/tests.rs`, clippy adds no warnings to new code.

## Review Focus

- A host whose sudo refuses `service` or `zfs get`: silent, never red (Task 1 test).
- Two writers (daemon pass and a manual check) on the same files: whole-entry merges, atomic rename, the later write wins, no torn file (Task 2 test).
- A crashed run leaves a marker: the red notice disappears after 3 minutes and `c` works again (Task 2 and 4 tests).
- A manual check on an unreachable host keeps the old red line with its error note (Task 4 test).
- `smart_check` in an existing config.toml still works after the rename (Task 2 test).

---

### Task 1: The shares check

**Files:**
- Create: `src/daemon/shares_check.rs` (declare in `src/daemon/mod.rs`)

**Interfaces:**
- Produces:
  - `pub struct SharesResult { pub checked_at: DateTime<Utc>, pub nfs_running: bool, pub samba_running: bool, pub nfs_shares: usize, pub smb_shares: usize, pub smb_error: Option<String>, pub error: Option<String> }` (serde, `Clone, PartialEq`).
  - `pub struct SharesFile { pub hosts: BTreeMap<String, SharesResult>, pub ran_at: Option<DateTime<Utc>> }` with `load(&Path) -> SharesFile`, `save(&self, &Path) -> io::Result<()>`, `newest_check() -> Option<DateTime<Utc>>`.
  - `pub enum Outcome { Silent, Checked(SharesResult), Failed(String) }` (same meaning as `smart_check::Outcome`; reuse that type if it can be shared without a cycle, otherwise define this one).
  - `pub fn check_host(runner: &dyn CommandRunner, now: DateTime<Utc>) -> Outcome`.
  - `pub struct ShareProblem { pub host: String, pub severity: crate::smart::Severity, pub text: String, pub checked_at: DateTime<Utc> }` and `pub fn problems(file: &SharesFile) -> Vec<ShareProblem>` (one per host, texts joined with `; `).

- [ ] **Step 1: Write the failing tests** (with a scripted runner like `smart_check`'s `Script`; routes by the first argument of `service`/`zfs`/`cat`): `a_running_server_with_shares_is_healthy_and_stored`, `a_stopped_nfsd_with_shares_is_red_and_the_text_counts_the_shares`, `a_stopped_samba_with_shares_is_red`, `stopped_services_without_shares_are_healthy`, `an_smb_error_is_yellow`, `sudo_refusal_and_command_not_found_are_silent` (stderr `sudo: a password is required`, `sudo: service: command not found`), `exit_255_is_failed`, `a_listing_that_sudo_refuses_is_silent`, `save_is_atomic_and_load_survives_missing_torn_or_empty_files`, `problems_joins_one_hosts_texts_and_leaves_healthy_hosts_out`.
- [ ] **Step 2: Run** `cargo test --offline shares_check`: FAIL.
- [ ] **Step 3: Implement** per the Global Constraints. Share counts come from `zfs::share::list_shares` (NFS) and `zfs::smb::list_managed` when Samba runs (as `list_shares_with_availability` does); a failure of either that carries sudo's refusal is silent, any other failure is Failed.
- [ ] **Step 4: Run** the new tests and `cargo test --offline`: PASS.
- [ ] **Step 5: Commit** `feat: the shares check`

### Task 2: One host's checks, the running marker, and the daily pass

**Files:**
- Create: `src/daemon/checks.rs` (declare in `src/daemon/mod.rs`)
- Modify: `src/daemon/mod.rs` (`smart_loop` becomes the checks loop), `src/daemon/smart_check.rs` (reuse where sensible), `src/config.rs` (`daily_check` with `smart_check` alias, `DaemonConfig.daily_check`, `daily_time()`), `docs/book/21-reference-config.md`, the `documented_config_keys()` list

**Interfaces:**
- Consumes: Task 1; `smart_check::{check_host, SmartFile, HostResult, HOST_BUDGET}`.
- Produces:
  - `pub struct Marker { pub by: String, pub since: DateTime<Utc>, pub beat: DateTime<Utc>, pub host: Option<String> }`; `pub fn read_marker(dir: &Path) -> Option<Marker>`; `pub fn fresh_marker(dir: &Path, now: DateTime<Utc>) -> Option<Marker>` (None when missing, torn or `beat` older than 3 minutes).
  - `pub struct RunGuard` with `RunGuard::start(dir: &Path, by: &str, host: Option<&str>) -> RunGuard` (writes the marker, starts a heartbeat thread refreshing `beat` every 20 s) and `Drop` (stops the thread, removes the marker only if it is still its own).
  - `pub struct HostChecks { pub disks: smart_check::Outcome, pub shares: shares_check::Outcome }` and `pub fn check_one_host(runner: &dyn CommandRunner, now: DateTime<Utc>, previous_disks: Option<&smart_check::HostResult>, previous_shares: Option<&SharesResult>) -> HostChecks`.
  - `pub fn merge_into(dir: &Path, host: &str, checks: &HostChecks, now: DateTime<Utc>)`: read-modify-write of the host's entries in `smart.json` and `shares.json` under the rules in the Global Constraints (success replaces; failure or silence keeps the previous entry with an `error` note; `ran_at` untouched).
  - `DaemonConfig::daily_time(&self) -> Result<Option<NaiveTime>, String>` replacing `smart_time`.

- [ ] **Step 1: Write the failing tests:** `a_fresh_marker_is_seen_and_a_stale_or_torn_one_is_not`, `the_guard_writes_the_marker_refreshes_it_and_removes_only_its_own`, `a_guard_dropped_after_another_run_took_over_leaves_the_others_marker`, `merge_replaces_on_success_and_keeps_with_a_note_on_failure_and_silence` (the removed-shares example: red entry, then a healthy success clears it), `merge_keeps_the_other_hosts_entries`, `two_merges_for_different_hosts_both_survive`, in `config.rs`: `daily_check_accepts_off_and_hh_mm_and_smart_check_is_still_read` (a config with `smart_check = "05:15"` gives 05:15; both keys present: `daily_check` wins).
- [ ] **Step 2: Run:** FAIL.
- [ ] **Step 3: Implement.** The daemon loop (rename `smart_loop` to `checks_loop`): when due and no fresh marker, take a `RunGuard` ("daemon"), run `check_one_host` for every host in parallel with the existing per-host deadline (`smart_check::run_all` generalised, or the same thread-and-`recv_timeout` pattern), `merge_into` each answer, set `ran_at` in both files, log `daily checks done: disks on N of M hosts, shares on N of M hosts, K problem(s)`. Config rename per the Global Constraints.
- [ ] **Step 4: Run** `cargo test --offline`: PASS (the book's config drift test too).
- [ ] **Step 5: Commit** `feat: the daily check covers shares, with a running marker`

### Task 3: The Dashboard shows it

**Files:**
- Modify: `src/ui/dashboard.rs` (`Inputs` gets `shares: &SharesFile` and `running: Option<&Marker>`; `Dashboard` gets `running: Option<String>`; share items; the stale line), `src/ui/dashboard_view.rs` (the red notice under the header), `src/ui/state.rs` (`App.shares: SharesFile`, `App.checks_marker: Option<Marker>`, loaded in `refresh_dashboard`; the Enter arm already selects the host for Shares lines if `Section::Shares` is added to it), `src/daemon/health_text.rs` and `src/daemon/monitor.rs` / `src/notify_cli.rs` (report lines for shares)
- Test: `src/ui/dashboard.rs`, `src/ui/dashboard_view.rs`, `src/ui/state/tests.rs`, `src/daemon/health_text.rs`

**Interfaces:**
- Consumes: Tasks 1 and 2.
- Produces: `fn share_items(inputs: &Inputs) -> Vec<Item>` (reachable hosts only, like `smart_items`); `health_text::share_lines(&SharesFile) -> Vec<String>`; `Dashboard.running: Option<String>` (the notice text, `None` when no fresh marker).

- [ ] **Step 1: Write the failing tests:** `a_stopped_nfsd_with_shares_is_one_red_line_that_goes_to_shares`, `a_smb_error_is_one_yellow_line`, `a_healthy_or_unreachable_host_has_no_share_line`, `the_last_good_share_result_stays_after_a_failed_check_with_its_real_age`, `the_stale_line_covers_both_files_and_a_missing_file_is_not_stale` (fresh smart.json + no shares.json: no line; both old: one line `disk and shares check is 4 days old`), `a_fresh_marker_shows_the_red_notice_and_is_not_counted_in_the_verdict`, `a_stale_marker_shows_nothing`, `enter_on_a_share_line_selects_the_host_in_shares` (state), `the_daily_report_lists_the_share_problems`, a view test that the notice line renders in the error style below the header.
- [ ] **Step 2: Run:** FAIL. **Step 3: Implement** to the signatures; texts per the Global Constraints. **Step 4: Run** `cargo test --offline`: PASS. **Step 5: Commit** `feat: shares problems and the running notice on the Dashboard`

### Task 4: `c` in Hosts: check this host now

**Files:**
- Modify: `src/ui/state.rs` (`WorkerMsg::HostChecked(String /*host*/, String /*status line*/)`, `App.check_running: bool`, `start_host_check`), `src/ui/state/worker.rs` (apply the message: set the status line, refresh the Dashboard data), `src/ui/hints.rs` (`c` Check disks and shares, in the Hosts section when a host is selected), `src/ui/mod.rs` (the event loop hands the selected host's runner to `start_host_check`), `docs/book/20-reference-keys.md` and the in-app Help text (`help_text`; the book drift tests demand both)
- Test: `src/ui/state/tests.rs`, `src/ui/hints.rs`

**Interfaces:**
- Consumes: Task 2 `fresh_marker`, `RunGuard`, `check_one_host`, `merge_into`.
- Produces: `App::start_host_check(&mut self, runner: Arc<dyn CommandRunner>)` (called when `c` is pressed in Hosts; spawns the thread; the thread takes a `RunGuard` with `by = "tui"` and the host name, runs `check_one_host` with the stored previous entries, `merge_into`, then sends `WorkerMsg::HostChecked`).

- [ ] **Step 1: Write the failing tests:** `c_in_hosts_starts_a_check_of_the_selected_host_and_says_so` (FakeRunner scripted for smartctl and service; after the worker message the status line names both halves), `c_is_refused_while_a_fresh_marker_exists_and_says_who_runs` (marker file written by "daemon"), `c_works_again_when_the_marker_is_stale`, `c_does_nothing_while_a_manual_check_runs`, `a_check_on_an_unreachable_host_keeps_the_old_entry_and_says_it_could_not_be_reached`, `the_hint_bar_offers_c_only_in_hosts_with_a_selected_host`, `c_is_inert_in_other_sections` (the unadvertised-keys guard test must stay green).
- [ ] **Step 2: Run:** FAIL. **Step 3: Implement.** Status texts per the spec (`Checking fbsd16...`, then `fbsd16: disks fine, shares: NFS stopped with 2 shares`, `fbsd16: disks fine, shares not reachable`, `fbsd16: nothing to check (no smartctl, no shares access)`). **Step 4: Run** `cargo test --offline`: PASS. **Step 5: Commit** `feat: c in Hosts checks disks and shares on the selected host`

### Task 5: Book, README and the live check

**Files:**
- Modify: `docs/book/06-the-screen.md` (Dashboard: the share lines, the red notice), `docs/book/07-hosts.md` (`c`), `docs/book/12-shares.md` (the Dashboard line), `docs/book/16-alerts.md`, `docs/book/21-reference-config.md` (already, Task 2), `README.md`

- [ ] **Step 1:** Update the prose; `cargo test --offline` (book drift tests) PASS.
- [ ] **Step 2: Live check** (build on the hub, run without installing first): stop `nfsd` on a VM that has an NFS share (ask the user for the root step if the VM needs it; never root SSH), press `c` on that host and confirm the red line and the red running notice during the check; start `nfsd` again, `c`, confirm the line clears; run `c` on a host without the `shares` sudoers feature and confirm silence; watch the daemon's marker appear and vanish in `/var/db/zfsctl` during a daily pass (delete `smart.json` and `shares.json` to force one). Restore every service afterwards.
- [ ] **Step 3: Commit** `docs: shares check and checks on demand`

### Task 6: Whole-branch review, merge, install

- [ ] **Step 1:** Full suite and clippy; whole-branch review with the Review Focus section; fix findings test-first.
- [ ] **Step 2:** Merge to `master` and install on the hub after the user's go-ahead; update the memory note.
