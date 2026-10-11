# Backups with restic

## What you will learn

- Why snapshots and replication are not yet a backup, and what restic adds.
- How to set up a repository and a backup job, and restore from it.
- How to send backups to the cloud through rclone.

## Why another layer

Snapshots live in the same pool as the data: if the pool dies, so do they. Replication ([chapter 14](14-jobs.md)) puts a copy on another machine, which is good, but it copies your mistakes too. A real backup is **a separate copy, in a different place, in a different format, that you can restore from by itself**. zfsctl's backup job uses [restic](https://restic.net), a well-regarded open-source backup tool: encrypted, deduplicated, and able to store its repository on a USB disk, another machine over sftp, or a cloud service.

> **Coming from Linux:** you may already use restic or borg; this runs restic for you on a schedule, from a snapshot. **Coming from Windows:** this is File History or Windows Backup, but encrypted and stored where you choose.

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
┌zfsctl────────────────┐┌Jobs — daemon running (pid 1698)────────────────────────────────────────────────────┐
│Hosts                 ││Job               Host     Target         Schedule        Next run        Last      │
│Pools                 ││scrub-local-tpool local    tpool          Weekly, Sunday  Sun 10-18 03:00 ok 8h ago │
│Datasets             ┌New backup job──────────────────────────────────────────────────┐0-12 10:41 skipped 24│
│Snapshots            │Backs up a read-only clone of a snapshot. Source: new = take a  │                     │
│Disks                │fresh snapshot each run and keep the last few of those; a prefix│                     │
│Shares               │= the newest <prefix>-<timestamp> (it takes none); @name =      │                     │
│Boot Envs            │always that one snapshot. The repository must already exist     │                     │
│Jobs                 │(restic init).                                                  │                     │
│Alerts               │                                                                │                     │
│                     │> Host (where restic runs): local                               │                     │
│                     │  Dataset:                                                      │                     │
│                     │  Path inside the dataset (blank = all of it):                  │                     │
│                     │  Source (F2 to pick: new, a prefix, or @snapshot): new         │                     │
│                     │  Snapshots to keep (Source new only; blank = 3):               │─────────────────────┘
│                     │  Repository (restic -r; no credentials in it):                 │─────────────────────┐
│                     │  Password file (absolute path on the host):                    │                     │
│                     │  Keep (e.g. 7d 4w 12m 3y 5l; blank = keep everything):         │                     │
│                     │  Schedule: 30 3 * * *                                          │                     │
│                     │  Catch up (y/n): y                                             │                     │
│                     │                                                                │                     │
│                     │custom schedule                                                 │                     │
│                     │Next: Mon 2026-10-12 03:30, Tue 2026-10-13 03:30, Wed 2026-10-14│                     │
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
- **Source**: which snapshot to back up. Press `F2` to choose; it lists what the dataset already has.
  - `new` (the default for a new job) takes a **fresh snapshot at the start of every run**, backs it up, and keeps the last few of the job's own snapshots (see **Snapshots to keep**). This is the easy way: nothing else to set up.
  - `@snap4` always backs up that one snapshot, whatever it is called (F2 lists every snapshot as `@name`). After the first run the job reports `skipped: up to date`; if the snapshot is later destroyed, the run **fails** so you notice.
  - Anything else is a **prefix**, such as `hourly`: the job backs up the newest `hourly-<timestamp>` snapshot and takes none itself, so pair it with a snapshot job that uses the same prefix. F2 lists the prefixes the dataset already uses.
- **Snapshots to keep**: only for `new`. The job destroys its own older snapshots, named `zit-<job id>-<timestamp>`, beyond this many; blank means 3. It never touches any other snapshot, and zfsctl asks you to confirm when you save a job that will start doing this.

Press `F2` on Source to see what the dataset has: `new`, then the prefixes in use (`hourly` here), then every snapshot as `@name`. Type to filter, Enter to choose.

