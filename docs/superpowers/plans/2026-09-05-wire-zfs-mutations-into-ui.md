# Wire ZFS Mutations Into The TUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user create/destroy/scrub a pool, offline/online/replace a disk, create/destroy a dataset, and create/destroy/rollback a snapshot, all from the TUI (local or remote hosts), via a modal/dialog interaction model built on top of Plan 1's already-tested `zfs::*` library layer.

**Architecture:** A `Modal` enum lives in `App` (`Option<Modal>`) with three shapes — `Confirm` (y/n), `ConfirmTyped` (type-the-name), `Form` (labeled text fields) — each carrying a `PendingAction` (host + section + a `MutationCall` enum naming the concrete `zfs::*` call). Submitting a modal clears it immediately and spawns a worker thread (the same `std::thread::spawn` + `mpsc` pattern Plan 1 established for refreshes); on success the affected section is automatically re-listed. Row selection use `ratatui::widgets::TableState` (native scrolling). A new Vdev drill-down view (reached via `Enter` on a selected pool) hosts the disk-level operations. Three prerequisite fixes land first: a `run_checked()` helper to stop hand-duplicating the success/failure-check block, per-(section, host) in-flight refresh tracking (replacing the current per-section-only tracking), and the row-selection infrastructure itself.

**Tech Stack:** Rust (2021 edition), ratatui 0.29, crossterm 0.28 — no new dependencies. Same crate, same `CommandRunner`/worker-thread patterns as Plan 1.

**Spec:** `docs/superpowers/specs/2026-09-05-zfsctl-mutation-ui-design.md` (and the top-level `docs/superpowers/specs/2026-09-04-zfsctl-tui-design.md` it argues from).

## Global Constraints

- FreeBSD only — no Linux/illumos code paths.
- No `unsafe` FFI/libzfs bindings — all ZFS/system integration goes through CLI shelling, parsed via machine-readable flags, never human-formatted text.
- Single binary crate — no cargo workspace, no new dependencies (ratatui/crossterm already provide everything this plan needs: `TableState`, `Clear`, `Layout`).
- UI event loop stays synchronous ratatui + crossterm; blocking work (mutations, refreshes) moves to worker threads via `std::thread::spawn` + `mpsc` — no async runtime.
- `CommandRunner` is the sole execution abstraction; all mutation dispatch goes through `MutationCall::run(&dyn CommandRunner)`, never a fresh ad-hoc `std::process::Command`.
- Every caller-supplied ZFS/pool/disk name reaches `zpool`/`zfs` through the existing `--`-guarded functions in `zfs::pool`/`zfs::dataset`/`zfs::snapshot` — this plan adds no new raw `runner.run()` call sites outside `run_checked()` itself.
- `replicate_snapshot` is explicitly out of scope for UI wiring in this plan (see spec's Non-goals) — do not add a keybinding or modal for it.
- Guided vdev-spec construction (a disk-picker wizard) is out of scope — Create Pool takes a free-text vdev spec, split on whitespace.
- Confirmation friction matches risk exactly as specified: `ConfirmTyped` (type the exact name) for destroy-pool, destroy-dataset, destroy-snapshot, rollback-snapshot; `Confirm` (y/n) for scrub-pool, offline-disk, online-disk; no confirmation for any create form (submitting the form itself is the action).
- Every mutation success re-lists (refreshes) the section whose displayed data changed; the three vdev disk operations (offline/online/replace) re-list **both** Vdevs and Pools (disk state changes affect the parent pool's `health` column too) — this is the one deliberate exception to "one mutation, one refreshed section," and it must not be generalized further.
- No ratatui rendering/terminal tests — manual verification only (per Plan 1's Task 11 precedent). All new logic must be unit-testable against `App` driven via `handle_key`/`handle_modal_key` with `FakeRunner`.
- Deliberate deviation from the spec's literal "one success-path `MutationDone` test per mutation": `dispatch_mutation`/`drain_worker_messages` never branch on which `MutationCall` variant is inside a `PendingAction` — only on its `section`. Task 5 proves that routing generically once (`Section::Pools`) and Task 8 proves the `Section::Vdevs` dual-refresh special case once; each mutation's own `handle_key` test already asserts it builds the correct `section`, and each `MutationCall::run` is unit-tested individually in `src/ui/modal.rs`. Together these give equivalent coverage to 11 near-duplicate integration tests without the duplication — do not add the other 9 per spec's literal wording.

---

## File Structure

```
src/
  zfs/
    mod.rs        # Modify: add run_checked()
    pool.rs        # Modify: use run_checked(); add is_leaf_disk()
    dataset.rs      # Modify: use run_checked()
    snapshot.rs      # Modify: use run_checked() (including replicate_snapshot's existing check)
  ui/
    mod.rs         # Modify: event loop branches on app.modal, dispatches post-mutation refreshes, drives vdev drill-down refresh
    state.rs        # Modify: Section gains Vdevs; App gains modal/table-state/vdev fields; row selection, drill-down navigation, action keys, handle_modal_key, dispatch_mutation
    layout.rs        # Modify: stateful table rendering (selection highlight), vdev view, modal overlay
    modal.rs        # Create: Modal, FormField, FormKind, PendingAction, MutationCall
README.md          # Modify: keybindings section
```

---

### Task 1: `run_checked()` helper and refactor

**Files:**
- Modify: `src/zfs/mod.rs`
- Modify: `src/zfs/pool.rs`
- Modify: `src/zfs/dataset.rs`
- Modify: `src/zfs/snapshot.rs`

**Interfaces:**
- Consumes: `AppError`, `Result` from `src/error.rs`; `CommandRunner`, `CommandOutput` from `src/exec/mod.rs`.
- Produces: `pub(crate) fn run_checked(runner: &dyn CommandRunner, cmd: &str, args: &[&str], display: &str) -> Result<CommandOutput>` in `src/zfs/mod.rs`. Behavior-preserving refactor — no other function's signature changes.

This is a mechanical, behavior-preserving refactor: every hand-inlined `let output = runner.run(...); if !output.success() { return Err(AppError::CommandFailed {...}) }` block becomes one `run_checked(...)?` call. The existing test suite is the regression check — every test in `pool.rs`/`dataset.rs`/`snapshot.rs` must still pass unchanged after this task, since none of them assert on the internal call shape, only on `Result` and `runner.calls`.

- [ ] **Step 1: Add `run_checked` to `src/zfs/mod.rs`**

Replace the full contents of `src/zfs/mod.rs` with:

```rust
pub mod dataset;
pub mod pool;
pub mod snapshot;

use crate::error::{AppError, Result};
use crate::exec::{CommandOutput, CommandRunner};

/// Parses a `-H -p` numeric column that may show `-` when the value is
/// unavailable (e.g. `zpool list` prints `-` for SIZE/ALLOC/FREE on a
/// FAULTED/UNAVAIL pool). Treats `-` as `0` rather than a parse error,
/// so unhealthy pools/datasets/snapshots still show up in listings.
pub(crate) fn parse_numeric_or_dash(s: &str) -> Option<u64> {
    if s == "-" {
        Some(0)
    } else {
        s.parse().ok()
    }
}

/// Runs `cmd args` via `runner` and maps a nonzero exit to
/// `AppError::CommandFailed`, using `display` as the human-readable
/// command description in the error (since `args` may include `--`
/// guards or a shell pipeline that isn't worth showing verbatim).
/// Collapses the success/failure-check block that was previously
/// hand-duplicated at every call site in `pool.rs`/`dataset.rs`/
/// `snapshot.rs`.
pub(crate) fn run_checked(
    runner: &dyn CommandRunner,
    cmd: &str,
    args: &[&str],
    display: &str,
) -> Result<CommandOutput> {
    let output = runner.run(cmd, args)?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: display.to_string(),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    #[test]
    fn run_checked_returns_output_on_success() {
        let runner = FakeRunner::new("hello\n");
        let output = run_checked(&runner, "echo", &["hello"], "echo hello").unwrap();
        assert_eq!(output.stdout, "hello\n");
    }

    #[test]
    fn run_checked_maps_failure_to_command_failed_with_display_string() {
        let runner = FakeRunner::failing("boom", 3);
        let result = run_checked(&runner, "zpool", &["destroy", "--", "tank"], "zpool destroy tank");
        match result {
            Err(AppError::CommandFailed { cmd, status, stderr }) => {
                assert_eq!(cmd, "zpool destroy tank");
                assert_eq!(status, 3);
                assert_eq!(stderr, "boom");
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run the new tests**

Run: `cargo test zfs::tests`
Expected: PASS (2 tests)

- [ ] **Step 3: Refactor `src/zfs/pool.rs` to use `run_checked`**

Replace `list_pools` through `replace_disk` (i.e. every function from `list_pools` down to but not including the `#[cfg(test)]` module) with:

```rust
pub fn list_pools(runner: &dyn CommandRunner) -> Result<Vec<Pool>> {
    let output = super::run_checked(runner, "zpool", POOL_LIST_ARGS, "zpool list")?;
    parse_pool_list(&output.stdout)
}

/// Runs `zpool status <pool>` and parses its vdev tree. Separate from
/// `list_pools` because `zpool status` has no tabular `-H -p -o` form.
pub fn get_pool_vdevs(runner: &dyn CommandRunner, pool: &str) -> Result<Vec<VdevLine>> {
    let output = super::run_checked(
        runner,
        "zpool",
        &["status", "--", pool],
        &format!("zpool status {pool}"),
    )?;
    Ok(parse_pool_status_vdevs(&output.stdout))
}

/// Creates a pool from a raw vdev spec, e.g. `&["mirror", "da0", "da1"]`
/// or `&["raidz1", "da0", "da1", "da2"]`. Guided vdev-spec construction
/// (picking disks/redundancy from a UI wizard) depends on a disk
/// inventory feature not built yet, so it is out of scope here — this
/// function takes an already-built spec.
pub fn create_pool(runner: &dyn CommandRunner, name: &str, vdev_spec: &[&str]) -> Result<()> {
    let mut args = vec!["create", "--", name];
    args.extend_from_slice(vdev_spec);
    super::run_checked(
        runner,
        "zpool",
        &args,
        &format!("zpool create {name} {}", vdev_spec.join(" ")),
    )?;
    Ok(())
}

pub fn scrub_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()> {
    super::run_checked(runner, "zpool", &["scrub", "--", pool], &format!("zpool scrub {pool}"))?;
    Ok(())
}

pub fn destroy_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()> {
    super::run_checked(runner, "zpool", &["destroy", "--", pool], &format!("zpool destroy {pool}"))?;
    Ok(())
}

pub fn offline_disk(runner: &dyn CommandRunner, pool: &str, disk: &str) -> Result<()> {
    super::run_checked(
        runner,
        "zpool",
        &["offline", "--", pool, disk],
        &format!("zpool offline {pool} {disk}"),
    )?;
    Ok(())
}

pub fn online_disk(runner: &dyn CommandRunner, pool: &str, disk: &str) -> Result<()> {
    super::run_checked(
        runner,
        "zpool",
        &["online", "--", pool, disk],
        &format!("zpool online {pool} {disk}"),
    )?;
    Ok(())
}

pub fn replace_disk(runner: &dyn CommandRunner, pool: &str, old_disk: &str, new_disk: &str) -> Result<()> {
    super::run_checked(
        runner,
        "zpool",
        &["replace", "--", pool, old_disk, new_disk],
        &format!("zpool replace {pool} {old_disk} {new_disk}"),
    )?;
    Ok(())
}
```

Leave `PoolHealth`, `Pool`, `parse_pool_list`, `VdevLine`, `parse_pool_status_vdevs`, and the entire `#[cfg(test)]` module untouched — only the functions shown above change.

- [ ] **Step 4: Refactor `src/zfs/dataset.rs` to use `run_checked`**

Replace `list_datasets`, `create_dataset`, and `destroy_dataset` with:

```rust
pub fn list_datasets(runner: &dyn CommandRunner) -> Result<Vec<Dataset>> {
    let output = super::run_checked(runner, "zfs", DATASET_LIST_ARGS, "zfs list")?;
    parse_dataset_list(&output.stdout)
}

/// The `--` before `name` stops `zfs` from treating a dataset name that
/// starts with `-` as an option flag (argument injection).
pub fn create_dataset(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    super::run_checked(runner, "zfs", &["create", "--", name], &format!("zfs create {name}"))?;
    Ok(())
}

pub fn destroy_dataset(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    super::run_checked(runner, "zfs", &["destroy", "--", name], &format!("zfs destroy {name}"))?;
    Ok(())
}
```

Leave `DatasetKind`, `Dataset`, `parse_dataset_list`, and the `#[cfg(test)]` module untouched.

- [ ] **Step 5: Refactor `src/zfs/snapshot.rs` to use `run_checked`**

Replace `list_snapshots`, `create_snapshot`, `destroy_snapshot`, `rollback_snapshot`, and `replicate_snapshot` with:

```rust
pub fn list_snapshots(runner: &dyn CommandRunner) -> Result<Vec<Snapshot>> {
    let output = super::run_checked(runner, "zfs", SNAPSHOT_LIST_ARGS, "zfs list -t snapshot")?;
    parse_snapshot_list(&output.stdout)
}

/// The `--` before `target` stops `zfs` from treating a dataset name
/// that starts with `-` as an option flag (argument injection).
pub fn create_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    super::run_checked(runner, "zfs", &["snapshot", "--", &target], &format!("zfs snapshot {target}"))?;
    Ok(())
}

pub fn destroy_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    super::run_checked(runner, "zfs", &["destroy", "--", &target], &format!("zfs destroy {target}"))?;
    Ok(())
}

pub fn rollback_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    super::run_checked(runner, "zfs", &["rollback", "--", &target], &format!("zfs rollback {target}"))?;
    Ok(())
}

/// Replicates `source` (a `dataset@snapshot` name) to `dest_dataset`.
/// `dest_host` is the SSH destination for the receiving side, or `None`
/// to receive locally on the same host that `runner` targets.
pub fn replicate_snapshot(
    runner: &dyn CommandRunner,
    source: &str,
    dest_host: Option<&str>,
    dest_dataset: &str,
) -> Result<()> {
    if !is_safe_zfs_target(source) {
        return Err(AppError::Parse(format!("invalid snapshot name for replication: {source:?}")));
    }
    if !is_safe_zfs_target(dest_dataset) {
        return Err(AppError::Parse(format!("invalid destination dataset name for replication: {dest_dataset:?}")));
    }
    if let Some(host) = dest_host {
        if !is_safe_host(host) {
            return Err(AppError::Parse(format!("invalid destination host for replication: {host:?}")));
        }
    }

    // `--` before each caller-supplied value stops the receiving command
    // from treating a value that starts with `-` as an option flag
    // (argument injection) — applies to `zfs send`/`zfs receive` and, for
    // the remote leg, to `ssh` itself.
    let pipeline = match dest_host {
        None => format!("zfs send -- {source} | zfs receive -- {dest_dataset}"),
        Some(host) => format!("zfs send -- {source} | ssh -- {host} zfs receive -- {dest_dataset}"),
    };
    super::run_checked(runner, "sh", &["-c", &pipeline], &pipeline)?;
    Ok(())
}
```

Leave `Snapshot`, `parse_snapshot_list`, `is_safe_zfs_target`, `is_safe_host`, and the `#[cfg(test)]` module untouched. Note `AppError` must remain imported at the top of the file (`use crate::error::{AppError, Result};`) since `replicate_snapshot`'s validation still constructs `AppError::Parse` directly.

- [ ] **Step 6: Run the full test suite and confirm no behavior change**

Run: `cargo test`
Expected: PASS, 74 tests (72 existing + 2 new `run_checked` tests from Step 1 — the refactored `pool.rs`/`dataset.rs`/`snapshot.rs` functions themselves have no test count change, since this is a behavior-preserving refactor of their internals only)

- [ ] **Step 7: Commit**

```bash
git add src/zfs/mod.rs src/zfs/pool.rs src/zfs/dataset.rs src/zfs/snapshot.rs
git commit -m "refactor: extract run_checked() helper, dedup success/failure checks"
```

---

### Task 2: Per-(section, host) in-flight refresh tracking

**Files:**
- Modify: `src/ui/state.rs`

**Interfaces:**
- Consumes: `Section` (existing enum, gains `Hash` derive), `WorkerMsg` (existing).
- Produces: `Section` now derives `Hash`; `App::is_loading(&self) -> bool` (unchanged signature, new implementation); private `App::is_refreshing(&self, section: Section, host: usize) -> bool`, `App::begin_refresh(&mut self, section: Section, host: usize)`, `App::end_refresh(&mut self, section: Section, host: usize)`. `pools_loading`/`datasets_loading`/`snapshots_loading` fields are removed.

This replaces the per-section-only `bool` flags with a `HashSet<(Section, usize)>` keyed by section *and* host, fixing the race Plan 1's final review found: refreshing host B's Pools while host A's Pools refresh is still hung (e.g. on the 10s SSH connect timeout) must not be silently dropped.

- [ ] **Step 1: Write failing tests for per-host independence**

Add to the `#[cfg(test)] mod tests` block in `src/ui/state.rs` (after the existing `refresh_pools_does_not_spawn_second_thread_while_in_flight` test):

```rust
#[test]
fn refresh_pools_for_one_host_does_not_block_refresh_for_another_host() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    let fake_a = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
    let runner_a: Arc<dyn CommandRunner> = fake_a.clone();

    // Host 0's refresh is "in flight" (never drained in this test, so it
    // stays in flight for the duration).
    app.refresh_pools(runner_a.clone());
    assert!(app.is_refreshing(Section::Pools, 0));

    // Switching to host 1 and refreshing must not be blocked by host 0's
    // still-in-flight refresh.
    app.selected_host = 1;
    let fake_b = Arc::new(FakeRunner::new("scratch\t50\t5\t45\tONLINE\n"));
    let runner_b: Arc<dyn CommandRunner> = fake_b.clone();
    app.refresh_pools(runner_b.clone());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.pools.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.pools.len(), 1);
    assert_eq!(app.pools[0].name, "scratch", "host 1's refresh must have run, not been blocked by host 0's in-flight one");
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test refresh_pools_for_one_host_does_not_block_refresh_for_another_host`
Expected: FAIL with "no method named `is_refreshing` found" (doesn't exist yet)

- [ ] **Step 3: Replace the `pools_loading`/`datasets_loading`/`snapshots_loading` fields and `is_loading`**

In the `App` struct definition, replace:

```rust
    pub pools_loading: bool,
    pub datasets_loading: bool,
    pub snapshots_loading: bool,
```

with:

```rust
    in_flight: HashSet<(Section, usize)>,
```

Add `use std::collections::HashSet;` to the imports at the top of the file.

Change the `Section` enum's derive line from:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
```

to:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
```

In `App::new`, replace:

```rust
            pools_loading: false,
            datasets_loading: false,
            snapshots_loading: false,
```

with:

```rust
            in_flight: HashSet::new(),
```

Replace the `is_loading` method and add the three new private helpers, right after `current_section`:

```rust
    /// True while any (section, host) refresh is in flight. Used only
    /// for the coarse "refreshing..." indicator in the sidebar title.
    pub fn is_loading(&self) -> bool {
        !self.in_flight.is_empty()
    }

    fn is_refreshing(&self, section: Section, host: usize) -> bool {
        self.in_flight.contains(&(section, host))
    }

    fn begin_refresh(&mut self, section: Section, host: usize) {
        self.in_flight.insert((section, host));
    }

    fn end_refresh(&mut self, section: Section, host: usize) {
        self.in_flight.remove(&(section, host));
    }
```

- [ ] **Step 4: Update `refresh_pools`/`refresh_datasets`/`refresh_snapshots` to use the new tracking**

Replace all three functions:

```rust
    /// No-ops if a Pools refresh for the current host is already in
    /// flight — otherwise holding 'r' against an unreachable host would
    /// spawn an unbounded number of blocking threads/subprocesses, one
    /// per key-repeat tick. Keyed by host too, so a hung refresh for one
    /// host never blocks refreshing a different host.
    pub fn refresh_pools(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Pools, host) {
            return;
        }
        self.begin_refresh(Section::Pools, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::pool::list_pools(runner.as_ref());
            let _ = tx.send(WorkerMsg::Pools(host, result));
        });
    }

    /// See `refresh_pools` for why this no-ops while already in flight.
    pub fn refresh_datasets(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Datasets, host) {
            return;
        }
        self.begin_refresh(Section::Datasets, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::dataset::list_datasets(runner.as_ref());
            let _ = tx.send(WorkerMsg::Datasets(host, result));
        });
    }

    /// See `refresh_pools` for why this no-ops while already in flight.
    pub fn refresh_snapshots(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Snapshots, host) {
            return;
        }
        self.begin_refresh(Section::Snapshots, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::snapshot::list_snapshots(runner.as_ref());
            let _ = tx.send(WorkerMsg::Snapshots(host, result));
        });
    }
```

- [ ] **Step 5: Update `drain_worker_messages` to clear the new tracking**

Replace the body of `drain_worker_messages`:

```rust
    pub fn drain_worker_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Pools(host, result) => {
                    self.end_refresh(Section::Pools, host);
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(pools) => {
                            self.pools = pools;
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::Datasets(host, result) => {
                    self.end_refresh(Section::Datasets, host);
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(datasets) => {
                            self.datasets = datasets;
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::Snapshots(host, result) => {
                    self.end_refresh(Section::Snapshots, host);
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(snapshots) => {
                            self.snapshots = snapshots;
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
            }
        }
    }
```

- [ ] **Step 6: Update existing tests that referenced the removed bool fields**

Three existing tests reference `app.pools_loading` directly — update each to use `app.is_refreshing(Section::Pools, 0)`:

In `drain_worker_messages_updates_pools_on_success`, replace `assert!(!app.pools_loading);` with `assert!(!app.is_refreshing(Section::Pools, 0));`.

In `drain_worker_messages_discards_result_for_stale_host`, replace `app.pools_loading = true;` with `app.begin_refresh(Section::Pools, 0);`, and replace `assert!(!app.pools_loading, "...");` with `assert!(!app.is_refreshing(Section::Pools, 0), "the in-flight flag must still clear even though the result was discarded");`.

In `refresh_pools_does_not_spawn_second_thread_while_in_flight`, replace `assert!(app.pools_loading);` with `assert!(app.is_refreshing(Section::Pools, 0));`.

- [ ] **Step 7: Run the full test suite**

Run: `cargo test`
Expected: PASS, 75 tests (74 from Task 1 + 1 new from this task)

- [ ] **Step 8: Commit**

```bash
git add src/ui/state.rs
git commit -m "fix: track in-flight refreshes per (section, host), not per-section only"
```

---

### Task 3: Row selection and scrolling (Pools, Datasets, Snapshots)

**Files:**
- Modify: `src/ui/state.rs`
- Modify: `src/ui/layout.rs`

**Interfaces:**
- Consumes: `Section`, `App` (Task 2's shape).
- Produces: `App` gains `pub pools_table: TableState`, `pub datasets_table: TableState`, `pub snapshots_table: TableState`. Free function `fn move_table_selection(state: &mut TableState, len: usize, forward: bool)` and `fn clamp_table_selection(state: &mut TableState, len: usize)` in `src/ui/state.rs` (private to the module). `layout::draw_pools`/`draw_datasets`/`draw_snapshots` now take `app: &mut App` and render via `f.render_stateful_widget`.

Row selection uses `ratatui::widgets::TableState`, which tracks both the selected row and the scroll offset natively — no hand-rolled scrolling math needed. Selection wraps at the ends (consistent with the existing Hosts/section navigation style), clamps down when the underlying list shrinks, and resets to `None` when host data is cleared.

- [ ] **Step 1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/ui/state.rs`:

```rust
#[test]
fn handle_key_navigates_pool_rows_and_wraps() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![
        Pool { name: "a".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
        Pool { name: "b".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
    ];
    app.handle_key(KeyCode::Right); // Hosts -> Pools
    assert_eq!(app.current_section(), Section::Pools);
    assert_eq!(app.pools_table.selected(), None);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.pools_table.selected(), Some(0));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.pools_table.selected(), Some(1));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.pools_table.selected(), Some(0), "must wrap back to the first row");
    app.handle_key(KeyCode::Up);
    assert_eq!(app.pools_table.selected(), Some(1), "must wrap back to the last row");
}

#[test]
fn handle_key_pool_row_navigation_is_noop_on_empty_list() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.pools_table.selected(), None);
}

#[test]
fn drain_worker_messages_clamps_selected_pool_row_when_list_shrinks() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools_table.select(Some(2));
    app.begin_refresh(Section::Pools, 0);
    app.tx
        .send(WorkerMsg::Pools(0, Ok(vec![Pool {
            name: "only".to_string(),
            size: 0,
            allocated: 0,
            free: 0,
            health: crate::zfs::pool::PoolHealth::Online,
        }])))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.pools_table.selected(), Some(0));
}

