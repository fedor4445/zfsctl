# Disk Health and Resilver Line Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Dashboard shows a yellow line for a pool that is resilvering, and one line per host whose disks have a SMART problem, found by a once-a-day `smartctl` check in the daemon.

**Architecture:** Part 2 (resilver) changes only the host probe's scan summary and the pure Dashboard judging. Part 1 (SMART) adds a pure parse-and-judge module (`src/smart.rs`) fed by fixture JSON, a sudoers feature, a daemon thread that writes `smart.json` once a day, and a Dashboard reader. The TUI never runs `smartctl`.

**Tech Stack:** Rust, serde_json (already a dependency), ratatui, the existing `CommandRunner`/`HostInfo` plumbing.

**Spec:** `docs/superpowers/specs/2026-10-17-disk-health-design.md`

## Global Constraints

- Resilver line: yellow ("to watch"), text `<pool> resilvering <n>%` (`resilvering` alone when the percent is unknown); a paused one reads `<pool> resilver paused`; a running scrub gets no line; a finished resilver gets no line. Enter goes to Pools, with the host selected.
- SMART: the daemon runs `smartctl` once a day at a local time, default `03:30`, `[daemon] smart_check = "off"` or `"HH:MM"`, read when the daemon starts only. It also checks at start when `smart.json` is missing or older than 24 hours.
- Read-only flags only: `smartctl --scan -j`, then `smartctl -j -H -A -i /dev/<dev>`. Never `-t`, never a write.
- Problem rules (fixed): red = overall health not PASSED; yellow = reallocated sectors > 0, pending sectors > 0, offline-uncorrectable > 0, NVMe critical warning != 0, NVMe available spare below its threshold, SSD/NVMe percentage used >= 80, temperature >= 60 C, or a disk that says `smart_support.available` true but answers with an error or no attribute table ("SMART unreadable").
- Silent: no `smartctl`, sudo denied, an empty scan, a disk without `smart_support.available` true, an unreachable host. smartmontools older than 7 (no `-j`) counts as "does not report".
- `smart.json` in the state directory, written atomically; one entry per host with its own `checked_at` and, when a check failed, an `error` kept next to the previous result. The Dashboard keeps showing the last good result with its real age; one extra yellow line `disk check is N days old` when the newest `checked_at` is older than 3 days while the daemon runs.
- Dashboard line per host: `<disk> <what>; <disk> <what>   (checked 6 h ago)`; red if any disk is red, else yellow. SMART lines are ordinary Needs attention items and count in the verdict. Enter goes to Disks of that host.
- The daily report gets the same lines; no push alerts for SMART.
- Sudoers: new feature `smart` (`--with smart` / `--without smart`), entry `smartctl` pinned to `--scan -j` and `-j -H -A -i /dev/*`; `check-host` must not report the wildcard form as too broad.
- Style: pure modules with fixtures, comments explain why, `cargo clippy --all-targets` adds no warnings to new code, tests next to the code or in `src/ui/state/tests.rs`.

## Review Focus

- A pool resilvering while also DEGRADED: both its health line and the resilver line show, once each (Task 1 test).
- Garbage, truncated or empty `smartctl` output, and JSON from a different smartmontools version: no panic, the disk is "SMART unreadable" or silent per the rules (Task 2 tests).
- A host that hangs or errors during the daily check: the other hosts still finish, `smart.json` still gets written, the previous result stays (Task 4 tests).
- A `smart.json` that is missing, empty, torn or from an older shape: the Dashboard shows nothing and does not crash (Task 5 test).
- An existing install without the `smart` sudoers entry: nothing turns yellow, `check-host --with smart` says what is missing (Task 3 test).

---

### Task 1: A resilver in progress on the Dashboard

**Files:**
- Modify: `src/zfs/host_health.rs` (`ScanSummary`, `scan_summary`), `src/ui/dashboard.rs` (attention items), every other use of `ScanSummary::Running` / `Paused` the compiler lists
- Test: the same files' `mod tests`, `src/ui/state/tests.rs`

