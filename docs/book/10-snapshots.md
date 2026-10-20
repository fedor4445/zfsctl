# Snapshots

## What you will learn

- What a snapshot costs, and how to read the Snapshots list and panel.
- How to take, destroy and roll back to a snapshot.
- How to protect a snapshot with a hold.
- How to see what changed between two snapshots, folder by folder.
- How scheduled snapshots are named, so you can recognise them.

## The list

A snapshot is a read-only picture of a dataset at one moment. Go to Snapshots and select one:

```screen
┌zfsctl────────────────┐┌Snapshots — local ('r' to refresh)──────────────────────────────────────────────────┐
│Dashboard             ││Snapshot                           Used     Hold                                    │
│Hosts                 ││scratch/data@snap1                 64.0K                                            │
│Pools                 ││scratch/data@snap2                 64.0K                                            │
│Datasets              ││scratch/data@snap4                 72.0K                                            │
│Snapshots             ││scratch/data@sphrase1              56.0K                                            │
│Disks                 ││scratch/data@before-cleanup        0B       H                                       │
│Shares                ││tpool@snap1                        93.0K                                            │
│Boot Envs             ││tpool@snap2                        2.0G                                             │
│Jobs                  ││zroot@09052026                     80.0K                                            │
│Cloud                 ││zroot/ROOT@snap1                   0B                                               │
│Alerts                ││zroot/home/alice@alice             8.0K                                             │
│Settings              ││zroot/home/fb@09052026             88.0K                                            │
│                      ││zroot/home/fb@09062026             88.0K                                            │
│                      ││zroot/home/carol@carol1            0B                                               │
│                      ││zroot/home/carol@snap5             0B                                               │
│                      ││zroot/var@snap1                    0B                                               │
│                      │└────────────────────────────────────────────────────────────────────────────────────┘
│                      │┌Metrics — scratch/data@before-cleanup───────────────────────────────────────────────┐
│                      ││Snapshot  before-cleanup                      Snapshot sizes                        │
│                      ││Created   1m ago                              oldest → newest, this one lit         │
│                      ││Used      0B   Refer 100M   Written 56.0K       █                                   │
│                      ││Reclaim   0B  (freed if destroyed)            ▄▄█                                   │
│                      ││Holds     1   Clones -  (blocks destroy)      ███                                   │
│                      ││Dataset   5 snaps, 256K = <1% of used         ████                                  │
│                      ││Oldest    44d 19h ago                         ████                                  │
│                      ││Newest    1m ago                              ████                                  │
│                      ││                                              ████                                  │
│                      ││                                              ████                                  │
│                      ││                                              ████                                  │
│                      ││                                              ████                                  │
│                      ││                                              ████                                  │
│                      ││                                              ████                                  │
│                      ││                                              ████                                  │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  d Destroy  b Rollback  H Hold  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

The list shows every snapshot on the host, named `dataset@name`, with the space it **Used**: the space that would be freed if you destroyed only that snapshot. A snapshot that has just been taken uses nothing; it grows as the dataset changes, because it keeps the old blocks. The **Written** column is how much new data went into the dataset since the snapshot before it: a very large number marks a busy period. (Sizes are in human units, like everywhere else.)

## The panel

The panel under the list explains the selected snapshot:

| Line | Meaning |
|---|---|
| **Created** | how long ago it was taken |
| **Used**, **Refer**, **Written** | its own space, the space it refers to, and what was written since the previous snapshot |
| **Reclaim** | what destroying it would free (this can differ from Used when snapshots share blocks) |
| **Holds**, **Clones** | whether it is protected from destruction by a hold, and whether a clone depends on it |
| **Dataset** | how many snapshots the dataset has and what share of its space they hold |
| **Oldest**, **Newest** | the age of the dataset's oldest and newest snapshot |

On the right, a small chart of the dataset's snapshot sizes, oldest to newest, with the selected one lit.

## Take a snapshot

Snapshots are taken from the Datasets section. Select the dataset and press `s`:

```screen
┌zfsctl────────────────┐┌Datasets — local ('r' refresh, → for properties)─┐┌Properties — scratch/data ('r' re┐
│Dashboard             ││Name                     Used    Avail    Kind   ││Property    Value      Source    │
│Hosts                 ││scratch                  103M    17.3M    Filesys││                                 │
│Pools                 ││scratch/data             100M    17.3M    Filesys││                                 │
│Datasets              ││scratch/vault            408K    17.3M    Filesys││                                 │
│Snapshots             ││scratch/vault/docs       200K    17.3M    Filesys││                                 │
│Disks                 ││tpool                    3.9G    5.2G     Filesys││                                 │
│Shares                ││zroot                    10.7G   30.5G    Filesys││                                 │
│Boot Envs             ││zroot/ROOT               5.3G    30.5G    Filesys││                                 │
│Jobs                  ││zroot/ROOT/default       5.3G    30.5G    Filesys││                                 │
│Cloud                 ││zroot/home               595M    30.5G    Filesys││                                 │
│Alerts               ┌Snapshot dataset 'scratch/data'─────────────────────────────────┐                     │
│Settings             │> Snapshot name:                                                │                     │
│                     │                                                                │─────────────────────┘
│                     │                                                                │─────────────────────┐
│                     └────────────────────────────────────────────────────────────────┘%  100M of 118M      │
│                      ││0 ops  lat -               0 ops  lat -              Refer   100M   Logical 100M    │
│                      ││                                                     Data    100M   Snaps 256K      │
│                      ││                                                     Kids    0B   Refres 0B         │
│                      ││                                                     Compr   on 1.00x   Rec 128K    │
│                      ││                                                     Quota   -   Ref -              │
│                      ││                                                     Pool    ONLINE  errs 0         │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
Enter Submit  Tab Next field  Shift+Tab Prev field  Esc Cancel
```

Type a name (`before-upgrade`, `2026-10-10`) and press `Enter`. It is taken at once. A good name says why: you will be glad of it later.

For regular snapshots you do not have to remember: chapter 14 shows how to schedule them with `J`.

## Destroy a snapshot

Select it in the Snapshots list and press `d`. You **type its full name** (`tank/home@before-upgrade`) to confirm. Destroying a snapshot frees the space only it held; the dataset itself is not touched.

## Roll back

Press `b` on a snapshot to roll its dataset back to that moment. You **type the full snapshot name** to confirm.

> **Safety:** a rollback **throws away everything written after the snapshot**, including any later snapshots. If you only need one old file, do not roll back: every snapshot is readable at `/<mountpoint>/.zfs/snapshot/<name>/`, so you can copy the file out. Use `b` when you really want the whole dataset to go back.

## Protecting a snapshot

A **hold** is a lock that ZFS itself puts on a snapshot. While a snapshot is held it cannot be destroyed, not even by root, until someone releases the hold. Use it when you want to keep a snapshot safe while you investigate something: the state before an upgrade, the last good copy before you chase a problem.

The Snapshots list has a **Hold** column:

| Mark | Meaning |
|---|---|
| `H` | held with zfsctl's own tag, `zfsctl` |
| `h` | held by some other tag only (or the tags are not known yet) |
| blank | not held |

Select a snapshot and press `H`:

- On a snapshot that is not held, you get a **y/n** question: press `y` (or `n`), then `Enter`. zfsctl then takes a hold with the tag `zfsctl`.
- On a snapshot with zfsctl's tag, you **type its full name** to confirm, and zfsctl releases its own hold.
- On a snapshot held only by someone else's tag, nothing happens except a message: `held by another tag: <tags>; zfsctl will not release it`. If the tags have not been read yet, the message is `hold tags not loaded yet; press r`.

zfsctl only ever releases the tag `zfsctl`. Holds placed by other people or other tools are shown, and never released by zfsctl; release them yourself with `zfs release` if you are sure.

A held snapshot is refused everywhere zfsctl could destroy it:

- `d` on the snapshot shows `protected: ...` and destroys nothing.
- `d` or `D` on a dataset refuses if any of its snapshots is held.
- Snapshot jobs (retention), the old snapshots that a backup or cloud mirror job takes with source `new`, and the **Keep on target** pruning of a replication job all skip a held snapshot. The run's message says `kept N (held)`; this is not an error.

Because a hold is ZFS's own mechanism (`zfs hold`), it is enforced by ZFS and protects the snapshot even when zfsctl is not running. zfsctl checks for holds before it destroys anything, and refuses (including a recursive destroy that would remove a held snapshot of a child dataset); ZFS itself also refuses to destroy a held snapshot. zfsctl never uses the deferred destroy (`zfs destroy -d`), which would remove the snapshot later without asking. No extra sudoers entry is needed.

If zfsctl cannot find out whether a snapshot is held (the check fails, or the host's answer is not understood), it does not guess: it refuses to destroy it and the message starts with `could not check holds:`. This is different from a hold. A snapshot job or replication run that meets such a failure is shown as **Failed**, with the reason in its message, so a check that never works does not hide behind a green run (backup and cloud-mirror pruning of old snapshots stays green and says so in a note; chapter 19 lists the messages); fix the cause (read the reason in the message) and the next run prunes again.

> **Safety:** a hold protects one snapshot from being destroyed; it is not a backup. If the pool is lost, the held snapshot is lost with it (chapter 15).

## What changed between snapshots

When something has gone wrong (a folder emptied, a mass delete, a sync gone astray) the first question is *when*. Select a snapshot and press `c`: zfsctl compares it with the snapshot made just before it and shows what changed in between. Press `C` instead to compare it with the dataset **as it is now**, which answers "what has changed since this snapshot?".

The view opens with a header of counts, for example `2,505 removed · 12 added · 31 modified · 0 renamed`, and the time span the changes cover. Below it the changes are grouped by **folder**, busiest first, with one count per kind. Press `Enter` on a folder to list its entries, newest first. Each entry shows:

| Column | Meaning |
|---|---|
| **Change** | `+ added`, `- removed`, `~ modified` or `R renamed` (a rename shows the old and the new path) |
| **Type** | file, directory, symlink, and the rarer kinds (socket, pipe, device) |
| **Changed** | when ZFS last recorded a change to it. This is not the date the file was created, and the view does not show sizes: it tells you what changed and when, not how big it was |
| **Path** | where it is, written relative to the dataset (see below) |

`[` and `]` make the folders shallower or deeper (one to four levels; two is the start), `Esc` closes a folder and then the view, and `Up` and `Down` move. Everything here only **reads**: the view never changes your data.

The one thing you can do from it is press `H` to **hold the older snapshot**, the one that still has the files, so that nothing can destroy it while you work out what happened (see *Protecting a snapshot* above).

A few things worth knowing:

- **The oldest snapshot** of a dataset has nothing before it, so `c` says so. Press `Esc`, then `C`, to compare it with the live dataset.
- **Big changes take time.** The read stops after two million changes or two minutes, and then the header says `stopped after N changes` and that the counts are partial; what was read up to then is still shown. `Esc` cancels a read that is still running. At most 5,000 entries are listed per folder (`… N more` says how many are not).
- **Paths.** A host may print a path the way *it* sees it. A dataset given to a zone, for example, is printed through the zone's root directory. zfsctl cuts that off so you see paths relative to the dataset; if it cannot, the header says `paths as the host prints them`.
- **Odd lines.** A line zfsctl cannot read is counted (`N unreadable`), never guessed at.

> **Tip:** to find out *when* files vanished, move down the snapshot list one at a time with `c`: the snapshot where the `removed` count jumps is the first one made after the deletion, and the one before it is the one to hold.

## How scheduled snapshots are named

A snapshot **job** (chapter 14) names its snapshots `<prefix>-<timestamp>`, for example `auto-20261010T020000Z`, and it only ever deletes snapshots with its own prefix when it applies its retention (`keep`). It also keeps a held snapshot (see above). Your own snapshots, with other names, are never touched by a job. A backup job uses the newest snapshot with its prefix, which is why the two share a prefix.

## Try it

On the practice pool: take a snapshot `second` of `bookdemo/home` with `s`. Change a file from the shell. In Snapshots, select `bookdemo/home@second`, read the panel, and then roll back with `b`, typing the name. Check that the file is back as it was. When you have finished with the practice pool, destroy it from Pools with `d`, typing its name.

## Where next

- [Disks](11-disks.md) to see the hardware under your pools.
- [Scheduled jobs](14-jobs.md) to take snapshots automatically.
