# Detail Pane + Pool/Dataset Property Editing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a right-hand detail pane, shown only for the Pools and Datasets sections, that displays and lets you edit `zpool`/`zfs` properties for whichever row is selected in the middle pane.

**Architecture:** A new `PaneFocus` enum (`Middle` | `Right`) tracks which pane receives navigation keys, meaningful only while the current section is Pools or Datasets. `Left`/`Right` toggle focus; `Tab`/`Shift+Tab` become the sole section-cycling keys (previously shared with `Left`/`Right`). Properties are fetched on entering `Right` focus (not on every middle-pane scroll) via the same worker-thread + stale-identity-discard pattern the Vdevs drill-down already uses. Editing reuses the existing `Modal::Form`/`FormKind`/`MutationCall` machinery from the mutation-UI plan — two new variants, no new plumbing.

**Tech Stack:** Rust (2021 edition), ratatui 0.29, crossterm 0.28 — no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-06-detail-pane-properties-design.md` (and the two specs it builds on: `2026-09-04-zfsctl-tui-design.md`, `2026-09-05-zfsctl-mutation-ui-design.md`).

## Global Constraints

- FreeBSD only, CLI shelling via machine-readable flags, no `unsafe` — unchanged from prior plans.
- `CommandRunner` is the sole execution abstraction; the two new `MutationCall` variants (`SetPoolProperty`/`SetDatasetProperty`) must delegate to the new library functions, never a raw `runner.run()` call.
- The right pane is shown **only** for the Pools and Datasets sections — Hosts, Snapshots, and the Vdevs drill-down are unchanged, no pane, no new state, no new library code for them.
- No sub-tabs in the right pane — only one view (Properties) exists per section in this plan. A follow-up plan introduces sub-tab cycling when a second view (Permissions/ACLs) exists; do not build unused sub-tab plumbing now.
- `Left`/`Right` stop cycling sidebar sections — that becomes `Tab`/`Shift+Tab`'s sole job. `Left`/`Right` become the pane-focus toggle for Pools/Datasets, and a no-op everywhere else.
- Property fetches are tagged by **both host and the pool/dataset name they were fetched for**, and discarded on a mismatch — the same stale-identity discard pattern the Vdevs drill-down uses (`WorkerMsg::Vdevs`'s `String` pool-name field is the precedent to follow exactly).
- No ratatui rendering/terminal tests — all logic unit-tested against `App` via `handle_key`/`handle_modal_key`/`FakeRunner`, consistent with every prior plan.
- Design decision made in this plan, not fully specified in the design doc: since the right pane always renders for Pools/Datasets regardless of focus, but properties are fetched lazily (only on entering `Right` focus), the middle-pane row-navigation arms (`Up`/`Down` while `PaneFocus::Middle`) clear `pool_properties`/`dataset_properties` (and their `TableState` selections) whenever the selected row changes. This guarantees the right pane never displays a *different* row's properties — it shows nothing (an empty table) until the user presses `Right` to actually look at the newly-selected row's properties. The alternative (leaving stale data visible while scrolling) would violate this codebase's established "never show stale/wrong-identity data" convention (`clear_host_data`, the Vdevs stale-pool discard, Plan 2's final review).

---

## File Structure

```
src/
  zfs/
    property.rs     # Create: Property, PropertySource, parse_property_list
    mod.rs           # Modify: pub mod property;
    pool.rs           # Modify: get_pool_properties, set_pool_property
    dataset.rs         # Modify: get_dataset_properties, set_dataset_property
  ui/
    modal.rs         # Modify: MutationCall::SetPoolProperty/SetDatasetProperty, FormKind::EditPoolProperty/EditDatasetProperty
    state.rs          # Modify: PaneFocus enum; App fields; handle_key overhaul (Left/Right/Tab/Shift+Tab, Esc, Enter, Up/Down); WorkerMsg + refresh_*/drain_worker_messages; build_pending_action arms
    layout.rs          # Modify: dynamic 2-pane/3-pane split; draw_pool_properties/draw_dataset_properties
    mod.rs            # Modify: event_loop wiring for Right-focus-entry fetch and 'r' while right-focused
