# zfsctl: Wiring ZFS mutations into the TUI — Design

## Summary

Plan 1 built a fully tested `zfs::*` library layer (pool/dataset/snapshot
create/destroy/scrub/replace/rollback/replicate) and a read-only, browse-only
TUI (Hosts → Pools → Datasets → Snapshots, with manual refresh). This design
wires 11 of those 12 mutations into the TUI as user-triggerable actions, via
a modal/dialog interaction model, plus three prerequisite fixes the Plan 1
final review identified as blocking or risky to build mutations on top of
without first addressing.

`replicate_snapshot` (`zfs send | zfs receive`, potentially cross-host and
long-running with no cancellation story) is explicitly out of scope — it
needs its own design pass for progress/cancellation and is left for a
dedicated follow-up plan.

## Goals

- Let a user create/destroy/scrub a pool, offline/online/replace a disk
  within a pool, create/destroy a dataset, and create/destroy/rollback a
  snapshot, all from the TUI, targeting local or remote (SSH) hosts.
- Confirmation friction matched to risk: type-to-confirm for irreversible
  operations, y/n for reversible/lower-risk ones, no confirmation for
  pure-creation forms.
- Fix the three Plan 1-review-identified prerequisites before adding more
  mutation call sites on top of them: row selection/scrolling (a hard
  dependency — actions target "the selected row", which doesn't exist yet),
  per-(section, host) in-flight refresh tracking (the existing per-section
  tracking can silently drop a refresh after switching hosts), and a
  `run_checked()` helper to stop hand-duplicating the success/failure-check
  block (already duplicated ~15 times; this plan roughly doubles the call
  site count).

## Non-goals (this plan)

- `replicate_snapshot` / any send-receive UI wiring (deferred, see Summary).
- Guided vdev-spec construction (disk-picker wizard) for `zpool create` —
  depends on a disk inventory feature (`disks.rs`, `camcontrol`/`smartctl`)
  not built yet. This plan's create-pool form takes a free-text vdev spec
  instead (see Components: Create Pool Form).
- Dataset property editing (`zfs set`) — not part of Plan 1's library layer,
  out of scope here too.
- Jobs, shares, users, disk health — untouched; separate future plans per
  the top-level design spec (`2026-09-04-zfsctl-tui-design.md`).

## Architecture

### Modal state machine

`App` gains a single new field: `pub modal: Option<Modal>`.

```rust
pub enum Modal {
    Confirm {
        prompt: String,
        action: PendingAction,
    },
    ConfirmTyped {
        prompt: String,
        expected: String,
        input: String,
        action: PendingAction,
    },
    Form {
        title: String,
        fields: Vec<FormField>,
        focused: usize,
        kind: PendingActionKind,
    },
}

pub struct FormField {
    pub label: String,
    pub value: String,
}

pub struct PendingAction {
    pub host: usize,       // index into HostInventory / App's host list
    pub section: Section,  // which section to re-list on success
    pub call: MutationCall, // the concrete zfs::* invocation to make
}
```

`MutationCall` is an enum with one variant per mutation, carrying exactly
the arguments that `zfs::*` function needs (e.g.
`ScrubPool { pool: String }`, `DestroyDataset { name: String }`,
`ReplaceDisk { pool: String, old: String, new: String }`). Built when a
modal is submitted, from the modal's collected input plus the row that was
selected when the action was triggered.

### Event loop changes

`event_loop` (in `src/ui/mod.rs`) branches on `app.modal.is_some()` before
dispatching a key:

- **Modal open:** route to `App::handle_modal_key(key)`. `Esc` clears
  `app.modal` (cancel, no-op). For `Form`: `Tab`/`Shift+Tab` move
  `focused` between fields, printable chars/`Backspace` edit the focused
  field's `value`, `Enter` on the last field (or a dedicated submit
  affordance) builds the `MutationCall` and dispatches it. For `Confirm`:
  `y`/`Enter` submits, `n`/`Esc` cancels. For `ConfirmTyped`: printable
  chars/`Backspace` edit `input`, `Enter` submits only if `input == expected`
  (otherwise no-op — wrong confirmation text just doesn't submit, no error
  shown), `Esc` cancels.
- **No modal:** existing `App::handle_key` runs unchanged, plus the new
  action keys below (which open a modal rather than acting immediately,
  except where noted).

### Rendering

`layout::draw` renders the normal frame first, then — if `app.modal` is
`Some` — draws a centered overlay: `Clear` over a `Rect` sized to the
modal's content, bordered `Block`, with the prompt/fields/type-to-confirm
buffer rendered inside. Standard ratatui popup pattern; no new rendering
abstraction needed.

## Prerequisite fixes (Tasks 1-3)

### 1. Row selection and scrolling