#[test]
fn handle_key_navigates_dataset_rows_and_wraps() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![
        Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
        Dataset { name: "b".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
    ];
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Right); // Hosts -> Pools -> Datasets
    assert_eq!(app.current_section(), Section::Datasets);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.datasets_table.selected(), Some(0));
    app.handle_key(KeyCode::Up);
    assert_eq!(app.datasets_table.selected(), Some(1), "must wrap back to the last row");
}

#[test]
fn drain_worker_messages_clamps_selected_dataset_row_when_list_shrinks() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets_table.select(Some(2));
    app.begin_refresh(Section::Datasets, 0);
    app.tx
        .send(WorkerMsg::Datasets(0, Ok(vec![Dataset {
            name: "only".to_string(),
            kind: crate::zfs::dataset::DatasetKind::Filesystem,
            used: 0,
            available: 0,
            referenced: 0,
            mountpoint: None,
        }])))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.datasets_table.selected(), Some(0));
}

#[test]
fn handle_key_navigates_snapshot_rows_and_wraps() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots = vec![
        Snapshot { dataset: "tank/data".to_string(), name: "a".to_string(), used: 0 },
        Snapshot { dataset: "tank/data".to_string(), name: "b".to_string(), used: 0 },
    ];
    app.handle_key(KeyCode::Left); // Hosts -> Snapshots (wraps backward)
    assert_eq!(app.current_section(), Section::Snapshots);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.snapshots_table.selected(), Some(0));
    app.handle_key(KeyCode::Up);
    assert_eq!(app.snapshots_table.selected(), Some(1), "must wrap back to the last row");
}

#[test]
fn drain_worker_messages_clamps_selected_snapshot_row_when_list_shrinks() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots_table.select(Some(2));
    app.begin_refresh(Section::Snapshots, 0);
    app.tx
        .send(WorkerMsg::Snapshots(0, Ok(vec![Snapshot {
            dataset: "tank/data".to_string(),
            name: "only".to_string(),
            used: 0,
        }])))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.snapshots_table.selected(), Some(0));
}
```

Also update the existing `switching_selected_host_clears_pane_data` test to assert table selection resets too — replace it with:

```rust
#[test]
fn switching_selected_host_clears_pane_data() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.pools = vec![Pool {
        name: "tank".to_string(),
        size: 100,
        allocated: 10,
        free: 90,
        health: crate::zfs::pool::PoolHealth::Online,
    }];
    app.datasets = vec![crate::zfs::dataset::Dataset {
        name: "tank/data".to_string(),
        kind: crate::zfs::dataset::DatasetKind::Filesystem,
        used: 1,
        available: 2,
        referenced: 1,
        mountpoint: None,
    }];
    app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "snap1".to_string(), used: 1 }];
    app.pools_table.select(Some(0));
    app.datasets_table.select(Some(0));
    app.snapshots_table.select(Some(0));

    app.handle_key(KeyCode::Down); // still on Hosts section by default; switches host

    assert!(app.pools.is_empty());
    assert!(app.datasets.is_empty());
    assert!(app.snapshots.is_empty());
    assert_eq!(app.pools_table.selected(), None);
    assert_eq!(app.datasets_table.selected(), None);
    assert_eq!(app.snapshots_table.selected(), None);
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test handle_key_navigates_pool_rows_and_wraps`
Expected: FAIL (no `pools_table` field exists yet)

- [ ] **Step 3: Add the `TableState` fields and helper functions**

Add `use ratatui::widgets::TableState;` to the imports at the top of `src/ui/state.rs`.

Add three fields to the `App` struct, after `snapshots`:

```rust
    pub pools_table: TableState,
    pub datasets_table: TableState,
    pub snapshots_table: TableState,
