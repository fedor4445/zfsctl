# zfsctl: Delegated ZFS Permissions (`zfs allow`/`unallow`) — Design

## Summary

Adds a **Permissions** sub-tab to the Datasets right pane — the first time
that pane has more than one view (it was previously Properties-only) —
exposing `zfs allow`'s delegated administration permissions: who (user,
group, or everyone) can run which ZFS subcommands/properties on a dataset,
permission sets (named, reusable bundles of permissions), and "create time"
permissions (auto-granted to whoever creates a new child dataset). Full
view/grant/revoke, including partial removal of a single permission from an
existing grant without revoking the whole thing. File ACLs
(`getfacl`/`setfacl` — a different, unrelated subsystem operating on actual
files/directories rather than dataset metadata) are explicitly out of scope
here, deferred to their own future feature.

## Goals

- A `Permissions` sub-tab in the Datasets right pane (new `DatasetDetailTab`
  enum: `Properties` | `Permissions`), cycled with `'['`/`']'`, scoped to
  whichever dataset is selected in the middle pane — mirrors how the
  Properties pane already works, just with a second view to switch to.
- View every permission delegated on the selected dataset: permission sets,
  create-time permissions, and grants (to a user, group, or everyone),
  each tagged with its scope (local / descendent / local+descendent).
- Grant permissions to a user/group/everyone (`'n'`), define or add to a
  named permission set (`'s'`), or add a create-time permission (`'c'`) —
  all three are the same underlying operation (`zfs allow`, which always
  *merges* with whatever's already delegated — confirmed live, see
  Architecture) with different targets.
- Revoke an entire delegation (`'d'`) or just specific permissions from one
  (`'x'`) — both map directly to `zfs unallow`.

## Non-goals

- File ACLs (`getfacl`/`setfacl`) — operates on real files/directories
  under a dataset's mountpoint, not dataset metadata; has its own
  path-selection UX question (which file/dir?) that delegated permissions
  don't; a genuinely separate subsystem. Deferred to its own future spec.
- Pool-level delegation — `zfs allow` only targets a filesystem or volume
  (confirmed: `zfs-allow(8)`'s synopsis says `filesystem|volume`, never
  `pool`), so this sub-tab only ever appears for Datasets, never Pools.
- A true "replace this grant's permission list" edit — `zfs allow`/
  `unallow` are purely additive/subtractive (confirmed live: granting the
  same user two different permissions in two separate calls merges into
  one combined grant; `zfs allow -s @t rollback` after `zfs allow -s @t
  snapshot` grew the set to `rollback,snapshot` rather than replacing it).
  "Editing" a grant to an arbitrary new list would require a diff
  (unallow the removed ones, allow the added ones) that isn't how the
  underlying tool thinks about state. Instead: `'n'`/`'s'`/`'c'` always
  *add*, `'x'` removes specific permissions, `'d'` removes everything for
  that target — three honest primitives instead of one that fakes a
  replace.
