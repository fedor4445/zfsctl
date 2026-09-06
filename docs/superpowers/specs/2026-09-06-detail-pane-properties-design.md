# zfsctl: Detail Pane + Pool/Dataset Property Editing — Design

## Summary

Adds a third pane — sidebar (unchanged) | middle pane (today's
per-section list) | right pane (new, per-row detail) — but only for
sections where it earns its keep: Pools and Datasets. Every other
section (Hosts, Snapshots, the Vdevs drill-down) keeps today's
two-region layout unchanged. This spec proves the layout and
focus-switching mechanism using the lowest-risk content available —
editable `zpool`/`zfs` properties, which parse via the same clean
`-H -p` tabular format every other data source in this app already
uses. Two follow-up specs build on this mechanism for riskier content:
delegated permissions (`zfs allow`) and file ACLs (`getfacl`/`setfacl`),
and bhyve zvol association — both deliberately out of scope here.

## Goals

- A right-hand detail pane, shown only for sections where it's
  editable — Pools and Datasets — displaying and letting you change
  `zpool`/`zfs` properties for whatever row is selected in the middle
  pane.
- Hosts, Snapshots, and the Vdevs drill-down stay exactly as they are
  today (middle pane at full width, no third pane) — there's nothing
  genuinely editable or worth showing for those that a static
  info-only pane would add, so no pane is built for them at all rather
  than building one with contrived content.
- A focus model (`Left`/`Right` arrows move focus between the middle
  and right panes, where the right pane exists) that reuses the app's
  existing "same key, different meaning depending on state" convention
  rather than inventing a new interaction paradigm.

## Non-goals (this spec)

- Delegated ZFS permissions (`zfs allow`/`unallow`) and file ACLs
  (`getfacl`/`setfacl`) — both lack a machine-readable output format
  (unlike `zpool`/`zfs get`), need dedicated parsers, and add their own
  mutation actions. Deferred to a follow-up "Permissions & ACLs" spec,
  which will also introduce a sub-tab concept in the Datasets right
  pane (Properties / Permissions / ACLs) once there's more than one
  view to switch between — no sub-tab plumbing is built in this spec,
  since building it for a single view would be speculative.
- bhyve zvol association (which VM, if any, uses a given zvol; start/
  stop) — a different external tool with its own config format and
  failure modes (not installed, VM on a remote host). Deferred to its
  own follow-up spec, isolated from the ZFS-permissions work.
- System users/groups (`pw`), which don't hang off a single selected
  row the way dataset/pool detail does — out of scope for a detail
  pane entirely, not just deferred.

## Architecture

### Layout

Two layouts, chosen per section:

```
Hosts / Snapshots / Vdevs drill-down (unchanged):
+----------+---------------------------------------------+
| sidebar  | middle pane (today's list/table)             |
| (24 col) |                                               |
+----------+---------------------------------------------+
|                  status bar                              |
+-------------------------------------------------------+

Pools / Datasets (new):
+----------+------------------------+------------------+
| sidebar  | middle pane            | right pane       |
| (24 col) | (today's list/table)   | (new: editable   |
|          |                        |  properties)     |
+----------+------------------------+------------------+
|                  status bar                          |
+-------------------------------------------------------+
```

`draw_main`'s horizontal `Layout` gains a third `Constraint` only when
`app.sections[app.selected_section]` is `Pools` or `Datasets` — every
other section keeps today's two-region split, full width for the
middle pane. This also means: no static/read-only detail content is
built for Hosts, Snapshots, or the Vdevs drill-down at all (see
Goals) — not "a pane with placeholder text," no pane.

### Focus model

A new `PaneFocus` enum on `App`: `Middle` | `Right`, meaningful only
while the current section is Pools or Datasets. Outside those two
sections there is no right pane to focus, so `Left`/`Right` are
straightforward no-ops there (there's nothing to toggle into).

- **`Left`/`Right` arrows**: toggle `PaneFocus` between `Middle` and
  `Right`, for sections where the right pane is interactive. Moving
  into `Right` focus (from a valid middle-pane selection) triggers the
  properties fetch (see Data Flow).
- **`Up`/`Down`**: apply to whichever pane currently has focus — middle
  pane's row selection when `PaneFocus::Middle` (existing behavior,
  unchanged), the properties list's row selection when
  `PaneFocus::Right`.
- **`Tab`/`Shift+Tab`**: cycle sidebar sections, always, regardless of
  `PaneFocus` — no sub-tab concept exists yet (see Non-goals), so
  there's nothing else for these keys to mean while right-focused.
  `Shift+Tab` is new (cycling backward), closing a gap the last
  whole-branch review flagged as an unimplemented spec item.
- **`Esc`**: when `PaneFocus::Right`, backs focus out to `Middle`
  first — same precedence pattern already used for backing out of the
  vdev drill-down (a specific "back out of X" check wins over the
  generic "clear status message" `Esc` arm).
- **`Enter`** on a selected property row (only meaningful when
  `PaneFocus::Right`): opens a one-field `Form` modal pre-filled with
  the property's current value. Submitting runs the matching `set`
  mutation. This reuses `Enter`'s existing "act on the thing you're
  looking at" meaning (already true for drilling into a pool's vdevs)
  rather than introducing a new key for "edit."