README.md            # Modify: keybindings section
```

---

### Task 1: `Property`/`PropertySource` domain model and parsing

**Files:**
- Create: `src/zfs/property.rs`
- Modify: `src/zfs/mod.rs`

**Interfaces:**
- Consumes: `AppError`, `Result` from `src/error.rs`.
- Produces: `pub struct Property { pub name: String, pub value: String, pub source: PropertySource }` (Debug, Clone, PartialEq), `pub enum PropertySource { Local, Default, Inherited(String), Other(String) }` (Debug, Clone, PartialEq, Eq), `pub fn parse_property_list(output: &str) -> Result<Vec<Property>>`.

Both `zpool get all -H -p <pool>` and `zfs get all -H -p <dataset>` emit the identical 4-column tab-separated shape (`name`, `property`, `value`, `source`) — this module parses that shape once, shared by both `pool.rs` and `dataset.rs` in later tasks. The leading `name` column (the pool/dataset name itself, repeated on every line) is intentionally ignored — both call sites always fetch properties for one already-known pool/dataset at a time.

- [ ] **Step 1: Write `src/zfs/property.rs` with its fixture-based tests**

```rust
use crate::error::{AppError, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub name: String,
    pub value: String,
    pub source: PropertySource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertySource {
    Local,
    Default,
    /// The ancestor dataset this property is inherited from (only
    /// meaningful for `zfs` properties — `zpool` properties never
    /// report this source).
    Inherited(String),
    /// Any other source string `zpool`/`zfs get` reports verbatim —
    /// most commonly `-` (read-only/computed properties like `used`
    /// or `creation`), but also `temporary` and `received`.
    Other(String),
}

impl PropertySource {
    fn parse(s: &str) -> PropertySource {
        if let Some(dataset) = s.strip_prefix("inherited from ") {
            return PropertySource::Inherited(dataset.to_string());
        }
        match s {
            "local" => PropertySource::Local,
            "default" => PropertySource::Default,
            other => PropertySource::Other(other.to_string()),
        }
    }
}

/// Parses `zpool get all -H -p <pool>` or `zfs get all -H -p <dataset>`
/// output — both emit the identical tab-separated
/// `name  property  value  source` shape, one property per line. The
/// leading `name` column is ignored (both call sites already know which
/// pool/dataset they asked about).
pub fn parse_property_list(output: &str) -> Result<Vec<Property>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != 4 {
                return Err(AppError::Parse(format!(
                    "expected 4 fields in get-all property line: {line:?}"
                )));
            }
            Ok(Property {
                name: fields[1].to_string(),
                value: fields[2].to_string(),
                source: PropertySource::parse(fields[3]),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZPOOL_GET_FIXTURE: &str = "\
tank\tsize\t4000000000000\t-
tank\tcapacity\t23\t-
tank\tautoexpand\toff\tdefault
tank\tcomment\t-\tdefault
tank\tautoreplace\ton\tlocal
";

    const ZFS_GET_FIXTURE: &str = "\
tank/data\ttype\tfilesystem\t-
tank/data\tused\t1234567\t-
tank/data\tcompression\tlz4\tlocal
tank/data\tquota\t0\tdefault
tank/data\treadonly\toff\tinherited from tank
";

    #[test]
    fn parses_zpool_get_fixture() {
        let props = parse_property_list(ZPOOL_GET_FIXTURE).unwrap();
        assert_eq!(
            props,
            vec![
                Property { name: "size".to_string(), value: "4000000000000".to_string(), source: PropertySource::Other("-".to_string()) },
                Property { name: "capacity".to_string(), value: "23".to_string(), source: PropertySource::Other("-".to_string()) },
                Property { name: "autoexpand".to_string(), value: "off".to_string(), source: PropertySource::Default },
                Property { name: "comment".to_string(), value: "-".to_string(), source: PropertySource::Default },
                Property { name: "autoreplace".to_string(), value: "on".to_string(), source: PropertySource::Local },
            ]
        );
    }

    #[test]
    fn parses_zfs_get_fixture_including_inherited_source() {
        let props = parse_property_list(ZFS_GET_FIXTURE).unwrap();
        assert_eq!(
            props,
            vec![
                Property { name: "type".to_string(), value: "filesystem".to_string(), source: PropertySource::Other("-".to_string()) },
                Property { name: "used".to_string(), value: "1234567".to_string(), source: PropertySource::Other("-".to_string()) },
                Property { name: "compression".to_string(), value: "lz4".to_string(), source: PropertySource::Local },
                Property { name: "quota".to_string(), value: "0".to_string(), source: PropertySource::Default },
                Property { name: "readonly".to_string(), value: "off".to_string(), source: PropertySource::Inherited("tank".to_string()) },
            ]
        );
    }

    #[test]
    fn rejects_malformed_property_line() {
        let result = parse_property_list("tank\tonly-two-fields\n");
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Declare the module in `src/zfs/mod.rs`**

Add `pub mod property;` after the existing `pub mod dataset;` line:

```rust
pub mod dataset;
pub mod pool;
pub mod property;
pub mod snapshot;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test zfs::property`
Expected: PASS (3 tests)

- [ ] **Step 4: Commit**

```bash
git add src/zfs/property.rs src/zfs/mod.rs
git commit -m "feat: Property/PropertySource domain model and shared parser"
```

---

### Task 2: Pool property get/set

**Files:**
- Modify: `src/zfs/pool.rs`

**Interfaces:**
- Consumes: `Property`, `parse_property_list` from `src/zfs/property.rs` (Task 1); `run_checked` from `src/zfs/mod.rs`.
- Produces: `pub fn get_pool_properties(runner: &dyn CommandRunner, pool: &str) -> Result<Vec<Property>>`, `pub fn set_pool_property(runner: &dyn CommandRunner, pool: &str, key: &str, value: &str) -> Result<()>`.

- [ ] **Step 1: Write failing tests**

Add to `src/zfs/pool.rs`'s test module:

```rust
const ZPOOL_GET_PROPERTIES_FIXTURE: &str = "\
tank\tautoexpand\toff\tdefault
tank\tcomment\tmy pool\tlocal
";

#[test]
fn get_pool_properties_uses_correct_command_and_parses_result() {
    let runner = FakeRunner::new(ZPOOL_GET_PROPERTIES_FIXTURE);
    let props = get_pool_properties(&runner, "tank").unwrap();
    assert_eq!(props.len(), 2);
    assert_eq!(props[0].name, "autoexpand");
    let calls = runner.calls.lock().unwrap();
    assert_eq!(
        calls[0],
        ("zpool".to_string(), vec!["get".to_string(), "all".to_string(), "-H".to_string(), "-p".to_string(), "--".to_string(), "tank".to_string()])
    );
}

#[test]
fn get_pool_properties_surfaces_command_failure() {
    let runner = FakeRunner::failing("no such pool", 1);
    let result = get_pool_properties(&runner, "tank");
    assert!(result.is_err());
}

#[test]
fn set_pool_property_runs_expected_command() {
    let runner = FakeRunner::new("");
    set_pool_property(&runner, "tank", "comment", "my pool").unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(
        calls[0],
        ("zpool".to_string(), vec!["set".to_string(), "comment=my pool".to_string(), "--".to_string(), "tank".to_string()])
    );
}

#[test]
fn set_pool_property_surfaces_command_failure() {
    let runner = FakeRunner::failing("invalid property", 1);
    let result = set_pool_property(&runner, "tank", "bogus", "x");
    assert!(result.is_err());
}

#[test]
fn guards_against_pool_names_that_look_like_flags_in_get_properties() {
    let runner = FakeRunner::new("");
    get_pool_properties(&runner, "-f").unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].1[4], "--", "a leading '--' must precede the pool name so zpool cannot parse it as an option");
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test get_pool_properties set_pool_property`
Expected: FAIL (functions don't exist yet)

- [ ] **Step 3: Add `get_pool_properties`/`set_pool_property`**

Add `use crate::zfs::property::Property;` to the imports at the top of `src/zfs/pool.rs`. Add the two functions after `replace_disk`:

```rust
/// Runs `zpool get all -H -p <pool>` and parses every property into a
/// flat list. There is no `-o` column-selection equivalent that would
/// narrow this down usefully — `all` is the standard way to fetch every
/// property at once, same as `zfs get all` in `dataset.rs`.
pub fn get_pool_properties(runner: &dyn CommandRunner, pool: &str) -> Result<Vec<Property>> {
    let output = super::run_checked(
        runner,
        "zpool",
        &["get", "all", "-H", "-p", "--", pool],
        &format!("zpool get all {pool}"),
    )?;
    crate::zfs::property::parse_property_list(&output.stdout)
}

pub fn set_pool_property(runner: &dyn CommandRunner, pool: &str, key: &str, value: &str) -> Result<()> {
    super::run_checked(
        runner,
        "zpool",
        &["set", &format!("{key}={value}"), "--", pool],
        &format!("zpool set {key}={value} {pool}"),
    )?;
    Ok(())
}
```

- [ ] **Step 4: Run the pool tests**

Run: `cargo test zfs::pool`
Expected: PASS (all existing tests plus the 5 new ones)

- [ ] **Step 5: Commit**

```bash
git add src/zfs/pool.rs
git commit -m "feat: pool property get/set"
```

---

### Task 3: Dataset property get/set

**Files:**
- Modify: `src/zfs/dataset.rs`

**Interfaces:**
- Consumes: `Property`, `parse_property_list` from `src/zfs/property.rs` (Task 1); `run_checked` from `src/zfs/mod.rs`.
- Produces: `pub fn get_dataset_properties(runner: &dyn CommandRunner, dataset: &str) -> Result<Vec<Property>>`, `pub fn set_dataset_property(runner: &dyn CommandRunner, dataset: &str, key: &str, value: &str) -> Result<()>`.

- [ ] **Step 1: Write failing tests**

Add to `src/zfs/dataset.rs`'s test module:

```rust
const ZFS_GET_PROPERTIES_FIXTURE: &str = "\
tank/data\tcompression\tlz4\tlocal
tank/data\tquota\t0\tdefault
";

#[test]
fn get_dataset_properties_uses_correct_command_and_parses_result() {
    let runner = FakeRunner::new(ZFS_GET_PROPERTIES_FIXTURE);
    let props = get_dataset_properties(&runner, "tank/data").unwrap();
    assert_eq!(props.len(), 2);
    assert_eq!(props[0].name, "compression");
    let calls = runner.calls.lock().unwrap();
    assert_eq!(
        calls[0],
        ("zfs".to_string(), vec!["get".to_string(), "all".to_string(), "-H".to_string(), "-p".to_string(), "--".to_string(), "tank/data".to_string()])
    );
}

#[test]
fn get_dataset_properties_surfaces_command_failure() {
    let runner = FakeRunner::failing("dataset does not exist", 1);
    let result = get_dataset_properties(&runner, "tank/data");
    assert!(result.is_err());
}

#[test]
fn set_dataset_property_runs_expected_command() {
    let runner = FakeRunner::new("");
    set_dataset_property(&runner, "tank/data", "compression", "zstd").unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(
        calls[0],
        ("zfs".to_string(), vec!["set".to_string(), "compression=zstd".to_string(), "--".to_string(), "tank/data".to_string()])
    );
}

#[test]
fn set_dataset_property_surfaces_command_failure() {
    let runner = FakeRunner::failing("invalid property", 1);
    let result = set_dataset_property(&runner, "tank/data", "bogus", "x");
    assert!(result.is_err());
}

#[test]
fn guards_against_dataset_names_that_look_like_flags_in_get_properties() {
    let runner = FakeRunner::new("");
    get_dataset_properties(&runner, "-r").unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].1[4], "--", "a leading '--' must precede the dataset name so zfs cannot parse it as an option");
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test get_dataset_properties set_dataset_property`
Expected: FAIL (functions don't exist yet)

- [ ] **Step 3: Add `get_dataset_properties`/`set_dataset_property`**

Add `use crate::zfs::property::Property;` to the imports at the top of `src/zfs/dataset.rs`. Add the two functions after `destroy_dataset`:

```rust
/// Runs `zfs get all -H -p <dataset>` and parses every property into a
/// flat list.
pub fn get_dataset_properties(runner: &dyn CommandRunner, dataset: &str) -> Result<Vec<Property>> {
    let output = super::run_checked(
        runner,
        "zfs",
        &["get", "all", "-H", "-p", "--", dataset],
        &format!("zfs get all {dataset}"),
    )?;
    crate::zfs::property::parse_property_list(&output.stdout)
}

pub fn set_dataset_property(runner: &dyn CommandRunner, dataset: &str, key: &str, value: &str) -> Result<()> {
    super::run_checked(
        runner,
        "zfs",
        &["set", &format!("{key}={value}"), "--", dataset],
        &format!("zfs set {key}={value} {dataset}"),
    )?;
    Ok(())
}
```

- [ ] **Step 4: Run the dataset tests**

Run: `cargo test zfs::dataset`
Expected: PASS (all existing tests plus the 5 new ones)

- [ ] **Step 5: Commit**

```bash
git add src/zfs/dataset.rs
git commit -m "feat: dataset property get/set"
```

---

### Task 4: `PaneFocus` state and keybinding overhaul

**Files:**
- Modify: `src/ui/state.rs`

**Interfaces:**
- Consumes: `Property` from `src/zfs/property.rs` (Task 1).
- Produces: `pub enum PaneFocus { Middle, Right }` (Debug, Clone, Copy, PartialEq, Eq). `App` gains `pub pane_focus: PaneFocus`, `pub pool_properties: Vec<Property>`, `pub pool_properties_table: TableState`, `pub dataset_properties: Vec<Property>`, `pub dataset_properties_table: TableState`. `App::selected_pool_name(&self) -> Option<String>`, `App::selected_dataset_name(&self) -> Option<String>`.

This task builds the focus-toggle and row-navigation mechanics only — no property fetching yet (Task 5), no editing yet (Task 7). `pool_properties`/`dataset_properties` start empty and stay empty until Task 5's refresh wiring exists; this task's tests only exercise the state transitions (focus toggling, clearing on row change, hygiene resets).

This task **replaces `Left`/`Right`'s existing meaning** (cycling sidebar sections) with pane-focus-toggle, and makes `Tab`/`Shift+Tab` (delivered by crossterm as `KeyCode::BackTab`) the sole section-cycling keys — closing a gap the mutation-UI plan's final review flagged (`Shift+Tab` was unimplemented).

- [ ] **Step 1: Write failing tests**

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn right_arrow_on_pools_enters_right_focus_and_clears_properties() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // Hosts -> Pools
    app.pool_properties = vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }];
    app.pool_properties_table.select(Some(0));
    assert_eq!(app.pane_focus, PaneFocus::Middle);
    app.handle_key(KeyCode::Right);
    assert_eq!(app.pane_focus, PaneFocus::Right);
    assert!(app.pool_properties.is_empty(), "entering right focus clears stale properties until Task 5's fetch repopulates them");
    assert_eq!(app.pool_properties_table.selected(), None);
}

#[test]
fn right_arrow_on_hosts_is_a_noop() {
    let mut app = App::new(vec!["local".to_string()]);
    assert_eq!(app.current_section(), Section::Hosts);
    app.handle_key(KeyCode::Right);
    assert_eq!(app.pane_focus, PaneFocus::Middle);
}

#[test]
fn left_arrow_returns_focus_to_middle() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::Right);
    assert_eq!(app.pane_focus, PaneFocus::Right);
    app.handle_key(KeyCode::Left);
    assert_eq!(app.pane_focus, PaneFocus::Middle);
}

#[test]
fn esc_returns_focus_to_middle_when_right_focused_without_clearing_status_message() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Right);
    app.status_message = Some("keep me".to_string());
    app.handle_key(KeyCode::Esc);
    assert_eq!(app.pane_focus, PaneFocus::Middle);
    assert_eq!(app.status_message, Some("keep me".to_string()), "backing out of right-focus takes priority over the generic Esc-clears-status-message arm, same precedence as the vdev drill-down");
}

