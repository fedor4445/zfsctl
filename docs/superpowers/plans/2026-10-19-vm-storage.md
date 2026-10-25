# VM storage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** zfsctl learns which ZFS storage belongs to which VM (vm-bhyve), refuses to damage a running VM's storage, creates a zvol for a VM, assigns a physical disk through a guarded form, shows the hypervisor in the Hosts table, and gets a VMs section.

**Architecture:** a new `src/vm/` module holds the model, the read-only vm-bhyve reader (a pure parser plus a `discover` that runs the commands) and the pure safety checks. The UI gets a `Section::Vms`, a dataset-to-VM map kept in `App`, refusals at key time and again at run time, and two forms. Everything privileged goes through the existing `exec::privilege` table.

**Tech Stack:** Rust, ratatui, the existing `CommandRunner` and `FakeRunner` (`exec::test_support`).

**Spec:** `docs/superpowers/specs/2026-10-19-vm-storage-design.md` (approved 2026-10-19, narrowed to FreeBSD and vm-bhyve only on 2026-10-25: the libvirt and OmniOS parts were removed from the spec and from this plan).

## Global Constraints

- Rust-only repo; the only non-Rust file stays `contrib/rc.d/zfsctl`.
- No root SSH; root only through sudoers, and every new privileged command is an entry in `exec::privilege::ENTRIES` (drift tests enforce the sudoers line, the book and `check-host`).
- zfsctl never creates, starts, stops, installs or deletes a VM, never hand-edits a manager's file, never wipes/partitions/formats a disk, never lets the user override a refusal.
- Two-key confirms (`y`, then Enter); the confirm shows the exact command; typed names for irreversible actions.
- Fail closed: "could not ask" is never read as "no VM uses this". Same for the disk checks.
- Secrets only on stdin (the encrypted-zvol option reuses the existing `create_encrypted` handling).
- Tests assert exact argument lists on the fake runner; a mutation run (`cargo mutants -f` on the new files) before the final review.
- Every command, file layout and output format of vm-bhyve is **unverified until Task 0**; no parser is written from memory.
- Commit trailer: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.

## Review Focus

- **Path aliases.** a vm-bhyve disk may be a dataset name or `/dev/zvol/tank/vm/web/disk0`; both must normalise to the dataset name `tank/vm/web/disk0` or the map misses the VM and the refusal never fires. Test each form.
- **Names with unusual characters.** A VM or dataset name with spaces, `@` or `%`; a zvol whose snapshot or clone is named in a config. Must not break parsing or let a refusal be skipped.
- **A VM with disks in several pools, or one disk used by two VMs.** The map holds all of them; the refusal names every running VM.
- **Manager installed but unreadable** (sudo refused, config unparsable, command times out). The refusals say "could not check whether a VM uses it" and refuse; the VMs section says why it cannot list.
- **A VM that starts between the picker and the confirm.** The checks run again immediately before the destructive or attach command, not only when the form opens.
- **An unrecognised state counts as running** (fail closed) for the refusals.
- **Snapshot of a running VM's zvol is allowed** (with the crash-consistent note); only destroy, recursive destroy, rollback and shrink are refused.

---

### Task 0: Verify the commands on real installs, and keep fixtures

**Files:**
- Create: `src/vm/fixtures/` (text fixtures: `vm_list.txt`, `vm_list_running.txt` (from documentation), `vm_conf_zvol.txt`, `vm_conf_custom_disk.txt`, `zdb_label_ok.txt`, `zdb_label_none.txt`)
- Create: `docs/superpowers/specs/2026-10-19-vm-storage-verified.md` (what was run, on which box, the exact output)

**Interfaces:** Produces: the fixtures every later parser test loads with `include_str!`, and the verified answers to the spec's "to verify" list.

This task changes test machines (installs packages, defines a VM that is never started). **Ask the user before touching any box**; nothing here starts a VM.