```

Initialize them in `App::new`, after `snapshots: Vec::new(),`:

```rust
            pools_table: TableState::default(),
            datasets_table: TableState::default(),
            snapshots_table: TableState::default(),
```

Add these two free functions after the `App` impl block's closing brace (or anywhere at module scope — they don't need `self`):

```rust
/// Moves a `TableState`'s selection by one row within `[0, len)`,
/// wrapping at either end (consistent with the Hosts/section navigation
/// style elsewhere in `App`). No-ops (selection stays `None`) when
/// `len == 0` — there's nothing to select in an empty list.
fn move_table_selection(state: &mut TableState, len: usize, forward: bool) {
    if len == 0 {
        state.select(None);
        return;
    }
    let next = match (state.selected(), forward) {
        (None, true) => 0,
        (None, false) => len - 1,
        (Some(i), true) => (i + 1) % len,
        (Some(i), false) => {
            if i == 0 {
                len - 1
            } else {
                i - 1
            }
        }
    };
    state.select(Some(next));
}

/// Pulls a `TableState`'s selection back into range after the
/// underlying list shrinks (e.g. a refresh returns fewer rows than
/// before). Selects `None` if the list is now empty.
fn clamp_table_selection(state: &mut TableState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    if let Some(i) = state.selected() {
        if i >= len {
            state.select(Some(len - 1));
        }
    }
}
```

- [ ] **Step 4: Wire row navigation into `handle_key`**

Add four new arms to the `match key` in `handle_key`, right before the final `_ => {}` catch-all:

```rust
            KeyCode::Down if self.current_section() == Section::Pools => {
                move_table_selection(&mut self.pools_table, self.pools.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Pools => {
                move_table_selection(&mut self.pools_table, self.pools.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::Snapshots => {
                move_table_selection(&mut self.snapshots_table, self.snapshots.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Snapshots => {
                move_table_selection(&mut self.snapshots_table, self.snapshots.len(), false);
            }
```

(These don't conflict with the existing `Section::Hosts` Up/Down arms above them — a match's guards make them mutually exclusive since `current_section()` returns exactly one value.)

- [ ] **Step 5: Clamp selection when a refresh returns fewer rows, and reset it in `clear_host_data`**

In `drain_worker_messages`, add a `clamp_table_selection` call right after each successful list is assigned:

```rust
                    match result {
                        Ok(pools) => {
                            self.pools = pools;
                            clamp_table_selection(&mut self.pools_table, self.pools.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
```

(and the equivalent for `datasets`/`self.datasets_table` and `snapshots`/`self.snapshots_table` in their respective arms).

In `clear_host_data`, add the three `.select(None)` calls:

```rust
    fn clear_host_data(&mut self) {
        self.pools.clear();
        self.datasets.clear();
        self.snapshots.clear();
        self.pools_table.select(None);
        self.datasets_table.select(None);
        self.snapshots_table.select(None);
    }
```

- [ ] **Step 6: Run the state tests**

Run: `cargo test ui::state`
Expected: PASS, all new tests plus existing ones green

- [ ] **Step 7: Render the selection highlight in `src/ui/layout.rs`**

Add `TableState` and `Clear` are not needed yet (Clear comes in Task 5) — just add `TableState` isn't actually referenced in `layout.rs` directly (it's passed by mutable reference from `app`), so no new import is needed there beyond what follows.

Change `pub fn draw(f: &mut Frame, app: &App)` to `pub fn draw(f: &mut Frame, app: &mut App)`, and `fn draw_main(f: &mut Frame, app: &App, area: Rect)` to `fn draw_main(f: &mut Frame, app: &mut App, area: Rect)`.

Replace `draw_pools`, `draw_datasets`, and `draw_snapshots`:

```rust
fn draw_pools(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.pools.iter().map(|p| {
        Row::new(vec![
            p.name.clone(),
            p.size.to_string(),
            p.allocated.to_string(),
            p.free.to_string(),
            format!("{:?}", p.health),
        ])
    });
    let widths = [
        Constraint::Length(20),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(10),
    ];
    let title = format!("Pools — {} ('r' to refresh)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Name", "Size", "Alloc", "Free", "Health"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.pools_table);
}

fn draw_datasets(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.datasets.iter().map(|d| {
        Row::new(vec![
            d.name.clone(),
            d.used.to_string(),
            d.available.to_string(),
            format!("{:?}", d.kind),
        ])
    });
    let widths = [
        Constraint::Length(24),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(12),
    ];
    let title = format!("Datasets — {} ('r' to refresh)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Name", "Used", "Avail", "Kind"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.datasets_table);
}

fn draw_snapshots(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app
        .snapshots
        .iter()
        .map(|s| Row::new(vec![format!("{}@{}", s.dataset, s.name), s.used.to_string()]));
    let widths = [Constraint::Length(34), Constraint::Length(14)];
    let title = format!("Snapshots — {} ('r' to refresh)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Snapshot", "Used"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.snapshots_table);
}
```

- [ ] **Step 8: Update the call site in `src/ui/mod.rs`**

The closure `terminal.draw(|f| layout::draw(f, app))` in `event_loop` already captures `app` from a `&mut App` parameter — no change needed there, `layout::draw`'s new `&mut App` signature is satisfied by the same reference (Rust reborrows it automatically).

- [ ] **Step 9: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, 82 tests (75 from Task 2 + 7 new from this task); release build clean aside from the existing expected dead_code warnings

- [ ] **Step 10: Commit**

```bash
git add src/ui/state.rs src/ui/layout.rs
git commit -m "feat: row selection and scrolling for Pools/Datasets/Snapshots tables"
```

---

### Task 4: Vdev drill-down view

**Files:**
- Modify: `src/ui/state.rs`
- Modify: `src/ui/layout.rs`
- Modify: `src/ui/mod.rs`

**Interfaces:**
- Consumes: `VdevLine`, `get_pool_vdevs` from `src/zfs/pool.rs`; `TableState` (Task 3).
- Produces: `Section::Vdevs` variant (excluded from `App::sections`, the `Tab`-cycled list). `App` gains `pub vdevs: Vec<VdevLine>`, `pub vdevs_table: TableState`, `pub drilled_pool: Option<String>`. `App::refresh_vdevs(&mut self, runner: Arc<dyn CommandRunner>, pool: String)`. `WorkerMsg` gains `Vdevs(usize, Result<Vec<VdevLine>>)`. `layout::draw_vdevs`.

Reached via `Enter` on a selected Pools row; left via `Esc`/`Backspace`. Not part of `Tab`/`Left`/`Right` section cycling — `drilled_pool: Some(_)` is what actually puts the UI "in" this view, layered on top of the still-selected `Pools` section.

- [ ] **Step 1: Write failing tests**

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn enter_on_selected_pool_drills_into_vdevs() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Right); // Hosts -> Pools
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.drilled_pool, Some("tank".to_string()));
}

#[test]
fn enter_with_no_row_selected_does_not_drill_in() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.drilled_pool, None);
}

#[test]
fn esc_backs_out_of_vdev_drill_down() {
    let mut app = App::new(vec!["local".to_string()]);
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 0, name: "tank".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Esc);
    assert_eq!(app.drilled_pool, None);
    assert!(app.vdevs.is_empty());
    assert_eq!(app.vdevs_table.selected(), None);
}

#[test]
fn esc_with_no_drill_down_active_clears_status_message_as_before() {
    let mut app = App::new(vec!["local".to_string()]);
    app.status_message = Some("stale".to_string());
    app.handle_key(KeyCode::Esc);
    assert_eq!(app.status_message, None);
}

#[test]
fn up_down_while_drilled_in_navigate_vdev_rows_not_pool_rows() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Right);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![
        VdevLine { depth: 0, name: "tank".to_string(), health: crate::zfs::pool::PoolHealth::Online },
        VdevLine { depth: 1, name: "da0".to_string(), health: crate::zfs::pool::PoolHealth::Online },
    ];
    app.handle_key(KeyCode::Down);
    assert_eq!(app.vdevs_table.selected(), Some(0));
    assert_eq!(app.pools_table.selected(), Some(0), "pool row selection must be untouched while drilled in");
}

#[test]
fn refresh_vdevs_populates_vdevs_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(
        "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME  STATE     READ WRITE CKSUM\n\ttank  ONLINE       0     0     0\n\nerrors: No known data errors\n",
    ));
    app.refresh_vdevs(runner, "tank".to_string());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.vdevs.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.vdevs.len(), 1);
    assert_eq!(app.vdevs[0].name, "tank");
}
```

Add `crate::zfs::pool::VdevLine` to the imports used by tests (it will already be imported at module level after Step 3 below, since `super::*` brings it in).

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test drills_into_vdevs`
Expected: FAIL (`drilled_pool` field doesn't exist yet)

- [ ] **Step 3: Add the `Vdevs` section, new fields, and `WorkerMsg` variant**

Change the `Section` enum to add a fifth variant:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Hosts,
    Pools,
    Datasets,
    Snapshots,
    Vdevs,
}
```

`Vdevs` is deliberately never added to `App::sections` in `App::new` — it stays `vec![Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots]`, unchanged.

Add `VdevLine` to the `zfs::pool` import at the top of the file — change:

```rust
use crate::zfs::pool::Pool;
```

to:

```rust
use crate::zfs::pool::{Pool, VdevLine};
```

Add a variant to `WorkerMsg`:

```rust
pub enum WorkerMsg {
    Pools(usize, Result<Vec<Pool>>),
    Datasets(usize, Result<Vec<Dataset>>),
    Snapshots(usize, Result<Vec<Snapshot>>),
    Vdevs(usize, Result<Vec<VdevLine>>),
}
```

Add three fields to `App`, after `snapshots_table`:

```rust
    pub vdevs: Vec<VdevLine>,
    pub vdevs_table: TableState,
    pub drilled_pool: Option<String>,
```

Initialize them in `App::new`, after `snapshots_table: TableState::default(),`:

```rust
            vdevs: Vec::new(),
            vdevs_table: TableState::default(),
            drilled_pool: None,
```

- [ ] **Step 4: Add `refresh_vdevs` and its `drain_worker_messages` arm**

Add after `refresh_snapshots`:

```rust
    /// See `refresh_pools` for why this no-ops while already in flight.
    /// `pool` is the name of the currently drilled-into pool.
    pub fn refresh_vdevs(&mut self, runner: Arc<dyn CommandRunner>, pool: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Vdevs, host) {
            return;
        }
        self.begin_refresh(Section::Vdevs, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::pool::get_pool_vdevs(runner.as_ref(), &pool);
            let _ = tx.send(WorkerMsg::Vdevs(host, result));
        });
    }
```

Add a `WorkerMsg::Vdevs` arm to `drain_worker_messages`'s `match msg`, alongside the existing three:

```rust
                WorkerMsg::Vdevs(host, result) => {
                    self.end_refresh(Section::Vdevs, host);
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(vdevs) => {
                            self.vdevs = vdevs;
                            clamp_table_selection(&mut self.vdevs_table, self.vdevs.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
```

- [ ] **Step 5: Wire drill-in/out and vdev row navigation into `handle_key`**

Replace the full body of `handle_key` with:

```rust
    pub fn handle_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc | KeyCode::Backspace if self.drilled_pool.is_some() => {
                self.drilled_pool = None;
                self.vdevs.clear();
                self.vdevs_table.select(None);
            }
            KeyCode::Esc => self.status_message = None,
            KeyCode::Enter if self.current_section() == Section::Pools && self.drilled_pool.is_none() => {
                if let Some(i) = self.pools_table.selected() {
                    if let Some(pool) = self.pools.get(i) {
                        self.drilled_pool = Some(pool.name.clone());
                        self.vdevs.clear();
                        self.vdevs_table.select(None);
                    }
                }
            }
            KeyCode::Right | KeyCode::Tab if self.drilled_pool.is_none() => {
                self.selected_section = (self.selected_section + 1) % self.sections.len();
            }
            KeyCode::Left if self.drilled_pool.is_none() => {
                self.selected_section = if self.selected_section == 0 {
                    self.sections.len() - 1
                } else {
                    self.selected_section - 1
                };
            }
            KeyCode::Down if self.current_section() == Section::Hosts && self.drilled_pool.is_none() && !self.hosts.is_empty() => {
                self.selected_host = (self.selected_host + 1) % self.hosts.len();
                self.clear_host_data();
            }
            KeyCode::Up if self.current_section() == Section::Hosts && self.drilled_pool.is_none() && !self.hosts.is_empty() => {
                self.selected_host = if self.selected_host == 0 {
                    self.hosts.len() - 1
                } else {
                    self.selected_host - 1
                };
                self.clear_host_data();
            }
            KeyCode::Down if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                move_table_selection(&mut self.vdevs_table, self.vdevs.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                move_table_selection(&mut self.vdevs_table, self.vdevs.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::Pools => {
                move_table_selection(&mut self.pools_table, self.pools.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Pools => {
                move_table_selection(&mut self.pools_table, self.pools.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::Snapshots => {
                move_table_selection(&mut self.snapshots_table, self.snapshots.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Snapshots => {
                move_table_selection(&mut self.snapshots_table, self.snapshots.len(), false);
            }
            _ => {}
        }
    }
```

(The `Section::Pools` Down/Up arms are now split into two pairs — one guarded by `self.drilled_pool.is_some()` moving `vdevs_table`, one unguarded [reached only when the first pair's guard fails, i.e. `drilled_pool.is_none()`] moving `pools_table` — Rust match arms are tried top to bottom, so the more specific guard must come first.)

- [ ] **Step 6: Run the state tests**

Run: `cargo test ui::state`
Expected: PASS, all new tests plus existing ones green

- [ ] **Step 7: Render the vdev view in `src/ui/layout.rs`**

Add `use crate::zfs::pool::VdevLine;` — actually not needed since `layout.rs` only accesses `app.vdevs` (already typed), no direct `VdevLine` construction. Skip this import.

Change `draw_main` to check for drill-down first:

```rust
fn draw_main(f: &mut Frame, app: &mut App, area: Rect) {
    if app.drilled_pool.is_some() {
        draw_vdevs(f, app, area);
        return;
    }
    match app.sections[app.selected_section] {
        Section::Hosts => draw_hosts(f, app, area),
        Section::Pools => draw_pools(f, app, area),
        Section::Datasets => draw_datasets(f, app, area),
        Section::Snapshots => draw_snapshots(f, app, area),
        Section::Vdevs => unreachable!("Vdevs is never in App::sections"),
    }
}
```

Add `draw_vdevs` after `draw_pools`:

```rust
fn draw_vdevs(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.vdevs.iter().map(|v| {
        let indented = format!("{}{}", "  ".repeat(v.depth), v.name);
        Row::new(vec![indented, format!("{:?}", v.health)])
    });
    let widths = [Constraint::Length(30), Constraint::Length(12)];
    let pool = app.drilled_pool.as_deref().unwrap_or("?");
    let title = format!("Vdevs — {pool} ('r' refresh, Esc/Backspace back)");
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Device", "Health"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.vdevs_table);
}
```

- [ ] **Step 8: Wire the drill-down refresh trigger into `src/ui/mod.rs`**

Replace the `if key.code == KeyCode::Char('r')` block in `event_loop` with:

```rust
                if key.code == KeyCode::Char('r') {
                    if let Some(host) = inventory.hosts.get(app.selected_host) {
                        if let Some(pool) = app.drilled_pool.clone() {
                            app.refresh_vdevs(host.runner.clone(), pool);
                        } else {
                            match app.current_section() {
                                Section::Pools => app.refresh_pools(host.runner.clone()),
                                Section::Datasets => app.refresh_datasets(host.runner.clone()),
                                Section::Snapshots => app.refresh_snapshots(host.runner.clone()),
                                Section::Hosts | Section::Vdevs => {}
                            }
                        }
                    }
                }
                if key.code == KeyCode::Enter {
                    if let Some(pool) = app.drilled_pool.clone() {
                        if let Some(host) = inventory.hosts.get(app.selected_host) {
                            app.refresh_vdevs(host.runner.clone(), pool);
                        }
                    }
                }
```

(The `Enter` block runs after `app.handle_key(key.code)` already ran earlier in the loop body, so `app.drilled_pool` reflects the just-pressed `Enter`'s effect — either freshly `Some(pool)` from drilling in, or still `Some(pool)` from before if the user pressed `Enter` again while already drilled in, which just re-triggers a refresh. Both are fine; `refresh_vdevs`'s in-flight guard makes this idempotent.)

- [ ] **Step 9: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, 88 tests (82 from Task 3 + 6 new from this task); release build clean aside from expected dead_code warnings

- [ ] **Step 10: Commit**

```bash
git add src/ui/state.rs src/ui/layout.rs src/ui/mod.rs
git commit -m "feat: vdev drill-down view for pools"
```

---

### Task 5: Modal core, mutation dispatch, and Scrub Pool wiring

**Files:**
- Create: `src/ui/modal.rs`
- Modify: `src/ui/state.rs`
- Modify: `src/ui/layout.rs`
- Modify: `src/ui/mod.rs`

**Interfaces:**
- Consumes: `Section` from `src/ui/state.rs`; `CommandRunner` from `src/exec/mod.rs`; `scrub_pool` from `src/zfs/pool.rs`.
- Produces (`src/ui/modal.rs`): `MutationCall` enum (starts with just `ScrubPool { pool: String }`, more variants added by later tasks), `MutationCall::run(&self, runner: &dyn CommandRunner) -> Result<()>`, `PendingAction { pub host: usize, pub section: Section, pub call: MutationCall }`, `FormField { pub label: String, pub value: String }` (unused until Task 7, defined now since `Modal::Form` references it), `Modal` enum (`Confirm`, `ConfirmTyped`, `Form` — `Form`'s `kind: FormKind` and `FormKind` itself are added in Task 7, so `Modal::Form`'s definition is deferred to that task; this task defines only `Confirm` and `ConfirmTyped`, adding `Form` when the first form-based mutation needs it).
- Produces (`src/ui/state.rs`): `App` gains `pub modal: Option<Modal>`. `App::handle_modal_key(&mut self, key: KeyCode) -> Option<PendingAction>`. `App::dispatch_mutation(&mut self, action: PendingAction, runner: Arc<dyn CommandRunner>)`. `drain_worker_messages` changes return type to `Vec<Section>` (sections that need re-listing after a successful mutation). `WorkerMsg` gains `MutationDone(usize, Section, Result<()>)`.

This task intentionally defines `Modal` with only two of its three variants (`Confirm`, `ConfirmTyped`) — `Form` and its `FormKind` companion are added in Task 7 alongside the first form-based mutation (Create Pool), since a variant with no constructor and no test coverage would violate the "no placeholders" rule. Everything else (dispatch, `handle_modal_key`'s `Confirm`/`ConfirmTyped` arms, rendering, the `WorkerMsg::MutationDone` plumbing) is built now and is real, tested infrastructure — Task 7 only adds one more `match` arm to `handle_modal_key` and one more branch to the modal renderer.

- [ ] **Step 1: Create `src/ui/modal.rs`**

```rust
use crate::error::Result;
use crate::exec::CommandRunner;
use crate::ui::state::Section;

/// Names a concrete `zfs::*` mutation call, with exactly the arguments
/// that function needs. Built from a modal's collected input (or, for
/// `Confirm`/`ConfirmTyped`, from the row that was selected when the
/// modal opened) and run on a worker thread once the modal is
/// confirmed/submitted.
#[derive(Debug, Clone, PartialEq)]
pub enum MutationCall {
    ScrubPool { pool: String },
}

impl MutationCall {
    /// The single place that maps a `MutationCall` variant to its
    /// underlying `zfs::*` library call.
    pub fn run(&self, runner: &dyn CommandRunner) -> Result<()> {
        match self {
            MutationCall::ScrubPool { pool } => crate::zfs::pool::scrub_pool(runner, pool),
        }
    }
}

/// Everything needed to actually run a mutation once its modal is
/// confirmed: which host to run it against, which section to re-list
/// on success, and the call itself.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingAction {
    pub host: usize,
    pub section: Section,
    pub call: MutationCall,
}

/// A single labeled text input in a `Modal::Form`.
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    pub label: String,
    pub value: String,
}

/// A modal dialog awaiting user input before a mutation runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// Simple y/n confirmation for reversible/lower-risk operations.
    Confirm { prompt: String, action: PendingAction },
    /// Type-the-exact-name confirmation for irreversible operations.
    ConfirmTyped {
        prompt: String,
        expected: String,
        input: String,
        action: PendingAction,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    #[test]
    fn scrub_pool_call_runs_scrub_pool() {
        let runner = FakeRunner::new("");
        let call = MutationCall::ScrubPool { pool: "tank".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["scrub".to_string(), "--".to_string(), "tank".to_string()]));
    }

    #[test]
    fn scrub_pool_call_surfaces_failure() {
        let runner = FakeRunner::failing("pool is busy", 1);
        let call = MutationCall::ScrubPool { pool: "tank".to_string() };
        assert!(call.run(&runner).is_err());
    }
}
```

- [ ] **Step 2: Run the new module's tests**

Run: `cargo test ui::modal`
Expected: PASS (2 tests)

- [ ] **Step 3: Declare the module and import its types in `src/ui/mod.rs`**

Add `pub mod modal;` to the top of `src/ui/mod.rs`, after `pub mod layout;`:

```rust
pub mod layout;
pub mod modal;
pub mod state;
```

- [ ] **Step 4: Add failing tests for the modal/dispatch plumbing**

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn handle_key_s_on_pools_opens_scrub_confirm_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Right); // Hosts -> Pools
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Char('s'));
    match app.modal {
        Some(crate::ui::modal::Modal::Confirm { action, .. }) => {
            assert_eq!(action.host, 0);
            assert_eq!(action.section, Section::Pools);
            assert_eq!(action.call, crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() });
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn handle_key_s_with_no_row_selected_does_not_open_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('s'));
    assert!(app.modal.is_none());
}

#[test]
fn handle_modal_key_confirm_yes_returns_action_and_clears_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    app.modal = Some(crate::ui::modal::Modal::Confirm { prompt: "Scrub tank? (y/n)".to_string(), action: action.clone() });
    let result = app.handle_modal_key(KeyCode::Char('y'));
    assert_eq!(result, Some(action));
    assert!(app.modal.is_none());
}

#[test]
fn handle_modal_key_confirm_enter_also_submits() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    app.modal = Some(crate::ui::modal::Modal::Confirm { prompt: "Scrub tank? (y/n)".to_string(), action });
    let result = app.handle_modal_key(KeyCode::Enter);
    assert!(result.is_some());
}

#[test]
fn handle_modal_key_confirm_no_cancels_and_clears_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    app.modal = Some(crate::ui::modal::Modal::Confirm { prompt: "Scrub tank? (y/n)".to_string(), action });
    let result = app.handle_modal_key(KeyCode::Char('n'));
    assert_eq!(result, None);
    assert!(app.modal.is_none());
}

#[test]
fn handle_modal_key_confirm_unrecognized_key_keeps_modal_open() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    app.modal = Some(crate::ui::modal::Modal::Confirm { prompt: "Scrub tank? (y/n)".to_string(), action });
    let result = app.handle_modal_key(KeyCode::Char('x'));
    assert_eq!(result, None);
    assert!(app.modal.is_some(), "an unrecognized key must not silently dismiss the confirmation");
}

#[test]
fn handle_modal_key_confirm_typed_requires_exact_match() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    app.modal = Some(crate::ui::modal::Modal::ConfirmTyped {
        prompt: "Type the pool name:".to_string(),
        expected: "tank".to_string(),
        input: "tan".to_string(),
        action: action.clone(),
    });
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None, "submitting with a mismatched confirmation must not run the action");
    match &app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { input, .. }) => assert_eq!(input, "tan"),
        other => panic!("expected the modal to stay open, got {other:?}"),
    }

    // Type the missing character, then submit again.
    let result = app.handle_modal_key(KeyCode::Char('k'));
    assert_eq!(result, None);
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, Some(action));
    assert!(app.modal.is_none());
}

