# zfsctl

A napp-it-like terminal UI for managing ZFS storage on FreeBSD.

## Status

This is v1, and it is browse/refresh only. From the TUI you can view pools,
datasets, and snapshots (locally or on a configured remote host over SSH)
and refresh any of those panes on demand. There is currently **no way to
create, destroy, scrub, replace a disk, roll back, or replicate anything
from the UI** — those mutating operations exist as tested library functions
(`zfs::pool`, `zfs::dataset`, `zfs::snapshot`) but are not yet wired up to
any key or action. Wiring them into the UI is planned as a follow-up.

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
- `r`: refresh the current section for the selected host
- `Esc`: dismiss the status message
- `q`: quit
