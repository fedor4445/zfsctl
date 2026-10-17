# Scheduled jobs

## What you will learn

- What the daemon does and how to run it.
- How to schedule scrubs and snapshots, and how replication is set up.
- How to read a job's history and handle a failure.

## The daemon

Jobs run unattended in the **daemon**, a background program on the hub: `zfsctl daemon`, started by the rc script you installed in chapter 4 (`service zfsctl start`). The title of the Jobs section tells you whether it is running (`daemon running (pid …)`); the screen reads a heartbeat file the daemon rewrites every second.

```screen
┌zfsctl────────────────┐┌Jobs — daemon running (pid 98986)───────────────────────────────────────────────────────────────────────┐
│Hosts                 ││Job                    Host     Target         Schedule             Next run        Last                │
│Pools                 ││scrub-local-tpool      local    tpool          Weekly, Sunday 03:00 Sun 10-11 03:00 never               │
│Datasets              ││                                                                                                        │
│Snapshots             ││                                                                                                        │
│Disks                 ││                                                                                                        │
│Shares                ││                                                                                                        │
│Boot Envs             ││                                                                                                        │
│Jobs                  ││                                                                                                        │
│Alerts                ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      │└────────────────────────────────────────────────────────────────────────────────────────────────────────┘
│                      │┌Job — scrub-local-tpool─────────────────────────────────────────────────────────────────────────────────┐
│                      ││Last runs                                                Weekly, Sunday 03:00  (0 3 * * 0)              │
│                      ││No runs yet                                              Next: Sun 2026-10-11 03:00, Sun 2026-10-18     │
│                      ││                                                         03:00, Sun 2026-10-25 03:00                    │
│                      ││                                                         enabled  catch up yes                          │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  Enter Edit  Space Disable  d Delete  n New scrub job  N New snapshot job  R New replication job  ? Help  q Quit
```

The Jobs section is global: it lists the jobs of every host. For the selected job, the panel shows its recent runs, its schedule in words and in cron form, and its next three run times. The **Target** column shows where data goes, and **Last** the result of the last run. Your local time zone is used everywhere.

## Schedules

A schedule is five fields, `minute hour day-of-month month day-of-week`, with `*`, numbers, lists (`1,15`), ranges (`1-5`) and steps (`*/15`). Day of week is `0`–`6` with `0` Sunday. `0 3 * * 0` is "every Sunday at 03:00". In the forms `F2` offers presets (Hourly, Daily, Weekly, Monthly…), and the form shows the next three run times as you type, so you can see whether you meant what you wrote. If both day fields are restricted a day matches when either does.

**Catch up** (`y` by default) means: if the hub was off at the scheduled time, run the job once when the daemon comes back. A catch-up never reaches back before the job was created.

> **Coming from Linux:** this is cron with a history and a screen. **Coming from Windows:** this is Task Scheduler, with the log next to the task.

## A scrub job

Press `n` in Jobs, or `J` on a pool in Pools:

```screen
┌zfsctl────────────────┐┌Jobs — daemon running (pid 98986)───────────────────────────────────────────────────┐
│Hosts                 ││Job               Host     Target         Schedule        Next run        Last      │
│Pools                 ││scrub-local-tpool local    tpool          Weekly, Sunday  Sun 10-11 03:00 never     │
│Datasets              ││                                                                                    │
│Snapshots             ││                                                                                    │
│Disks                 ││                                                                                    │
│Shares                ││                                                                                    │
│Boot Envs             ││                                                                                    │
│Jobs                  ││                                                                                    │
│Alerts                ││                                                                                    │
│                     ┌New scrub job───────────────────────────────────────────────────┐                     │
│                     │> Host: local                                                   │                     │
│                     │  Pool:                                                         │                     │
│                     │  Schedule: 0 3 * * 0                                           │                     │
│                     │  Catch up (y/n): y                                             │─────────────────────┘
│                     │                                                                │─────────────────────┐
│                     │Weekly, Sunday 03:00                                            │                     │
│                     │Next: Sun 2026-10-11 03:00, Sun 2026-10-18 03:00, Sun 2026-10-25│                     │
│                     │03:00                                                           │                     │
│                     └────────────────────────────────────────────────────────────────┘                     │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
Enter Next  Tab Next field  Shift+Tab Prev field  F2 Pick  Esc Cancel
```

