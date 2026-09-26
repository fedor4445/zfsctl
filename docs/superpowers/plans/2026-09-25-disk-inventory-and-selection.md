# Disk Inventory + Selection-Based Pool/Vdev Creation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `Section::Disks` listing every block device on the host with its size and pool/vdev membership, let the user select available disks and build a pool or add a vdev from that selection, and add `zpool resilver` as a Pools-section action.

**Architecture:** A new `zfs::disk` module (`list_disks` parsing `geom disk list`, `annotate_membership` cross-referencing a pool's vdev tree, `list_disks_with_membership` composing both plus `zpool list`) backs a new host-scoped `Section::Disks` following the exact `Pools`/`Datasets`/`Snapshots` refresh pattern. Selection (`HashSet<String>`) toggled with Space feeds two new `FormKind`s that build the **existing** `MutationCall::CreatePool`/`AddVdev` — no new mutation calls for disk selection. `zpool resilver` is a new, independent `MutationCall::ResilverPool`, same shape as the existing `ScrubPool`.

**Tech Stack:** Rust (2021 edition), ratatui 0.29, crossterm 0.28 — no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-25-disk-inventory-and-selection-design.md`.

## Global Constraints

- FreeBSD only, CLI shelling via machine-readable flags where the tool provides them — `geom disk list` does not (no `-H`/tabular form), so `zfs::disk`'s parser is a lenient line-by-line block parser, the same accepted style `parse_pool_status_vdevs` already uses for `zpool status`.
- `CommandRunner` is the sole execution abstraction. `resilver_pool` goes through the existing `run_checked()` helper and the existing `--` argument-injection guard convention — confirmed empirically against the real test box (`zpool resilver -- nonexistent-pool` → `cannot open 'nonexistent-pool': no such pool`, not a `--`-parsing error), so `resilver_pool` uses `--` exactly like `scrub_pool`/`create_pool`/etc., **not** the one deliberate `set` exception.
- No new `MutationCall` variants for disk-selection actions — `FormKind::CreatePoolFromDisks`/`AddVdevFromDisks` build the **existing** `MutationCall::CreatePool`/`MutationCall::AddVdev`. Only `MutationCall::ResilverPool` is genuinely new.
- `Section::Disks` is host-scoped like `Pools`/`Datasets`/`Snapshots` — no per-entity staleness tag on `WorkerMsg::Disks` (it isn't tied to one selected row the way pool/dataset properties are).
- `FakeRunner` (test-only, `src/exec/mod.rs`) gains per-command routing (Task 1) but stays fully backward compatible — every pre-existing test using `FakeRunner::new`/`FakeRunner::failing` with no routes must keep working unmodified.
- No ratatui rendering/terminal tests — all logic unit-tested against `App` via `handle_key`/`handle_modal_key`/`FakeRunner`, consistent with every prior plan.
- Adding `Section::Disks` to the `Section` enum makes every exhaustive `match` over `Section` in `layout.rs` and `ui/mod.rs` fail to compile until they're updated — Task 5 updates the enum, `state.rs`, `layout.rs`, and `ui/mod.rs` **together** in one task specifically because of this compile-time coupling. Likewise, Task 4's `MutationCall::ResilverPool` is self-contained within `modal.rs` (variant + its own `run()` arm), but the two new `FormKind` variants are deferred to Task 6, bundled with the `state.rs` code that builds and consumes them — a `FormKind` variant added alone in `modal.rs` would leave `build_pending_action` in `state.rs` non-exhaustive.
- `Section::Disks` is appended to the end of `App::sections` (`Hosts, Pools, Datasets, Snapshots, Disks`) — this changes which section `BackTab` wraps to from `Hosts` (`Snapshots` → `Disks`), which is why Task 5 includes mechanical fixes to three pre-existing tests that hardcoded `Snapshots` as "the last section."

## Review Focus

- A `geom disk list` block with no `Mediasize:` line at all (some virtual/optical geoms omit it) must still produce a `DiskInfo` (size `0`), not silently vanish from the list — pinned in Task 2.
- A disk that was `Available` and got selected, then transitions to claimed by a concurrent operation (or a stale/slow refresh lands late), must be dropped from `selected_disks` on the next refresh — otherwise a create/add submission could reference a device that's no longer free. Pinned in Task 5.
- Submitting the Create-Pool-from-Disks or Add-Vdev-from-Disks form with a blank name/target-pool field must keep the form open (matching every other creation form's empty-field behavior), not silently build a broken `zpool create`/`add` call. Pinned in Task 6.
- A failing `zpool resilver` (e.g. "no valid replicas") must surface its real stderr through the status bar exactly like every other mutation, not be swallowed. Pinned in Tasks 3-4.
- Switching hosts while disks are selected must not leak the previous host's selection (or disk list) onto the newly-selected host's Disks view. Pinned in Task 5.

---

## File Structure

```
src/
  exec/
    mod.rs            # Modify: FakeRunner gains per-command routing (test-only)
  zfs/
    disk.rs            # Create: DiskInfo, DiskMembership, list_disks, annotate_membership, list_disks_with_membership
    mod.rs              # Modify: pub mod disk;
    pool.rs              # Modify: resilver_pool
  ui/
    modal.rs            # Modify: MutationCall::ResilverPool (Task 4); FormKind::CreatePoolFromDisks/AddVdevFromDisks (Task 6)
    state.rs             # Modify: Section::Disks; App fields (disks, disks_table, selected_disks); WorkerMsg::Disks; refresh_disks; clear_host_data; App::sections; mechanical test fixes (Task 5) — then handle_key arms, build_pending_action arms, drain_worker_messages rule (Task 6)
    layout.rs             # Modify: sidebar label, draw_main dispatch, draw_disks (Task 5)
    mod.rs                # Modify: 'r'-key and queued-refresh dispatch arms for Section::Disks (Task 5)
README.md                 # Modify: keybindings (Task 7)
```

---

### Task 1: `FakeRunner` per-command routing

**Files:**
- Modify: `src/exec/mod.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `FakeRunner::route(self, cmd: &str, first_arg: &str, stdout: &str) -> Self` (builder-style, test-only). Existing `FakeRunner::new`/`FakeRunner::failing`/`FakeRunner::calls`/`FakeRunner::response` are unchanged.

`Task 2`'s `list_disks_with_membership` composes three different subcommands (`geom disk list`, `zpool list`, `zpool status`) against one runner — `FakeRunner`'s single scripted response can't stand in for all three at once. This task adds routing by `(cmd, first_arg)` so a test can script `("zpool", "list")` and `("zpool", "status")` differently, with an unrouted call falling back to the existing `response` field.

- [ ] **Step 1: Write the failing test**

Add a new top-level test module to `src/exec/mod.rs`, after the existing `pub mod test_support { ... }` block:

```rust
#[cfg(test)]
mod tests {
    use super::test_support::FakeRunner;
    use super::CommandRunner;

    #[test]
    fn fake_runner_route_overrides_default_response_for_matching_cmd_and_first_arg() {
        let runner = FakeRunner::new("default")
            .route("zpool", "status", "status-output")
            .route("zpool", "list", "list-output");
        assert_eq!(runner.run("zpool", &["status", "--", "tank"]).unwrap().stdout, "status-output");
        assert_eq!(runner.run("zpool", &["list", "-H"]).unwrap().stdout, "list-output");
        assert_eq!(
            runner.run("geom", &["disk", "list"]).unwrap().stdout,
            "default",
            "an unrouted call falls back to the default response"
        );
    }
}
```

- [ ] **Step 2: Run the test to confirm it fails**

Run: `cargo test fake_runner_route`
Expected: FAIL (`route` method doesn't exist yet)

- [ ] **Step 3: Add routing to `FakeRunner`**

Replace the existing `test_support` module's `FakeRunner` struct, `impl FakeRunner`, and `impl CommandRunner for FakeRunner` with:

```rust
    /// A `CommandRunner` that records every call it receives and, by
    /// default, always returns the same scripted `CommandOutput`. Used
    /// by unit tests throughout the crate so ZFS/host logic can be
    /// tested without a real system.
    ///
    /// Some library functions (e.g.
    /// `zfs::disk::list_disks_with_membership`) compose several
    /// different subcommands in one call — a single scripted response
    /// can't stand in for `geom disk list`, `zpool list`, and `zpool
    /// status` all at once. `route` scripts a distinct response for
    /// calls matching a given `cmd` and first argument (e.g. `("zpool",
    /// "status")` vs `("zpool", "list")`); calls that don't match any
    /// route fall back to `response`.
    pub struct FakeRunner {
        pub calls: Mutex<Vec<(String, Vec<String>)>>,
        pub response: CommandOutput,
        routes: std::collections::HashMap<(String, String), CommandOutput>,
    }

    impl FakeRunner {
        pub fn new(stdout: &str) -> Self {
            FakeRunner {
                calls: Mutex::new(Vec::new()),
                response: CommandOutput {
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                    status: 0,
                },
                routes: std::collections::HashMap::new(),
            }
        }

        pub fn failing(stderr: &str, status: i32) -> Self {
            FakeRunner {
                calls: Mutex::new(Vec::new()),
                response: CommandOutput {
                    stdout: String::new(),
                    stderr: stderr.to_string(),
                    status,
                },
                routes: std::collections::HashMap::new(),
            }
        }

        /// Scripts a distinct successful response for calls matching
        /// `cmd` whose first argument is `first_arg`. Builder-style —
        /// call before wrapping the runner in `Arc`.
        pub fn route(mut self, cmd: &str, first_arg: &str, stdout: &str) -> Self {
            self.routes.insert(
                (cmd.to_string(), first_arg.to_string()),
                CommandOutput { stdout: stdout.to_string(), stderr: String::new(), status: 0 },
            );
            self
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.calls
                .lock()
                .unwrap()
                .push((cmd.to_string(), args.iter().map(|s| s.to_string()).collect()));
            let first_arg = args.first().copied().unwrap_or("");
            if let Some(routed) = self.routes.get(&(cmd.to_string(), first_arg.to_string())) {
                return Ok(routed.clone());
            }
            Ok(self.response.clone())
        }
    }
```

- [ ] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: PASS, all existing tests plus the 1 new one (every pre-existing `FakeRunner::new`/`failing` call site is unaffected — no routes means every call falls back to `response`, identical to today's behavior)

- [ ] **Step 5: Commit**

```bash
git add src/exec/mod.rs
git commit -m "test: add per-command routing to FakeRunner"
```

---

### Task 2: `zfs::disk` module

**Files:**
- Create: `src/zfs/disk.rs`
- Modify: `src/zfs/mod.rs`

**Interfaces:**
- Consumes: `Result` from `src/error.rs`; `CommandRunner` from `src/exec/mod.rs`; `run_checked` from `src/zfs/mod.rs`; `is_leaf_disk`, `VdevLine`, `list_pools`, `get_pool_vdevs` from `src/zfs/pool.rs` (all already exist).
- Produces: `pub struct DiskInfo { pub name: String, pub size: u64, pub membership: DiskMembership }` (Debug, Clone, PartialEq); `pub enum DiskMembership { Available, InPool { pool: String, vdev: String } }` (Debug, Clone, PartialEq); `pub fn list_disks(runner: &dyn CommandRunner) -> Result<Vec<DiskInfo>>`; `pub fn annotate_membership(disks: &mut [DiskInfo], pool: &str, vdevs: &[VdevLine])`; `pub fn list_disks_with_membership(runner: &dyn CommandRunner) -> Result<Vec<DiskInfo>>`.

- [ ] **Step 1: Write `src/zfs/disk.rs` with its tests**

```rust
use crate::error::Result;
use crate::exec::CommandRunner;
use crate::zfs::pool::{self, is_leaf_disk, VdevLine};

#[derive(Debug, Clone, PartialEq)]
pub struct DiskInfo {
    pub name: String,
    pub size: u64,
    pub membership: DiskMembership,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiskMembership {
    Available,
    InPool { pool: String, vdev: String },
}

/// Parses `geom disk list`'s block format — tracks the current disk
/// from each `Geom name: <x>` line, and reads the first `Mediasize:
/// <bytes> (...)` line inside that block for size. Not a clean `-H -p`
/// tabular format, so parsed leniently line-by-line, the same approach
/// `parse_pool_status_vdevs` already uses for `zpool status`. A block
/// with no `Mediasize:` line at all (some virtual/optical geoms omit
/// it) still produces a `DiskInfo` with `size: 0`, rather than being
/// silently dropped.
fn parse_disk_list(output: &str) -> Vec<DiskInfo> {
    let mut disks = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_size: Option<u64> = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix("Geom name:") {
            if let Some(name) = current_name.take() {
                disks.push(DiskInfo { name, size: current_size.take().unwrap_or(0), membership: DiskMembership::Available });
            }
            current_name = Some(name.trim().to_string());
        } else if current_size.is_none() {
            if let Some(rest) = trimmed.strip_prefix("Mediasize:") {
                current_size = rest.trim().split_whitespace().next().and_then(|s| s.parse().ok());
            }
        }
    }
    if let Some(name) = current_name {
        disks.push(DiskInfo { name, size: current_size.unwrap_or(0), membership: DiskMembership::Available });
    }
    disks
}

pub fn list_disks(runner: &dyn CommandRunner) -> Result<Vec<DiskInfo>> {
    let output = super::run_checked(runner, "geom", &["disk", "list"], "geom disk list")?;
    Ok(parse_disk_list(&output.stdout))
}

/// Walks `vdevs` (the same depth-tagged shape `get_pool_vdevs` produces)
/// and, for each leaf disk matching a `DiskInfo` in `disks`, sets its
/// membership to the owning pool and the nearest enclosing group's
/// name (or the disk's own name if it's a bare top-level vdev with no
/// group, e.g. a stripe member). Depth 0 is the pool's own name line
/// (not a vdev); depth 1 is either a group node (`mirror-0`,
/// `raidz1-0`, ...) or a bare top-level disk; depth 2+ are a group's
/// children.
pub fn annotate_membership(disks: &mut [DiskInfo], pool: &str, vdevs: &[VdevLine]) {
    let mut current_group: Option<String> = None;
    for vdev in vdevs {
        if vdev.depth == 0 {
            continue;
        }
        if vdev.depth == 1 {
            current_group = Some(vdev.name.clone());
        }
        if !is_leaf_disk(&vdev.name) {
            continue;
        }
        let vdev_name = if vdev.depth == 1 {
            vdev.name.clone()
        } else {
            current_group.clone().unwrap_or_else(|| vdev.name.clone())
        };
        if let Some(disk) = disks.iter_mut().find(|d| d.name == vdev.name) {
            disk.membership = DiskMembership::InPool { pool: pool.to_string(), vdev: vdev_name };
        }
    }
}

/// Fetches every disk on the host and annotates each with its pool/vdev
/// membership by cross-referencing every pool currently on the host.
/// Self-contained — correct even if the caller has never separately
/// fetched the pool list this session.
pub fn list_disks_with_membership(runner: &dyn CommandRunner) -> Result<Vec<DiskInfo>> {
    let mut disks = list_disks(runner)?;
    let pools = pool::list_pools(runner)?;
    for p in &pools {
        let vdevs = pool::get_pool_vdevs(runner, &p.name)?;
        annotate_membership(&mut disks, &p.name, &vdevs);
    }
    Ok(disks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;
    use crate::zfs::pool::PoolHealth;

    const GEOM_DISK_LIST_FIXTURE: &str = "\
Geom name: ada0
Providers:
1. Name: ada0
   Mediasize: 48318382080 (45G)
   Sectorsize: 512
   Mode: r3w3e6
   descr: QEMU HARDDISK
   ident: o3q4Qrvy
   rotationrate: unknown
   fwsectors: 63
   fwheads: 16
Geom name: da0
Providers:
1. Name: da0
   Mediasize: 268435456 (256M)
   Sectorsize: 512
   Mode: r1w1e1
   descr: Virtio Block Device
   ident: (null)
   rotationrate: unknown
   fwsectors: 63
   fwheads: 16
Geom name: da1
Providers:
1. Name: da1
   Mediasize: 268435456 (256M)
   Sectorsize: 512
   Mode: r1w1e1
   descr: Virtio Block Device
   ident: (null)
   rotationrate: unknown
   fwsectors: 63
   fwheads: 16
";

    const GEOM_DISK_LIST_MISSING_MEDIASIZE_FIXTURE: &str = "\
Geom name: cd0
Providers:
1. Name: cd0
   Sectorsize: 2048
   Mode: r0w0e0
   descr: (unknown device)
";

    #[test]
    fn parses_multi_disk_geom_disk_list_fixture() {
        let disks = parse_disk_list(GEOM_DISK_LIST_FIXTURE);
        assert_eq!(
            disks,
            vec![
                DiskInfo { name: "ada0".to_string(), size: 48_318_382_080, membership: DiskMembership::Available },
                DiskInfo { name: "da0".to_string(), size: 268_435_456, membership: DiskMembership::Available },
                DiskInfo { name: "da1".to_string(), size: 268_435_456, membership: DiskMembership::Available },
            ]
        );
    }

    #[test]
    fn parses_disk_block_with_no_mediasize_line_as_zero_size_instead_of_dropping_it() {
        let disks = parse_disk_list(GEOM_DISK_LIST_MISSING_MEDIASIZE_FIXTURE);
        assert_eq!(disks, vec![DiskInfo { name: "cd0".to_string(), size: 0, membership: DiskMembership::Available }]);
    }

    #[test]
    fn list_disks_uses_correct_command_and_parses_result() {
        let runner = FakeRunner::new(GEOM_DISK_LIST_FIXTURE);
        let disks = list_disks(&runner).unwrap();
        assert_eq!(disks.len(), 3);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("geom".to_string(), vec!["disk".to_string(), "list".to_string()]));
    }

    #[test]
    fn list_disks_surfaces_command_failure() {
        let runner = FakeRunner::failing("geom: no such class", 1);
        let result = list_disks(&runner);
        assert!(result.is_err());
    }

    #[test]
    fn annotate_membership_tags_leaf_disks_nested_under_a_mirror_group() {
        let mut disks = vec![
            DiskInfo { name: "ada0".to_string(), size: 0, membership: DiskMembership::Available },
            DiskInfo { name: "da0".to_string(), size: 0, membership: DiskMembership::Available },
            DiskInfo { name: "da1".to_string(), size: 0, membership: DiskMembership::Available },
        ];
        let vdevs = vec![
            VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 1, name: "mirror-0".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 2, name: "da0".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 2, name: "da1".to_string(), health: PoolHealth::Online },
        ];
        annotate_membership(&mut disks, "tank", &vdevs);
        assert_eq!(disks[0].membership, DiskMembership::Available, "ada0 isn't in this pool's vdev tree, must stay Available");
        assert_eq!(disks[1].membership, DiskMembership::InPool { pool: "tank".to_string(), vdev: "mirror-0".to_string() });
        assert_eq!(disks[2].membership, DiskMembership::InPool { pool: "tank".to_string(), vdev: "mirror-0".to_string() });
    }

    #[test]
    fn annotate_membership_tags_bare_top_level_disk_with_its_own_name() {
        let mut disks = vec![DiskInfo { name: "da0".to_string(), size: 0, membership: DiskMembership::Available }];
        let vdevs = vec![
            VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 1, name: "da0".to_string(), health: PoolHealth::Online },
        ];
        annotate_membership(&mut disks, "tank", &vdevs);
        assert_eq!(disks[0].membership, DiskMembership::InPool { pool: "tank".to_string(), vdev: "da0".to_string() });
    }

    const ZPOOL_LIST_TANK_FIXTURE: &str = "tank\t4000000000000\t1200000000000\t2800000000000\tONLINE\n";
    const ZPOOL_STATUS_TANK_FIXTURE: &str = "\
  pool: tank
 state: ONLINE
config:

\tNAME        STATE     READ WRITE CKSUM
\ttank        ONLINE       0     0     0
\t  mirror-0  ONLINE       0     0     0
\t    da0     ONLINE       0     0     0
\t    da1     ONLINE       0     0     0

errors: No known data errors
";

    #[test]
    fn list_disks_with_membership_cross_references_the_hosts_pools() {
        let runner = FakeRunner::new("")
            .route("geom", "disk", GEOM_DISK_LIST_FIXTURE)
            .route("zpool", "list", ZPOOL_LIST_TANK_FIXTURE)
            .route("zpool", "status", ZPOOL_STATUS_TANK_FIXTURE);
        let disks = list_disks_with_membership(&runner).unwrap();
        assert_eq!(disks.len(), 3);
        assert_eq!(disks.iter().find(|d| d.name == "ada0").unwrap().membership, DiskMembership::Available);
        assert_eq!(
            disks.iter().find(|d| d.name == "da0").unwrap().membership,
            DiskMembership::InPool { pool: "tank".to_string(), vdev: "mirror-0".to_string() }
        );
    }

    #[test]
    fn list_disks_with_membership_surfaces_command_failure() {
        let runner = FakeRunner::failing("geom: no such class", 1);
        let result = list_disks_with_membership(&runner);
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Declare the module in `src/zfs/mod.rs`**

```rust
pub mod dataset;
pub mod disk;
pub mod pool;
pub mod property;
pub mod snapshot;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test zfs::disk`
Expected: PASS (9 tests)

- [ ] **Step 4: Commit**

```bash
git add src/zfs/disk.rs src/zfs/mod.rs
git commit -m "feat: zfs::disk module — geom disk list parsing and pool/vdev membership"
```

---

### Task 3: `zpool resilver` library function

**Files:**
- Modify: `src/zfs/pool.rs`

**Interfaces:**
- Consumes: `run_checked` from `src/zfs/mod.rs`.
- Produces: `pub fn resilver_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()>`.

- [ ] **Step 1: Write failing tests**

Add to `src/zfs/pool.rs`'s test module, near the `scrub_pool` tests:

```rust
#[test]
fn resilver_pool_runs_expected_command() {
    let runner = FakeRunner::new("");
    resilver_pool(&runner, "tank").unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0], ("zpool".to_string(), vec!["resilver".to_string(), "--".to_string(), "tank".to_string()]));
}

#[test]
fn resilver_pool_surfaces_command_failure() {
    let runner = FakeRunner::failing("no valid replicas", 1);
    let result = resilver_pool(&runner, "tank");
    assert!(result.is_err());
}

#[test]
fn guards_against_pool_names_that_look_like_flags_in_resilver() {
    let runner = FakeRunner::new("");
    resilver_pool(&runner, "-f").unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].1[1], "--", "a leading '--' must precede the pool name so zpool cannot parse it as an option");
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test resilver_pool`
Expected: FAIL (`resilver_pool` doesn't exist yet)

- [ ] **Step 3: Add `resilver_pool`**

Add after `scrub_pool`:

```rust
/// Runs `zpool resilver <pool>` — restarts an in-progress resilver from
/// the beginning (or starts one, if a device needs it and none is
/// running). Confirmed via `man zpool-resilver` and empirically against
/// a real `zpool` binary that `--` is accepted as an end-of-options
/// marker here, same as `scrub_pool` — no disk argument, this is
/// pool-scoped.
pub fn resilver_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()> {
    super::run_checked(runner, "zpool", &["resilver", "--", pool], &format!("zpool resilver {pool}"))?;
    Ok(())
}
```

- [ ] **Step 4: Run the pool tests**

Run: `cargo test zfs::pool`
Expected: PASS, all existing tests plus the 3 new ones

- [ ] **Step 5: Commit**

```bash
git add src/zfs/pool.rs
git commit -m "feat: zpool resilver library function"
```

---

### Task 4: `MutationCall::ResilverPool`

**Files:**
- Modify: `src/ui/modal.rs`

**Interfaces:**
- Consumes: `resilver_pool` from `src/zfs/pool.rs` (Task 3).
- Produces: `MutationCall::ResilverPool { pool: String }`.

This is self-contained within `modal.rs` (the new variant and its `run()` delegation arm land together) — the `Modal::Confirm` that builds this call is deferred to Task 6, alongside the other new `state.rs` keybinding work.

- [ ] **Step 1: Write the failing test**

Add to `src/ui/modal.rs`'s test module, near `scrub_pool_call_runs_scrub_pool`:

```rust
#[test]
fn resilver_pool_call_runs_resilver_pool() {
    let runner = FakeRunner::new("");
    let call = MutationCall::ResilverPool { pool: "tank".to_string() };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0], ("zpool".to_string(), vec!["resilver".to_string(), "--".to_string(), "tank".to_string()]));
}
```

- [ ] **Step 2: Run the test to confirm it fails**

Run: `cargo test resilver_pool_call`
Expected: FAIL (`MutationCall::ResilverPool` doesn't exist yet)

- [ ] **Step 3: Add the variant and its `run()` arm**

In the `MutationCall` enum, add after `ScrubPool { pool: String },`:

```rust
    ResilverPool { pool: String },
```

In `MutationCall::run`'s match, add after the `ScrubPool` arm:

```rust
            MutationCall::ResilverPool { pool } => crate::zfs::pool::resilver_pool(runner, pool),
```

- [ ] **Step 4: Run the modal tests**

Run: `cargo test ui::modal`
Expected: PASS, all existing tests plus the 1 new one

- [ ] **Step 5: Commit**

```bash
git add src/ui/modal.rs
git commit -m "feat: MutationCall::ResilverPool"
```

---

### Task 5: `Section::Disks` — state, refresh, and rendering

**Files:**
- Modify: `src/ui/state.rs`, `src/ui/layout.rs`, `src/ui/mod.rs`

**Interfaces:**
- Consumes: `DiskInfo`, `DiskMembership`, `list_disks_with_membership` from `src/zfs/disk.rs` (Task 2).
- Produces: `Section::Disks` variant. `App` fields `pub disks: Vec<DiskInfo>`, `pub disks_table: TableState`, `pub selected_disks: HashSet<String>`. `WorkerMsg::Disks(usize, Result<Vec<DiskInfo>>)`. `App::refresh_disks(&mut self, runner: Arc<dyn CommandRunner>)`. `draw_disks` (private to `layout.rs`).

Because adding `Section::Disks` to the enum makes `layout.rs`'s and `ui/mod.rs`'s exhaustive `match`es over `Section` fail to compile, this task updates all three files together — the crate must build and all tests must pass at the end of this single task, same as every other task.

- [ ] **Step 1: Write failing tests**

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn refresh_disks_populates_disks_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    let runner: Arc<dyn CommandRunner> = Arc::new(
        FakeRunner::new("")
            .route("geom", "disk", "Geom name: da0\nProviders:\n1. Name: da0\n   Mediasize: 268435456 (256M)\n")
            .route("zpool", "list", "tank\t0\t0\t0\tONLINE\n")
            .route(
                "zpool",
                "status",
                "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME  STATE     READ WRITE CKSUM\n\ttank  ONLINE       0     0     0\n\t  da0 ONLINE       0     0     0\n\nerrors: No known data errors\n",
            ),
    );
    app.refresh_disks(runner);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.disks.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.disks.len(), 1);
    assert_eq!(
        app.disks[0].membership,
        crate::zfs::disk::DiskMembership::InPool { pool: "tank".to_string(), vdev: "da0".to_string() }
    );
}

#[test]
fn refresh_disks_does_not_spawn_second_thread_while_in_flight() {
    let mut app = App::new(vec!["local".to_string()]);
    let fake = Arc::new(
        FakeRunner::new("")
            .route("geom", "disk", "Geom name: da0\nProviders:\n1. Name: da0\n   Mediasize: 268435456 (256M)\n")
            .route("zpool", "list", "")
            .route("zpool", "status", ""),
    );
    let runner: Arc<dyn CommandRunner> = fake.clone();
    app.refresh_disks(runner.clone());
    app.refresh_disks(runner.clone());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.disks.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fake.calls.lock().unwrap().len(), 2, "geom disk list + zpool list; zpool status is skipped since the empty zpool list fixture has no pools");
}

#[test]
fn disks_result_for_a_stale_host_is_discarded() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.tx
        .send(WorkerMsg::Disks(1, Ok(vec![crate::zfs::disk::DiskInfo {
            name: "da0".to_string(),
            size: 0,
            membership: crate::zfs::disk::DiskMembership::Available,
        }])))
        .unwrap();
    app.drain_worker_messages();
    assert!(app.disks.is_empty(), "a result for a host that isn't selected must not populate the pane");
}
```

Also extend the existing `switching_host_resets_pane_focus_and_clears_properties` test — add these lines right before its final `}`:

```rust
    app.disks = vec![crate::zfs::disk::DiskInfo { name: "da0".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available }];
    app.selected_disks.insert("da0".to_string());
