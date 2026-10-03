# zfsctl

A napp-it-like terminal UI for managing ZFS storage on FreeBSD.

## Requirements

- FreeBSD (uses `zpool`/`zfs`; no Linux/illumos support)
- Root privileges
- `ssh` configured (with working host keys / `~/.ssh/config` entries) for any remote hosts you add to the config

## Running

```sh
cargo build --release
sudo ./target/release/zfsctl
```

## Configuration

Host inventory lives in `/usr/local/etc/zfsctl/config.toml`:

```toml
[[hosts]]
name = "backup"
ssh_target = "backup.example.com"
```

The local machine is always available as the "local" host and does not need a config entry.

## Keys

- `Tab` / `Shift+Tab`: switch sections (Hosts, Pools, Datasets, Snapshots, Disks, Shares) — this is now the *only* way to cycle sections; `Left`/`Right` have a different meaning (see below).
- `'['` / `']'` (Datasets right pane only, once focused with `Right`): cycle between the Properties and Permissions sub-tabs.
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
- `R`: resilver the selected pool (y/n confirm)

### Pool/Dataset properties pane (right pane, `Right` to focus)

- `Up` / `Down`: select a property (or, on the Datasets Permissions sub-tab, a permission row — see below)
- `Enter`: edit the selected property's value (prompts pre-filled with the current value; submitting runs `zpool set`/`zfs set`)
- `r`: re-fetch properties (or permissions, on that sub-tab) for the currently-selected pool/dataset
- `Esc` / `Left`: back out to the middle pane

#### Datasets' Permissions sub-tab (`'['`/`']'` to switch to it)

Delegated ZFS administration permissions (`zfs allow`/`unallow`) for the
selected dataset — who can run which ZFS subcommands/properties on it.
Not available for Pools (`zfs allow` only targets a filesystem or
volume).

- `n`: grant a permission to a user/group/everyone (prompts for
  Recipient — `everyone` / `user:<name>` / `group:<name>` — Scope —
  `local`/`descendent`/`both`, blank means `both` — and Permissions, a
  required comma-separated list)
- `s`: define or add to a named permission set (prompts for the set
  name without `@`, and Permissions)
- `c`: add a create-time permission — auto-granted to whoever creates a
  new child dataset under this one (prompts for Permissions only)
- `d`: revoke everything for the selected row's target (y/n confirm)
- `x`: revoke just specific permissions from the selected row, leaving
  the rest intact (prompts for which permissions to remove)

Granting is always additive (never a replace) — this mirrors how `zfs
allow` itself works: granting a user two different permissions in two
separate calls merges into one combined grant, it never overwrites.

### Vdevs (inside a drilled-into pool)

- `Esc` / `Backspace`: back out to the Pools list
- `o`: offline the selected disk (y/n confirm) — no-op on a mirror/raidz group row, not a physical disk
- `O`: online the selected disk (y/n confirm) — same restriction as `o`
- `x`: replace the selected disk (prompts for the new device name) — same restriction as `o`
- `a`: add a new vdev/mirror/raidz group to this pool (prompts for a vdev spec, e.g. `mirror da2 da3`)

### Datasets

- `Right` / `Left`: move focus to/from the properties pane (same as Pools, above)
- `n`: create a new dataset (prompts for name)
- `d`: destroy the selected dataset (type the dataset's name to confirm)
- `s`: snapshot the selected dataset (prompts for the snapshot's name only)
- `N`: create/edit an NFS share for the selected dataset (prompts for networks, hosts, read-only, and root user mapping — all optional; press `r` in the Shares section at least once first, so zfsctl knows the dataset's current share state before offering to change it)

### Snapshots

- `d`: destroy the selected snapshot (type `dataset@name` to confirm)
- `b`: roll back to the selected snapshot (type `dataset@name` to confirm) — discards any writes made since

### Disks

- `Up` / `Down`: select a disk
- `Space`: toggle-select the highlighted disk (only if it's not already claimed by a pool)
- `n`: create a new pool from the selected disk(s) (prompts for name and redundancy — blank for a stripe, or `mirror`/`raidz1`/`raidz2`/`raidz3`)
- `a`: add a vdev to an existing pool from the selected disk(s) (prompts for the target pool name and redundancy, same as above)
- `Esc`: clear the current selection
- `r`: refresh the disk list

### Shares

- `Up` / `Down`: select a share
- `Enter`: edit an NFS share's networks/hosts/read-only/root mapping. SMB
  rows show a hint instead of opening a form — FreeBSD's `sharesmb` ZFS
  property doesn't actually work (confirmed: `zfs set sharesmb=on` prints
  "No SMB support in FreeBSD yet." and fails, even though the property gets
  stored), so zfsctl can only view and remove an existing SMB share, never
  create or edit one.
- `d`: remove the selected share (NFS or SMB), y/n confirm
- `r`: refresh the share list

## Status

v1 covers browsing and 11 of the 12 ZFS operations implemented at the library layer: create/destroy/scrub a pool, offline/online/replace a disk, create/destroy a dataset, and create/destroy/rollback a snapshot — all from the TUI, locally or over SSH. `zfs send`/`receive` replication is implemented and tested at the library layer (`zfs::snapshot::replicate_snapshot`) but is **not yet wired into the UI** — it needs its own design pass for progress display and cancellation, since a single replication can run for hours. A dedicated follow-up plan will wire it up. Pool and dataset properties (`zpool`/`zfs get`/`set`) can now be viewed and edited from a properties pane next to the Pools/Datasets lists (`Right` to focus it). Delegated ZFS administration permissions (`zfs allow`/`unallow`) can be viewed, granted, and revoked from a Permissions sub-tab in the Datasets right pane (`'['`/`']'` to switch to it, once focused with `Right`) — Pools has no equivalent, since `zfs allow` only targets a filesystem or volume. File ACLs (`getfacl`/`setfacl`) and bhyve zvol association are still not implemented — planned as follow-up work. A Disks section lists every block device on the host with its size and pool/vdev membership, and lets you build a new pool or add a vdev to an existing one from a multi-disk selection instead of typing device names; `zpool resilver` is also available from the Pools section. A Shares section lists every dataset's active NFS/SMB share and lets you create or edit an NFS share's networks/hosts/read-only/root mapping from a structured form; SMB shares can only be viewed and removed, not created or edited, since FreeBSD's `sharesmb` ZFS property doesn't actually serve anything (confirmed empirically).
