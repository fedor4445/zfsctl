# zfsctl: Disk Inventory + Selection-Based Pool/Vdev Creation — Design

## Summary

Adds a new top-level **Disks** section listing every block device the
host sees, with its size and current pool/vdev membership (or
`available` if unclaimed). Available disks can be selected with a
checkbox-style toggle and used to build a pool or add a vdev without
typing device names from memory — replacing the `create_pool`/`add_vdev`
free-text vdev-spec entry point's biggest friction point while
reusing those functions unchanged.

A related but separately-scoped action, `zpool resilver`, is added to
the Pools section alongside the existing Scrub key, since it operates
on a whole pool rather than a single disk.

## Goals

- A `Section::Disks` (Tab-cycled, sibling to Pools/Datasets/Snapshots)
  showing `name | size | status` for every disk `geom disk list`
  reports, with status cross-referenced against every pool on the host
  (fetched fresh on each Disks refresh, not reused from an
  already-loaded Pools list).
- Space-toggle multi-select on `available` disks; `'n'` builds a new
  pool from the selection, `'a'` adds a vdev to a named existing pool
  from the selection — both via the existing `Modal::Form` mechanism
  and the existing `MutationCall::CreatePool`/`AddVdev`, no new
  mutation calls.
- `zpool resilver <pool>` as a new Pools-section action, same
  `Modal::Confirm` shape as the existing Scrub key.

## Non-goals

- Building more than one vdev group in a single create/add from the
  Disks section (e.g. two separate mirrors at once) — the existing
  free-text Create Pool form still covers that case.
- Per-disk actions (offline/online/replace) duplicated into the Disks
  section — those stay exclusively in the Vdevs drill-down, where the
  owning pool is already known from context.
- A guided redundancy-level picker/wizard (radio buttons, capacity
  preview, etc.) — the redundancy field is a single free-text keyword
  (blank/mirror/raidz1/raidz2/raidz3), same input style as every other
  form in this app.
- Live end-to-end demonstration of the create-from-selection flow on
  the current FreeBSD test box: it has exactly one physical disk
  (`ada0`, already claimed by `zroot`) — `scratch` uses file-backed
  vdevs, not real disks. Verified via unit tests plus a live check that
  `ada0` renders correctly as `in zroot`; the full create/add flow
  needs either a VM with spare virtual disks or to be exercised against
  `FakeRunner` only.

## Architecture

### Data model (`src/zfs/disk.rs`, new module)

```rust
pub struct DiskInfo {
    pub name: String,       // e.g. "ada0"
    pub size: u64,          // bytes, from Mediasize
    pub membership: DiskMembership,
}

pub enum DiskMembership {
    Available,
    InPool { pool: String, vdev: String }, // vdev = owning group's name (e.g. "mirror-0"), or the disk's own name if it's a bare top-level vdev
}

pub fn list_disks(runner: &dyn CommandRunner) -> Result<Vec<DiskInfo>>; // geom disk list
pub fn annotate_membership(disks: &mut [DiskInfo], pool: &str, vdevs: &[VdevLine]);
```

`list_disks` parses `geom disk list`'s block format — tracks the
current disk from each `Geom name: <x>` line, and reads the first
`Mediasize: <bytes> (...)` line inside that block for size:

```
Geom name: ada0
Providers:
1. Name: ada0
   Mediasize: 48318382080 (45G)
   Sectorsize: 512
   ...
```