`PaneFocus` and `drilled_pool` (the existing vdev drill-down state) are
independent, but only one is reachable at a time in practice: drilling
into a pool's vdevs (`Enter` on a Pools row) is only bound while
`PaneFocus::Middle` — while `PaneFocus::Right` (viewing/editing that
pool's properties), `Enter` means "edit the selected property," not
"drill in." To drill into vdevs from `Right` focus, back out with `Esc`
first. This mirrors the existing guard style (`current_section() ==
Section::Pools && drilled_pool.is_none()`) with one more condition
(`&& pane_focus == PaneFocus::Middle`) added to the drill-in arm — no
new guard *pattern*, just one more clause on an existing one.

Two more hygiene resets, consistent with the app's existing "don't
leave stale state visible after switching context" convention: `Tab`/
`Shift+Tab` (switching sidebar sections) resets `PaneFocus` to
`Middle` — right-pane focus from one section has no meaning in
another. Switching hosts (already calls `clear_host_data`) also resets
`PaneFocus` to `Middle` and clears `pool_properties`/`dataset_properties`,
the same way it already clears `pools`/`datasets`/`snapshots`.

### Data flow: fetching and editing properties

Properties are fetched **on entering `Right` focus** for a given
row — not on every middle-pane row change while `Middle`-focused, to
avoid spawning a fetch (and, over SSH, a network round-trip) on every
arrow-key scroll through the Pools/Datasets list. `r` re-fetches while
`Right`-focused, same as every other section's refresh key.

The fetch result is tagged by **both host and the pool/dataset name it
was fetched for** — `WorkerMsg::PoolProperties(usize, String,
Result<Vec<Property>>)` / `WorkerMsg::DatasetProperties(usize, String,
Result<Vec<Property>>)` — and `drain_worker_messages` discards a result
whose name no longer matches the currently-selected row, the same
stale-host-*and*-stale-identity discard pattern the Vdevs drill-down
now uses after its final-review fix. Building this in from the start
avoids re-learning that lesson: a slow property fetch for pool A must
not render under pool B's title if the user switched rows while it was
in flight.

Editing: `Enter` on a property row opens `Modal::Form` with one field
(the new value), reusing the exact `FormKind`/`build_pending_action`
mechanism Plan 2 built. Two new `FormKind` variants:
`EditPoolProperty { host: usize, pool: String, key: String }` and
`EditDatasetProperty { host: usize, dataset: String, key: String }`
(fields: `[value]`). Submitting builds a `PendingAction` with two new
`MutationCall` variants, `SetPoolProperty { pool, key, value }` /
`SetDatasetProperty { dataset, key, value }`, delegating to the new
library functions below. On success, the properties fetch re-runs
(not the Pools/Datasets list itself, unless the property happens to be
one displayed there — out of scope to special-case that).

## Components

### `Property` (new, shared shape)

```rust
pub struct Property {
    pub name: String,
    pub value: String,
    pub source: PropertySource,
}

pub enum PropertySource {
    Local,
    Default,
    Inherited(String), // the ancestor dataset it's inherited from
    Other(String),
}
```

`zpool`/`zfs get all -H -p` both emit a `source` column (`local`,
`default`, `inherited from <dataset>`, or occasionally other values
depending on property type) — kept even though this spec's UI only
needs to *display* it (e.g. dimmed styling for non-local values is a
natural touch, not a hard requirement), since parsing it once now means
the follow-up specs and any later "reset to default" feature don't
need to re-derive it.

### Right-pane content

Only two sections have one:

- **Pools**: `Vec<Property>` from `get_pool_properties`, rendered as a
  `Table` with its own `TableState` (`pool_properties_table`), same
  selection/highlight pattern as every other table in this app.
- **Datasets**: `Vec<Property>` from `get_dataset_properties`, same
  shape (`dataset_properties_table`).

Hosts, Snapshots, and the Vdevs drill-down are unchanged from today —
no new rendering, no new state, no new library code for them in this
spec.

### Library layer (`src/zfs/pool.rs`, `src/zfs/dataset.rs`)

```rust
pub fn get_pool_properties(runner: &dyn CommandRunner, pool: &str) -> Result<Vec<Property>>; // zpool get all -H -p -- <pool>
pub fn set_pool_property(runner: &dyn CommandRunner, pool: &str, key: &str, value: &str) -> Result<()>; // zpool set <key>=<value> -- <pool>

pub fn get_dataset_properties(runner: &dyn CommandRunner, dataset: &str) -> Result<Vec<Property>>; // zfs get all -H -p -- <dataset>
pub fn set_dataset_property(runner: &dyn CommandRunner, dataset: &str, key: &str, value: &str) -> Result<()>; // zfs set <key>=<value> -- <dataset>
```

Both parse the standard 4-column tab-separated `get all` output
(`name`, `property`, `value`, `source`) — the same parsing shape
`parse_pool_list`/`parse_dataset_list` already use, just a different
column set. Both mutations go through the existing `run_checked()`
helper and the existing `--` argument-injection guard convention, no
new security surface.

`Property` itself lives in a new small module, `zfs::property` (shared
by both `pool.rs` and `dataset.rs` rather than duplicated), with its
own parse function taking the raw 4-column line and its own fixture
tests.

## Testing

Same approach as Plans 1-2: fixture-based parser tests (captured real
`zpool get all -H -p`/`zfs get all -H -p` output, including the
`source` column's several real forms) plus `FakeRunner`-driven tests
for the mutation functions and the new `App` state (focus toggling,
stale-identity discard for `PoolProperties`/`DatasetProperties`,
edit-form submit → correct `MutationCall`). No ratatui rendering
tests, consistent with every prior plan.