```

placed immediately after the existing `app.pool_properties = vec![...]` line, and add these assertions at the end of the test (after the existing `assert_eq!(app.dataset_properties_table.selected(), None);` line):

```rust
    assert!(app.disks.is_empty());
    assert_eq!(app.disks_table.selected(), None);
    assert!(app.selected_disks.is_empty());
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test refresh_disks_populates_disks_from_fake_runner`
Expected: FAIL (`Section::Disks`, `App::disks`, `App::refresh_disks` don't exist yet)

- [ ] **Step 3: Add `Section::Disks`, the new `App` fields, `WorkerMsg::Disks`, and update `App::new`**

Add `use crate::zfs::disk::DiskInfo;` to the top-level imports (alongside the existing `use crate::zfs::dataset::Dataset;` line).

Change the `Section` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Hosts,
    Pools,
    Datasets,
    Snapshots,
    Disks,
    Vdevs,
    PoolProperties,
    DatasetProperties,
}
```

Add a field to `WorkerMsg`, after `Snapshots`:

```rust
    Disks(usize, Result<Vec<DiskInfo>>),
```

Add three fields to the `App` struct, after `snapshots_table: TableState,`:

```rust
    pub disks: Vec<DiskInfo>,
    pub disks_table: TableState,
    /// Names of disks currently selected (via Space, in the Disks
    /// section) to build a pool or vdev from. Only ever contains names
    /// of disks whose membership is `Available` — see `drain_worker_messages`'s
    /// `WorkerMsg::Disks` arm for how a disk that stops being available
    /// (or disappears) is dropped from this set.
    pub selected_disks: HashSet<String>,
```