- [ ] **Step 1: FreeBSD (the `fbsd16t1` VM, a scratch pool).** `pkg install vm-bhyve`; initialise a datastore on a scratch zpool; `vm create -s 1G probe`; capture `vm list`, `vm info probe` (state, disk lines), the VM's `probe.conf` (note `disk0_type`, `disk0_name`, `disk0_dev`), then `vm add -d disk -t zvol -s 1G probe` and capture the new conf and the zvol it created. Record whether `vm list` needs sudo, the exact `vm add` form for an existing zvol and for a raw device (`disk0_dev=custom`), and what `vm list` shows for a guest that is not running. The Running line cannot be captured here (bhyve cannot start on a KVM guest): take it from the vm-bhyve documentation, quote it, and mark the fixture `from documentation`.
- [ ] **Step 2: Label read.** `zdb -l /dev/<disk>` on a pool member and on a blank disk of the hub or a VM; record exit codes and output for "has labels" and "none".
- [ ] **Step 3: Hypervisor name.** Record `sysctl -n kern.vm_guest` on a KVM guest and on the hub (what the probe already reads).
- [ ] **Step 4: Write `...-verified.md`** with each command, box, privilege needed and a one-line conclusion; copy outputs to the fixtures. Where reality differs from the spec, the verified file wins and the later tasks follow it; state each difference at the top of the file.
- [ ] **Step 5: Commit** `docs: verified VM manager commands and fixtures`.

### Task 1: The VM model and the vm-bhyve parser

**Files:**
- Create: `src/vm/mod.rs`, `src/vm/bhyve.rs`
- Modify: `src/main.rs` (`mod vm;`)
- Test: unit tests in each file, using the Task 0 fixtures

**Interfaces:** Produces (all in `crate::vm`):
```rust
pub enum VmState { Running, Stopped, Other(String) }      // Other counts as running for refusals
pub enum VmDisk { Dataset(String), Device(String), File(String) } // Dataset: normalised "pool/path"; Device: "/dev/..." not under /dev/zvol
pub struct Vm { pub name: String, pub state: VmState, pub disks: Vec<VmDisk> }
pub fn normalise_zvol_path(path: &str) -> Option<String>   // "/dev/zvol/tank/a/b", "tank/a/b" -> Some("tank/a/b")
pub fn parse_vm_bhyve(list: &str, confs: &[(String, String)]) -> Result<Vec<Vm>>  // (vm name, conf text)
```

- [ ] **Step 1: Write failing tests** `normalise_zvol_path_handles_every_alias` (both forms above, plus `None` for `/dev/ada0`, `/dev/zvol/` and `""`), a test for the parser asserting the `Vm` list for the stopped and the from-documentation running fixture (name, state, disk list), and `an_unparsable_listing_is_an_error_not_an_empty_list`. Run: `cargo test vm::` - FAIL: module missing.
- [ ] **Step 2: Implement** the types, `normalise_zvol_path` and the parser. It returns `Err` on a line it cannot read (fail closed), never a partial list.
- [ ] **Step 3: Run** `cargo test vm::` - PASS; `cargo clippy` clean for the new files.
- [ ] **Step 4: Commit** `feat: VM model and vm-bhyve parser`.

### Task 2: Discovery, the map, and sudoers entries

**Files:**
- Create: `src/vm/discover.rs`
- Modify: `src/vm/mod.rs`, `src/exec/privilege.rs` (new `Feature::Vms`, `ENTRIES`, `works_on`, `ALL`), `src/exec/sudoers.rs` only if its tests need the new feature list
- Test: `src/vm/discover.rs` tests with `FakeRunner`; the existing drift tests in `exec` and `book_check.rs`