#[test]
fn esc_still_clears_status_message_when_middle_focused() {
    let mut app = App::new(vec!["local".to_string()]);
    app.status_message = Some("stale".to_string());
    app.handle_key(KeyCode::Esc);
    assert_eq!(app.status_message, None);
}

#[test]
fn tab_cycles_sections_and_resets_pane_focus() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::Right);
    assert_eq!(app.pane_focus, PaneFocus::Right);
    app.handle_key(KeyCode::Tab); // -> Datasets
    assert_eq!(app.current_section(), Section::Datasets);
    assert_eq!(app.pane_focus, PaneFocus::Middle, "switching sections resets focus back to the middle pane");
}

#[test]
fn backtab_cycles_sections_backward() {
    let mut app = App::new(vec!["local".to_string()]);
    assert_eq!(app.current_section(), Section::Hosts);
    app.handle_key(KeyCode::BackTab);
    assert_eq!(app.current_section(), Section::Snapshots, "must wrap backward from the first section to the last");
}

#[test]
fn up_down_on_pools_middle_focus_clears_properties_on_row_change() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.pools = vec![
        Pool { name: "a".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
        Pool { name: "b".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
    ];
    app.pool_properties = vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }];
    app.pool_properties_table.select(Some(0));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.pools_table.selected(), Some(0));
    assert!(app.pool_properties.is_empty(), "moving to a different pool row must not leave the previous row's properties visible");
    assert_eq!(app.pool_properties_table.selected(), None);
}

#[test]
fn up_down_on_pools_right_focus_moves_properties_table_not_pools_table() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.pools = vec![Pool { name: "a".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.pool_properties = vec![
        Property { name: "p1".to_string(), value: "v1".to_string(), source: PropertySource::Local },
        Property { name: "p2".to_string(), value: "v2".to_string(), source: PropertySource::Local },
    ];
    app.handle_key(KeyCode::Down);
    assert_eq!(app.pool_properties_table.selected(), Some(0));
    assert_eq!(app.pools_table.selected(), Some(0), "pools_table selection must be untouched while right-focused");
}

#[test]
fn up_down_on_datasets_middle_focus_clears_properties_on_row_change() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![
        Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
        Dataset { name: "b".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
    ];
    app.dataset_properties = vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }];
    app.dataset_properties_table.select(Some(0));
    app.handle_key(KeyCode::Down);
    assert!(app.dataset_properties.is_empty());
    assert_eq!(app.dataset_properties_table.selected(), None);
}

#[test]
fn up_down_on_datasets_right_focus_moves_properties_table() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.dataset_properties = vec![Property { name: "p1".to_string(), value: "v1".to_string(), source: PropertySource::Local }];
    app.handle_key(KeyCode::Down);
    assert_eq!(app.dataset_properties_table.selected(), Some(0));
}

#[test]
fn switching_host_resets_pane_focus_and_clears_properties() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::Right);
    app.pool_properties = vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }];
    app.handle_key(KeyCode::Tab); // -> Datasets, resets pane_focus to Middle first
    app.handle_key(KeyCode::BackTab); // -> Pools again
    app.handle_key(KeyCode::BackTab); // -> Hosts
    app.handle_key(KeyCode::Down); // switch host, triggers clear_host_data
    assert_eq!(app.pane_focus, PaneFocus::Middle);
    assert!(app.pool_properties.is_empty());
    assert_eq!(app.pool_properties_table.selected(), None);
    assert!(app.dataset_properties.is_empty());
    assert_eq!(app.dataset_properties_table.selected(), None);
}

