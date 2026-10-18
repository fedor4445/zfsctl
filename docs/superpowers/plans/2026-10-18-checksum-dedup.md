# Checksum and deduplication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans (native execution chosen by the user). Checkbox steps.

**Goal:** `checksum` and `dedup` edited with real value lists and the right guards (typed confirms for `checksum=off` and `dedup` on, a plain confirm with the "new writes only" note for other changes), shown in the Datasets panel when not default, and watched on the Dashboard (checksum off, dedup that saves nothing).

**Architecture:** A pure module `zfs::integrity` (value tables, OpenZFS version gating, which confirm a change needs). The dataset Properties `Enter` form for the two keys is validated in `submit_form` (same path as the free-text form: it is the same form). The storage check (slice 1) gains the host's `zfs version` and an `integrity` reading (`zfs get -s local,received`, `zpool list -o name,allocated,dedupratio`); `problems` and the Dashboard/report get two new line kinds.

**Tech Stack:** Rust; `zfs get/set/version`, `zpool list` (options before positionals for OmniOS); no new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-18-checksum-dedup-design.md` (+ its decisions and "Decided by the user").

## Global Constraints

- TDD; no new dependencies; Rust only; core sudoers; three OSes.
- `checksum=off` and turning `dedup` on: `Modal::ConfirmTyped` (type the dataset name). Any other checksum change, and dedup off or a dedup algorithm change on a dataset that already has dedup on: `Modal::Confirm` (y then Enter) with the "only data written afterwards" note and the scrub reminder for checksum.
- Values: checksum `on, off, fletcher2, fletcher4, sha256, sha512, skein, edonr, blake3` (+ `noparity` is NOT offered); dedup `off, on, verify, sha256, sha256,verify, sha512, sha512,verify, skein, skein,verify, edonr,verify, blake3, blake3,verify`. `edonr` for dedup only as `edonr,verify`.
- Version gate: OpenZFS >= 2.2 for `blake3`; `skein`/`edonr` >= 0.8 (always on 2.x). **Unknown version (no `zfs version`, as on illumos, or no storage reading yet) gates nothing**: the host's own error stands.
- Rulings: (1) F2 pickers list the whole vocabulary and the version gate is applied at submit, with a clear message; (2) the dedup confirm does not read `zpool status -D` (table size): it states the RAM rule of thumb and the host's physical RAM instead; (3) the Dashboard ignores a volume named `<pool>/dump` (illumos's dump device has `checksum=off` by design, seen on omnios); (4) `dedup_min_ratio` is stored as hundredths (`110` = 1.10x) in `Thresholds`.
- Dashboard rules: dedup line per pool only when some dataset has `dedup` set locally/received to anything but `off`, the pool has more than 10 GiB allocated and `dedupratio` < `dedup_min_ratio` (default 1.10): `tank dedup is on but saves only 1.02x`; checksum line per host naming up to three datasets and a count: `tank/scratch checksum is off` / `tank/a, tank/b, tank/c checksum is off (+2 more)`. Both Watch, go to Datasets.

## Review Focus

- Value parsing: case, spaces, `sha256, verify`, `SHA256,VERIFY`, empty, `none`, `fletcher4,verify`, `verify,sha256`.
- Version strings: `2.2.2-0ubuntu9.5`, `2.4.2-1-FreeBSD_g6330a45b0`, `zfs-2.1.14-1`, garbage, empty, `2.10.0` vs `2.2` ordering.
- Confirm choice for transitions: checksum `off` -> `on`; dedup `on` -> `verify`; dedup `off` -> `off` (no change); setting the value a dataset already has.
- Dashboard false alarms: inherited values, `rpool/dump`, a pool with dedup just switched on (allocated under 10 GiB), `dedupratio` formats (`1.00`, `1.00x`, `-`).
- A dataset renamed/destroyed between scans; hosts without `zfs version`.

---

### Task 1: `zfs::integrity` pure module

**Files:** Create `src/zfs/integrity.rs`; Modify `src/zfs/mod.rs`.

**Interfaces — Produces:**
- `pub const CHECKSUMS: [&str; 9]`, `pub const DEDUPS: [&str; 12]` (the vocabularies above, in display order).
- `pub fn openzfs_version(text: &str) -> Option<(u32, u32)>`: major.minor from `2.2.2-0ubuntu9.5`, `2.4.2-1-FreeBSD_g...`, `zfs-2.1.14-1` (a leading `zfs-` is stripped); `None` for anything else.
- `pub fn check_checksum(value: &str, version: Option<&str>) -> Result<String, String>`: normalized value (trim, lower-case) when it is in `CHECKSUMS` and the version (if known) allows it; Err with the reason otherwise.
- `pub fn check_dedup(value: &str, version: Option<&str>) -> Result<String, String>`: normalized (`sha256,verify`: spaces around the comma removed) when in `DEDUPS` and allowed.
- `#[derive(Debug, PartialEq, Eq)] pub enum Confirm { Typed, Plain }` and `pub fn checksum_confirm(new: &str) -> Confirm` (`off` -> Typed else Plain), `pub fn dedup_confirm(old: &str, new: &str) -> Confirm` (`old == "off"` and `new != "off"` -> Typed; everything else Plain).

