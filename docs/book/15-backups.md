# Backups with restic

## What you will learn

- Why snapshots and replication are not yet a backup, and what restic adds.
- How to set up a repository and a backup job, and restore from it.
- How to send backups to the cloud through rclone.
- How to mirror a dataset to AWS S3 or Backblaze B2 as plain files you can browse, and how to set up the cloud account for it.

## Why another layer

Snapshots live in the same pool as the data: if the pool dies, so do they. Replication ([chapter 14](14-jobs.md)) puts a copy on another machine, which is good, but it copies your mistakes too. A real backup is **a separate copy, in a different place, in a different format, that you can restore from by itself**. zfsctl's backup job uses [restic](https://restic.net), a well-regarded open-source backup tool: encrypted, deduplicated, and able to store its repository on a USB disk, another machine over sftp, or a cloud service.

## One-time setup

On the host that will run the backup (the hub, in the running example):

1. Install restic: `pkg install restic`.
2. Make a password file only root can read, and put the repository password in it:
   ```sh
   install -m 600 /dev/null /root/.restic-password
   vi /root/.restic-password
   ```
3. Create the repository once: `restic init --repo /mnt/usb/restic-tank --password-file /root/.restic-password`.

> **Safety:** **keep the password in your password manager too.** If you lose it, the backup cannot be opened by anyone, including you. zfsctl never creates a repository for you, on purpose: a mistyped path would otherwise silently start a new, empty one.

## The backup job

Press `B` in Jobs:

```screen
┌zfsctl────────────────┐┌Jobs — daemon running (pid 74775)───────────────────────────────────────────────────┐
│Dashboard             ││Job               Host     Target         Schedule        Next run        Last      │
│Hosts                 ││scrub-local-tpool local    tpool          Weekly, Sunday  Sun 10-25 03:00 ok 2d ago │
│Pools                ┌New backup job──────────────────────────────────────────────────┐0-25 03:00 ok 2d ago │
│Datasets             │Backs up a read-only clone of a snapshot. Source: new = take a  │                     │
│Snapshots            │fresh snapshot each run and keep the last few of those; a prefix│                     │
│Disks                │= the newest <prefix>-<timestamp> (it takes none); @name =      │                     │
│Shares               │always that one snapshot. The repository must already exist     │                     │
│Boot Envs            │(restic init).                                                  │                     │
│Jobs                 │                                                                │                     │
│Cloud                │> Host (where restic runs): local                               │                     │
│Alerts               │  Dataset:                                                      │                     │
│Settings             │  Path inside the dataset (blank = all of it):                  │                     │
│                     │  Source (F2 to pick: new, a prefix, or @snapshot): new         │                     │
│                     │  Snapshots to keep (Source new only; blank = 3):               │─────────────────────┘
│                     │  Repository (restic -r; no credentials in it):                 │─────────────────────┐
│                     │  Password file (absolute path on the host):                    │                     │
│                     │  Keep (e.g. 7d 4w 12m 3y 5l; blank = keep everything):         │                     │
│                     │  Schedule: 30 3 * * *                                          │                     │
│                     │  Catch up (y/n): y                                             │                     │
│                     │                                                                │                     │
│                     │custom schedule                                                 │                     │
│                     │Next: Wed 2026-10-21 03:30, Thu 2026-10-22 03:30, Fri 2026-10-23│                     │
│                     │03:30                                                           │                     │
│                     │                                                                │                     │
│                     │                                                                │                     │
│                     └────────────────────────────────────────────────────────────────┘                     │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
Enter Next  Tab Next field  Shift+Tab Prev field  F2 Pick  F5 Check  Esc Cancel
```