**Interfaces:** Consumes: Task 1 types. Produces:
```rust
pub struct Discovery { pub vms: Vec<Vm>, pub manager_present: bool }
pub fn discover(runner: &dyn CommandRunner) -> Result<Discovery>
// Ok with manager_present = false: vm-bhyve is not installed. Err = it is present but could not be read (fail closed).
pub struct VmMap { /* dataset -> Vec<(vm name, VmState)> */ }
impl VmMap { pub fn from(d: &Discovery) -> VmMap; pub fn users_of(&self, dataset: &str) -> &[(String, VmState)]; pub fn running_users_of(&self, dataset: &str) -> Vec<&str> }
```
`users_of` also matches a **child** dataset of a VM's disk dataset and a **parent** of it (destroying `tank/vm` takes `tank/vm/web/disk0` with it): add `users_under(&self, dataset: &str)` for "this dataset or anything below it".

- [ ] **Step 1: Failing tests** with the fake runner: it runs the `vm` commands and reads each conf; a host without vm-bhyve returns `Ok` with `manager_present = false` (the runner reports command-not-found as the verified output says); a refused sudo or non-zero status on a **present** manager returns `Err`; `VmMap::users_of` finds a VM from each alias form; `users_under` finds a VM when the destroyed dataset is an ancestor. Run — FAIL.
- [ ] **Step 2: Implement** `discover` and `VmMap`. Privileged or not follows Task 0's verified answer.
- [ ] **Step 3: Add `Feature::Vms`** with one `ENTRY` per command actually needed (`vm`, `zdb`), narrowly pinned where the verified forms allow, FreeBSD only. Run `cargo test` — the sudoers, `check-host` and book drift tests tell what else to update (the book's privileged-command list: fold into Task 8's docs and keep the suite green here by updating the reference chapter minimally).
- [ ] **Step 4: Commit** `feat: discover vm-bhyve VMs`.

### Task 3: The hypervisor name in the Hosts table

