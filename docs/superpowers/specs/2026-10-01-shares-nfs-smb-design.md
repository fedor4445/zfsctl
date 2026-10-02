# zfsctl: Shares (NFS/SMB) — Design

## Summary

Adds a new top-level **Shares** section exposing ZFS's own `sharenfs`/
`sharesmb` dataset properties as structured, friendly forms instead of
opaque property strings — networks, hosts, read-only vs read-write,
and root-user mapping for NFS; a share name for SMB. Every mutation is
just a richer way to build a `zpool`/`zfs set` value, reusing the
existing `MutationCall::SetDatasetProperty` call with no new mutation
plumbing. Detects whether `nfsd`/Samba are actually running on the
host and disables the corresponding create action with a clear hint
when they aren't, rather than erroring.

## Goals

- A `Section::Shares` (Tab-cycled, sibling to Pools/Datasets/Disks)
  listing every currently active NFS/SMB share across all datasets on
  the host, one row per share (a dataset shared via both protocols
  shows as two rows) — edit or remove a share from here.
- `'N'`/`'M'` keys in the **Datasets** section (paralleling the
  existing `'s'`-creates-a-snapshot-from-Datasets convention) open the
  same structured create/edit forms the Shares section's `Enter` uses,
  pre-filled from the dataset's current share state if it's already
  shared — one form pair serves both creation and editing.
- NFS gets a full structured form now: Networks, Hosts, read-only vs
  read-write, and root-user mapping — assembled into a real
  OpenZFS-native `sharenfs` value (confirmed syntax, see Architecture).
- SMB gets on/off + an optional share name only, for this feature —
  genuine Samba permission control lives in `smb4.conf`, a file ZFS
  never touches, and is explicitly out of scope here (see Non-goals).
- Read-only detection of whether `nfsd`/Samba are installed and
  running, surfaced as a disabled-action hint — never starts, stops,
  or reconfigures either system service.

## Non-goals

- Samba permission/ACL editing via `smb4.conf` (valid users, guest
  access, read-only per-share beyond the simple on/off toggle, etc.) —
  a genuinely separate subsystem (parsing/writing a config file ZFS
  doesn't know about, reloading the Samba service), deferred to its
  own future feature once this one is proven.
- Enabling/starting `nfsd` or Samba if they're installed but not
  running, or installing Samba if it's missing entirely — this stays a
  ZFS-focused tool; the user enables system services themselves.
- A general config-file editor for `/etc/exports` entries created
  outside of ZFS's `sharenfs` mechanism (e.g. hand-written NFS exports
  for non-ZFS paths) — out of scope, this feature only manages shares
  through ZFS dataset properties.
