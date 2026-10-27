# Pools

## What you will learn

- How to read the Pools list, the panel and the vdev tree.
- How to scrub, create, extend, repair and destroy pools safely.
- How to read and edit pool properties.

## The list and the panel

Press `Tab` to reach Pools, then `Down` to select a pool.

```screen
┌zfsctl────────────────┐┌Pools — local ('r' refresh, → for properties)────┐┌Properties — ? ('r' refresh, Ente┐
│Dashboard             ││Name                  Health       Used          ││Property    Value      Source    │
│Hosts                 ││scratch               ● ONLINE     ███░░░░░ 42%  ││                                 │
│Pools                 ││tpool                 ● ONLINE     ███░░░░░ 41%  ││                                 │
│Datasets              ││zroot                 ● ONLINE     ██░░░░░░ 25%  ││                                 │
│Snapshots             ││                                                 ││                                 │
│Disks                 ││                                                 ││                                 │
│Shares                ││                                                 ││                                 │
└──────────────────────┘└─────────────────────────────────────────────────┘└─────────────────────────────────┘
↑↓ Move  → Properties  n New pool  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

Each row shows the pool's **health** (`● ONLINE` and friends), a **capacity bar** with the percentage, and its size and allocated space. With a pool selected, the panel below shows its details:

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
│Jobs                  ││                                                 ││                                 │
│Cloud                 ││                                                 ││                                 │
│Alerts                ││                                                 ││                                 │
│Settings              ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      │└─────────────────────────────────────────────────┘└─────────────────────────────────┘
│                      │┌Metrics — scratch───────────────────────────────────────────────────────────────────┐
│                      ││Read  0B/s  peak 0B/s      Write 0B/s  peak 0B/s     Health  ● ONLINE               │
│                      ││0 ops  lat -               0 ops  lat -              Used    43%  103M / 240M       │
│                      ││                                                     Vdevs      1  (1 mirror)       │
│                      ││                                                     Disks      2  (2 data)         │
│                      ││                                                     Datasets   4 filesystems       │
│                      ││                                                     Snapshots  4                   │
│                      ││                                                     Compress   on 1.00x   Trim on  │
│                      ││                                                     Frag    42%   Dedup 1.00x      │
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

**Trim** shows whether the pool has `autotrim` on, which it is not unless someone turned it on. TRIM tells an SSD which blocks are free so it can prepare them; ordinary spinning disks gain little from it. OpenZFS warns that automatic trimming can stress lower-end devices, and a periodic `zpool trim tank` (for example monthly, from cron) may suit those better ([zpoolprops(7)](https://openzfs.github.io/openzfs-docs/man/master/7/zpoolprops.7.html)). `autotrim` is an ordinary pool property: edit it in the Properties tab.

## Inside a pool: the vdev tree

Press `Enter` on a pool to drill in:

```screen
┌zfsctl────────────────┐┌Vdevs — tpool ('r' refresh, Esc/Backspace back)─────────────────────────────────────┐
│Dashboard             ││Device                         Health                                               │
│Hosts                 ││tpool                          ● ONLINE                                             │
│Pools                 ││  raidz2-0                     ● ONLINE                                             │
│Datasets              ││    ada1                       ● ONLINE                                             │
│Snapshots             ││    ada2                       ● ONLINE                                             │
│Disks                 ││    ada3                       ● ONLINE                                             │
│Shares                ││    ada4                       ● ONLINE                                             │
│Boot Envs             ││                                                                                    │
│Jobs                  ││                                                                                    │
│Cloud                 │└────────────────────────────────────────────────────────────────────────────────────┘
│Alerts                │┌Metrics — tpool─────────────────────────────────────────────────────────────────────┐
│Settings              ││Read  0B/s  peak 0B/s      Write 0B/s  peak 0B/s     Health  ● ONLINE               │
│                      ││0 ops  lat -               0 ops  lat -              Used    41%  8.1G / 19.5G      │
│                      ││                                                     Vdevs      1  (1 raidz2)       │
│                      ││                                                     Disks      4  (4 data)         │
│                      ││                                                     Datasets   1 filesystem        │
│                      ││                                                     Snapshots  2                   │
│                      ││                                                     Compress   on 1.00x   Trim off │
│                      ││                                                     Frag    2%   Dedup 1.00x       │
│                      ││                                                     +2 more lines: enlarge the ter…│
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
scrub repaired 0B in 00:00:29 with 0 errors on Sun Oct 18 03:00:30 2026
↑↓ Move  a Add vdev  r Refresh  Esc Back  ! Shell  A About  ? Help  q Quit
```

You see the pool, its vdevs (`raidz2-0`) and the disks in each, with their state. `Esc` or `Backspace` goes back. In this view:

| Key | What it does |
|---|---|
| `o` / `O` | take the selected disk offline / bring it online (y/n) |
| `x` | replace the selected disk with another (a form, then the question) |
| `+` | attach a disk to the selected mirror (another side), lone disk (it becomes a mirror) or raidz group (it is widened): a form, then the question |
| `a` | add a vdev to the pool (a form, then the question), including log, cache, special, dedup and spare devices |
| `X` | remove the selected log, cache or spare device (a question first) |

## Scrub and resilver

On the Pools list, `s` starts a **scrub** of the selected pool and `R` a **resilver**, each after a yes/no question. A scrub reads everything and verifies it against the checksums, repairing from redundancy; do it regularly (the next chapters show how to schedule it with `J`, so you never have to remember). While it runs, the panel and the Hosts list show the progress. A resilver is the repair that follows a replaced or returned disk; ZFS starts one by itself, so you rarely need `R`.

## Create a pool

Press `n` for the Create Pool form:

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
│Jobs                 ┌Create Pool─────────────────────────────────────────────────────┐                     │
│Cloud                │> Name:                                                         │                     │
│Alerts               │  Vdev spec (e.g. mirror da0 da1):                              │─────────────────────┘
│Settings             │                                                                │─────────────────────┐
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

> **Tip:** the vdev spec may start with options: `-o ashift=12 mirror da0 da1` (or `-O compression=zstd` for a property of the pool's root dataset). `-o` takes a pool property and `-O` a dataset property, each as `NAME=VALUE`; any other flag is refused (`-f` would overwrite disks that belong to another pool, `-n` would make the command do nothing). The question shows the command as `zpool` wants it, options first: `zpool create -o ashift=12 tank mirror da0 da1`. If your disks have 4096-byte sectors (chapter 2, *Sector size*), give `-o ashift=12`. When the spec names disks that the Disks list knows and your `ashift` is smaller than one of their physical sectors, zfsctl asks you to type the pool's name before it goes on (chapter 11). The `ashift` pool property is only the default for vdevs added later: `zdb -C tank | grep ashift` shows what each vdev really uses (if `zdb` cannot find the pool, `zdb -l` on a disk or partition that ZFS uses, such as `zdb -l /dev/da0p3`, reads it from the label). Keep raidz vdevs between three and nine disks (chapter 2).

> **Safety:** creating a pool **overwrites whatever is on those disks**. Check the names against the Disks section (chapter 11), where each disk's size, model and serial number are shown, before you press `y`. If you are practising, use files instead of disks (chapter 2).

## Add a vdev, replace a disk, offline and online

- **Add a vdev** (`a` inside a pool): another group of disks, for example `mirror da2 da3`, joins the pool and its space is available at once. The question shows the exact `zpool add` command and, a moment later, the layout `zpool add -n` says the pool would have (the host answers; if it refuses the add, you get the form back with its message). If the spec starts with an `-o ashift` smaller than the physical sector of a disk the Disks list knows, you must type the pool's name first (chapter 11). Think before you say yes, because a pool with one weak vdev is only as safe as that vdev. ZFS can take a mirror or single-disk vdev out again (`zpool remove` moves its data onto the rest of the pool, which needs room for it), but only while the pool has no raidz or draid vdev at all and every vdev has the same `ashift`; a raidz vdev can never be removed ([zpool-remove(8)](https://openzfs.github.io/openzfs-docs/man/master/8/zpool-remove.8.html)). zfsctl removes only log, cache and spare devices (`X`, below); anything else is a job for the shell.

## Fast disks next to slow ones: log, cache, special, dedup, spare

A *hybrid* pool puts a few fast devices beside the big slow ones. Start the spec with the role's word:

| Role | Spec | What it does | Removable again? |
|---|---|---|---|
| **log** (SLOG) | `log da4` or `log mirror da4 da5` | takes the synchronous writes (databases, NFS), so they finish before the slow disks do | yes |
| **cache** (L2ARC) | `cache da5 da6` | a second-level read cache on fast disks; each disk is its own device, never mirrored | yes |
| **special** | `special mirror da6 da7` | holds metadata (and, with `special_small_blocks`, small blocks) | at the shell, under the conditions above |
| **dedup** | `dedup mirror da8 da9` | holds the dedup table | at the shell, under the conditions above |
| **spare** | `spare da9` | steps in when a disk fails | yes |

From the Disks section the same choice is the **Role** field of the Add vdev form (`a` on the marked disks; `F2` lists the roles).

> **Safety:** a **special** or **dedup** vdev holds data the pool cannot do without (OpenZFS says its redundancy should match or exceed that of the pool's other vdevs): if it is lost, the whole pool is lost. zfsctl therefore refuses one disk for either (use a mirror of two or more), and the Dashboard shows a red line, `tank special vdev has no redundancy`, for a pool that has such a vdev anyway. A log or cache device that goes bad is only a yellow line (`tank log device faulted`): the pool keeps working.

**Before you buy a log or cache device.** A log device speeds up only *synchronous* writes (databases, NFS clients that ask for them); ordinary asynchronous writes never go through it, and OpenZFS notes that a log rarely has more than 4 GB in use. It must not lose what it has acknowledged when the power fails, so use a drive made for it ([OpenZFS hardware notes](https://openzfs.github.io/openzfs-docs/Performance%20and%20Tuning/Hardware.html)). A log can be mirrored (not raidz). If a log device is lost and the machine then crashes, the pool can still be imported without it (`zpool import -m`), but the most recent synchronous writes are gone. A cache (L2ARC) device helps random reads of mostly unchanging data, does nothing for writes, cannot be mirrored and keeps its contents across reboots ([zpoolconcepts(7)](https://openzfs.github.io/openzfs-docs/man/master/7/zpoolconcepts.7.html)).

Inside a pool, `X` removes the selected **log**, **cache** or **spare** device (a log mirror goes as a whole). Data, special and dedup vdevs are not removed from here (the conditions above decide whether `zpool remove` can do it), and the leaves of a log mirror are not offered. In the dataset's Properties tab, `Enter` on `special_small_blocks` opens a size form (`0`, `4K`, `32K` ... up to `16M`, a power of two); the question warns when the size reaches the recordsize, because then *all* of that dataset's data lands on the special vdev. It only affects data written afterwards.
- **Replace a disk** (`x` on the disk): type the new device and confirm; the pool resilvers onto it. `zpool replace` works as if it attached the new disk, waited for the resilver and then detached the old one ([zpool-replace(8)](https://openzfs.github.io/openzfs-docs/man/master/8/zpool-replace.8.html)), so a disk that still works keeps helping while the new one fills: leave it online unless you must pull it. The new disk must be at least as large as the smallest disk of the vdev, and the question warns that it is overwritten.
- **Offline / online** (`o` / `O`): take a disk out of service (before pulling it, or to stop a failing disk from slowing the pool), then bring it back. An offline disk no longer counts as redundancy: on a mirror of two or on a raidz1 that leaves no margin until it is back.

## Attaching a disk to a mirror or a lone disk

The same `+` key on a mirror row (or on one of its disks) adds another side to the mirror, and on a lone top-level disk it turns the disk into a two-way mirror. The pool resilvers onto the new disk, which is overwritten. `zpool detach` takes a side out again (at the shell); a raidz vdev cannot be shrunk. The key is offered for the data vdevs only: log, cache, spare, special and dedup devices are added with `a`, and the disks of a raidz group are not attached to (the group is).

## Widening a raidz vdev

Since OpenZFS 2.3 a raidz vdev can grow one disk at a time with `zpool attach tank raidz2-0 da9`, while the pool stays in use. It needs the `raidz_expansion` feature (a pool created on an older release must be upgraded first, see the Handbook); once the pool has been expanded the feature is active and older software can no longer import it. Expansion does not change fault tolerance: a raidz2 stays a raidz2. Blocks written before keep their old data-to-parity ratio, spread over the wider vdev, and only new writes use the new one; `zfs rewrite` can redo old data. If a disk fails during the expansion, it pauses until the vdev is healthy again ([zpool-attach(8)](https://openzfs.github.io/openzfs-docs/man/master/8/zpool-attach.8.html)). In zfsctl, select the raidz group row in the vdev tree and press `+`: a form asks for the new disk (it must not be in any pool; it is overwritten), and the question shows the exact command, `zpool attach tank raidz2-0 da9`, and what widening means, in the words above. zfsctl never adds `-f`, so ZFS itself refuses a disk that belongs to a pool. Progress shows as the pool's `expand:` line in `zpool status`; the vdev tree shows the disk at once.

## Pool properties

Press `Right` to move into the properties pane, or select a pool and look at it in the right pane:

```screen
┌zfsctl────────────────┐┌Pools — local ('r' refresh, → for p┐┌Properties — tpool ('r' refresh, Enter to edit,┐
│Dashboard             ││Name             Health       Used ││Property         Value               Source    │
│Hosts                 ││scratch          ● ONLINE     42%  ││size             20937965568         -         │
│Pools                 ││tpool            ● ONLINE     41%  ││capacity         41                  -         │
│Datasets              ││zroot            ● ONLINE     25%  ││altroot          -                   default   │
│Snapshots             ││                                   ││health           ONLINE              -         │
│Disks                 ││                                   ││guid             5954732628176323138 -         │
│Shares                ││                                   ││version          -                   default   │
│Boot Envs             ││                                   ││bootfs           -                   default   │
│Jobs                  ││                                   ││delegation       on                  default   │
│Cloud                 ││                                   ││autoreplace      on                  local     │
│Alerts                ││                                   ││cachefile        -                   default   │
│Settings              ││                                   ││failmode         wait                default   │
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
│                      ││                                                     Scrub   Oct 18 03:00:30 2026   │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  ← Back  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

The columns are the property, its value, and its **source** (`local` if set on this pool, `default` if untouched). `Up` and `Down` move; `Enter` on an editable property opens a form with its current value (for properties with a fixed set of values, such as `failmode` or `autoexpand`, `F2` offers the choices). Read-only properties (`size`, `health`, `guid`…) cannot be edited. Enabling a pool feature (`feature@…`) asks the two-key question first, because it cannot be undone. A feature that is only *enabled* does not stop older software from importing the pool; it becomes *active* the first time something uses it (zstd compression, a raidz expansion), and an active feature that the software does not know keeps it from importing the pool ([zpool-features(7)](https://openzfs.github.io/openzfs-docs/man/master/7/zpool-features.7.html)). `zpool upgrade` enables every feature the system knows, so it is the step after which an older system can be locked out as soon as a feature gets used. That includes the boot loader of a pool you boot from, which reads the pool before the kernel runs: enable features on such a pool deliberately, and keep the boot code up to date (the Handbook reminds you to update it when you attach devices to a boot pool). A pool that must stay importable by older software can be created with a `compatibility` feature set (`zpool create -o compatibility=…`, see zpool-features(7)). `Left` returns to the list.

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
