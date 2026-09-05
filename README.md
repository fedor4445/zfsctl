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
- `r`: refresh the current section for the selected host
- `Esc`: dismiss the status message
- `q`: quit