Add `Section::Disks` to `App::new`'s `sections` field:

```rust
            sections: vec![Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots, Section::Disks],
```

Initialize the three new fields in `App::new`, after `snapshots_table: TableState::default(),`:

```rust
            disks: Vec::new(),
            disks_table: TableState::default(),
            selected_disks: HashSet::new(),
```

- [ ] **Step 4: Update `clear_host_data`**

Add three lines, after `self.snapshots_table.select(None);`:

```rust
        self.disks.clear();
        self.disks_table.select(None);
        self.selected_disks.clear();
```

- [ ] **Step 5: Add `refresh_disks`**

Add after `refresh_snapshots`:

```rust
    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight. Fetches every disk on the host
    /// and its pool/vdev membership in one call — see
    /// `zfs::disk::list_disks_with_membership`.
    pub fn refresh_disks(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Disks, host) {
            self.pending_refresh.insert((Section::Disks, host));
            return;
        }
        self.begin_refresh(Section::Disks, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::disk::list_disks_with_membership(runner.as_ref());
            let _ = tx.send(WorkerMsg::Disks(host, result));
        });
    }
```

- [ ] **Step 6: Add the `WorkerMsg::Disks` arm to `drain_worker_messages`**

Add after the `WorkerMsg::Snapshots` arm:

