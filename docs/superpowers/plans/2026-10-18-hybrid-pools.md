# Hybrid pools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans (native execution chosen by the user). Checkbox steps.

**Goal:** Add disks to a pool as `log` (SLOG), `cache` (L2ARC), `special`, `dedup` or `spare` with the dangerous mistakes refused, remove log/cache/spare again, tune `special_small_blocks`, and put Dashboard lines on special/dedup vdevs without redundancy and on faulted log/cache devices.

**Architecture:** A pure module `zfs::vdev_role` (roles, spec building and refusals for both the disk-selection form and the raw-spec form). The Add vdev forms gain a Role field and go through a background `zpool add -n` dry run whose output is shown in the confirm. `MutationCall::RemoveVdev` for log/cache/spare. The existing `storage_check` (slice 1) gains per-pool device-class facts from `zpool status`.

**Tech Stack:** Rust, ratatui; `zpool add/remove/status`, `zfs set` only (core sudoers; three OSes; options before positionals for OmniOS).

**Spec:** `docs/superpowers/specs/2026-10-18-hybrid-pools-design.md` (with "Decisions from the builder check" and "Shared by all four slices").

## Global Constraints

- No new dependencies; Rust only; TDD (test seen failing first).
- Spec words: `zpool add [-n] pool [log|cache|special|dedup|spare] [mirror|raidzN] disks…`.
- Rules: `special`/`dedup`: one mirror (default) or raidz over the selection, one disk refused; `log`: stripe or one mirror; `cache`/`spare`: one device per disk, no redundancy choice; `data`: today's behaviour.
- Confirm tiers: `data`/`special`/`dedup` = irreversible two-key text ("cannot be removed again"); `log`/`cache`/`spare` = plain y/n confirm. Both show the exact command and the `zpool add -n` layout. Dry-run failure: error shown verbatim, form stays open.
- Remove offered only for: a top-level log device or log mirror, a single cache leaf, a single spare leaf.
- Dashboard: `special`/`dedup` top-level `disk` leaf (not under `mirror-N`/`raidz*`) = Issue; log `FAULTED|UNAVAIL`, cache `FAULTED|UNAVAIL|REMOVED` = Watch; lines go to Pools.
- Rulings: (1) per-class sizes from `zpool list -v` are **not built**: the Pools panel already shows `Disks 10 (6 data, 2 log, 1 cache, 1 spare)` from `parse_pool_layout` and the vdev drill-down already shows the class headings, so the roles are visible; (2) no refusal for `special_small_blocks` when the pool has no special vdev (ZFS accepts it; the form warns in its description); (3) the Disks Membership column keeps `pool/group` as is (a role label there is a later nicety).

## Review Focus

- Raw spec edge cases: `log`, `special mirror da0` (one disk under mirror), `special da0` (single), `special mirror da0 da1 mirror da2 da3` (two groups), `log log da0`, mixed role words, `-f` flags, paths with `/`.
- A role word that is also a disk name; file-backed vdevs (`/root/f1`) are valid disks in `zpool add`.
- Dry-run result arriving after the user closed the form or changed host; two dry runs in flight; the confirm acting on the wrong host.
- `zpool status` layouts: pool with no classes, `spares` section, `special` with a raidz, a `replacing-0` inside a special mirror, disk leaf names containing "mirror".
- Remove: a log mirror's leaves must not be removable (detach is out of scope); cache/spare leaves only at depth 1.
- Illumos: `zpool add -n` and `zpool remove` syntax; special on OmniOS.

---

### Task 1: `zfs::vdev_role` pure module

**Files:** Create `src/zfs/vdev_role.rs`; Modify `src/zfs/mod.rs`.

