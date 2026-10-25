# Recipes

## What you will learn

- Short, complete answers to the things a home lab asks most.
- Which keys to press, in order, and what to check at the end.

Each recipe is self-contained. The names (`tank`, `backup`, `hub`) are the running example from the cover; use your own.

## Make a pool from new disks

1. Disks: look at the **serial numbers** and compare them with the labels on the drives.
2. Mark the disks you want with `Space`.
3. Press `n`, name the pool, and choose the redundancy (`mirror` for two disks; for three or more, `raidz2` is the safer choice with large disks). If the disks have 4096-byte sectors, put `-o ashift=12` in the options (chapter 8).
4. Read the command in the confirmation. Press `y`, then `Enter`.
5. Check: Pools shows `● ONLINE`.

Then make datasets (Datasets, `n`) for the things you will treat differently. On OpenZFS 2.2 and later `compression=on` (lz4) is already the default; set a different value on the pool's root dataset to have children inherit it.

## Protect a dataset with automatic snapshots

1. Datasets: select it, press `J`.
2. Prefix `hourly`, Keep `24`, schedule `0 * * * *`. Press `Enter` on the last field.
3. Add a second job, prefix `daily`, Keep `14`, schedule `0 2 * * *`.
4. Check: after the next hour, Snapshots shows `…@hourly-…`.

## Scrub every pool monthly

Pools: select a pool, `J`, schedule `0 3 1 * *`. Repeat for each pool (or for each host: pick the host in the form with `F2`). Check Jobs for the next run time.

## Copy a dataset to another machine

1. Make sure a snapshot job with prefix `hourly` runs on the source (above).
2. On the destination host, make a parent dataset to receive into (`backup`).
3. Jobs, `R`: source host and dataset, prefix `hourly`, destination host and `backup/tank-data`, schedule `30 * * * *`.
4. Check: after the run, the panel shows `sent …`/`skipped: up to date`, and the destination has the snapshots.

Do not write to the destination dataset: a receive fails if the destination changed since its last snapshot (unless you set Force).

## Keep a dataset encrypted

1. Datasets: press `E`, give the name with its pool, keep `aes-256-gcm`, type a passphrase of 8 or more characters twice, then type the dataset's name to confirm.
2. Write the passphrase down somewhere safe. If it is lost the data cannot be recovered by anyone.
3. To lock it: unmount it at the shell, then `U`, `y`, `Enter`. To use it again: `L` and the passphrase.
4. Check: the Lock column says `open` or `LOCKED`.

Replication and cloud mirrors refuse an encrypted source; restic backups work while it is open. A full walkthrough is in [Datasets](09-datasets.md#try-encryption).

## Back up to a USB disk, or the cloud

1. Install restic; make a password file; `restic init` (chapter 15).
2. Jobs, `B`: the dataset, Source `new` (it takes its own snapshot each run), the repository (`/mnt/usb/restic-tank`, or `rclone:myremote:bucket/path`), the password file, Keep `7d 4w 12m`, schedule `30 3 * * *`.
3. Check: Jobs shows `ok`; run `restic snapshots` on the repository. Restore one file as a test.

## Replace a failed disk

1. Hosts shows the host red (`tank DEGRADED`); Pools shows `▲ DEGRADED`; press `Enter` to see which disk is `FAULTED` or `UNAVAIL`.
2. Check the serial number of that disk in Disks and find the physical drive.
3. If the old disk still answers, leave it online: a replacement works as attach, resilver, then detach, so the old disk keeps helping until the new one is full. Take it offline (`o`) only when you have to pull it.
4. If you have a free slot, connect the new drive first. Select the old entry in the vdev view, press `x`, type the new device name (`ada3`), and confirm. If the old drive has to come out first, swap it and do the same.
5. Check: the pool resilvers (progress in the panel) and returns to `● ONLINE`. Leave it alone until it is done.

> **Safety:** replacing overwrites the new disk. Check its name against its serial number first.

## Undo a bad upgrade

1. Before upgrading: Boot Envs, `n`, name it `before-upgrade`.
2. Upgrade and reboot. If the system misbehaves, at the boot menu choose the older boot environment, or from a working shell: select it in Boot Envs and press `a`, then reboot.
3. Clean up with `d` when you are sure.

## Share a dataset with the family

1. Datasets: select it, press `N` for NFS (Linux and Unix clients) or `M` for SMB (Windows and macOS). Visit Shares first.
2. NFS: give a network, `172.16.10.0/24`. SMB: a share name, a Unix user and a Samba password.
3. Check: Shares lists it. From a client, mount it.

## Get an email when something breaks

1. Make sure `msmtp` (or your mail command) works from a shell on the hub.
2. Alerts: `e`, fill in your address and command, save; press `t` to send a test.
3. Check: the message arrives; the daily report arrives the next morning.

## Add a machine to the lab

Chapter 5: create the account and authorise the key, add the host with `n` in Hosts, then `zfsctl print-sudoers HOST` and `zfsctl check-host HOST`.

## Try it

Pick the recipe closest to a worry you have today and do it on the practice pool first.

## Where next

- [Safety](18-safety.md)
- [Troubleshooting](19-troubleshooting.md)