#[test]
fn handle_modal_key_confirm_typed_backspace_edits_input() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    app.modal = Some(crate::ui::modal::Modal::ConfirmTyped {
        prompt: "Type the pool name:".to_string(),
        expected: "tank".to_string(),
        input: "tanx".to_string(),
        action,
    });
    app.handle_modal_key(KeyCode::Backspace);
    match &app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { input, .. }) => assert_eq!(input, "tan"),
        other => panic!("expected the modal to stay open, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_confirm_typed_esc_cancels() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    app.modal = Some(crate::ui::modal::Modal::ConfirmTyped {
        prompt: "Type the pool name:".to_string(),
        expected: "tank".to_string(),
        input: "tank".to_string(),
        action,
    });
    let result = app.handle_modal_key(KeyCode::Esc);
    assert_eq!(result, None);
    assert!(app.modal.is_none());
}

#[test]
fn dispatch_mutation_success_queues_refresh_for_its_section() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(""));
    app.dispatch_mutation(action, runner);

    let deadline = Instant::now() + Duration::from_secs(2);
    let refreshes = loop {
        let r = app.drain_worker_messages();
        if !r.is_empty() || Instant::now() > deadline {
            break r;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(refreshes, vec![Section::Pools]);
    assert_eq!(app.status_message, None);
}

#[test]
fn dispatch_mutation_failure_sets_status_message_and_queues_no_refresh() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::failing("pool is busy", 1));
    app.dispatch_mutation(action, runner);

    let deadline = Instant::now() + Duration::from_secs(2);
    let refreshes = loop {
        let r = app.drain_worker_messages();
        if app.status_message.is_some() || Instant::now() > deadline {
            break r;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(refreshes.is_empty());
    assert!(app.status_message.is_some());
}

#[test]
fn dispatch_mutation_result_for_stale_host_is_discarded() {
    // Simulates: a mutation was dispatched against host 0, the user
    // switched to host 1, and only then did host 0's result arrive.
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.tx.send(WorkerMsg::MutationDone(0, Section::Pools, Ok(()))).unwrap();
    app.selected_host = 1; // navigated away before the result arrived
    let refreshes = app.drain_worker_messages();
    assert!(refreshes.is_empty(), "a mutation result for a host we've navigated away from must not queue a refresh");
    assert_eq!(app.status_message, None);
}
```

- [ ] **Step 5: Run the new tests to confirm they fail**

Run: `cargo test handle_key_s_on_pools_opens_scrub_confirm_modal`
Expected: FAIL (`app.modal` field doesn't exist yet)

- [ ] **Step 6: Add `modal: Option<Modal>` to `App`, the `MutationDone` variant, and the `use` import**

Add `use crate::ui::modal::{Modal, MutationCall, PendingAction};` to the imports at the top of `src/ui/state.rs`.

Add a field to `App`, after `drilled_pool`:

```rust
    pub modal: Option<Modal>,
```

Initialize it in `App::new`, after `drilled_pool: None,`:

```rust
            modal: None,
```

Add a variant to `WorkerMsg`:

```rust
pub enum WorkerMsg {
    Pools(usize, Result<Vec<Pool>>),
    Datasets(usize, Result<Vec<Dataset>>),
    Snapshots(usize, Result<Vec<Snapshot>>),
    Vdevs(usize, Result<Vec<VdevLine>>),
    MutationDone(usize, Section, Result<()>),
}
```

- [ ] **Step 7: Add the `'s'` action key to `handle_key`**

Add one arm, anywhere before the final `_ => {}` (placed here right after the `Section::Pools` drill-in-navigation arms added in Task 4, before the `Section::Datasets` arms):

```rust
            KeyCode::Char('s') if self.current_section() == Section::Pools && self.drilled_pool.is_none() => {
                if let Some(i) = self.pools_table.selected() {
                    if let Some(pool) = self.pools.get(i) {
                        self.modal = Some(Modal::Confirm {
                            prompt: format!("Scrub pool '{}'? (y/n)", pool.name),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Pools,
                                call: MutationCall::ScrubPool { pool: pool.name.clone() },
                            },
                        });
                    }
                }
            }
