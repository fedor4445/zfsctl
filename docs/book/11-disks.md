# Disks

## What you will learn

- What the Disks section lists and what each column tells you.
- How to identify a physical disk before you act on it.
- How to build a pool or add a vdev from disks you select.

## The list

(On a Linux or illumos host the list comes from `lsblk` or `iostat -En` instead of `geom`; see chapter 5. The columns are the same, but a rotating disk shows `HDD` without a speed, and illumos shows no Type.)

The Disks section lists every disk the host has, and which pool each belongs to:

```screen
┌zfsctl────────────────┐┌Disks — local ('r' refresh, Space select, 'n' create pool, 'a' add vdev, Esc clear s┐
│Hosts                 ││Name               Size     Type      Status                                        │
│Pools                 ││[ ] ada0           45.0G              in zroot (ada0p4)                             │
│Datasets              ││[ ] ada1           5.0G               in tpool (raidz2-0)                           │
│Snapshots             ││[ ] ada2           5.0G               in tpool (raidz2-0)                           │
│Disks                 ││[ ] ada3           5.0G               in tpool (raidz2-0)                           │
│Shares                ││[ ] ada4           5.0G               in tpool (raidz2-0)                           │
│Boot Envs             ││                                                                                    │
│Jobs                  ││                                                                                    │
│Alerts                ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      │└────────────────────────────────────────────────────────────────────────────────────┘
│                      │┌Metrics — disk ada0─────────────────────────────────────────────────────────────────┐
│                      ││Read  0B/s  peak 0B/s      Write 0B/s  peak 0B/s     Disk     ada0  45.0G  type unk…│
│                      ││0 ops  lat -               0 ops  lat -              Model    QEMU HARDDISK         │
│                      ││                                                     Serial   o3q4Qrvy              │
│                      ││                                                     Pool     zroot / ada0p4        │
│                      ││                                                     Health   ONLINE   R0 W0 C0     │
│                      ││                                                     Busy     0%   queue 0          │
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
↑↓ Move  Space Select  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

- **Name** is the system's name for the disk (`ada1`; chapter 3 explains FreeBSD's naming).
- **Size** is its capacity.
- **Type** shows `SSD` or `HDD` and, for a hard disk, its speed in rpm, when the system can tell (a virtual disk, as on this test host, shows nothing).
- **Status** says `available` or which pool and vdev the disk is in (`in tpool (raidz2-0)`); a system disk shows its partition.

## Know which disk is which

Select a disk and the panel shows what you need to identify it: its **model** and **serial number**, the **pool and vdev** it is in, its **health** and error counters (`R0 W0 C0` means no read, write or checksum errors), and how **busy** it is and the length of its queue. The graphs show its read and write activity.

> **Safety:** before you replace, offline or build a pool from a disk, check its **serial number** against the label on the physical drive. Device names (`ada3`) can change when cables move; serial numbers do not. A pool built from the wrong disk overwrites it.

> **Coming from Linux:** this is `lsblk` plus `smartctl -i` (model and serial) plus `zpool status` in one place, and `/dev/disk/by-id` is replaced by the serial. **Coming from Windows:** it is Disk Management's list, with the ZFS membership added.

## Build a pool from disks

Instead of typing disk names, select them. `Space` toggles the mark on the selected disk (`[x]`), `Esc` clears all marks, and then:

- `n` **creates a pool** from the marked disks. A form asks for the pool's name and the **redundancy**: leave it blank for a simple stripe, or type `mirror`, `raidz1`, `raidz2` or `raidz3` (`F2` lists the choices). zfsctl works out the command and shows it, and the two-key question warns that the disks will be overwritten.
- `a` **adds a vdev** to an existing pool from the marked disks, with a form that asks which pool and the redundancy of the new vdev.

A disk that already belongs to a pool cannot be marked: a message tells you which pool it is in. Both forms show the model and serial number of every marked disk, so you can check them one more time before the question.

> **Safety:** a **stripe** (no redundancy) has no protection: if any disk fails, the pool is lost. Use a mirror or raidz for anything you care about. The question shows the exact `zpool create` command, so read it before you press `y`.

> **Not built yet:** zfsctl does not partition disks or label them (`gpart`). It uses whole disks as ZFS does when you give it `da0`.

## Try it

Look at your own disks: select each in turn and compare the serial number in the panel with the label on the drive or with `geom disk list`. If you have a spare disk or a USB stick you can erase, mark it with `Space` and press `n` to see the form and the confirmation, then answer `n` and `Enter` to cancel.

## Where next

- [Pools](08-pools.md) for what happens after the pool exists.
- [Recipes](17-recipes.md): "replace a failed disk".