```rust
                WorkerMsg::Disks(host, result) => {
                    self.end_refresh(Section::Disks, host);
                    let had_pending = self.pending_refresh.remove(&(Section::Disks, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::Disks);
                    }
                    match result {
                        Ok(disks) => {
                            // Drop any selected disk that no longer
                            // exists, or has stopped being Available
                            // (claimed by a concurrent operation, or a
                            // slow refresh landing late) — a stale
                            // selection must not be submittable against
                            // a device that's no longer free.
                            self.selected_disks.retain(|name| {
                                disks.iter().any(|d| &d.name == name && d.membership == DiskMembership::Available)
                            });
                            self.disks = disks;
                            clamp_table_selection(&mut self.disks_table, self.disks.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
```

This requires `DiskMembership` to implement `PartialEq` (already does, from Task 2) and to be in scope — add `use crate::zfs::disk::DiskMembership;` to the top-level imports alongside `use crate::zfs::disk::DiskInfo;` from Step 3.

- [ ] **Step 7: Apply the three mechanical test fixes for the new last section**

`Section::Disks` is now the last section, so `BackTab` from `Hosts` wraps to `Disks`, not `Snapshots`. Three pre-existing tests hardcode the old wrap target:

In `handle_key_navigates_sections_and_wraps`, change the final assertion:

```rust
        assert_eq!(app.current_section(), Section::Disks);
```

In `backtab_cycles_sections_backward`, change the assertion:

```rust
        assert_eq!(app.current_section(), Section::Disks, "must wrap backward from the first section to the last");
```

In `handle_key_navigates_snapshot_rows_and_wraps`, this test needs to reach `Snapshots` specifically (to test snapshot-row wrapping), which now takes two `BackTab`s instead of one. Change:

```rust
        app.handle_key(KeyCode::BackTab); // Hosts -> Snapshots (wraps backward)
        assert_eq!(app.current_section(), Section::Snapshots);
```

to:

```rust
        app.handle_key(KeyCode::BackTab); // Hosts -> Disks (wraps backward)
        app.handle_key(KeyCode::BackTab); // Disks -> Snapshots
        assert_eq!(app.current_section(), Section::Snapshots);
```

- [ ] **Step 8: Update `layout.rs`**

Add `use crate::zfs::disk::DiskMembership;` to the top-level imports.

In `draw_sidebar`'s label `match`, add:

```rust
                Section::Disks => "Disks",
```

In `draw_main`'s final `match section { ... }` block, add a new arm and remove `Section::Disks` from being unhandled:

```rust
    match section {
        Section::Hosts => draw_hosts(f, app, area),
        Section::Snapshots => draw_snapshots(f, app, area),
        Section::Disks => draw_disks(f, app, area),
        Section::Pools | Section::Datasets => unreachable!("handled above"),
        Section::Vdevs | Section::PoolProperties | Section::DatasetProperties => {
            unreachable!("never in App::sections")
        }
    }
```