**Interfaces — Produces:**
- `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Role { Data, Log, Cache, Special, Dedup, Spare }` with `pub const ALL: [Role; 6]`, `pub fn word(self) -> &'static str` (`data`,`log`,`cache`,`special`,`dedup`,`spare`), `pub fn parse(&str) -> Option<Role>`, `pub fn removable(self) -> bool` (log/cache/spare), `pub fn irreversible(self) -> bool` (data/special/dedup).
- `pub fn build_spec(role: Role, redundancy: &str, disks: &[String]) -> Result<Vec<String>, String>`: `redundancy` is `""|mirror|raidz1|raidz2|raidz3`. Data: as `build_vdev_spec_from_selection` today (blank = stripe). Log: stripe (blank) or `log mirror d…`; raidz refused. Cache/Spare: `cache d1 d2…` (redundancy must be blank or ignored with an error if non-blank). Special/Dedup: blank defaults to `mirror`; one disk refused with the reason; `special mirror d…` or `special raidzN d…`. Mirror needs >= 2 disks, raidz1 >= 2 (3 recommended, >=2 accepted), raidz2 >= 3, raidz3 >= 4, else Err.
- `pub fn check_raw_spec(spec: &[String]) -> Result<Role, String>`: finds role words in a raw spec and applies the same refusals: a `special`/`dedup` segment whose group is a single bare disk (no `mirror`/`raidz*` keyword before the disks) is refused; a mixed-role spec is accepted only when each segment passes; `log`/`cache`/`spare` segments with raidz refused. Returns the riskiest role found (Special/Dedup > Data > Log > Spare > Cache) for the confirm tier. Flags (`-f`, `-o…`) are skipped; an empty spec is Err.

- [ ] **Step 1: Failing tests** (names and assertions): `build_spec_for_each_role` (`build_spec(Log,"mirror",[a,b])==["log","mirror","a","b"]`; `(Cache,"",[a,b])==["cache","a","b"]`; `(Spare,"",[a])==["spare","a"]`; `(Special,"",[a,b])==["special","mirror","a","b"]`; `(Dedup,"raidz1",[a,b,c])==["dedup","raidz1","a","b","c"]`; `(Data,"mirror",[a,b])==["mirror","a","b"]`; `(Data,"",[a,b])==["a","b"]`); `special_and_dedup_refuse_a_single_disk_with_the_pool_loss_reason` (Err contains "pool"); `mirror_and_raidz_need_enough_disks`; `cache_and_log_refuse_what_they_cannot_be` (`(Cache,"mirror",…)` Err; `(Log,"raidz1",…)` Err); `raw_spec_checks` (the Review Focus cases: `["special","da0"]` Err, `["special","mirror","da0","da1"]` Ok(Special), `["log","da0"]` Ok(Log), `["cache","da0","da1"]` Ok(Cache), `["special","mirror","da0"]` Err (mirror of one), `["-f","mirror","da0","da1"]` Ok(Data), `["spare","da9"]` Ok(Spare), `[]` Err, `["special","mirror","a","b","dedup","c"]` Err (dedup single)); `role_words_round_trip_and_tiers`.
- [ ] **Step 2:** run -> FAIL. **Step 3:** implement. **Step 4:** full suite. **Step 5:** commit "feat: vdev roles: spec building and refusals".

### Task 2: Role in the Add-vdev-from-disks form with a `zpool add -n` dry run

**Files:** Modify `src/ui/modal.rs` (`FormKind::AddVdevFromDisks` unchanged; fields gain Role at index 1), `src/ui/state.rs` (the `a` handler: fields `[Target pool, Role (data, log, cache, special, dedup, spare), Redundancy]`; `pending_add_checks`, `WorkerMsg::AddChecked`), `src/ui/state/forms.rs` (`build_pending_action` for `AddVdevFromDisks` uses `vdev_role::build_spec`; a `form_problem` message for refusals), `src/ui/state/modal_keys.rs` (`submit_add_vdev_form`), `src/ui/state/worker.rs` (`on_add_checked`), `src/ui/picker.rs` (`vocabulary_for`: field 1 = roles, field 2 = REDUNDANCY), `src/ui/mod.rs` event loop (drain `pending_add_checks` with a runner like `pending_host_checks`), `src/zfs/pool.rs` (`add_vdev_dry_run(runner, pool, spec) -> Result<String>` running `zpool add -n -- pool spec…`, options before positionals).