```

- [ ] **Step 8: Add `handle_modal_key`**

Add this method to the `impl App` block, after `handle_key`:

```rust
    /// Routes a key press to the currently open modal. Returns
    /// `Some(action)` when the modal was submitted/confirmed — the
    /// caller (event loop) is responsible for looking up the target
    /// host's runner and calling `dispatch_mutation`. Returns `None`
    /// for a cancel, an in-progress edit, or an unrecognized key (which
    /// leaves the modal open rather than silently dismissing it).
    /// No-ops (returns `None`, does nothing) if no modal is open.
    pub fn handle_modal_key(&mut self, key: KeyCode) -> Option<PendingAction> {
        let modal = self.modal.take()?;
        match modal {
            Modal::Confirm { prompt, action } => match key {
                KeyCode::Char('y') | KeyCode::Enter => Some(action),
                KeyCode::Char('n') | KeyCode::Esc => None,
                _ => {
                    self.modal = Some(Modal::Confirm { prompt, action });
                    None
                }
            },
            Modal::ConfirmTyped { prompt, expected, mut input, action } => match key {
                KeyCode::Esc => None,
                KeyCode::Enter => {
                    if input == expected {
                        Some(action)
                    } else {
                        self.modal = Some(Modal::ConfirmTyped { prompt, expected, input, action });
                        None
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.modal = Some(Modal::ConfirmTyped { prompt, expected, input, action });
                    None
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    self.modal = Some(Modal::ConfirmTyped { prompt, expected, input, action });
                    None
                }
                _ => {
                    self.modal = Some(Modal::ConfirmTyped { prompt, expected, input, action });
                    None
                }
            },
        }
    }
```

- [ ] **Step 9: Add `dispatch_mutation`**

Add this method after `handle_modal_key`:

```rust
    /// Runs `action.call` on a worker thread against `runner` and
    /// reports the outcome as `WorkerMsg::MutationDone`. The modal is
    /// already closed by the time this is called (`handle_modal_key`
    /// clears it on submit) — this only executes the mutation itself.
    pub fn dispatch_mutation(&mut self, action: PendingAction, runner: Arc<dyn CommandRunner>) {
        let PendingAction { host, section, call } = action;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = call.run(runner.as_ref());
            let _ = tx.send(WorkerMsg::MutationDone(host, section, result));
        });
    }