- [ ] **Step 1: Failing tests:** `versions_are_read_from_the_real_strings` (the strings in Review Focus; `2.10.0` > `2.2` via tuple compare); `checksum_values_are_normalized_and_blake3_needs_2_2` (`" SHA256 "` -> `sha256`; `blake3` with `Some("2.1.14-1")` Err mentioning 2.2; with `Some("2.2.2")`/`None` Ok; `fletcher3`/`""`/`none` Err); `dedup_values_normalize_the_comma_and_edonr_needs_verify` (`SHA256, Verify` -> `sha256,verify`; `edonr` Err, `edonr,verify` Ok; `verify,sha256` Err); `which_changes_need_the_typed_confirm` (`checksum_confirm("off")==Typed`, `("sha256")==Plain`; `dedup_confirm("off","on")==Typed`, `("on","verify")==Plain`, `("on","off")==Plain`, `("off","off")==Plain`).
- [ ] **Step 2-5:** red, implement, suite, commit.

### Task 2: the guarded form for `checksum` and `dedup`

**Files:** `src/ui/state.rs` (Enter handler: for the two keys, label with the vocabulary and a description; `dataset_properties_for` unchanged), `src/ui/state/modal_keys.rs` (`submit_integrity_form`, arm in `submit_form` next to the quota arms), `src/ui/picker.rs` (`vocabulary_for`: `EditDatasetProperty { key: "checksum"|"dedup" }` field 0 -> the vocabulary), `src/ui/state/forms.rs` (prompt texts).

**Interfaces:** Consumes `integrity::*`, `self.storage_check.hosts[host_name].zfs_version`, `self.host_resources(name)` (`physical`), `Modal::ConfirmTyped { prompt, expected, input, action }`. Produces `App::submit_integrity_form(&mut self, form: FormParts) -> Option<PendingAction>`.

