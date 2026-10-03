# zfsctl: Shares (NFS/SMB) — Design

## Summary

Adds a new top-level **Shares** section exposing ZFS's own `sharenfs`/
`sharesmb` dataset properties as a structured, friendly view instead of
opaque property strings — networks, hosts, read-only vs read-write, and
root-user mapping for NFS. Every NFS mutation is just a richer way to
build a `zpool`/`zfs set` value, reusing the existing
`MutationCall::SetDatasetProperty` call with no new mutation plumbing.
Detects whether `nfsd` is actually running on the host and disables NFS
share creation with a clear hint when it isn't, rather than erroring.
**SMB share creation/editing is not implemented** — confirmed live
against the real test box that `zfs set sharesmb=on` does not actually
work on FreeBSD (see Non-goals) — but an already-existing SMB share
(created some other way, or inherited from before this discovery) can
still be viewed and removed from the Shares section.

## Goals

- A `Section::Shares` (Tab-cycled, sibling to Pools/Datasets/Disks)
  listing every currently active NFS/SMB share across all datasets on
  the host, one row per share (a dataset shared via both protocols
  shows as two rows) — edit (NFS) or remove (NFS or SMB) a share from
  here.
- A `'N'` key in the **Datasets** section (paralleling the existing
  `'s'`-creates-a-snapshot-from-Datasets convention) opens the same
  structured create/edit form the Shares section's `Enter` uses for NFS
  rows, pre-filled from the dataset's current share state if it's
  already shared — one form serves both creation and editing.
- NFS gets a full structured form: Networks, Hosts, read-only vs
  read-write, and root-user mapping — assembled into a real
  OpenZFS-native `sharenfs` value (confirmed syntax, see Architecture).
- Read-only detection of whether `nfsd` is installed and running,
  surfaced as a disabled-action hint on the `'N'` key — never starts,
  stops, or reconfigures the service.
- An existing SMB share (however it came to be `on`) is listed, its
  dataset and share name shown, and removable via `'d'` — the one SMB
  capability this feature actually delivers.

## Non-goals

- **SMB share creation or editing of any kind.** Originally scoped
  identically to NFS (on/off + an optional share name — see the design
  history below), but confirmed live against the real test box, as
  root: `zfs set sharesmb=on <dataset>` prints `"No SMB support in
  FreeBSD yet."` and fails with `"operation not supported"` — yet `zfs
  get sharesmb` afterwards reports the value as `on`/`local` anyway (the
  property is stored despite the share never being created), and
  `smbclient -L localhost -N` confirms nothing is actually listening
  (`NT_STATUS_CONNECTION_REFUSED`). FreeBSD's OpenZFS simply has no SMB
  support — this isn't a zfsctl bug to route around, it's a genuine
  capability gap in the underlying platform. Per explicit product
  decision, zfsctl does not pretend otherwise: it never offers to
  create or edit an SMB share. (Design history: an earlier revision of
  this spec/the implementation plan described a `'M'` key and a
  `FormKind::ShareSmb` mirroring the NFS form; both were built, then
  removed in the final-review fix wave once this was confirmed. See the
  implementation plan's fix-wave notes for the full transcript.)
- Samba permission/ACL editing via `smb4.conf` (valid users, guest
  access, etc.) — moot now that SMB creation itself is out of scope;
  would be a genuinely separate subsystem regardless.
- Enabling/starting `nfsd` if it's installed but not running, or
  installing/configuring Samba at all — this stays a ZFS-focused tool;
  the user manages system services themselves. (Samba's running-state
  detection, `samba_available`, was built for the original SMB-creation
  design and is kept — it's harmless, cheap, and still accurate
  information — but no longer gates anything in the UI.)
- A general config-file editor for `/etc/exports` entries created
  outside of ZFS's `sharenfs` mechanism (e.g. hand-written NFS exports
  for non-ZFS paths) — out of scope, this feature only manages shares
  through ZFS dataset properties.