```

- [ ] **Step 10: Change `drain_worker_messages` to return sections needing a refresh**

Replace the whole function:

```rust
    /// Drains all pending worker messages, applying each to `self`.
    /// Returns the list of sections that need re-listing because a
    /// mutation targeting the currently-selected host just succeeded —
    /// the caller (event loop) looks up the right runner and triggers
    /// the actual refresh, since `App` has no way to resolve a host
    /// index to a runner on its own.
    pub fn drain_worker_messages(&mut self) -> Vec<Section> {
        let mut refreshes_needed = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Pools(host, result) => {
                    self.end_refresh(Section::Pools, host);
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(pools) => {
                            self.pools = pools;
                            clamp_table_selection(&mut self.pools_table, self.pools.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::Datasets(host, result) => {
                    self.end_refresh(Section::Datasets, host);
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(datasets) => {
                            self.datasets = datasets;
                            clamp_table_selection(&mut self.datasets_table, self.datasets.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::Snapshots(host, result) => {
                    self.end_refresh(Section::Snapshots, host);
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(snapshots) => {
                            self.snapshots = snapshots;
                            clamp_table_selection(&mut self.snapshots_table, self.snapshots.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::Vdevs(host, result) => {
                    self.end_refresh(Section::Vdevs, host);
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(vdevs) => {
                            self.vdevs = vdevs;
                            clamp_table_selection(&mut self.vdevs_table, self.vdevs.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
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
            }
        }
        refreshes_needed
    }
```

- [ ] **Step 11: Update the two other call sites of `drain_worker_messages`**

Both the existing tests and `event_loop` (Step 13 below) called `app.drain_worker_messages();` as a bare statement, discarding the (previously nonexistent) return value — that continues to compile unchanged for tests that don't need the return list (Rust doesn't require using a return value unless it's marked `#[must_use]`, which this isn't). No test changes are needed here beyond what Step 4 already added.

- [ ] **Step 12: Run the state and modal tests**

Run: `cargo test ui::state ui::modal`
Expected: PASS, all new tests plus existing ones green

- [ ] **Step 13: Wire modal-mode key routing and post-mutation refresh dispatch into `src/ui/mod.rs`**

Replace the body of `event_loop`:

```rust
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    inventory: &HostInventory,
) -> Result<()> {
    loop {
        let refreshes = app.drain_worker_messages();
        for section in refreshes {
            if let Some(host) = inventory.hosts.get(app.selected_host) {
                match section {
                    Section::Pools => app.refresh_pools(host.runner.clone()),
                    Section::Datasets => app.refresh_datasets(host.runner.clone()),
                    Section::Snapshots => app.refresh_snapshots(host.runner.clone()),
                    Section::Vdevs => {
                        if let Some(pool) = app.drilled_pool.clone() {
                            app.refresh_vdevs(host.runner.clone(), pool);
                        }
                    }
                    Section::Hosts => {}
                }
            }
        }

        terminal
            .draw(|f| layout::draw(f, app))
            .map_err(|e| AppError::Config(format!("drawing frame: {e}")))?;

        if event::poll(Duration::from_millis(250))
            .map_err(|e| AppError::Config(format!("polling events: {e}")))?
        {
            if let Event::Key(key) =
                event::read().map_err(|e| AppError::Config(format!("reading event: {e}")))?
            {
                if app.modal.is_some() {
                    if let Some(action) = app.handle_modal_key(key.code) {
                        if let Some(host) = inventory.hosts.get(action.host) {
                            app.dispatch_mutation(action, host.runner.clone());
                        }
                    }
                } else {
                    app.handle_key(key.code);
                    if key.code == KeyCode::Char('r') {
                        if let Some(host) = inventory.hosts.get(app.selected_host) {
                            if let Some(pool) = app.drilled_pool.clone() {
                                app.refresh_vdevs(host.runner.clone(), pool);
                            } else {
                                match app.current_section() {
                                    Section::Pools => app.refresh_pools(host.runner.clone()),
                                    Section::Datasets => app.refresh_datasets(host.runner.clone()),
                                    Section::Snapshots => app.refresh_snapshots(host.runner.clone()),
                                    Section::Hosts | Section::Vdevs => {}
                                }
                            }
                        }
                    }
                    if key.code == KeyCode::Enter {
                        if let Some(pool) = app.drilled_pool.clone() {
                            if let Some(host) = inventory.hosts.get(app.selected_host) {
                                app.refresh_vdevs(host.runner.clone(), pool);
                            }
                        }
                    }
                }
            }
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}
```

(Modal-mode input is now fully separate from normal-mode input: while a modal is open, `handle_key`'s section/row navigation and the `'r'`/`Enter` shortcuts are bypassed entirely, since they'd be meaningless — and, for `'y'`/`'n'`, actively wrong — while a confirmation is on screen.)

- [ ] **Step 14: Render the modal overlay in `src/ui/layout.rs`**

Add `Clear` to the `ratatui::widgets` import:

```rust
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Row, Table};
```

Add `use crate::ui::modal::Modal;` to the imports.

Change `pub fn draw` to render the modal last, on top of everything else:

```rust
pub fn draw(f: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(f.area());

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(0)])
        .split(outer[0]);

    draw_sidebar(f, app, chunks[0]);
    draw_main(f, app, chunks[1]);
    draw_status_bar(f, app, outer[1]);

    if let Some(modal) = app.modal.clone() {
        let area = f.area();
        draw_modal(f, &modal, area);
    }
}
```

Add `draw_modal` and its `centered_rect` helper at the end of the file:

```rust
fn draw_modal(f: &mut Frame, modal: &Modal, area: Rect) {
    let popup_area = centered_rect(60, 20, area);
    f.render_widget(Clear, popup_area);
    match modal {
        Modal::Confirm { prompt, .. } => {
            let p = Paragraph::new(prompt.as_str())
                .block(Block::default().borders(Borders::ALL).title("Confirm (y/n)"));
            f.render_widget(p, popup_area);
        }
        Modal::ConfirmTyped { prompt, input, .. } => {
            let text = format!("{prompt}\n\n> {input}");
            let p = Paragraph::new(text)
                .block(Block::default().borders(Borders::ALL).title("Type to confirm (Enter/Esc)"));
            f.render_widget(p, popup_area);
        }
    }
}

/// Carves a centered rectangle out of `area`, `percent_x`/`percent_y`
/// wide/tall as a percentage of `area` itself. Standard ratatui popup
/// pattern.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
```

(`app.modal.clone()` sidesteps holding an immutable borrow of `app` — needed for `Modal`'s contents — across the point where `draw_main` already took a mutable borrow earlier in the same function; `Modal`/`PendingAction`/`MutationCall` are all cheap, string-and-enum-sized values, so the clone is not a performance concern.)

- [ ] **Step 15: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, 102 tests (88 from Task 4 + 14 new from this task: 2 in `ui::modal` + 12 in `ui::state`); release build clean aside from expected dead_code warnings

- [ ] **Step 16: Commit**

```bash
git add src/ui/modal.rs src/ui/state.rs src/ui/layout.rs src/ui/mod.rs
git commit -m "feat: modal dialog core, mutation dispatch, and Scrub Pool wiring"
```

---

### Task 6: Destroy Pool wiring

**Files:**
- Modify: `src/ui/modal.rs`
- Modify: `src/ui/state.rs`

**Interfaces:**
- Consumes: `Modal::ConfirmTyped`, `PendingAction` (Task 5).
- Produces: `MutationCall` gains `DestroyPool { pool: String }`.

Reuses `Modal::ConfirmTyped` exactly as built in Task 5 — no new modal plumbing, just a new `MutationCall` variant and a new `handle_key` arm.

- [ ] **Step 1: Write failing tests**

Add to `src/ui/modal.rs`'s test module:

```rust
#[test]
fn destroy_pool_call_runs_destroy_pool() {
    let runner = FakeRunner::new("");
    let call = MutationCall::DestroyPool { pool: "scratch".to_string() };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0], ("zpool".to_string(), vec!["destroy".to_string(), "--".to_string(), "scratch".to_string()]));
}
```

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn handle_key_d_on_pools_opens_destroy_confirm_typed_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "scratch".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Right);
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
            assert_eq!(expected, "scratch");
            assert_eq!(action.call, crate::ui::modal::MutationCall::DestroyPool { pool: "scratch".to_string() });
        }
        other => panic!("expected ConfirmTyped modal, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test destroy_pool_call_runs_destroy_pool handle_key_d_on_pools_opens_destroy_confirm_typed_modal`
Expected: FAIL (`MutationCall::DestroyPool` doesn't exist yet)

- [ ] **Step 3: Add the `DestroyPool` variant**

In `src/ui/modal.rs`, change the `MutationCall` enum and its `run` match:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum MutationCall {
    ScrubPool { pool: String },
    DestroyPool { pool: String },
}

impl MutationCall {
    /// The single place that maps a `MutationCall` variant to its
    /// underlying `zfs::*` library call.
    pub fn run(&self, runner: &dyn CommandRunner) -> Result<()> {
        match self {
            MutationCall::ScrubPool { pool } => crate::zfs::pool::scrub_pool(runner, pool),
            MutationCall::DestroyPool { pool } => crate::zfs::pool::destroy_pool(runner, pool),
        }
    }
}
```

- [ ] **Step 4: Add the `'d'` action key to `handle_key` in `src/ui/state.rs`**

Add, right after the `'s'` (scrub) arm added in Task 5:

```rust
            KeyCode::Char('d') if self.current_section() == Section::Pools && self.drilled_pool.is_none() => {
                if let Some(i) = self.pools_table.selected() {
                    if let Some(pool) = self.pools.get(i) {
                        self.modal = Some(Modal::ConfirmTyped {
                            prompt: format!("Type the pool name to destroy '{}':", pool.name),
                            expected: pool.name.clone(),
                            input: String::new(),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Pools,
                                call: MutationCall::DestroyPool { pool: pool.name.clone() },
                            },
                        });
                    }
                }
            }
```

- [ ] **Step 5: Run the full test suite**

Run: `cargo test`
Expected: PASS, 104 tests (102 from Task 5 + 2 new)

- [ ] **Step 6: Commit**

```bash
git add src/ui/modal.rs src/ui/state.rs
git commit -m "feat: destroy pool wiring"
```

---

### Task 7: Create Pool wiring (introduces `Modal::Form`)

**Files:**
- Modify: `src/ui/modal.rs`
- Modify: `src/ui/state.rs`
- Modify: `src/ui/layout.rs`

**Interfaces:**
- Consumes: `FormField`, `PendingAction` (Task 5).
- Produces: `MutationCall` gains `CreatePool { name: String, vdev_spec: Vec<String> }`. `Modal` gains `Form { title: String, fields: Vec<FormField>, focused: usize, kind: FormKind }`. New `FormKind` enum (starts with `CreatePool { host: usize }`). `App::handle_modal_key` gains the `Form` match arm. Free function `fn build_pending_action(kind: &FormKind, fields: &[FormField]) -> Option<PendingAction>` in `src/ui/state.rs`.

This is the first form-based mutation, so it introduces the general `Form` mechanics: `Tab` moves focus between fields, `Char`/`Backspace` edit the focused field, `Enter` on a non-last field moves to the next one, `Enter` on the last field attempts to submit — if the field values fail validation (e.g. an empty required field), the form modal is put back with the user's input intact rather than silently discarding it.

- [ ] **Step 1: Write failing tests**

Add to `src/ui/modal.rs`'s test module:

```rust
#[test]
fn create_pool_call_splits_vdev_spec_and_runs_create_pool() {
    let runner = FakeRunner::new("");
    let call = MutationCall::CreatePool {
        name: "tank".to_string(),
        vdev_spec: vec!["mirror".to_string(), "da0".to_string(), "da1".to_string()],
    };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(
        calls[0],
        (
            "zpool".to_string(),
            vec!["create".to_string(), "--".to_string(), "tank".to_string(), "mirror".to_string(), "da0".to_string(), "da1".to_string()]
        )
    );
}
```

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn handle_key_n_on_pools_opens_create_pool_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Right); // Hosts -> Pools
    app.handle_key(KeyCode::Char('n'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, focused, .. }) => {
            assert_eq!(fields.len(), 2);
            assert_eq!(*focused, 0);
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_form_char_edits_focused_field() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Char('t'));
    app.handle_modal_key(KeyCode::Char('a'));
    app.handle_modal_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Char('k'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, .. }) => assert_eq!(fields[0].value, "tank"),
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_form_tab_moves_focus_and_wraps() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Tab);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 1),
        other => panic!("expected Form modal, got {other:?}"),
    }
    app.handle_modal_key(KeyCode::Tab);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 0, "must wrap back to the first field"),
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_form_enter_on_non_last_field_advances_focus() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('n'));
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 1),
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_form_enter_on_last_field_with_empty_required_value_keeps_form_open() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Char('t')); // name = "t"
    app.handle_modal_key(KeyCode::Tab); // move to vdev spec field, leave it empty
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None, "an empty required field must block submission");
    assert!(app.modal.is_some(), "the form must stay open so the user can fill in the missing field");
}

#[test]
fn handle_modal_key_form_enter_on_last_field_with_valid_values_submits() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('n'));
    for c in "tank".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in "mirror da0 da1".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Pools,
            call: crate::ui::modal::MutationCall::CreatePool {
                name: "tank".to_string(),
                vdev_spec: vec!["mirror".to_string(), "da0".to_string(), "da1".to_string()],
            },
        })
    );
    assert!(app.modal.is_none());
}

#[test]
fn handle_modal_key_form_esc_cancels() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('n'));
    let result = app.handle_modal_key(KeyCode::Esc);
    assert_eq!(result, None);
    assert!(app.modal.is_none());
}

#[test]
fn handle_modal_key_form_backspace_edits_focused_field() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Char('t'));
    app.handle_modal_key(KeyCode::Char('x'));
    app.handle_modal_key(KeyCode::Backspace);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, .. }) => assert_eq!(fields[0].value, "t"),
        other => panic!("expected Form modal, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test handle_key_n_on_pools_opens_create_pool_form`
Expected: FAIL (`MutationCall::CreatePool` and `Modal::Form` don't exist yet)

- [ ] **Step 3: Add `CreatePool`, `Modal::Form`, and `FormKind` to `src/ui/modal.rs`**

Replace the full contents of `src/ui/modal.rs`'s non-test code (everything above `#[cfg(test)]`) with:

```rust
use crate::error::Result;
use crate::exec::CommandRunner;
use crate::ui::state::Section;

/// Names a concrete `zfs::*` mutation call, with exactly the arguments
/// that function needs. Built from a modal's collected input (or, for
/// `Confirm`/`ConfirmTyped`, from the row that was selected when the
/// modal opened) and run on a worker thread once the modal is
/// confirmed/submitted.
#[derive(Debug, Clone, PartialEq)]
pub enum MutationCall {
    ScrubPool { pool: String },
    DestroyPool { pool: String },
    CreatePool { name: String, vdev_spec: Vec<String> },
}

impl MutationCall {
    /// The single place that maps a `MutationCall` variant to its
    /// underlying `zfs::*` library call.
    pub fn run(&self, runner: &dyn CommandRunner) -> Result<()> {
        match self {
            MutationCall::ScrubPool { pool } => crate::zfs::pool::scrub_pool(runner, pool),
            MutationCall::DestroyPool { pool } => crate::zfs::pool::destroy_pool(runner, pool),
            MutationCall::CreatePool { name, vdev_spec } => {
                let spec: Vec<&str> = vdev_spec.iter().map(String::as_str).collect();
                crate::zfs::pool::create_pool(runner, name, &spec)
            }
        }
    }
}

/// Everything needed to actually run a mutation once its modal is
/// confirmed: which host to run it against, which section to re-list
/// on success, and the call itself.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingAction {
    pub host: usize,
    pub section: Section,
    pub call: MutationCall,
}

/// A single labeled text input in a `Modal::Form`.
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    pub label: String,
    pub value: String,
}

/// Which mutation a `Modal::Form` is collecting input for, plus the
/// context (host, and any row data fixed at modal-open time) that
/// doesn't come from the form's fields themselves. Field values are
/// read positionally in `build_pending_action` — the order here must
/// match the order the modal's `fields` were constructed in.
#[derive(Debug, Clone, PartialEq)]
pub enum FormKind {
    /// Fields: `[name, vdev_spec]`.
    CreatePool { host: usize },
}

/// A modal dialog awaiting user input before a mutation runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// Simple y/n confirmation for reversible/lower-risk operations.
    Confirm { prompt: String, action: PendingAction },
    /// Type-the-exact-name confirmation for irreversible operations.
    ConfirmTyped {
        prompt: String,
        expected: String,
        input: String,
        action: PendingAction,
    },
    /// Labeled text-field input for creation operations. No
    /// confirmation beyond submitting the form itself.
    Form {
        title: String,
        fields: Vec<FormField>,
        focused: usize,
        kind: FormKind,
    },
}
```

- [ ] **Step 4: Add `build_pending_action` and the `Form` arm to `handle_modal_key` in `src/ui/state.rs`**

Add `use crate::ui::modal::{FormField, FormKind, Modal, MutationCall, PendingAction};` (replacing the narrower import from Task 5) to the top of `src/ui/state.rs`.

Add this free function near `move_table_selection`/`clamp_table_selection`:

```rust
/// Builds the `PendingAction` for a submitted form, mapping its fields
/// positionally per `FormKind`'s documented field order. Returns `None`
/// if a required field is empty — the caller keeps the form modal open
/// in that case rather than losing the user's input.
fn build_pending_action(kind: &FormKind, fields: &[FormField]) -> Option<PendingAction> {
    match kind {
        FormKind::CreatePool { host } => {
            let name = fields[0].value.trim();
            let vdev_spec_raw = fields[1].value.trim();
            if name.is_empty() || vdev_spec_raw.is_empty() {
                return None;
            }
            let vdev_spec: Vec<String> = vdev_spec_raw.split_whitespace().map(String::from).collect();
            Some(PendingAction {
                host: *host,
                section: Section::Pools,
                call: MutationCall::CreatePool { name: name.to_string(), vdev_spec },
            })
        }
    }
}
```

Add a `Modal::Form` arm to `handle_modal_key`'s `match modal`, alongside `Confirm`/`ConfirmTyped`:

```rust
            Modal::Form { title, mut fields, mut focused, kind } => match key {
                KeyCode::Esc => None,
                KeyCode::Tab => {
                    focused = (focused + 1) % fields.len();
                    self.modal = Some(Modal::Form { title, fields, focused, kind });
                    None
                }
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
                KeyCode::Backspace => {
                    fields[focused].value.pop();
                    self.modal = Some(Modal::Form { title, fields, focused, kind });
                    None
                }
                KeyCode::Char(c) => {
                    fields[focused].value.push(c);
                    self.modal = Some(Modal::Form { title, fields, focused, kind });
                    None
                }
                _ => {
                    self.modal = Some(Modal::Form { title, fields, focused, kind });
                    None
                }
            },
```

- [ ] **Step 5: Add the `'n'` action key to `handle_key`**

Add, right after the `'d'` (destroy) arm added in Task 6:

```rust
            KeyCode::Char('n') if self.current_section() == Section::Pools && self.drilled_pool.is_none() => {
                self.modal = Some(Modal::Form {
                    title: "Create Pool".to_string(),
                    fields: vec![
                        FormField { label: "Name".to_string(), value: String::new() },
                        FormField { label: "Vdev spec (e.g. mirror da0 da1)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    kind: FormKind::CreatePool { host: self.selected_host },
                });
            }
```

- [ ] **Step 6: Render the `Form` modal in `src/ui/layout.rs`**

Add a `Modal::Form` arm to `draw_modal`'s `match modal`:

```rust
        Modal::Form { title, fields, focused, .. } => {
            let mut lines = Vec::new();
            for (i, field) in fields.iter().enumerate() {
                let marker = if i == *focused { ">" } else { " " };
                lines.push(format!("{marker} {}: {}", field.label, field.value));
            }
            let p = Paragraph::new(lines.join("\n"))
                .block(Block::default().borders(Borders::ALL).title(title.as_str()));
            f.render_widget(p, popup_area);
        }
```

- [ ] **Step 7: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, 113 tests (104 from Task 6 + 9 new: 1 in `ui::modal` + 8 in `ui::state`); release build clean aside from expected dead_code warnings

- [ ] **Step 8: Commit**

```bash
git add src/ui/modal.rs src/ui/state.rs src/ui/layout.rs
git commit -m "feat: create pool wiring, introduce Modal::Form"
```

---

### Task 8: Offline and Online disk wiring

**Files:**
- Modify: `src/zfs/pool.rs`
- Modify: `src/ui/modal.rs`
- Modify: `src/ui/state.rs`

**Interfaces:**
- Consumes: `VdevLine`, `Modal::Confirm` (Tasks 4-5).
- Produces: `pub fn is_leaf_disk(name: &str) -> bool` in `src/zfs/pool.rs`. `MutationCall` gains `OfflineDisk { pool: String, disk: String }` and `OnlineDisk { pool: String, disk: String }`.

Disk-level operations only make sense targeting a physical disk, not an intermediate `mirror-N`/`raidzN-N` grouping node in the vdev tree. `is_leaf_disk` distinguishes the two by name shape — ZFS names group vdevs `<type>-<index>` (`mirror-0`, `raidz1-2`, etc.), a pattern a physical disk name (`da0`, `nvd1`, ...) never matches. Selecting a group row and pressing `o`/`O` is a no-op with a hint in the status bar, rather than silently doing nothing or erroring.

- [ ] **Step 1: Write failing tests for `is_leaf_disk`**

Add to `src/zfs/pool.rs`'s test module:

```rust
#[test]
fn is_leaf_disk_true_for_physical_device_names() {
    assert!(is_leaf_disk("da0"));
    assert!(is_leaf_disk("nvd1"));
    assert!(is_leaf_disk("tank")); // top-level pool name itself, if ever selected
}

#[test]
fn is_leaf_disk_false_for_group_vdev_names() {
    assert!(!is_leaf_disk("mirror-0"));
    assert!(!is_leaf_disk("raidz1-2"));
    assert!(!is_leaf_disk("raidz2-0"));
    assert!(!is_leaf_disk("raidz3-10"));
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test is_leaf_disk`
Expected: FAIL (function doesn't exist yet)

- [ ] **Step 3: Add `is_leaf_disk` to `src/zfs/pool.rs`**

Add after `parse_pool_status_vdevs`:

```rust
/// True unless `name` matches ZFS's group-vdev naming shape
/// (`mirror-N`, `raidz1-N`, `raidz2-N`, `raidz3-N`) — those are
/// intermediate tree nodes, not physical disks, and don't make sense
/// as an offline/online/replace target.
pub fn is_leaf_disk(name: &str) -> bool {
    const GROUP_PREFIXES: &[&str] = &["mirror-", "raidz1-", "raidz2-", "raidz3-"];
    !GROUP_PREFIXES.iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
    })
}
```

- [ ] **Step 4: Run the pool tests**

Run: `cargo test zfs::pool`
Expected: PASS, all tests including the 2 new ones

- [ ] **Step 5: Write failing tests for the offline/online wiring**

Add to `src/ui/modal.rs`'s test module:

```rust
#[test]
fn offline_disk_call_runs_offline_disk() {
    let runner = FakeRunner::new("");
    let call = MutationCall::OfflineDisk { pool: "tank".to_string(), disk: "da1".to_string() };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0], ("zpool".to_string(), vec!["offline".to_string(), "--".to_string(), "tank".to_string(), "da1".to_string()]));
}

#[test]
fn online_disk_call_runs_online_disk() {
    let runner = FakeRunner::new("");
    let call = MutationCall::OnlineDisk { pool: "tank".to_string(), disk: "da1".to_string() };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0], ("zpool".to_string(), vec!["online".to_string(), "--".to_string(), "tank".to_string(), "da1".to_string()]));
}
```

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn handle_key_o_on_leaf_disk_opens_offline_confirm_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Right);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('o'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { action, .. }) => {
            assert_eq!(action.section, Section::Vdevs);
            assert_eq!(action.call, crate::ui::modal::MutationCall::OfflineDisk { pool: "tank".to_string(), disk: "da1".to_string() });
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn handle_key_o_on_group_vdev_row_does_not_open_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Right);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "mirror-0".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('o'));
    assert!(app.modal.is_none());
    assert!(app.status_message.is_some(), "selecting a group row should hint that a disk must be selected instead");
}

#[test]
fn handle_key_shift_o_on_leaf_disk_opens_online_confirm_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Right);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Offline }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('O'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { action, .. }) => {
            assert_eq!(action.call, crate::ui::modal::MutationCall::OnlineDisk { pool: "tank".to_string(), disk: "da1".to_string() });
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn dispatch_mutation_on_vdev_section_queues_both_vdevs_and_pools_refresh() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Vdevs,
        call: crate::ui::modal::MutationCall::OfflineDisk { pool: "tank".to_string(), disk: "da1".to_string() },
    };
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(""));
    app.dispatch_mutation(action, runner);

    let deadline = Instant::now() + Duration::from_secs(2);
    let refreshes = loop {
        let r = app.drain_worker_messages();
        if !r.is_empty() || Instant::now() > deadline {
            break r;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(refreshes, vec![Section::Vdevs, Section::Pools]);
}
```

- [ ] **Step 6: Run the new tests to confirm they fail**

Run: `cargo test offline_disk_call_runs_offline_disk handle_key_o_on_leaf_disk_opens_offline_confirm_modal`
Expected: FAIL (`MutationCall::OfflineDisk` doesn't exist yet)

- [ ] **Step 7: Add `OfflineDisk`/`OnlineDisk` to `MutationCall` in `src/ui/modal.rs`**

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum MutationCall {
    ScrubPool { pool: String },
    DestroyPool { pool: String },
    CreatePool { name: String, vdev_spec: Vec<String> },
    OfflineDisk { pool: String, disk: String },
    OnlineDisk { pool: String, disk: String },
}
```

Add to `MutationCall::run`'s `match self`:

```rust
            MutationCall::OfflineDisk { pool, disk } => crate::zfs::pool::offline_disk(runner, pool, disk),
            MutationCall::OnlineDisk { pool, disk } => crate::zfs::pool::online_disk(runner, pool, disk),
```

- [ ] **Step 8: Add the `'o'`/`'O'` action keys to `handle_key` in `src/ui/state.rs`**

Add `VdevLine` is already imported (Task 4); no new import needed. Add, right after the vdev row-navigation arms (the `Section::Pools` pair guarded by `self.drilled_pool.is_some()` from Task 4):

```rust
            KeyCode::Char('o') if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                self.open_vdev_confirm(false);
            }
            KeyCode::Char('O') if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                self.open_vdev_confirm(true);
            }
```

Add this private helper method to the `impl App` block (used by both arms above, and by Task 9's `'x'` arm below):

```rust
    /// Shared by the offline/online disk action keys: looks up the
    /// selected vdev row, and — only if it's a leaf disk, not a
    /// mirror/raidz group node — opens the matching `Confirm` modal.
    /// `online` selects which of the two operations to build.
    fn open_vdev_confirm(&mut self, online: bool) {
        let Some(i) = self.vdevs_table.selected() else { return };
        let Some(vdev) = self.vdevs.get(i) else { return };
        if !crate::zfs::pool::is_leaf_disk(&vdev.name) {
            self.status_message = Some("select a disk (not a mirror/raidz group) to offline/online it".to_string());
            return;
        }
        let pool = self.drilled_pool.clone().unwrap();
        let disk = vdev.name.clone();
        let (verb, call) = if online {
            ("Online", MutationCall::OnlineDisk { pool: pool.clone(), disk: disk.clone() })
        } else {
            ("Offline", MutationCall::OfflineDisk { pool: pool.clone(), disk: disk.clone() })
        };
        self.modal = Some(Modal::Confirm {
            prompt: format!("{verb} disk '{disk}' in pool '{pool}'? (y/n)"),
            action: PendingAction { host: self.selected_host, section: Section::Vdevs, call },
        });
    }
```

- [ ] **Step 9: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, 121 tests (113 from Task 7 + 8 new: 2 in `zfs::pool` + 2 in `ui::modal` + 4 in `ui::state`); release build clean aside from expected dead_code warnings

- [ ] **Step 10: Commit**

```bash
git add src/zfs/pool.rs src/ui/modal.rs src/ui/state.rs
git commit -m "feat: offline/online disk wiring with leaf-disk filtering"
```

---

### Task 9: Replace disk wiring

**Files:**
- Modify: `src/ui/modal.rs`
- Modify: `src/ui/state.rs`

**Interfaces:**
- Consumes: `is_leaf_disk` (Task 8), `Modal::Form`/`FormKind` (Task 7).
- Produces: `MutationCall` gains `ReplaceDisk { pool: String, old_disk: String, new_disk: String }`. `FormKind` gains `ReplaceDisk { host: usize, pool: String, old_disk: String }`.

Reuses the `Form` machinery from Task 7 — one field (new device name), with `pool`/`old_disk` fixed at modal-open time from the selected vdev row (same leaf-disk filtering as Task 8's offline/online).

- [ ] **Step 1: Write failing tests**

Add to `src/ui/modal.rs`'s test module:

```rust
#[test]
fn replace_disk_call_runs_replace_disk() {
    let runner = FakeRunner::new("");
    let call = MutationCall::ReplaceDisk { pool: "tank".to_string(), old_disk: "da1".to_string(), new_disk: "da2".to_string() };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(
        calls[0],
        ("zpool".to_string(), vec!["replace".to_string(), "--".to_string(), "tank".to_string(), "da1".to_string(), "da2".to_string()])
    );
}
```

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn handle_key_x_on_leaf_disk_opens_replace_disk_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Right);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Faulted }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('x'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 1);
            assert_eq!(
                *kind,
                crate::ui::modal::FormKind::ReplaceDisk { host: 0, pool: "tank".to_string(), old_disk: "da1".to_string() }
            );
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_x_on_group_vdev_row_does_not_open_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Right);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "raidz1-0".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('x'));
    assert!(app.modal.is_none());
    assert!(app.status_message.is_some());
}

#[test]
fn handle_modal_key_replace_disk_form_submits_with_correct_pool_and_disks() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Right);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Faulted }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('x'));
    for c in "da2".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Vdevs,
            call: crate::ui::modal::MutationCall::ReplaceDisk {
                pool: "tank".to_string(),
                old_disk: "da1".to_string(),
                new_disk: "da2".to_string(),
            },
        })
    );
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test replace_disk_call_runs_replace_disk handle_key_x_on_leaf_disk_opens_replace_disk_form`
Expected: FAIL (`MutationCall::ReplaceDisk`/`FormKind::ReplaceDisk` don't exist yet)

- [ ] **Step 3: Add `ReplaceDisk` to `MutationCall` and `FormKind` in `src/ui/modal.rs`**

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum MutationCall {
    ScrubPool { pool: String },
    DestroyPool { pool: String },
    CreatePool { name: String, vdev_spec: Vec<String> },
    OfflineDisk { pool: String, disk: String },
    OnlineDisk { pool: String, disk: String },
    ReplaceDisk { pool: String, old_disk: String, new_disk: String },
}
```