**Files:**
- Modify: `src/zfs/host_health.rs` (`parse_detection`, `HostKind`), `src/ui/layout.rs` (Type column ~line 420), `src/daemon/health_text.rs` (uses `HostKind::word`), `docs/superpowers/specs/2026-10-19-vm-storage-design.md` part F (say "Hosts section's Type column", the user's correction)
- Test: the existing tests in those files plus new ones

**Interfaces:** Produces `HostKind::label(&self) -> String`: `Vm(h)` -> the hypervisor name normalised (`kvm`, `bhyve`, `xen`, `vmware`, `oracle`..., or `vm` when the name is empty), `Metal` -> `metal`, `Jail` -> `jail`, `Unknown` -> `-`. `word()` stays for callers that need a `&'static str`.

- [ ] **Step 1: Failing tests:** `parse_detection_reads_the_hypervisor_from_kern_vm_guest` (fixtures `kvm` and `bhyve`; empty -> `vm`), `host_kind_label_names_the_hypervisor`, and a layout test that the Type column shows `kvm` for `HostKind::Vm("kvm")` and `bhyve` for `Vm("bhyve")`. Run — FAIL.
- [ ] **Step 2: Implement:** `parse_detection` keeps the hypervisor name it already reads from `kern.vm_guest`; the Type cell and `health_text` use `label()`; widen the column only as far as needed (read `Cols` in `layout.rs`).
- [ ] **Step 3: Run** `cargo test` — PASS (the detection fixtures for FreeBSD must be unchanged).
- [ ] **Step 4: Commit** `feat: Hosts table names the hypervisor`.

### Task 4: Refusals and labels

**Files:**
- Create: `src/vm/guard.rs`
- Modify: `src/ui/state.rs` (`vm_map` state, key arms for `d`, `D`, `b`, volsize edit), `src/ui/modal.rs` (`MutationCall` run-time recheck for `DestroyDataset`, `DestroyDatasetRecursive`, `RollbackSnapshot`, `SetDatasetProperty` with `volsize`), `src/ui/layout.rs` (the "used by VM" label), `src/ui/state/worker.rs` (map refresh)
- Test: `src/vm/guard.rs`, `src/ui/state/tests.rs`

**Interfaces:** Consumes: `VmMap`, `Discovery`. Produces:
```rust
pub enum Action { Destroy, DestroyRecursive, Rollback, Shrink }
pub enum Verdict { Allowed, AllowedNote(String), Refused(String) }     // note: "used by VM web (stopped)"
pub fn check(map: &Result<VmMap, String>, dataset: &str, action: Action) -> Verdict
// Err(map) (could not ask) => Refused("could not check whether a VM uses it: <why>") for the four actions.
// Running VM using dataset (or, for destroy-recursive/destroy of a parent, anything under it) => Refused naming every VM.
// Stopped VM => AllowedNote naming it. No VM => Allowed.
pub fn check_now(runner: &dyn CommandRunner, os: Option<HostOs>, dataset: &str, action: Action) -> Result<(), String>  // fresh discover, same rules; used at run time
```

- [ ] **Step 1: Failing tests** in `guard.rs`: each action x (running, stopped, none, unreadable map); two running VMs named in one refusal; ancestor destroy refused; snapshot (not an `Action`) untouched. In `tests.rs`: pressing `d` on a zvol of a running VM sets a status refusal naming the VM and opens no modal; on a stopped VM the confirm text contains the VM name; `MutationCall::DestroyDataset` run with a fake runner whose fresh discovery says running returns an error and **never calls `zfs destroy`** (assert on the recorded calls). Run — FAIL.
- [ ] **Step 2: Implement** `guard.rs`; keep `App.vm_map: Option<Result<VmMap, String>>` refreshed with the dataset list (a thread like the other refreshes; not on every redraw) and cleared on host change; call `check` in the four key arms; call `check_now` first inside `MutationCall::run` for the four calls.
- [ ] **Step 3: Label:** the Datasets list and detail show `used by VM <name>` for a zvol present in the map. Test in `layout.rs` with the existing render helpers.
- [ ] **Step 4: Run** `cargo test` — PASS. **Step 5: Commit** `feat: refuse to damage a running VM's storage`.

### Task 5: The VMs section

**Files:**
- Create: `src/ui/vms.rs` (drawing helpers and row text, kept out of the 3400-line `layout.rs`)
- Modify: `src/ui/state.rs` (`Section::Vms`, `sections` vec between `BootEnvs` and `Jobs`, `vms`, `vms_table`, `vms_tab`, `refresh_vms`, key arms `[`/`]`), `src/ui/state/worker.rs` (`WorkerMsg::Vms(usize, Result<Discovery>)`, `on_vms`), `src/ui/support.rs` (`section_label`, `ALL` in its test, `supports`), `src/ui/mod.rs` (the two `refresh_*` match arms), `src/ui/layout.rs` (`draw_vms`), `src/ui/hints.rs`, `src/ui/state.rs::help_text` (a `VMs` heading), `docs` keys reference in Task 8
- Test: `tests.rs`, `layout.rs`, `hints.rs`, `support.rs`

**Interfaces:** Consumes: `Discovery`, `Vm`, `HostKind`. Produces: `Section::Vms`; `pub enum VmTab { Disks, Snapshots }`; `App::selected_vm(&self) -> Option<&Vm>`.

- [ ] **Step 1: Failing tests:** the section is in the sidebar between Boot Envs and Jobs; `supports(_, Section::Vms)` is true for FreeBSD hosts only (it reports "vm-bhyve is not installed" itself); the list shows name and state; `[`/`]` switch the tabs; the Disks tab lists each disk with size and used (looked up from the dataset list already in the app) and a physical disk as `device /dev/...`; the Snapshots tab lists the snapshots of the VM's datasets grouped by snapshot name with a running/stopped note; an unreadable manager shows its reason, not an empty list; with no manager the section says so. Hints: `n` and `a` are advertised only on the Disks tab with a VM selected. Run — FAIL.
- [ ] **Step 2: Implement** state, worker, drawing and hints following `BootEnvs` (`refresh_bootenvs`, `on_boot_envs`, `draw_bootenvs`) as the pattern, including the stale-refresh handling and `pin_selection`.
- [ ] **Step 3: Run** `cargo test` — PASS (the `help` drift test in `book_check.rs` will demand the book's keys reference list the new keys: do that in Task 8, and keep the test green here by adding the rows now).
- [ ] **Step 4: Commit** `feat: VMs section with Disks and Snapshots tabs`.

### Task 6: Create a zvol for a VM (slice 1)

**Files:**
- Create: `src/zfs/volume.rs`
- Modify: `src/zfs/mod.rs`, `src/ui/modal.rs` (`MutationCall::CreateVmZvol`, `FormKind::CreateVmZvol`), `src/ui/state/forms.rs` (form build and submit), `src/ui/state.rs` (`n` on the VMs Disks tab), `src/vm/mod.rs` (`config_line`)
- Test: `src/zfs/volume.rs`, `tests.rs`

**Interfaces:** Produces:
```rust
pub struct ZvolSpec { pub dataset: String, pub size_bytes: u64, pub volblocksize: Option<String>, pub sparse: bool, pub compression: Option<String> }
pub fn create_args(spec: &ZvolSpec) -> Vec<String>   // ["create", ("-s"), "-V", size, ("-o","volblocksize=..."), ("-o","compression=..."), "--"?, dataset]: options before the name; `-s` only when sparse
pub fn create_zvol(runner: &dyn CommandRunner, spec: &ZvolSpec) -> Result<()>
pub fn overcommit_bytes(pool_free: u64, existing_sparse_total: u64, new_size: u64) -> Option<u64>  // Some(excess) when sparse promises exceed free space
pub fn default_name(pool: &str, vm: &str, disk_no: u32) -> String  // "pool/vm/<vm>/disk<N>"
// crate::vm
pub fn config_line(vm: &str, dataset: &str, disk_no: u32) -> String   // the vm-bhyve diskN_* stanza
```
The encrypted variant calls the existing `zfs::encryption::create_encrypted` path with the same options (passphrase on stdin only); read it before wiring.

- [ ] **Step 1: Failing tests:** exact `create_args` for reserved and sparse, with and without options; options-before-name for every case; `overcommit_bytes` at the boundary (equal -> `None`, one byte over -> `Some(1)`); `default_name`; `config_line` from the verified form; the form refuses an existing name, a size of 0, a size that is not a number with unit, a name not under the chosen parent; the confirm shows `zfs create -V ...`; after success the info screen shows the config line; with an encrypted option the passphrase appears only in the fake runner's stdin. Run — FAIL.
- [ ] **Step 2: Implement.** The form fields: VM, Disk number, Size, Pool/parent (F2), Sparse (y/n, default n), Block size (default `16K`), Encrypt (y/n). Warn (never refuse) on overcommit in the confirm text.
- [ ] **Step 3: Run** `cargo test` — PASS. **Step 4: Commit** `feat: create a zvol for a VM and show its config line`.

### Task 7: The guarded physical-disk form (slice 2)

**Files:**
- Create: `src/vm/disk_check.rs`, `src/vm/attach.rs`
- Modify: `src/ui/modal.rs` (`MutationCall::AttachVmDisk`, `FormKind::AssignVmDisk`), `src/ui/state/forms.rs`, `src/ui/picker.rs` (a picker whose rows carry a verdict), `src/ui/state.rs` (`a` on the Disks tab), `src/exec/privilege.rs` (the attach commands, narrowly pinned)
- Test: `disk_check.rs`, `attach.rs`, `tests.rs`

**Interfaces:** Consumes: `disk::list_disks_with_membership`, `DiskInfo`, `Discovery`. Produces:
```rust
pub enum DiskVerdict { Free, Refused(String) }
pub fn verdicts(runner: &dyn CommandRunner, target_vm: &Vm, disks: &[DiskInfo], discovery: &Discovery) -> Vec<(String, DiskVerdict)>
// Refused reasons, in this priority: "member of pool <p> (<vdev>)" | "ZFS label from pool \"<p>\" (not imported)" |
// "could not read its ZFS label" (fail closed) | "mounted / swap / boot disk" | "used by VM <v> (<disk>)" ; and a running target VM
// refuses every disk with "VM <name> is running".
pub fn attach_args(vm: &str, device: &str) -> (String, Vec<String>)   // the verified vm-bhyve attach command
pub fn attach_disk(runner: &dyn CommandRunner, vm: &Vm, device: &str, disks: &[DiskInfo], discovery_now: &dyn Fn() -> Result<Discovery>) -> Result<String>
// re-runs `verdicts` (fresh disks + fresh discovery) and returns Err(reason) without running anything unless the disk is still Free
```
Never writes to the disk; no `--force`; no override path exists in the API.

- [ ] **Step 1: Failing tests** with fake runners, one per refusal reason and the priority order; a label-read failure (`zdb` exits non-zero in a way the fixture shows is "cannot read", not "no label") is **Refused**; a free disk is `Free`; `attach_args` exact; `attach_disk` runs nothing when the recheck finds the disk now in a pool or the VM now running (assert the recorded calls), and runs exactly the verified command when still free. Form tests: refused disks appear greyed with the reason and cannot be chosen (Enter on one sets a status message and does not advance); the confirm shows the exact command. Run — FAIL.
- [ ] **Step 2: Implement** the checks, the attach, the form and picker. Disk names follow what `list_disks` already returns (`ada1`, `da2`); the device path given to the manager is built from the verified form.
- [ ] **Step 3: Add the privileged attach entries** to `ENTRIES` (pinned arguments as Task 0 showed), then run `cargo test` — the sudoers drift tests pass.
- [ ] **Step 4: Commit** `feat: guarded physical-disk assignment for VMs`.

### Task 8: Docs, status, and drift tests

**Files:**
- Create: `docs/book/24-vms.md` (name and number follow the book's `SUMMARY.md` rule; the README chapter list must match: `src/book_check.rs` enforces it), `docs/book/captures/` text captures made with `examples/capture.rs` (no real names: use `alice`/`carol`, host names from the test boxes)
- Modify: `docs/book/20-reference-keys.md`, `docs/book/22-reference-commands.md` (the privileged commands), `docs/book/18-safety.md` (the refusals), `docs/book/23-glossary.md`, `README.md` (chapter list, a VMs section, and the "What is tested, and what is not" paragraph: vm-bhyve discovery is tested on fixtures and on real tooling without a running VM, **not on a running bhyve guest**), `docs/book/03-freebsd-for-newcomers.md` untouched
- Test: `src/book_check.rs`

- [ ] **Step 1:** write the chapter from the code, the verified file and real captures (technical only: no migration or newcomer prose). Cover: what zfsctl does and does not do for VMs, vm-bhyve, the VMs section and tabs, snapshots of a running VM's zvol being crash-consistent only, replication (not restic) as the copy for zvols, the refusals and why there is no override, creating a zvol and the config line, the guarded disk form, the Hosts hypervisor mark, and the sudoers entries.
- [ ] **Step 2:** `cargo test` — the book drift tests (keys, commands, chapter list, privileged names) must pass; fix whatever they name.
- [ ] **Step 3: Commit** `docs: VMs chapter, keys, safety, status`.

### Task 9: Mutation run, review, live check

- [ ] **Step 1:** `cargo clippy --all-targets` clean; `cargo mutants -f src/vm -f src/zfs/volume.rs` (in `/home/me/zfsctl-mutants/` style, background); add tests for survivors in the guards (`vm::guard`, `vm::disk_check`, `vm::attach`).
- [ ] **Step 2:** whole-branch review by a fresh most-capable reviewer with the Review Focus section verbatim; one fix pass, each fix RED then GREEN.
- [ ] **Step 3:** live check on the hub and the test boxes: Hosts Type column shows `kvm` for the KVM guests; VMs section on `fbsd16t1` (vm-bhyve, no running VM); a refusal can only be shown with a running VM, so test it by fake in unit tests and say so; the zvol form creates a real zvol on a scratch pool; the disk form refuses a pool member.
- [ ] **Step 4:** ask before merge and install (standing workflow), then merge `--no-ff`, install on the hub, update memory.