- **Host**: where restic runs.
- **Dataset** and **Path**: the dataset, and optionally one directory inside it (`photos/2026`, relative to the dataset's root, no `..`). Blank backs up all of the dataset.

> **Safety:** every part of a Path must be a real folder. If one is a symlink, the run stops with an error before restic reads anything, because restic runs as root and would follow the link out of the snapshot (a `docs -> /` link with Path `docs/etc` would back up the host's own `/etc`). To look, the run uses `readlink`, which `zfsctl print-sudoers HOST --with restic` includes; a remote host set up before this check must have that line installed again, or its jobs that have a Path fail with a message saying so. Jobs without a Path are not affected.
- **Source**: which snapshot to back up. Press `F2` to choose; it lists what the dataset already has.
  - `new` (the default for a new job) takes a **fresh snapshot at the start of every run**, backs it up, and keeps the last few of the job's own snapshots (see **Snapshots to keep**). This is the easy way: nothing else to set up.
  - `@snap4` always backs up that one snapshot, whatever it is called (F2 lists every snapshot as `@name`). After the first run the job reports `skipped: up to date`; if the snapshot is later destroyed, the run **fails** so you notice.
  - Anything else is a **prefix**, such as `hourly`: the job backs up the newest `hourly-<timestamp>` snapshot and takes none itself, so pair it with a snapshot job that uses the same prefix. F2 lists the prefixes the dataset already uses.
- **Snapshots to keep**: only for `new`. The job destroys its own older snapshots, named `zit-<job id>-<timestamp>`, beyond this many; blank means 3. A snapshot with a hold (chapter 10) is kept and the run says `kept N (held)`. It never touches any other snapshot, and zfsctl asks you to confirm when you save a job that will start doing this.

Press `F2` on Source to see what the dataset has: `new`, then the prefixes in use (`hourly` here), then every snapshot as `@name`. Type to filter, Enter to choose.

```screen
┌zfsctl────────────────┐┌Jobs — daemon running (pid 74775)───────────────────────────────────────────────────┐
│Dashboard             ││Job               Host     Target         Schedule        Next run        Last      │
│Hosts                 ││scrub-local-tpool local    tpool          Weekly, Sunday  Sun 10-25 03:00 ok 2d ago │
│Pools                ┌New backup job──────────────────────────────────────────────────┐0-25 03:00 ok 2d ago │
│Datasets             │Backs up a read-only clone of a snapshot. Source: new = take a  │                     │
│Snapshots            │fresh snapshot each run and keep the last few of those; a prefix│                     │
│Disks                │= the newest <prefix>-<timestamp> (it takes none); @name =      │                     │
│Shares               │always that one snapshot. The repository must already exist     │                     │
│Boot Envs            │(restic init).                                                  │                     │
│Jobs                 │                                                                │                     │
│Cloud                │  Hos┌Pick a value (type filter, Enter OK, Esc cancel)─────┐    │                     │
│Alerts               │  Dat│new                                                  │    │                     │
│Settings             │  Pat│@sphrase1                                            │    │                     │
│                     │> Sou│@snap4                                               │    │                     │
│                     │  Sna│@snap2                                               │    │─────────────────────┘
│                     │  Rep│@snap1                                               │    │─────────────────────┐
│                     │  Pas│                                                     │    │                     │
│                     │  Kee│                                                     │    │                     │
│                     │  Sch└─────────────────────────────────────────────────────┘    │                     │
│                     │  Catch up (y/n): y                                             │                     │
│                     │                                                                │                     │
│                     │custom schedule                                                 │                     │
│                     │Next: Wed 2026-10-21 03:30, Thu 2026-10-22 03:30, Fri 2026-10-23│                     │
│                     │03:30                                                           │                     │
│                     │                                                                │                     │
│                     │                                                                │                     │
│                     └────────────────────────────────────────────────────────────────┘                     │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  type Filter  Enter OK  Esc Cancel
```
- **Repository**: restic's `-r` value: a path (`/mnt/usb/restic-tank`), `sftp:user@host:/path`, or `rclone:remote:path`.
- **Password file**: the absolute path of the file you made, on that host (the form asks for it after the repository).
- **Keep**: how many backups to keep, in restic's words: `7d 4w 12m 3y` is "seven dailies, four weeklies, twelve monthlies, three yearlies"; blank keeps everything.
- **Schedule** and **catch up**, as in chapter 14.

### Check before you save

Before the first scheduled run, press `F5` in the form. zfsctl tries to open the repository with the password file on that host, and shows the answer at the bottom of the form:

```screen
┌zfsctl────────────────┐┌Jobs — daemon running (pid 74775)───────────────────────────────────────────────────┐
│Dashboard             ││Job               Host     Target         Schedule        Next run        Last      │
│Hosts                ┌New backup job──────────────────────────────────────────────────┐0-25 03:00 ok 2d ago │
│Pools                │Backs up a read-only clone of a snapshot. Source: new = take a  │0-25 03:00 ok 2d ago │
│Datasets             │fresh snapshot each run and keep the last few of those; a prefix│                     │
│Snapshots            │= the newest <prefix>-<timestamp> (it takes none); @name =      │                     │
│Disks                │always that one snapshot. The repository must already exist     │                     │
│Shares               │(restic init).                                                  │                     │
│Boot Envs            │                                                                │                     │
│Jobs                 │  Host (where restic runs): local                               │                     │
│Cloud                │  Dataset:                                                      │                     │
│Alerts               │  Path inside the dataset (blank = all of it):                  │                     │
│Settings             │  Source (F2 to pick: new, a prefix, or @snapshot): new         │                     │
│                     │  Snapshots to keep (Source new only; blank = 3):               │                     │
│                     │  Repository (restic -r; no credentials in it):                 │                     │
│                     │/mnt/usb/restic-tank                                            │                     │
│                     │> Password file (absolute path on the host): /root/.restic-key  │─────────────────────┘
│                     │  Keep (e.g. 7d 4w 12m 3y 5l; blank = keep everything):         │─────────────────────┐
│                     │  Schedule: 30 3 * * *                                          │                     │
│                     │  Catch up (y/n): y                                             │                     │
│                     │                                                                │                     │
│                     │custom schedule                                                 │                     │
│                     │Next: Wed 2026-10-21 03:30, Thu 2026-10-22 03:30, Fri 2026-10-23│                     │
│                     │03:30                                                           │                     │
│                     │                                                                │                     │
│                     │Check: no repository at that address yet: create it once with   │                     │
│                     │`restic init`                                                   │                     │
│                     │                                                                │                     │
│                     │                                                                │                     │
│                     │                                                                │                     │
│                     └────────────────────────────────────────────────────────────────┘                     │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
Enter Next  Tab Next field  Shift+Tab Prev field  F5 Check  Esc Cancel
```

The check only reads: it never creates a repository (that stays your `restic init`) and never blocks saving, so a USB disk that is not plugged in yet does not stop you saving the job. It runs in the background and gives up after 45 seconds. Editing the host, repository or password file hides the answer, because it no longer describes what is on screen. What the answers mean:

| It says | Do this |
|---|---|
| `OK, the repository opens` | nothing: the program, the remote, the password file, the password and the repository all work |
| `no repository at that address yet` | create it once with `restic init` (see above), or fix the path |
| `the password file does not unlock this repository` | the file holds a different password than the one the repository was made with |
| `no readable password file at …` | create it on that host (not on the machine you are typing on), readable by root |
| `rclone remote '…' is not in root's rclone config` | run `rclone config` as root on that host, and use the remote's name |
| `rclone is not installed on …` | `pkg install rclone` on that host |
| `restic is not installed on …, or its sudoers entry does not allow it` | `pkg install restic`, and for a remote host see `zfsctl print-sudoers` |
| `no answer from … in 45 s` | the target did not respond: an unreachable sftp host, a disk that is not mounted |

Anything else is restic's own message, with the repository address hidden. A failed check is not a reason to panic: fix the cause and press `F5` again.

### Protect the password file

The password file holds the repository password as plain text, because a scheduled backup has nobody to type it. Anyone who can read it, and reach the repository, can open every backup. So `F5` also looks at the file itself and adds `Note:` lines under its answer:

```screen
┌zfsctl────────────────┐┌Jobs — daemon running (pid 74775)───────────────────────────────────────────────────┐
│Dashboard            ┌New backup job──────────────────────────────────────────────────┐run        Last      │
│Hosts                │Backs up a read-only clone of a snapshot. Source: new = take a  │0-25 03:00 ok 2d ago │
│Pools                │fresh snapshot each run and keep the last few of those; a prefix│0-25 03:00 ok 2d ago │
│Datasets             │= the newest <prefix>-<timestamp> (it takes none); @name =      │                     │
│Snapshots            │always that one snapshot. The repository must already exist     │                     │
│Disks                │(restic init).                                                  │                     │
│Shares               │                                                                │                     │
│Boot Envs            │  Host (where restic runs): local                               │                     │
│Jobs                 │  Dataset:                                                      │                     │
│Cloud                │  Path inside the dataset (blank = all of it):                  │                     │
│Alerts               │  Source (F2 to pick: new, a prefix, or @snapshot): new         │                     │
│Settings             │  Snapshots to keep (Source new only; blank = 3):               │                     │
│                     │  Repository (restic -r; no credentials in it):                 │                     │
│                     │/mnt/usb/restic-tank                                            │                     │
│                     │> Password file (absolute path on the host): /tmp/zit-note-key  │                     │
│                     │  Keep (e.g. 7d 4w 12m 3y 5l; blank = keep everything):         │                     │
│                     │  Schedule: 30 3 * * *                                          │                     │
│                     │  Catch up (y/n): y                                             │─────────────────────┘
│                     │                                                                │─────────────────────┐
│                     │custom schedule                                                 │                     │
│                     │Next: Wed 2026-10-21 03:30, Thu 2026-10-22 03:30, Fri 2026-10-23│                     │
│                     │03:30                                                           │                     │
│                     │                                                                │                     │
│                     │Check: no repository at that address yet: create it once with   │                     │
│                     │`restic init`                                                   │                     │
│                     │Note: the password file is in a home or temporary directory:    │                     │
│                     │keep it where only root can reach it, such as /root or          │                     │
│                     │/usr/local/etc                                                  │                     │
│                     │Note: the password file can be read or changed by other users   │                     │
│                     │(mode 644): run `chmod 600 /tmp/zit-note-key`                   │                     │
│                     │                                                                │                     │
│                     │                                                                │                     │
│                     │                                                                │                     │
│                     │                                                                │                     │
│                     │                                                                │                     │
└─────────────────────└────────────────────────────────────────────────────────────────┘─────────────────────┘
Enter Next  Tab Next field  Shift+Tab Prev field  F5 Check  Esc Cancel

```

| The note says | Do this |
|---|---|
| `in a home or temporary directory` | move it to `/root` or `/usr/local/etc`, where only root can reach it, and change the job's Password file |
| `can be read or changed by other users (mode 644)` | `chmod 600` on the file, as the note spells out |
| `owned by fb, not root` | `chown root` on the file |
| `permissions could not be checked from here` | the ssh user cannot see the file (a root-only directory such as `/root`); check it on the host: `ls -l`, it should show `-rw-------` and `root` |

The mode and owner come from a plain `stat`, run without `sudo`, so on the hub (where zfsctl runs as root) every file is seen, and on a remote host only what the ssh user can see. The notes are advice: they never block saving. Keep the password in your password manager too, and never inside a dataset you back up, or the password ends up in the backup it protects.

### How a run works

The job picks its snapshot (taking a new one first, for `new`), makes a temporary read-only clone of it, backs up the clone, and destroys the clone again, even if the backup fails or is cancelled. Backing up a snapshot rather than the live files means the backup is **consistent**: it is exactly the data at one moment, even if the files are changing. If the repository already holds that snapshot, the run is `skipped: up to date`. Because the clone always has the same path, restic recognises unchanged files and does not read them again.

If you set **Keep**, a successful backup is followed by a restic *forget and prune* limited to this job's own backups; without it nothing is ever deleted.

## Choose where it goes

A *repository* can live almost anywhere. Pick the way that suits you:

| Where | Repository value | Needs |
|---|---|---|
| A local or USB disk | `/mnt/usb/restic-tank` | the disk mounted |
| Another machine over ssh | `sftp:backup@nas:/srv/restic` | root's ssh key authorised on the target, and its host key accepted once |
| **A cloud service, through rclone** | `rclone:myremote:bucket/path` | `rclone` installed and the remote set up in root's rclone config |

rclone speaks to dozens of services: Amazon S3 and compatible stores (Wasabi, Cloudflare R2, MinIO), Backblaze B2, Google Drive, Dropbox, OneDrive and many more. You configure the remote once with `rclone config` as root (`/root/.config/rclone/rclone.conf` holds its keys), and zfsctl only needs the name.

> **Tip:** zfsctl does not accept a repository with a password or key in its text (`rest:https://user:pass@host/` is refused), and it does not support restic's direct `s3:`/`b2:` addresses with environment variables, because secrets would then have to travel through `sudo`. Using rclone for the cloud keeps the keys in one root-only file. Cloud services often limit request rates and charge for requests, so schedule backups daily rather than hourly, and use a generous Keep: pruning a large remote repository is slow.

## Restoring

Use restic itself, which is the point: you can restore without zfsctl, even on another machine. For example:

```sh
restic restore latest --repo /mnt/usb/restic-tank --password-file /root/.restic-password \
      --tag zfsctl:backup-local-tank-data --target /restore
```

The backed-up path inside is `/var/run/zfsctl/<job id>`, followed by the job's **Path** if it has one. Test a restore of one file now and then; a backup you have never restored from is a hope, not a backup.

## Cancelling

`c` on a running backup stops it, destroys the temporary clone and leaves the repository fine (restic may leave a stale lock, which the next retention run removes safely). A cancelled run is not a failure.

## Remote hosts

On a host other than the hub, restic runs over ssh and `sudo`, so it needs restic installed and `/usr/local/bin/restic` added to that host's sudoers entry.

> **Safety:** that entry is as powerful as `zfs` or `bectl`: restic running as root can read any file and write anywhere, so whoever holds the ssh key effectively has root on that host. Run `zfsctl print-sudoers` to see the entry, and decide.

## Mirror to the cloud as plain files

A restic repository is not something you can open in a web browser: it is encrypted blocks. Sometimes you want the opposite: **your photos and documents sitting in a bucket as ordinary files**, so you can open the cloud console, browse to a folder and download one picture. That is the **cloud mirror job** (`C` in Jobs). It uses [rclone](https://rclone.org), the same tool as above, but copies plain files instead of building a repository.

Press `C` in Jobs:

```screen
┌zfsctl────────────────┐┌Jobs — daemon running (pid 74775)───────────────────────────────────────────────────┐
│Dashboard             ││Job               Host     Target         Schedule        Next run        Last      │
│Hosts                 ││scrub-local-tpool local    tpool          Weekly, Sunday  Sun 10-25 03:00 ok 2d ago │
│Pools                ┌New cloud mirror job────────────────────────────────────────────┐0-25 03:00 ok 2d ago │
│Datasets             │Copies a read-only clone of a snapshot to <remote>/current as   │                     │
│Snapshots            │plain files. What a run replaces or deletes is first moved to   │                     │
│Disks                │<remote>/old/<date and time>/; zfsctl never deletes anything in │                     │
│Shares               │old/. Symlinks, devices, empty folders and child datasets are   │                     │
│Boot Envs            │not copied.                                                     │                     │
│Jobs                 │                                                                │                     │
│Cloud                │> Host (where rclone runs): local                               │                     │
│Alerts               │  Dataset:                                                      │                     │
│Settings             │  Path inside the dataset (blank = all of it):                  │                     │
│                     │  Source (F2 to pick: new, a prefix, or @snapshot): new         │                     │
│                     │  Snapshots to keep (Source new only; blank = 3):               │─────────────────────┘
│                     │  Remote (rclone name:path, e.g. aws:mybucket/photos; no keys in│─────────────────────┐
│                     │it):                                                            │                     │
│                     │  Speed limit (bytes/s, e.g. 5M; blank = none):                 │                     │
│                     │  Schedule: 30 3 * * *                                          │                     │
│                     │  Catch up (y/n): y                                             │                     │
│                     │                                                                │                     │
│                     │custom schedule                                                 │                     │
│                     │Next: Wed 2026-10-21 03:30, Thu 2026-10-22 03:30, Fri 2026-10-23│                     │
│                     │03:30                                                           │                     │
│                     │                                                                │                     │
│                     │                                                                │                     │
│                     └────────────────────────────────────────────────────────────────┘                     │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
Enter Next  Tab Next field  Shift+Tab Prev field  F2 Pick  F5 Check  Esc Cancel
```

The form asks for the same things as the backup job (host, dataset, an optional path inside it, which snapshot to read, how many fresh snapshots to keep), plus:

- **Remote:** where to send it, written `name:path`, such as `aws:mybucket/photos` or `b2:mybucket/photos`. The name is a remote you created with `rclone config`; the keys live in root's rclone config on that host, never in zfsctl. A text with keys in it is refused. The part after the colon must name the bucket (or a folder on the remote): a bare `aws:` is refused, and so is a `.` or `..` part.
- **Speed limit:** optional, in bytes per second, such as `5M`, so a nightly upload does not use your whole connection. Schedule the job for the night if the connection matters; there is no time-of-day setting.

Press `F5` in the form to check the remote before you save. It lists the remote read-only and tells you in plain words if the remote name is not set up, the bucket is missing or the keys are refused.

```screen
┌zfsctl────────────────┐┌Jobs — daemon running (pid 32361)───────────────────────────────────────────────────┐
│Dashboard            ┌New cloud mirror job────────────────────────────────────────────┐run        Last      │
│Hosts                │Copies a read-only clone of a snapshot to <remote>/current as   │0-25 03:00 ok 2d ago │
│Pools                │plain files. What a run replaces or deletes is first moved to   │0-25 03:00 ok 2d ago │
│Datasets             │<remote>/old/<date and time>/; zfsctl never deletes anything in │                     │
│Snapshots            │old/. Symlinks, devices, empty folders and child datasets are   │                     │
│Disks                │not copied.                                                     │                     │
│Shares               │                                                                │                     │
│Boot Envs            │  Host (where rclone runs): local                               │                     │
│Jobs                 │  Dataset: scratch/data                                         │                     │
│Cloud                │  Path inside the dataset (blank = all of it):                  │                     │
│Alerts               │  Source (F2 to pick: new, a prefix, or @snapshot): new         │                     │
│Settings             │  Snapshots to keep (Source new only; blank = 3):               │                     │
│                     │> Remote (rclone name:path, e.g. aws:mybucket/photos; no keys in│                     │
│                     │it): b2:zfsctl-b2/photos                                        │─────────────────────┘
│                     │  Speed limit (bytes/s, e.g. 5M; blank = none):                 │─────────────────────┐
│                     │  Schedule: 30 3 * * *                                          │                     │
│                     │  Catch up (y/n): y                                             │                     │
│                     │                                                                │                     │
│                     │custom schedule                                                 │                     │
│                     │Next: Wed 2026-10-21 03:30, Thu 2026-10-22 03:30, Fri 2026-10-23│                     │
│                     │03:30                                                           │                     │
│                     │                                                                │                     │
│                     │Check: OK, the remote opens.                                    │                     │
│                     │                                                                │                     │
│                     │                                                                │                     │
│                     │                                                                │                     │
│                     └────────────────────────────────────────────────────────────────┘                     │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
Enter Next  Tab Next field  Shift+Tab Prev field  F5 Check  Esc Cancel
```

### How the bucket looks

```text
mybucket/photos/current/                       <- always equals the dataset
mybucket/photos/old/20261017T033000Z/...       <- what that run replaced or deleted
mybucket/photos/old/20261018T033000Z/...
```

Each run makes `current` match the dataset. A run never writes into an `old/` folder that already exists: if two runs ever got the same date and time, the second stops with an error and tries again with a new name next time. Before it replaces or deletes a file there, it **moves** the old copy into a folder named for the run's date and time (UTC). zfsctl never expires or deletes anything in `old/`: when the bucket gets big, you delete old folders yourself, in the cloud console or with `rclone purge`.

> **Safety:** a run whose source has **no files** (an empty dataset, a wrong path, nothing but symlinks) stops with an error and changes nothing, so a broken clone can never look like "every file was deleted". zfsctl only runs `rclone sync` (with the `old/` folder) and read-only listings: never `delete`, `purge` or `cleanup`.

> **Safety:** a mirror copies **one dataset**: its **child datasets are not included** (`tank/data/photos` is a different dataset from `tank/data`, and the run still reports OK). Make one mirror job per dataset you want in the bucket, each with its own remote folder. A *Path inside the dataset* must be made of real folders: if any part of it is a symlink, the run stops with an error before anything is read, because rclone would otherwise follow the link as root and could upload files from elsewhere on the host.

> **Safety:** only regular files are copied. **Symlinks, sockets, devices and empty folders are not**, and file owners and permissions are not kept in a bucket. A run that skipped only such things still counts as OK and does not mention them. If you rely on symlinks, use the restic backup instead.

### Set up the cloud account

zfsctl has been tried end to end against a real Backblaze B2 bucket and a real AWS S3 bucket (first copy, a later run that replaced and deleted files, the check), using exactly the keys and policy below. With any other provider, treat your first run as a test: mirror a small dataset first and look at the bucket.

**AWS S3.** Create a bucket (any region; leave "block public access" on). In IAM create a user that can only use that bucket, with a policy like this (replace `mybucket`):

```json
{
  "Version": "2012-10-17",
  "Statement": [
    { "Effect": "Allow", "Action": ["s3:ListBucket"], "Resource": "arn:aws:s3:::mybucket" },
    { "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"], "Resource": "arn:aws:s3:::mybucket/*" }
  ]
}
```

The policy does not allow creating buckets, and zfsctl does not ask for it: the bucket must already exist. Create an access key for that user. Optionally turn on **bucket versioning** in the bucket's properties: S3 then keeps every older version of every object on its own, a second safety net behind `old/`.

**Backblaze B2.** Create a private bucket. Under *Application Keys* create a key restricted to that one bucket, allowing list, read, write and delete files. Note the key ID and the key (it is shown once). **A B2 bucket keeps every version of every file by default**, so a file moved into `old/` also leaves a hidden version behind in `current/` and you pay for both. Set the bucket's lifecycle to *Keep only the last version of the file* unless you want that second hidden history.

> **Tip:** the key must be allowed to delete: moving a file into `old/` is a copy followed by a delete of the original. A key that cannot delete will make every run that replaces a file fail, which is annoying but safe.

**Tell rclone.** On the host that runs the job, as root, run `rclone config`, choose *New remote*, give it a name (`aws` or `b2`), pick the provider (`s3` with provider *AWS*, or `b2`) and paste the key. The keys are saved in `/root/.config/rclone/rclone.conf`, which must be readable by root only (`chmod 600`; the `F5` check tells you if it is not). On a host other than the hub also run `zfsctl print-sudoers HOST --with rclone` and install the line it prints. Do not use an rclone `crypt` remote if you want to browse the files: it encrypts the names too.

### Seeing your cloud copies: the Cloud screen

You should not have to remember which buckets you have set up. The **Cloud** section (between Jobs and Alerts in the sidebar) lists every rclone remote on the hub, plus the remote of any job on another host, with the jobs that write to it:

```screen
┌zfsctl────────────────┐┌Cloud — 3 remotes, 0 jobs─────────────────────────────────────────────────┐
│Dashboard             ││Provider         Remote         Host     Jobs Last         In bucket      │
│Hosts                 ││Backblaze B2     b2:            local    0    -            -              │
│Pools                 ││SFTP             t1:            local    0    -            -              │
│Datasets              ││Amazon S3 (etc.) zfsctl-aws:    local    0    -            -              │
│Snapshots             ││                                                                          │
│Disks                 ││                                                                          │
│Shares                ││                                                                          │
│Boot Envs             ││                                                                          │
│Jobs                  ││                                                                          │
│Cloud                 ││                                                                          │
│Alerts                ││                                                                          │
│Settings              ││                                                                          │
│                      ││                                                                          │
│                      │└──────────────────────────────────────────────────────────────────────────┘
│                      │┌Remote────────────────────────────────────────────────────────────────────┐
│                      ││nothing selected                                                          │
│                      ││                                                                          │
│                      ││                                                                          │
│                      ││                                                                          │
│                      ││                                                                          │
│                      ││                                                                          │
│                      ││                                                                          │
│                      ││                                                                          │
│                      ││                                                                          │
│                      ││                                                                          │
│                      ││                                                                          │
│                      ││                                                                          │
└──────────────────────┘└──────────────────────────────────────────────────────────────────────────┘
↑↓ Move  C New cloud mirror  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

- **Provider, Remote, Host:** the provider behind the remote (`Backblaze B2`, `Amazon S3 (etc.)`, `Google Drive`, `SFTP`...), the name you gave in `rclone config`, and the host whose rclone config has it. rclone says only the *type* of a remote, so every `s3` remote reads `Amazon S3 (etc.)`, whether it is Amazon, Wasabi, Cloudflare R2 or your own MinIO, and a type zfsctl has no name for is shown as rclone calls it. Only names and types are read (`rclone listremotes --long`): no key is ever read into the screen.
- **Jobs and Last:** how many cloud mirror jobs (and restic jobs that use an `rclone:` repository) write to the remote, and how the latest run of any of them ended. A remote whose latest run failed turns red, and the panel below says why. A remote nobody uses is dim: it is set up but unused.
- **In bucket:** blank (`-`) until you press `s`. It then adds up what that remote's mirror jobs hold in the bucket, `current/` and `old/` together, because that is what a plan charges for. With a plan cap set (below) it also says how much of the cap that is.
- **The panel:** the selected remote's jobs, and after `s` the size of `current/` and of `old/` for each, so you can see when `old/` is getting big. `F5` runs the same read-only check as in the form.

**Seeing how much of a free plan you have used.** Backblaze's free account gives 10 GB and AWS's free tier 5 GB. Tell zfsctl once, on the Settings screen or in `/usr/local/etc/zfsctl/config.toml`, by remote name:

```toml
[cloud.caps]
b2 = "10G"
zfsctl-aws = "5G"
```

After you press `s`, the table says `1.1 MiB of 10 GB (0.01%)` and the panel says how much is left. The line turns yellow from 80% and red from 95%, so you notice before an upload fails. Two limits to know: the numbers cover only what zfsctl's mirror jobs put in the bucket (restic repositories and anything else on the account are not counted), and the cap is a number you typed, not one zfsctl asked the provider for. A cap that is not a size such as `10G` is reported on screen and ignored.

> **Tip:** `s` is on demand because measuring a bucket means listing every file in it, which a cloud account may count as requests, and on a free account those are limited. The screen never does it by itself, and says how long ago it was measured.

> **Safety:** the Cloud screen only reads. It cannot create, change or delete a remote or anything in a bucket.

A host other than the hub is asked only if it has a cloud job, and only if it has the `rclone` sudoers line (`zfsctl print-sudoers HOST --with rclone`); otherwise its row says why in the title instead of failing quietly.

### Restoring

There is nothing special to restore from: open the bucket and download. From a shell, `rclone copy aws:mybucket/photos/current /restore` copies everything back, and an older version of one file is in the `old/<date and time>/` folder of the run that replaced it.

> **Safety:** the sudoers line for rclone (`zfsctl print-sudoers HOST --with rclone`) cannot limit rclone's arguments, so whoever holds zfsctl's ssh key on that host can also run `rclone purge` on your bucket or read the keys in root's rclone config: it is as powerful as root. The promise that "zfsctl never deletes anything in `old/`" is about what zfsctl itself runs. For a bucket you cannot afford to lose, turn on versioning (S3) or keep the lifecycle's hidden history (B2), and use a key that cannot touch anything else.

> **Safety:** two mirror jobs cannot have overlapping remotes, on one host or on different hosts (the same remote name and path on two hosts counts as one bucket), and a mirror cannot contain or sit inside a restic job's `rclone:` repository: the sync would move the repository's files into `old/`. zfsctl refuses to save or enable such a job. It cannot tell that two remote names (`aws:` and `aws2:`) point to one bucket, so do not do that. The opposite mistake is possible too: if two hosts use one remote name for two different accounts, zfsctl refuses the second job; give the remotes different names to tell it they are different places.

## Try it

Make a small repository on the practice pool, `restic init` it, create a backup job for the practice dataset with Source `new`, wait for it to run, and restore one file with the command above.

## Where next

- [Alerts](16-alerts.md): get told when a backup fails.
- [Recipes](17-recipes.md): "back up to the cloud".