Add to `MutationCall::run`'s `match self`:

```rust
            MutationCall::ReplaceDisk { pool, old_disk, new_disk } => {
                crate::zfs::pool::replace_disk(runner, pool, old_disk, new_disk)
            }
```

Change `FormKind`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum FormKind {
    /// Fields: `[name, vdev_spec]`.
    CreatePool { host: usize },
    /// Fields: `[new_disk]`.
    ReplaceDisk { host: usize, pool: String, old_disk: String },
}
```

- [ ] **Step 4: Add the `ReplaceDisk` arm to `build_pending_action` in `src/ui/state.rs`**

```rust
        FormKind::ReplaceDisk { host, pool, old_disk } => {
            let new_disk = fields[0].value.trim();
            if new_disk.is_empty() {
                return None;
            }
            Some(PendingAction {
                host: *host,
                section: Section::Vdevs,
                call: MutationCall::ReplaceDisk {
                    pool: pool.clone(),
                    old_disk: old_disk.clone(),
                    new_disk: new_disk.to_string(),
                },
            })
        }
```

- [ ] **Step 5: Add the `'x'` action key to `handle_key`**

Add, right after the `'o'`/`'O'` arms added in Task 8:

```rust
            KeyCode::Char('x') if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                if let Some(i) = self.vdevs_table.selected() {
                    if let Some(vdev) = self.vdevs.get(i) {
                        if crate::zfs::pool::is_leaf_disk(&vdev.name) {
                            let pool = self.drilled_pool.clone().unwrap();
                            self.modal = Some(Modal::Form {
                                title: format!("Replace disk '{}' in pool '{pool}'", vdev.name),
                                fields: vec![FormField { label: "New device".to_string(), value: String::new() }],
                                focused: 0,
                                kind: FormKind::ReplaceDisk { host: self.selected_host, pool, old_disk: vdev.name.clone() },
                            });
                        } else {
                            self.status_message = Some("select a disk (not a mirror/raidz group) to replace it".to_string());
                        }
                    }
                }
            }
```

- [ ] **Step 6: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, 125 tests (121 from Task 8 + 4 new: 1 in `ui::modal` + 3 in `ui::state`); release build clean aside from expected dead_code warnings

- [ ] **Step 7: Commit**

```bash
git add src/ui/modal.rs src/ui/state.rs
git commit -m "feat: replace disk wiring"
```

---

### Task 10: Create Dataset and Destroy Dataset wiring

**Files:**
- Modify: `src/ui/modal.rs`
- Modify: `src/ui/state.rs`

**Interfaces:**
- Consumes: `Modal::Form`/`FormKind` (Task 7), `Modal::ConfirmTyped` (Task 5).
- Produces: `MutationCall` gains `CreateDataset { name: String }` and `DestroyDataset { name: String }`. `FormKind` gains `CreateDataset { host: usize }`.

Both reuse existing modal machinery entirely — no new plumbing, just two more `MutationCall`/`FormKind` variants and two more `handle_key` arms, this time guarded on `Section::Datasets`.

- [ ] **Step 1: Write failing tests**

Add to `src/ui/modal.rs`'s test module:

```rust
#[test]
fn create_dataset_call_runs_create_dataset() {
    let runner = FakeRunner::new("");
    let call = MutationCall::CreateDataset { name: "tank/newds".to_string() };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0], ("zfs".to_string(), vec!["create".to_string(), "--".to_string(), "tank/newds".to_string()]));
}

#[test]
fn destroy_dataset_call_runs_destroy_dataset() {
    let runner = FakeRunner::new("");
    let call = MutationCall::DestroyDataset { name: "tank/olds".to_string() };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0], ("zfs".to_string(), vec!["destroy".to_string(), "--".to_string(), "tank/olds".to_string()]));
}
```

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn handle_key_n_on_datasets_opens_create_dataset_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Right); // Hosts -> Pools -> Datasets
    app.handle_key(KeyCode::Char('n'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 1);
            assert_eq!(*kind, crate::ui::modal::FormKind::CreateDataset { host: 0 });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_create_dataset_form_submits() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('n'));
    for c in "tank/newds".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Datasets,
            call: crate::ui::modal::MutationCall::CreateDataset { name: "tank/newds".to_string() },
        })
    );
}

#[test]
fn handle_key_d_on_datasets_opens_destroy_confirm_typed_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![crate::zfs::dataset::Dataset {
        name: "tank/olds".to_string(),
        kind: crate::zfs::dataset::DatasetKind::Filesystem,
        used: 0,
        available: 0,
        referenced: 0,
        mountpoint: None,
    }];
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Right);
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
            assert_eq!(expected, "tank/olds");
            assert_eq!(action.call, crate::ui::modal::MutationCall::DestroyDataset { name: "tank/olds".to_string() });
        }
        other => panic!("expected ConfirmTyped modal, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test create_dataset_call_runs_create_dataset destroy_dataset_call_runs_destroy_dataset handle_key_n_on_datasets_opens_create_dataset_form handle_key_d_on_datasets_opens_destroy_confirm_typed_modal`
Expected: FAIL (new `MutationCall`/`FormKind` variants don't exist yet)

- [ ] **Step 3: Add `CreateDataset`/`DestroyDataset` to `MutationCall`, and `CreateDataset` to `FormKind`, in `src/ui/modal.rs`**

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum MutationCall {
    ScrubPool { pool: String },
    DestroyPool { pool: String },
    CreatePool { name: String, vdev_spec: Vec<String> },
    OfflineDisk { pool: String, disk: String },
    OnlineDisk { pool: String, disk: String },
    ReplaceDisk { pool: String, old_disk: String, new_disk: String },
    CreateDataset { name: String },
    DestroyDataset { name: String },
}
```

Add to `MutationCall::run`'s `match self`:

```rust
            MutationCall::CreateDataset { name } => crate::zfs::dataset::create_dataset(runner, name),
            MutationCall::DestroyDataset { name } => crate::zfs::dataset::destroy_dataset(runner, name),
```

Change `FormKind`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum FormKind {
    /// Fields: `[name, vdev_spec]`.
    CreatePool { host: usize },
    /// Fields: `[new_disk]`.
    ReplaceDisk { host: usize, pool: String, old_disk: String },
    /// Fields: `[name]`.
    CreateDataset { host: usize },
}
```

- [ ] **Step 4: Add the `CreateDataset` arm to `build_pending_action` in `src/ui/state.rs`**

```rust
        FormKind::CreateDataset { host } => {
            let name = fields[0].value.trim();
            if name.is_empty() {
                return None;
            }
            Some(PendingAction {
                host: *host,
                section: Section::Datasets,
                call: MutationCall::CreateDataset { name: name.to_string() },
            })
        }
```

- [ ] **Step 5: Add the `'n'`/`'d'` action keys on `Section::Datasets` to `handle_key`**

Add, right after the existing `Section::Datasets` row-navigation arms:

```rust
            KeyCode::Char('n') if self.current_section() == Section::Datasets => {
                self.modal = Some(Modal::Form {
                    title: "Create Dataset".to_string(),
                    fields: vec![FormField { label: "Name".to_string(), value: String::new() }],
                    focused: 0,
                    kind: FormKind::CreateDataset { host: self.selected_host },
                });
            }
            KeyCode::Char('d') if self.current_section() == Section::Datasets => {
                if let Some(i) = self.datasets_table.selected() {
                    if let Some(ds) = self.datasets.get(i) {
                        self.modal = Some(Modal::ConfirmTyped {
                            prompt: format!("Type the dataset name to destroy '{}':", ds.name),
                            expected: ds.name.clone(),
                            input: String::new(),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Datasets,
                                call: MutationCall::DestroyDataset { name: ds.name.clone() },
                            },
                        });
                    }
                }
            }
```

- [ ] **Step 6: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, 130 tests (125 from Task 9 + 5 new: 2 in `ui::modal` + 3 in `ui::state`); release build clean aside from expected dead_code warnings

- [ ] **Step 7: Commit**

```bash
git add src/ui/modal.rs src/ui/state.rs
git commit -m "feat: create/destroy dataset wiring"
```

---

### Task 11: Create Snapshot wiring (from the Datasets view)

**Files:**
- Modify: `src/ui/modal.rs`
- Modify: `src/ui/state.rs`

**Interfaces:**
- Consumes: `Modal::Form`/`FormKind` (Task 7).
- Produces: `MutationCall` gains `CreateSnapshot { dataset: String, name: String }`. `FormKind` gains `CreateSnapshot { host: usize, dataset: String }`.

Per the spec, creating a snapshot is triggered from the Datasets view (the dataset is implied by the selected row), not from the Snapshots view — the form only collects the new snapshot's name.

- [ ] **Step 1: Write failing tests**

Add to `src/ui/modal.rs`'s test module:

```rust
#[test]
fn create_snapshot_call_runs_create_snapshot() {
    let runner = FakeRunner::new("");
    let call = MutationCall::CreateSnapshot { dataset: "tank/data".to_string(), name: "manual-1".to_string() };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0], ("zfs".to_string(), vec!["snapshot".to_string(), "--".to_string(), "tank/data@manual-1".to_string()]));
}
```

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn handle_key_s_on_datasets_opens_create_snapshot_form_with_implied_dataset() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![crate::zfs::dataset::Dataset {
        name: "tank/data".to_string(),
        kind: crate::zfs::dataset::DatasetKind::Filesystem,
        used: 0,
        available: 0,
        referenced: 0,
        mountpoint: None,
    }];
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Right);
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Char('s'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 1);
            assert_eq!(*kind, crate::ui::modal::FormKind::CreateSnapshot { host: 0, dataset: "tank/data".to_string() });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_create_snapshot_form_submits_with_implied_dataset() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![crate::zfs::dataset::Dataset {
        name: "tank/data".to_string(),
        kind: crate::zfs::dataset::DatasetKind::Filesystem,
        used: 0,
        available: 0,
        referenced: 0,
        mountpoint: None,
    }];
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Right);
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Char('s'));
    for c in "manual-1".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Snapshots,
            call: crate::ui::modal::MutationCall::CreateSnapshot {
                dataset: "tank/data".to_string(),
                name: "manual-1".to_string(),
            },
        })
    );
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test create_snapshot_call_runs_create_snapshot handle_key_s_on_datasets_opens_create_snapshot_form_with_implied_dataset`
Expected: FAIL (`MutationCall::CreateSnapshot`/`FormKind::CreateSnapshot` don't exist yet)

- [ ] **Step 3: Add `CreateSnapshot` to `MutationCall` and `FormKind` in `src/ui/modal.rs`**

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum MutationCall {
    ScrubPool { pool: String },
    DestroyPool { pool: String },
    CreatePool { name: String, vdev_spec: Vec<String> },
    OfflineDisk { pool: String, disk: String },
    OnlineDisk { pool: String, disk: String },
    ReplaceDisk { pool: String, old_disk: String, new_disk: String },
    CreateDataset { name: String },
    DestroyDataset { name: String },
    CreateSnapshot { dataset: String, name: String },
}
```

