# Quotas and reservations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (the user chose native execution). Steps use checkbox syntax.

**Goal:** Guided editing of `quota`, `refquota`, `reservation`, `refreservation`; a Quota column and quota-aware Used line; Dashboard lines for datasets near their quota, from a new shared `storage.json` check.

**Architecture:** A pure module `zfs::quota` (size parsing, the refusal matrix, confirm tier, percent-of-limit). The dataset Properties `Enter` form for the four keys is validated in `submit_form` against facts read from the selected dataset's property list. A new standalone `daemon::storage_check` (own file `storage.json`, own 10-minute loop, also run by `c` in Hosts) feeds Dashboard Needs-attention items and the daily report. Thresholds join `[dashboard]` and the Settings screen.

**Tech Stack:** Rust, ratatui; `zfs list/get/set` only (no sudoers change, all three OSes).

**Spec:** `docs/superpowers/specs/2026-10-18-quotas-reservations-design.md` (with its "Decisions from the builder check" and "Shared by all four slices" sections).

## Global Constraints

- No new dependencies; Rust only. TDD: every behaviour has a test seen failing first.
- Sizes are powers of 1024 (`K M G T P`, case-insensitive, optional trailing `B`/`iB`), decimals allowed (`1.5T`); `none` removes a limit. `zfs list -p` prints an unset limit as `0` (or `-` on volumes): `0`/`-` mean "no limit" in data, `-` in display.
- Confirm tier: two-key (`Modal::Confirm`, y then Enter) when the new limit leaves < 10% headroom over current use, or a reservation rises by more than 10% of the dataset's `available`; otherwise the edit applies directly like other property edits. Removing a limit applies directly.
- Thresholds: `[dashboard] quota_warn` (default 90) and `quota_crit` (98), validated like `swap_warn/crit` (`0 < warn < crit <= 100`, else defaults).
- `storage.json` cadence 10 minutes; a host that is unreachable keeps no stale lines (Dashboard filters to reached hosts, as for smart/shares).
- Validation uses the **selected host's** runner data (the property list already comes from it).
- Volumes (`type volume`): only `refreservation` is offered; the other three show "not offered for volumes".

## Review Focus

- Unset-limit encodings: `0`, `-`, `none`, empty field; a quota below `used` but above `referenced` (snapshots count toward `quota`, not `refquota`).
- Only `refquota` set (no `quota`): column, panel percentage and Dashboard line must use `referenced/refquota`.
- Size text edge cases: `1.5T`, `10g`, `10GB`, `0`, `-5`, `1e3`, `99999999999P` (overflow), trailing spaces, empty.
- Tier boundary exactly at 10% headroom; reservation raise equal to 10% of `available`.
- A dataset destroyed or its quota removed between scans: the line must disappear on the next scan; an unreachable host must show no quota lines.

---

### Task 1: `zfs::quota` pure module

**Files:** Create `src/zfs/quota.rs`; Modify `src/zfs/mod.rs` (`pub mod quota;`).

**Interfaces:**
- Produces:
  - `pub const KEYS: [&str; 4] = ["quota", "refquota", "reservation", "refreservation"];`
  - `pub fn parse_size(text: &str) -> Result<Option<u64>, String>` (`none`/`off`/empty-after-trim is an error for empty; `none` -> `Ok(None)`).
  - `pub fn format_size(bytes: u64) -> String` (reuse `zfs::disk::human_size`, then the form re-parses it; keep exact bytes when not a clean unit: return `"{bytes}"`).
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)] pub struct Facts { pub used: u64, pub referenced: u64, pub available: u64, pub quota: u64, pub refquota: u64, pub reservation: u64, pub refreservation: u64, pub is_volume: bool }` (0 = unset)
  - `#[derive(Debug, PartialEq, Eq)] pub enum Tier { Direct, TwoKey }`
  - `pub fn check(key: &str, new: Option<u64>, facts: &Facts) -> Result<Tier, String>` implementing the spec's refusals (quota<used; refquota<referenced; reservation>quota; refreservation>refquota; refreservation>quota; raise > available; volume keys) and the tier rule.
  - `#[derive(Debug, Clone, Copy, PartialEq)] pub struct Use { pub percent: f64, pub limit: u64, pub is_ref: bool }` and `pub fn usage(f: &Facts) -> Option<Use>`: the larger of `used/quota` and `referenced/refquota`; `None` when neither is set.