Add a new function, after `draw_snapshots`:

```rust
fn draw_disks(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.disks.iter().map(|d| {
        let marker = if app.selected_disks.contains(&d.name) { "[x]" } else { "[ ]" };
        let status = match &d.membership {
            DiskMembership::Available => "available".to_string(),
            DiskMembership::InPool { pool, vdev } => format!("in {pool} ({vdev})"),
        };
        Row::new(vec![format!("{marker} {}", d.name), d.size.to_string(), status])
    });
    let widths = [Constraint::Length(18), Constraint::Length(14), Constraint::Min(20)];
    let title = format!(
        "Disks — {} ('r' refresh, Space select, 'n' create pool, 'a' add vdev, Esc clear selection)",
        app.active_host_name()
    );
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Name", "Size", "Status"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.disks_table);
}
```

- [ ] **Step 9: Update `ui/mod.rs`**

In the queued-refresh dispatch `match section { ... }` (near the top of `event_loop`), add after the `Section::Snapshots` arm:

```rust
                    Section::Disks => app.refresh_disks(host.runner.clone()),
```

In the `'r'`-key handling's `else` branch (the `match app.current_section() { Section::Pools => ..., Section::Datasets => ..., Section::Snapshots => ..., Section::Hosts | Section::Vdevs | Section::PoolProperties | Section::DatasetProperties => {} }`), add a new arm after `Section::Snapshots => app.refresh_snapshots(host.runner.clone()),` — the trailing `Section::Hosts | Section::Vdevs | ...` catch-all needs no change, since `Section::Disks` is now named explicitly instead of falling into it:

```rust
                                    Section::Disks => app.refresh_disks(host.runner.clone()),
```

- [ ] **Step 10: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, all existing tests plus the 3 new ones from Step 1 (the 3 mechanical fixes and the extended host-switch test don't add new tests, they fix/extend existing ones); release build clean aside from pre-existing dead_code warnings unrelated to this task

- [ ] **Step 11: Commit**

```bash
git add src/ui/state.rs src/ui/layout.rs src/ui/mod.rs
git commit -m "feat: Section::Disks — state, refresh, and rendering"
```

---

### Task 6: Disks section interactivity, and the resilver keybinding

**Files:**
- Modify: `src/ui/modal.rs`, `src/ui/state.rs`

**Interfaces:**
- Consumes: `disks`, `disks_table`, `selected_disks` from `src/ui/state.rs` (Task 5); `MutationCall::ResilverPool` from `src/ui/modal.rs` (Task 4); `MutationCall::CreatePool`/`AddVdev` (pre-existing).
- Produces: `FormKind::CreatePoolFromDisks { host: usize, disks: Vec<String> }`, `FormKind::AddVdevFromDisks { host: usize, disks: Vec<String> }`. `handle_key` arms for Up/Down/Space/`'n'`/`'a'`/Esc in the Disks section, and `'R'` in the Pools section. `build_pending_action` arms for the two new `FormKind`s, plus a private helper `fn build_vdev_spec_from_selection(redundancy: &str, disks: &[String]) -> Vec<String>`.

- [ ] **Step 1: Write failing tests**

Add to `src/ui/modal.rs`'s `FormKind` enum documentation is covered by Step 3 below (modal.rs has no tests for `FormKind` itself, matching the existing pattern — `FormKind` variants are only tested via `state.rs`'s `build_pending_action`).

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn space_toggles_selection_on_available_disk() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.handle_key(KeyCode::Tab); // -> Snapshots
    app.handle_key(KeyCode::Tab); // -> Disks
    app.disks = vec![crate::zfs::disk::DiskInfo { name: "da0".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available }];
    app.disks_table.select(Some(0));
    app.handle_key(KeyCode::Char(' '));
    assert!(app.selected_disks.contains("da0"));
    app.handle_key(KeyCode::Char(' '));
    assert!(!app.selected_disks.contains("da0"), "pressing Space again deselects it");
}

#[test]
fn space_on_claimed_disk_sets_hint_and_does_not_select() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.disks = vec![crate::zfs::disk::DiskInfo {
        name: "da0".to_string(),
        size: 0,
        membership: crate::zfs::disk::DiskMembership::InPool { pool: "tank".to_string(), vdev: "mirror-0".to_string() },
    }];
    app.disks_table.select(Some(0));
    app.handle_key(KeyCode::Char(' '));
    assert!(app.selected_disks.is_empty());
    assert_eq!(app.status_message, Some("disk 'da0' already in pool 'tank'".to_string()));
}

#[test]
fn esc_clears_disk_selection_in_disks_section() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.selected_disks.insert("da0".to_string());
    app.handle_key(KeyCode::Esc);
    assert!(app.selected_disks.is_empty());
}

#[test]
fn up_down_in_disks_section_moves_disks_table_selection() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.disks = vec![
        crate::zfs::disk::DiskInfo { name: "da0".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available },
        crate::zfs::disk::DiskInfo { name: "da1".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available },
    ];
    app.handle_key(KeyCode::Down);
    assert_eq!(app.disks_table.selected(), Some(0));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.disks_table.selected(), Some(1));
}

#[test]
fn handle_key_n_in_disks_with_selection_opens_create_pool_from_disks_form() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.selected_disks.insert("da1".to_string());
    app.selected_disks.insert("da0".to_string());
    app.handle_key(KeyCode::Char('n'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 2);
            assert_eq!(
                *kind,
                FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string(), "da1".to_string()] },
                "disks must be sorted for a deterministic snapshot"
            );
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_n_in_disks_without_selection_sets_hint() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message, Some("select at least one disk with Space first".to_string()));
}

#[test]
fn handle_key_a_in_disks_with_selection_opens_add_vdev_from_disks_form() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.selected_disks.insert("da0".to_string());
    app.handle_key(KeyCode::Char('a'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 2);
            assert_eq!(*kind, FormKind::AddVdevFromDisks { host: 0, disks: vec!["da0".to_string()] });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_a_in_disks_without_selection_sets_hint() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.handle_key(KeyCode::Char('a'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message, Some("select at least one disk with Space first".to_string()));
}

#[test]
fn build_pending_action_create_pool_from_disks_blank_redundancy_stripes_each_disk() {
    let kind = FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string(), "da1".to_string()] };
    let fields = vec![
        FormField { label: "Name".to_string(), value: "tank".to_string() },
        FormField { label: "Redundancy".to_string(), value: String::new() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action,
        PendingAction {
            host: 0,
            section: Section::Pools,
            call: MutationCall::CreatePool { name: "tank".to_string(), vdev_spec: vec!["da0".to_string(), "da1".to_string()] },
        }
    );
}

