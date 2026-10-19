# VM storage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** zfsctl learns which ZFS storage belongs to which VM (vm-bhyve, libvirt/KVM, OmniOS bhyve zones), refuses to damage a running VM's storage, creates a zvol for a VM, assigns a physical disk through a guarded form, shows the hypervisor in the Hosts table, and gets a VMs section.

**Architecture:** a new `src/vm/` module holds the model, three read-only manager readers (pure parsers plus a `discover` that runs the commands) and the pure safety checks. The UI gets a `Section::Vms`, a dataset-to-VM map kept in `App`, refusals at key time and again at run time, and two forms. Everything privileged goes through the existing `exec::privilege` table.

**Tech Stack:** Rust, ratatui, the existing `CommandRunner` and `FakeRunner` (`exec::test_support`).

**Spec:** `docs/superpowers/specs/2026-10-19-vm-storage-design.md` (approved 2026-10-19).

## Global Constraints

- Rust-only repo; the only non-Rust file stays `contrib/rc.d/zfsctl`.
- No root SSH; root only through sudoers, and every new privileged command is an entry in `exec::privilege::ENTRIES` (drift tests enforce the sudoers line, the book and `check-host`).
- zfsctl never creates, starts, stops, installs or deletes a VM, never hand-edits a manager's file, never wipes/partitions/formats a disk, never lets the user override a refusal.
- Two-key confirms (`y`, then Enter); the confirm shows the exact command; typed names for irreversible actions.
- Fail closed: "could not ask" is never read as "no VM uses this". Same for the disk checks.
- Secrets only on stdin (the encrypted-zvol option reuses the existing `create_encrypted` handling).
- illumos strict getopt: options before positionals, no `--` after a `get` property list (the lint test `no_get_call_puts_a_double_dash_after_the_property_list` must stay green).
- Tests assert exact argument lists on the fake runner; a mutation run (`cargo mutants -f` on the new files) before the final review.
- Every command, file layout and output format of vm-bhyve, virsh, zonecfg/zoneadm and the illumos hypervisor source is **unverified until Task 0**; no parser is written from memory.
- Commit trailer: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.

## Review Focus

- **Path aliases.** libvirt reports `/dev/zvol/tank/vm/web/disk0` (or `/dev/zvol/rdsk/...` on illumos), vm-bhyve a dataset name; all must normalise to the dataset name `tank/vm/web/disk0` or the map misses the VM and the refusal never fires. Test each form.
- **Names with unusual characters.** A VM or dataset name with spaces, `@` or `%`; a zvol whose snapshot or clone is named in a config. Must not break parsing or let a refusal be skipped.
- **A VM with disks in several pools, or one disk used by two VMs.** The map holds all of them; the refusal names every running VM.
- **Manager installed but unreadable** (sudo refused, config unparsable, command times out). The refusals say "could not check whether a VM uses it" and refuse; the VMs section says why it cannot list.
- **A VM that starts between the picker and the confirm.** The checks run again immediately before the destructive or attach command, not only when the form opens.
- **Snapshot of a running VM's zvol is allowed** (with the crash-consistent note); only destroy, recursive destroy, rollback and shrink are refused.

---

### Task 0: Verify the commands on real installs, and keep fixtures

**Files:**
- Create: `src/vm/fixtures/` (text fixtures: `vm_list.txt`, `vm_conf_zvol.txt`, `vm_conf_custom_disk.txt`, `virsh_list_all.txt`, `virsh_domblklist.txt`, `zoneadm_list.txt`, `zonecfg_info_bhyve.txt`, `zdb_label_ok.txt`, `zdb_label_none.txt`, `detect_virt_*.txt`)
- Create: `docs/superpowers/specs/2026-10-19-vm-storage-verified.md` (what was run, on which box, the exact output)

**Interfaces:** Produces: the fixtures every later parser test loads with `include_str!`, and the verified answers to the spec's "to verify" list.

