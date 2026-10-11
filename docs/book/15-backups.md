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
┌zfsctl────────────────┐┌Jobs — daemon running (pid 10398)───────────────────────────────────────────────────┐
│Hosts                 ││Job               Host     Target         Schedule        Next run        Last      │
│Pools                 ││scrub-local-tpool local    tpool          Weekly, Sunday  Sun 10-18 03:00 ok 5h ago │
│Datasets              ││                                                                                    │
│Snapshots             ││                                                                                    │
│Disks                 ││                                                                                    │
│Shares                ││                                                                                    │
│Boot Envs             ││                                                                                    │
│Jobs                 ┌New backup job──────────────────────────────────────────────────┐                     │
│Alerts               │Backs up the newest <prefix>-<timestamp> snapshot (it never     │                     │
│                     │takes snapshots) through a temporary read-only clone. The       │                     │
│                     │repository must already exist (restic init).                    │                     │
│                     │                                                                │                     │
│                     │> Host (where restic runs): local                               │                     │
│                     │  Dataset:                                                      │                     │
│                     │  Prefix (snapshots to back up): auto                           │                     │
│                     │  Repository (restic -r; no credentials in it):                 │                     │
│                     │  Password file (absolute path on the host):                    │─────────────────────┘
│                     │  Keep (e.g. 7d 4w 12m 3y 5l; blank = keep everything):         │─────────────────────┐
│                     │  Schedule: 30 3 * * *                                          │                     │
│                     │  Catch up (y/n): y                                             │                     │
│                     │                                                                │                     │
│                     │custom schedule                                                 │                     │
│                     │Next: Mon 2026-10-12 03:30, Tue 2026-10-13 03:30, Wed 2026-10-14│                     │
│                     │03:30                                                           │                     │
│                     │                                                                │                     │
│                     └────────────────────────────────────────────────────────────────┘                     │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
Enter Next  Tab Next field  Shift+Tab Prev field  F2 Pick  F5 Check  Esc Cancel
```

- **Host**: where restic runs.
- **Dataset** and **Prefix**: the dataset, and which of its snapshots to back up. A backup job *never takes snapshots*; pair it with a snapshot job that uses the same prefix, and it backs up the newest one.
- **Repository**: restic's `-r` value: a path (`/mnt/usb/restic-tank`), `sftp:user@host:/path`, or `rclone:remote:path`.
- **Password file**: the absolute path of the file you made, on that host (the form asks for it after the repository).
- **Keep**: how many backups to keep, in restic's words: `7d 4w 12m 3y` is "seven dailies, four weeklies, twelve monthlies, three yearlies"; blank keeps everything.
- **Schedule** and **catch up**, as in chapter 14.

### Check before you save

Before the first scheduled run, press `F5` in the form. zfsctl tries to open the repository with the password file on that host, and shows the answer at the bottom of the form:

```screen
┌zfsctl────────────────┐┌Jobs — daemon running (pid 10398)───────────────────────────────────────────────────┐
│Hosts                 ││Job               Host     Target         Schedule        Next run        Last      │
│Pools                 ││scrub-local-tpool local    tpool          Weekly, Sunday  Sun 10-18 03:00 ok 5h ago │
│Datasets              ││                                                                                    │
│Snapshots             ││                                                                                    │
│Disks                 ││                                                                                    │
│Shares               ┌New backup job──────────────────────────────────────────────────┐                     │
│Boot Envs            │Backs up the newest <prefix>-<timestamp> snapshot (it never     │                     │
│Jobs                 │takes snapshots) through a temporary read-only clone. The       │                     │
│Alerts               │repository must already exist (restic init).                    │                     │
│                     │                                                                │                     │
│                     │  Host (where restic runs): local                               │                     │
│                     │  Dataset:                                                      │                     │
│                     │  Prefix (snapshots to back up): auto                           │                     │
│                     │  Repository (restic -r; no credentials in it):                 │                     │
│                     │/mnt/usb/restic-tank                                            │                     │
│                     │> Password file (absolute path on the host): /root/.restic-key  │                     │
│                     │  Keep (e.g. 7d 4w 12m 3y 5l; blank = keep everything):         │─────────────────────┘
│                     │  Schedule: 30 3 * * *                                          │─────────────────────┐
│                     │  Catch up (y/n): y                                             │                     │
│                     │                                                                │                     │
│                     │custom schedule                                                 │                     │
│                     │Next: Mon 2026-10-12 03:30, Tue 2026-10-13 03:30, Wed 2026-10-14│                     │
│                     │03:30                                                           │                     │
│                     │                                                                │                     │
│                     │Check: no repository at that address yet: create it once with   │                     │
│                     │`restic init`                                                   │                     │
│                     │                                                                │                     │
│                     └────────────────────────────────────────────────────────────────┘                     │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
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

### How a run works

The job finds the newest matching snapshot, makes a temporary read-only clone of it, backs up the clone, and destroys the clone again, even if the backup fails or is cancelled. Backing up a snapshot rather than the live files means the backup is **consistent**: it is exactly the data at one moment, even if the files are changing. If the repository already holds that snapshot, the run is `skipped: up to date`. Because the clone always has the same path, restic recognises unchanged files and does not read them again.

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

The backed-up path inside is `/var/run/zfsctl/<job id>`. Test a restore of one file now and then; a backup you have never restored from is a hope, not a backup.

## Cancelling

`c` on a running backup stops it, destroys the temporary clone and leaves the repository fine (restic may leave a stale lock, which the next retention run removes safely). A cancelled run is not a failure.

## Remote hosts

On a host other than the hub, restic runs over ssh and `sudo`, so it needs restic installed and `/usr/local/bin/restic` added to that host's sudoers entry.

> **Safety:** that entry is as powerful as `zfs` or `bectl`: restic running as root can read any file and write anywhere, so whoever holds the ssh key effectively has root on that host. Run `zfsctl print-sudoers` to see the entry, and decide.

## Try it

Make a small repository on the practice pool, `restic init` it, create a snapshot job and a backup job for the practice dataset, wait for it to run, and restore one file with the command above.

## Where next

- [Alerts](16-alerts.md): get told when a backup fails.
- [Recipes](17-recipes.md): "back up to the cloud".
