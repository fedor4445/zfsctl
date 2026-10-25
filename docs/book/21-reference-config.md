# Reference: configuration

## What you will learn

- Every setting in `config.toml` and `jobs.toml`, with its type, default and meaning.
- Where zfsctl keeps its state.

This chapter is checked against the program's own list of settings by the book's tests. A `config.toml` that does not parse stops zfsctl at startup with a message; a daemon that finds a bad `[notify]` section logs it and keeps the previous settings.

## config.toml

The file is `/usr/local/etc/zfsctl/config.toml`. It has up to eight parts: any number of `[[hosts]]` tables, and one each of `[ui]`, `[ssh]`, `[daemon]`, `[notify]`, `[cloud]`, `[dashboard]` and `[protection]`. The TUI's Add Host and the Alerts screen change only their own part and leave the rest of the file as it is; the Settings screen does the same for `theme` and `auto_refresh_secs` in `[ui]`, for `[dashboard]`, for `[protection]` and for `[cloud.caps]`.

### `[[hosts]]`: one table per remote host

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | text | (required) | the label shown everywhere; the hub is always present as `local` |
| `ssh_target` | text | none | `user@address` of the unprivileged account, or an alias from `~/.ssh/config` |
| `tag` | text | none | your own label (at most 40 characters), shown in Hosts and matched by its filter |
| `site` | text | none | what this host shares its failure with (`rack1`, `office`). Hosts with the same `site` count as one place when the data-safety line counts how many separate failures the data survives; without it a host is its own place |

### `[ui]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `theme` | text | `ansi` | `ansi`, `mono`, `solarized-dark` or `solarized-light`; `mono` is also chosen when `NO_COLOR` is set |
| `auto_refresh_secs` | number | `10` | seconds between background refreshes of the list you are looking at; `0` turns them off |

### `[ssh]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `multiplex` | `true` or `false` | `false` | `true` makes the short commands to one host share a single ssh connection, kept open for a minute after the last one, instead of opening a new connection for each. A command then takes about 55 ms instead of about 220 ms, and the host's `sshd` logs one login instead of dozens. At most 8 commands share a connection at once. Backups, cloud mirrors and replication always use a connection of their own, so a dropped shared connection cannot interrupt them. The sockets live in `/var/db/zfsctl/ssh`, readable by root only; if that directory cannot be made, zfsctl quietly goes back to one connection per command. Read when zfsctl or the daemon starts: restart the daemon after changing it |

### `[daemon]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `daily_check` | text | `03:30` | when the daemon runs the daily checks on every host, in the hub's local time: the disks with `smartctl` and the file sharing services (is `nfsd`, is Samba running where there are shares). `HH:MM`, or `off`. Read when the daemon starts. The results are kept in `smart.json` and `shares.json` and shown on the Dashboard; a host without `smartctl`, without the `smart` sudoers feature or without the `shares`/`samba` ones is skipped without a word. The name was `smart_check` for a day; that is still read |
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

### `[dashboard]`

When the Dashboard turns a reading yellow ("to watch") and red ("issue"). All keys are optional. A warn/crit pair that does not make sense (warn not below crit, zero, over 100) is ignored and its defaults are used.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `memory_warn` | number | `90` | memory in use, in percent, from which the value turns yellow; free pages, inactive pages and the ZFS cache do not count as in use |
| `memory_crit` | number | `97` | memory in use, in percent, from which it turns red |
| `swap_warn` | number | `50` | swap in use, in percent, from which it turns yellow |
| `swap_crit` | number | `90` | swap in use, in percent, from which it turns red |
| `quota_warn` | number | `90` | how much of its quota (or refquota) a dataset uses, in percent, from which Needs attention lists it in yellow |
| `quota_crit` | number | `98` | the same, from which it turns red |
| `dedup_min_ratio` | number | `1.10` | a pool with deduplication on that saves less than this factor (and holds more than 10 GiB) is listed in yellow in Needs attention |
| `cpu_warn` | number | `95` | CPU busy, in percent, from which it turns yellow, measured between the last two probes (about 30 seconds); the CPU alone never turns red |
| `scrub_warn_days` | number | `35` | days since a pool's last finished scrub from which it turns yellow (a pool never scrubbed is yellow too) |
| `scrub_crit_days` | number | `90` | days since the last scrub from which it turns red |
| `scrub_ignore` | list of text | none | pool names the Dashboard does not judge by scrub age, such as `["scratch"]`; a resilver does not count as a scrub |

### `[protection]`

What counts as a protected dataset. All keys are optional, and the words are lower case. A wrong value, including one of the wrong type (`copies = "2"`, `copies = 300`, `level = 5`), never stops the daemon or the rest of the file from loading: zfsctl names the key and the values it takes (`[protection] copies must be 1, 2 or 3`) in the daemon's log (`config.toml [protection] problem ...`), on the Settings screen under Data protection, and as a yellow `config.toml` line in the Dashboard's Needs attention (`[protection]: email must be immediate, report or off; judging at Basic`). While it is wrong, the Dashboard and the `zfsctl report` preview judge at Basic; the running daemon keeps the settings it had before the change (it uses Basic only when it starts with a wrong section). Fix the file, or the row on the Settings screen.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `level` | text | `basic` | `basic` (one copy), `careful` (three copies, one of them online) or `custom` |
| `copies` | number | `1` | with `level = "custom"`: how many copies a dataset needs, 1 to 3; ignored for the other levels |
| `online` | number | `0` | with `level = "custom"`: how many of those copies must be online, 0 to 2 and never more than `copies`; ignored for the other levels |
| `domains` | number | `1` | with any level: how many separate failure domains the copies must span, the original's included, 1 to 3. A host is a domain, hosts that share a `site` are one, and a cloud remote is its own. `1` asks nothing; `2` lists a dataset whose copies all sit in one domain (for example a replica on a VM of the same machine) |
| `recent` | text | `normal` | how old a copy may be and still count: `strict` (one interval), `normal` (two) or `relaxed` (four) |
| `email` | text | `immediate` | when an unprotected dataset is emailed: `immediate`, `report` (only in the daily report) or `off` |

### `[groups.<tag>]`

Hosts that share a `tag` (chapter 7) form a group, and a group may set some things for itself. Every key is optional; what a group does not set it inherits from the global sections. A host without a tag, and `local`, the hub, use the global settings.

```toml
[groups.databases.protection]
level = "careful"

[groups.lab.protection]
email = "off"

[groups.web.notify]
mail_to = "web-team@example.com, oncall@example.com"
```

`[groups.<tag>.protection]` takes the same keys as `[protection]` above (`level`, `copies`, `online`, `recent`, `email`), laid over them for the hosts of the group.

| Key | Type | What it does |
|---|---|---|
| `mail_to` | text | in `[groups.<tag>.notify]`: where the alerts about this group's hosts are mailed, one address or several separated by commas. The daily report always goes to the global `mail_to` |

Which group decides what: how much protection a body of data needs (copies and online copies) is asked by the group of its **original**, the dataset that is not a replica of another (chapter 6); how recent a job must be is the group of the host that **runs** the job; whether a dataset is mailed at once, in the daily report only, or not at all is the group of the host the dataset **is on**. A group with `email = "off"` is not reported either, and the report says `(email off)` next to the host instead of calling its datasets protected.

A wrong value in a group never stops the program: zfsctl names it (`[groups.db.protection] level must be basic, careful or custom`) in the daemon's log, in the report preview and as a yellow line in Needs attention, and that group is judged by the global settings meanwhile. A wrong global `[protection]` still keeps the previous settings, as before.

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