This task changes test machines (installs packages, defines a VM that is never started). **Ask the user before touching any box**; nothing here starts a VM.

- [ ] **Step 1: FreeBSD (the `fbsd16t1` VM, a scratch pool).** `pkg install vm-bhyve`; initialise a datastore on a scratch zpool; `vm create -s 1G probe`; capture `vm list`, `vm info probe` (state, disk lines), the VM's `probe.conf` (note `disk0_type`, `disk0_name`, `disk0_dev`), then `vm add -d disk -t zvol -s 1G probe` and capture the new conf and the zvol it created. Record whether `vm list` needs sudo, and the exact `vm add` form for an existing zvol and for a raw device (`disk0_dev=custom`).
- [ ] **Step 2: Linux (`ub24`).** Install `libvirt-clients` and `libvirt-daemon-system`; define (never start) a domain whose disk is `/dev/zvol/<pool>/vm/probe/disk0` (a zvol made for it); capture `virsh list --all`, `virsh domstate`, `virsh domblklist --details`, and the `virsh attach-disk <vm> <path> vdb --config` result. Record whether `virsh` needs sudo or the libvirt group, and `systemd-detect-virt` output on this KVM guest.
- [ ] **Step 3: illumos (`omnios`).** Create (never boot) a `bhyve`-brand zone config; capture `zoneadm list -cv`, `zonecfg -z probe info`, the `zonecfg` command that adds a disk, and whether `zadm` exists. Capture `zdb -l` on a pool disk and on a blank disk (the read-only label check), and find what identifies the hypervisor on this KVM guest (candidates: `smbios`, `prtdiag`, `prtconf -b`).
- [ ] **Step 4: FreeBSD label read.** `zdb -l /dev/<disk>` on a pool member and on a blank disk of the hub or a VM; record exit codes and output for "has labels" and "none".
- [ ] **Step 5: Write `...-verified.md`** with each command, box, privilege needed and a one-line conclusion; copy outputs to the fixtures. Where reality differs from the spec, the verified file wins and the later tasks follow it; state each difference at the top of the file.
- [ ] **Step 6: Commit** `docs: verified VM manager commands and fixtures`.

### Task 1: The VM model and the three parsers

**Files:**
- Create: `src/vm/mod.rs`, `src/vm/bhyve.rs`, `src/vm/libvirt.rs`, `src/vm/zone.rs`
- Modify: `src/main.rs` (`mod vm;`)
- Test: unit tests in each file, using the Task 0 fixtures

**Interfaces:** Produces (all in `crate::vm`):
```rust
pub enum Manager { VmBhyve, Libvirt, BhyveZone }          // Manager::label() -> &'static str: "bhyve", "kvm", "bhyve zone"
pub enum VmState { Running, Stopped, Other(String) }
pub enum VmDisk { Dataset(String), Device(String), File(String) } // Dataset: normalised "pool/path"; Device: "/dev/..." not under /dev/zvol
pub struct Vm { pub name: String, pub manager: Manager, pub state: VmState, pub disks: Vec<VmDisk> }
pub fn normalise_zvol_path(path: &str) -> Option<String>   // "/dev/zvol/tank/a/b", "/dev/zvol/rdsk/tank/a/b", "tank/a/b" -> Some("tank/a/b")
pub fn parse_vm_bhyve(list: &str, confs: &[(String, String)]) -> Result<Vec<Vm>>  // (vm name, conf text)
pub fn parse_virsh(list_all: &str, domblklists: &[(String, String)]) -> Result<Vec<Vm>>
pub fn parse_zones(zoneadm_list: &str, zonecfgs: &[(String, String)]) -> Result<Vec<Vm>>
```

