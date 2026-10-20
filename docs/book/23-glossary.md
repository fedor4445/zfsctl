# Glossary

## What you will learn

- What the words in this book mean.

Terms are in alphabetical order.

**ACL**: access control list: the rules saying who may read, write or execute a file. FreeBSD ZFS uses NFSv4 ACLs.

**alert**: a message sent when a host turns bad on two checks in a row; see chapter 16.

**ARC**: adaptive replacement cache: ZFS's memory cache of recently used data; shown in the host panel.

**backup**: a separate copy of your data, elsewhere, that you can restore from by itself; chapter 15.

**bectl**: FreeBSD's tool for boot environments.

**bhyve**: FreeBSD's built-in hypervisor for virtual machines.

**boot environment**: a bootable copy of the operating system kept as a ZFS clone, so a bad upgrade can be undone; chapter 13.

**catch up**: running a missed scheduled job once when the daemon starts.

**checksum**: a fingerprint ZFS stores with every block to detect silent corruption.

**clone**: a writable copy of a snapshot that shares its blocks.

**compression**: ZFS can compress data as it writes it; `lz4` is a good default.

**copy-on-write**: ZFS never overwrites a block in place; it writes a new one and then moves the pointer, which is what makes snapshots free.

**cron**: the five-field schedule notation used by jobs.

**daemon**: a program that runs in the background; `zfsctl daemon` runs the jobs and the health monitor.

**dataset**: a filesystem or volume inside a pool; chapter 9.

**dedup**: deduplication: storing identical blocks once; costly in memory and rarely worth it.

**delegation**: letting an ordinary user perform certain ZFS actions, with `zfs allow`.

**destroy**: to delete a pool, dataset, snapshot or boot environment for good.

**disk**: a physical storage device such as `ada1`; chapter 11.

**Disks**: the section that lists the host's disks and builds pools from them.

**ENV**: an environment variable; the notify hook receives its details this way.

**escalation**: an alert that gets worse, from yellow to red.

**export**: to detach a pool so it can be moved to another machine (`zpool export`).

**fragmentation**: how scattered a pool's free space is; high values slow writes.

**FreeBSD**: the operating system zfsctl manages; chapter 3.

**guest access**: an SMB share anyone can reach without a password.

**health**: a pool's state: ONLINE, DEGRADED, FAULTED and so on.

**heartbeat**: a file the daemon rewrites every second so the TUI can tell it is running.

**hold**: a lock ZFS puts on a snapshot so it cannot be destroyed until the hold is released; zfsctl's own tag is `zfsctl`; chapter 10.

**hook**: the `notify_command` that runs for alerts and failed jobs; the way to reach Telegram, ntfy and other services.

**host**: a machine zfsctl manages; the hub itself is `local`; chapter 7.

**hub**: the one machine that runs zfsctl and the daemon, and reaches the others over ssh.

**import**: to attach a pool to the running system (`zpool import`).

**incremental**: a send that carries only what changed since a common snapshot.

**jail**: FreeBSD's lightweight container.

**job**: a scheduled task of the daemon: scrub, snapshot, replicate or backup; chapter 14.

**keep**: how many of a job's snapshots to retain; for backups, restic's retention words.

**L2ARC**: an optional second-level read cache on a fast disk.

**lz4**: the usual ZFS compression algorithm: fast, and often a net speed-up.

**mirror**: a vdev of two or more disks holding the same data.

**mountpoint**: where a dataset appears in the directory tree.

**NFS**: the Unix network filesystem; chapter 12.

**offline**: a disk taken out of service on purpose.

**pool**: one or more vdevs presented as a single store of space; chapter 8.

**prefix**: the start of the name of snapshots a job makes and uses, such as `hourly`.

**property**: a setting of a pool or dataset, such as `compression` or `quota`.

**quota**: a limit on the space a dataset may use.

**raidz**: a vdev with parity: raidz1 survives one failed disk, raidz2 two, raidz3 three. (Like RAID 5/6)

**rclone**: a tool that talks to dozens of cloud storage services; restic can use it as a repository.

**recordsize**: the block size ZFS uses for files in a dataset.

**redundancy**: extra copies or parity that let a pool survive a failed disk.

**refreservation**: space guaranteed to a dataset itself, not counting its snapshots.

**replication**: copying a dataset and its snapshots to another place with `zfs send` and `receive`; chapter 14.

**repository**: where restic keeps a backup: a path, an sftp address or an rclone remote.

**reservation**: space guaranteed to a dataset.

**resilver**: the repair after a disk is replaced or returns: data is rebuilt onto it.

**restic**: the open-source backup tool zfsctl's backup jobs run.

**rollback**: returning a dataset to a snapshot, discarding everything after it.

**root**: the administrator account; zfsctl runs as root on the hub.

**rtt**: round-trip time of a host's last health probe.

**Samba**: the software that provides SMB sharing on FreeBSD and Linux.

**scrub**: reading all data and checking it against the checksums, repairing from redundancy; chapter 8.

**send and receive**: `zfs send` writes a snapshot as a stream; `zfs receive` rebuilds it elsewhere.

**share**: a dataset made available to other machines over NFS or SMB; chapter 12.

**sharenfs**: the dataset property that holds NFS export options.

**SMB**: the Windows file-sharing protocol, served by Samba.

**snapshot**: a read-only picture of a dataset at one moment; chapter 10.

**source**: where a property's value comes from: local, inherited or default.

**stripe**: a pool with no redundancy: if any disk fails, everything is lost.

**sudoers**: the file that says which commands an account may run as root; chapter 5.

**tag**: your own label for a host.

**temporary boot**: using a boot environment for the next boot only.

**theme**: the colour scheme of the screen.

**vdev**: a virtual device: a disk, a mirror or a raidz group, from which pools are built; chapter 8.

**volume**: a ZFS block device, used for virtual machine disks or iSCSI.

**zfs command**: the command-line tool for datasets and snapshots.

**ZFS**: the filesystem and volume manager that zfsctl manages; chapter 2.

**zpool command**: the command-line tool for pools.

## Where next

- [The cover](README.md)
