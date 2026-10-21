# Reference: commands

## What you will learn

- Every way to start zfsctl, with its options and exit codes.
- Exactly which programs it may run as root on a managed host.

zfsctl must be started as root (on the hub, or through `sudo`); that includes `--help`. The command forms below are checked by the book's tests against the program's own command-line parser.

## Commands

### `zfsctl`

Opens the terminal interface.

### `zfsctl --help`

Prints the usage of every command, the feature names `--with` and `--without` accept, the files zfsctl reads and writes, and the exit codes, then exits 0. `-h` does the same, and so does `--help` after any command (`zfsctl print-sudoers --help`). Like every other form it must run as root.

### `zfsctl daemon`

Runs the scheduler and the health monitor in the foreground; the rc script starts it for you (`service zfsctl start`). Stops cleanly on `SIGTERM` or `SIGINT`.

### `zfsctl daemon --state-dir DIR`

The same, with state kept in `DIR` instead of `/var/db/zfsctl` (for tests and trials).

### `zfsctl print-sudoers HOST`

Prints the sudoers entry for a managed host, built from the programs it finds there. Reads only. Exit codes: 0 success, 1 a problem (such as a missing program), 2 the host cannot be reached.

### `zfsctl print-sudoers HOST --with FEATURE --without FEATURE`

`--with restic` adds backup support, `--with rclone` adds cloud mirror support and `--with smart` the daily disk health check (needs `smartmontools`); `--without acl`, `disks`, `bootenvs`, `shares` or `samba` leaves a feature out. The options may be repeated.

### `zfsctl check-host HOST`

Checks that a host's sudo rules allow exactly what is needed: one line per program (`ok`, `MISSING` or `TOO BROAD`) and a count. Exit codes: 0 everything ok, 1 something is not, 2 the host cannot be reached. It accepts the same `--with` and `--without` options.

### `zfsctl notify-test`

Sends a short test message through every configured channel and prints one line per channel (`mail: ok`, `mail: failed: …`, `hook: ok`). Sends **real** mail. Exit code 0 when every configured channel worked; 1 when any failed or none is configured.

### `zfsctl report`

Prints the daily report as it would be sent now, and sends nothing. It probes the hosts the way the monitor does.

### `zfsctl report --state-dir DIR`

The same, reading events and state from `DIR`.

## The sudoers entry

On a managed host the unprivileged account may run these programs as root through `sudo -n`, each at its full path as the host reports it (the output of `print-sudoers` is the authority). A feature the host does not use can be left out.

| Program | Feature | How it is allowed |
|---|---|---|
| `zpool` | core | any arguments |
| `zfs` | core | any arguments |
| `getfacl` | ACL tab | any arguments |
| `setfacl` | ACL tab | any arguments |
| `geom` | Disks | any arguments |
| `bectl` | Boot Envs | any arguments |
| `service` | Shares | any arguments |
| `smbpasswd` | Samba shares | any arguments |
| `smbstatus` | Samba shares | only with `-S` |
| `cat` | Samba shares | only on `/usr/local/etc/smb4.conf` |
| `tee` | Samba shares | only on `/usr/local/etc/smb4.conf` |
| `restic` | backup jobs (`--with restic`) | any arguments |
| `readlink` | backup jobs with a path (`--with restic`) | any arguments (it only reads where a symlink points) |
| `rclone` | cloud mirror jobs (`--with rclone`) | any arguments |
| `beadm` | boot environments on illumos hosts (default there; `--with beadm`) | any arguments |
| `smartctl` | the daily disk health check (`--with smart`) | only `--scan -j` and `-j -H -A -i /dev/*` (the `*` also matches spaces, so an extra flag is not stopped; zfsctl sends only read-only flags) |
| `ls` | looking inside snapshots (`--with browse`, off by default) | only eight `ls -l...` forms that all end in `-- *`, so no option can follow; the `*` also matches spaces and `/`, so the path is not confined to `.zfs/snapshot`, but it lists names and metadata only, never contents, and the account already holds `zfs` |

Other programs zfsctl runs on a host need no sudo: `getent`, `uname`, `date`, `hostname`, `sysctl`, `swapinfo`, `iostat` (also `iostat -En` for the Disks of an illumos host), `lsblk` (the Disks of a Linux host), `showmount` and `stat` (the backup form's check reads the password file's mode and owner) only read.

> **Safety:** `zfs`, `zpool`, `bectl`, `restic` and `rclone` are powerful: whoever holds the account's ssh key effectively has root on that host. That is the price of managing it remotely; keep the key safe and the account locked to key logins.

## Try it

Run `zfsctl print-sudoers fbsd16` and `zfsctl check-host fbsd16` for one of your hosts and compare with the table above.

## Where next

- [Keys](20-reference-keys.md)
- [Glossary](23-glossary.md)
