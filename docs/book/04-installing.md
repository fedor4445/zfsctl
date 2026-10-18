# Installing

## What you will learn

- What you need, and how to build and install zfsctl on your hub.
- What the configuration file looks like and where it lives.
- How to run the daemon at boot, and where it keeps its files and its log.

## What you need

- A **FreeBSD machine with ZFS** to be the hub. zfsctl uses FreeBSD's `zpool` and `zfs` and has no Linux or illumos support.
- **Root.** The screen and the daemon both run as root on the hub, because they run ZFS commands.
- **ssh set up for any other host** you want to manage (chapter 5): keys and host keys that work from the hub's root account.
- A Rust toolchain to build it (`pkg install rust`), or a binary someone built for you.

Optional tools that unlock features: `restic` (backups, chapter 15), `rclone` (cloud backups), `smartmontools` (the daily disk health check on the Dashboard, chapter 6; `pkg install smartmontools`), and a mail tool such as `msmtp` (alerts, chapter 16).

## Build and install

From a checkout of the project:

```sh
cargo build --release
install -m 755 target/release/zfsctl /usr/local/bin/zfsctl
```

You can also run it straight from the build directory (`sudo ./target/release/zfsctl`) while you are trying it out.

## First run

Run it as root:

```sh
sudo zfsctl
```

Not sure what is available? `sudo zfsctl --help` prints every command, the files it uses and the exit codes.

You land on the Hosts section with one host in the list, `local`: the hub itself. Use `Tab` to move between sections and `q` to quit; press `?` at any time for the Help screen. The next chapters explain every screen.

> **Safety:** looking never changes anything. The screen only changes your data when you submit a form or answer a confirmation, and the dangerous changes always ask twice.

## The configuration file

zfsctl reads `/usr/local/etc/zfsctl/config.toml`. It does not need to exist: without it you have just the hub. This is the hub used for this book's screens:

```toml
[[hosts]]
name = "fbsd16"
ssh_target = "fbsd@172.16.10.60"

[[hosts]]
name = "fbsd16t1"
ssh_target = "fbsd@172.16.10.61"

[[hosts]]
name = "fbsd16t2"
ssh_target = "fbsd@172.16.10.62"

[ui]
theme = "solarized-dark"

[notify]
mail_to = "you@example.com"
mail_command = "msmtp -a default -t"
```

