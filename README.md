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
