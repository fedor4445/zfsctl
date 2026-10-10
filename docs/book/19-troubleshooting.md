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

The feature runs a program that the host's sudoers entry does not allow. Run `zfsctl check-host <name>`; it names the missing line. Replace the entry with the output of `zfsctl print-sudoers <user>`.

## The daemon

The Jobs title says `daemon not running`: start it with `service zfsctl start`, and check `sysrc zfsctl_enable`. The log is `/var/log/zfsctl.log`; it is stamped in the hub's local time.

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

## No mail arrives

1. Run `zfsctl notify-test` as root on the hub. It prints one line per channel.
2. `mail: msmtp exited 78` or similar: the mail command itself failed; run the same command by hand and read its error. Check `~/.msmtprc` of the user the daemon runs as (root).
3. `mail: ok` but nothing arrives: look in spam; check the sender address (`Mail from`) is accepted by your provider.
4. Nothing configured: the Alerts screen says `Monitor: off`.

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