Choose the host (`F2` lists your configured hosts), the pool (`F2` fetches the pool names of that host live), the schedule and catch up. A monthly scrub of every pool is a good minimum.

## A snapshot job

Press `N` in Jobs, or `J` on a dataset in Datasets:

```screen
┌zfsctl────────────────┐┌Jobs — daemon running (pid 98986)───────────────────────────────────────────────────┐
│Hosts                 ││Job               Host     Target         Schedule        Next run        Last      │
│Pools                 ││scrub-local-tpool local    tpool          Weekly, Sunday  Sun 10-11 03:00 never     │
│Datasets              ││                                                                                    │
│Snapshots             ││                                                                                    │
│Disks                 ││                                                                                    │
│Shares               ┌New snapshot job────────────────────────────────────────────────┐                     │
│Boot Envs            │Only snapshots named <prefix>-<timestamp> made by this job are  │                     │
│Jobs                 │ever pruned.                                                    │                     │
│Alerts               │                                                                │                     │
│                     │> Host: local                                                   │                     │
│                     │  Dataset:                                                      │                     │
│                     │  Recursive (y/n): n                                            │                     │
│                     │  Prefix: auto                                                  │                     │
│                     │  Keep (newest N): 24                                           │─────────────────────┘
│                     │  Schedule: 0 * * * *                                           │─────────────────────┐
│                     │  Catch up (y/n): y                                             │                     │
│                     │                                                                │                     │
│                     │Hourly                                                          │                     │
│                     │Next: Sat 2026-10-10 18:00, Sat 2026-10-10 19:00, Sat 2026-10-10│                     │
│                     │20:00                                                           │                     │
│                     │                                                                │                     │
│                     └────────────────────────────────────────────────────────────────┘                     │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
Enter Next  Tab Next field  Shift+Tab Prev field  F2 Pick  Esc Cancel
```

The job takes a snapshot of the dataset named `<prefix>-<UTC timestamp>` (for example `hourly-20261010T170000Z`) and then keeps only the newest **Keep** of the snapshots it made. **Recursive** includes the child datasets, taken together. The job only ever deletes names that match exactly its own `<prefix>-<timestamp>`: your manual snapshots, other jobs' snapshots, and look-alikes like `hourly-keep-forever` are never touched. If a snapshot cannot be destroyed (a hold, a clone), the run is recorded as failed and says why.

## A replication job

Press `R` in Jobs:

```screen
┌zfsctl────────────────┐┌Jobs — daemon running (pid 12894)───────────────────────────────────────────────────┐
│Hosts                 ││Job               Host     Target         Schedule        Next run        Last      │
│Pools                 ││scrub-local-tpool local    tpool          Weekly, Sunday  Sun 10-18 03:00 ok 5d ago │
│Datasets              ││                                                                                    │
│Snapshots             ││                                                                                    │
│Disks                ┌New replication job─────────────────────────────────────────────┐                     │
│Shares               │Sends the newest <prefix>-<timestamp> snapshot, incrementally.  │                     │
│Boot Envs            │The destination is received unmounted and rolled back only if   │                     │
│Jobs                 │Force is y. Keep on target destroys this job's older snapshots  │                     │
│Alerts               │on a host destination after each run.                           │                     │
│                     │                                                                │                     │
│                     │> Source host: local                                            │                     │
│                     │  Source dataset:                                               │                     │
│                     │  Prefix (snapshots to send): auto                              │                     │
│                     │  Destination host (or file):                                   │                     │
│                     │  Destination dataset (or directory):                           │─────────────────────┘
│                     │  Force: roll back the destination (y/n): n                     │─────────────────────┐
│                     │  Schedule: 30 * * * *                                          │                     │
│                     │  Catch up (y/n): y                                             │                     │
│                     │  Keep on target (blank = keep everything):                     │                     │
│                     │                                                                │                     │
│                     │custom schedule                                                 │                     │
│                     │Next: Fri 2026-10-16 22:30, Fri 2026-10-16 23:30, Sat 2026-10-17│                     │
│                     │00:30                                                           │                     │
│                     │                                                                │                     │
│                     │                                                                │                     │
│                     └────────────────────────────────────────────────────────────────┘                     │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
Enter Next  Tab Next field  Shift+Tab Prev field  F2 Pick  Esc Cancel
```