#[test]
fn build_pending_action_create_pool_from_disks_with_redundancy_groups_all_disks() {
    let kind = FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string(), "da1".to_string()] };
    let fields = vec![
        FormField { label: "Name".to_string(), value: "tank".to_string() },
        FormField { label: "Redundancy".to_string(), value: "mirror".to_string() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action.call,
        MutationCall::CreatePool { name: "tank".to_string(), vdev_spec: vec!["mirror".to_string(), "da0".to_string(), "da1".to_string()] }
    );
}

#[test]
fn build_pending_action_create_pool_from_disks_rejects_empty_name() {
    let kind = FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string()] };
    let fields = vec![
        FormField { label: "Name".to_string(), value: "  ".to_string() },
        FormField { label: "Redundancy".to_string(), value: String::new() },
    ];
    assert_eq!(build_pending_action(&kind, &fields), None);
}

#[test]
fn build_pending_action_add_vdev_from_disks() {
    let kind = FormKind::AddVdevFromDisks { host: 0, disks: vec!["da2".to_string(), "da3".to_string()] };
    let fields = vec![
        FormField { label: "Target pool name".to_string(), value: "tank".to_string() },
        FormField { label: "Redundancy".to_string(), value: "mirror".to_string() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action,
        PendingAction {
            host: 0,
            section: Section::Vdevs,
            call: MutationCall::AddVdev { pool: "tank".to_string(), vdev_spec: vec!["mirror".to_string(), "da2".to_string(), "da3".to_string()] },
        }
    );
}

#[test]
fn selected_disks_cleared_after_create_pool_from_disks_submit() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.selected_disks.insert("da0".to_string());
    app.handle_key(KeyCode::Char('n'));
    for c in "tank".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab); // move to the Redundancy field
    let result = app.handle_modal_key(KeyCode::Enter);
    assert!(result.is_some());
    assert!(app.selected_disks.is_empty());
}

