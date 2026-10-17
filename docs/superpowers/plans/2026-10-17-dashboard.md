# Dashboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A first sidebar section, Dashboard: one borderless, colour-coded screen with the health of every zfsctl component (hosts with space, memory, CPU, load, scrub age and disk errors; daemon; jobs; cloud; alerts), problems on top. Display only.

**Architecture:** A pure rule layer `ui::dashboard` turns facts the app already holds (host probes, jobs view, cloud rows, alerts view) plus a few new per-host readings into rows and levels; a separate draw module renders them. The new readings (memory, CPU ticks, load, swap, scrub time) ride the existing 30 s host probe through one new function, only in the TUI. The alert monitor, `assess()` and the Hosts tab are unchanged.

**Tech Stack:** Rust, ratatui, existing `CommandRunner` (`run_unprivileged` for sysctl/swapinfo/date), `FakeRunner` test support, toml/serde config.

**Spec:** `docs/superpowers/specs/2026-10-17-dashboard-design.md` (read its "Calls made after the builder check" section: those calls are binding).

## Global Constraints

- Display only: no key on the Dashboard changes anything. Keys: `Tab`/`Shift+Tab` as everywhere, `r` refreshes, `Up`/`Down` select a line of Needs attention, `Enter` jumps to the owning section (navigation only).
- No new sudoers entries and no new privileged commands: only `sysctl`, `swapinfo`, `date` through `run_unprivileged`. At most three extra calls per host per probe (`sysctl -n` x1, `swapinfo -k` x1, `date +%z` only while the host's offset is unknown). The daemon's probes are untouched.
- The Dashboard never measures a bucket and never calls `rclone size`.
- `assess()`, the Hosts tab, `daemon::monitor` are unchanged. The Dashboard may only raise a host's level, never lower it.
- Levels: Fine (green), Watch (yellow), Issue (red), Unknown (dim `?`). Missing or stale data is Unknown, never Fine. Every status has a glyph and a word: `●` fine, `▲` watch, `✖` issue, `○` unknown.
- Defaults (spec table): space 80/90 (existing); memory 90/97; CPU 95 sustained over two probes, watch only, never red; swap 50/90; scrub age 35 days watch, 90 days issue; never scrubbed is Watch; a resilver does not reset scrub age; `[dashboard]` overrides them and `scrub_ignore = [pool names]`.
- Memory in use must not count reclaimable cache: `used% = 100 * (physical - min(physical, free + inactive + arc)) / physical`.
- While any host has not answered its first probe, the verdict is "checking..." (dim), never "all quiet". After that an unreachable or uninspectable host is a Watch line.
- Disabled jobs are left out of job counts.
- Colours come from the theme (`app.theme`), never hard-coded; the `mono` theme must stay readable.
- Usable at 80 columns, not broken at 60 and at 40; gauges only where width allows.
- Work on branch `dashboard`; commit after every task; `cargo test` must pass before each commit; deploy to the hub as in earlier work (scp changed files, `cargo test --release`, install, restart) only in the last task.

## Review Focus

1. A host with no pools, or a probe that reached the host but has no resource readings: shown as Unknown cells, never Fine, never a panic (Task 5).
2. A FreeBSD host whose ARC fills the RAM: memory stays Fine (Task 1 and Task 5 boundary tests).
3. A never-scrubbed pool and a `scrub_ignore` pool: Watch for the first, ignored for the second; a resilver-only pool counts as never scrubbed (Task 2, Task 5).
4. The first seconds after start-up, every host unanswered: verdict "checking...", not "all quiet" and not "N issues" (Task 6).
5. No hosts besides local, no jobs, no cloud remotes, an unreadable `config.toml`, invalid threshold values (warn >= crit, 0, over 100): sensible text, defaults used, never a crash (Tasks 4, 6, 8).

---

### Task 1: Per-host resource readings (pure parsing and maths)

**Files:**
- Create: `src/zfs/host_resources.rs`; register `pub mod host_resources;` in `src/zfs/mod.rs`

**Interfaces:**
- Consumes: `zfs::host::{CpuTimes, Memory, Swap, parse_cpu_times, parse_loadavg, parse_swapinfo, cpu_percent}`, `exec::CommandRunner`.
- Produces:
  - `pub struct Resources { pub cores: u32, pub physical: u64, pub free: u64, pub inactive: u64, pub arc: u64, pub load: Option<[f64; 3]>, pub cpu_ticks: Option<CpuTimes>, pub swap: Option<Swap> }` (derive Debug, Clone, PartialEq)
  - `pub fn parse_resources_sysctl(output: &str) -> Option<(u32, u64, u64, u64, u64, Option<[f64;3]>, Option<CpuTimes>)>` or an equivalent private helper: nine `sysctl -n` lines in the order `hw.ncpu hw.physmem hw.pagesize vm.stats.vm.v_free_count vm.stats.vm.v_inactive_count kstat.zfs.misc.arcstats.size vm.loadavg kern.cp_time` (pages are multiplied by the page size).
  - `pub fn probe_resources(runner: &dyn CommandRunner) -> Option<Resources>`: one `run_unprivileged("sysctl", ...)`, one `run_unprivileged("swapinfo", &["-k"])`; `None` if the sysctl call fails or does not parse; swap `None` when swapinfo fails.
  - `pub fn memory_used_percent(r: &Resources) -> u8`, `pub fn swap_used_percent(r: &Resources) -> Option<u8>`, `pub fn cpu_percent_between(before: &Resources, after: &Resources) -> Option<u8>` (rounded, clamped to 100).
  - `pub fn utc_offset_secs(runner: &dyn CommandRunner) -> Option<i32>`: one `run_unprivileged("date", &["+%z"])`, `+0200` is 7200, `-0430` is -16200.

- [ ] **Step 1: Write failing tests** in the module: `memory_used_ignores_arc_inactive_and_free` (physical 20 GiB, free 2, inactive 3, arc 10 gives 25%; arc 19 with free 1 gives 0%, no underflow), `an_arc_filled_host_is_not_under_memory_pressure`, `swap_percent_is_none_without_swap`, `cpu_percent_between_two_readings_is_the_busy_share` (use `CpuTimes` from the existing tests' shape), `probe_resources_makes_exactly_two_unprivileged_calls` (FakeRunner records calls: `sysctl -n hw.ncpu ...` then `swapinfo -k`), `a_failed_sysctl_gives_none`, `utc_offsets_parse_both_signs_and_reject_garbage`.
- [ ] **Step 2: Run** `cargo test host_resources` and see them fail to compile or fail.
- [ ] **Step 3: Implement** the signatures above in `src/zfs/host_resources.rs`, reusing the existing parsers; the arithmetic follows the Global Constraints formula.
- [ ] **Step 4: Run** `cargo test host_resources`; all pass.
- [ ] **Step 5: Commit** `git commit -m "feat: per-host resource readings (memory without cache, cpu ticks, load, swap)"`

### Task 2: When the last scrub finished

**Files:**
- Modify: `src/zfs/host_health.rs` (`PoolProbe`, `StatusFacts`, `parse_status_all`, every literal construction of `PoolProbe`, also in `src/zfs/host.rs::sample_host`)

**Interfaces:**
- Produces: `PoolProbe.last_scrub: Option<String>` (the `when` text, as `zpool status` prints it in the host's local time, only for a finished **scrub**: summary starting `scrub repaired`, never for `resilvered`); `StatusFacts.last_scrub: Option<String>`; `pub fn scrub_age_days(when: &str, utc_offset_secs: i32, now_utc: chrono::DateTime<chrono::Utc>) -> Option<i64>` (parses `Sat Oct  8 19:48:07 2026` after collapsing whitespace, subtracts the offset, floors to whole days, `None` if unparsable, never negative).

- [ ] **Step 1: Write failing tests** next to the existing status captures: a pool whose scan line is `scrub repaired 0B in 00:00:07 with 0 errors on Wed Oct  8 19:48:07 2026` has `last_scrub == Some("Wed Oct  8 19:48:07 2026")`; one with `resilvered 1.2M in ... on ...` has `None`; one with no scan has `None`; `scrub_age_days("Wed Oct  8 19:48:07 2026", 7200, now)` is 9 when now is 2026-10-17T20:00:00Z; garbage gives `None`.
- [ ] **Step 2: Run** the module tests; fail.
- [ ] **Step 3: Implement.** Take the text from `ScanStatus::Completed { summary, .. }` inside `parse_status_all` (the summary is already split at ` on ` in `scan_summary`; derive `last_scrub` from the same summary). Set `last_scrub` in both probe constructors.
- [ ] **Step 4: Run** `cargo test`; everything passes (fix the struct-literal call sites).
- [ ] **Step 5: Commit** `feat: pool probes remember when the last scrub finished`

### Task 3: Collect the readings in the TUI's host probe

**Files:**
- Modify: `src/ui/state.rs` (`HostProbeState`, `start_host_probe`, `WorkerMsg`), `src/ui/state/worker.rs` (message handling), tests in `src/ui/state/tests.rs`

**Interfaces:**
- Consumes: Task 1 `probe_resources`, `utc_offset_secs`, `Resources`.
- Produces: `WorkerMsg::HostResources(String /*host name*/, Option<Resources>, Option<i32> /*offset if asked*/)`; `HostProbeState.resources: Vec<Resources>` (the last two readings, oldest first), `HostProbeState.utc_offset_secs: Option<i32>`; `App::host_resources(&self, name: &str) -> Option<(&Resources, Option<&Resources>)>` (newest and the one before).

- [ ] **Step 1: Write failing tests:** a `HostResources` message stores the reading and keeps only the last two; the offset is stored once and `start_host_probe` is told it need not ask again (assert via a recording runner that `date` is called once over two probes); a probe that is `Disconnected` clears `resources`; the daemon's probe path (`probe_host_guarded`) makes no resource calls (grep-style assertion with a recording runner).
- [ ] **Step 2: Run** them; fail.
- [ ] **Step 3: Implement.** In the thread spawned by `start_host_probe`, after `probe_host_guarded` returns a `Reached` probe, call `probe_resources` and, only when the offset is unknown, `utc_offset_secs`, then send `HostResources`. A non-`Reached` probe sends `HostResources(name, None, None)` so stale numbers do not stay on screen.
- [ ] **Step 4: Run** `cargo test`; pass.
- [ ] **Step 5: Commit** `feat: the host probe collects memory, cpu, load and swap for the TUI`

### Task 4: `[dashboard]` thresholds

**Files:**
- Modify: `src/config.rs`, `docs/book/21-reference-config.md`

**Interfaces:**
- Produces: `pub struct DashboardConfig { memory_warn, memory_crit, swap_warn, swap_crit, cpu_warn: Option<u8>, scrub_warn_days, scrub_crit_days: Option<u32>, scrub_ignore: Vec<String> }` (serde default, skipped when default, field of `Config` as `dashboard`); `pub struct Thresholds { pub memory: (u8, u8), pub swap: (u8, u8), pub cpu_warn: u8, pub scrub_days: (u32, u32), pub scrub_ignore: Vec<String> }` with `impl Default` (spec defaults) and `impl DashboardConfig { pub fn resolve(&self) -> Thresholds }` that falls back to the default for any pair that is not `0 < warn < crit <= 100` (days: `0 < warn < crit`) and for `cpu_warn` outside 1..=100.

- [ ] **Step 1: Failing tests:** defaults equal the spec numbers; a TOML `[dashboard]` with overrides round-trips and resolves; invalid pairs (warn >= crit, 0, 101) fall back to defaults; `documented_config_keys()` gains the new keys and the drift test passes only once the book lists them.
- [ ] **Step 2: Run** `cargo test config`; fail.
- [ ] **Step 3: Implement**, add the keys to `documented_config_keys()` and the `[dashboard]` table to `21-reference-config.md` (one row per key with default and meaning) and "five parts" becomes "six parts".
- [ ] **Step 4: Run** `cargo test`; pass (including `book_check`).
- [ ] **Step 5: Commit** `feat: [dashboard] thresholds in config.toml`

### Task 5: Host rows and levels (pure)

**Files:**
- Create: `src/ui/dashboard.rs`; register `pub mod dashboard;` in `src/ui/mod.rs`

**Interfaces:**
- Consumes: Tasks 1-4, `zfs::host_health::{HostProbe, PoolProbe, ScanSummary, Assessment, Level as HealthLevel, scrub_age_days}`.
- Produces:
  - `pub enum Level { Fine, Watch, Issue, Unknown }` (Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord with `Issue > Watch > Unknown > Fine` for "worst first" sorting via a helper `Level::rank(self) -> u8`)
  - `pub struct Cell { pub text: String, pub level: Level, pub gauge: Option<u8> }`
  - `pub struct HostRow { pub name: String, pub state: Cell, pub pools: Cell, pub space: Cell, pub memory: Cell, pub cpu: Cell, pub load: Cell, pub scrub: Cell, pub errors: Cell }`
  - `pub struct HostInput<'a> { pub name: &'a str, pub assessment: Assessment, pub probe: Option<&'a HostProbe>, pub resources: Option<(&'a Resources, Option<&'a Resources>)>, pub utc_offset_secs: Option<i32> }`
  - `pub fn host_row(input: &HostInput, t: &Thresholds, now: DateTime<Utc>) -> HostRow` and `pub fn host_attention(row: &HostRow) -> Vec<(Level, String)>` (one `(level, what)` per non-Fine cell, e.g. `(Issue, "memory 98%")`, `(Watch, "never scrubbed: zroot")`).

- [ ] **Step 1: Write failing tests**, each with its exact boundary values: space 79 Fine / 80 Watch / 90 Issue; memory 89 Fine / 90 Watch / 97 Issue; swap 50 and 90 edges and no-swap = Fine "-"; CPU 95 is Watch only when **both** consecutive readings reach it, a single spike is Fine, CPU never Issue; scrub age 34 Fine, 35 Watch, 90 Issue, `Never` Watch ("never scrubbed: zroot"), a pool in `scrub_ignore` is skipped, a resilver-only pool (`last_scrub None`, scan `Finished`) counts as never; pool errors above 0 are Watch with the count in the text, pool DEGRADED/FAULTED stays Issue via `assess`; the host's state cell starts from `assessment` and is only raised (a Fine assessment with memory 98% gives state Issue; an Issue assessment with everything Fine stays Issue); `probe: None` or `Disconnected` gives Unknown or Issue per `assess` and Unknown for every reading; a host with no pools shows `-` Fine pools/space/scrub; resources `None` gives Unknown memory/cpu/load (never Fine); ARC-full host (arc 18 of 20 GiB) memory Fine.
- [ ] **Step 2: Run** `cargo test dashboard`; fail.
- [ ] **Step 3: Implement** with small private helpers per reading (`memory_cell`, `cpu_cell`, `swap` folded into the memory cell text as `98% (swap 60%)` only when swap itself is Watch/Issue, `scrub_cell`, `space_cell` reusing `host_health::used_cell` levels, `errors_cell`).
- [ ] **Step 4: Run** `cargo test dashboard`; pass.
- [ ] **Step 5: Commit** `feat: dashboard host rows and levels`

### Task 6: The other components, the attention list and the verdict (pure)

**Files:**
- Modify: `src/ui/dashboard.rs`

**Interfaces:**
- Consumes: Task 5, `ui::jobs::{JobsView, DaemonState}`, `ui::cloud::{Row, rows, failed, used_bytes, Caps}`, `ui::alerts::AlertsView`.
- Produces:
  - `pub struct Line { pub level: Level, pub text: String }`
  - `pub struct Item { pub level: Level, pub subject: String, pub what: String, pub goto: crate::ui::state::Section }`
  - `pub struct Dashboard { pub verdict: Line, pub attention: Vec<Item>, pub hosts: Vec<HostRow>, pub daemon: Line, pub jobs: Line, pub failed_jobs: Vec<String>, pub cloud: Vec<Line>, pub alerts: Line }`
  - `pub struct Inputs<'a> { pub hosts: Vec<HostInput<'a>>, pub jobs: &'a JobsView, pub cloud_rows: Vec<crate::ui::cloud::Row>, pub cloud_sizes: &'a HashMap<String, crate::ui::cloud::Size>, pub cloud_caps: &'a crate::ui::cloud::Caps, pub alerts: &'a AlertsView, pub thresholds: &'a Thresholds, pub now: DateTime<Utc> }`
  - `pub fn build(inputs: &Inputs) -> Dashboard`; verdict text: `checking...` while any host probe is `None`; else `all quiet`, or `N issue(s), M to watch`.

- [ ] **Step 1: Write failing tests:** daemon fresh heartbeat Fine "running (pid N), heartbeat Xs ago", missing or stale Issue; jobs line counts only enabled jobs by kind `scrub 1/1 ok  backup 0/1 FAILED`, failed job ids listed in `failed_jobs` and as Issue items going to `Section::Jobs`; cloud lines mirror `ui::cloud` (failed latest run Issue, no jobs Unknown dim "no jobs", measured size appears only if present in `cloud_sizes`); alerts: monitor off while settings exist is Watch, `problem` set is Watch with the text, no settings at all is Unknown "alerts not configured"; attention is sorted Issue before Watch before Unknown, then by subject, capped by the caller (build returns all); verdict: all hosts unanswered gives `checking...`; one answered Fine host and nothing else gives `all quiet`; an unreachable host gives a Watch item "cannot be reached" and the verdict counts it; zero hosts besides `local`, zero jobs, zero cloud rows, empty alerts all build without panic.
- [ ] **Step 2: Run** `cargo test dashboard`; fail.
- [ ] **Step 3: Implement** `build` composing `host_row`, `host_attention` and the new lines; `goto` is `Section::Hosts` for host items, `Jobs`, `Cloud`, `Alerts` otherwise.
- [ ] **Step 4: Run** `cargo test dashboard`; pass.
- [ ] **Step 5: Commit** `feat: dashboard components, attention list and verdict`

### Task 7: The section, its data and its keys

**Files:**
- Modify: `src/ui/state.rs` (`Section::Dashboard` first in the enum and in `sections`, `selected_section` default 0 stays and now means Dashboard; `App::dashboard_attention_table`; refresh hooks), `src/ui/mod.rs` (event-loop arms), `src/ui/hints.rs`, help text in `state.rs`, `docs/book/20-reference-keys.md`, tests in `src/ui/state/tests.rs` and `src/ui/hints.rs`

**Interfaces:**
- Consumes: Task 6 `build`, `Inputs`.
- Produces: `App::dashboard(&self) -> crate::ui::dashboard::Dashboard` (assembles `Inputs` from `host_assessment`, `host_probes`, `host_resources`, `jobs_view`, `cloud_rows()`, `cloud_sizes`, `cloud_caps`, `alerts_view`, thresholds read from `config_path` with defaults on error, `Utc::now()`); `App::dashboard_table: TableState` over the attention lines; Enter on a selected line sets `selected_section` to that item's `goto`.
- While the Dashboard is on screen it refreshes jobs every 2 s, alerts and the cloud remote list (`refresh_cloud_if_stale` also fires for `Section::Dashboard`), thresholds with the cloud caps.

- [ ] **Step 1: Failing tests:** the sidebar order starts with Dashboard then Hosts; a fresh `App` is on Dashboard; Tab from Alerts wraps to Dashboard; Down/Up move through attention lines, Enter jumps to that item's section and changes nothing else (`pending_mutations` empty, no modal); `r` refreshes; while on Dashboard the cloud list is requested once and then once a minute; hints advertise `Move` and `Enter` only when attention lines exist and "Refresh" always; the `every_advertised_key_actually_does_something` guard passes with `dashboard_table` in the fingerprint; the key-reference drift test passes with a `## Dashboard` table.
- [ ] **Step 2: Run** `cargo test`; fail.
- [ ] **Step 3: Implement**, fixing every existing test that assumed Hosts is first (grep `selected_section`, `Section::Hosts` in tests; use name lookups, not indexes).
- [ ] **Step 4: Run** `cargo test`; pass.
- [ ] **Step 5: Commit** `feat: the Dashboard section, its refreshes and keys`

### Task 8: Drawing it

**Files:**
- Create: `src/ui/dashboard_view.rs` (register in `src/ui/mod.rs`); Modify: `src/ui/layout.rs` (`draw_sidebar` label, `draw_main` arm, no metrics panel for Dashboard)

**Interfaces:**
- Consumes: `App::dashboard()`, `app.theme` (styles `ok`, `warn`, `err`, `muted`/`dim`), `app.dashboard_table`.
- Produces: `pub fn draw_dashboard(f: &mut Frame, app: &mut App, area: Rect)` and pure helpers `pub fn gauge(percent: u8, width: u16) -> String` (`▕████░░░░░░▏` style, exactly `width` cells, clamped) and `pub fn glyph_word(level: Level) -> (&'static str, &'static str)`.

- [ ] **Step 1: Failing render tests** (`TestBackend`, same helper style as `layout.rs` tests): at 120 columns every section heading and the gauges appear and a Fine, Watch and Issue host are distinguishable by glyph and word; at 80 columns gauges are gone and numbers remain; at 60 and at 40 nothing panics and the host name and state stay; under the `mono` theme the words and glyphs alone identify levels; the attention block is capped at 6 lines with `+N more`; an empty attention list says `nothing needs attention` in the Fine style; `gauge(43, 10)` has 10 cells and `gauge(250, 10)` is full; the verdict text sits on the header line right-aligned when it fits.
- [ ] **Step 2: Run**; fail.
- [ ] **Step 3: Implement** with thin dim rules under each uppercase heading, aligned columns, the selected attention line highlighted with `theme.selected_row()`, columns dropping in this order as width shrinks: gauges, load, cpu, scrub, errors, pools.
- [ ] **Step 4: Run** `cargo test`; pass.
- [ ] **Step 5: Commit** `feat: draw the Dashboard`

### Task 9: Docs, live check, merge, install

**Files:**
- Modify: `docs/book/*` (new chapter section "The Dashboard" in `06-the-screen.md` or its own chapter, with a real capture `06-dashboard`), `docs/book/tools/capture.py` (`SECTIONS` gains `Dashboard` first), `README.md`, `docs/book/20-reference-keys.md`

- [ ] **Step 1: Write** the book text (what each line means, the thresholds table, `[dashboard]` keys, "display only", the Shares row left out in this version and why: needs a privileged check per host per probe) and the README paragraph.
- [ ] **Step 2: Deploy to the hub** (scp changed files, `cargo test --release`, install with a fallback copy, restart) and look at the screen with the pexpect/pyte driver. Force colours without stressing anything: temporarily set `[dashboard] memory_warn = 1, memory_crit = 2` in the hub's `config.toml` (backup first) and see yellow then red; stop the daemon briefly to see its red line; restore both; check a VM host row appears with its own readings.
- [ ] **Step 3: Capture** `06-dashboard` with `capture.py`, embed it, run `cargo test` (the book drift tests).
- [ ] **Step 4: Fresh whole-branch review** by a new reviewer on the most capable model, fix findings.
- [ ] **Step 5: Merge** `dashboard` into `master` (`--no-ff`), install on the hub, update memory notes.