Add to `MutationCall::run`'s `match self`:

```rust
            MutationCall::CreateSnapshot { dataset, name } => crate::zfs::snapshot::create_snapshot(runner, dataset, name),
```

Change `FormKind`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum FormKind {
    /// Fields: `[name, vdev_spec]`.
    CreatePool { host: usize },
    /// Fields: `[new_disk]`.
    ReplaceDisk { host: usize, pool: String, old_disk: String },
    /// Fields: `[name]`.
    CreateDataset { host: usize },
    /// Fields: `[name]` — the snapshot's name; the dataset is fixed at
    /// modal-open time from the selected Datasets row.
    CreateSnapshot { host: usize, dataset: String },
}
```

- [ ] **Step 4: Add the `CreateSnapshot` arm to `build_pending_action` in `src/ui/state.rs`**

```rust
        FormKind::CreateSnapshot { host, dataset } => {
            let name = fields[0].value.trim();
            if name.is_empty() {
                return None;
            }
            Some(PendingAction {
                host: *host,
                section: Section::Snapshots,
                call: MutationCall::CreateSnapshot { dataset: dataset.clone(), name: name.to_string() },
            })
        }
```

- [ ] **Step 5: Add the `'s'` action key on `Section::Datasets` to `handle_key`**

Add, right after the `'n'`/`'d'` arms added in Task 10:

```rust
            KeyCode::Char('s') if self.current_section() == Section::Datasets => {
                if let Some(i) = self.datasets_table.selected() {
                    if let Some(ds) = self.datasets.get(i) {
                        self.modal = Some(Modal::Form {
                            title: format!("Snapshot dataset '{}'", ds.name),
                            fields: vec![FormField { label: "Snapshot name".to_string(), value: String::new() }],
                            focused: 0,
                            kind: FormKind::CreateSnapshot { host: self.selected_host, dataset: ds.name.clone() },
                        });
                    }
                }
            }
```

- [ ] **Step 6: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, 133 tests (130 from Task 10 + 3 new: 1 in `ui::modal` + 2 in `ui::state`); release build clean aside from expected dead_code warnings

- [ ] **Step 7: Commit**

```bash
git add src/ui/modal.rs src/ui/state.rs
git commit -m "feat: create snapshot wiring from the datasets view"
```

---

### Task 12: Destroy Snapshot and Rollback Snapshot wiring

**Files:**
- Modify: `src/ui/modal.rs`
- Modify: `src/ui/state.rs`

**Interfaces:**
- Consumes: `Modal::ConfirmTyped` (Task 5).
- Produces: `MutationCall` gains `DestroySnapshot { dataset: String, name: String }` and `RollbackSnapshot { dataset: String, name: String }`.

Both are irreversible (rollback discards any writes made since the snapshot), so both use `ConfirmTyped` with the full `dataset@name` as the expected confirmation text — matching the Destroy Pool/Dataset precedent from Tasks 6 and 10.

- [ ] **Step 1: Write failing tests**

Add to `src/ui/modal.rs`'s test module:

```rust
#[test]
fn destroy_snapshot_call_runs_destroy_snapshot() {
    let runner = FakeRunner::new("");
    let call = MutationCall::DestroySnapshot { dataset: "tank/data".to_string(), name: "old-1".to_string() };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0], ("zfs".to_string(), vec!["destroy".to_string(), "--".to_string(), "tank/data@old-1".to_string()]));
}

#[test]
fn rollback_snapshot_call_runs_rollback_snapshot() {
    let runner = FakeRunner::new("");
    let call = MutationCall::RollbackSnapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string() };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0], ("zfs".to_string(), vec!["rollback".to_string(), "--".to_string(), "tank/data@2026-09-01".to_string()]));
}
```

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn handle_key_d_on_snapshots_opens_destroy_confirm_typed_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "old-1".to_string(), used: 0 }];
    app.handle_key(KeyCode::Left); // Hosts -> Snapshots
    app.snapshots_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
            assert_eq!(expected, "tank/data@old-1");
            assert_eq!(
                action.call,
                crate::ui::modal::MutationCall::DestroySnapshot { dataset: "tank/data".to_string(), name: "old-1".to_string() }
            );
        }
        other => panic!("expected ConfirmTyped modal, got {other:?}"),
    }
}

#[test]
fn handle_key_b_on_snapshots_opens_rollback_confirm_typed_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string(), used: 0 }];
    app.handle_key(KeyCode::Left);
    app.snapshots_table.select(Some(0));
    app.handle_key(KeyCode::Char('b'));
    match app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
            assert_eq!(expected, "tank/data@2026-09-01");
            assert_eq!(
                action.call,
                crate::ui::modal::MutationCall::RollbackSnapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string() }
            );
        }
        other => panic!("expected ConfirmTyped modal, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test destroy_snapshot_call_runs_destroy_snapshot rollback_snapshot_call_runs_rollback_snapshot handle_key_d_on_snapshots_opens_destroy_confirm_typed_modal handle_key_b_on_snapshots_opens_rollback_confirm_typed_modal`
Expected: FAIL (new `MutationCall` variants don't exist yet)

- [ ] **Step 3: Add `DestroySnapshot`/`RollbackSnapshot` to `MutationCall` in `src/ui/modal.rs`**

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum MutationCall {
    ScrubPool { pool: String },
    DestroyPool { pool: String },
    CreatePool { name: String, vdev_spec: Vec<String> },
    OfflineDisk { pool: String, disk: String },
    OnlineDisk { pool: String, disk: String },
    ReplaceDisk { pool: String, old_disk: String, new_disk: String },
    CreateDataset { name: String },
    DestroyDataset { name: String },
    CreateSnapshot { dataset: String, name: String },
    DestroySnapshot { dataset: String, name: String },
    RollbackSnapshot { dataset: String, name: String },
}
```

Add to `MutationCall::run`'s `match self`:

```rust
            MutationCall::DestroySnapshot { dataset, name } => crate::zfs::snapshot::destroy_snapshot(runner, dataset, name),
            MutationCall::RollbackSnapshot { dataset, name } => crate::zfs::snapshot::rollback_snapshot(runner, dataset, name),
```

This is the last `MutationCall` variant this plan adds — 11 total, matching the spec's "all 12 except `replicate_snapshot`".

- [ ] **Step 4: Add the `'d'`/`'b'` action keys on `Section::Snapshots` to `handle_key`**

Add, right after the existing `Section::Snapshots` row-navigation arms:

```rust
            KeyCode::Char('d') if self.current_section() == Section::Snapshots => {
                if let Some(i) = self.snapshots_table.selected() {
                    if let Some(snap) = self.snapshots.get(i) {
                        let target = format!("{}@{}", snap.dataset, snap.name);
                        self.modal = Some(Modal::ConfirmTyped {
                            prompt: format!("Type the snapshot name to destroy '{target}':"),
                            expected: target,
                            input: String::new(),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Snapshots,
                                call: MutationCall::DestroySnapshot { dataset: snap.dataset.clone(), name: snap.name.clone() },
                            },
                        });
                    }
                }
            }
            KeyCode::Char('b') if self.current_section() == Section::Snapshots => {
                if let Some(i) = self.snapshots_table.selected() {
                    if let Some(snap) = self.snapshots.get(i) {
                        let target = format!("{}@{}", snap.dataset, snap.name);
                        self.modal = Some(Modal::ConfirmTyped {
                            prompt: format!("Type the snapshot name to roll back to '{target}':"),
                            expected: target,
                            input: String::new(),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Snapshots,
                                call: MutationCall::RollbackSnapshot { dataset: snap.dataset.clone(), name: snap.name.clone() },
                            },
                        });
                    }
                }
            }
```

- [ ] **Step 5: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, 137 tests (133 from Task 11 + 4 new: 2 in `ui::modal` + 2 in `ui::state`); release build clean — and, for the first time in this plan, only the dead_code warnings the plan started with that this task doesn't touch: `is_leaf_disk` is now used (Task 8/9), and every `zfs::*` mutation function is now called from `MutationCall::run` — only `parse_pool_status_vdevs`/`VdevLine`/`get_pool_vdevs` (used, via Task 4's `refresh_vdevs`), `Config::save`, `HostInventory::get`, and the `replicate_snapshot`-only helpers (`is_safe_zfs_target`, `is_safe_host`, `replicate_snapshot` itself) remain unused, since replication is out of this plan's scope

- [ ] **Step 6: Commit**

```bash
git add src/ui/modal.rs src/ui/state.rs
git commit -m "feat: destroy/rollback snapshot wiring"
```

---

### Task 13: README update and manual verification

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: nothing new — this task only documents and manually verifies what Tasks 1-12 built.

- [ ] **Step 1: Update `README.md`'s Keys section and Status section**

Replace the existing `## Keys` section and the `## Status` section (added by Plan 1's final review fix round) with:

```markdown
## Keys

- `Tab` / `Left` / `Right`: switch sections (Hosts, Pools, Datasets, Snapshots)
- `Up` / `Down` (in Hosts section): select the active host
- `Up` / `Down` (in Pools/Datasets/Snapshots): select a row
- `r`: refresh the current section (or, while drilled into a pool's vdevs, the vdev view) for the selected host
- `q`: quit
- `Esc`: dismiss the current status message, or (while a modal is open) cancel it

### Pools

- `Enter` on a selected pool: drill into its vdev/disk tree
- `n`: create a new pool (prompts for name and vdev spec, e.g. `mirror da0 da1`)
- `d`: destroy the selected pool (type the pool's name to confirm)
- `s`: scrub the selected pool (y/n confirm)

### Vdevs (inside a drilled-into pool)

- `Esc` / `Backspace`: back out to the Pools list
- `o`: offline the selected disk (y/n confirm) — no-op on a mirror/raidz group row, not a physical disk
- `O`: online the selected disk (y/n confirm) — same restriction as `o`
- `x`: replace the selected disk (prompts for the new device name) — same restriction as `o`

### Datasets

- `n`: create a new dataset (prompts for name)
- `d`: destroy the selected dataset (type the dataset's name to confirm)
- `s`: snapshot the selected dataset (prompts for the snapshot's name only)

### Snapshots

- `d`: destroy the selected snapshot (type `dataset@name` to confirm)
- `b`: roll back to the selected snapshot (type `dataset@name` to confirm) — discards any writes made since

## Status

v1 covers browsing and 11 of the 12 ZFS operations implemented at the library layer: create/destroy/scrub a pool, offline/online/replace a disk, create/destroy a dataset, and create/destroy/rollback a snapshot — all from the TUI, locally or over SSH. `zfs send`/`receive` replication is implemented and tested at the library layer (`zfs::snapshot::replicate_snapshot`) but is **not yet wired into the UI** — it needs its own design pass for progress display and cancellation, since a single replication can run for hours. A dedicated follow-up plan will wire it up.
```

- [ ] **Step 2: Manually verify on a real FreeBSD host with an existing zpool**

Checklist (run as root on FreeBSD, ideally against a scratch pool/dataset you don't mind losing):

1. `cargo build --release` succeeds, `cargo test` passes all 137 tests.
2. Selecting a pool and pressing `Enter` opens the vdev drill-down; `Esc`/`Backspace` returns to the Pools list.
3. In the vdev view, pressing `o`/`O`/`x` on a mirror/raidz group row (not a leaf disk) shows the "select a disk" status message and opens no modal; on a leaf disk it opens the expected confirm/form modal.
4. `s` on a selected pool opens a y/n Scrub confirmation; `y` runs `zpool scrub` and the Pools table refreshes.
5. `d` on a selected pool opens a type-to-confirm Destroy dialog; typing the wrong name and pressing Enter does nothing (dialog stays open); typing the exact name and pressing Enter destroys the pool and the Pools table refreshes with it gone.
6. `n` on Pools opens a two-field Create Pool form; `Tab` moves between fields; submitting with a valid name and vdev spec (e.g. a spare disk available on the test host) creates the pool and it appears in the refreshed Pools table.
7. Offlining a disk (`o`, confirm) shows it offline in both the Vdevs view and the Pools table's health column (the dual-refresh behavior) after `zpool status`/`zpool list` reflect the change.
8. `n`/`d` on Datasets create/destroy a dataset as expected, each refreshing the Datasets table.
9. `s` on a selected dataset opens a one-field Create Snapshot form; submitting creates `dataset@name` and it appears in the refreshed Snapshots table.
10. `d`/`b` on Snapshots destroy/roll back as expected (type-to-confirm `dataset@name`), each refreshing the Snapshots table.
11. If a remote host is configured, repeat a representative subset of the above (e.g. scrub + create/destroy dataset) against it over SSH.
12. `q` cleanly exits back to the normal terminal from every view, including from inside a modal and from the vdev drill-down (press `Esc` first, then `q`).

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: update keybindings and status for mutation UI wiring"
```
