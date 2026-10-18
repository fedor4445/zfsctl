# Linux and illumos hosts, steps 1 and 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** zfsctl knows each host's OS, dims and explains the sections that do not work there, picks sudoers defaults by OS, and lists the real disks of Linux and illumos hosts.

**Architecture:** `HostOs` is read once per host in the existing first probe (one `sh -c` round trip that also carries today's FreeBSD `sysctl` read) and kept in `HostProbe::Reached`. A pure `supports(os, section)` table decides what works; the sidebar, hints, key handling, refresh functions and daemon shares check consult it. Disks get one backend per OS behind `zfs::disk::list_disks_with_membership(runner, os)`.

**Tech Stack:** Rust, ratatui; the existing `CommandRunner`, `HostProbe`, `App` plumbing.

**Spec:** `docs/superpowers/specs/2026-10-18-linux-illumos-hosts-design.md` (steps 1 and 2 of its build order: OS and seam, then Disks)

## Global Constraints

- `HostOs`: `FreeBsd`, `Linux`, `Illumos`, `Other`; words `FreeBSD`, `Linux`, `illumos`, `unknown`. Read from `uname -s` (`FreeBSD`, `Linux`, `SunOS`); anything else, or a failed read, is `Other`. **An unknown or not-yet-probed OS behaves exactly like FreeBSD today** (everything offered); an ssh-level failure (exit 255) leaves the OS undetected so the next probe asks again.
- One round trip: the first probe asks `sh -c 'uname -s; sysctl -n kern.vm_guest security.jail.jailed 2>/dev/null'` instead of the bare `sysctl` (both `sh` and `uname` are registered unprivileged commands). The host Type comes from the sysctl lines only on FreeBSD; on Linux and illumos it stays `-`.
- Support table (this plan's end state): FreeBSD and Other: everything. Linux: Disks yes; Boot Envs, Shares, the dataset ACL tab no. illumos: Disks yes; Boot Envs, Shares, the ACL tab no (Boot Envs and Shares flip to yes in the later steps 3 and 4). No other section is ever gated.
- An unsupported section for the selected host: its sidebar entry is dimmed (`theme.dim()`), it can still be opened and says `<Section> is not supported on <OS> hosts yet.` (OS word as above), its refresh does nothing and runs no command, its hint bar shows only the global hints, and its section keys are inert. Switching to a supported host restores it at once.
- Sudoers by OS: `print-sudoers` and `check-host` read the host's OS (unprivileged `uname -s` over the same runner) before choosing defaults: FreeBSD as today; Linux and illumos default to `core` (+ nothing else; `smart`, `restic`, `rclone` stay opt-in); `--with` of a feature the OS does not support (`acl`, `disks`, `bootenvs`, `shares`, `samba` on Linux and illumos) is refused with `feature '<x>' is not available on <OS> hosts`; the printed install path is `/etc/sudoers.d/zfsctl` on Linux and illumos (`/usr/local/etc/sudoers.d/zfsctl` on FreeBSD).
- The daily shares check (`daemon::shares_check::check_host`) first asks `uname -s` (unprivileged) and is silent unless the answer is FreeBSD or unknown.
- Disks backends: Linux `lsblk -b -d -n -P -o NAME,SIZE,TYPE,MODEL,SERIAL,ROTA` (unprivileged, only `TYPE="disk"`, `ROTA` 1 = rotating, 0 = SSD); illumos `iostat -En` (unprivileged; per disk the name, `Vendor:`+`Product:` as the model, `Serial No:`, the byte count in `Size: ... <N bytes>`; entries without a size are skipped). FreeBSD stays `geom disk list`. Pool membership comes from `zpool status` as today, with name matching per OS: Linux a vdev is the disk itself or `<disk><digits>` or `<disk>p<digits>`; illumos as FreeBSD's slice rule (`c1t0d0` or `c1t0d0s0`); file-backed vdevs match nothing.
- `lsblk` is registered as an unprivileged command under the `disks` feature; no new privileged command anywhere in this plan.
- Style: pure functions with fixtures captured from the real hosts, comments say why, tests next to the code or in `src/ui/state/tests.rs`; FreeBSD fixtures and behaviour unchanged.

## Review Focus

- A FreeBSD host (all existing tests and the live hub/VMs) behaves exactly as before, including the first probe's host Type (Task 1 and 2 tests; live check on the hub).
- A host whose OS probe failed or answers something odd: treated as FreeBSD, never dims a section (Task 1 and 2 tests).
- Keys of a dimmed section are inert, including `n` in Boot Envs (which needs no selection) and the unadvertised-keys guard test (Task 2).
- `print-sudoers` for a Linux/illumos host never prints `geom`/`bectl`/`service`, and never fails with "not found on host" for them (Task 3).
- lsblk and iostat output with odd content: loop and optical devices, empty model/serial, no size, extra spaces (Task 4 tests with real fixtures).

---

### Task 1: Know the OS

**Files:**
- Modify: `src/zfs/host_health.rs` (`HostOs`, the probe, `HostProbe::Reached { os }`), `src/ui/state.rs` (store the OS per host, `host_os`), every `HostProbe::Reached { .. }` constructor the compiler lists (tests and fixtures)
- Test: `src/zfs/host_health.rs`, `src/ui/state/tests.rs`

**Interfaces:**
- Produces: `pub enum HostOs { FreeBsd, Linux, Illumos, Other }` (`Clone, Copy, PartialEq, Eq, Debug`) with `fn word(self) -> &'static str` and `pub fn host_os_from_uname(out: &str) -> HostOs`; `HostProbe::Reached { rtt_ms, pools, kind, os: Option<HostOs> }` (`os` is `Some` only on a probe that detected, like `kind`); `App::host_os(&self, host: usize) -> Option<HostOs>` (the last detected OS of that host, `None` until known).

- [ ] **Step 1: Write the failing tests.** `uname_output_maps_to_the_os` (`FreeBSD\n`, `Linux\n`, `SunOS\n`, `Darwin\n`, empty, garbage), `the_first_probe_asks_os_and_kind_in_one_round_trip` (a `FakeRunner` routing `("sh", "-c")` to `FreeBSD\nnone\n0\n`: `os == Some(FreeBsd)`, kind `Metal`; and exactly one unprivileged call), `on_linux_and_illumos_the_kind_stays_unknown` (`Linux\n` only; `SunOS\n` only: `os` set, `kind == Some(Unknown)`), `a_failed_os_read_is_treated_as_unknown_and_ssh_255_leaves_it_undetected` (status 1: `os == Some(Other)`; status 255: `os == None`), and in the state tests `the_apps_host_os_follows_the_last_probe_that_detected_it` (a later probe without `os` does not forget it).
- [ ] **Step 2: Run** `cargo test --offline host_health` and the state test: FAIL.
- [ ] **Step 3: Implement** per the signatures; the existing detect branch becomes the single `sh -c` call; keep `host_kind_from_sysctl` for the sysctl lines. Update the existing probe tests that scripted `("sysctl", ...)`.
- [ ] **Step 4: Run** `cargo test --offline`: PASS.
- [ ] **Step 5: Commit** `feat: the first probe of a host reads its OS`

### Task 2: What each OS supports, dimmed and explained

**Files:**
- Create: `src/ui/support.rs` (declare in `src/ui/mod.rs`)
- Modify: `src/ui/state.rs` (`section_supported`, gating in `refresh_disks`, `refresh_shares`, `refresh_bootenvs` and the ACL refresh, key guard in `handle_key`), `src/ui/layout.rs` (sidebar dimming; the "not supported" body in the Disks, Shares, Boot Envs and ACL drawers), `src/ui/hints.rs`
- Test: `src/ui/support.rs`, `src/ui/state/tests.rs`, `src/ui/layout.rs`, `src/ui/hints.rs`

**Interfaces:**
- Consumes: Task 1 `HostOs`, `App::host_os`.
- Produces: `pub fn supports(os: Option<HostOs>, section: Section) -> bool` (the table in the Global Constraints); `pub fn unsupported_text(os: HostOs, section: Section) -> String` (`Boot Envs is not supported on Linux hosts yet.`); `App::section_supported(&self, section: Section) -> bool` (selected host's OS) and `App::current_section_supported(&self) -> bool`.

- [ ] **Step 1: Write the failing tests.** `support.rs`: `unknown_and_freebsd_support_everything`, `linux_supports_disks_but_not_boot_envs_shares_or_acl`, `illumos_supports_disks_but_not_boot_envs_shares_or_acl`, `only_those_sections_are_ever_gated` (loop over all `Section`s: Dashboard, Hosts, Pools, Datasets, Snapshots, Jobs, Cloud, Alerts, Settings are always supported). State: `selecting_a_linux_host_dims_boot_envs_and_a_freebsd_host_restores_it`, `a_dimmed_section_runs_no_command_when_refreshed` (a `FakeRunner` that panics on any call), `keys_of_a_dimmed_section_are_inert` (`n` in Boot Envs on a Linux host opens no form; section keys do nothing; Tab and `q` still work). Layout: `the_sidebar_dims_unsupported_entries_for_the_selected_host`, `a_dimmed_section_says_why_in_its_body`. Hints: `a_dimmed_section_offers_only_the_global_hints`.
- [ ] **Step 2: Run:** FAIL. **Step 3: Implement.** The key guard sits in `handle_key` before the section-specific chunks: when `!current_section_supported()`, only the global and navigation chunks run. **Step 4: Run** `cargo test --offline` (the hints guard tests too): PASS. **Step 5: Commit** `feat: unsupported sections are dimmed and explained per host OS`

### Task 3: Sudoers and the shares check know the OS

**Files:**
- Modify: `src/exec/privilege.rs` (`select_for_os`), `src/exec/sudoers.rs` (read the OS, install path), `src/main.rs` (`admin` uses it), `src/daemon/shares_check.rs` (the `uname` gate)
- Test: the same files' `mod tests`

**Interfaces:**
- Consumes: Task 1 `HostOs`, `host_os_from_uname`.
- Produces: `exec::privilege::select_for_os(os: HostOs, with: &[String], without: &[String]) -> Result<Vec<Feature>>`; `exec::sudoers::detect_os(runner: &dyn CommandRunner) -> HostOs` (unprivileged `uname -s`; `Other` on any failure); `print_sudoers`/`check_host` take the features already chosen (their signatures stay); the printed header names the install path for the OS.

- [ ] **Step 1: Write the failing tests:** `select_for_os_defaults_linux_and_illumos_to_core_only`, `select_for_os_keeps_the_freebsd_defaults`, `with_a_feature_the_os_does_not_support_is_refused_with_its_name`, `smart_and_restic_stay_opt_in_on_every_os`, `print_sudoers_for_a_linux_host_prints_only_zpool_and_zfs_and_the_etc_path` (a scripted host answering `uname -s` and the resolve script), `shares_check_is_silent_on_linux_and_illumos_before_asking_about_services` (the runner panics on `service`), `shares_check_still_runs_on_freebsd_and_on_an_odd_uname` (the existing `Script` tests gain a `uname` route answering `FreeBSD`).
- [ ] **Step 2: Run:** FAIL. **Step 3: Implement**; `admin()` in `main.rs` detects the OS first and passes the OS-chosen features to `print_sudoers`/`check_host_target`. **Step 4: Run** `cargo test --offline`: PASS. **Step 5: Commit** `feat: sudoers defaults and the shares check follow the host's OS`

### Task 4: Disks on Linux and illumos

**Files:**
- Modify: `src/zfs/disk.rs` (backends, matching, `DiskMedia` display of an unknown rpm), `src/exec/privilege.rs` (`lsblk` entry), `src/ui/state.rs` (`refresh_disks` passes the OS), `src/ui/support.rs` (Disks yes on Linux and illumos, already in the table), `src/ui/layout.rs` if the Disks table needs the HDD-without-rpm text
- Create: fixtures `src/zfs/disk_fixtures/lsblk_ubuntu.txt`, `iostat_en_omnios.txt` (captured from `ub24` and `omnios`, serials kept or masked)
- Test: `src/zfs/disk.rs`

**Interfaces:**
- Consumes: Task 1 `HostOs`.
- Produces: `zfs::disk::list_disks_with_membership(runner, os: Option<HostOs>)`, `parse_lsblk(output: &str) -> Vec<DiskInfo>`, `parse_iostat_en(output: &str) -> Vec<DiskInfo>`, `DiskMedia::Hdd(0)` read as a rotating disk of unknown speed (`HDD`).

- [ ] **Step 1: Write the failing tests:** `lsblk_lists_only_disks_with_size_model_serial_and_media` (real fixture: loop devices dropped), `lsblk_survives_empty_model_and_serial_and_garbage`, `iostat_en_reads_name_model_serial_and_size_per_disk` (real fixture: `c1t0d0` ATA QEMU HARDDISK, `c2t0d0` Virtio Block Device), `iostat_en_skips_entries_without_a_size_and_survives_garbage`, `linux_membership_matches_whole_disks_and_numbered_or_p_partitions_only` (`sdb`, `sdb1`, `nvme0n1p1` match; `sdb10` vs disk `sdb1` does not; a file vdev matches nothing), `illumos_membership_matches_c1t0d0_and_c1t0d0s0`, `the_backend_follows_the_os` (a `FakeRunner` answering only the right command per `HostOs`; `None` and `Other` use `geom`), `an_unknown_rpm_hdd_is_called_hdd_without_a_number`.
- [ ] **Step 2: Run:** FAIL. **Step 3: Implement** per the Global Constraints (registry entry `Entry { name: "lsblk", features: &[Feature::Disks], kind: Kind::Unprivileged }`; the book's command-list drift test will want it named in the commands reference). **Step 4: Run** `cargo test --offline`: PASS. **Step 5: Commit** `feat: Disks lists the real disks of Linux and illumos hosts`

### Task 5: Book, README, live check

**Files:**
- Modify: `docs/book/05-remote-hosts.md` (which OSes and what works), `docs/book/11-disks.md`, `docs/book/22-reference-commands.md` (`lsblk`, the per-OS defaults of `print-sudoers`), `README.md`; a short new section "Linux and illumos hosts" in chapter 5

- [ ] **Step 1:** Update the prose; `cargo test --offline` (book drift tests) PASS.
- [ ] **Step 2: Live check** on the hub with the build run from its build directory against `ub24` and `omnios` (and a FreeBSD VM for regression): Hosts shows no sudo errors; Disks lists the real disks on both; Boot Envs, Shares and the Datasets ACL tab are dimmed and say why on both; selecting a FreeBSD host restores them; `print-sudoers ub24` and `omnios` print core only with the `/etc/sudoers.d` path; `check-host` both ok; the daemon log shows no `service` errors for them after a forced daily pass (delete `smart.json` and `shares.json`).
- [ ] **Step 3: Commit** `docs: Linux and illumos hosts`

### Task 6: Whole-branch review, merge, install

- [ ] **Step 1:** Full suite and clippy; whole-branch review with the Review Focus section; fix findings test-first.
- [ ] **Step 2:** Merge to `master` and install on the hub after the user's go-ahead; update the memory note.