#[test]
fn selected_pool_name_and_selected_dataset_name_helpers() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    assert_eq!(app.selected_pool_name(), Some("tank".to_string()));
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    assert_eq!(app.selected_dataset_name(), Some("tank/data".to_string()));
    assert_eq!(App::new(vec!["local".to_string()]).selected_pool_name(), None);
}
```

Also add `use crate::zfs::property::{Property, PropertySource};` to the test module's imports (or the top-level imports, since `Property`/`PropertySource` will be used by non-test code in Task 5 too — add it to the file's top-level `use` block now).

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test right_arrow_on_pools_enters_right_focus`
Expected: FAIL (`PaneFocus` doesn't exist yet)

- [ ] **Step 3: Add `PaneFocus`, the new `App` fields, and the two helper methods**

Add `use crate::zfs::property::Property;` to the top-level imports (the test module also needs `PropertySource` — add both there via `use super::*;` as already established, plus a direct `use crate::zfs::property::PropertySource;` inside the test module since `PropertySource` isn't used by non-test code in this task).

Add near the top of the file, after the `Section` enum:

```rust
/// Which pane currently receives navigation keys. Meaningful only while
/// the current section is Pools or Datasets — those are the only two
/// sections with a right pane at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneFocus {
    Middle,
    Right,
}
```

Add four fields to the `App` struct, after `drilled_pool`:

```rust
    pub pane_focus: PaneFocus,
    pub pool_properties: Vec<Property>,
    pub pool_properties_table: TableState,
    pub dataset_properties: Vec<Property>,
    pub dataset_properties_table: TableState,
```

Initialize them in `App::new`, after `drilled_pool: None,`:

```rust
            pane_focus: PaneFocus::Middle,
            pool_properties: Vec::new(),
            pool_properties_table: TableState::default(),
            dataset_properties: Vec::new(),
            dataset_properties_table: TableState::default(),
```

Add two helper methods after `active_host_name`:

```rust
    /// The name of the pool currently selected in the middle pane, if
    /// any — used to know which pool the right pane's properties belong
    /// to. Valid regardless of `pane_focus`: the middle-pane selection
    /// doesn't change while the right pane has focus.
    pub fn selected_pool_name(&self) -> Option<String> {
        self.pools_table.selected().and_then(|i| self.pools.get(i)).map(|p| p.name.clone())
    }

    /// See `selected_pool_name`.
    pub fn selected_dataset_name(&self) -> Option<String> {
        self.datasets_table.selected().and_then(|i| self.datasets.get(i)).map(|d| d.name.clone())
    }
```

- [ ] **Step 4: Update `clear_host_data`**

Replace it:

```rust
    /// Clears data panes so a stale render from the previously-selected
    /// host is never left on screen looking current after switching hosts.
    fn clear_host_data(&mut self) {
        self.pools.clear();
        self.datasets.clear();
        self.snapshots.clear();
        self.pools_table.select(None);
        self.datasets_table.select(None);
        self.snapshots_table.select(None);
        self.pane_focus = PaneFocus::Middle;
        self.pool_properties.clear();
        self.pool_properties_table.select(None);
        self.dataset_properties.clear();
        self.dataset_properties_table.select(None);
    }
```

- [ ] **Step 5: Replace `handle_key` with the full new version**

This is a wholesale replacement of the function body — `Left`/`Right`'s meaning changes (no longer section-cycling), `Tab` drops `Right` from its pattern and gains a `pane_focus` reset, and `BackTab` is new:

```rust
    pub fn handle_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc | KeyCode::Backspace if self.drilled_pool.is_some() => {
                self.drilled_pool = None;
                self.vdevs.clear();
                self.vdevs_table.select(None);
            }
            KeyCode::Esc if self.pane_focus == PaneFocus::Right => {
                self.pane_focus = PaneFocus::Middle;
            }
            KeyCode::Esc => self.status_message = None,
            KeyCode::Enter if self.current_section() == Section::Pools && self.drilled_pool.is_none() && self.pane_focus == PaneFocus::Middle => {
                if let Some(i) = self.pools_table.selected() {
                    if let Some(pool) = self.pools.get(i) {
                        self.drilled_pool = Some(pool.name.clone());
                        self.vdevs.clear();
                        self.vdevs_table.select(None);
                    }
                }
            }
            KeyCode::Tab if self.drilled_pool.is_none() => {
                self.selected_section = (self.selected_section + 1) % self.sections.len();
                self.pane_focus = PaneFocus::Middle;
            }
            KeyCode::BackTab if self.drilled_pool.is_none() => {
                self.selected_section = if self.selected_section == 0 {
                    self.sections.len() - 1
                } else {
                    self.selected_section - 1
                };
                self.pane_focus = PaneFocus::Middle;
            }
            KeyCode::Right
                if (self.current_section() == Section::Pools || self.current_section() == Section::Datasets)
                    && self.drilled_pool.is_none()
                    && self.pane_focus == PaneFocus::Middle =>
            {
                self.pane_focus = PaneFocus::Right;
                match self.current_section() {
                    Section::Pools => {
                        self.pool_properties.clear();
                        self.pool_properties_table.select(None);
                    }
                    Section::Datasets => {
                        self.dataset_properties.clear();
                        self.dataset_properties_table.select(None);
                    }
                    _ => {}
                }
            }
            KeyCode::Left if self.pane_focus == PaneFocus::Right => {
                self.pane_focus = PaneFocus::Middle;
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
            KeyCode::Char('o') if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                self.open_vdev_confirm(false);
            }
            KeyCode::Char('O') if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                self.open_vdev_confirm(true);
            }
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
            KeyCode::Down if self.current_section() == Section::Pools && self.pane_focus == PaneFocus::Right => {
                move_table_selection(&mut self.pool_properties_table, self.pool_properties.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Pools && self.pane_focus == PaneFocus::Right => {
                move_table_selection(&mut self.pool_properties_table, self.pool_properties.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::Pools => {
                move_table_selection(&mut self.pools_table, self.pools.len(), true);
                self.pool_properties.clear();
                self.pool_properties_table.select(None);
            }
            KeyCode::Up if self.current_section() == Section::Pools => {
                move_table_selection(&mut self.pools_table, self.pools.len(), false);
                self.pool_properties.clear();
                self.pool_properties_table.select(None);
            }
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
            KeyCode::Down if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => {
                move_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => {
                move_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), true);
                self.dataset_properties.clear();
                self.dataset_properties_table.select(None);
            }
            KeyCode::Up if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), false);
                self.dataset_properties.clear();
                self.dataset_properties_table.select(None);
            }
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
            KeyCode::Down if self.current_section() == Section::Snapshots => {
                move_table_selection(&mut self.snapshots_table, self.snapshots.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Snapshots => {
                move_table_selection(&mut self.snapshots_table, self.snapshots.len(), false);
            }
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
            _ => {}
        }
    }
```

- [ ] **Step 6: Run the state tests**

Run: `cargo test ui::state`
Expected: many existing tests FAIL at this point (see below) — that's expected, not a regression; fix them per the mechanical rule below, then re-run to confirm PASS.

Before this task, `Right`/`Left` had exactly one meaning in `handle_key`: cycle sections forward/backward (the `KeyCode::Right | KeyCode::Tab` and `KeyCode::Left` arms this task just removed). Every pre-existing test in this file that calls `app.handle_key(KeyCode::Right)` or `app.handle_key(KeyCode::Left)` was therefore using it to navigate between sections — confirmed by grepping the file: every occurrence is exactly `app.handle_key(KeyCode::Right)` or `app.handle_key(KeyCode::Left)`, no other usage pattern. This gives a safe, mechanical rule:

**In every test function that already existed in this file before this task's Step 1 (i.e., every test *except* the 13 new ones this task's Step 1 just added — `right_arrow_on_pools_enters_right_focus_and_clears_properties`, `right_arrow_on_hosts_is_a_noop`, `left_arrow_returns_focus_to_middle`, `esc_returns_focus_to_middle_when_right_focused_without_clearing_status_message`, `esc_still_clears_status_message_when_middle_focused`, `tab_cycles_sections_and_resets_pane_focus`, `backtab_cycles_sections_backward`, `up_down_on_pools_middle_focus_clears_properties_on_row_change`, `up_down_on_pools_right_focus_moves_properties_table_not_pools_table`, `up_down_on_datasets_middle_focus_clears_properties_on_row_change`, `up_down_on_datasets_right_focus_moves_properties_table`, `switching_host_resets_pane_focus_and_clears_properties`, `selected_pool_name_and_selected_dataset_name_helpers`, which correctly use `Right`/`Left` for the *new* pane-focus meaning and must NOT be touched — replace every `app.handle_key(KeyCode::Right)` with `app.handle_key(KeyCode::Tab)`, and every `app.handle_key(KeyCode::Left)` with `app.handle_key(KeyCode::BackTab)`.**

For example, `handle_key_navigates_sections_and_wraps` becomes:

```rust
    #[test]
    fn handle_key_navigates_sections_and_wraps() {
        let mut app = App::new(vec!["local".to_string()]);
        assert_eq!(app.current_section(), Section::Hosts);
        app.handle_key(KeyCode::Tab);
        assert_eq!(app.current_section(), Section::Pools);
        app.handle_key(KeyCode::BackTab);
        assert_eq!(app.current_section(), Section::Hosts);
        app.handle_key(KeyCode::BackTab);
        assert_eq!(app.current_section(), Section::Snapshots);
    }
```

...and a test that previously did `app.handle_key(KeyCode::Right); app.handle_key(KeyCode::Right);` to reach Datasets becomes `app.handle_key(KeyCode::Tab); app.handle_key(KeyCode::Tab);`. Apply this identically to every other pre-existing occurrence (there are 40 `app.handle_key(KeyCode::Right)`/`app.handle_key(KeyCode::Left)` call sites across the pre-existing tests as of this task) — no test needs any change beyond this mechanical substitution, since `Tab`/`BackTab` compute `selected_section` identically to how the old `Right`/`Left` arms did.

- [ ] **Step 7: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, 165 tests (152 from Task 3 + 13 new from this task); release build clean aside from expected dead_code warnings (`pool_properties`/`dataset_properties`/`selected_pool_name`/`selected_dataset_name` are exercised by this task's own tests, so no new dead_code warnings from this task specifically)

- [ ] **Step 8: Commit**

```bash
git add src/ui/state.rs
git commit -m "feat: PaneFocus state and keybinding overhaul (Left/Right/Tab/Shift+Tab)"
```

---

### Task 5: Property fetch/refresh wiring

**Files:**
- Modify: `src/ui/state.rs`

**Interfaces:**
- Consumes: `Property`, `get_pool_properties`/`get_dataset_properties` from Tasks 1-3; `PaneFocus`, `selected_pool_name`/`selected_dataset_name` from Task 4.
- Produces: `WorkerMsg` gains `PoolProperties(usize, String, Result<Vec<Property>>)` and `DatasetProperties(usize, String, Result<Vec<Property>>)`. `App::refresh_pool_properties(&mut self, runner: Arc<dyn CommandRunner>, pool: String)`, `App::refresh_dataset_properties(&mut self, runner: Arc<dyn CommandRunner>, dataset: String)`. `Section` gains `PoolProperties`/`DatasetProperties` variants (never added to `App::sections`, exactly like `Vdevs` — used only to tag in-flight/pending-refresh keys and the `Vec<Section>` `drain_worker_messages` returns).

This follows the exact `refresh_vdevs`/`WorkerMsg::Vdevs` pattern from the mutation-UI plan: in-flight tracking keyed by `(Section::PoolProperties, host)` (not pool-aware, same simplifying tradeoff `refresh_vdevs` already makes — correctness is fully handled by the stale-identity discard, not by a pool-aware in-flight key), a `String` identity tag on the `WorkerMsg` variant, and a stale-identity discard in `drain_worker_messages` that compares the tag against `self.selected_pool_name()`/`self.selected_dataset_name()` (not a stored snapshot — the middle-pane selection is always current, since Task 4 guarantees it doesn't change while `PaneFocus::Right`).

- [ ] **Step 1: Write failing tests**

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn refresh_pool_properties_populates_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank\tautoexpand\toff\tdefault\n"));
    app.refresh_pool_properties(runner, "tank".to_string());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.pool_properties.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.pool_properties.len(), 1);
    assert_eq!(app.pool_properties[0].name, "autoexpand");
}

#[test]
fn pool_properties_result_for_a_different_pool_than_selected_is_discarded() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "scratch".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0)); // selected pool is "scratch"
    app.tx
        .send(WorkerMsg::PoolProperties(0, "tank".to_string(), Ok(vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }])))
        .unwrap();
    app.drain_worker_messages();
    assert!(app.pool_properties.is_empty(), "a result for a pool that's no longer selected must not overwrite the pane");
    assert_eq!(app.status_message, None);
}