(confirmed against the real test box — a single-disk system, so the
fixture used for the multi-disk parser test is synthesized from this
real block's shape, repeated).

`annotate_membership` walks `vdevs` (the same `Vec<VdevLine>` shape
`parse_pool_status_vdevs`/`get_pool_vdevs` already produce, depth-
tracked) and, for each entry where `is_leaf_disk(name)` is true and
matches a `DiskInfo.name`, sets that disk's membership to `InPool`,
naming the nearest enclosing group (or the disk's own name if it's a
bare top-level vdev with no group).

### `Section::Disks`

Added to the Tab-cycled `App::sections` list, alongside Hosts, Pools,
Datasets, Snapshots (unlike `Vdevs`/`PoolProperties`/`DatasetProperties`,
which stay drill-down-only tags never added to that list).

New `App` fields: `disks: Vec<DiskInfo>`, `disks_table: TableState`,
`selected_disks: HashSet<String>` (disk names).

### Data flow: refresh

`WorkerMsg::Disks(usize, Result<Vec<DiskInfo>>)` — host-scoped, like
`Pools`/`Datasets`/`Snapshots` (no per-entity staleness tag needed,
since this isn't tied to one selected row).

`App::refresh_disks` follows the same `begin_refresh`/`is_refreshing`/
`pending_refresh` in-flight-guard pattern `refresh_pools` uses, but its
spawned thread makes three sequential calls rather than one:
`zfs::disk::list_disks`, then `zfs::pool::list_pools` (to discover
every pool name), then `zfs::pool::get_pool_vdevs` once per pool —
folding the results together with `annotate_membership` before sending
`WorkerMsg::Disks`. This makes a Disks refresh self-contained and
correct even if the user has never visited the Pools section this
session, at the cost of being a heavier refresh than the other
sections' single-call ones.

Refresh triggers, matching the existing convention (no new
auto-refresh-on-Tab behavior — Datasets/Snapshots don't have that
today either):

- `'r'` while on `Section::Disks` — new arm in `ui/mod.rs`'s existing
  `'r'`-handling match.
- Post-mutation: any mutation that changes pool topology (`CreatePool`,
  `AddVdev`, `ReplaceDisk`, `OfflineDisk`, `OnlineDisk`, `DestroyPool`)
  already queues a `Section::Pools`/`Section::Vdevs` refresh in
  `drain_worker_messages`'s `refreshes_needed` — `Section::Disks` is
  added alongside those, with a matching dispatch arm in `ui/mod.rs`'s
  queued-refresh loop.

### UX and forms

Table columns: `Name | Size | Status` (`"available"` or `"in <pool>
(<vdev>)"`).

- **Space** (guarded: `current_section() == Section::Disks`) — toggles
  the highlighted disk in `selected_disks` if its status is
  `Available`; on a claimed disk it sets a status-message hint instead
  ("disk already in pool '<pool>'"), the same pattern as the existing
  "select a disk, not a group" hint in the Vdevs drill-down. Selected
  disks render with a `[x]`/`[ ]` marker.
- **'n'** (guarded: `current_section() == Section::Disks &&
  !selected_disks.is_empty()`) — opens `Modal::Form { title: "Create
  Pool from N selected disk(s)", fields: [Name, Redundancy
  (blank=stripe, or mirror/raidz1/raidz2/raidz3)], kind:
  FormKind::CreatePoolFromDisks { host, disks } }`, `disks` being a
  sorted snapshot of the current selection.
- **'a'** (same guard) — opens `FormKind::AddVdevFromDisks { host,
  disks }` asking for `[Target pool name, Redundancy]`.
- Pressing `'n'`/`'a'` with nothing selected sets a status-message hint
  ("select at least one disk with Space first") instead of opening a
  form.
- **Esc** while on the Disks section with no modal open clears the
  current selection (a quick "start over" affordance).
- On successful submit of either form, `selected_disks` is cleared.

`'n'`/`'a'` are reused key letters — Pools' Create Pool and the Vdevs
drill-down's Add Vdev already use them — disambiguated purely by
section/context guards, the same convention `'o'`/`'O'`/`'x'` already
follow.

Both new `FormKind`s reuse the **existing** `MutationCall::CreatePool`/
`MutationCall::AddVdev` — no new `MutationCall` variants. New
`build_pending_action` arms construct `vdev_spec` as `disks` verbatim
when redundancy is blank (each disk becomes its own top-level vdev, a
stripe) or as `[redundancy] + disks` when a keyword is given (one
mirror/raidz group over every selected disk). Building more than one
group from a single selection isn't supported by this flow (see
Non-goals).

### `zpool resilver` (Pools section)

```rust
pub fn resilver_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()>; // zpool resilver -- <pool>
```

Confirmed via `man zpool-resilver` on the real test box: pool-scoped,
takes one or more pool names, restarts any in-progress resilver from
the beginning. Same shape as `scrub_pool` exactly — no disk argument.
New key `'R'` (shift-r, guarded like the existing `'s'` scrub key:
`current_section() == Section::Pools && drilled_pool.is_none() &&
pane_focus == PaneFocus::Middle`) opens `Modal::Confirm { prompt:
"Resilver pool '<pool>'? (y/n)", .. }`, new `MutationCall::ResilverPool
{ pool }` delegating to `resilver_pool`. Uppercase avoids any clash
with the lowercase `'r'` refresh key, which is global across every
section.

## Components / files touched

- New: `src/zfs/disk.rs` (`DiskInfo`, `DiskMembership`, `list_disks`,
  `annotate_membership`).
- `src/zfs/mod.rs`: register the new module.
- `src/zfs/pool.rs`: `resilver_pool`.
- `src/ui/state.rs`: `Section::Disks`; `App` fields (`disks`,
  `disks_table`, `selected_disks`); `WorkerMsg::Disks`;
  `refresh_disks`; `handle_key` arms (Space/`'n'`/`'a'`/Esc/Up/Down for
  Disks, `'R'` for resilver in Pools);
  `build_pending_action` arms for `CreatePoolFromDisks`/
  `AddVdevFromDisks`; `drain_worker_messages` additions
  (`Section::Disks` alongside topology-changing mutations' queued
  refreshes).
- `src/ui/modal.rs`: `FormKind::CreatePoolFromDisks`,
  `FormKind::AddVdevFromDisks`, `MutationCall::ResilverPool`.
- `src/ui/layout.rs`: Disks table rendering (including selection
  markers), title hints for both Disks and Pools sections.
- `src/ui/mod.rs`: `'r'`-key and queued-refresh dispatch arms for
  `Section::Disks`.

## Testing

- `disk.rs`: `list_disks` against a (synthesized, real-shape)
  multi-disk `geom disk list` fixture; `annotate_membership` correctly
  tags a leaf disk nested under a mirror/raidz group and leaves
  unrelated disks `Available`.
- `pool.rs`: `resilver_pool` command-shape, flag-smuggling guard, and
  failure-surfacing tests, matching the existing `scrub_pool`/
  `destroy_pool` test shapes.
- `state.rs`: `refresh_disks` populates from a `FakeRunner` sequence
  (list_disks → list_pools → get_pool_vdevs per pool); Space toggles
  only `Available` rows and no-ops with a hint on claimed ones; `'n'`/
  `'a'` open the right `FormKind` with the right disk snapshot and hint
  when selection is empty; `build_pending_action` for both new
  `FormKind`s (blank-redundancy stripe case and named-redundancy group
  case); selection clears after submit and on Esc; a topology-changing
  mutation queues `Section::Disks` alongside `Pools`/`Vdevs`; the new
  resilver key opens the right `Confirm` modal.
- `modal.rs`: `MutationCall::ResilverPool` delegation test (matching
  the existing `ScrubPool`/`DestroyPool` delegation test shape). No new
  delegation test needed for `CreatePoolFromDisks`/`AddVdevFromDisks`
  since they build the *existing* `CreatePool`/`AddVdev` calls, already
  covered.

No ratatui rendering tests, consistent with every prior plan.