Each of Pools, Datasets, Snapshots, and the new Vdevs view (below) gets a
`ratatui::widgets::TableState` (tracks both selected row and scroll
offset natively — no hand-rolled scrolling math). `Up`/`Down` move the
selection via `TableState::select`; `layout.rs`'s `Table` widgets switch
from `f.render_widget(table, area)` to
`f.render_stateful_widget(table, area, &mut table_state)`, with the
selected row styled via `.highlight_style(...)` on the `Table`.

Selection clamps when the underlying `Vec` shrinks (same pattern Plan 1
already established for `drain_worker_messages` clamping stale indices) and
resets to `0` when the host or section's data is cleared (host switch,
already existing behavior from Plan 1's fixes).

### 2. Per-(section, host) in-flight tracking

Replaces `pools_loading: bool` / `datasets_loading: bool` /
`snapshots_loading: bool` with a single `in_flight: HashSet<(Section,
usize)>` (section, host index). `refresh_*(host_idx, runner)` becomes a
no-op if `(section, host_idx)` is already in the set; inserts on spawn,
removes on the matching `WorkerMsg` arrival (regardless of Ok/Err).
`is_loading()` becomes `!self.in_flight.is_empty()` (unchanged observable
behavior for the sidebar indicator) or, if useful for the Vdevs view, a
per-section query — implementer's call, not load-bearing for correctness.

### 3. `run_checked()` helper

In `src/zfs/mod.rs`:

```rust
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
```

Every existing mutation function in `pool.rs`/`dataset.rs`/`snapshot.rs`
(and the new ones added by this plan, if any — there are none, all 11 target
functions already exist) is refactored to call `run_checked` instead of
hand-inlining the check. Parsing-path functions (`list_pools`,
`list_datasets`, `list_snapshots`, `get_pool_vdevs`) are refactored too,
for consistency — they have the identical block. Purely mechanical; no
behavior change, covered by the existing test suite (which asserts on
`Result`, not on the internal call shape).

## Components

### Vdev drill-down view

New `Section`-adjacent view (not part of the `Tab`-cycled `sections` list —
reached only via `Enter` on a selected Pools row, left via `Esc`/
`Backspace`). Renders `get_pool_vdevs(runner, pool_name)`'s `Vec<VdevLine>`
as an indented list/table (depth already carried on `VdevLine`), with its
own `TableState` for selecting a specific disk (a `VdevLine` whose `name`
is a leaf device, not an intermediate `mirror-N`/`raidz1-N` grouping node —
offline/online/replace only make sense targeting a physical disk. The
implementer should filter selectable rows to leaves, e.g. depth == the
tree's max depth for that pool, or by checking the name doesn't match
`^(mirror|raidz[123])-\d+$`; either is acceptable, tests should assert
whichever is chosen).

`App` tracks which pool is drilled into (`pub drilled_pool: Option<String>`
or similar) and whose vdevs are currently displayed (fetched fresh via the
same worker-thread pattern as other refreshes, on entering the view and on
`r`).

`Section` gains a `Vdevs` variant so vdev-view state (in-flight tracking,
`WorkerMsg` tagging, `TableState`) fits the same per-section patterns as
the other views — but it is never added to `App::sections` (the `Tab`-cycled
list), since it's only reachable via the Pools drill-down, not direct
navigation.

### Keybindings

| Section | Key | Action | Modal |
|---|---|---|---|
| Pools | `n` | Create pool | Form: name, vdev spec (free text) |
| Pools | `d` | Destroy selected pool | ConfirmTyped: pool name |
| Pools | `s` | Scrub selected pool | Confirm (y/n) |
| Pools | `Enter` | Drill into vdev tree | — (navigation, not a mutation) |
| Vdevs | `o` | Offline selected disk | Confirm (y/n) |
| Vdevs | `O` | Online selected disk | Confirm (y/n) |
| Vdevs | `x` | Replace selected disk | Form: new device name |
| Vdevs | `Esc` / `Backspace` | Back to Pools | — |
| Datasets | `n` | Create dataset | Form: name |
| Datasets | `d` | Destroy selected dataset | ConfirmTyped: dataset name |
| Datasets | `s` | Snapshot selected dataset | Form: snapshot name (dataset implied by selection) |
| Snapshots | `d` | Destroy selected snapshot | ConfirmTyped: `dataset@name` |
| Snapshots | `b` | Rollback to selected snapshot | ConfirmTyped: `dataset@name` |

Existing global keys (`q`, `Tab`/`Left`/`Right`, `Up`/`Down`, `r`) are
unchanged and remain active whenever no modal is open. `Enter`/`Esc` are
newly meaningful (Pools drill-down, modal confirm/cancel) but don't
collide with any existing binding.

### Create Pool Form

Two fields: `name` (text), `vdev spec` (free text, e.g. `mirror da0 da1`
or `raidz1 da0 da1 da2`). On submit, the vdev-spec string is split on
whitespace into `Vec<&str>` and passed to the existing
`create_pool(runner, name, vdev_spec: &[&str])` unchanged — no new
parsing/validation beyond what `zpool create` itself performs. This
mirrors the plan's explicit deferral of a guided disk-picker wizard.

