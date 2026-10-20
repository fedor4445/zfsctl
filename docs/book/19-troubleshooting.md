# Troubleshooting

## What you will learn

- What the messages mean, and the first thing to try for each.

## A host shows red or yellow in Hosts

Read the State column ([chapter 7](07-hosts.md) lists every text). The usual ones:

| State | First thing to try |
|---|---|
| `unreachable` | from the hub, as the hub's user, run `ssh user@host true`; check the network and the host's power |
| `ssh login refused` | the key is not authorised; see chapter 5 |
| `host key not accepted` | run `ssh user@host` once and accept the key, or fix a changed key deliberately |
| `cannot inspect: …` | run `zfsctl check-host <name>`; usually sudoers |
| `<pool> DEGRADED` | a disk has failed; see "Replace a failed disk" (chapter 17) |
| `<pool> <n>% full` | free space, or add storage |
| `job failed` | open Jobs and read the last run |

## "a password is required"

The feature runs a program that the host's sudoers entry does not allow. Run `zfsctl check-host <name>`; it names the missing line. Replace the entry with the output of `zfsctl print-sudoers <name>`.

## The daemon

The Jobs title says `daemon NOT running: jobs will not fire`: start it with `service zfsctl start`, and check `sysrc zfsctl_enable`. The log is `/var/log/zfsctl.log`; it is stamped in the hub's local time.

## A job did not run

- Is it **enabled** (`Space`)? Is the daemon running?
- Look at **Next run** in the panel: is the time what you meant (local time of the hub)?
- If the hub was off: with **catch up** `y` it runs once when the daemon starts.
- A replication whose previous transfer is still running skips the slot.

## Replication or backup fails

The panel shows the tool's own message. Common ones:

- `destination … has been modified since most recent snapshot`: something wrote to the destination; remove the change or set Force.
- A resume token that cannot be continued: on the destination run `zfs receive -A <dataset>`.
- restic: `repository does not exist`, or `wrong password`: check the repository and the password file path **on that host**; run `restic snapshots` there as root.
- rclone: the remote must be in root's rclone config on that host.
- `skipped: no snapshots with prefix 'x' on tank/data`: the backup's Source is a prefix, and the job only counts snapshots named exactly `x-<UTC timestamp>` (such as `x-20261011T151500Z`). A snapshot called just `x`, or `x-old`, is invisible to it. Set Source to `new` (the job takes its own), to `@name` (that one snapshot), or to a prefix a snapshot job really makes. `F2` on Source lists what exists.
- `the snapshot tank/data@x no longer exists`: the Source is pinned to `@x` and someone destroyed it. The run fails on purpose, so you notice. Pick another Source.
- `skipped: up to date (…)`: normal. The repository already holds a backup of that snapshot from this job.
- `cannot snapshot tank/data: …`: a `new` Source could not take its snapshot (out of space, or a snapshot limit on the dataset).
- `could not remove the old snapshot …`: the backup worked, but a clone (or another destroy failure) stops the job from pruning one of its own `zit-…` snapshots; it tries again next run. A snapshot with a hold is not an error: the run says `kept the old snapshot <name> (held)` and the reason instead. If zfsctl cannot check for holds, the note says `could not remove the old snapshot <name> (could not check holds: …)`.
- `could not check holds: …`: zfsctl could not tell whether a snapshot is held, so it refused to destroy it (nothing was destroyed). A snapshot job or replication run with this message is **Failed**, not green. Check that the host answers and that `zfs get -H -p -t snapshot -o name,value userrefs <dataset>` works there as the same user; chapter 10 explains holds.
- `cannot destroy snapshot …: dataset is busy` (when you run `zfs destroy` by hand, outside zfsctl): ZFS's own refusal for a snapshot that has a hold or a clone. `zfs holds <snapshot>` lists the holds. zfsctl checks first and says `protected: …` instead; the message above is what ZFS itself says, on Linux and on OmniOS (checked on both).
- `sftp: no space left on device`: the disk holding the repository is full. Free space there (for example the package cache with `pkg clean -a`, as root) or move the repository; `df` on that host shows the filesystem.
- `Note:` lines under `F5` in the backup form are about the password file's location and permissions, not about the backup (chapter 15, "Protect the password file").

## No mail arrives

1. Run `zfsctl notify-test` as root on the hub. It prints one line per channel.
2. `mail: failed: exited with status 78` or similar: the mail command itself failed; run the same command by hand and read its error. Check `~/.msmtprc` of the user the daemon runs as (root).
3. `mail: ok` but nothing arrives: look in spam; check the sender address (`Mail from`) is accepted by your provider.
4. Nothing configured: the Alerts screen says `Monitor off — set an email address to turn alerts on`.

## NFS or SMB does not work

- `NFS server not available on this host`: start `nfsd` (`service nfsd start`).
- A client cannot mount: check the network in the share matches the client's address, and the firewall.
- SMB: Samba must be installed and running; the valid user must exist as a Unix user; check `smbd` is running.

## The screen looks wrong

Make the terminal wider or taller; colours need a terminal that supports them (`theme = "mono"` is always safe). Press `r` to refresh.

## Where to look

- `/var/log/zfsctl.log` (daemon), `/var/db/zfsctl/` (state: jobs, history, events), `config.toml`.
- `zfsctl check-host`, `zfsctl notify-test`, `zfsctl report`.
- When you report a bug, include the version (`A` for About) and the message.

## Try it

Stop the daemon (`service zfsctl stop`), look at the Jobs title, and start it again.

## Where next

- [Reference: configuration](21-reference-config.md)
- [The cover](README.md)