- Full raw `exports(5)` syntax coverage in the structured NFS form —
  `sharenfs` actually accepts OpenZFS's own simpler cross-platform
  mini-language (confirmed live: `rw=@172.16.10.0/24,root=nobody`,
  matching `zfs(8)`'s own documented example), not literal
  dash-prefixed `exports(5)` flags; the common fields (networks, hosts,
  ro/rw, root mapping) are supported. Anything the parser doesn't
  recognize in an existing value (`sec=`, `crossmnt`, etc.) still
  displays via the kept raw fallback rather than failing, and a
  `sharenfs_editable_losslessly` check warns the user before they edit
  (and potentially silently drop) such a value through the structured
  form; it remains editable losslessly through the existing generic
  property-edit form as a raw-text escape hatch.
- Filtering snapshots/zvols out of the share list was originally
  missed — `zfs get sharenfs,sharesmb` with no type filter returns every
  dataset *and* snapshot/zvol on the host, and snapshots report `-`/`-`
  for both properties, which an early version of `parse_shares` didn't
  recognize as "unshared" (only the literal string `"off"` was
  checked). Fixed by adding `-t filesystem` to the `zfs get` call and
  treating `"-"` the same as `"off"` defensively in the parser.

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
pub fn sharenfs_editable_losslessly(raw: &str) -> bool;
```

(`Share::Smb` is still a variant, and `samba_available` still gets
checked — both exist purely to support *viewing and removing* an
existing SMB share, not to support creating one. See Non-goals.)

`list_shares` runs `zfs get -H -p -t filesystem sharenfs,sharesmb` with
**no dataset argument** — this lists every dataset (filesystem only —
`-t filesystem` excludes snapshots, zvols, and bookmarks) on the host in
one call, the same tab-separated shape every other `get` call in this
app already parses, just keeping the leading `name` column this time
(every other parser in the app discards it, since those call sites
already know which single dataset/pool they asked about — this is the
first parser that needs it). Datasets where both properties are `off`
(or, defensively, `-`) are dropped entirely. A `sharenfs` value is
parsed on a best-effort basis into the structured fields; `raw` is
always kept alongside so a value the parser doesn't fully recognize
still displays (unparsed) instead of the row disappearing or an error
surfacing.

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

`sharenfs_editable_losslessly(raw)` reports whether re-parsing then
re-assembling `raw` through the structured fields reproduces it exactly
— `"on"`/`"off"` always pass; a value using a clause the parser doesn't
recognize (`sec=`, `crossmnt`, etc.) fails, which the edit form uses to
show a warning before the user risks dropping it.

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
avoids a false "unavailable" reading in that real, observed case. (As
noted in Non-goals, this detection is kept but no longer gates anything
in the UI, since SMB creation isn't offered at all.)

`list_shares_with_availability` composes all three calls into one
`SharesSnapshot`, mirroring the `zfs::disk::list_disks_with_membership`
precedent exactly (avoids extra round-trips, one `WorkerMsg` covers a
full refresh).

### `Section::Shares`

Added to `App::sections` (the Tab-cycled list), like every other
top-level section. New `App` fields: `shares: Vec<Share>`,
`shares_table: TableState`, `nfs_available: Option<bool>`,
`samba_available: Option<bool>`, `shares_loaded: bool` — the
`Option<bool>` fields use `None` to mean "not yet checked", kept
distinct from `Some(false)` ("checked, unavailable") so the Shares
title doesn't flash a false "unavailable" hint before the first
refresh, consistent with how every other section stays quiet (an
empty list) rather than asserting a default before it's actually
fetched anything. `shares_loaded` becomes `true` once a Shares refresh
has completed for the currently-selected host (reset on host switch) —
it guards the Datasets `'N'` key from pre-filling its form from a
`self.shares` that was never populated, which would otherwise silently
widen an already-restricted share to "shared to everyone" on submit.

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

**Shares section**: table columns `Dataset | Protocol | Details` — the
Details column shows the share's raw value for NFS rows (not a
hand-reconstructed summary — the raw value is the only representation
guaranteed not to silently hide an unrecognized clause) and the share
name (or `"(default name)"`) for SMB rows. `Up`/`Down` navigate. `Enter`
on an NFS row opens the structured edit form, pre-filled from the row's
current (parsed) values, with a warning `description` if
`sharenfs_editable_losslessly` says the value isn't safely editable
through the structured fields. `Enter` on an SMB row sets a
status-message hint instead of opening a form — SMB creation/editing
isn't supported (see Non-goals). `'d'` on either row type opens
`Modal::Confirm` to remove the share (sets the relevant property back
to `off`) — this works identically for both protocols. `'r'` refreshes.
The title shows a hint when NFS is unavailable, e.g. `"Shares — local
('r' refresh, Enter edit, 'd' remove) [NFS unavailable: nfsd not
running]"` (no equivalent SMB hint — nothing is gated on Samba's
running-state).

**Creating/editing a share**: a `'N'` key in the **Datasets** section,
guarded by `pane_focus == PaneFocus::Middle` (not drilled into anything
— Datasets has no drill-down):

- If `!shares_loaded` (the Shares section has never successfully
  refreshed for this host), `'N'` sets a status-message hint
  ("press 'r' in the Shares section at least once…") instead of opening
  a form — pre-filling from an empty/stale `self.shares` could silently
  widen an existing restricted share to "on" on submit.
- Otherwise, if NFS isn't available (`nfs_available == Some(false)`),
  `'N'` sets a "NFS server not available on this host" hint.