## Data flow: mutation dispatch and refresh

`WorkerMsg` gains one variant:

```rust
pub enum WorkerMsg {
    Pools(usize, Result<Vec<Pool>>),
    Datasets(usize, Result<Vec<Dataset>>),
    Snapshots(usize, Result<Vec<Snapshot>>),
    Vdevs(usize, Result<Vec<VdevLine>>),          // new: for the drill-down view
    MutationDone(usize, Section, Result<()>),      // new: host, section, outcome
}
```

Submitting a modal (Enter with valid input/confirmation):
1. Builds the `MutationCall` from the modal's collected state.
2. Clears `app.modal` immediately (the modal does not wait for the
   operation to complete).
3. Spawns a worker thread that runs the single matching `zfs::*` call
   against the target host's `Arc<dyn CommandRunner>` and sends
   `WorkerMsg::MutationDone(host, section, result)`.

`drain_worker_messages` handles `MutationDone` like the existing variants:
if `host` no longer matches `selected_host`, discard (same stale-host rule
Plan 1's fix already established for `Pools`/`Datasets`/`Snapshots`).
Otherwise: on `Ok(())`, immediately spawn the matching `refresh_*` for that
section/host (re-list, per the top-level spec's "check exit status +
stderr, then re-list to refresh state") and clear the in-flight flag for
the mutation itself (distinct from the refresh's own in-flight tracking,
which starts fresh); on `Err(e)`, set `status_message = Some(e.to_string())`
and do not refresh — nothing changed on the target, no need to re-list.

Which section(s) get re-listed on success is generally "whichever section's
displayed data the mutation changed" (pool ops → Pools, dataset ops →
Datasets, snapshot ops → Snapshots), with one exception: the three vdev
ops (offline/online/replace-disk) re-list **both** Vdevs (the open
drill-down view) **and** Pools, since a disk going offline/online/being
replaced changes the parent pool's `health` too, and leaving the Pools
table's health column stale after a disk operation defeats the point of
having just performed one. `PendingAction` for these three carries
`section: Section::Vdevs`, and `MutationCall`'s handling for these three
variants specifically triggers both `refresh_vdevs` and `refresh_pools`
rather than a single generic "refresh `self.section`" call — this is the
one place the dispatch isn't a uniform one-mutation-one-refresh rule, and
the implementer should not generalize it further.

## Error handling

- Mutation failures reuse `AppError::CommandFailed` exactly as `zfs::*`
  already produces it — no new error variant. Surfaced via the existing
  `status_message` / status-bar line, same as a failed refresh today.
- The status bar's known multi-line-clipping limitation (Plan 1 final
  review, Minor, still deferred) is not fixed by this plan — a long
  stderr from a failed mutation clips exactly as a long stderr from a
  failed refresh does today. Not made worse, not fixed either.
- `ConfirmTyped` mismatch and empty required form fields block submission
  client-side (Enter is a no-op); there is no "submitted but invalid"
  state requiring its own error path.

## Testing

Same approach as Plan 1: unit tests against `App` driven via
`handle_key`/`handle_modal_key` with `FakeRunner`, no ratatui
rendering/terminal tests (manual verification instead, per Plan 1 Task 11
precedent).

- Modal lifecycle: opening (action key with no modal open → correct
  `Modal` variant with expected initial state), `Esc` cancels and clears
  `app.modal` with no `FakeRunner` call recorded, `ConfirmTyped` submit
  blocked while `input != expected` (no call recorded), submit with
  matching confirmation clears the modal and records the expected call.
- One success-path test per of the 11 mutations: `WorkerMsg::MutationDone`
  with `Ok(())` triggers the expected `refresh_*` call for the right
  section/host. For the three vdev ops (offline/online/replace-disk),
  asserts both `refresh_vdevs` and `refresh_pools` are triggered (see
  Data Flow's dual-refresh exception).
- One failure-path test (shared pattern, not necessarily 11 separate
  tests): `MutationDone` with `Err` sets `status_message` and does not
  spawn a refresh.
- Stale-host discard for `MutationDone`, mirroring the existing test for
  `Pools`/`Datasets`/`Snapshots`.
- Row selection/scrolling: `TableState` selection moves and clamps
  correctly as the underlying `Vec` changes, matching Plan 1's existing
  navigation test style.
- `run_checked()` gets its own focused tests (success passthrough, failure
  maps to `CommandFailed`); existing `zfs::*` tests continue to pass
  unchanged since `run_checked`'s refactor is behavior-preserving.

Failure-path tests already added to `zfs::*` mutation functions in Plan
1's final-review fix round are not re-tested at the UI layer — only that
`App` reacts correctly to the `Result` it's handed.
