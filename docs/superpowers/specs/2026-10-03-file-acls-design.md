# zfsctl: File ACLs (NFSv4, `getfacl`/`setfacl`) — Design

## Summary

Adds an **ACL** sub-tab to the Datasets right pane (a third sub-tab
alongside Properties and Permissions — `'['`/`']'` now cycle through all
three), exposing FreeBSD's native NFSv4 access control lists on a
dataset's mountpoint: view every ACL entry in its evaluated order, append
a new entry, and remove an entry by position. This is the other half of
the original "Permissions & ACLs" idea from the detail-pane-properties
spec's Non-goals, deliberately split out and done second because it
operates on a real filesystem path (the dataset's mountpoint), not ZFS
dataset metadata — a genuinely different mechanism (`getfacl`/`setfacl`
on a path) from delegated ZFS permissions (`zfs allow`/`unallow` on a
dataset name), even though both ended up as sub-tabs in the same pane.

POSIX.1e ACLs (FreeBSD's other ACL style) are explicitly out of scope for
this pass — NFSv4 is ZFS's default `acltype` on FreeBSD (confirmed live:
every dataset checked had `acltype=nfsv4`) and the user's own choice to
scope this feature to NFSv4 first.

## Goals

- An `Acl` sub-tab in the Datasets right pane, cycled with `'['`/`']'`
  alongside Properties and Permissions, scoped to the selected dataset's
  **mountpoint** (not the dataset name — `getfacl`/`setfacl` operate on
  filesystem paths).
- View every ACL entry on that mountpoint, in the order NFSv4 evaluates
  them, each showing its position index, target (owner/group/everyone/a
  named user or group), permissions, inheritance flags, and allow/deny
  type.
- Append a new entry at the end of the list (`'n'`) or remove an entry by
  its position (`'d'`) — no in-place edit, for the same reason the
  Permissions sub-tab has none: there's no single clean "replace this
  entry" operation once flags and allow/deny type are both in play: to
  change an entry, remove it and re-add it.

## Non-goals

- **POSIX.1e ACLs** — FreeBSD supports both ACL styles depending on a
  dataset's `acltype` property, with completely different `getfacl`/
  `setfacl` entry syntax (a POSIX entry is `tag:qualifier:permissions`,
  3 fields using a 3-bit rwx mask; an NFSv4 entry is `tag[:qualifier]:
  permissions:flags:type`, 4-5 fields using named permissions and
  allow/deny semantics). Deferred to its own future pass if a dataset
  is ever found using `acltype=posix` — confirmed live that every
  dataset checked on the real test box defaults to `acltype=nfsv4`.
- **ACLs on arbitrary files/directories within a dataset** — a real file
  browser is a materially different, much larger feature (path
  navigation, recursive apply, etc.). This feature only ever touches
  one path per dataset: its mountpoint. Explicitly the user's own choice
  ("simplest" — just the mountpoint, not a file browser).
- **ACLs on zvols** — a zvol is a raw block device with no mounted
  filesystem ZFS has any visibility into (zvols don't even have a
  `mountpoint` property). If a filesystem is formatted and mounted on
  top of a zvol, that filesystem's own permission model takes over
  entirely, decoupled from ZFS — out of scope regardless. The `Acl`
  sub-tab stays reachable via `'['`/`']'` for a zvol-kind dataset (same
  always-available-and-predictable key behavior as the other sub-tabs),
  but shows a hint instead of attempting a `getfacl` call, the same
  "not applicable" pattern already used elsewhere (e.g. Permissions
  has no Pools equivalent at all, Shares' SMB rows hint instead of
  opening a form).
- **Reordering existing entries** — `setfacl -a <position>`/`-x
  <position>` support inserting/removing at an arbitrary position, but
  this feature only ever appends (`position = current entry count`) and
  removes by the selected row's own position. Moving an entry means
  removing and re-adding it in the right spot — a deliberate scope trim
  the user approved, not an oversight.
- **Default ACLs** (`getfacl -d`/`setfacl -d`) — FreeBSD's `-d` flag for
  default (inherited-on-creation) ACL entries is explicitly documented
  as "not applicable to NFSv4 ACLs" (`setfacl(1)`) — NFSv4's inheritance
  flags (`file_inherit`/`dir_inherit`/etc.) already cover this use case
  within a single ACL, so there's no separate "default ACL" concept to
  expose for this ACL type.

## Architecture

### Confirmed live against the real FreeBSD 15.1-RELEASE test box

Every dataset checked has `acltype=nfsv4` (e.g. `zfs get acltype
scratch/data zroot` → both `nfsv4`). `getfacl -v <path>` (the `-v` flag
is essential — it renders permissions/flags as named, `/`-separated
long-form tokens like `read_data/write_data/execute`, not the cryptic
fixed-width positional letter codes the non-verbose form uses, which
have undocumented column-width quirks not worth reverse-engineering when
a self-documenting form already exists) on a real mountpoint produced:

```
# file: /scratch/data
# owner: root
# group: wheel
            owner@:read_data/write_data/execute/append_data/read_attributes/write_attributes/read_xattr/write_xattr/read_acl/write_acl/write_owner/synchronize::allow
            group@:read_data/execute/read_attributes/read_xattr/read_acl/synchronize::allow
         everyone@:read_data/execute/read_attributes/read_xattr/read_acl/synchronize::allow
```

Per `setfacl(1)` (the authoritative syntax reference for both reading
and writing — `getfacl(1)` points to it), each entry line is 4 or 5
colon-separated fields: `owner@`/`group@`/`everyone@` (no qualifier —
4 fields total) or `user`/`group` (with a qualifier — 5 fields total),
then permissions, then inheritance flags, then `allow`/`deny`. Both
permissions and flags are `/`-joined when using long-form names (short
form exists too — single concatenated letters — but this app only ever
uses long-form, for both reading via `-v` and writing, so the two sides
match exactly with no translation needed). An empty flags field is two
adjacent colons (`::`), confirmed in the fixture above.

**Confirmed live, a full write/read round-trip including flags and
`deny`**:

```
# as root:
setfacl -a 0 "user:nobody:read_data/write_data:file_inherit/dir_inherit:deny" /scratch/data
getfacl -v /scratch/data
#        user:nobody:read_data/write_data:file_inherit/dir_inherit:deny    <- exact match, confirmed
#             owner@:...::allow
#             group@:...::allow
#          everyone@:...::allow
setfacl -x 0 /scratch/data   # removed entry 0 cleanly, the other 3 entries untouched
```

This confirms: long-form permissions and flags round-trip exactly
between `getfacl -v` and `setfacl`, and position-indexed insert/remove
(`-a <position>`/`-x <position>`) works as documented.

### Data model (`src/acl.rs`, new top-level module — sibling to `src/zfs/`, not nested inside it)

This module wraps a generic FreeBSD filesystem-ACL mechanism
(`getfacl`/`setfacl` on a path), not a ZFS command — it only happens to
be used on ZFS dataset mountpoints in this app, which is why it lives
alongside `zfs::`, not inside it.

```rust
pub enum AclTag {
    Owner,              // owner@
    OwningGroup,        // group@
    Everyone,           // everyone@
    User(String),       // user:<name>
    NamedGroup(String), // group:<name>
}

pub enum AclEntryType { Allow, Deny }

pub struct AclEntry {
    pub tag: AclTag,
    pub permissions: Vec<String>, // long-form names (e.g. "read_data") or a set name (e.g. "full_set")
    pub flags: Vec<String>,       // long-form names (e.g. "file_inherit"); empty means no flags
    pub entry_type: AclEntryType,
}

pub fn list_acl(runner: &dyn CommandRunner, path: &str) -> Result<Vec<AclEntry>>;
pub fn add_acl_entry(runner: &dyn CommandRunner, path: &str, position: usize, entry: &AclEntry) -> Result<()>;
pub fn remove_acl_entry(runner: &dyn CommandRunner, path: &str, position: usize) -> Result<()>;
```

`list_acl` runs `getfacl -v <path>`, skips `#`-prefixed header lines and
blank lines, and parses each remaining line by splitting on `:` into 4
or 5 fields as described above (4 fields → `owner@`/`group@`/`everyone@`
tag with no qualifier; 5 fields → `user`/`group` tag with a qualifier),
then splitting the permissions and flags fields on `/` (an empty flags
field — two adjacent colons — yields an empty `Vec`). Entries are
returned in `getfacl`'s own display order, which **is** the position
order `setfacl -a`/`-x` operate on (per `setfacl(8)`: "NFSv4 ACL entries
are evaluated in their visible order") — no separate stored index field
needed; a `Vec<AclEntry>`'s index is the position.

`add_acl_entry` builds the single colon-separated entry string (tag,
optional qualifier, `/`-joined permissions, `/`-joined flags, type) and
runs `setfacl -a <position> "<entry>" <path>`. `remove_acl_entry` runs
`setfacl -x <position> <path>`. Both confirmed live above.

### `DatasetDetailTab` grows a third variant

`DatasetDetailTab` (currently `Properties`/`Permissions`) grows `Acl`.
`'['`/`']'` currently implement a 2-way toggle (each key does the same
thing, since there were only 2 variants) — this needs to become a real
3-way cycle now (`'['` previous, `']'` next, wrapping). New `App` fields
mirroring the Permissions sub-tab's exactly: `acl_entries: Vec<AclEntry>`,
`acl_entries_table: TableState`, `acl_entries_for: Option<String>` (the
mountpoint path the current data is for — same stale-identity-discard
role as `dataset_permissions_for`). Entering the `Acl` tab fetches via a
new `WorkerMsg::DatasetAcl(usize, String, Result<Vec<AclEntry>>)` (the
`String` is the mountpoint path, for the same stale-discard pattern every
other identity-scoped refresh uses) and a `refresh_dataset_acl` method
following the established `begin_refresh`/`is_refreshing`/
`pending_refresh` pattern.

**Resolving the path**: the selected row in the Datasets middle pane
already carries `mountpoint: Option<String>` (an existing field). If
`None` (zvol, or an unmounted filesystem), the `Acl` tab shows a status
hint ("no mountpoint to show ACLs for") instead of fetching — the fetch
trigger (entering the tab, or `'r'`) checks this before calling
`refresh_dataset_acl` at all, mirroring how Permissions' `'N'` key
already checks `shares_loaded` before proceeding.

### Table rendering

Columns `# | Target | Permissions | Flags | Type` — e.g. `0 | owner@ |
read_data/write_data/execute/... | — | Allow`, `1 | user nobody |
read_data/write_data | file_inherit/dir_inherit | Deny`. `#` is the
`Vec`'s index (= the `setfacl` position). `Target` renders the tag:
`"owner@"`/`"group@"`/`"everyone@"` literally, or `"user <name>"`/`"group
<name>"` for the named forms (matching the Permissions sub-tab's
`"user fb"`/`"group wheel"` rendering convention exactly).

## Mutations

```rust
MutationCall::AddAclEntry { path: String, position: usize, entry: AclEntry }
MutationCall::RemoveAclEntry { path: String, position: usize }

FormKind::AddAclEntry { host: usize, path: String, position: usize }
// fields: [Recipient, Permissions, Flags, Type]
```

`'n'` always appends (`position = self.acl_entries.len()` at the moment
the key is pressed) — no position field in the create form, per the
Non-goals' reordering trim. `'d'` reads the position directly from the
selected row's index in `acl_entries` — Confirm modal, no form, same
shape as Permissions' full-revoke.

### Field validation in `build_pending_action`

- **`Recipient`** must parse as exactly `owner` / `group` / `everyone` /
  `user:<name>` / `group:<name>` (five shapes — one more than
  Permissions' three, since ACLs distinguish the owning group `group@`
  from the file's actual owner/group differently) or the form stays
  open.
- **`Permissions`** is **required** — split on `,`, trimmed, empty
  tokens dropped, `/`-joined for the actual `setfacl` call; blank (or
  only commas/whitespace) keeps the form open, same reasoning as
  Permissions' field: there's no sensible default for "which
  permissions."
- **`Flags`** is the one genuinely optional field — blank means no
  inheritance flags (a real, valid, common case — e.g. the 3 default
  entries on every dataset checked have no flags at all). Parsed the
  same way as `Permissions` (comma-split, `/`-joined) when non-blank.
- **`Type`** has a default for blank (`allow`, the more common case) but
  is otherwise *strict*: only `allow`/`deny` (case-insensitive) are
  accepted, any other non-blank text keeps the form open rather than
  silently defaulting to the less-restrictive `allow` on a typo — same
  footgun-avoidance reasoning as Permissions' `Scope` field defaulting
  strictly rather than leniently.

## Components / files touched

- New: `src/acl.rs` (`AclTag`, `AclEntryType`, `AclEntry`, `list_acl`,
  `add_acl_entry`, `remove_acl_entry`, the parser/assembler).
- `src/main.rs`: add `mod acl;` alongside the existing `mod config;` /
  `mod error;` / `mod exec;` / `mod hosts;` / `mod ui;` / `mod zfs;`
  lines.
- `src/ui/modal.rs`: `FormKind::AddAclEntry`; `MutationCall::
  AddAclEntry`/`RemoveAclEntry`.
- `src/ui/state.rs`: `DatasetDetailTab::Acl` (and the 2-way-toggle →
  3-way-cycle change to the existing `'['`/`']'` arms); `App` fields
  (`acl_entries`, `acl_entries_table`, `acl_entries_for`);
  `WorkerMsg::DatasetAcl`; `refresh_dataset_acl`; `handle_key` arms
  (`'n'`/`'d'`/`'r'` within the `Acl` sub-tab, guarded on
  `mountpoint.is_some()`); `build_pending_action` arm for the new
  `FormKind`.
- `src/ui/layout.rs`: `Acl` sub-tab table rendering, sub-tab indicator.
- `src/ui/mod.rs`: `'r'`-key and queued-refresh dispatch, and the
  `'['`/`']'` refresh-on-tab-switch block, extended for the third tab.
- `README.md`: keybindings.

## Testing

- `acl.rs`: fixture-based parser tests for `list_acl` — a 4-field
  `owner@`/`group@`/`everyone@` entry, a 5-field `user`/`group` entry,
  an empty-flags entry (`::allow`), a multi-flag entry, and a `deny`
  entry — using the exact confirmed real output shapes above.
  Command-building tests for `add_acl_entry`/`remove_acl_entry`
  asserting the exact `setfacl` args, confirmed against the live
  round-trip (the `user:nobody:read_data/write_data:file_inherit/
  dir_inherit:deny` example).
- `state.rs`: the `'['`/`']'` 3-way cycle (including wrapping both
  directions); `build_pending_action` tests for `FormKind::
  AddAclEntry` (blank-`Permissions` rejection, all 5 `Recipient` shapes
  — 4 valid + 1 invalid, blank `Flags` accepted, strict `Type`
  parsing — blank→`allow`, `allow`/`deny` accepted case-insensitively,
  garbage keeps the form open); `'n'`/`'d'` with no dataset selected or
  no row selected do nothing, don't panic; a dataset with
  `mountpoint: None` shows the hint instead of fetching;
  `refresh_dataset_acl` populates `App` state from a `FakeRunner` and
  discards a stale result, mirroring `refresh_dataset_permissions`'s
  precedent exactly.

No ratatui rendering tests, consistent with every prior plan.
