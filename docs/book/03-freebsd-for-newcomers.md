# FreeBSD for newcomers

## What you will learn

- How FreeBSD is organised, in just enough detail to use zfsctl comfortably.
- The few commands you will use every day: `pkg`, `service`, `sysrc`, `geom` and friends.
- How FreeBSD names disks.

[FreeBSD](https://www.freebsd.org/) is developed by the volunteers of the FreeBSD Project, and zfsctl exists because of their work; ZFS in FreeBSD comes from [OpenZFS](https://openzfs.org/). If you already run FreeBSD, skim the headings and move on to [Installing](04-installing.md). For depth, the [FreeBSD Handbook](https://docs.freebsd.org/en/books/handbook/) is excellent.

## One system, not a distribution

FreeBSD is a complete operating system: kernel, libraries and basic tools come together from one project and are released together (the test hub in this book runs 15.1-RELEASE). Software you add yourself lives apart, under `/usr/local`: its programs in `/usr/local/bin`, its configuration in `/usr/local/etc`. The base system never touches `/usr/local`, and packages never touch the base. That is why zfsctl installs to `/usr/local/bin/zfsctl` and its configuration is `/usr/local/etc/zfsctl/config.toml`.

## Installing software with pkg

```sh
pkg search restic        # find a package
pkg install restic       # install it (as root)
pkg info restic          # what is installed, and which version
pkg upgrade              # upgrade installed packages
```

On the test hub, `pkg info` shows `restic-0.19.1`, `rclone-1.75.1` and `msmtp-1.8.32`: the three extra tools zfsctl can use for backups and mail (chapters 15 and 16).

## Services: rc.conf, sysrc and service

FreeBSD starts services from small scripts in `/etc/rc.d` (base) and `/usr/local/etc/rc.d` (packages). Which ones start at boot is set in `/etc/rc.conf`, and you change that file with `sysrc`, which edits it safely:

```sh
sysrc zfsctl_enable=YES      # start zfsctl's daemon at boot
service zfsctl start         # start it now
service zfsctl status        # is it running?
sysrc zfsctl_enable          # what is the setting?
```

On the test hub, `sysrc zfsctl_enable zfs_enable` prints `zfsctl_enable: YES` and `zfs_enable: YES`, and `ls /usr/local/etc/rc.d` shows the `zfsctl` script. A service is "enabled" only when `rc.conf` says `NAME_enable="YES"`.

## Disks have driver names

FreeBSD names a disk after the driver that found it, plus a number. The most common:

| Name | What it is | Linux |
|---|---|---|
| `ada0`, `ada1`… | SATA disks | `/dev/sda`… |
| `da0`, `da1`… | SCSI, SAS and USB disks | `/dev/sdb`… |
| `nda0` | NVMe disks | `/dev/nvme0n1` |
| `vtbd0` | virtio disks in a VM | `/dev/vda` |

The test hub's disks are `ada0` (the system disk) and `ada1` to `ada4` (the disks of the `tpool` pool, 5 GB each). A name can change if you move a disk to another port, so zfsctl also shows each disk's model and serial number, which is how you tell which physical disk is which before you replace one.

```sh
geom disk list           # every disk with its size, model and serial
zpool status             # which disks are in which pool
```

## Users, root and sudo

FreeBSD has a `root` account and the `wheel` group (the people allowed to become root). Installing `sudo` (`pkg install sudo`) works as on Linux, and `doas` is available as a package (`security/doas`). zfsctl itself must be started by root on the hub, and on every other host it uses an ordinary user that may run only a short list of commands through `sudo` (chapter 5).

## A few more things you will meet

- `kldload` and `kldstat` load and list kernel modules (`zfs.ko`).
- `freebsd-update fetch install` updates the base system; `bectl` manages *boot environments*, which let you roll back an upgrade (chapter 13).
- `man zfs`, `man zpool`, `man rc.conf`: the manual pages are complete and worth reading.
- `dmesg` shows what the kernel found at boot, including your disks.

## Jails

FreeBSD *jails* are lightweight containers built into the system, much like Linux containers or Docker, but older and simpler. They are common on ZFS machines because a jail's files can be a dataset you snapshot and replicate like any other.

A jail's datasets are ordinary datasets, so everything in this book works on them.

## Try it

On any FreeBSD machine, as an ordinary user, run these read-only commands and look at what they tell you:

```sh
freebsd-version
geom disk list
zpool list
pkg info | head
service -e | head
```

## Where next

- [Installing](04-installing.md) to put zfsctl on your hub.
- [ZFS in 15 minutes](02-zfs-in-15-minutes.md) if you have not read it.