- [ ] **Step 1: Write failing tests** `normalise_zvol_path_handles_every_alias` (the three forms above, plus `None` for `/dev/sda`, `/dev/zvol/` and `""`), and one test per parser asserting the `Vm` list for its fixture (name, state, disk list), plus `an_unparsable_listing_is_an_error_not_an_empty_list` per parser. Run: `cargo test vm::` — FAIL: module missing.
- [ ] **Step 2: Implement** the types and `normalise_zvol_path`, then the parsers. Each parser returns `Err` on a line it cannot read (fail closed), never a partial list.
- [ ] **Step 3: Run** `cargo test vm::` — PASS; `cargo clippy` clean for the new files.
- [ ] **Step 4: Commit** `feat: VM model and manager parsers`.

### Task 2: Discovery, the map, and sudoers entries

**Files:**
- Create: `src/vm/discover.rs`
- Modify: `src/vm/mod.rs`, `src/exec/privilege.rs` (new `Feature::Vms`, `ENTRIES`, `works_on`, `ALL`), `src/exec/sudoers.rs` only if its tests need the new feature list
- Test: `src/vm/discover.rs` tests with `FakeRunner`; the existing drift tests in `exec` and `book_check.rs`

**Interfaces:** Consumes: Task 1 types. Produces:
```rust
pub struct Discovery { pub vms: Vec<Vm>, pub managers: Vec<Manager> }   // managers actually present on the host
pub fn discover(runner: &dyn CommandRunner, os: Option<HostOs>) -> Result<Discovery>
// Ok with empty `managers` = no manager installed. Err = a manager is present but could not be read (fail closed).
pub struct VmMap { /* dataset -> Vec<(vm name, VmState)> */ }
impl VmMap { pub fn from(d: &Discovery) -> VmMap; pub fn users_of(&self, dataset: &str) -> &[(String, VmState)]; pub fn running_users_of(&self, dataset: &str) -> Vec<&str> }
```
`users_of` also matches a **child** dataset of a VM's disk dataset and a **parent** of it (destroying `tank/vm` takes `tank/vm/web/disk0` with it): add `users_under(&self, dataset: &str)` for "this dataset or anything below it".

- [ ] **Step 1: Failing tests** per OS with the fake runner: FreeBSD runs the `vm` commands and reads each conf; Linux runs `virsh`; illumos runs `zoneadm`/`zonecfg`; a host with no manager returns `Ok` with empty `managers` (the runner reports command-not-found as the verified output says); a refused sudo or non-zero status on a **present** manager returns `Err`; `VmMap::users_of` finds a VM from each alias; `users_under` finds a VM when the destroyed dataset is an ancestor. Run — FAIL.
- [ ] **Step 2: Implement** `discover` and `VmMap`. Privileged or not follows Task 0's verified answer.
- [ ] **Step 3: Add `Feature::Vms`** with one `ENTRY` per command actually needed (`vm`, `virsh`, `zoneadm`, `zonecfg`, `zdb`), narrowly pinned where the verified forms allow, `works_on` per OS. Run `cargo test` — the sudoers, `check-host` and book drift tests tell what else to update (the book's privileged-command list: fold into Task 8's docs and keep the suite green here by updating the reference chapter minimally).
- [ ] **Step 4: Commit** `feat: discover VMs on FreeBSD, Linux and illumos`.

### Task 3: The hypervisor name in the Hosts table