It copies the newest snapshot with a given prefix (typically the ones a snapshot job takes) from a dataset on one host to a dataset on another, **incrementally** after the first full copy, through the same ssh and sudo path the TUI uses, so nothing new is needed in sudoers. The destination can instead be a *file*: a directory on the hub that receives the stream as files.

- **Nothing is destroyed on the source.** The destination is received unmounted, and is only touched if you set **Force** to `y`; without it, a destination that has been written to fails the run with ZFS's own message.
- **Interrupted transfers resume** (to another host). After a network drop, a cancel or a restart, the next run continues where it stopped. A transfer to files cannot resume; it starts again.
- **Progress and cancel.** A running replication shows `sent 12.3 GiB of 40.0 GiB (30%) 85.0 MiB/s eta 5m 00s` in the panel; `c` cancels it within a second or two. A cancelled run is not a failure and sends no alert.
- **Keep on target** (optional, hosts only). The last field of the form, `dest_keep` in `jobs.toml`: keep only the newest N of this job's own snapshots on the destination and destroy the older ones after each run. Blank keeps everything, which is the default. When you save, zfsctl looks at the destination and, if the setting would destroy snapshots right now, shows how many, the oldest and the newest of them, and how many stay; it needs a second keystroke. Only snapshots named exactly `<prefix>-<timestamp>` are ever destroyed, never ones you made by hand, and a snapshot with a hold or a clone is left alone. A destination that is a file is never pruned.
- Two enabled jobs cannot write to the same destination, and `Space` refuses to enable one that would clash.

> **Safety:** **Force** adds `zfs receive -F`: it rolls the destination back to the last snapshot both sides share, **destroying destination snapshots and changes newer than that**. Leave it at `n` unless you know the destination has stray changes you want to discard.

> **Safety:** **Keep on target** permanently destroys snapshots on the destination, including ones the source no longer has. Choose N large enough to go back as far as you may need, and read the confirmation before you accept it.

> **Safety:** replication is not a backup of mistakes: a deleted file is deleted on the copy at the next run. Keep several snapshots on the destination (a snapshot job there) if you want to go back.

## Managing jobs

| Key | What it does |
|---|---|
| `Up` / `Down` | select a job |
| `n` / `N` / `R` / `B` / `C` | new scrub / snapshot / replication / backup / cloud mirror job |
| `Enter` | edit the selected job (its history and enabled state are kept) |
| `Space` | enable or disable it |
| `c` | cancel a running replication or backup |
| `d` | delete it (yes/no); its past runs stay in the history for a while |

Jobs live in `/var/db/zfsctl/jobs.toml` and run history in `history.jsonl`; the daemon notices an edit of `jobs.toml` within a second, and keeps the previous schedule if the new file does not parse.

## When a job fails

A failed job turns the host's State in Hosts yellow (`job failed`), counts in the Jobs column, and, when alerts are on, sends you a message ([chapter 16](16-alerts.md)). Select the job: the panel lists its recent runs with the error text. A fixed job clears the state at its next successful run.

## Try it

Make a snapshot job for the practice dataset with `J` in Datasets: prefix `practice`, keep `3`, schedule `* * * * *` (every minute, only for this test). After four minutes you should see exactly three snapshots in Snapshots. Delete the job with `d` afterwards.

## Where next

- [Backups](15-backups.md): off-site copies with restic.
- [Alerts](16-alerts.md): be told when a job fails.