- A permission-name vocabulary allowlist — the ~50 documented permission
  names (confirmed via `zfs-allow(8)`) could be validated client-side, but
  that list needs maintenance as ZFS adds properties/subcommands over
  versions. Instead, reuse the exact existing guard pattern from
  `zfs::snapshot`'s `is_safe_zfs_target`/`is_safe_host` (reject a token
  starting with `-`) — this is the actual structural risk (the perm list
  is a bare positional argument in `zfs allow`'s synopsis, unlike `zfs
  get`/`set`'s flag-based arguments), needs no vocabulary to maintain, and
  an invalid-but-safe permission name still just fails with `zfs`'s own
  clear error, same as every other creation form in this app.

## Architecture

### `zfs allow` output format — confirmed live against the real test box

No machine-readable mode exists for this command (unlike `zfs get -H -p`) —
confirmed via `zfs-allow(8)`'s synopsis and live testing. `zfs allow
<dataset>` with nothing delegated prints **nothing at all** (confirmed: not
even a banner line). With delegations present, the format is:

```
---- Permissions on scratch/data -------------------------------------
Permission sets:
        @admin clone,rollback,snapshot
Create time permissions:
        destroy
Local permissions:
        group wheel create,destroy
Descendent permissions:
        everyone mount
Local+Descendent permissions:
        user fb @admin,snapshot
```

Confirmed details that drove the parser design:

- The banner line (`---- Permissions on ... ----`) is skipped; everything
  else is a header line (no leading whitespace, ends with `:`, one of
  exactly 5 literal strings) followed by zero or more indented entry
  lines, until the next header or end of output. No blank lines between
  sections (confirmed).
- **`Permission sets:`** entries are `@name <comma-list>`.
- **`Create time permissions:`** entries are a bare `<comma-list>` — no
  recipient, since `-c` permissions aren't granted *to* anyone, they're
  auto-granted to whoever creates a new child dataset. (The
  `zfs-allow(8)` man page's own example for this is wrong — it shows `-c`
  output under a `Permission sets:` header with no `@` prefix, which
  doesn't match reality; confirmed live that the real header is `Create
  time permissions:`, a 5th distinct section, not a `Permission sets:`
  entry. This project's established practice of verifying CLI behavior
  empirically rather than trusting docs caught a real upstream man-page
  bug here.)
- **`Local permissions:`** / **`Descendent permissions:`** /
  **`Local+Descendent permissions:`** entries are `user <name>
  <comma-list>` / `group <name> <comma-list>` / `everyone <comma-list>`
  (no name token for `everyone`).
- Comma-lists are always alphabetically sorted by ZFS itself regardless of
  grant order (confirmed: granting `snapshot` then later `@admin` to the
  same user displays as `@admin,snapshot`, not grant order) and may mix
  bare permission names with `@name` set references in the same list.
- `zfs allow`/`unallow` are purely additive/subtractive — re-running
  `zfs allow -s @t rollback` against a set that already had `snapshot`
  grew it to `rollback,snapshot` rather than replacing it (confirmed
  live). `zfs unallow` with no permission list removes the whole target;
  with a permission list, removes just those.

### Data model (`src/zfs/permission.rs`, new module)

```rust
pub enum Recipient { User(String), Group(String), Everyone }
pub enum Scope { Local, Descendent, Both }

pub enum PermissionTarget {
    Grant { recipient: Recipient, scope: Scope },
    Set(String),      // set name, without the leading @
    CreateTime,
}

pub struct PermissionGrant { pub recipient: Recipient, pub scope: Scope, pub perms: Vec<String> }
pub struct PermissionSet { pub name: String, pub perms: Vec<String> }

pub struct PermissionsSnapshot {
    pub sets: Vec<PermissionSet>,
    pub create_time: Vec<String>,
    pub grants: Vec<PermissionGrant>,
}

pub fn list_permissions(runner: &dyn CommandRunner, dataset: &str) -> Result<PermissionsSnapshot>;
pub fn allow(runner: &dyn CommandRunner, dataset: &str, target: &PermissionTarget, perms: &[String]) -> Result<()>;
pub fn unallow(runner: &dyn CommandRunner, dataset: &str, target: &PermissionTarget, perms: Option<&[String]>) -> Result<()>;
```

`list_permissions` runs `zfs allow <dataset>` and parses the format above.
Unlike `zfs get`, there's no dataset-type filter to apply or need for
one — this command always takes exactly one dataset and only makes sense
for a filesystem/volume (the UI never calls it for anything else, since
it's only reachable by drilling into a selected Datasets row).

`allow`/`unallow` build the flag combination from `PermissionTarget`,
confirmed against the live probes above:

| Target | Scope | Flags |
|---|---|---|
| `Grant{User(name), Local}` | — | `-l -u <name>` |
| `Grant{User(name), Descendent}` | — | `-d -u <name>` |
| `Grant{User(name), Both}` | — | `-u <name>` (neither `-l` nor `-d`) |
| `Grant{Group(name), scope}` | — | same as `User`, with `-g` |
| `Grant{Everyone, scope}` | — | bare `everyone` token, no `-u`/`-g` (confirmed: `zfs allow -d everyone mount scratch/data` worked) |
| `Set(name)` | n/a | `-s @<name>` (no `-l`/`-d` — not applicable to sets) |
| `CreateTime` | n/a | `-c` (no `-l`/`-d`) |

`unallow` mirrors this exactly, with the permission-list argument omitted
entirely when `perms` is `None` (confirmed: this is what fully removes a
target — e.g. `zfs unallow -s @admin scratch/data` deleted the whole set).

Every caller-supplied permission token is checked with a new guard
function (same shape and rationale as `zfs::snapshot`'s
`is_safe_zfs_target`/`is_safe_host`): reject any token starting with `-`
before it's ever passed to `allow`/`unallow`, since the permission list is
a bare positional argument in `zfs allow`'s own synopsis (unlike `zfs get`/
`set`'s flag-value arguments), not because this app treats its own
operator as an adversary.

Always pass explicit `-u`/`-g` (never the bare-name heuristic the man page
also supports) — per `zfs-allow(8)` itself: "to specify a user or group
named `everyone`, use the `-g`/`-u` options," so the explicit form is
strictly safer and removes any ambiguity, at no extra cost.

### `DatasetDetailTab` and the Permissions sub-tab

New enum `DatasetDetailTab { Properties, Permissions }`, new `App` field
`dataset_detail_tab: DatasetDetailTab` (default `Properties`). New keys
`'['`/`']'` cycle it, guarded to `pane_focus == PaneFocus::Right &&
current_section() == Section::Datasets` — a no-op everywhere else,
including the Pools right pane (which has exactly one view and stays
that way; this plan adds no sub-tab plumbing there).

New `WorkerMsg::DatasetPermissions(usize, String, Result<PermissionsSnapshot>)`
— host- and dataset-name-scoped, mirroring `DatasetProperties`'s existing
stale-identity discard pattern exactly (a result for a dataset that's no
longer selected is dropped). Fetch triggers: entering the Permissions
sub-tab for the first time for the current dataset, switching to it while
already in Right focus, or changing the selected dataset while it's
already showing — same "fetch on demand, not on every scroll" principle
Properties already established.

New `App` fields: `dataset_permissions: Vec<PermissionRow>` (parsed and
immediately **flattened** into one display/selection list when the
`WorkerMsg` arrives — no separate `PermissionsSnapshot` stored on `App`),
`dataset_permissions_table: TableState`, `dataset_permissions_for:
Option<String>` (the dataset name the current data is for, mirroring
`dataset_properties_for`'s stale-discard role).

```rust
enum PermissionRow {
    Set { name: String, perms: Vec<String> },
    CreateTime { perms: Vec<String> },
    Grant { recipient: Recipient, scope: Scope, perms: Vec<String> },
}
```

Table columns: `Target | Scope | Permissions` — e.g. `"Set @admin" | "—" |
"clone,rollback,snapshot"`, `"Create-time" | "—" | "destroy"`, `"user fb" |
"Local+Descendent" | "@admin,snapshot"`.

## Mutations

Two new `MutationCall` variants cover every case:

```rust
MutationCall::AllowPermission { dataset: String, target: PermissionTarget, perms: Vec<String> }
MutationCall::UnallowPermission { dataset: String, target: PermissionTarget, perms: Option<Vec<String>> }
```

Four new `FormKind` variants:

- `GrantPermission { host, dataset }` — fields `[Recipient, Scope,
  Permissions]` → `AllowPermission` with `Grant`. `'n'` in the Permissions
  sub-tab.
- `GrantPermissionSet { host, dataset }` — fields `[Set name,
  Permissions]` → `AllowPermission` with `Set`. `'s'`.
- `GrantCreateTimePermission { host, dataset }` — fields `[Permissions]`
  → `AllowPermission` with `CreateTime`. `'c'`.
- `RevokePermissions { host, dataset, target }` — `target` is read from
  the currently-selected `PermissionRow` at key-press time, not typed;
  one field `[Permissions to remove]` → `UnallowPermission` with
  `Some(perms)`. `'x'` on a selected row.

`'d'` on a selected row needs no form — straight to `Modal::Confirm`
(same pattern as Shares' `'d'`), `target` again read from the selected
row, `perms: None` (full revoke).

### Field validation in `build_pending_action`

- **`Permissions`** is **required** in all three grant forms and the
  revoke-specific form — unlike Shares' all-optional fields, there's no
  sensible default for "which permissions" (an empty `zfs allow` perm
  list is meaningless, not a valid "share to everyone"-style default).
  Blank keeps the form open, same as `CreatePool`'s name field. The field
  is split on `,`, each token trimmed, and empty tokens dropped (same as
  Shares' `networks`/`hosts` parsing) — if that leaves zero tokens (e.g.
  the field was just commas/whitespace), it's treated as blank and the
  form stays open, same as if it had been empty to begin with. Each
  remaining token is checked with the `-`-prefix guard above; any token
  starting with `-` also keeps the form open (treated the same as a
  missing required field — this app has no existing inline-validation-
  error UI, so an invalid token is surfaced the same way a blank required
  field already is: the form just stays open).
- **`Recipient`** must parse as exactly `everyone`, `user:<name>`, or
  `group:<name>` (a small mini-language, same spirit as
  `assemble_sharenfs`'s `rw=@net:host`) — anything else keeps the form
  open. No bare-name fallback (see "always pass explicit `-u`/`-g`"
  above).
- **`Scope`** is the one field with a default for blank (`Both`), but is
  otherwise *strict*: only `local`/`descendent`/`both` (case-insensitive)
  are accepted; any other non-blank text keeps the form open rather than
  silently falling back to `Both`. Silently defaulting a typo to the
  *widest* scope would be a real footgun specific to a permissions
  feature, so this one field is deliberately less forgiving than the rest
  of this app's forms.

## Components / files touched

- New: `src/zfs/permission.rs` (`Recipient`, `Scope`, `PermissionTarget`,
  `PermissionGrant`, `PermissionSet`, `PermissionsSnapshot`,
  `list_permissions`, `allow`, `unallow`, the output parser, the
  `-`-prefix guard).
- `src/zfs/mod.rs`: register the new module.
- `src/ui/modal.rs`: `FormKind::GrantPermission`/`GrantPermissionSet`/
  `GrantCreateTimePermission`/`RevokePermissions`.
- `src/ui/state.rs`: `DatasetDetailTab`; `App` fields
  (`dataset_detail_tab`, `dataset_permissions`,
  `dataset_permissions_table`, `dataset_permissions_for`);
  `PermissionRow`; `WorkerMsg::DatasetPermissions`; a
  `refresh_dataset_permissions` method mirroring
  `refresh_dataset_properties`; `handle_key` arms (`'['`/`']'` sub-tab
  cycle; `Up`/`Down`/`'n'`/`'s'`/`'c'`/`'d'`/`'x'`/`'r'` within the
  Permissions sub-tab); `build_pending_action` arms for the four new
  `FormKind`s.
- `src/ui/layout.rs`: Permissions sub-tab table rendering, sub-tab
  indicator in the right pane's title/border.
- `src/ui/mod.rs`: `'r'`-key and queued-refresh dispatch arm for the
  Permissions sub-tab (mirroring how Properties' refresh dispatch works).
- `README.md`: keybindings.

## Testing

- `permission.rs`: fixture-based parser tests for `list_permissions` —
  the empty-output case, and a fixture covering all 5 real section
  headers at once (using the exact confirmed format above, including a
  `user`/`group`/`everyone` mix and a set reference mixed into a grant's
  perm list, e.g. `@admin,snapshot`). Command-building tests for
  `allow`/`unallow` across all three `PermissionTarget` kinds × all three
  scopes, asserting the exact flag combination each produces (see the
  table above). A guard test mirroring `is_safe_zfs_target`'s tests,
  rejecting a permission token starting with `-`.
- `state.rs`: `'['`/`']'` sub-tab cycling works in Datasets-Right-focus
  and no-ops in Pools-Right-focus and everywhere else;
  `WorkerMsg::DatasetPermissions` populates `dataset_permissions` (as the
  flattened `Vec<PermissionRow>`) and discards a stale result (dataset
  changed mid-fetch), mirroring the existing `dataset_properties`
  precedent exactly; `build_pending_action` tests for all 4 `FormKind`s
  (blank-`Permissions` rejection, a `-`-prefixed permission token
  rejected, `Recipient` parse success/failure for all 3 recipient
  shapes, strict `Scope` parsing — blank→`Both`, `"local"`/`"descendent"`/
  `"both"` accepted case-insensitively, garbage keeps the form open);
  `'n'`/`'s'`/`'c'` open the right form; `'d'`/`'x'` correctly read the
  target back out of each `PermissionRow` variant (`Set`, `CreateTime`,
  `Grant`).

No ratatui rendering tests, consistent with every prior plan.