**Interfaces:**
- Produces: `ScanSummary::Running { percent: Option<u8>, resilver: bool }` and `ScanSummary::Paused { resilver: bool }`; a Dashboard attention item `Item { level: Level::Watch, subject: <host>, what: "<pool> resilvering 42%", goto: Section::Pools, target: Some(<host>) }`.

- [ ] **Step 1: Write the failing tests.** In `host_health.rs`: `scan_summary_tells_a_resilver_from_a_scrub` (a `zpool status` block with `scan: resilver in progress since ...` and `42.00% done` gives `Running { percent: Some(42), resilver: true }`; the scrub-in-progress block gives `resilver: false`; `scan: resilver paused` gives `Paused { resilver: true }`). In `dashboard.rs`: `a_resilvering_pool_is_one_yellow_line_and_a_scrub_or_a_finished_resilver_is_none` (reached host with pools `tank` Running resilver 42 -> exactly one item `tank resilvering 42%`, Watch, goto Pools; scrub running -> none; Finished -> none; paused resilver -> `tank resilver paused`; a DEGRADED resilvering pool -> its health item and the resilver item both present).
- [ ] **Step 2: Run** `cargo test --offline resilver` and the new host_health test: FAIL (compile or assertion).
- [ ] **Step 3: Implement** the two signatures above; derive `resilver` from the status text (the summary starts with `resilver`); add the item in the same place that builds the pool items of a reached host.
- [ ] **Step 4: Run** `cargo test --offline` (whole suite): PASS.
- [ ] **Step 5: Commit** `feat: the Dashboard flags a pool that is resilvering`

### Task 2: Parse and judge smartctl output

**Files:**
- Create: `src/smart.rs` (declare in `src/main.rs`), fixtures under `src/smart/fixtures/*.json` (SATA HDD healthy, SATA HDD with reallocated sectors, SATA SSD at 87% wear, NVMe with low spare, a FAILED overall, a disk with `smart_support.available: false`, a truncated file, an empty file, `{}`)

**Interfaces:**
- Produces:
  - `pub struct Disk { pub dev: String, pub problems: Vec<Problem> }`, `pub struct Problem { pub level: Level, pub text: String }` with `Level` = `crate::zfs::host_health::Level`-like `{ Watch, Issue }` (define `pub enum Severity { Watch, Issue }` in `smart.rs`).
  - `pub fn parse_scan(json: &str) -> Vec<String>`: device names from `smartctl --scan -j` (`/dev/ada0` -> `ada0`), empty on garbage.
  - `pub fn judge(dev: &str, json: &str) -> Option<Disk>`: `None` = silent (does not report); `Some(Disk { problems: [] })` = healthy; problems per the Global Constraints, texts like `reallocated sectors 8`, `SSD wear 87%`, `temperature 63 C`, `SMART health FAILED`, `SMART unreadable`.

- [ ] **Step 1: Write the failing tests** (one per fixture): `a_healthy_sata_hdd_has_no_problems`, `reallocated_sectors_are_a_watch_with_the_count`, `an_ssd_at_87_percent_wear_is_a_watch`, `a_low_nvme_spare_is_a_watch`, `a_failed_overall_health_is_an_issue`, `a_disk_without_smart_support_is_silent`, `a_disk_that_claims_support_but_has_no_attribute_table_is_unreadable`, `garbage_truncated_empty_and_braces_only_never_panic` (all four inputs return `None` or an unreadable disk, per rule), `parse_scan_lists_devices_and_survives_garbage`.
- [ ] **Step 2: Run** `cargo test --offline smart::`: FAIL.
- [ ] **Step 3: Implement** with `serde_json::Value` navigation (`smart_status.passed`, `ata_smart_attributes.table[]` ids 5/197/198, `nvme_smart_health_information_log`, `temperature.current`, `smart_support.available`, `device.name`); the 60 C and 80 % limits are named constants.
- [ ] **Step 4: Run** `cargo test --offline smart::`: PASS; clippy clean for the new file.
- [ ] **Step 5: Commit** `feat: parse and judge smartctl output`

