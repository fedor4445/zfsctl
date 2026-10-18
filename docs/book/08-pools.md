# Pools

## What you will learn

- How to read the Pools list, the panel and the vdev tree.
- How to scrub, create, extend, repair and destroy pools safely.
- How to read and edit pool properties.

## The list and the panel

Press `Tab` to reach Pools, then `Down` to select a pool.

```screen
┌zfsctl────────────────┐┌Pools — local ('r' refresh, → for properties)────┐┌Properties — ? ('r' refresh, Ente┐
│Hosts                 ││Name                  Health       Used          ││Property    Value      Source    │
│Pools                 ││scratch               ● ONLINE     ███░░░░░ 42%  ││                                 │
│Datasets              ││tpool                 ● ONLINE     ███░░░░░ 41%  ││                                 │
│Snapshots             ││zroot                 ● ONLINE     ██░░░░░░ 24%  ││                                 │
│Disks                 ││                                                 ││                                 │
│Shares                ││                                                 ││                                 │
│Boot Envs             ││                                                 ││                                 │
└──────────────────────┘└─────────────────────────────────────────────────┘└─────────────────────────────────┘
↑↓ Move  → Properties  n New pool  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

Each row shows the pool's **health** (`● ONLINE` and friends), a **capacity bar** with the percentage, and its size and allocated space. With a pool selected, the panel below shows its details:

```screen
┌zfsctl────────────────┐┌Pools — local ('r' refresh, → for properties)────┐┌Properties — scratch ('r' refresh┐
│Hosts                 ││Name                  Health       Used          ││Property    Value      Source    │
│Pools                 ││scratch               ● ONLINE     ███░░░░░ 42%  ││                                 │
│Datasets              ││tpool                 ● ONLINE     ███░░░░░ 41%  ││                                 │
│Snapshots             ││zroot                 ● ONLINE     ██░░░░░░ 24%  ││                                 │
│Disks                 ││                                                 ││                                 │
│Shares                ││                                                 ││                                 │
│Boot Envs             ││                                                 ││                                 │
│Jobs                  ││                                                 ││                                 │
│Alerts                ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      │└─────────────────────────────────────────────────┘└─────────────────────────────────┘
│                      │┌Metrics — scratch───────────────────────────────────────────────────────────────────┐
│                      ││Read  0B/s  peak 0B/s      Write 0B/s  peak 0B/s     Health  ● ONLINE               │
│                      ││0 ops  lat -               0 ops  lat -              Used    42%  102M / 240M       │
│                      ││                                                     Vdevs      1  (1 mirror)       │
│                      ││                                                     Disks      2  (2 data)         │
│                      ││                                                     Datasets   2 filesystems       │
│                      ││                                                     Snapshots  3                   │
│                      ││                                                     Compress   on 1.00x   Trim on  │
│                      ││                                                     Frag    40%   Dedup 1.00x      │
│                      ││                                                     Errors  R0 W0 C0               │
│                      ││                                                     Scrub   Oct  8 19:48:07 2026   │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  Enter Vdevs  → Properties  s Scrub  J Schedule  R Resilver  d Destroy  n New pool  ? Help  q Quit
```

On the left, the Read and Write graphs. On the right: the pool's **Health**, **Used** space, **Frag**mentation and **Dedup** ratio, the **Errors** (read, write, checksum counters), and the last **Scrub**. Above those, the make-up of the pool: **Vdevs** (how many top-level groups, and of which kind), **Disks** (how many physical disks, by role), **Datasets** (filesystems and volumes), **Snapshots** (a count only, never the list), and the **Compress**ion setting and ratio and whether **Trim** is on. The counts of datasets and snapshots are read at most every 30 seconds in the background, so they may show `…` for a moment; counting many thousands of snapshots can take a few seconds.

## Inside a pool: the vdev tree

Press `Enter` on a pool to drill in:

```screen
┌zfsctl────────────────┐┌Vdevs — tpool ('r' refresh, Esc/Backspace back)─────────────────────────────────────┐
│Hosts                 ││Device                         Health                                               │
│Pools                 ││tpool                          ● ONLINE                                             │
│Datasets              ││  raidz2-0                     ● ONLINE                                             │
│Snapshots             ││    ada1                       ● ONLINE                                             │
│Disks                 ││    ada2                       ● ONLINE                                             │
│Shares                ││    ada3                       ● ONLINE                                             │
│Boot Envs             ││    ada4                       ● ONLINE                                             │
│Jobs                  ││                                                                                    │
│Alerts                ││                                                                                    │
│                      │└────────────────────────────────────────────────────────────────────────────────────┘
│                      │┌Metrics — tpool─────────────────────────────────────────────────────────────────────┐
│                      ││Read  0B/s  peak 0B/s      Write 0B/s  peak 0B/s     Health  ● ONLINE               │
│                      ││0 ops  lat -               0 ops  lat -              Used    41%  8.1G / 19.5G      │
│                      ││                                                     Vdevs      1  (1 raidz2)       │
│                      ││                                                     Disks      4  (4 data)         │
│                      ││                                                     Datasets   1 filesystem        │
│                      ││                                                     Snapshots  2                   │
│                      ││                                                     Compress   on 1.00x   Trim off │
│                      ││                                                     Frag    2%   Dedup 1.00x       │
│                      ││                                                     +2 more lines: enlarge the ter…│
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
resilvered 501M in 00:00:30 with 0 errors on Sat Sep 26 13:15:14 2026
↑↓ Move  a Add vdev  r Refresh  Esc Back  ! Shell  A About  ? Help  q Quit
```

You see the pool, its vdevs (`raidz2-0`) and the disks in each, with their state. `Esc` or `Backspace` goes back. In this view:

| Key | What it does |
|---|---|
| `o` / `O` | take the selected disk offline / bring it online (y/n) |
| `x` | replace the selected disk with another (a form, then the question) |
| `a` | add a vdev to the pool (a form, then the question), including log, cache, special, dedup and spare devices |
| `X` | remove the selected log, cache or spare device (a question first) |

## Scrub and resilver

On the Pools list, `s` starts a **scrub** of the selected pool and `R` a **resilver**, each after a yes/no question. A scrub reads everything and verifies it against the checksums, repairing from redundancy; do it regularly (the next chapters show how to schedule it with `J`, so you never have to remember). While it runs, the panel and the Hosts list show the progress. A resilver is the repair that follows a replaced or returned disk; ZFS starts one by itself, so you rarely need `R`.

## Create a pool

Press `n` for the Create Pool form:

```screen
┌zfsctl────────────────┐┌Pools — local ('r' refresh, → for properties)────┐┌Properties — ? ('r' refresh, Ente┐
│Hosts                 ││Name                  Health       Used          ││Property    Value      Source    │
│Pools                 ││scratch               ● ONLINE     ███░░░░░ 42%  ││                                 │
│Datasets              ││tpool                 ● ONLINE     ███░░░░░ 41%  ││                                 │
│Snapshots             ││zroot                 ● ONLINE     ██░░░░░░ 24%  ││                                 │
│Disks                 ││                                                 ││                                 │
│Shares                ││                                                 ││                                 │
│Boot Envs             ││                                                 ││                                 │
│Jobs                  ││                                                 ││                                 │
│Alerts               ┌Create Pool─────────────────────────────────────────────────────┐                     │
│                     │> Name:                                                         │                     │
│                     │  Vdev spec (e.g. mirror da0 da1):                              │─────────────────────┘
│                     │                                                                │─────────────────────┐
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
Enter Next  Tab Next field  Shift+Tab Prev field  Esc Cancel
```

Type a **name** and a **vdev spec**: the layout followed by the disks, for example `mirror da0 da1`, `raidz1 da0 da1 da2`, or just `da0` for a single disk. Press `Enter` on the last field and zfsctl shows you the exact command (`zpool create tank mirror da0 da1`) and the question; only `y` then `Enter` creates it.

> **Safety:** creating a pool **overwrites whatever is on those disks**. Check the names against the Disks section (chapter 11), where each disk's size, model and serial number are shown, before you press `y`. If you are practising, use files instead of disks (chapter 2).

## Add a vdev, replace a disk, offline and online

- **Add a vdev** (`a` inside a pool): another group of disks, for example `mirror da2 da3`, joins the pool and its space is available at once. The question shows the exact `zpool add` command and, a moment later, the layout `zpool add -n` says the pool would have (the host answers; if it refuses the add, you get the form back with its message). A vdev added to a pool usually cannot be removed again: think about it before you say yes, because a pool with one weak vdev is only as safe as that vdev.

## Fast disks next to slow ones: log, cache, special, dedup, spare

A *hybrid* pool puts a few fast devices beside the big slow ones. Start the spec with the role's word:

| Role | Spec | What it does | Removable again? |
|---|---|---|---|
| **log** (SLOG) | `log da4` or `log mirror da4 da5` | takes the synchronous writes (databases, NFS), so they finish before the slow disks do | yes |
| **cache** (L2ARC) | `cache da5 da6` | a second-level read cache on fast disks; each disk is its own device, never mirrored | yes |
| **special** | `special mirror da6 da7` | holds metadata (and, with `special_small_blocks`, small blocks) | no |
| **dedup** | `dedup mirror da8 da9` | holds the dedup table | no |
| **spare** | `spare da9` | steps in when a disk fails | yes |

From the Disks section the same choice is the **Role** field of the Add vdev form (`a` on the marked disks; `F2` lists the roles).

> **Safety:** a **special** or **dedup** vdev holds data the pool cannot do without: if it is lost, the whole pool is lost. zfsctl therefore refuses one disk for either (use a mirror of two or more), and the Dashboard shows a red line, `tank special vdev has no redundancy`, for a pool that has such a vdev anyway. A log or cache device that goes bad is only a yellow line (`tank log device faulted`): the pool keeps working.

Inside a pool, `X` removes the selected **log**, **cache** or **spare** device (a log mirror goes as a whole). Data, special and dedup vdevs cannot be removed here, and the leaves of a log mirror are not offered. In the dataset's Properties tab, `Enter` on `special_small_blocks` opens a size form (`0`, `4K`, `32K` ... up to `16M`, a power of two); the question warns when the size reaches the recordsize, because then *all* of that dataset's data lands on the special vdev. It only affects data written afterwards.
- **Replace a disk** (`x` on the failed disk): type the new device and confirm; the pool resilvers onto it. The question warns that the new disk is overwritten.
- **Offline / online** (`o` / `O`): take a disk out of service (before pulling it, or to stop a failing disk from slowing the pool), then bring it back.

## Pool properties

Press `Right` to move into the properties pane, or select a pool and look at it in the right pane:

```screen
┌zfsctl────────────────┐┌Pools — local ('r' refresh, → for p┐┌Properties — tpool ('r' refresh, Enter to edit,┐
│Hosts                 ││Name             Health       Used ││Property         Value               Source    │
│Pools                 ││scratch          ● ONLINE     42%  ││size             20937965568         -         │
│Datasets              ││tpool            ● ONLINE     41%  ││capacity         41                  -         │
│Snapshots             ││zroot            ● ONLINE     24%  ││altroot          -                   default   │
│Disks                 ││                                   ││health           ONLINE              -         │
│Shares                ││                                   ││guid             5954732628176323138 -         │
│Boot Envs             ││                                   ││version          -                   default   │
│Jobs                  ││                                   ││bootfs           -                   default   │
│Alerts                ││                                   ││delegation       on                  default   │
│                      ││                                   ││autoreplace      on                  local     │
│                      ││                                   ││cachefile        -                   default   │
│                      ││                                   ││failmode         wait                default   │
│                      ││                                   ││listsnapshots    on                  local     │
│                      │└───────────────────────────────────┘└───────────────────────────────────────────────┘
│                      │┌Metrics — tpool─────────────────────────────────────────────────────────────────────┐
│                      ││Read  0B/s  peak 0B/s      Write 0B/s  peak 0B/s     Health  ● ONLINE               │
│                      ││0 ops  lat -               0 ops  lat -              Used    41%  8.1G / 19.5G      │
│                      ││                                                     Vdevs      1  (1 raidz2)       │
│                      ││                                                     Disks      4  (4 data)         │
│                      ││                                                     Datasets   1 filesystem        │
│                      ││                                                     Snapshots  2                   │
│                      ││                                                     Compress   on 1.00x   Trim off │
│                      ││                                                     Frag    2%   Dedup 1.00x       │
│                      ││                                                     Errors  R0 W0 C0               │
│                      ││                                                     Scrub   Sep 26 13:15:14 2026   │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  ← Back  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

The columns are the property, its value, and its **source** (`local` if set on this pool, `default` if untouched). `Up` and `Down` move; `Enter` on an editable property opens a form with its current value (for properties with a fixed set of values, such as `failmode` or `autoexpand`, `F2` offers the choices). Read-only properties (`size`, `health`, `guid`…) cannot be edited. Enabling a pool feature (`feature@…`) asks the two-key question first, because it cannot be undone and can make the pool unreadable by older systems. `Left` returns to the list.

## Destroy a pool

`d` on the Pools list asks you to **type the pool's name**. It then runs `zpool destroy` and the pool is gone.

> **Safety:** a destroyed pool can sometimes be imported again with `zpool import -D` if nothing has overwritten the disks, but do not count on it. Make sure of the name, and that you have a copy of anything you need.

## Schedule a scrub

With a pool selected, `J` opens the scrub job form, already filled in with the host and the pool; you only choose the schedule. Chapter 14 explains schedules and the Jobs section.

## Try it

On the practice pool from chapter 2 (made of files; if you have not made one, do that first): go to Pools, select it, press `s` and `y` then `Enter`, and watch the Scrub line in the panel finish. Press `Enter` to see the vdev tree, then `Esc`. Leave the pool in place: chapters 9 and 10 use it, and chapter 10 ends by showing how to destroy it. If you made it from the shell, zfsctl saw it within ten seconds, or press `r`.

## Where next

- [Datasets](09-datasets.md): the filesystems inside a pool.
- [Disks](11-disks.md): choosing the right disks before you create a pool.