- [ ] **Step 1: Failing tests** in the module: `parse_size_accepts_units_decimals_none_and_refuses_junk` (`"10G"`, `"10g"`, `"1.5T"`=1_649_267_441_664, `"512M"`, `"10GB"`, `"10GiB"`, `" 5K "`, `"none"` -> `Ok(None)`; `""`, `"-5"`, `"1e3"`, `"G"`, `"1.2.3G"`, `"99999999999P"` -> `Err`); `check_refuses_a_quota_below_use_and_a_refquota_below_referenced` (used 5G referenced 3G: quota 4G -> Err, refquota 2G -> Err, quota 6G ok); `check_refuses_reservations_above_their_limits` (reservation>quota, refreservation>refquota, refreservation>quota); `check_refuses_a_reservation_raise_beyond_available` (increase compared, not the full value: old reservation 8G, new 10G, available 1G -> Err; available 3G -> Ok); `volumes_take_only_refreservation`; `tier_is_two_key_under_ten_percent_headroom_or_a_big_reservation_raise` (quota exactly 110% of used -> Direct; 109% -> TwoKey; none -> Direct; reservation raise exactly 10% of available -> Direct, above -> TwoKey); `usage_is_the_closer_limit` (quota 10G used 9.6G -> 96% non-ref; only refquota 4G referenced 3G -> 75% is_ref; none -> None; both: picks larger).
- [ ] **Step 2:** `cargo test --offline quota` -> FAIL (module missing).
- [ ] **Step 3:** Implement the signatures above in `src/zfs/quota.rs` (decimals via integer math on a split at `.`; overflow with `checked_mul`; tier uses `u128` arithmetic).
- [ ] **Step 4:** `cargo test --offline` -> all pass.
- [ ] **Step 5:** `git add -A && git commit -m "feat: quota arithmetic: sizes, refusals, confirm tier and usage"`.

### Task 2: Guided quota/reservation form

**Files:** Modify `src/ui/state.rs` (Enter on a dataset property row: prefill, volumes), `src/ui/state/modal_keys.rs` (`submit_form` arm), `src/ui/state/forms.rs` (a `quota_facts(&[Property], is_volume)` helper + prompt text), `src/zfs/property.rs` (describe text already exists), tests in `src/ui/state/tests.rs`.

**Interfaces:**
- Consumes: `zfs::quota::{KEYS, parse_size, format_size, check, Facts, Tier}`, `App.dataset_properties: Vec<Property>` (values are `-p` bytes; `type` property says `volume`/`filesystem`).
- Produces: `pub(super) fn quota_facts(props: &[Property]) -> Facts` in `forms.rs`; `App::submit_quota_form(&mut self, form: FormParts) -> Option<PendingAction>` in `modal_keys.rs`, reached from `submit_form` when `form.kind` is `EditDatasetProperty { key, .. }` with `key` in `KEYS`.

- [ ] **Step 1: Failing tests:** `enter_on_a_quota_row_prefills_a_human_size_and_none_for_unset` (value `10737418240` -> `10G`; `0` -> `none`); `a_quota_below_use_keeps_the_form_open_with_our_message` (modal stays a Form, `status`/error text mentions "uses"); `a_safe_quota_change_applies_directly` (returns `Some(PendingAction{ call: SetDatasetProperty{ key:"quota", value:"21474836480" } })` — value sent as exact bytes, `none` sent as `none`); `a_quota_with_under_ten_percent_headroom_asks_y_then_enter` (`Modal::Confirm` whose prompt shows `zfs set quota=… <dataset>` and the headroom); `a_volume_refuses_quota_and_offers_refreservation` (`Enter` on `quota` of a volume sets a status message "not offered for volumes"); `the_free_text_route_is_validated_too` (the same key through the generic form cannot bypass: it IS the same form).
- [ ] **Step 2:** run -> FAIL. **Step 3:** implement; a size field label shows "Size (10G, 500M, none)". **Step 4:** full suite passes. **Step 5:** commit.

### Task 3: Quota in the Datasets list and panel

**Files:** Modify `src/zfs/dataset.rs` (list command gets `quota,refquota` appended; `Dataset` gets `quota: u64, refquota: u64`, `referenced: u64` if absent; parser tolerant of 7 fields for old fixtures), `src/ui/layout.rs` (Quota column dropping first on narrow terminals), `src/ui/metrics.rs` (Quota line shows `quota` and `refquota`; Used line judged by `quota::usage`), existing fixtures/tests.

**Interfaces:** Consumes `quota::usage`, `human_size`. Produces: `Dataset.quota/refquota` (0 = none); column text `"96% of 10G"` (or `"75% of 4G ref"`), `-` when none.

- [ ] **Step 1: Failing tests:** `dataset_list_command_asks_for_quota_and_refquota` (exact args vec), `parse_dataset_list_reads_quota_columns_and_treats_dash_and_zero_as_none`, `the_datasets_table_shows_how_full_a_quota_is_and_drops_the_column_when_narrow` (render), `the_panel_used_line_follows_the_quota` (96% not 4%), `the_panel_quota_line_shows_refquota_too`.
- [ ] **Step 2-5:** red, implement, suite green, commit.