### Task 3: The `smart` sudoers feature

**Files:**
- Modify: `src/exec/privilege.rs` (`Feature::Smart`, `ALL`, `name`, an `Entry` for `smartctl`), `src/exec/sudoers.rs` (the wildcard-pinned form in render and in `check_host`), the book drift tests' expectations the compiler or tests name
- Test: `src/exec/sudoers.rs` `mod tests`

**Interfaces:**
- Produces: `Feature::Smart` named `smart`; `smartctl` entry, `Kind::Privileged` with `pinned: &[&["--scan", "-j"], &["-j", "-H", "-A", "-i", "/dev/*"]]` and `probe: ""` meaning "no appended-argument probe" (document that on `Kind::Privileged`); render writes `/usr/local/sbin/smartctl --scan -j, /usr/local/sbin/smartctl -j -H -A -i /dev/*` (path as resolved on the host).

- [ ] **Step 1: Write the failing tests:** `print_sudoers_with_smart_pins_the_two_smartctl_forms_and_says_the_wildcard_is_not_a_barrier`, `check_host_does_not_flag_the_device_wildcard_as_too_broad`, `check_host_reports_smartctl_missing_when_sudo_refuses_it`, `smart_is_not_in_the_default_feature_set` (default features unchanged; `--with smart` adds it).
- [ ] **Step 2: Run** them: FAIL.
- [ ] **Step 3: Implement** as described; the generated comment line says the entry cannot stop extra flags because sudo's `*` matches spaces and the account already holds `zfs`/`zpool`.
- [ ] **Step 4: Run** `cargo test --offline` (the book's privileged-names drift test too): PASS.
- [ ] **Step 5: Commit** `feat: the smart sudoers feature for smartctl`

### Task 4: The daily check in the daemon

**Files:**
- Create: `src/daemon/smart_check.rs` (declare in `src/daemon/mod.rs`)
- Modify: `src/daemon/mod.rs` (start the thread in `run` beside the monitor), `src/config.rs` (`DaemonConfig.smart_check: Option<String>`, validated; `is_default`), `docs/book/21-reference-config.md` row (the book test lists settings)
- Test: `src/daemon/smart_check.rs` `mod tests`, `src/config.rs`

**Interfaces:**
- Consumes: Task 2 `parse_scan`, `judge`, `Disk`; `Shared.hosts` runners (`CommandRunner::run`, privileged form) as the monitor uses them.
- Produces: `pub struct HostResult { pub checked_at: DateTime<Utc>, pub disks: Vec<Disk>, pub error: Option<String> }`, `pub fn check_host(runner: &dyn CommandRunner, now: DateTime<Utc>, previous: Option<&HostResult>) -> Option<HostResult>` (`None` = silent host: no smartctl/denied/empty scan; keeps no entry); `pub fn run_all(hosts: &[(String, Arc<dyn CommandRunner>)], now, previous: &SmartFile, deadline: Duration) -> SmartFile`; `pub struct SmartFile(BTreeMap<String, HostResult>)` with `load(path) -> SmartFile` (empty on missing/torn/old shape) and `save(path, &SmartFile) -> io::Result<()>` (temp file + rename); `pub fn due(now_local: NaiveDateTime, at: Option<NaiveTime>, last: Option<DateTime<Utc>>) -> bool`; config accessor `DaemonConfig::smart_time() -> Result<Option<NaiveTime>, String>` (default 03:30, `off` = None).

- [ ] **Step 1: Write the failing tests** with `FakeRunner` and fixtures from Task 2: `a_host_with_a_failing_disk_gets_an_entry_with_its_problem`, `a_host_without_smartctl_or_with_sudo_denied_gets_no_entry`, `a_failed_check_keeps_the_previous_result_and_records_the_error`, `a_hung_host_does_not_stop_the_others` (runner that sleeps past the deadline), `save_is_atomic_and_load_survives_a_torn_or_empty_file`, `due_once_per_local_day_from_the_time_on_and_at_start_when_the_file_is_old`, `smart_check_accepts_off_and_hh_mm_and_rejects_the_rest`.
- [ ] **Step 2: Run** them: FAIL.
- [ ] **Step 3: Implement.** One thread started in `run`, independent of the notify monitor (it must work with alerts off); it wakes at least every 10 minutes and when `due`; per-host deadline 2 minutes; logs one line per run through the daemon's `log`.
- [ ] **Step 4: Run** `cargo test --offline`: PASS; clippy clean for new code.
- [ ] **Step 5: Commit** `feat: the daemon checks disks with smartctl once a day`

### Task 5: Show it: Dashboard lines and the daily report

**Files:**
- Modify: `src/ui/dashboard.rs` (`Inputs` gets `smart: &SmartFile` and `daemon_running`; items), `src/ui/state.rs` (load `smart.json` where the Dashboard's other file data is refreshed; `App.smart`), `src/daemon/health_text.rs` (`report_message` gets the lines), `src/daemon/monitor.rs` (passes them)
- Test: `src/ui/dashboard.rs`, `src/ui/state/tests.rs`, `src/daemon/health_text.rs`

**Interfaces:**
- Consumes: Task 4 `SmartFile`, `HostResult`; Task 2 `Disk`, `Severity`.
- Produces: `pub fn smart_items(file: &SmartFile, now: DateTime<Utc>, daemon_running: bool) -> Vec<Item>` (one item per host with problems, `goto: Section::Disks`, `target: Some(host)`; plus the `disk check is N days old` item) and `pub fn smart_lines(file: &SmartFile, now) -> Vec<String>` used by the report.

- [ ] **Step 1: Write the failing tests:** `a_host_with_a_failing_disk_is_one_red_line_and_a_healthy_host_none`, `several_disks_of_a_host_share_one_line_with_its_age`, `the_last_good_result_stays_after_a_failed_check_with_its_real_age`, `a_check_older_than_three_days_while_the_daemon_runs_adds_one_yellow_line`, `a_missing_or_torn_smart_file_shows_nothing`, `enter_on_a_smart_line_selects_the_host_in_disks` (state test), `the_verdict_counts_smart_lines`, `the_daily_report_lists_the_same_lines`.
- [ ] **Step 2: Run:** FAIL. **Step 3: Implement** to the signatures; text format from the Global Constraints. **Step 4: Run** `cargo test --offline`: PASS. 
- [ ] **Step 5: Commit** `feat: disk health on the Dashboard and in the daily report`

### Task 6: Book, README and the live check

**Files:**
- Modify: `docs/book/06-the-screen.md` (Dashboard section: the two new kinds of line), `docs/book/16-alerts.md` (the report lists them), `docs/book/04-installing.md` or `05-remote-hosts.md` (the `smart` feature and `pkg install smartmontools`), `docs/book/21-reference-config.md` (`smart_check`; already added in Task 4 if the test forced it), `docs/book/22-reference-commands.md` (`--with smart`), `README.md`

- [ ] **Step 1:** Update the prose; run `cargo test --offline` (book drift tests) to PASS.
- [ ] **Step 2: Live check on the hub** (deploy as before, run the built binary without installing first): `pkg install smartmontools` on the hub, `zfsctl print-sudoers local --with smart` and install the line, start the daemon build, confirm `smart.json` appears with the hub's disks; confirm the VMs (virtual disks) get no entry and no Dashboard line; copy `smart.json` aside, hand-write a red and a yellow entry, confirm the Dashboard lines and Enter navigation, restore the file. For the resilver line, replace a scratch file vdev in a throwaway pool on the hub and watch the line appear and disappear; remove the throwaway pool.
- [ ] **Step 3: Commit** `docs: disk health and the resilver line`

### Task 7: Whole-branch review, merge, install

- [ ] **Step 1:** Full suite and clippy; whole-branch review with the Review Focus section; fix findings test-first.
- [ ] **Step 2:** Merge to `master` and install on the hub (after the user's go-ahead), update the memory note.