**Interfaces:** Consumes `vdev_role::{Role, build_spec}`. Produces `pub struct AddCheck { host: usize, pool: String, spec: Vec<String>, role: Role, form: Modal }`, `App::start_add_check(&mut self, check: AddCheck, runner: Arc<dyn CommandRunner>)`, `WorkerMsg::AddChecked(AddCheck, Result<String, String>)`.

- [ ] **Step 1: Failing tests:** `the_add_vdev_form_has_a_role_field_with_a_picker` (fields `["Target pool name","Role (…)","Redundancy (…)"]`, default role `data`); `a_single_disk_special_is_refused_before_anything_runs` (Enter through the form -> form stays open with the reason, `pending_add_checks` empty); `a_valid_log_add_queues_a_dry_run` (queue holds spec `["log","da4"]`); `the_dry_run_result_becomes_a_plain_confirm_for_log_and_a_two_key_text_for_special` (feed `WorkerMsg::AddChecked(..., Ok("would update…"))`: Confirm prompt contains `zpool add tank log da4`, the layout text, and for special the "cannot be removed again" sentence, for log not); `a_failed_dry_run_reopens_the_form_with_the_message_verbatim`; `add_vdev_dry_run_calls_zpool_add_dash_n_with_options_first` (pool.rs: args `["add","-n","--","tank","log","da4"]`).
- [ ] **Step 2-5:** red, implement, suite green, commit.

### Task 3: The raw Add-vdev form (Pools -> vdevs) uses the same checks

**Files:** `src/ui/state/modal_keys.rs`, `forms.rs`.
**Interfaces:** Consumes `vdev_role::check_raw_spec`.

- [ ] **Step 1: Failing tests:** `the_raw_add_form_refuses_a_single_disk_special` (message mentions the pool is lost); `the_raw_add_form_goes_through_the_dry_run_too` (queues an `AddCheck` with the role from `check_raw_spec`); `a_plain_data_spec_behaves_as_before_but_with_the_dry_run` (existing test `add_vdev_*` updated: confirm text still shows `zpool add tank mirror da2 da3`).
- [ ] **Step 2-5:** red, implement, green, commit.

### Task 4: Remove a log, cache or spare device

**Files:** `src/zfs/pool.rs` (`remove_vdev(runner, pool, device)`: `zpool remove -- pool device`), `src/ui/modal.rs` (`MutationCall::RemoveVdev { pool, device }`, tone Warn, name "REMOVE VDEV"), `src/ui/state.rs` (key `x` in the vdev drill-down, a plain y/n confirm showing the command), `src/zfs/pool.rs` (`pub fn removable_device(lines: &[VdevLine], index: usize) -> Option<String>`), hints.