#[test]
fn handle_key_shift_r_on_pools_opens_resilver_confirm_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab); // -> Pools
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Char('R'));
    match &app.modal {
        Some(Modal::Confirm { action, .. }) => {
            assert_eq!(action.call, MutationCall::ResilverPool { pool: "tank".to_string() });
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn mutation_done_for_pools_section_also_queues_disks_refresh() {
    let mut app = App::new(vec!["local".to_string()]);
    app.tx.send(WorkerMsg::MutationDone(0, Section::Pools, Ok(()))).unwrap();
    let refreshes = app.drain_worker_messages();
    assert!(refreshes.contains(&Section::Pools));
    assert!(refreshes.contains(&Section::Disks), "a Pools-section mutation (create/destroy pool) changes disk membership too");
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test handle_key_n_in_disks_with_selection`
Expected: FAIL (`FormKind::CreatePoolFromDisks` doesn't exist yet)

- [ ] **Step 3: Add the two new `FormKind` variants**

In `src/ui/modal.rs`'s `FormKind` enum, add after `ReplaceDisk { host: usize, pool: String, old_disk: String },`:

```rust
    /// Fields: `[name, redundancy]` — `redundancy` blank means a stripe
    /// (each disk its own top-level vdev); `disks` is fixed at
    /// modal-open time from the Disks section's current selection.
    CreatePoolFromDisks { host: usize, disks: Vec<String> },
    /// Fields: `[pool, redundancy]` — same `redundancy` convention as
    /// `CreatePoolFromDisks`.
    AddVdevFromDisks { host: usize, disks: Vec<String> },
```

- [ ] **Step 4: Add the `handle_key` arms**

Add the Esc-clears-selection arm to `src/ui/state.rs`'s `handle_key`, directly **before** the existing `KeyCode::Esc => self.status_message = None,` catch-all (this ordering matters — a guarded arm placed after an unconditional one is unreachable):

```rust
            KeyCode::Esc if self.current_section() == Section::Disks && !self.selected_disks.is_empty() => {
                self.selected_disks.clear();
            }
```

Add the resilver arm, directly after the existing `'s'` scrub arm (`KeyCode::Char('s') if self.current_section() == Section::Pools && ...`):

```rust
            KeyCode::Char('R') if self.current_section() == Section::Pools && self.drilled_pool.is_none() && self.pane_focus == PaneFocus::Middle => {
                if let Some(i) = self.pools_table.selected() {
                    if let Some(pool) = self.pools.get(i) {
                        self.modal = Some(Modal::Confirm {
                            prompt: format!("Resilver pool '{}'? (y/n)", pool.name),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Pools,
                                call: MutationCall::ResilverPool { pool: pool.name.clone() },
                            },
                        });
                    }
                }
            }
```

Add the Disks section's arms, directly before the final `_ => {}` catch-all:

```rust
            KeyCode::Down if self.current_section() == Section::Disks => {
                move_table_selection(&mut self.disks_table, self.disks.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Disks => {
                move_table_selection(&mut self.disks_table, self.disks.len(), false);
            }
            KeyCode::Char(' ') if self.current_section() == Section::Disks => {
                if let Some(i) = self.disks_table.selected() {
                    if let Some(disk) = self.disks.get(i) {
                        match &disk.membership {
                            crate::zfs::disk::DiskMembership::Available => {
                                if !self.selected_disks.remove(&disk.name) {
                                    self.selected_disks.insert(disk.name.clone());
                                }
                            }
                            crate::zfs::disk::DiskMembership::InPool { pool, .. } => {
                                self.status_message = Some(format!("disk '{}' already in pool '{pool}'", disk.name));
                            }
                        }
                    }
                }
            }
            KeyCode::Char('n') if self.current_section() == Section::Disks && !self.selected_disks.is_empty() => {
                let mut disks: Vec<String> = self.selected_disks.iter().cloned().collect();
                disks.sort();
                self.modal = Some(Modal::Form {
                    title: format!("Create Pool from {} selected disk(s)", disks.len()),
                    fields: vec![
                        FormField { label: "Name".to_string(), value: String::new() },
                        FormField { label: "Redundancy (blank=stripe, or mirror/raidz1/raidz2/raidz3)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    kind: FormKind::CreatePoolFromDisks { host: self.selected_host, disks },
                });
            }
            KeyCode::Char('n') if self.current_section() == Section::Disks => {
                self.status_message = Some("select at least one disk with Space first".to_string());
            }
            KeyCode::Char('a') if self.current_section() == Section::Disks && !self.selected_disks.is_empty() => {
                let mut disks: Vec<String> = self.selected_disks.iter().cloned().collect();
                disks.sort();
                self.modal = Some(Modal::Form {
                    title: format!("Add vdev from {} selected disk(s)", disks.len()),
                    fields: vec![
                        FormField { label: "Target pool name".to_string(), value: String::new() },
                        FormField { label: "Redundancy (blank=stripe, or mirror/raidz1/raidz2/raidz3)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    kind: FormKind::AddVdevFromDisks { host: self.selected_host, disks },
                });
            }
            KeyCode::Char('a') if self.current_section() == Section::Disks => {
                self.status_message = Some("select at least one disk with Space first".to_string());
            }
```

- [ ] **Step 5: Clear `selected_disks` on successful form submit**

In `handle_modal_key`'s `Modal::Form` arm, replace:

```rust
                KeyCode::Enter => {
                    if focused + 1 < fields.len() {
                        focused += 1;
                        self.modal = Some(Modal::Form { title, fields, focused, kind });
                        None
                    } else {
                        match build_pending_action(&kind, &fields) {
                            Some(action) => Some(action),
                            None => {
                                self.modal = Some(Modal::Form { title, fields, focused, kind });
                                None
                            }
                        }
                    }
                }
```

with:

```rust
                KeyCode::Enter => {
                    if focused + 1 < fields.len() {
                        focused += 1;
                        self.modal = Some(Modal::Form { title, fields, focused, kind });
                        None
                    } else {
                        match build_pending_action(&kind, &fields) {
                            Some(action) => {
                                if matches!(kind, FormKind::CreatePoolFromDisks { .. } | FormKind::AddVdevFromDisks { .. }) {
                                    self.selected_disks.clear();
                                }
                                Some(action)
                            }
                            None => {
                                self.modal = Some(Modal::Form { title, fields, focused, kind });
                                None
                            }
                        }
                    }
                }
```

- [ ] **Step 6: Add the `build_pending_action` arms and the helper function**

Add a private helper, directly before `fn build_pending_action`:

```rust
/// Builds a `zpool create`/`zpool add` vdev spec from a Disks-section
/// selection: `disks` verbatim (each disk becomes its own top-level
/// vdev — a stripe) when `redundancy` is blank, or `[redundancy] +
/// disks` (one mirror/raidz group over every selected disk) otherwise.
fn build_vdev_spec_from_selection(redundancy: &str, disks: &[String]) -> Vec<String> {
    if redundancy.is_empty() {
        disks.to_vec()
    } else {
        std::iter::once(redundancy.to_string()).chain(disks.iter().cloned()).collect()
    }
}
```

In `build_pending_action`'s match, add after the `FormKind::CreatePool` arm:

```rust
        FormKind::CreatePoolFromDisks { host, disks } => {
            let name = fields[0].value.trim();
            if name.is_empty() {
                return None;
            }
            let redundancy = fields[1].value.trim();
            Some(PendingAction {
                host: *host,
                section: Section::Pools,
                call: MutationCall::CreatePool { name: name.to_string(), vdev_spec: build_vdev_spec_from_selection(redundancy, disks) },
            })
        }
```

Add after the `FormKind::AddVdev` arm:

```rust
        FormKind::AddVdevFromDisks { host, disks } => {
            let pool = fields[0].value.trim();
            if pool.is_empty() {
                return None;
            }
            let redundancy = fields[1].value.trim();
            Some(PendingAction {
                host: *host,
                section: Section::Vdevs,
                call: MutationCall::AddVdev { pool: pool.to_string(), vdev_spec: build_vdev_spec_from_selection(redundancy, disks) },
            })
        }
```

- [ ] **Step 7: Add the `Disks`-refresh rule to `drain_worker_messages`'s `MutationDone` arm**

Replace:

```rust
                WorkerMsg::MutationDone(host, section, result) => {
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(()) => {
                            self.status_message = None;
                            refreshes_needed.push(section);
                            if section == Section::Vdevs {
                                // A disk going offline/online/being replaced
                                // changes the parent pool's health too —
                                // the one deliberate two-section exception.
                                refreshes_needed.push(Section::Pools);
                            }
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
```

with:

```rust
                WorkerMsg::MutationDone(host, section, result) => {
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(()) => {
                            self.status_message = None;
                            refreshes_needed.push(section);
                            if section == Section::Vdevs {
                                // A disk going offline/online/being replaced
                                // changes the parent pool's health too —
                                // the one deliberate two-section exception.
                                refreshes_needed.push(Section::Pools);
                            }
                            if section == Section::Vdevs || section == Section::Pools {
                                // Both changing a pool's vdev tree
                                // (add/replace vdev) and creating/destroying
                                // a pool outright change which disks are
                                // claimed — the Disks section must catch up.
                                refreshes_needed.push(Section::Disks);
                            }
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
```

- [ ] **Step 8: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, all existing tests plus the 15 new ones from Step 1

- [ ] **Step 9: Commit**

```bash
git add src/ui/modal.rs src/ui/state.rs
git commit -m "feat: Disks section interactivity (selection, create/add-from-disks) and zpool resilver keybinding"
```

---

### Task 7: README keybindings

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update the keybindings**

In the "## Keys" intro bullet, change:

```
- `Tab` / `Shift+Tab`: switch sections (Hosts, Pools, Datasets, Snapshots) — this is now the *only* way to cycle sections; `Left`/`Right` have a different meaning (see below).
```

to:

```
- `Tab` / `Shift+Tab`: switch sections (Hosts, Pools, Datasets, Snapshots, Disks) — this is now the *only* way to cycle sections; `Left`/`Right` have a different meaning (see below).
```

In the "### Pools" section, add a bullet after `- `s`: scrub the selected pool (y/n confirm)`:

```
- `R`: resilver the selected pool (y/n confirm)
```

In the "### Vdevs (inside a drilled-into pool)" section, add a bullet after the `x` replace bullet:

```
- `a`: add a new vdev/mirror/raidz group to this pool (prompts for a vdev spec, e.g. `mirror da2 da3`)
```

Add a new "### Disks" section, after "### Snapshots" and before "## Status":

```
### Disks

- `Up` / `Down`: select a disk
- `Space`: toggle-select the highlighted disk (only if it's not already claimed by a pool)
- `n`: create a new pool from the selected disk(s) (prompts for name and redundancy — blank for a stripe, or `mirror`/`raidz1`/`raidz2`/`raidz3`)
- `a`: add a vdev to an existing pool from the selected disk(s) (prompts for the target pool name and redundancy, same as above)
- `Esc`: clear the current selection
- `r`: refresh the disk list
```

In the "## Status" paragraph, add a sentence at the end:

```
A Disks section lists every block device on the host with its size and pool/vdev membership, and lets you build a new pool or add a vdev to an existing one from a multi-disk selection instead of typing device names; `zpool resilver` is also available from the Pools section.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: document Disks section, resilver, and the previously-undocumented add-vdev key"
```