- Otherwise, `'N'` opens `FormKind::ShareNfs { host, dataset }`, fields
  `[Networks, Hosts, Read-only (y/n), Root user mapping]`, pre-filled
  from the dataset's current `sharenfs` value's parsed fields if
  already shared, blank otherwise.

This is the **same** `FormKind` the Shares section's `Enter` uses for
NFS rows — one form serves both creation and editing, no duplicated
forms or logic. There is no `'M'` key and no SMB equivalent — see
Non-goals.

### Mutations

No new `MutationCall` variants — every share mutation is a
`MutationCall::SetDatasetProperty { dataset, key, value }` call,
reusing the existing library function and its `--`-guard entirely
unchanged.

- `FormKind::ShareNfs`'s fields assemble into a `sharenfs` value using
  the confirmed syntax above. Every field left blank assembles to plain
  `"on"` (share to everyone, read-write, no root-mapping restriction).
  Otherwise: the read-only/read-write clause is a bare `rw`/`ro` if both
  Networks and Hosts are blank (restriction-free, matching the
  confirmed bare-`rw` default), or `rw=<list>`/`ro=<list>` with each
  network prefixed `@` and colon-joined together with any hosts if
  either field is filled in; `,root=<user>` is appended if the Root
  user mapping field is filled in. E.g. Networks=`172.16.10.0/24`,
  Hosts=blank, Read-only=no, Root=`nobody` assembles to
  `rw=@172.16.10.0/24,root=nobody` — the exact value confirmed live
  above.
- Removing a share (`'d'` in the Shares section) reuses the existing
  `Modal::Confirm` pattern directly — no new `FormKind` — setting the
  row's relevant property (`sharenfs` or `sharesmb`, whichever that
  row's protocol is) to `"off"`. This is the only mutation path that
  ever touches `sharesmb`.
- Unlike every other creation form in this app so far, **no field in
  the NFS share form is required** — every field may legitimately be
  blank (blank Networks/Hosts means "share to everyone"), so
  `build_pending_action`'s `ShareNfs` arm needs no empty-field
  rejection check.

## Components / files touched

- New: `src/zfs/share.rs` (`Share`, `SharesSnapshot`, `list_shares`,
  `nfs_server_available`, `samba_available`,
  `list_shares_with_availability`, `sharenfs_editable_losslessly`, the
  `sharenfs`-syntax mini parser/assembler).
- `src/zfs/mod.rs`: register the new module.
- `src/ui/modal.rs`: `FormKind::ShareNfs`.
- `src/ui/state.rs`: `Section::Shares`; `App` fields (`shares`,
  `shares_table`, `nfs_available`, `samba_available`, `shares_loaded`);
  `WorkerMsg::Shares`; `refresh_shares`; `handle_key` arms
  (Up/Down/Enter/`'d'`/`'r'` in Shares, `'N'` in Datasets);
  `build_pending_action`'s `ShareNfs` arm.
- `src/ui/layout.rs`: Shares table rendering (raw value in Details),
  title hint, sidebar label.
- `src/ui/mod.rs`: `'r'`-key and queued-refresh dispatch arms for
  `Section::Shares`.
- `README.md`: keybindings.

## Testing

- `share.rs`: fixture-based parser tests for `list_shares` (a `zfs get`
  fixture mixing NFS-only, SMB-only, both, neither-shared, and a
  snapshot-shaped `-`/`-` pair — neither-shared and the snapshot pair
  must not appear in the result; one dataset with a `sharenfs` value the
  structured parser doesn't fully recognize, proving it still displays
  via the kept `raw` field rather than failing or vanishing). Two
  targeted availability tests: `samba_available` is `true` when `smbd`
  is up even though the overall `onestatus` exit code is nonzero (the
  `winbindd` case found above), and `false` when `smbd` itself isn't
  running. `list_shares_with_availability`'s three-call composition
  tested with `FakeRunner::route`, mirroring
  `list_disks_with_membership`'s precedent. A parse→assemble round-trip
  test: parsing an existing `sharenfs` value into structured fields and
  reassembling it must stay semantically consistent. Two
  `sharenfs_editable_losslessly` tests: `true` for canonical values,
  `false` for a value with unrecognized clauses.
- `state.rs`: `build_pending_action` test for `FormKind::ShareNfs`
  (assembly correctness only — no empty-field-rejection test needed,
  every field is optional); `'N'` from Datasets opens the form
  pre-filled correctly (blank when unshared, parsed-from-existing when
  already shared), hints when NFS is unavailable, and hints when
  `shares_loaded` is still `false`; `Enter`/`'d'` in the Shares section
  open the right edit/remove flow for NFS, and the right hint/remove
  flow for SMB; `refresh_shares` populates `App` state (including
  `shares_loaded`) from a `FakeRunner`; `clear_host_data` resets
  `shares_loaded` on host switch.

No ratatui rendering tests, consistent with every prior plan.
