# The screen

## What you will learn

- The three areas of the screen and how to move between them.
- How the hint bar, the Help screen and the confirmations work.
- How colours, themes and refreshing behave.

## The three areas

Here is the Pools section with nothing selected yet (a real capture; the pools belong to the author's test hub):

```screen
┌zfsctl────────────────┐┌Pools — local ('r' refresh, → for properties)────┐┌Properties — ? ('r' refresh, Ente┐
│Dashboard             ││Name                  Health       Used          ││Property    Value      Source    │
│Hosts                 ││scratch               ● ONLINE     ███░░░░░ 42%  ││                                 │
│Pools                 ││tpool                 ● ONLINE     ███░░░░░ 41%  ││                                 │
│Datasets              ││zroot                 ● ONLINE     ██░░░░░░ 25%  ││                                 │
│Snapshots             ││                                                 ││                                 │
│Disks                 ││                                                 ││                                 │
│Shares                ││                                                 ││                                 │
│Boot Envs             ││                                                 ││                                 │
│Jobs                  ││                                                 ││                                 │
│Cloud                 ││                                                 ││                                 │
│Alerts                ││                                                 ││                                 │
│Settings              ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      │└─────────────────────────────────────────────────┘└─────────────────────────────────┘
│                      │┌Metrics─────────────────────────────────────────────────────────────────────────────┐
│                      ││nothing selected                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  → Properties  n New pool  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

- **The sidebar** (left) lists the sections: Dashboard, Hosts, Pools, Datasets, Snapshots, Disks, Shares, Boot Envs, Jobs, Cloud, Alerts and Settings. The highlighted one is where you are. zfsctl opens on the Dashboard.
- **The list** (top right) shows the items of that section: hosts, pools, datasets and so on. In Pools and Datasets a second pane on the right shows the selected item's properties.
- **The panel** (bottom right) shows live details of whatever is selected: space, health, counts, and two graphs of read and write activity. It disappears on a very short terminal so that the list keeps room.
- **The hint bar** is the last line of the screen. It lists exactly the keys that work right now, and it changes as you move: a key that would do nothing is not shown. When there is a message for you (a result, an error), it appears on the line just above.

## The Dashboard

zfsctl opens on the **Dashboard**, the one screen that answers "is anything wrong, or about to be?". It has no boxes: just headings, aligned columns and colour.

```screen
┌zfsctl────────────────┐ Dashboard  ·  Sun 25 Oct 11:57                                                                                                  ✖ 4 issues
│Dashboard             │
│Hosts                 │ NEEDS ATTENTION ──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
│Pools                 │   ✖ issue fbsd16:zroot      no copy job
│Datasets              │   ✖ issue fbsd16:zroot/ho…  no copy job
│Snapshots             │   ✖ issue fbsd16:zroot/ho…  no copy job
│Disks                 │   ✖ issue protection        and 22 more not protected (22 of your data)
│Shares                │
│Boot Envs             │ GROUPS ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
│Jobs                  │  Group         State       Pools  Capacity             Memory             CPU              Load  Scrub     Errors
│Cloud                 │  freebsd (3)   ● ok        5      ━━━━━━━━╌╌ 79%       ━━━╌╌╌╌╌╌╌ 27%     ?                0.69  7 d ago   0
│Alerts                │  (no tag) (1)  ● ok        3      ━━━━━╌╌╌╌╌ 45%       ━━╌╌╌╌╌╌╌╌ 16%     ?                2.98  16 d ago  0
│Settings              │
│                      │ DATA SAFETY ──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
│                      │   ● weakest: local:scratch/data  tolerates 1 failure  · oldest copy 27 min · 1 data set has copies
│                      │     ignored by choice: local:zroot/ROOT, local:zroot/ROOT/default, local:zroot/tmp and 41 more (and everything below; Enter lists them)
│                      │
│                      │ DAEMON AND JOBS ──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
│                      │   ● daemon  running (pid 45678), heartbeat 2s ago
│                      │   ● jobs    scrub 2/2 ok  snapshot 34/34 ok  replicate 1/1 ok
│                      │
│                      │ CLOUD ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
│                      │     Provider          Remote      Jobs     Last run  Next run         Traffic  In bucket
│                      │   ○ Backblaze B2      b2          no jobs  -         -                -        -
│                      │   ○ SFTP              t1          no jobs  -         -                -        -
│                      │   ○ Amazon S3 (etc.)  zfsctl-aws  no jobs  -         -                -        -
│                      │
│                      │ ALERTS ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
│                      │   ● monitor on · last alert ub24 2 datasets not protected (15h ago) · report 2026-10-25
│                      │
│                      │
│                      │
│                      │
│                      │
└──────────────────────┘
↑↓ Move  e All hosts  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

- **The header** names the screen and gives the verdict on the right: `all quiet`, or how many things need attention (`1 issue, 1 to watch`). Until every host has answered its first probe it says `checking...`, because a screen that does not know yet must not say all is quiet.
- **Needs attention** lists the problems, worst first. `✖ issue` (red) is wrong now; `▲ watch` (yellow) is an early warning from today's numbers, such as a pool at 85% or a memory reading that is too high. When all is well it says `nothing needs attention`. It shows six lines and counts the rest (`+3 more`).
- **Hosts** has a row per host. **OS** (the short name, `FreeBSD 16.0`) is the first column to go when the terminal is narrow, then **ARC** (and **L2ARC**, drawn only if some host has a cache device), before the gauges. ARC is the ZFS read cache's hit rate since boot, the same figure as the Hosts panel; L2ARC is the cache device's size and its own hit rate, `2.0G 75.0%`. Neither is judged (no colour), and a host that cannot say shows `-`. State is the same verdict as in the Hosts section, raised (never lowered) by the readings that only the Dashboard has. **Capacity** is how full the fullest pool. **Memory** is memory in use *not counting* free pages, inactive pages and the ZFS cache: on FreeBSD the cache fills most of the RAM by design and gives it back when asked, so counting it would make the screen alarm all the time. **CPU** is the busy share between the last two probes (about 30 seconds). **Load** is the one-minute average and is never coloured. **Scrub** is how long since the last finished scrub (a resilver does not count). **Errors** is the read, write and checksum errors ZFS has counted.
- **Groups.** When any host has a `tag` (chapter 7), the Hosts block becomes **Groups**: one row per tag, with the number of hosts in it (`web (12)`), and hosts without a tag together as `(no tag)`. A group row shows the worst of its hosts: its state counts them (`1 issue, 2 to watch`, or `ok`), pools and errors add up, and every other reading is the worst any of them has (the fullest pool, the highest memory use, the oldest scrub). A group with a problem comes first, and under it are listed, indented, only the hosts that need a look, so a healthy group of forty machines takes one line. Every host still has its own line in Needs attention and its own row in the Hosts section. `Up` and `Down` move through the Needs attention lines and then the group rows; `Enter` on a group row opens the Hosts section narrowed to that tag (the hosts without a tag open the whole list), and `e` lists every host under its group, or folds the healthy ones again. Without any tag the Dashboard shows every host, as before.
- **Data safety** is one line: `weakest: local:tank/photos  tolerates 1 failure  · oldest copy 3 h · 4 data sets have copies`. zfsctl follows your copy jobs across every host as a graph (chapter 6, "Replicas and the chain"): each dataset that has a copy job is an original, and each replica, backup repository and cloud remote it feeds is a place that holds its data. The line is about the data whose picture is weakest. `Enter` on it (move down past the other lines) opens a pop-up with every such dataset drawn as a tree, weakest first: for each place its kind, how old the data there is at most (the oldest last success on the way to it, so a cloud copy fed through a replica is as old as the older of the two jobs), and whether every job on the way is current. "Tolerates N failures" counts the separate failure domains (every host, and every cloud remote, is its own; give hosts the same `site` in `config.toml` when they share a failure, such as two VMs on one machine, and they count as one) that hold a current copy, minus one: that many can be lost at once. Under a tree a line says what stops being fed if a place fails (`if nas:tank/copy fails, b2:bkt/photos is no longer fed`). The line is yellow when the weakest data has a place that is not current or no second failure domain; it is dim and says `no dataset has a copy yet` before the first copy job exists. It is advice about how well the copies are placed; the protection check above is what judges them, and it can ask for separate failure domains too (`domains` under `[protection]`, chapter 21).
- **Daemon and jobs** and **Alerts** are one line each: whether the daemon runs and how fresh its heartbeat is, how many jobs of each kind are fine (`backup 0/1 FAILED`), and whether the alert monitor can send mail.
- **Cloud** has one line per rclone remote under a header that names the columns: **Provider** (`Backblaze B2`, `SFTP`...), **Remote**, **Jobs** (how many use it, or `no jobs`), **Last run** (the last result and its age), **Next run** (the soonest of its enabled jobs), **Traffic** and **In bucket** (the size from the last time you measured it with `s` in Cloud, with the plan cap if you set one). Measuring lists the bucket and costs requests, so the Dashboard never does it: it shows the last value, which survives a restart, and a measurement that fails leaves the last good value in place. **Traffic** is a dotted track, `┄┄┄┄┄┄`, that is just a track while the remote is idle. While one of its jobs is sending, an arrow walks along it, `┄┄▸┄┄┄ 12.0 MiB/s`, with the rate the daemon last measured; a pull from the remote would walk the other way, `◂`, with `↑` and `↓` rates when both happen at once (today's cloud jobs only send, so you will see `▸`). It costs nothing: it reads the live progress the daemon already writes. A failed job turns its line red, and a bucket near its cap turns it yellow.
- **Jobs that stopped succeeding.** An enabled snapshot, replication, backup or cloud job whose last success is older than two of its schedule's intervals gets a yellow line, `snapshot tank/data: last ok 3h ago (Hourly)`, and a red one at four intervals. A job that has never succeeded is measured from the day you created it (`never succeeded (created 10h ago)`). One missed run is not flagged, because a run can start late or a long replication can overlap the next slot. A weekday-only job is not late over the weekend. Jobs that are disabled, running, failed (they have their own red line) or whose host is unsupported are left out, and nothing is listed while the daemon is down (its own red line says why). It reads only the job history, so it costs no call to any host. Enter goes to the job.
- **Resilvering and disk health.** A pool that is resilvering (a replaced disk catching up) gets a yellow line, `tank resilvering 42%`, until it finishes, because its redundancy is reduced meanwhile. A host whose disks have a SMART problem gets one line with the disks and the age of the check, `da1 reallocated sectors 8; ada4 (cache of tank) SSD wear 87%   (checked 6h ago)`, where the bracket says what the disk is for (cache, log, special, dedup or spare) when the storage check knows it (a disk named by label or id is shown bare): red if a drive says its own health check FAILED, yellow for reallocated, pending or uncorrectable sectors, an SSD worn to 80% or more, a low spare, or 60 degrees and up. Healthy hosts add nothing. The daemon asks `smartctl` once a day (03:30 by default; `daily_check` in `[daemon]` changes or stops it), so these lines are as old as the last check; if the check stops happening for three days, one more line says so (it counts the newest answer of the disk and the shares checks together). A host without `smartctl`, without the `smart` sudoers entry, or with virtual disks that report nothing, is left out without a word. Enter on a resilver line goes to that host's Pools, on a disk line to its Disks, on a shares line to its Shares.
- **File sharing.** The same daily pass asks each host whether its NFS server and Samba are running where it has shares for them. A host with NFS shares while `nfsd` is stopped, or with Samba shares while `smbd` is stopped, gets a red line (`NFS: 2 shares but nfsd is not running   (checked 3h ago)`); a Samba block that cannot be read is yellow. Hosts with healthy sharing, with no shares, or without the `shares`/`samba` sudoers entries add nothing. While a check runs (the daemon's, or one you started with `c` in Hosts) a red line under the header says so (`▶ checking: daily disk and shares check since 03:30`); it is a notice, not a problem, and it goes when the check is done. A check that crashed leaves it for at most three minutes.
- **Unprotected data.** A dataset with no safety net gets a line of its own, `fbsd16:tank/vm   no snapshot job; no copy job`: red for your data, yellow for the operating system's. See [Is my data protected?](#is-my-data-protected) below. Enter on one of these lines fixes that dataset, and the fix starts with the snapshots: a dataset that no snapshot job covers opens the **New snapshot job** form with its host and name filled in; one that has snapshots but too few copies asks which kind of copy (replicate to another host, back up with restic, or mirror to the cloud) and opens that form, again with the host and dataset filled in; and one whose job exists but never ran, stopped or is failing goes to that job in Jobs, so a second job is never made on top of it. Nothing is created until you confirm the form, and the destination of a copy is always yours to type. A dataset stays unprotected until its new job has run and counts as current. These lines come after the other problems of the same colour, so a degraded pool or a failed job is never pushed below them.
- **Unknown is never green.** A host that has not answered, or a reading that cannot be taken yet (the first CPU figure needs two probes), is shown as `?` in dim text. Every status has a glyph and a word as well as a colour, so the `mono` theme reads the same.

| Reading | Yellow (watch) | Red (issue) |
|---|---|---|
| Capacity | 80% | 90% |
| Memory in use | 90% | 97% |
| Swap in use | 50% | 90% |
| CPU, last two probes | 95% | never |
| Last scrub | 35 days, or never | 90 days |
| Host does not answer | | unreachable |

A pool whose special or dedup vdev is a single disk gets a red line (`tank special vdev has no redundancy`: losing it loses the pool), and a faulted log or cache device a yellow one; the same ten-minute check reads them.

A dataset whose checksum is off, and a pool with dedup on for a week that saves almost nothing, are yellow lines (`dedup_min_ratio` in `[dashboard]` sets "almost nothing").

A locked (encrypted, key not loaded) dataset that a restic backup reads, that a replication writes into, or that is shared over NFS (a Samba share is not seen) is a yellow line (`tank/secure is locked (a job uses it)`): it is something that will fail.

A dataset that uses 90% or more of its quota (or refquota) is listed too, one line per host naming the fullest one (`tank/home 94% of its 100G quota (+1 more)`), red from 98%; the daemon re-reads quotas every ten minutes.

These defaults can be changed under `[dashboard]` in `config.toml` (see the configuration reference). To stop a pool nobody scrubs, such as a scratch pool, from keeping the screen yellow, list it in `scrub_ignore = ["scratch"]`.

> **Safety:** the Dashboard itself changes nothing. `Up` / `Down` select a line of Needs attention and `Enter` takes you to the section that owns it: scrub a pool in Pools, look at a failed job in Jobs. On an unprotected-data line `Enter` opens the form that makes the missing job (or the list of the datasets, from the "and N more" line); nothing is created until you confirm that form.

> **Tip:** gauges (`━━━━╌╌╌╌╌╌`) appear when the terminal is wide enough. On a narrower one they go first, then the least useful columns; the host's name and state always stay. It stays readable at 80 columns.


### Is my data protected?

zfsctl calls a dataset **protected** when two things hold: **snapshots** and **a copy on another machine**, both made by jobs that are *current*. A job counts only while it keeps running: its last success must be no older than 1, 2 or 4 of its own schedule intervals, which is the "overdue" idea used above (the setting **How recent** picks strict, normal or relaxed; normal is 2). Only a run that really copied counts, so a copy job that often finds nothing new to send (for example a replication every 15 minutes with snapshots only hourly) may read "stopped" for part of each snapshot cycle; make the copy job's schedule match the snapshot job's. A job that never ran, failed last time or is disabled protects nothing.

How many copies are needed is your choice, the **level**:

| Level | Copies on other machines | Online |
|---|---|---|
| **Basic** (the default) | 1 | none needed |
| **Careful** | 3 | at least 1 (the "2 on-premises plus 1 online" rule) |
| **Custom** | 1 to 3, as you pick | 0 to 2, as you pick |

Snapshots are always required. The operating system's own datasets (boot environments under `ROOT`, `tmp`, `var`, `usr/ports`, `usr/src`, `usr/obj`, swap and dump) only ever need Basic's single copy and are listed in yellow; everything else is your data and is listed in red.

#### What counts as a copy

A copy counts when a job makes it on **another machine**, for the **whole dataset**, and the job is **current**. These are the jobs that can:

| Job | Counts as a copy when | Kind |
|---|---|---|
| Replication to a host | the destination host is not the host the job runs on | on-premises |
| Replication to a file | the job runs on another host (the file lands on the hub) | on-premises |
| Backup (restic) | the repository starts with `sftp:` or `rest:` (counted as on-premises, even if the server is far away) | on-premises |
| Backup (restic) | the repository starts with `s3:`, `b2:`, `azure:`, `gs:`, `swift:` or `rclone:` | **online** |
| Cloud mirror (rclone) | the remote is a real `name:path` remote | **online** |

Two jobs that write to the same place count as one copy.

#### Replicas and the chain: one body of data

A dataset that a current replication writes into is a **replica**: it holds the same data as its source. zfsctl follows these links and treats the original and all its replicas as **one body of data**. The body is judged once, and the verdict is shown on every dataset in it:

- Each replica is a copy on another machine.
- A cloud job, a backup or a replication on any dataset of the body is a copy of the whole body: a cloud mirror of the replica protects the original, and a cloud mirror of the original protects the replica. A place that several of them write to counts once.
- The original's own snapshots come from its snapshot job. A replica has the snapshots it was sent, so it needs no snapshot job of its own, as long as the replication that feeds it is current.
- If the replication stops, fails or is disabled, the link is cut: the replica is judged alone again and the original loses that copy.
- The original is never counted as a copy of itself. Where the data lives is the first thing to protect, and every check starts from it.

Some examples, at the Careful level (three copies, one of them online), with host `a` (the original) and host `b` (cold storage, a replica):

| Set up | `a:tank/data` | `b:tank/copy` |
|---|---|---|
| `a` replicates to `b` only | `1 of 3 copies on other machines; no online copy` | the same line: the data is short of copies, wherever it is held |
| `a` replicates to `b`, and `b` mirrors `tank/copy` to the cloud | `2 of 3 copies on other machines` (the replica and the cloud copy) | the same line |
| `a` replicates to `b`, and `a` mirrors `tank/data` to the cloud | `2 of 3 copies on other machines`, as above | the same line: it does not matter which machine sends to the cloud |
| the replication to `b` stopped three days ago | `copy on another machine stopped 3 days ago` | listed on its own again: `no snapshot job; no copy job` |

A group of hosts can ask for more or less than the rest: `[groups.databases.protection]` with `level = "careful"` makes the data whose original lives on a `databases` host need three copies, while the other hosts keep Basic (chapter 21 says which group decides what).

At the **Basic** level one copy on another machine is enough, so `a` replicating to `b` clears both lines without a cloud copy. To make the cloud mandatory, choose Careful, or Custom with at least one online copy.


#### What does not count as a copy

- **A backup repository on a local path**, such as `/mnt/usb/repo`: it is on the same machine, even if it is a different disk.
- **A replication to a file on the hub, run by the hub itself**: the file is on the same machine.
- **A replication whose destination is the host it runs on.**
- **A backup or cloud mirror with a Path set.** The Path field means "only this folder of the dataset", so the job copies a folder, not the dataset, and the dataset's line stays. The folder is protected, but the check says nothing about it.
- **A job on the parent or on a child.** A copy job covers only the dataset it names, even when it is marked recursive: each child needs a job of its own. (A recursive *snapshot* job is different, see below.)
- **A copy you made by hand**, such as a `restic backup` or a `zfs send` you ran yourself. The check judges jobs, because only a job keeps running.
- **A disabled job**, a job that has **never run**, one whose **last run failed**, or one whose **last success is too old** (older than the **How recent** setting allows). A run that was skipped because there was nothing new to send does not count as a success either.

#### When does a "no copy job" line clear?

All of these have to be true, for the exact dataset on the line:

1. A copy job from the table above names that host and dataset (no Path).
2. The job is enabled and its destination is another machine.
3. The job has run **successfully**, and recently. Until its first success the line reads `copy job has not run yet`.
4. There are **enough copies for your level**. At Basic one current copy is enough. At Careful a dataset needs three, at least one online, and until then the line reads `1 of 3 copies on other machines` or `no online copy`.
5. The dataset also has its **snapshots**: a current snapshot job covers it. The two halves are judged separately, and the dataset is listed while either is missing.

Some examples, at the Basic level:

| What you set up | Does the line clear? |
|---|---|
| A replication of `tank/photos` to host `nas`, once it has run | Yes, for `tank/photos` only (its children still need their own) |
| A restic backup of `tank/photos` to `sftp:nas:/backups/photos` | Yes, once it has run |
| A restic backup of `tank/photos` to `/mnt/usb/photos` | No: a local path |
| A restic backup of only the folder `holiday` inside `tank/photos` (Path set) | No: it copies a folder, not the dataset |
| A cloud mirror of `tank/photos` to `b2:mybucket/photos` | Yes, once it has run |
| A replication of `tank` marked recursive | Only for `tank` itself, not for `tank/photos` |
| A backup you ran by hand last night | No: nothing says another one will follow |

#### Snapshots and children

A snapshot job covers a dataset's children only if it is **recursive** and its latest successful run was a recursive one (the run says `(recursive)` in its message). A job you have just made recursive therefore reads `snapshot job has not run recursively yet` for the children until its next run.

Some things are deliberately quiet. A dataset created in the last day is not judged yet. A host that did not answer, or whose stored dataset list is more than two days old, says nothing about its datasets (the host already shows its own state). And there is **no exclusion list**: a swap, scratch or test dataset is listed until you protect or remove it. That is on purpose.

The check judges **jobs**, not the snapshots that happen to exist: snapshots you took by hand, or copies you made by hand, do not count, because nothing says more will come. So the words are about the job: `no snapshot job` means no enabled snapshot job covers the dataset, `snapshot job has not run yet` (or `copy job has not run yet`) means one does but has not succeeded yet (a job you have just made says this until its first run), `snapshots stopped 3 days ago` and `snapshot job failing` mean a job that stopped working, and `no copy job` means no replication, backup or cloud mirror covers it.

```text
 NEEDS ATTENTION
   ✖ issue  tank/photos          no snapshot job
   ✖ issue  fbsd16:tank/vm       no copy job
   ▲ watch  fbsd16:zroot/var     no snapshot job
```

The same list reaches you by email (chapter 16). Set the level, how recent and the email timing in Settings, group **Data protection**.

The Dashboard names the first three unprotected datasets (your data first). When there are more, the rest become one line, `and 122 more not protected (62 of your data)`, red while any of your data is among them and yellow when only the operating system's is. That line counts once in the header. Every dataset is still judged, named in the daily report and emailed according to the **Email** setting; only what the Dashboard draws is shorter. Enter on the "and N more" line opens the full list in a pop-up, grouped by host (the hub first, then the others by name, each under a heading that counts its datasets), red rows for your data and yellow for the operating system's, with the first dataset that has no line of its own selected: Up and Down move, Esc closes it, and Enter on a row fixes that dataset (below).

**Many datasets at once.** When dozens of datasets lack a snapshot job, make the jobs together: `Space` marks the selected row (a marked row shows `+`), `a` marks every row of the selected row's host that has no snapshot job (`a` again unmarks them), and `s` makes one snapshot job for each marked row, or for the selected row when none is marked. Each job is the one the form would suggest: every hour (`0 * * * *`), prefix `auto`, the newest 24 kept, caught up after downtime, not recursive. Before anything is saved you see how many jobs will be created and for which datasets, and which marked rows were left out and why: a dataset that already has a snapshot job (stopped, failing or not yet run) goes to its job instead and is never given a second one, and a row that lacks only a copy is not a snapshot problem. Confirm with `y` and Enter; `n` returns to the list with your marks. A job only counts as protection once it has run, and the oldest `auto` snapshots of a job are pruned beyond 24, never any other. Copies are not batched: each needs its own destination, so `Enter` on a row still opens the form that asks for it.

**Leaving datasets out.** Some datasets nobody wants snapshots of (scratch space, caches, build trees). Mark them (or select one) and press `i`: after a confirmation that says how many of the listed datasets it hides, `host:dataset` is added to `ignore` under `[protection]` in `config.toml`. An ignored dataset and **everything below it** is no longer judged: it leaves this list, the Dashboard, the alerts and the daily report's list of unprotected data. It is never hidden, though: the Dashboard names the ignored datasets under the data-safety line (`ignored by choice: local:zroot/tmp, local:zroot/var and 4 more (and everything below; Enter lists them)`: the first three, then how many more) and the report says `Ignored on purpose, with everything below them: ...` under the unprotected data, so "all datasets are protected" never speaks for what was left out. Nothing on the hosts is touched. A dataset that already had an open alert is dropped without a "protected again" email, since it was not protected, only set aside. To bring one back, select that line (it is the one after the data-safety line) and press `Enter`: a list of the ignored datasets opens. `Space` marks several, then `Enter` (or `u`) checks the marked ones again, or the selected one when nothing is marked, after a confirmation that names them; `n` or `Esc` leaves everything as it was. Nothing on the hosts is touched, and the dataset is judged again at the next refresh. The row **Ignored datasets** in Settings (a comma-separated list of `host:dataset`; empty clears it) edits the same list as text.

```screen
┌zfsctl────────────────┐┌Ignored datasets──────────────────────────────────────────────────────┐             ✖ 4 issues
│Dashboard             ││44 datasets are ignored, with everything below.                       │
│Hosts                 ││Space marks; Enter or u checks the marked ones (or this one) again.   │───────────────────────
│Pools                 ││                                                                      │
│Datasets              ││  local:zroot/ROOT                                                    │
│Snapshots             ││  local:zroot/ROOT/default                                            │
│Disks                 ││  local:zroot/tmp                                                     │
│Shares                ││  local:zroot/usr/ports                                               │
│Boot Envs             ││  local:zroot/usr/src                                                 │───────────────────────
│Jobs                  ││  local:zroot/var                                                     │  Errors
│Cloud                 ││  local:zroot/var/audit                                               │  0
│Alerts                ││  local:zroot/var/crash                                               │  0
│Settings              ││  local:zroot/var/log                                                 │copy 27 min · 1 data se…
│                      ││  local:zroot/var/mail                                                │l:zroot/tmp and 41 more…
│                      ││  local:zroot/var/tmp                                                 │
│                      ││  fbsd16:zroot/ROOT                                                   │
│                      ││  fbsd16:zroot/ROOT/default                                           │       Traffic  In buck…
│                      ││  fbsd16:zroot/tmp                                                    │       -        -
│                      ││  fbsd16:zroot/usr/ports                                              │       -        -
│                      ││  fbsd16:zroot/usr/src                                                │       -        -
│                      ││  fbsd16:zroot/var                                                    │ago) · report 2026-10-25
│                      ││  fbsd16:zroot/var/audit                                              │
│                      ││  fbsd16:zroot/var/crash                                              │
│                      ││  fbsd16:zroot/var/log                                                │
└──────────────────────┘│  fbsd16:zroot/var/mail                                               │
↑/↓ Move  Enter Check ag└──────────────────────────────────────────────────────────────────────┘
```

## Settings

The last section of the sidebar edits the parts of `config.toml` that no other screen has: the **theme** and the **auto-refresh** interval (`[ui]`), the **Dashboard thresholds** (`[dashboard]`: memory, swap, CPU, scrub age, and the pools the Dashboard does not judge by scrub age), the **cloud caps** (`[cloud.caps]`, one row per rclone remote) and **Data protection** (`[protection]`: the rows `Level`, `How recent (strict, normal, relaxed)` and `Email (immediate, report, off)`, plus `Copies on other machines (1 to 3)` and `Online copies (0 to 2)`, which appear only when the level is Custom; a wrong value in the file is shown as written, with a red line under the group that says what is wrong; see "Is my data protected?" above). Up and Down select a row and `Enter` opens a small form for it; a blank field goes back to the default, and a dimmed `(default)` marks every setting you have not set. A pair such as *Memory warn / crit* needs both numbers (or both empty), with warn above 0 and below crit. A save asks for the usual two-key confirmation, then writes only that key into `config.toml`: comments, hosts and every other section stay as they were, and the old file is kept as `config.toml.bak`. The change shows at once, with no restart: the theme repaints, the Dashboard uses the new thresholds and the Cloud screen the new caps. The Alerts screen keeps the mail address, the mail command and the report time, while this check's email timing lives in Data protection; the hook command is edited in the file.

```screen
┌zfsctl────────────────┐┌Settings — config.toml────────────────────────────────────────────────────┐
│Dashboard             ││Display                                                                   │
│Hosts                 ││  Theme                    solarized-dark                                 │
│Pools                 ││  Auto-refresh             10 s   (default)                               │
│Datasets              ││                                                                          │
│Snapshots             ││Dashboard thresholds                                                      │
│Disks                 ││  Memory warn / crit       90 / 97 %   (default)                          │
│Shares                ││  Swap warn / crit         50 / 90 %   (default)                          │
│Boot Envs             ││  Quota warn / crit        90 / 98 %   (default)                          │
│Jobs                  ││  Dedup warn below         1.10x   (default)                              │
│Cloud                 ││  CPU warn                 95 %   (default)                               │
│Alerts                ││  Scrub warn / crit        35 / 90 days   (default)                       │
│Settings              ││  Scrub ignore             (none)   (default)                             │
│                      ││                                                                          │
│                      ││Data protection                                                           │
│                      ││  Level                    basic   (default)                              │
│                      ││  Failure domains (1 to 3) 1   (default)                                  │
│                      ││  How recent (strict, normal, relaxed) normal   (default)                 │
│                      ││  Ignored datasets         local:zroot/ROOT, local:zroot/ROOT/default, lo…│
│                      ││  Email (immediate, report, off) report                                   │
│                      ││                                                                          │
│                      ││Cloud caps                                                                │
│                      ││  Cap of b2                10G                                            │
│                      ││  Cap of t1                (no cap)   (default)                           │
│                      ││  Cap of zfsctl-aws        (no cap)   (default)                           │
│                      ││                                                                          │
└──────────────────────┘└──────────────────────────────────────────────────────────────────────────┘
↑↓ Move  Enter Edit  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

## Moving around

| Key | What it does |
|---|---|
| `Tab` / `Shift+Tab` | next / previous section (the sidebar wraps around) |
| `Up` / `Down` | select the previous / next row |
| `Right` / `Left` | move into / out of the properties pane (Pools, Datasets) |
| `Enter` | open the selected item (for example drill into a pool) or edit it |
| `Esc` | go back one step: close a message, leave a pane, cancel a form |
| `r` | refresh what you are looking at |
| `?` | the Help screen |
| `A` | About: the zfsctl wordmark, the version and what the tool is for (include the version when you report a bug) |
| `!` | open a shell on the selected host |
| `q` | quit |

The full list, section by section, is in [the keys reference](20-reference-keys.md).

## Help

Press `?` at any time. The Help screen lists every key for every section:

```screen
┌zfsctl───────────────┌Help────────────────────────────────────────────────────────────┐─────────────────────┐
│Dashboard            │Global                                                          │Tag     Site    Used │
│Hosts                │  Tab / Shift+Tab  switch sections                              │                45%  │
│Pools                │                                                                │freebsd lab-box 62%  │
│Datasets             │  Up / Down        select a row                                 │freebsd lab-box 53%  │
│Snapshots            │(host/pool/dataset/snapshot/disk/share, or a                    │freebsd lab-box 79%  │
│Disks                │property/permission/ACL row)                                    │                     │
│Shares               │                                                                │                     │
│Boot Envs            │  r                refresh the focused pane                     │                     │
│Jobs                 │                                                                │                     │
│Cloud                │  F2               in a form, on a field with known values      │                     │
│Alerts               │(Permissions, Flags, Type, Recipient, Valid user, Root user     │                     │
│Settings             │mapping, a pool property with fixed values such as failmode or  │                     │
│                     │autoexpand, a from-disks Redundancy, a job's Host, Pool or      │                     │
│                     │Dataset, a replication's Destination host, and Schedule): open a│                     │
│                     │picker (type filter, Enter OK, Esc cancel; Space toggles in     │                     │
│                     │multi-select ones)                                              │                     │
│                     │                                                                │                     │
│                     │  F5               in the backup job form: check the repository │                     │
│                     │check the remote (both read-only; never block saving)           │                     │
│                     │                                                                │                     │
│                     │  Esc              dismiss a status message, back out of a      │                     │
│                     │focused pane, or cancel a modal (in a y/n confirmation Esc      │                     │
│                     │selects No, then Enter)                                         │─────────────────────┘
│                     │                                                                │─────────────────────┐
│                     │  y / n, Enter     a y/n confirmation takes two keys: y (yes) or│BSD_g6330a45b0       │
│                     │n / Esc (no) selects, then Enter applies it; Enter alone does   │E)                   │
│                     │nothing. Forms that cannot be undone (create pool, add vdev,    │ 62.2G               │
│                     │replace disk, revoke, shrinking a volume, pool features) end in │  hit 100.0%         │
│                     │one, showing the exact command                                  │e healthy            │
│                     │                                                                │                     │
│                     │  ?                this Help screen                             │                     │
│                     │                                                                │                     │
│                     │  !                open a shell on the selected host (remote    │                     │
│                     │hosts: your unprivileged ssh login); exit returns to zfsctl     │                     │
│                     │                                                                │                     │
│                     │  A                About                                        │                     │
│                     │                                                                │                     │
│                     │  q                quit                                         │                     │
│                     │                                                                │                     │
│                     │Hosts                                                           │                     │
│                     │  Up / Down        select the active host                       │                     │
│                     │                                                                │                     │
│                     │  /                filter the host list by name, address, tag,  │                     │
│                     │site or type (Enter keeps it, Esc clears it)                    │                     │
│                     │                                                                │                     │
│                     │  c                check the selected host's disks (smartctl)   │                     │
│                     │and shares (NFS, Samba) now; the result joins the Dashboard     │                     │
└─────────────────────│(read-only)                                                     │─────────────────────┘
↑/↓ Scroll  any key Cl└────────────────────────────────────────────────────────────────┘
```

It is part of the program, so it is always right for the version you are running; any key closes it. (On a small terminal the window may cut off the end of the list; the keys reference in this book has everything.)

## Confirmations

The actions that destroy or overwrite data ask first, and ask in a way that a slip cannot answer. (Ordinary edits, such as setting a property like `quota` or `readonly`, creating a dataset or a snapshot, or saving a share, apply when you submit the form: the form is your confirmation.) A yes/no question looks like this:

```screen
┌zfsctl────────────────┐┌Pools — local ('r' refresh, → for properties)────┐┌Properties — scratch ('r' refresh┐
│Dashboard             ││Name                  Health       Used          ││Property    Value      Source    │
│Hosts                 ││scratch               ● ONLINE     ███░░░░░ 42%  ││                                 │
│Pools                 ││tpool                 ● ONLINE     ███░░░░░ 41%  ││                                 │
│Datasets              ││zroot                 ● ONLINE     ██░░░░░░ 25%  ││                                 │
│Snapshots             ││                                                 ││                                 │
│Disks                 ││                                                 ││                                 │
│Shares                ││                                                 ││                                 │
│Boot Envs             ││                                                 ││                                 │
│Jobs                 ┌Confirm (y/n, then Enter)───────────────────────────────────────┐                     │
│Cloud                │Scrub pool 'scratch'? (y/n)                                     │                     │
│Alerts               │                                                                │─────────────────────┘
│Settings             │Press y for yes or n for no, then Enter.                        │─────────────────────┐
│                     │                                                                │ONLINE               │
│                     └────────────────────────────────────────────────────────────────┘%  103M / 240M       │
│                      ││                                                     Vdevs      1  (1 mirror)       │
│                      ││                                                     Disks      2  (2 data)         │
│                      ││                                                     Datasets   4 filesystems       │
│                      ││                                                     Snapshots  4                   │
│                      ││                                                     Compress   on 1.00x   Trim on  │
│                      ││                                                     Frag    42%   Dedup 1.00x      │
│                      ││                                                     +2 more lines: enlarge the ter…│
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
y Yes  n/Esc No
```

Press `y` to choose yes or `n` (or `Esc`) to choose no, and then `Enter` to apply your choice. `Enter` on its own does nothing, and a held-down key cannot answer twice. The hint bar says `y Yes  n/Esc No` while the question is open.

Some actions go further and ask you to **type a name**: destroying a pool or dataset, destroying or rolling back to a snapshot, destroying a boot environment. You type the exact name and then `Enter`; a wrong name keeps the prompt open and only `Esc` cancels. If you say no to a confirmation that came from a form, you return to the form with what you typed.

> **Safety:** destroying, rolling back, creating a pool, adding a vdev, replacing a disk, revoking permissions, shrinking a volume and enabling a pool feature all need two keys (or a typed name) on a question that says what it will do. Smaller changes do not ask, so read a form before you press `Enter` on its last field. If you are not sure, `Esc`.

## Forms

A form is a small window of fields. `Tab` or `Down` moves to the next field, `Shift+Tab` or `Up` to the previous one, and `Enter` on the last field submits it. `Esc` cancels. On some fields `F2` opens a list of valid choices to pick from (hosts, pools, schedules, users and so on). A form that can destroy something (create a pool, add a vdev, replace a disk) shows you the exact command and asks the two-key question before it runs.

## The shell key

`!` opens a shell and returns to zfsctl when you exit it. On the hub it is your normal shell, running as the user zfsctl runs as (root). On another host it is a normal interactive `ssh` login as that host's unprivileged account, never a root shell, and it needs nothing extra in sudoers. It is handy for a quick `zpool status` or `ls` without leaving the screen. If the host stops answering (it reboots, or the network drops), the login gives up after about 15 seconds and you are back on the screen with a message, instead of staring at a frozen terminal.

## Colours and themes

Headings are set apart from the body: section headings (`HOSTS`, `NEEDS ATTENTION`, a Settings group, the sections of Help) and the column titles of every table are drawn in bold in the theme's accent colour (blue in the Solarized themes, cyan in `ansi`); the thin rule after a Dashboard heading stays dim, and in the `mono` theme the headings are simply bold.

Colour is never the only signal: pool health is shown as `● ONLINE` (green), `▲ DEGRADED` (yellow), `✖ FAULTED`, `UNAVAIL` or `REMOVED` (red) and `○ OFFLINE` (grey), so it still reads on a monochrome terminal. Capacity bars turn yellow at 70% full and red above 85%.

Choose a theme in `config.toml`: `ansi` (the default: it uses only your terminal's own 16 colours, so your terminal theme applies), `mono` (no colour; chosen automatically when `NO_COLOR` is set), `solarized-dark` or `solarized-light` (best on a truecolor terminal). An unknown name is a startup error that lists the valid ones.

## Refreshing

The lists in Pools, Datasets, Snapshots, Disks, Shares and Boot Envs refresh themselves every ten seconds while you look at them (change it with `auto_refresh_secs` in `config.toml`; `0` turns it off). Your selection follows the item you had selected, by name, so a refresh never moves your cursor onto a different row. Press `r` to refresh now.

## Try it

Start zfsctl, press `Tab` to visit each section, press `?` and read the Help, close it, go to Pools, press `Down` to select a pool and `s`. You get the scrub question from the capture above. Press `Enter` alone and nothing happens, because the question is still open: that is the two-key rule. Press `n`, then `Enter`, to cancel.

## Where next

- [Hosts](07-hosts.md): reading the Hosts list.
- [Pools](08-pools.md): the heart of ZFS.
