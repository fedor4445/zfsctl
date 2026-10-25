# Disks

## What you will learn

- What the Disks section lists and what each column tells you.
- How to identify a physical disk before you act on it.
- How to build a pool or add a vdev from disks you select.

## The list


The Disks section lists every disk the host has, and which pool each belongs to:

```screen
┌zfsctl────────────────┐┌Disks — local ('r' refresh, Space select, 'n' create pool, 'a' add vdev, Esc clear s┐
│Dashboard             ││Name               Size     Type      Status                                        │
│Hosts                 ││[ ] ada0           45.0G              in zroot (ada0p4)                             │
│Pools                 ││[ ] ada1           5.0G               in tpool (raidz2-0)                           │
│Datasets              ││[ ] ada2           5.0G               in tpool (raidz2-0)                           │
│Snapshots             ││[ ] ada3           5.0G               in tpool (raidz2-0)                           │
│Disks                 ││[ ] ada4           5.0G               in tpool (raidz2-0)                           │
│Shares                ││                                                                                    │
│Boot Envs             ││                                                                                    │
│Jobs                  ││                                                                                    │
│Cloud                 ││                                                                                    │
│Alerts                ││                                                                                    │
│Settings              ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      │└────────────────────────────────────────────────────────────────────────────────────┘
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
↑↓ Move  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

- **Name** is the system's name for the disk (`ada1`; chapter 3 explains FreeBSD's naming).
- **Size** is its capacity.
- **Type** shows `SSD` or `HDD` and, for a hard disk, its speed in rpm, when the system can tell (a virtual disk, as on this test host, shows nothing).
- **Status** says `available` or which pool and vdev the disk is in (`in tpool (raidz2-0)`; a disk that is a log, cache, spare, special or dedup device says so: `in tank (cache)`, `in tank (log)`, `in tank (special mirror-1)`); a system disk shows its partition.

## Know which disk is which

Select a disk and the panel shows what you need to identify it: its **model** and **serial number**, the **pool and vdev** it is in, its **health** and error counters (`R0 W0 C0` means no read, write or checksum errors), and how **busy** it is and the length of its queue. The graphs show its read and write activity.

> **Safety:** before you replace, offline or build a pool from a disk, check its **serial number** against the label on the physical drive. Device names (`ada3`) can change when cables move; serial numbers do not. A pool built from the wrong disk overwrites it.

## Build a pool from disks

Instead of typing disk names, select them. `Space` toggles the mark on the selected disk (`[x]`), `Esc` clears all marks, and then:

- `n` **creates a pool** from the marked disks. A form asks for the pool's name, the **redundancy** (leave it blank for a simple stripe, or type `mirror`, `raidz1`, `raidz2` or `raidz3`; `F2` lists the choices) and, optionally, **options** such as `-o ashift=12` (chapter 8 lists what is accepted). zfsctl works out the command and shows it, and the two-key question warns that the disks will be overwritten.
- `a` **adds a vdev** to an existing pool from the marked disks, with a form that asks which pool, the **role** (data, log, cache, special, dedup or spare) and the redundancy of the new vdev. Special and dedup default to a mirror and refuse a single disk; cache and spare take each marked disk as a device of its own. See chapter 8 for what each role is.

A disk that already belongs to a pool cannot be marked: a message tells you which pool it is in. Both forms show the model and serial number of every marked disk, so you can check them one more time before the question.

> **Safety:** a **stripe** (no redundancy) has no protection: if any disk fails, the pool is lost. Use a mirror or raidz for anything you care about. The question shows the exact `zpool create` command, so read it before you press `y`.

> **Not built yet:** zfsctl does not partition disks or label them (`gpart`). It uses whole disks as ZFS does when you give it `da0`.

## Hardware notes

- **Sector size.** Match it when you create the pool (`ashift`, chapter 2): it cannot be changed afterwards.
- **A replacement disk must be at least as large as the smallest disk of its vdev** ([zpool-replace(8)](https://openzfs.github.io/openzfs-docs/man/master/8/zpool-replace.8.html)). Disks sold with the same capacity can differ a little between models and batches, which is why some administrators build pools from partitions slightly smaller than the disks. zfsctl does not partition (see above), so when you buy disks, buy spares of the same model or a little larger.
- **Avoid drive-managed SMR disks.** OpenZFS's [hardware notes](https://openzfs.github.io/openzfs-docs/Performance%20and%20Tuning/Hardware.html) say they are "not recommended for ZFS pools": they can stall badly during a resilver, a scrub or a receive. Check the model before you buy.
- **ECC memory.** The same notes call ECC memory "highly recommended", because "no technique used in ZFS (or any other filesystem) is capable of protecting against bit flips". ZFS runs without it; the point is that the checksum is made from whatever is in memory.

## Try it

Look at your own disks: select each in turn and compare the serial number in the panel with the label on the drive or with `geom disk list`. If you have a spare disk or a USB stick you can erase, mark it with `Space` and press `n` to see the form and the confirmation, then answer `n` and `Enter` to cancel.

## Where next

- [Pools](08-pools.md) for what happens after the pool exists.
- [Recipes](17-recipes.md): "replace a failed disk".