**Interfaces:** `removable_device`: scans back from `index` to the nearest depth-0 heading to know the class (`logs`->log, `cache`, `spares`); returns the device name when depth == 1 and the class is log/cache/spare; `None` otherwise (data, special, dedup, any depth > 1 so a log mirror's leaves are not removable).

- [ ] **Step 1: Failing tests:** `removable_device_covers_log_cache_spare_top_level_only` (a real-shaped status with `logs` containing `mirror-1` and `da5`, `cache da6`, `spares da7`, a data mirror: indices of `mirror-1`, `da5`, `da6`, `da7` are `Some`, a leaf of `mirror-1` and any data device are `None`); `remove_vdev_runs_zpool_remove_with_options_first`; `x_on_a_removable_row_asks_y_n_with_the_command_and_sends_remove`; `x_on_a_data_device_says_it_is_not_offered`; the hint bar lists `x Remove` only on a removable row.
- [ ] **Step 2-5:** red, implement, green, commit.

### Task 5: Guided `special_small_blocks`

**Files:** `src/ui/state.rs` (Enter handler: key `special_small_blocks`), `modal_keys.rs` (`submit_special_blocks_form`), `src/zfs/vdev_role.rs` (`pub fn parse_small_blocks(text) -> Result<u64, String>`: `0` or a power of two from 512 to 16M, sizes via `quota::parse_size`).

- [ ] **Step 1: Failing tests:** `parse_small_blocks_takes_zero_or_a_power_of_two_in_range` (`0`,`512`,`4K`,`32K`,`1M` ok; `3K`,`100`,`32M`,`none`,`` Err); `the_form_sends_the_exact_bytes_after_a_confirm_that_warns_when_the_value_reaches_the_recordsize` (recordsize 128K, value 128K: prompt contains "all of this dataset's data" ; 32K does not).
- [ ] **Step 2-5:** red, implement, green, commit.

### Task 6: Pool device-class facts and Dashboard lines

**Files:** `src/zfs/pool_info.rs` or a new `src/zfs/pool_classes.rs` (`parse_classes(status_all: &str) -> Vec<PoolClasses>`), `src/daemon/storage_check.rs` (`StorageResult.pools: Vec<PoolClasses>` serde default; `check_host` also runs `zpool status` best-effort; `pool_problems(file) -> Vec<StorageProblem>`), `src/ui/dashboard.rs` (items, goto Pools), `src/daemon/health_text.rs` (report lines).

**Interfaces:** `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct PoolClasses { pub name: String, pub unprotected: Vec<String> /* "special","dedup" with a lone disk */, pub log_bad: bool, pub cache_bad: bool }`.

- [ ] **Step 1: Failing tests:** `parse_classes_reads_a_hybrid_pool` (a real-shaped two-pool `zpool status` text: pool A with mirrored special, a faulted log disk and a healthy cache; pool B with a lone special disk and a lone dedup disk), `a_lone_special_disk_is_an_issue_and_a_mirrored_one_is_not`, `a_faulted_log_or_cache_is_a_watch`, `a_pool_without_classes_gives_nothing`, `check_host_keeps_pool_facts_and_survives_a_failing_zpool_status`, `the_dashboard_lists_them_for_reached_hosts_and_goes_to_pools`, `the_daily_report_lists_them`.
- [ ] **Step 2-5:** red, implement, green, commit. Deploy to the hub.

### Task 7: Docs, live check, review, merge

- [ ] **Step 1:** Book: 08 Pools (roles, Add vdev with Role, dry run, remove), 11 Disks (`a` with a role), 06 Dashboard (the new lines), README; update the book's "Not built yet" boxes if any mention log/cache.
- [ ] **Step 2: Live check on the hub** with file-backed vdevs (`mkfile`/`truncate` 128M files under `/root/hyb/`): create pool `hyb` on `f1`; via the TUI Pools -> vdev drill-down -> add raw `log /root/hyb/f2`, `cache /root/hyb/f3`, `spare /root/hyb/f4`, `special mirror /root/hyb/f5 /root/hyb/f6`; try `special /root/hyb/f7` (refused); see the Pools panel counts; remove the log, cache and spare with `x`; check the Dashboard line by creating a pool with a lone special disk (`zpool create -f hyb2 f8 special f9`) and `c` in Hosts; clean up (`zpool destroy hyb hyb2`, delete files). Verify empirically on `omnios` (`zpool add -n` with `log`/`cache`) and record anything that differs.
- [ ] **Step 3:** Whole-branch subagent review with the Review Focus list; fix Critical/Important test-first.
- [ ] **Step 4:** Ask the user before merge + install; update memory.

## Review fixes (2026-10-18)

- A pool's own first line is never read as a class heading (`removable_device`, `classes_of`, `parse_pool_layout`).
- The add confirm waits for the host's `zpool add -n` layout (Enter after y is held until it arrives; 30 s deadline gives the form back; each ask has an id); the layout is cut to the class being added, not the whole tree; multi-line refusals become one status line.
- Verified on the hub: a mirrored special or log on a raidz or mirror pool needs no `-f`; only a single-disk data pool ("pool uses file/disk and new vdev is mirror") needs `-f`, which the raw form takes as a leading `-f`.
- Raw specs: `-n` refused, bare `raidz` needs two disks, role words are case-sensitive like zpool's.
- Left as is: the Disks form still offers the Redundancy picker for cache/spare (refused on submit); `special_small_blocks` for a zvol does not use volblocksize in its warning.
