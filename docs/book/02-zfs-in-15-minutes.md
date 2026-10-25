# ZFS in 15 minutes

## What you will learn

- The handful of ideas that make ZFS different: pools, vdevs, datasets, snapshots, send/receive and scrub.
- A first hands-on pass using a practice pool made of ordinary files, so nothing real is at risk.

## Why ZFS

ZFS combines three jobs that are usually separate layers: the volume manager, the filesystem, and the snapshot and copy machinery. Because one system sees everything, it can do things the separate layers cannot:

- **It checksums everything.** Every block of data and metadata has a checksum; a read that does not match is detected, and if you have redundancy ZFS repairs it from the good copy. Silent corruption ("bit rot") does not stay silent.
- **It never overwrites in place** (copy-on-write). A crash cannot leave a half-written block, and snapshots are nearly free: a snapshot just remembers the old blocks.
- **Space is shared.** You do not carve out partitions; every filesystem in a pool draws from the same free space, and you limit them with *quotas* if you want.

## The building blocks

**Pool.** A pool (`zpool`) is where the storage lives. You create it from disks and every filesystem you make afterwards draws from it.

**Vdev.** A *virtual device* is a group of disks inside a pool. A pool is made of one or more vdevs, and the pool's redundancy is decided by its vdevs:

| Vdev type | Disks | Survives | In other terms |
|---|---|---|---|
| single disk | 1 | nothing (it detects damage but cannot repair) | a plain disk |
| mirror | 2 or more | all but one disk | RAID 1 |
| raidz1 | 3 or more | one disk | RAID 5 |
| raidz2 | 4 or more | two disks | RAID 6 |
| raidz3 | 5 or more | three disks | (no common equivalent) |

A pool can also have *log* (a fast device for synchronous writes), *cache* (a second-level read cache), *spare* (a disk that steps in), and *special* or *dedup* vdevs for metadata. If any normal vdev of a pool is lost, the pool is lost, so a pool is only as safe as its weakest vdev.