#[test]
fn refresh_dataset_properties_populates_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank/data\tcompression\tlz4\tlocal\n"));
    app.refresh_dataset_properties(runner, "tank/data".to_string());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.dataset_properties.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.dataset_properties.len(), 1);
    assert_eq!(app.dataset_properties[0].name, "compression");
}

#[test]
fn dataset_properties_result_for_a_different_dataset_than_selected_is_discarded() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/other".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.tx
        .send(WorkerMsg::DatasetProperties(0, "tank/data".to_string(), Ok(vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }])))
        .unwrap();
    app.drain_worker_messages();
    assert!(app.dataset_properties.is_empty());
}

#[test]
fn refresh_pool_properties_does_not_spawn_second_thread_while_in_flight() {
    let mut app = App::new(vec!["local".to_string()]);
    let fake = Arc::new(FakeRunner::new("tank\tautoexpand\toff\tdefault\n"));
    let runner: Arc<dyn CommandRunner> = fake.clone();
    app.refresh_pool_properties(runner.clone(), "tank".to_string());
    app.refresh_pool_properties(runner.clone(), "tank".to_string());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.pool_properties.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test refresh_pool_properties_populates_from_fake_runner`
Expected: FAIL (`refresh_pool_properties`/`WorkerMsg::PoolProperties` don't exist yet)

- [ ] **Step 3: Add `Section::PoolProperties`/`DatasetProperties` and the two `WorkerMsg` variants**

Change the `Section` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Hosts,
    Pools,
    Datasets,
    Snapshots,
    Vdevs,
    PoolProperties,
    DatasetProperties,
}
```

`App::new`'s `sections` field stays exactly as it is — `PoolProperties`/`DatasetProperties`, like `Vdevs`, are never added to it.

Add two variants to `WorkerMsg`:

```rust
pub enum WorkerMsg {
    Pools(usize, Result<Vec<Pool>>),
    Datasets(usize, Result<Vec<Dataset>>),
    Snapshots(usize, Result<Vec<Snapshot>>),
    Vdevs(usize, String, Result<Vec<VdevLine>>),
    /// The `String` is the pool name this fetch was for — same
    /// stale-identity discard rationale as `Vdevs`.
    PoolProperties(usize, String, Result<Vec<Property>>),
    /// The `String` is the dataset name this fetch was for.
    DatasetProperties(usize, String, Result<Vec<Property>>),
    MutationDone(usize, Section, Result<()>),
}
```

- [ ] **Step 4: Add `refresh_pool_properties`/`refresh_dataset_properties`**

Add after `refresh_vdevs`:

```rust
    /// See `refresh_pools` for the in-flight/pending-refresh behavior.
    /// Not pool-aware in its in-flight key, same simplifying tradeoff
    /// `refresh_vdevs` makes for pools — correctness (never showing one
    /// pool's properties under another's) is fully handled by the
    /// stale-identity discard in `drain_worker_messages`.
    pub fn refresh_pool_properties(&mut self, runner: Arc<dyn CommandRunner>, pool: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::PoolProperties, host) {
            self.pending_refresh.insert((Section::PoolProperties, host));
            return;
        }
        self.begin_refresh(Section::PoolProperties, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::pool::get_pool_properties(runner.as_ref(), &pool);
            let _ = tx.send(WorkerMsg::PoolProperties(host, pool, result));
        });
    }

    /// See `refresh_pool_properties`.
    pub fn refresh_dataset_properties(&mut self, runner: Arc<dyn CommandRunner>, dataset: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::DatasetProperties, host) {
            self.pending_refresh.insert((Section::DatasetProperties, host));
            return;
        }
        self.begin_refresh(Section::DatasetProperties, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::dataset::get_dataset_properties(runner.as_ref(), &dataset);
            let _ = tx.send(WorkerMsg::DatasetProperties(host, dataset, result));
        });
    }
```

- [ ] **Step 5: Add the two `drain_worker_messages` arms**

Add to the `match msg` block, alongside the existing `Vdevs` arm:

```rust
                WorkerMsg::PoolProperties(host, pool, result) => {
                    self.end_refresh(Section::PoolProperties, host);
                    let had_pending = self.pending_refresh.remove(&(Section::PoolProperties, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::PoolProperties);
                    }
                    if Some(&pool) != self.selected_pool_name().as_ref() {
                        continue;
                    }
                    match result {
                        Ok(props) => {
                            self.pool_properties = props;
                            clamp_table_selection(&mut self.pool_properties_table, self.pool_properties.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::DatasetProperties(host, dataset, result) => {
                    self.end_refresh(Section::DatasetProperties, host);
                    let had_pending = self.pending_refresh.remove(&(Section::DatasetProperties, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::DatasetProperties);
                    }
                    if Some(&dataset) != self.selected_dataset_name().as_ref() {
                        continue;
                    }
                    match result {
                        Ok(props) => {
                            self.dataset_properties = props;
                            clamp_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
```

- [ ] **Step 6: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, 170 tests (165 from Task 4 + 5 new from this task); release build clean aside from expected dead_code warnings (`refresh_pool_properties`/`refresh_dataset_properties` are exercised by this task's own tests; `Section::PoolProperties`/`DatasetProperties` and the `WorkerMsg` variants are used by `drain_worker_messages` itself, so no new dead_code)

- [ ] **Step 7: Commit**

```bash
git add src/ui/state.rs
git commit -m "feat: pool/dataset property fetch and refresh wiring"
```

---

### Task 6: Dynamic 2-pane/3-pane layout and property table rendering

**Files:**
- Modify: `src/ui/layout.rs`

**Interfaces:**
- Consumes: `pool_properties`/`pool_properties_table`/`dataset_properties`/`dataset_properties_table`/`selected_pool_name`/`selected_dataset_name` from Tasks 4-5.
- Produces: `draw_pool_properties`, `draw_dataset_properties` (new render functions). `draw_main`'s behavior changes: a third pane appears for Pools/Datasets (when not drilled into vdevs).

No new tests in this task — per this plan's Global Constraints (and every prior plan's), ratatui rendering isn't unit-tested; this task's correctness is verified by `cargo build` succeeding and the manual verification checklist in the final task.

- [ ] **Step 1: Replace `draw_main`**

```rust
fn draw_main(f: &mut Frame, app: &mut App, area: Rect) {
    if app.drilled_pool.is_some() {
        draw_vdevs(f, app, area);
        return;
    }
    let section = app.sections[app.selected_section];
    if section == Section::Pools || section == Section::Datasets {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);
        match section {
            Section::Pools => {
                draw_pools(f, app, panes[0]);
                draw_pool_properties(f, app, panes[1]);
            }
            Section::Datasets => {
                draw_datasets(f, app, panes[0]);
                draw_dataset_properties(f, app, panes[1]);
            }
            _ => unreachable!("only Pools/Datasets reach this branch"),
        }
        return;
    }
    match section {
        Section::Hosts => draw_hosts(f, app, area),
        Section::Snapshots => draw_snapshots(f, app, area),
        Section::Pools | Section::Datasets => unreachable!("handled above"),
        Section::Vdevs | Section::PoolProperties | Section::DatasetProperties => {
            unreachable!("never in App::sections")
        }
    }
}
```

- [ ] **Step 2: Update `draw_pools`/`draw_datasets` titles with the new pane-toggle hint**

In `draw_pools`, change the title line:

```rust
    let title = format!("Pools — {} ('r' refresh, → for properties)", app.active_host_name());
```

In `draw_datasets`, change the title line:

```rust
    let title = format!("Datasets — {} ('r' refresh, → for properties)", app.active_host_name());
```

- [ ] **Step 3: Add `draw_pool_properties`/`draw_dataset_properties`**

Add after `draw_pools`:

```rust
fn draw_pool_properties(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app
        .pool_properties
        .iter()
        .map(|p| Row::new(vec![p.name.clone(), p.value.clone(), format!("{:?}", p.source)]));
    let widths = [Constraint::Length(16), Constraint::Length(16), Constraint::Min(10)];
    let pool = app.selected_pool_name().unwrap_or_else(|| "?".to_string());
    let title = format!("Properties — {pool} ('r' refresh, Enter to edit, ← back)");
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Property", "Value", "Source"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.pool_properties_table);
}
```

Add after `draw_datasets`:

```rust
fn draw_dataset_properties(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app
        .dataset_properties
        .iter()
        .map(|p| Row::new(vec![p.name.clone(), p.value.clone(), format!("{:?}", p.source)]));
    let widths = [Constraint::Length(16), Constraint::Length(16), Constraint::Min(10)];
    let dataset = app.selected_dataset_name().unwrap_or_else(|| "?".to_string());
    let title = format!("Properties — {dataset} ('r' refresh, Enter to edit, ← back)");
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Property", "Value", "Source"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.dataset_properties_table);
}
```

- [ ] **Step 4: Update the sidebar's section-label match for exhaustiveness**

`draw_sidebar`'s label `match` must stay exhaustive over `Section`'s now-7 variants (it only ever iterates `app.sections`, which never contains the two new variants, but the match is on the `Section` type, so it must handle them). Add two arms, alongside the existing `Section::Vdevs => "Vdevs"`:

```rust
                Section::Vdevs => "Vdevs",
                Section::PoolProperties => "Pool Properties",
                Section::DatasetProperties => "Dataset Properties",
```

(Same pattern as `Vdevs` — harmless, never actually rendered since neither variant is ever in `app.sections`.)

- [ ] **Step 5: Build and run the full test suite**

Run: `cargo build --release && cargo test`
Expected: build succeeds; PASS, 170 tests unchanged (this task adds no new tests, per the note above)

- [ ] **Step 6: Commit**

```bash
git add src/ui/layout.rs
git commit -m "feat: dynamic 2-pane/3-pane layout and property table rendering"
```

---

### Task 7: Property editing (`MutationCall`, `FormKind`, `Enter` key handling)

**Files:**
- Modify: `src/ui/modal.rs`
- Modify: `src/ui/state.rs`

**Interfaces:**
- Consumes: `Modal::Form`, `build_pending_action` from the mutation-UI plan; `PaneFocus`, `selected_pool_name`/`selected_dataset_name` from Task 4.
- Produces: `MutationCall` gains `SetPoolProperty { pool: String, key: String, value: String }` and `SetDatasetProperty { dataset: String, key: String, value: String }`. `FormKind` gains `EditPoolProperty { host: usize, pool: String, key: String }` and `EditDatasetProperty { host: usize, dataset: String, key: String }` (fields: `[value]`, pre-filled with the property's current value at modal-open time).

This is the last piece needed to actually edit a property — everything else (fetching, rendering, focus/navigation) already exists from Tasks 4-6.

- [ ] **Step 1: Write failing tests**

Add to `src/ui/modal.rs`'s test module:

```rust
#[test]
fn set_pool_property_call_runs_set_pool_property() {
    let runner = FakeRunner::new("");
    let call = MutationCall::SetPoolProperty { pool: "tank".to_string(), key: "comment".to_string(), value: "my pool".to_string() };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(
        calls[0],
        ("zpool".to_string(), vec!["set".to_string(), "comment=my pool".to_string(), "--".to_string(), "tank".to_string()])
    );
}

#[test]
fn set_dataset_property_call_runs_set_dataset_property() {
    let runner = FakeRunner::new("");
    let call = MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "compression".to_string(), value: "zstd".to_string() };
    call.run(&runner).unwrap();
    let calls = runner.calls.lock().unwrap();
    assert_eq!(
        calls[0],
        ("zfs".to_string(), vec!["set".to_string(), "compression=zstd".to_string(), "--".to_string(), "tank/data".to_string()])
    );
}
```

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn enter_on_right_focused_pool_property_opens_prefilled_edit_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.pool_properties = vec![Property { name: "comment".to_string(), value: "old comment".to_string(), source: PropertySource::Local }];
    app.pool_properties_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].value, "old comment", "the form must be pre-filled with the property's current value");
            assert_eq!(*kind, crate::ui::modal::FormKind::EditPoolProperty { host: 0, pool: "tank".to_string(), key: "comment".to_string() });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn enter_on_right_focused_pool_property_with_none_selected_does_not_open_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Enter);
    assert!(app.modal.is_none());
}