- [ ] **Step 1: Failing tests:** `enter_on_checksum_and_dedup_rows_open_a_form_that_lists_the_values` (label contains `fletcher4`/`blake3`; `F2` picker offers the vocabulary: `vocabulary_for`); `an_unknown_value_keeps_the_form_open_with_the_list` (`sha3`); `blake3_on_a_host_known_to_run_2_1_is_refused_with_the_version` (put a `StorageResult { zfs_version: Some("2.1.14") }` into `app.storage_check`); `checksum_off_asks_for_the_dataset_name_and_says_what_it_means` (ConfirmTyped, `expected == "tank/data"`, prompt mentions "silent corruption" and "new writes"); `another_checksum_asks_y_enter_with_the_scrub_reminder`; `dedup_on_asks_for_the_dataset_name_and_warns_about_ram` (prompt contains the host's RAM when known, "cannot be undone"/"does not undeduplicate", and a low-RAM warning under 16 GiB); `dedup_off_is_a_plain_confirm`; `setting_the_value_it_already_has_says_so_and_sends_nothing`; `the_exact_normalized_value_is_sent` (`SHA256, VERIFY` -> `sha256,verify`).
- [ ] **Step 2-5:** red, implement, green, commit.

### Task 3: Panel: Checksum and Dedup when not default

**Files:** `src/zfs/metrics.rs` (`DatasetSpace.checksum: String`, `dedup: String`; added to `DATASET_SPACE_PROPS`; parsed), `src/ui/metrics.rs` (`dataset_lines`: a seventh line only when `checksum` is not `on`/`fletcher4`/empty or `dedup` is not `off`/empty), fixtures.

- [ ] **Step 1: Failing tests:** `parse_dataset_space_reads_checksum_and_dedup`; `the_panel_adds_a_line_only_for_a_non_default_checksum_or_dedup` (`Checks  sha256   Dedup on`; a red style for `off`).
- [ ] **Step 2-5:** red, implement, green, commit.

### Task 4: The storage reading: version and integrity facts

**Files:** `src/daemon/storage_check.rs`: `StorageResult.zfs_version: Option<String>` and `integrity: Integrity { checksum_off: Vec<String>, dedup_on: Vec<String> /* pools with a dataset that has dedup on */, pools: Vec<PoolSpace { name, allocated, ratio_x100 }> }` (all `#[serde(default)]`); `check_host` runs, best effort each: `zfs version` (first line, `zfs-` stripped), `zfs get -H -p -s local,received -t filesystem,volume -o name,property,value checksum,dedup`, `zpool list -H -p -o name,allocated,dedupratio`.

- [ ] **Step 1: Failing tests:** `integrity_facts_are_read_from_real_shaped_output` (checksum off datasets listed; `rpool/dump` skipped; dedup_on holds the pool of each dataset with dedup `on`/`verify`/`sha256,verify` but not `off`; ratios `1.00` -> 100, `1.02x` -> 102, `-` -> skipped); `the_version_is_the_first_line_without_the_prefix`; `each_command_may_fail_alone` (other facts survive); the argument vectors have options before positionals.
- [ ] **Step 2-5:** red, implement, green, commit.

### Task 5: Dashboard lines, report and the `dedup_min_ratio` setting

**Files:** `src/daemon/storage_check.rs` (`integrity_problems(file, min_ratio_x100) -> Vec<StorageProblem>`), `src/config.rs` (`DashboardConfig.dedup_min_ratio: Option<f64>` lenient; `Thresholds.dedup_ratio: u16` default 110; valid 1.00..=10.00 else default), `src/ui/settings.rs` (`Setting::DedupRatio`, single value, label "Dedup warn below (x)", shown `1.10x`), `src/ui/dashboard.rs`, `src/daemon/health_text.rs`, book config reference (`docs/book/21-reference-config.md`).

- [ ] **Step 1: Failing tests:** `a_pool_with_dedup_on_that_saves_nothing_is_one_watch_line` (alloc 20 GiB, ratio 102 -> line; ratio 150 -> none; alloc 5 GiB -> none; no dedup dataset -> none), `checksum_off_names_up_to_three_datasets_and_counts_the_rest`, `the_dashboard_lists_them_for_reached_hosts_and_goes_to_datasets`, `the_report_lists_them`, `dedup_ratio_defaults_validates_and_is_a_setting` (`1.05` ok, `0.5`/`11`/`abc` refused, default removes the key), book key drift test passes.
- [ ] **Step 2-5:** red, implement, green, commit; deploy to the hub.

### Task 6: Docs, live check, review, merge

- [ ] **Step 1:** Book ch. 09 (a "Checksum and deduplication" section), 06 Dashboard lines, 21 reference, README.
- [ ] **Step 2: Live (hub):** scratch dataset `tpool/ck`: `Enter` on `checksum` (see the list; `blake3` ok on 2.4), set `sha256` (plain confirm + scrub reminder), set `off` (typed name; wrong name refused), set `dedup=on` (typed; RAM text), `c` in Hosts -> Dashboard shows `tpool/ck checksum is off`; set it back -> line gone after `c`; `dedup=off`; destroy the dataset. On `omnios` check that `rpool/dump` does not raise a line.
- [ ] **Step 3:** Whole-branch subagent review (Review Focus); fix Critical/Important test-first.
- [ ] **Step 4:** Ask the user before merge + install; update memory.