### Task 4: `storage_check` module

**Files:** Create `src/daemon/storage_check.rs`; Modify `src/daemon/mod.rs` (`pub mod storage_check;`).

**Interfaces:**
- Produces: `StorageResult { checked_at, datasets: Vec<LimitedDataset>, error: Option<String> }` (`LimitedDataset { name, used, referenced, quota, refquota }`, only datasets that have a limit), `StorageFile { hosts: BTreeMap<String, StorageResult>, ran_at }` with `load/save` (reuse `smart_check::write_atomic`), `pub enum Outcome { Silent, Checked(StorageResult), Failed(String) }`, `pub fn check_host(runner: &dyn CommandRunner, now: DateTime<Utc>) -> Outcome` using `zfs list -H -p -t filesystem,volume -o name,used,referenced,quota,refquota,type` (sudo-refused -> `Silent`; status 255/runner error -> `Failed`), `pub struct StorageProblem { host, severity: Severity, text, checked_at }`, `pub fn problems(file: &StorageFile, quota: (u8, u8)) -> Vec<StorageProblem>` (one per host: the worst dataset; text `tank/home 94% of its 100G quota`, plus ` (+2 more)` when others are also over warn).
- [ ] **Step 1: Failing tests:** parse with real-shaped lines (unset `0`, volume `-`); `a_dataset_at_94_percent_is_watch_and_at_98_is_issue`; `refquota_only_uses_referenced`; `only_the_worst_per_host_with_a_count_of_the_rest`; `unlimited_datasets_are_not_stored`; `sudo_refusal_is_silent_and_ssh_loss_is_failed`; `a_new_answer_replaces_the_hosts_old_lines` (merge function `merge(file, host, outcome)`).
- [ ] **Step 2-5:** red, implement, green, commit.

### Task 5: Thresholds, loop, `c`, Dashboard and report

**Files:** Modify `src/config.rs` (`DashboardConfig.quota_warn/quota_crit` via `lenient`, `Thresholds.quota: (u8,u8)` default `(90,98)`, `resolve`), `src/ui/settings.rs` and `src/config_edit.rs` (a `Setting::Quota` pair row "Quota warn / crit" like Swap), `src/daemon/mod.rs` (`storage_loop` every 600 s writing `storage.json`, log line on problems change only), the TUI's manual check (`c` in Hosts: where it calls `run_pass`/`check_host`, also run `storage_check` and merge), `src/ui/dashboard.rs` (`Inputs.storage: &StorageFile`; items in `smart_items` style, `goto: Section::Datasets`), `src/ui/state.rs` (`reload_check_files` loads `storage.json`), `src/daemon/health_text.rs` (report lists the same lines).

- [ ] **Step 1: Failing tests:** `quota_thresholds_default_validate_and_fall_back` (config), `the_settings_screen_edits_quota_warn_and_crit` (build_changes, refuses warn>=crit), `a_quota_problem_is_a_needs_attention_item_that_goes_to_datasets`, `no_item_for_an_unreached_host`, `the_daily_report_lists_the_quota_line`, `the_storage_loop_writes_storage_json_for_every_host` (use `Shared`/hosts test pattern of `checks_loop` tests if present, else call a `run_storage_pass(dir, hosts, now)` function directly), `c_in_hosts_also_refreshes_storage` (existing `c` test pattern).
- [ ] **Step 2-5:** red, implement, green, commit. Then deploy to the hub for Task 6.

### Task 6: Docs, live check, review, merge

- [ ] **Step 1:** Update book chapters 09 (Datasets: the guided edit, the column, the Dashboard line), 06 (Dashboard), 21 (reference config: `quota_warn`, `quota_crit`) and 20 if keys changed, README; regenerate affected captures with `cargo run --example capture`.
- [ ] **Step 2: Live check** (hub build via the usual tar+cargo, a VM `fbsd16t1` scratch pool): set a 1G quota on a scratch dataset through the TUI, fill it past 90% (`dd`) and see the Dashboard line and the column, over 98% red; remove the quota and see the line go within one scan (or `c`); try the refusals (quota below use; reservation above quota; volume). Verify empirically on the VM which pairs ZFS itself rejects and record it in the spec if different.
- [ ] **Step 3:** Whole-branch subagent review with the Review Focus list; fix Critical/Important test-first.
- [ ] **Step 4:** Ask the user before merge + install; then update memory.

- NOTE (task 3): the spec/plan claimed the panel Used line ignores quota; wrong: ZFS `available` already reflects quota, so Used% is right. Only the Quota line gained percentages.