#[test]
fn handle_modal_key_edit_pool_property_form_submits_with_new_value() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.pool_properties = vec![Property { name: "comment".to_string(), value: "old".to_string(), source: PropertySource::Local }];
    app.pool_properties_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    // clear the pre-filled value, then type a new one
    for _ in 0.."old".len() {
        app.handle_modal_key(KeyCode::Backspace);
    }
    for c in "new comment".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::PoolProperties,
            call: crate::ui::modal::MutationCall::SetPoolProperty {
                pool: "tank".to_string(),
                key: "comment".to_string(),
                value: "new comment".to_string(),
            },
        })
    );
}

#[test]
fn enter_on_right_focused_dataset_property_opens_prefilled_edit_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.dataset_properties = vec![Property { name: "compression".to_string(), value: "lz4".to_string(), source: PropertySource::Local }];
    app.dataset_properties_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields[0].value, "lz4");
            assert_eq!(*kind, crate::ui::modal::FormKind::EditDatasetProperty { host: 0, dataset: "tank/data".to_string(), key: "compression".to_string() });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_edit_dataset_property_form_submits_with_new_value() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.dataset_properties = vec![Property { name: "compression".to_string(), value: "lz4".to_string(), source: PropertySource::Local }];
    app.dataset_properties_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    for _ in 0.."lz4".len() {
        app.handle_modal_key(KeyCode::Backspace);
    }
    for c in "zstd".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::DatasetProperties,
            call: crate::ui::modal::MutationCall::SetDatasetProperty {
                dataset: "tank/data".to_string(),
                key: "compression".to_string(),
                value: "zstd".to_string(),
            },
        })
    );
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test set_pool_property_call_runs_set_pool_property enter_on_right_focused_pool_property_opens_prefilled_edit_form`
Expected: FAIL (new `MutationCall`/`FormKind` variants don't exist yet)

- [ ] **Step 3: Add `SetPoolProperty`/`SetDatasetProperty` to `MutationCall` in `src/ui/modal.rs`**

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum MutationCall {
    ScrubPool { pool: String },
    DestroyPool { pool: String },
    CreatePool { name: String, vdev_spec: Vec<String> },
    OfflineDisk { pool: String, disk: String },
    OnlineDisk { pool: String, disk: String },
    ReplaceDisk { pool: String, old_disk: String, new_disk: String },
    SetPoolProperty { pool: String, key: String, value: String },
    CreateDataset { name: String },
    DestroyDataset { name: String },
    SetDatasetProperty { dataset: String, key: String, value: String },
    CreateSnapshot { dataset: String, name: String },
    DestroySnapshot { dataset: String, name: String },
    RollbackSnapshot { dataset: String, name: String },
}
```

