# Reference: configuration

## What you will learn

- Every setting in `config.toml` and `jobs.toml`, with its type, default and meaning.
- Where zfsctl keeps its state.

This chapter is checked against the program's own list of settings by the book's tests. A `config.toml` that does not parse stops zfsctl at startup with a message; a daemon that finds a bad `[notify]` section logs it and keeps the previous settings.

## config.toml

The file is `/usr/local/etc/zfsctl/config.toml`. It has up to five parts: any number of `[[hosts]]` tables, and one each of `[ui]`, `[daemon]`, `[notify]` and `[cloud]`. The TUI's Add Host and the Alerts screen change only their own part and leave the rest of the file as it is.

### `[[hosts]]`: one table per remote host

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | text | (required) | the label shown everywhere; the hub is always present as `local` |
| `ssh_target` | text | none | `user@address` of the unprivileged account, or an alias from `~/.ssh/config` |
| `tag` | text | none | your own label (at most 40 characters), shown in Hosts and matched by its filter |

### `[ui]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `theme` | text | `ansi` | `ansi`, `mono`, `solarized-dark` or `solarized-light`; `mono` is also chosen when `NO_COLOR` is set |
| `auto_refresh_secs` | number | `10` | seconds between background refreshes of the list you are looking at; `0` turns them off |

### `[daemon]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `notify_command` | text | none | a shell command run (through `sh -c`, killed after 30 seconds) for job failures and health messages; the details arrive in `ZFSCTL_KIND`, `ZFSCTL_HOST`, `ZFSCTL_JOB`, `ZFSCTL_RESULT`, `ZFSCTL_MESSAGE` and `ZFSCTL_BODY` |

### `[notify]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `mail_to` | text | none | one address, or several separated by commas; unset means no email |
| `mail_command` | text | `msmtp -t` | the command that receives the whole message on its standard input |
| `mail_from` | text | `zfsctl@<hub name>` | the From address |
| `check_interval_minutes` | number | `5` | minutes between health checks (at least 1) |
| `report_time` | text | `07:00` | local time of the daily report, `HH:MM`, or `off` |
| `remind_hours` | number | `6` | repeat an unresolved alert this often; `0` for never |

The monitor runs only when at least one way of reaching you is set: `mail_to` or `notify_command`.

### `[cloud]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `caps` | table | none | `[cloud.caps]`: the room your cloud plan gives you, by rclone remote name, such as `b2 = "10G"` or `zfsctl-aws = "5G"`. A size is digits (a decimal point is allowed) and an optional `K`, `M`, `G` or `T`, counted the way cloud plans count (`10G` is 10 000 000 000 bytes). The Cloud screen then shows how much of it each remote uses |

## jobs.toml

The file is `/var/db/zfsctl/jobs.toml`. Each job is one `[[jobs]]` table; the TUI writes it for you, and the daemon notices a change within a second. A job needs only the keys its kind uses.

### `[[jobs]]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `id` | text | (required) | a unique name |
| `host` | text | (required) | a host name from `config.toml`, or `local` for the hub; where the work runs (the source, for a replication) |
| `kind` | text | (required) | `scrub`, `snapshot`, `replicate`, `backup` or `rclone` |
| `schedule` | text | (required) | five-field cron in the hub's local time |
| `pool` | text | none | scrub: the pool |
| `dataset` | text | none | snapshot, replicate, backup: the dataset |
| `recursive` | yes/no | `false` | snapshot: include child datasets, atomically |
| `prefix` | text | `auto` | snapshot: the name prefix; replicate and backup: which snapshots to use. Letters, digits, `_`, `-`, `.` |
| `keep` | number | none | snapshot: keep the newest N of this job's snapshots (at least 1); backup with `source = "new"`: how many of its own snapshots to keep (default 3) |
| `dest_host` | text | none | replicate: the receiving host |
| `dest_dataset` | text | none | replicate: the receiving dataset |
| `dest_dir` | text | none | replicate to files: the directory on the hub that receives the stream |
| `dest_keep` | number | absent | replicate to a host: keep this many of the job's own snapshots on the destination, destroy the older ones after each run (absent keeps all; minimum 1) |
| `remote` | text | none | rclone: the remote path to mirror to, such as `aws:mybucket/photos` (a configured rclone remote name, a colon and a path; never keys) |
| `bwlimit` | text | none | rclone: speed limit in bytes per second with an optional K, M or G, such as `5M` |
| `force` | yes/no | `false` | replicate: roll the destination back to the base (`receive -F`) |
| `repository` | text | none | backup: restic's `-r` value; no credentials in it |
| `password_file` | text | none | backup: absolute path of the password file on the job's host |
| `retention` | text | none | backup: restic retention, such as `7d 4w 12m 3y 5l`; blank keeps everything |
| `source` | text | `prefix` | backup: which snapshot to back up: `new` (take a fresh snapshot every run), `snapshot` (the one named in `snapshot`) or `prefix` (the newest `<prefix>-<timestamp>`) |
| `snapshot` | text | none | backup with `source = "snapshot"`: the snapshot's short name, the part after `@` |
| `path` | text | none | backup: a directory inside the dataset, such as `photos/2026`; blank backs up the whole dataset |
| `catch_up` | yes/no | `true` | run once at start-up if a scheduled time was missed |
| `enabled` | yes/no | `true` | whether the job runs |
| `created_at` | time | none | when it was made; a catch-up never reaches back before it |

## The state directory

`/var/db/zfsctl/`, readable by root only.

| File | What it holds |
|---|---|
| jobs.toml | the jobs |
| history.jsonl | one line per run event, in UTC |
| daemon.json | the daemon's heartbeat, rewritten every second |
| replication.json | progress memory of replications |
| notify.json | the date of the last daily report |
| health-events.jsonl | alerts, recoveries and starts, for the report and the Alerts screen |
| progress/ and cancel/ | progress files and cancel requests of running transfers |

The daemon's log is `/var/log/zfsctl.log`; the rc settings `zfsctl_enable`, `zfsctl_binary`, `zfsctl_state_dir` and `zfsctl_log` in `rc.conf` change the service.

## Try it

Open `config.toml` and add `tag = "backup"` to a host; save, restart zfsctl and find the tag in Hosts.

## Where next

- [Commands reference](22-reference-commands.md)
