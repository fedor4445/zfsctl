# Rename zfsctl to zfsctl: design

Date: 2026-10-18. The user chose the name **zfsctl** (no "-it", to avoid confusion). Option 1 (clean break with a one-time
migration) was chosen over a long compatibility period.

## What changes

Everything a person or a host sees: the crate and binary (`zfsctl`), the CLI text, the service and rc.d script (`zfsctl`,
`zfsctl_enable`, ...), the pid and log files, the config path (`/usr/local/etc/zfsctl/config.toml`), the state directory
(`/var/db/zfsctl`), the mount root (`/var/run/zfsctl`), the sudoers drop-in name, the mail sender (`zfsctl@host`) and subject
(`[zfsctl]`), the hook environment (`ZFSCTL_JOB`, `ZFSCTL_RESULT`, ...), the capture tool's `ZFSCTL_BIN`, the book, the README.
Historic specs and plans under `docs/superpowers` stay as they were. Real captures stay until they are re-taken on the hub.

## What stays compatible (persisted identifiers that existing data carries)

- **smb4.conf managed block:** the new markers are `## zfsctl managed shares ...` and `# zfsctl-dataset`; the parser still
  reads the old `zfsctl` ones, and the next write replaces them.
- **restic tags:** new backups carry `zfsctl:<job>` and `zfsctl-snapshot:<snapshot>`; `snapshots` and `forget` also match
  the old `zfsctl:` / `zfsctl-snapshot:` tags (repeated `--tag` is an OR), so old backups keep counting and keep being pruned.
- **`zit-` prefix** of fresh snapshots, clones and boot environments stays as it is: pruning is by prefix, and changing it
  would strand every existing snapshot. It is a legacy identifier, not a brand.
- **Migration on first start:** the daemon and the TUI, finding no new state directory but an old `/var/db/zfsctl`, move it
  (same filesystem); finding no new config but an old `/usr/local/etc/zfsctl/config.toml`, copy it (the old file is left).
  Both say so on stderr / in the log. Hosts' sudoers rules are by command, so existing drop-ins keep working; the name of the
  drop-in file is only advice in `print-sudoers`.

## Done means

All tests green; the hub moved by hand (rc.conf variables, service, sudoers name) and the three FreeBSD VMs still reached;
the book's captures re-taken; memory notes updated. The repository directory and the GitHub repository are renamed by the user.