Add to `MutationCall::run`'s `match self`:

```rust
            MutationCall::SetPoolProperty { pool, key, value } => crate::zfs::pool::set_pool_property(runner, pool, key, value),
            MutationCall::SetDatasetProperty { dataset, key, value } => {
                crate::zfs::dataset::set_dataset_property(runner, dataset, key, value)
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
    /// Fields: `[value]` — pre-filled with the property's current value.
    EditPoolProperty { host: usize, pool: String, key: String },
    /// Fields: `[name]`.
    CreateDataset { host: usize },
    /// Fields: `[value]` — pre-filled with the property's current value.
    EditDatasetProperty { host: usize, dataset: String, key: String },
    /// Fields: `[name]` — the snapshot's name; the dataset is fixed at
    /// modal-open time from the selected Datasets row.
    CreateSnapshot { host: usize, dataset: String },
}
```

- [ ] **Step 4: Add the two `build_pending_action` arms in `src/ui/state.rs`**

```rust
        FormKind::EditPoolProperty { host, pool, key } => {
            let value = fields[0].value.clone();
            Some(PendingAction {
                host: *host,
                section: Section::PoolProperties,
                call: MutationCall::SetPoolProperty { pool: pool.clone(), key: key.clone(), value },
            })
        }
        FormKind::EditDatasetProperty { host, dataset, key } => {
            let value = fields[0].value.clone();
            Some(PendingAction {
                host: *host,
                section: Section::DatasetProperties,
                call: MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: key.clone(), value },
            })
        }
```