**Choosing a layout.** A mirror is the simplest to run and repair and does random I/O best; raidz gives more usable space from the same disks. The [FreeBSD Handbook](https://docs.freebsd.org/en/books/handbook/zfs/) recommends three to nine disks in a raidz vdev and splitting bigger sets into several vdevs. Replacing a failed disk means reading every surviving disk of a raidz vdev, and a raidz1 vdev has no redundancy left until that finishes; on very large disks that takes long, which is why many people choose raidz2 or mirrors for them. Since OpenZFS 2.3 a raidz vdev can be widened one disk at a time, but data written before keeps its old layout (chapter 8).

**Sector size (`ashift`).** Every vdev remembers the sector size it was built for, as a power of two: `ashift=12` means 4096 bytes. It is fixed when the vdev is created: setting the pool property later changes nothing for vdevs that already exist ([zpoolprops(7)](https://openzfs.github.io/openzfs-docs/man/master/7/zpoolprops.7.html)). Many disks have 4096-byte sectors but report 512 bytes, for compatibility, and a vdev built on that report stays slower than it needs to be. The Handbook's advice for such drives is to create the pool with an explicit `-o ashift=12`; chapter 8 shows how to give it in the Create Pool form.

**Dataset.** A dataset is a filesystem (or a *volume*, a block device) inside a pool, for example `tank/home` and `tank/media`. Datasets are cheap, so you make many: one per purpose, because each has its own settings (compression, quota, snapshot schedule). They mount under the pool's mountpoint automatically.

**Property.** Settings on a pool or dataset: `compression`, `quota`, `recordsize`, `mountpoint`, `readonly`. Children inherit from parents unless you set them locally.

**Snapshot.** A read-only picture of a dataset at one moment, named `tank/home@before-upgrade`. It costs almost nothing when made, and only grows as the dataset changes. You can read old files out of it, roll the dataset back to it, or copy it somewhere else.

**Clone.** A writable copy of a snapshot that shares its blocks until they change.

**Send and receive.** `zfs send` turns a snapshot (or the difference between two) into a stream; `zfs receive` turns a stream back into a dataset. Pipe them across ssh and you have *replication*: an exact, incremental copy on another machine.

**Scrub and resilver.** A *scrub* reads everything in the pool and checks it against the checksums, repairing what it can. A *resilver* is the repair after a disk is replaced or comes back. Run a scrub regularly (monthly is common) so problems are found while you still have redundancy.

**ARC.** The *adaptive replacement cache*: ZFS uses spare RAM as a read cache, so free memory looks low on a ZFS machine. That is normal.

## A worked example

These are real commands from a practice pool made of two files on the test hub (so nothing real was touched). A mirror of two 128 MB files:

```text
$ sudo zpool create bookdemo mirror /tmp/bookdemo-a.img /tmp/bookdemo-b.img
$ zpool status bookdemo
  pool: bookdemo
 state: ONLINE
config:

	NAME                     STATE     READ WRITE CKSUM
	bookdemo                 ONLINE       0     0     0
	  mirror-0               ONLINE       0     0     0
	    /tmp/bookdemo-a.img  ONLINE       0     0     0
	    /tmp/bookdemo-b.img  ONLINE       0     0     0

errors: No known data errors
```

Two datasets, a setting inherited by both, and a quota on one:

```text
$ sudo zfs create bookdemo/home
$ sudo zfs create bookdemo/media
$ sudo zfs set compression=lz4 bookdemo
$ sudo zfs set quota=50M bookdemo/home
$ zfs list -r -o name,used,avail,refer,mountpoint bookdemo
NAME             USED  AVAIL  REFER  MOUNTPOINT
bookdemo         716K  55.3M   104K  /bookdemo
bookdemo/home     96K  49.9M    96K  /bookdemo/home
bookdemo/media    96K  55.3M    96K  /bookdemo/media
$ zfs get -o property,value,source compression,quota,mountpoint bookdemo/home
PROPERTY     VALUE           SOURCE
compression  lz4             inherited from bookdemo
quota        50M             local
mountpoint   /bookdemo/home  default
```

Note `inherited from bookdemo` and `local`: that is the property system in one table. A snapshot, a change, and a rollback:

```text
$ echo "hello from bookdemo" > /bookdemo/home/note.txt
$ sudo zfs snapshot bookdemo/home@before-edit
$ echo "edited" > /bookdemo/home/note.txt
$ zfs list -t snapshot -r bookdemo
NAME                        USED  AVAIL  REFER  MOUNTPOINT
bookdemo/home@before-edit     0B      -    96K  -
$ sudo zfs rollback bookdemo/home@before-edit
$ cat /bookdemo/home/note.txt
hello from bookdemo
```

> **Safety:** `zfs rollback` throws away everything written after the snapshot. zfsctl makes you type the snapshot's name before it does it. `zpool destroy` is likewise permanent, and zfsctl asks for the pool's name before it runs.

And a scrub:

```text
$ sudo zpool scrub bookdemo
$ zpool status bookdemo
  pool: bookdemo
 state: ONLINE
  scan: scrub repaired 0B in 00:00:00 with 0 errors on Sat Oct 10 15:33:34 2026
  ...
```

zfsctl shows exactly these things (the `STATE` column, the scrub line, the properties and their source) in its screens; you will recognise them.

## What to remember

- Pools hold vdevs; vdevs hold disks; a pool is only as safe as its least redundant vdev.
- Datasets are cheap: make many, set properties per dataset.
- Snapshots are cheap and read-only; send/receive copies them elsewhere.
- Scrub regularly; do not rely on a single copy of anything.
- Redundancy is not a backup: a mirror happily keeps two copies of a file you deleted by mistake. Snapshots protect against that, and copies on another machine protect against the machine.

## Try it

Make your own practice pool on a FreeBSD machine (as root), poke at it, and throw it away. Files stand in for disks, so no real disk is touched:

```sh
truncate -s 128m /tmp/practice-a.img /tmp/practice-b.img
zpool create practice mirror /tmp/practice-a.img /tmp/practice-b.img
zfs create practice/home
zfs snapshot practice/home@first
zpool status practice
zfs list -t all -r practice
# when you are done:
zpool destroy practice
rm /tmp/practice-a.img /tmp/practice-b.img
```

> **Safety:** practise with files like this, never with a real disk's name. A typo in `zpool create` on a real disk overwrites it.

## Where next

- [FreeBSD for newcomers](03-freebsd-for-newcomers.md) for the system around ZFS.
- [Installing](04-installing.md) to get zfsctl itself.