**Files:**
- Modify: `src/zfs/host_health.rs` (`DETECT_SCRIPT`, `parse_detection`, `HostKind`), `src/ui/layout.rs` (Type column ~line 420), `src/daemon/health_text.rs` (uses `HostKind::word`), `docs/superpowers/specs/2026-10-19-vm-storage-design.md` part F (say "Hosts section's Type column", the user's correction)
- Test: the existing tests in those files plus new ones

**Interfaces:** Produces `HostKind::label(&self) -> String`: `Vm(h)` -> the hypervisor name normalised (`kvm`, `bhyve`, `xen`, `vmware`, `oracle`..., or `vm` when the name is empty), `Metal` -> `metal`, `Jail` -> `jail`, `Unknown` -> `-`. `word()` stays for callers that need a `&'static str`.

- [ ] **Step 1: Failing tests:** `parse_detection_reads_the_hypervisor_on_linux` (fixture of `systemd-detect-virt` output `kvm`; `none` -> Metal; the command missing -> `Unknown`), the same for illumos from the Task 0 source, `host_kind_label_names_the_hypervisor`, and a layout test that the Type column shows `kvm` for `HostKind::Vm("kvm")` and `bhyve` for `Vm("bhyve")`. Run — FAIL.
- [ ] **Step 2: Implement:** extend `DETECT_SCRIPT` (keeping `uname -s;` first, and the exit status for ssh failures only) with the Linux and illumos sources; `parse_detection` fills `HostKind` for them; the Type cell and `health_text` use `label()`; widen the column only as far as needed (read `Cols` in `layout.rs`).
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

**Interfaces:** Consumes: `Discovery`, `Vm`, `Manager::label`, `HostKind`. Produces: `Section::Vms`; `pub enum VmTab { Disks, Snapshots }`; `App::selected_vm(&self) -> Option<&Vm>`.

- [ ] **Step 1: Failing tests:** the section is in the sidebar between Boot Envs and Jobs; `supports(_, Section::Vms)` is true for every OS (it reports "no VM manager found" itself); the list shows name, manager label, state; `[`/`]` switch the tabs; the Disks tab lists each disk with size and used (looked up from the dataset list already in the app) and a physical disk as `device /dev/...`; the Snapshots tab lists the snapshots of the VM's datasets grouped by snapshot name with a running/stopped note; an unreadable manager shows its reason, not an empty list; with no manager the section says so. Hints: `n` and `a` are advertised only on the Disks tab with a VM selected. Run — FAIL.
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
pub fn create_args(spec: &ZvolSpec) -> Vec<String>   // ["create", ("-s"), "-V", size, ("-o","volblocksize=..."), ("-o","compression=..."), "--"?, dataset]: options first (illumos getopt); `-s` only when sparse
pub fn create_zvol(runner: &dyn CommandRunner, spec: &ZvolSpec) -> Result<()>
pub fn overcommit_bytes(pool_free: u64, existing_sparse_total: u64, new_size: u64) -> Option<u64>  // Some(excess) when sparse promises exceed free space
pub fn default_name(pool: &str, vm: &str, disk_no: u32) -> String  // "pool/vm/<vm>/disk<N>"
// crate::vm
pub fn config_line(manager: Manager, vm: &str, dataset: &str, disk_no: u32) -> String
```
The encrypted variant calls the existing `zfs::encryption::create_encrypted` path with the same options (passphrase on stdin only); read it before wiring.

- [ ] **Step 1: Failing tests:** exact `create_args` for reserved and sparse, with and without options; options-before-name for every case; `overcommit_bytes` at the boundary (equal -> `None`, one byte over -> `Some(1)`); `default_name`; `config_line` for each manager from the verified forms; the form refuses an existing name, a size of 0, a size that is not a number with unit, a name not under the chosen parent; the confirm shows `zfs create -V ...`; after success the info screen shows the config line; with an encrypted option the passphrase appears only in the fake runner's stdin. Run — FAIL.
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
pub fn verdicts(runner: &dyn CommandRunner, os: Option<HostOs>, target_vm: &Vm, disks: &[DiskInfo], discovery: &Discovery) -> Vec<(String, DiskVerdict)>
// Refused reasons, in this priority: "member of pool <p> (<vdev>)" | "ZFS label from pool \"<p>\" (not imported)" |
// "could not read its ZFS label" (fail closed) | "mounted / swap / boot disk" | "used by VM <v> (<disk>)" ; and a running target VM
// refuses every disk with "VM <name> is running".
pub fn attach_args(manager: Manager, vm: &str, device: &str) -> (String, Vec<String>)   // the manager's verified persistent attach command
pub fn attach_disk(runner: &dyn CommandRunner, os: Option<HostOs>, manager: Manager, vm: &Vm, device: &str, disks: &[DiskInfo], discovery_now: &dyn Fn() -> Result<Discovery>) -> Result<String>
// re-runs `verdicts` (fresh disks + fresh discovery) and returns Err(reason) without running anything unless the disk is still Free
```
Never writes to the disk; no `--force`; no override path exists in the API.

- [ ] **Step 1: Failing tests** with fake runners, one per refusal reason and the priority order; a label-read failure (`zdb` exits non-zero in a way the fixture shows is "cannot read", not "no label") is **Refused**; a free disk is `Free`; `attach_args` exact per manager; `attach_disk` runs nothing when the recheck finds the disk now in a pool or the VM now running (assert the recorded calls), and runs exactly the verified command when still free. Form tests: refused disks appear greyed with the reason and cannot be chosen (Enter on one sets a status message and does not advance); the confirm shows the exact command. Run — FAIL.
- [ ] **Step 2: Implement** the checks, the attach, the form and picker. Disk names on Linux/illumos follow what `list_disks` already returns (`sdb`, `c1t0d0`); the device path given to the manager is built from the verified form.
- [ ] **Step 3: Add the privileged attach entries** to `ENTRIES` (pinned arguments as Task 0 showed), then run `cargo test` — the sudoers drift tests pass.
- [ ] **Step 4: Commit** `feat: guarded physical-disk assignment for VMs`.

### Task 8: Docs, status, and drift tests

**Files:**
- Create: `docs/book/24-vms.md` (name and number follow the book's `SUMMARY.md` rule; the README chapter list must match: `src/book_check.rs` enforces it), `docs/book/captures/` text captures made with `examples/capture.rs` (no real names: use `alice`/`carol`, host names from the test boxes)
- Modify: `docs/book/20-reference-keys.md`, `docs/book/22-reference-commands.md` (the privileged commands), `docs/book/18-safety.md` (the refusals), `docs/book/23-glossary.md`, `README.md` (chapter list, a VMs section, and the "What is tested, and what is not" paragraph: vm-bhyve and bhyve-zone discovery are tested on fixtures and on real tooling without a running VM, **not on a running bhyve guest**), `docs/book/03-freebsd-for-newcomers.md` untouched
- Test: `src/book_check.rs`

- [ ] **Step 1:** write the chapter from the code, the verified file and real captures (technical only: no migration or newcomer prose). Cover: what zfsctl does and does not do for VMs, the three managers, the VMs section and tabs, the refusals and why there is no override, creating a zvol and the config line, the guarded disk form, the Hosts hypervisor mark, and the sudoers entries.
- [ ] **Step 2:** `cargo test` — the book drift tests (keys, commands, chapter list, privileged names) must pass; fix whatever they name.
- [ ] **Step 3: Commit** `docs: VMs chapter, keys, safety, status`.

### Task 9: Mutation run, review, live check

- [ ] **Step 1:** `cargo clippy --all-targets` clean; `cargo mutants -f src/vm -f src/zfs/volume.rs` (in `/home/me/zfsctl-mutants/` style, background); add tests for survivors in the guards (`vm::guard`, `vm::disk_check`, `vm::attach`).
- [ ] **Step 2:** whole-branch review by a fresh most-capable reviewer with the Review Focus section verbatim; one fix pass, each fix RED then GREEN.
- [ ] **Step 3:** live check on the hub and the test boxes: Hosts Type column shows `kvm` for the KVM guests; VMs section on `fbsd16t1` (vm-bhyve, no running VM) and `ub24` (libvirt, defined VM) and `omnios`; a refusal can only be shown with a running VM, so test it by fake in unit tests and say so; the zvol form creates a real zvol on a scratch pool; the disk form refuses a pool member on every box.
- [ ] **Step 4:** ask before merge and install (standing workflow), then merge `--no-ff`, install on the hub, update memory.