(Unlike every other `FormKind` arm, this one never returns `None` for an empty value — an empty string is a valid value for some ZFS properties, e.g. clearing `comment`, so there's no "required field" validation to apply here.)

- [ ] **Step 5: Add the two `Enter`-while-right-focused arms to `handle_key`**

Add these two arms to the `match key` in `handle_key`, right after the existing `Enter`-drills-into-vdevs arm:

```rust
            KeyCode::Enter if self.pane_focus == PaneFocus::Right && self.current_section() == Section::Pools => {
                if let (Some(pool), Some(i)) = (self.selected_pool_name(), self.pool_properties_table.selected()) {
                    if let Some(prop) = self.pool_properties.get(i) {
                        self.modal = Some(Modal::Form {
                            title: format!("Edit '{}' on pool '{pool}'", prop.name),
                            fields: vec![FormField { label: prop.name.clone(), value: prop.value.clone() }],
                            focused: 0,
                            kind: FormKind::EditPoolProperty { host: self.selected_host, pool, key: prop.name.clone() },
                        });
                    }
                }
            }
            KeyCode::Enter if self.pane_focus == PaneFocus::Right && self.current_section() == Section::Datasets => {
                if let (Some(dataset), Some(i)) = (self.selected_dataset_name(), self.dataset_properties_table.selected()) {
                    if let Some(prop) = self.dataset_properties.get(i) {
                        self.modal = Some(Modal::Form {
                            title: format!("Edit '{}' on dataset '{dataset}'", prop.name),
                            fields: vec![FormField { label: prop.name.clone(), value: prop.value.clone() }],
                            focused: 0,
                            kind: FormKind::EditDatasetProperty { host: self.selected_host, dataset, key: prop.name.clone() },
                        });
                    }
                }
            }
```

- [ ] **Step 6: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, 177 tests (170 from Task 6 + 7 new: 2 in `ui::modal` + 5 in `ui::state`); release build clean aside from expected dead_code warnings

- [ ] **Step 7: Commit**

```bash
git add src/ui/modal.rs src/ui/state.rs
git commit -m "feat: pool/dataset property editing"
```

---

### Task 8: `event_loop` wiring

**Files:**
- Modify: `src/ui/mod.rs`

**Interfaces:**
- Consumes: `PaneFocus`, `refresh_pool_properties`/`refresh_dataset_properties`, `selected_pool_name`/`selected_dataset_name` from Tasks 4-5.

No new tests — `event_loop` isn't unit-tested (it drives a real terminal), consistent with the mutation-UI plan's Task 5. Correctness here is verified by `cargo build` (the `match` over `Section`'s now-7 variants must stay exhaustive) and the manual verification checklist in the final task.

- [ ] **Step 1: Replace `event_loop`'s body**

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
                    Section::PoolProperties => {
                        if let Some(pool) = app.selected_pool_name() {
                            app.refresh_pool_properties(host.runner.clone(), pool);
                        }
                    }
                    Section::DatasetProperties => {
                        if let Some(dataset) = app.selected_dataset_name() {
                            app.refresh_dataset_properties(host.runner.clone(), dataset);
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
                            } else if app.pane_focus == PaneFocus::Right {
                                match app.current_section() {
                                    Section::Pools => {
                                        if let Some(pool) = app.selected_pool_name() {
                                            app.refresh_pool_properties(host.runner.clone(), pool);
                                        }
                                    }
                                    Section::Datasets => {
                                        if let Some(dataset) = app.selected_dataset_name() {
                                            app.refresh_dataset_properties(host.runner.clone(), dataset);
                                        }
                                    }
                                    _ => {}
                                }
                            } else {
                                match app.current_section() {
                                    Section::Pools => app.refresh_pools(host.runner.clone()),
                                    Section::Datasets => app.refresh_datasets(host.runner.clone()),
                                    Section::Snapshots => app.refresh_snapshots(host.runner.clone()),
                                    Section::Hosts
                                    | Section::Vdevs
                                    | Section::PoolProperties
                                    | Section::DatasetProperties => {}
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
                    if key.code == KeyCode::Right && app.pane_focus == PaneFocus::Right {
                        if let Some(host) = inventory.hosts.get(app.selected_host) {
                            match app.current_section() {
                                Section::Pools => {
                                    if let Some(pool) = app.selected_pool_name() {
                                        app.refresh_pool_properties(host.runner.clone(), pool);
                                    }
                                }
                                Section::Datasets => {
                                    if let Some(dataset) = app.selected_dataset_name() {
                                        app.refresh_dataset_properties(host.runner.clone(), dataset);
                                    }
                                }
                                _ => {}
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

Add `use crate::ui::state::PaneFocus;` to the imports at the top of `src/ui/mod.rs`, alongside the existing `use crate::ui::state::Section;`.

- [ ] **Step 2: Build and run the full test suite**

Run: `cargo build --release && cargo test`
Expected: build succeeds (the `Section` match arms are exhaustive over all 7 variants); PASS, 177 tests unchanged (this task adds no new tests, per the note above)

- [ ] **Step 3: Commit**

```bash
git add src/ui/mod.rs
git commit -m "feat: wire property fetch/refresh into the event loop"
```

---

### Task 9: README update and manual verification

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: nothing new — this task only documents and manually verifies what Tasks 1-8 built.

- [ ] **Step 1: Update `README.md`'s Keys section**

Replace the `## Keys` section (and its `### Pools`/`### Vdevs`/`### Datasets`/`### Snapshots` subsections, added by the mutation-UI plan) with:

```markdown
## Keys

- `Tab` / `Shift+Tab`: switch sections (Hosts, Pools, Datasets, Snapshots) — this is now the *only* way to cycle sections; `Left`/`Right` have a different meaning (see below).
- `Up` / `Down` (in Hosts section): select the active host
- `Up` / `Down` (in Pools/Datasets/Snapshots, or a properties pane): select a row
- `r`: refresh whichever pane currently has focus (the middle list, or — for Pools/Datasets — the properties pane if it's focused)
- `q`: quit
- `Esc`: dismiss the current status message, back out of a focused properties pane, or (while a modal is open) cancel it

### Pools

- `Enter` on a selected pool: drill into its vdev/disk tree
- `Right`: move focus to the properties pane on the right (fetches the selected pool's `zpool` properties)
- `Left` (while the properties pane is focused): move focus back to the pool list
- `n`: create a new pool (prompts for name and vdev spec, e.g. `mirror da0 da1`)
- `d`: destroy the selected pool (type the pool's name to confirm)
- `s`: scrub the selected pool (y/n confirm)

### Pool/Dataset properties pane (right pane, `Right` to focus)

- `Up` / `Down`: select a property
- `Enter`: edit the selected property's value (prompts pre-filled with the current value; submitting runs `zpool set`/`zfs set`)
- `r`: re-fetch properties for the currently-selected pool/dataset
- `Esc` / `Left`: back out to the middle pane

### Vdevs (inside a drilled-into pool)

- `Esc` / `Backspace`: back out to the Pools list
- `o`: offline the selected disk (y/n confirm) — no-op on a mirror/raidz group row, not a physical disk
- `O`: online the selected disk (y/n confirm) — same restriction as `o`
- `x`: replace the selected disk (prompts for the new device name) — same restriction as `o`

### Datasets

- `Right` / `Left`: move focus to/from the properties pane (same as Pools, above)
- `n`: create a new dataset (prompts for name)
- `d`: destroy the selected dataset (type the dataset's name to confirm)
- `s`: snapshot the selected dataset (prompts for the snapshot's name only)

### Snapshots

- `d`: destroy the selected snapshot (type `dataset@name` to confirm)
- `b`: roll back to the selected snapshot (type `dataset@name` to confirm) — discards any writes made since
```

Also update the `## Status` section (added by the mutation-UI plan) to mention property editing — append this sentence to its existing paragraph:

```markdown
Pool and dataset properties (`zpool`/`zfs get`/`set`) can now be viewed and edited from a properties pane next to the Pools/Datasets lists (`Right` to focus it). Delegated permissions (`zfs allow`), file ACLs (`getfacl`/`setfacl`), and bhyve zvol association are still not implemented — planned as follow-up work.
```

- [ ] **Step 2: Manually verify on a real FreeBSD host**

Checklist (run as root on FreeBSD, ideally against the same scratch pool/dataset used for the mutation-UI plan's manual verification):

1. `cargo build --release` succeeds, `cargo test` passes all 177 tests.
2. On Pools, pressing `Right` opens the properties pane on the right and populates it with the selected pool's `zpool get all` output; the pool list stays visible on the left.
3. `Up`/`Down` while the properties pane is focused moves selection within the properties list, not the pool list.
4. `Left` (or `Esc`) returns focus to the pool list; `Up`/`Down` there moves the pool list selection and the properties pane clears until `Right` is pressed again for the newly-selected pool.
5. `Enter` on a selected property opens a form pre-filled with its current value; changing it and submitting runs `zpool set` and the properties pane refreshes with the new value.
6. `Tab`/`Shift+Tab` still cycle Hosts/Pools/Datasets/Snapshots correctly, and reset focus back to the middle pane if a properties pane was focused.
7. Repeat steps 2-5 for Datasets (`zfs get`/`zfs set`).
8. Hosts, Snapshots, and the Vdevs drill-down look and behave exactly as before this plan — no properties pane, `Left`/`Right` do nothing there.
9. If a remote host is configured, repeat a representative subset (view + edit one pool property, one dataset property) over SSH.
10. `q` cleanly exits from every state, including with a properties pane focused (press `Esc` first if needed, matching the existing vdev drill-down pattern).

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: update keybindings and status for the properties pane"
```