- Full raw `exports(5)` syntax coverage in the structured NFS form —
  `sharenfs` actually accepts OpenZFS's own simpler cross-platform
  mini-language (confirmed live: `rw=@172.16.10.0/24,root=nobody`,
  matching `zfs(8)`'s own documented example), not literal
  dash-prefixed `exports(5)` flags; the common fields (networks, hosts,
  ro/rw, root mapping) are supported, anything the parser doesn't
  recognize in an existing value still displays (via a kept raw
  fallback) rather than failing, and remains editable through the
  existing generic property-edit form as a raw-text escape hatch.

## Architecture

### Data model (`src/zfs/share.rs`, new module)

```rust
pub enum Share {
    Nfs {
        dataset: String,
        raw: String,
        networks: Vec<String>,
        hosts: Vec<String>,
        read_only: bool,
        root: Option<String>,
    },
    Smb {
        dataset: String,
        name: Option<String>,
    },
}

pub struct SharesSnapshot {
    pub shares: Vec<Share>,
    pub nfs_available: bool,
    pub samba_available: bool,
}

pub fn list_shares(runner: &dyn CommandRunner) -> Result<Vec<Share>>;
pub fn nfs_server_available(runner: &dyn CommandRunner) -> Result<bool>;
pub fn samba_available(runner: &dyn CommandRunner) -> Result<bool>;
pub fn list_shares_with_availability(runner: &dyn CommandRunner) -> Result<SharesSnapshot>;
```

`list_shares` runs `zfs get -H -p sharenfs,sharesmb` with **no dataset
argument** — this lists every dataset on the host in one call, the
same tab-separated shape every other `get` call in this app already
parses, just keeping the leading `name` column this time (every other
parser in the app discards it, since those call sites already know
which single dataset/pool they asked about — this is the first parser
that needs it). Datasets where both properties are `off` are dropped
entirely. A `sharenfs` value is parsed on a best-effort basis into the
structured fields; `raw` is always kept alongside so a value the
parser doesn't fully recognize still displays (unparsed) instead of
the row disappearing or an error surfacing. `sharesmb`'s value is
either `off`, `on`, or a custom share name — no parsing ambiguity.

**`sharenfs` syntax, confirmed live** (not the raw dash-prefixed
`exports(5)` flag syntax — OpenZFS's own simpler cross-platform
mini-language, matching `zfs(8)`'s own documented example):
comma-separated clauses, no dashes. `rw=<list>` or `ro=<list>` restrict
access to specific networks/hosts (`@` prefixes a network/CIDR, e.g.
`@172.16.10.0/24`; a bare name/IP is a specific host; multiple entries
in one clause are colon-joined, e.g. `rw=@172.16.10.0/24:somehost`); a
bare `rw`/`ro` with no `=list` applies to everyone. `root=<user>` maps
root's remote credential to `<user>` (confirmed: `zfs set
sharenfs='rw=@172.16.10.0/24,root=nobody' scratch/data` round-tripped
through `zfs get` unchanged and produced the expected
`/scratch/data rw=@172.16.10.0/24 root=nobody` line in
`/etc/zfs/exports`, which ZFS manages automatically — the app never
touches that file or runs `zfs share`/`exportfs` itself, only `zfs
set`). No `mapall=`-equivalent keyword is documented anywhere in
`zfs(8)`/`zfsprops(5)` for this syntax (unlike raw `exports(5)`, which
has both `-maproot=`/`-mapall=`) — so the structured form has exactly
one root-mapping field (`root=`), not two.

`nfs_server_available` runs `service nfsd onestatus` — exit code alone
is reliable (confirmed against the real test box: exit 0 while
running, nonzero otherwise). `samba_available` runs `service
samba_server onestatus` and checks the **output text** for `"smbd is
running"`, **not** the exit code — confirmed against the real test box
that this specific rc.d meta-service's exit code reflects the combined
status of `smbd`, `nmbd`, *and* `winbindd` (the AD/domain-integration
daemon), so a host with `winbindd` stopped but `smbd` serving shares
fine reports a nonzero exit code from `onestatus` despite SMB sharing
being fully functional. Checking for `smbd` specifically in the output
avoids a false "unavailable" reading in that real, observed case.

`list_shares_with_availability` composes all three calls into one
`SharesSnapshot`, mirroring the `zfs::disk::list_disks_with_membership`
precedent exactly (avoids extra round-trips, one `WorkerMsg` covers a
full refresh).

### `Section::Shares`

Added to `App::sections` (the Tab-cycled list), like every other
top-level section. New `App` fields: `shares: Vec<Share>`,
`shares_table: TableState`, `nfs_available: Option<bool>`,
`samba_available: Option<bool>` — `None` means "not yet checked", kept
distinct from `Some(false)` ("checked, unavailable") so the Shares
title doesn't flash a false "unavailable" hint before the first
refresh, consistent with how every other section stays quiet (an
empty list) rather than asserting a default before it's actually
fetched anything.

### Data flow: refresh

`WorkerMsg::Shares(usize, Result<SharesSnapshot>)` — host-scoped, like
`Pools`/`Datasets`/`Disks` (no per-entity staleness tag needed, this
isn't tied to one selected row). `App::refresh_shares` follows the
same `begin_refresh`/`is_refreshing`/`pending_refresh` pattern every
other refresh method uses, its thread body a single call to
`list_shares_with_availability`.

Refresh triggers, matching the established convention (no
auto-refresh-on-Tab, same as Datasets/Snapshots/Disks): `'r'` while on
`Section::Shares`, and automatically after any share mutation succeeds
— free, since a share mutation's `PendingAction` is tagged `section:
Section::Shares` and the existing generic `refreshes_needed.push(section)`
in `drain_worker_messages` already re-triggers the right section's
refresh with no special-casing needed.

### UX and forms

**Shares section**: table columns `Dataset | Protocol | Details`
(networks/hosts/ro-rw summary for NFS rows, share name for SMB rows).
`Up`/`Down` navigate. `Enter` opens the matching structured form,
pre-filled from the row's current (parsed) values. `'d'` opens
`Modal::Confirm` to remove the share (sets the relevant property back
to `off`). `'r'` refreshes. The title shows a hint for any unavailable
protocol, e.g. `"Shares — local ('r' refresh, Enter edit, 'd' remove)
[SMB unavailable: Samba not running]"`.

**Creating a share**: new keys in the **Datasets** section, guarded by
`pane_focus == PaneFocus::Middle` (not drilled into anything — Datasets
has no drill-down):

- `'N'` — share the selected dataset via NFS. Opens
  `FormKind::ShareNfs { host, dataset }`, fields `[Networks, Hosts,
  Read-only (y/n), Root user mapping]`, pre-filled from the
  dataset's current `sharenfs` value's parsed fields if already shared,
  blank otherwise.
- `'M'` — share the selected dataset via SMB. Opens `FormKind::ShareSmb
  { host, dataset }`, fields `[Share name (blank = default)]`, same
  pre-fill convention.
- If the relevant service isn't available (`nfs_available`/
  `samba_available` is `Some(false)`), the key sets a status-message
  hint instead of opening a form — same pattern as the Disks section's
  "select at least one disk with Space first" hint.

Both keys open the **same** `FormKind`s the Shares section's `Enter`
uses — one form pair serves both creation and editing, no duplicated
forms or logic.

### Mutations

No new `MutationCall` variants — every share mutation is a
`MutationCall::SetDatasetProperty { dataset, key, value }` call,
reusing the existing library function and its `--`-guard entirely
unchanged.

- `FormKind::ShareNfs`'s fields assemble into a `sharenfs` value using
  the confirmed syntax above. Every field left blank assembles to plain
  `"on"` (share to everyone, read-write, no root-mapping restriction),
  matching `sharesmb`'s all-blank convention. Otherwise: the
  read-only/read-write clause is a bare `rw`/`ro` if both Networks and
  Hosts are blank (restriction-free, matching the confirmed bare-`rw`
  default), or `rw=<list>`/`ro=<list>` with each network prefixed `@`
  and colon-joined together with any hosts if either field is filled
  in; `,root=<user>` is appended if the Root user mapping field is
  filled in. E.g. Networks=`172.16.10.0/24`, Hosts=blank,
  Read-only=no, Root=`nobody` assembles to
  `rw=@172.16.10.0/24,root=nobody` — the exact value confirmed live
  above.
- `FormKind::ShareSmb`'s single field assembles into a `sharesmb`
  value: blank → `"on"`, otherwise the given name.
- Removing a share (`'d'` in the Shares section) reuses the existing
  `Modal::Confirm` pattern directly — no new `FormKind` — setting the
  row's relevant property (`sharenfs` or `sharesmb`, whichever that
  row's protocol is) to `"off"`.
- Unlike every other creation form in this app so far, **no field in
  either share form is required** — every field may legitimately be
  blank (blank Networks/Hosts means "share to everyone"; blank SMB name
  means "use the default name"), so neither `build_pending_action` arm
  needs an empty-field rejection check.

## Components / files touched

- New: `src/zfs/share.rs` (`Share`, `SharesSnapshot`, `list_shares`,
  `nfs_server_available`, `samba_available`,
  `list_shares_with_availability`, the `sharenfs`-syntax mini
  parser/assembler).
- `src/zfs/mod.rs`: register the new module.
- `src/ui/modal.rs`: `FormKind::ShareNfs`, `FormKind::ShareSmb`.
- `src/ui/state.rs`: `Section::Shares`; `App` fields (`shares`,
  `shares_table`, `nfs_available`, `samba_available`); `WorkerMsg::Shares`;
  `refresh_shares`; `handle_key` arms (Up/Down/Enter/`'d'`/`'r'` in
  Shares, `'N'`/`'M'` in Datasets); `build_pending_action` arms for the
  two new `FormKind`s.
- `src/ui/layout.rs`: Shares table rendering, title hint, sidebar label.
- `src/ui/mod.rs`: `'r'`-key and queued-refresh dispatch arms for
  `Section::Shares`.
- `README.md`: keybindings.

## Testing

- `share.rs`: fixture-based parser tests for `list_shares` (a `zfs get`
  fixture mixing NFS-only, SMB-only, both, and neither-shared datasets
  — the neither-shared one must not appear in the result; one dataset
  with a `sharenfs` value the structured parser doesn't fully
  recognize, proving it still displays via the kept `raw` field rather
  than failing or vanishing). Two targeted availability tests: `samba_available`
  is `true` when `smbd` is up even though the overall `onestatus` exit
  code is nonzero (the `winbindd` case found above), and `false` when
  `smbd` itself isn't running. `list_shares_with_availability`'s
  three-call composition tested with `FakeRunner::route`, mirroring
  `list_disks_with_membership`'s precedent. A parse→assemble round-trip
  test: parsing an existing `sharenfs` value into structured fields and
  reassembling it must stay semantically consistent — the whole point
  of editing an opaque `sharenfs` value through friendly fields.
- `state.rs`: `build_pending_action` tests for both new `FormKind`s
  (assembly correctness only — no empty-field-rejection test needed,
  since every field is optional for both forms); `'N'`/`'M'` from
  Datasets open the right form pre-filled correctly (blank when
  unshared, parsed-from-existing when already shared) and hint instead
  when the relevant service is unavailable; `Enter`/`'d'` in the Shares
  section open the right edit/remove flow; `refresh_shares` populates
  `App` state from a `FakeRunner`, matching the established refresh-test
  pattern.

No ratatui rendering tests, consistent with every prior plan.