- Each `[[hosts]]` block adds a machine. `name` is the label you will see, and `ssh_target` is `user@address` for the unprivileged account zfsctl connects as (never `root`). An optional `tag = "…"` is a free label shown in the Hosts table.
- `[ui]` holds display settings. `theme` is one of `ansi` (the default, uses your terminal's own colours), `mono` (no colour, also chosen automatically when `NO_COLOR` is set), `solarized-dark` and `solarized-light`. `auto_refresh_secs` sets how often lists refresh in the background (10 by default, 0 turns it off).
- `[notify]` and `[daemon]` configure alerts and are covered in chapter 16.

You rarely need to edit this file by hand: adding a host in the screen (`n` in Hosts) and changing the alert settings (the Alerts screen) both write to it, changing only their own part and leaving your comments and the rest of the file alone. The complete list of settings is in [the configuration reference](21-reference-config.md).

> **Coming from Linux:** this is the equivalent of a file in `/etc`, except that on FreeBSD packages keep theirs in `/usr/local/etc`.

## The daemon

The screen shows and changes things while you are there. **Scheduled jobs** (snapshots, scrubs, replication, backups) and the **alerts** need something that keeps running when you are not: the daemon, `zfsctl daemon`. The project ships an rc script for it:

```sh
install -m 755 contrib/rc.d/zfsctl /usr/local/etc/rc.d/zfsctl
sysrc zfsctl_enable=YES
service zfsctl start
service zfsctl status
```

The script starts it through `daemon(8)`, which detaches it, writes a pid file, sends its output to a log, and restarts it ten seconds after a crash. Three optional settings in `rc.conf` change the defaults: `zfsctl_binary` (`/usr/local/bin/zfsctl`), `zfsctl_state_dir` (`/var/db/zfsctl`) and `zfsctl_log` (`/var/log/zfsctl.log`).

> **Coming from Linux:** this is the `systemd` unit; `sysrc zfsctl_enable=YES` is `systemctl enable`, and `service zfsctl start` is `systemctl start`. The log is a plain file you can `tail -f`.

> **Coming from Windows:** it is a Windows service that starts with the machine and keeps your scheduled tasks running.

The Jobs section's title tells you whether the daemon is running (`daemon running (pid …)` or `daemon NOT running`), so you will notice if it has stopped.

## Where it keeps its files

The daemon and the screen share a directory, `/var/db/zfsctl`, readable only by root:

```text
total 144
drwx------  2 root wheel     2 Oct  9 22:07 cancel
-rw-------  1 root wheel   104 Oct 10 15:36 daemon.json
-rw-r--r--  1 root wheel   428 Oct 10 15:21 health-events.jsonl
-rw-------  1 root wheel 54844 Oct  9 22:35 history.jsonl
-rw-------  1 root wheel   179 Oct 10 10:32 jobs.toml
-rw-r--r--  1 root wheel    29 Oct 10 14:11 notify.json
drwx------  2 root wheel     2 Oct  9 22:32 progress
-rw-------  1 root wheel     2 Oct  7 20:52 replication.json
```

| File | What it holds |
|---|---|
| `jobs.toml` | your scheduled jobs |
| `history.jsonl` | one line per job event (started, finished, skipped) |
| `daemon.json` | a heartbeat the daemon rewrites every second; if it is older than 10 seconds the daemon is not running |
| `replication.json` | what each replication to a file last wrote |
| `notify.json` | the date of the last daily report |
| `health-events.jsonl` | alerts, recoveries and daemon starts, for the report and the Alerts screen |
| `progress/`, `cancel/` | live transfer progress and cancel requests |

Back this directory up along with `config.toml`: together they are everything zfsctl knows. The daemon's log is `/var/log/zfsctl.log`.

## Upgrading

Build the new version, then replace the binary and restart the daemon. Keep the old binary next to it so you can go back:

```sh
cp /usr/local/bin/zfsctl /usr/local/bin/zfsctl.prev
install -m 755 target/release/zfsctl /usr/local/bin/zfsctl
service zfsctl restart
```

A job that is running when you restart is noticed and marked as lost when the daemon comes back, and the next scheduled run starts it again.

### Coming from zfsctl

zfsctl is the new name of zfsctl. Moving an existing install over is a one-time step, in this order:

1. Stop the old daemon: `service zfsctl stop`.
2. Install the new binary (`install -m 755 target/release/zfsctl /usr/local/bin/zfsctl`) and the new rc script (`install -m 555 contrib/rc.d/zfsctl /usr/local/etc/rc.d/zfsctl`). Remove the old script, `/usr/local/etc/rc.d/zfsctl`.
3. In `rc.conf` replace `zfsctl_enable="YES"` with `zfsctl_enable="YES"` (`sysrc zfsctl_enable=YES` and `sysrc -x zfsctl_enable`). The other rc variables were renamed the same way (`zfsctl_binary` is now `zfsctl_binary`, and so on).
4. Start it: `service zfsctl start`.

The first start moves the old state directory `/var/db/zfsctl` to `/var/db/zfsctl` (jobs, history and the heartbeat come along) and copies `/usr/local/etc/zfsctl/config.toml` to `/usr/local/etc/zfsctl/config.toml`, leaving the old file in place; it says so on its standard error and in the log. Only the default locations are moved: a daemon started with its own `--state-dir` keeps it. The log is now `/var/log/zfsctl.log`.

Things that were renamed and that your own scripts may mention: the hook command's environment variables are now `ZFSCTL_JOB`, `ZFSCTL_RESULT`, `ZFSCTL_HOST`, `ZFSCTL_KIND`, `ZFSCTL_MESSAGE` and `ZFSCTL_BODY`, and mail comes from `zfsctl@<host>` with a `[zfsctl]` subject. Two things were deliberately kept so nothing that already exists is stranded: restic backups made before the rename (tagged `zfsctl:...`) are still found and pruned, and the managed block of `smb4.conf` is still read under its old markers and rewritten under the new ones at the next change. The names of the snapshots, clones and boot environments that jobs create keep their `zit-` prefix, because pruning goes by that prefix. The sudoers rules on your hosts need no change: they are by command, and a drop-in file called `zfsctl` keeps working (new ones are named `zfsctl`).

## Try it

On a FreeBSD machine that has ZFS: build zfsctl, run it with `sudo ./target/release/zfsctl`, look at the Hosts list, press `?` to read the Help, then `q`. Then install the rc script, enable the daemon, and open the **Jobs** section (press `Tab` seven times, or `Shift+Tab` twice from Hosts: the sidebar wraps around). The title should say `daemon running`.

## Where next

- [Remote hosts and sudoers](05-remote-hosts.md) to add other machines.
- [The screen](06-the-screen.md) for a tour of what you see.