```screen
┌zfsctl────────────────┐┌Jobs — daemon running (pid 1698)────────────────────────────────────────────────────┐
│Hosts                 ││Job               Host     Target         Schedule        Next run        Last      │
│Pools                 ││scrub-local-tpool local    tpool          Weekly, Sunday  Sun 10-18 03:00 ok 8h ago │
│Datasets             ┌New backup job──────────────────────────────────────────────────┐0-12 10:41 skipped 25│
│Snapshots            │Backs up a read-only clone of a snapshot. Source: new = take a  │                     │
│Disks                │fresh snapshot each run and keep the last few of those; a prefix│                     │
│Shares               │= the newest <prefix>-<timestamp> (it takes none); @name =      │                     │
│Boot Envs            │always that one snapshot. The repository must already exist     │                     │
│Jobs                 │(restic init).                                                  │                     │
│Alerts               │                                                                │                     │
│                     │  Hos┌Pick a value (type filter, Enter OK, Esc cancel)─────┐    │                     │
│                     │  Dat│new                                                  │    │                     │
│                     │  Pat│hourly                                               │    │                     │
│                     │> Sou│@manual1                                             │    │                     │
│                     │  Sna│@hourly-20261010T130000Z                             │    │─────────────────────┘
│                     │  Rep│@hourly-20261010T120000Z                             │    │─────────────────────┐
│                     │  Pas│                                                     │    │                     │
│                     │  Kee│                                                     │    │                     │
│                     │  Sch└─────────────────────────────────────────────────────┘    │                     │
│                     │  Catch up (y/n): y                                             │                     │
│                     │                                                                │                     │
│                     │custom schedule                                                 │                     │
│                     │Next: Mon 2026-10-12 03:30, Tue 2026-10-13 03:30, Wed 2026-10-14│                     │
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
┌zfsctl────────────────┐┌Jobs — daemon running (pid 1698)────────────────────────────────────────────────────┐
│Hosts                 ││Job               Host     Target         Schedule        Next run        Last      │
│Pools                ┌New backup job──────────────────────────────────────────────────┐0-18 03:00 ok 8h ago │
│Datasets             │Backs up a read-only clone of a snapshot. Source: new = take a  │0-12 10:41 skipped 28│
│Snapshots            │fresh snapshot each run and keep the last few of those; a prefix│                     │
│Disks                │= the newest <prefix>-<timestamp> (it takes none); @name =      │                     │
│Shares               │always that one snapshot. The repository must already exist     │                     │
│Boot Envs            │(restic init).                                                  │                     │
│Jobs                 │                                                                │                     │
│Alerts               │  Host (where restic runs): local                               │                     │
│                     │  Dataset:                                                      │                     │
│                     │  Path inside the dataset (blank = all of it):                  │                     │
│                     │  Source (F2 to pick: new, a prefix, or @snapshot): new         │                     │
│                     │  Snapshots to keep (Source new only; blank = 3):               │                     │
│                     │  Repository (restic -r; no credentials in it):                 │                     │
│                     │/mnt/usb/restic-tank                                            │                     │
│                     │> Password file (absolute path on the host): /root/.restic-key  │─────────────────────┘
│                     │  Keep (e.g. 7d 4w 12m 3y 5l; blank = keep everything):         │─────────────────────┐
│                     │  Schedule: 30 3 * * *                                          │                     │
│                     │  Catch up (y/n): y                                             │                     │
│                     │                                                                │                     │
│                     │custom schedule                                                 │                     │
│                     │Next: Mon 2026-10-12 03:30, Tue 2026-10-13 03:30, Wed 2026-10-14│                     │
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
┌zfsctl────────────────┐┌Jobs — daemon running (pid 9108)────────────────────────────────────────────────────┐
│Hosts                ┌New backup job──────────────────────────────────────────────────┐run        Last      │
│Pools                │Backs up a read-only clone of a snapshot. Source: new = take a  │0-18 03:00 ok 10h ago│
│Datasets             │fresh snapshot each run and keep the last few of those; a prefix│                     │
│Snapshots            │= the newest <prefix>-<timestamp> (it takes none); @name =      │                     │
│Disks                │always that one snapshot. The repository must already exist     │                     │
│Shares               │(restic init).                                                  │                     │
│Boot Envs            │                                                                │                     │
│Jobs                 │  Host (where restic runs): local                               │                     │
│Alerts               │  Dataset:                                                      │                     │
│                     │  Path inside the dataset (blank = all of it):                  │                     │
│                     │  Source (F2 to pick: new, a prefix, or @snapshot): new         │                     │
│                     │  Snapshots to keep (Source new only; blank = 3):               │                     │
│                     │  Repository (restic -r; no credentials in it):                 │                     │
│                     │/mnt/usb/restic-tank                                            │                     │
│                     │> Password file (absolute path on the host): /tmp/zit-note-key  │                     │
│                     │  Keep (e.g. 7d 4w 12m 3y 5l; blank = keep everything):         │                     │
│                     │  Schedule: 30 3 * * *                                          │                     │
│                     │  Catch up (y/n): y                                             │─────────────────────┘
│                     │                                                                │─────────────────────┐
│                     │custom schedule                                                 │                     │
│                     │Next: Mon 2026-10-12 03:30, Tue 2026-10-13 03:30, Wed 2026-10-14│                     │
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

## Try it

Make a small repository on the practice pool, `restic init` it, create a backup job for the practice dataset with Source `new`, wait for it to run, and restore one file with the command above.

## Where next

- [Alerts](16-alerts.md): get told when a backup fails.
- [Recipes](17-recipes.md): "back up to the cloud".
