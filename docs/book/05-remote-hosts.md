# Remote hosts and sudoers

## What you will learn

- How zfsctl manages other machines without ever using root over ssh.
- How to set a host up: an ordinary account, your key, and a short sudoers entry.
- How to let zfsctl write that entry for you and check it.

## The model

For every other machine, zfsctl connects over ssh as an **ordinary, unprivileged user** that you create there, and runs each ZFS command through `sudo -n` (the `-n` means it never waits for a password: if sudo would ask, the command fails at once with a clear message). The user's sudoers entry lists the exact programs allowed, by full path, and nothing else.

This means a stolen ssh key does not hand over the machine: it is worth only what the sudoers entry allows. And zfsctl never uses `root@` as an ssh target, so you can (and should) turn off root ssh login on every host.

> **Safety:** the sudoers entry is the whole privilege boundary, so it is worth understanding. `zfs`, `bectl` and `restic` are, in practice, as powerful as root (for example `zfs` can mount over directories and `restic` can read and write any file), so grant them only to hosts you trust zfsctl with.

## Setting up a host, step by step

On the host (the machine to be managed), as root:

1. **Create the account** (any name; `fbsd` in the examples), for instance with `adduser`, and give it no special groups.
2. **Authorise the hub's key.** From the hub, as root, copy the hub's ssh public key into that account's `~/.ssh/authorized_keys` (for example with `ssh-copy-id`). Then, still as root on the hub, connect once (`ssh fbsd@host`) so the host's key is accepted.
3. **Install sudo** if it is missing: `pkg install sudo`.
4. **Add the sudoers entry.** You do not have to write it yourself: zfsctl prints it for you (next section).

Then add the host on the hub: press `n` in the Hosts section, type a name and the `user@address`, and optionally a tag. zfsctl checks it can reach the host (it runs a harmless `true` over ssh, without sudo) before accepting it, and saves it to `config.toml`. If the check fails you get ssh's own message.

## Let zfsctl write and check the entry

On the hub, as root:

```sh
zfsctl print-sudoers fbsd16t1
```

It connects to the host as the unprivileged account (read-only: it changes nothing), finds where each program really lives, and prints the entry:

```text
# zfsctl sudoers entry for host fbsd16t1, generated 2026-10-10
# features: core, acl, disks, bootenvs, shares, samba
# zfs, bectl are as powerful as root: grant them only to hosts you trust zfsctl with.
# Install as /usr/local/etc/sudoers.d/zfsctl (owner root, mode 0440) after checking it:
#   visudo -c -f /usr/local/etc/sudoers.d/zfsctl
# These run without sudo (needed only if the ssh key is locked to a forced command): getent, uname, date, hostname, sysctl, swapinfo, iostat, showmount
fbsd ALL=(root) NOPASSWD: \
    /sbin/zpool, \
    /sbin/zfs, \
    /bin/getfacl, \
    /bin/setfacl, \
    /sbin/geom, \
    /sbin/bectl, \
    /usr/sbin/service, \
    /usr/local/bin/smbpasswd, \
    /usr/local/bin/smbstatus -S, \
    /bin/cat /usr/local/etc/smb4.conf, \
    /usr/bin/tee /usr/local/etc/smb4.conf
```

Check the output, then on the host save it as `/usr/local/etc/sudoers.d/zfsctl` (owner `root`, mode `0440`) and test it first with `visudo -c -f /usr/local/etc/sudoers.d/zfsctl`, as the comment says.

Options: `--with restic` adds the restic entry (needed only on hosts that run backup jobs), `--with smart` adds `smartctl` for the daily disk health check (the host needs `smartmontools`), and `--without samba` (or `acl`, `disks`, `bootenvs`, `shares`) leaves a feature out. If a program cannot be found on the host, or is found in an unexpected place (such as a home directory), zfsctl says so and prints nothing rather than a risky line. The exit code is 0 on success, 1 for a problem such as a missing program, and 2 if the host cannot be reached. `--with restore` adds a pinned `cp` so a file can be copied out of a snapshot (`cp` as root is as powerful as root itself: it can create a new file anywhere). `--with browse` adds a pinned, read-only `ls` so the snapshot browser can list a snapshot's folders (a host set up before the escaped listings needs the browse entry installed again: run `zfsctl print-sudoers HOST --with browse` and install the new file; on OmniOS it also runs `zoneadm` and `zonecfg`, without sudo, to find which zone holds a dataset; a zone that cannot be checked makes the browser refuse rather than guess).

Then check that the host really allows exactly what is needed:

```sh
zfsctl check-host fbsd16
```

```text
ok        zpool      /sbin/zpool
ok        zfs        /sbin/zfs
ok        getfacl    /bin/getfacl
ok        setfacl    /bin/setfacl
ok        geom       /sbin/geom
ok        bectl      /sbin/bectl
ok        service    /usr/sbin/service
ok        smbpasswd  /usr/local/bin/smbpasswd
ok        smbstatus  /usr/local/bin/smbstatus -S
ok        cat        /bin/cat /usr/local/etc/smb4.conf
ok        tee        /usr/bin/tee /usr/local/etc/smb4.conf
11 ok, 0 missing, 0 too broad
```

For each entry it reports `ok`, `MISSING` (sudo would refuse it) or `TOO BROAD` (sudo allows more than it should: for example a plain `cat` instead of `cat` pinned to `smb4.conf`). It exits 0 only when everything is `ok` and there is no note, 1 when anything is not (or there is a note), and 2 if the host cannot be reached. If the account also has a sudo rule that asks for a password (a `%wheel` rule, say), it says so in a note, because such a rule can make an entry look fine when it is not: use an account whose only sudo rules are the zfsctl ones.

## What the entry allows, and why

These programs may run as root through sudo; each is there for a feature, so a host that never uses a feature does not need its entry:

| Program | Used for |
|---|---|
| `zpool`, `zfs` | everything about pools and datasets (core) |
| `getfacl`, `setfacl` | the ACL tab of Datasets |
| `geom` | the Disks section |
| `bectl` | boot environments |
| `service` | the Shares section (is NFS/SMB running, reload Samba) |
| `smbpasswd` | setting a Samba user's password for a managed SMB share |
| `smbstatus -S` | which clients are connected to an SMB share |
| `cat` and `tee`, each pinned to `/usr/local/etc/smb4.conf` | reading and writing the managed Samba block |
| `restic` | backup jobs (add with `--with restic`) |
| `readlink` | backup jobs with a path (comes with `--with restic`) |
| `smartctl` | the daily disk health check (add with `--with smart`) |
| `ls` | looking inside snapshots (add with `--with browse`; metadata only, never file contents) |
| `cp` | copying a file out of a snapshot (add with `--with restore`; root-equivalent: it can create a new file anywhere; `-n` means it never overwrites, and on Linux `-T` also stops a copy into a directory) |

Small read-only programs run over ssh **without** sudo: `getent`, `uname`, `date`, `hostname`, `sysctl`, `swapinfo`, `iostat` and `showmount` (FreeBSD), and `sh` (one `sh -c` that reads `/proc`, `/sys` or `kstat`, and detects the OS), `lsblk` (Linux disks), `systemctl`, `svcs` and `pgrep` (service state on Linux and illumos), `stat` (restic), and `zoneadm` and `zonecfg` (illumos zones, for the snapshot browser). They need no entry, unless the ssh key on that host is locked to a single forced command, in which case that command must allow them.

The `cat` and `tee` entries are pinned to one file on purpose: a bare `sudo cat` would let the account read any file as root.

## Linux and illumos hosts

The hub is always FreeBSD, but the hosts it manages may run Linux (tested on Ubuntu 24 with OpenZFS) or illumos (tested on OmniOS). zfsctl reads each host's OS once (`uname -s`) and adapts:

- **Works the same everywhere:** the Hosts and Dashboard rows (pools, capacity, scrub, errors), Pools, Datasets, Snapshots, and the properties and permissions panes, because `zpool` and `zfs` print the same columns on all three systems.
- **Disks** lists the real disks with the tool each OS has: `lsblk` on Linux, `iostat -En` on illumos (neither needs sudo). Pool membership comes from `zpool status` (on Linux with `-L`, so pools built from `/dev/disk/by-id` names still match their disks); a pool made of files shows its disks as available. Zvols, `zram` and `nbd` devices are not listed as disks.
- **Boot Envs** on illumos lists the boot environments with `beadm list -H` (no sudo needed), with the same Active, Space, Mountpoint and Created columns. Changes use `beadm` through sudo, so the host needs the `beadm` sudoers entry (`print-sudoers` includes it by default on illumos): `n` creates (from the running boot environment, or from a named inactive one), `a` activates, `t` activates for the next boot only, `T` clears that, `R` renames and `d` destroys, each with the usual two-key confirmation. Mounting (`m`, `u`) and destroying together with the origin snapshot (`D`) have no `beadm` equivalent and are not offered there. On Linux Boot Envs is not supported yet.
- **Shares** lists the datasets that have `sharenfs` set (the raw value, since FreeBSD's `exports(5)` syntax does not apply) and, on illumos, the native `sharesmb` shares, and its title shows whether the services run (`systemctl` on Linux, `svcs` on illumos, no sudo). It is read-only there: creating, editing and removing shares is FreeBSD-only for now. The daily check and `c` in Hosts use the same service states: NFS shares while the NFS server is not running (or not installed) is a red line on the Dashboard, with the service's own name (`nfs-server`, `nfs/server`); SMB shares are counted on illumos only. On Linux the state comes from `systemctl`; a host without systemd (Alpine, Void, OpenRC, a container) is asked with `pgrep -x nfsd` and `pgrep -x smbd` instead, and when even that cannot answer the state is *unknown*: the list still shows, nothing is claimed about the services, and the daily check does not go red. A server that was never installed cannot be told from a stopped one without systemd. A unit that is reloading or starting counts as running, and so does an illumos service that is `degraded`. Samba shares on Linux are not listed or counted yet, so the SMB state is not shown there. Share editing (`Enter`, `d`, `N`, `M`) is offered only on a host whose OS has been read and is FreeBSD, so nothing FreeBSD-specific is sent to a host that has not answered its first probe yet.
- **Not supported yet:** the dataset ACL tab (and Boot Envs on Linux). Their entries in the sidebar are dimmed while such a host is selected, they say `<Section> is not supported on <OS> hosts yet.`, and their keys do nothing. They come back as soon as you select a FreeBSD host. Memory, CPU, load and swap on the Dashboard are read with one unprivileged `sh -c` per probe (`/proc` on Linux, `kstat`, `swap -l` and `uptime` on illumos), with the same meaning as on FreeBSD: the ZFS cache does not count as memory in use (on Linux the kernel's own "available" figure is used, which also leaves out the reclaimable page cache). The host detail panel at the bottom of Hosts reads the same things with the host's own tools (uptime, CPU model, memory, swap, load, the ZFS cache, the ZFS version where `/sys/module/zfs/version` exists); on illumos the ZFS version stays `-` because there is no `zfs version` there.
- **Sudoers:** `zfsctl print-sudoers HOST` and `check-host HOST` look at the host's OS first. For Linux and illumos they default to the `core` entry only (`zpool` and `zfs`); `--with smart` adds `smartctl`, and the other features are refused with a message, because they run FreeBSD's tools. The printed file goes to `/etc/sudoers.d/zfsctl` there (FreeBSD's is `/usr/local/etc/sudoers.d/zfsctl`).
- A host whose OS cannot be read is treated as FreeBSD, so a setup that worked before keeps working.

## If something is wrong

- **`unreachable`** in the Hosts list: ssh could not connect (wrong address, host down, firewall). Try `ssh user@host` from the hub as root.
- **`ssh login refused`** or **`host key not accepted`**: the hub's key is not authorised for that account, or you have not connected once to accept the host key.
- **`a password is required`** the first time you use a feature: that program is missing from the sudoers entry. Run `zfsctl check-host`.
- **`TOO BROAD`**: the entry allows more than it should; replace it with the output of `print-sudoers`.

More messages and fixes are in [Troubleshooting](19-troubleshooting.md).

## Try it

On the hub, add one test host: create the account and authorise the key, then press `n` in the Hosts section and add it (this only needs ssh to work). Now run `zfsctl print-sudoers host` and `zfsctl check-host host` (they look the host up in `config.toml`, so it must be added first) until it says `ok` for every line. The host should show in the list with a green `●` and `ok`.

## Where next

- [The screen](06-the-screen.md) to learn your way around.
- [Hosts](07-hosts.md) to read what the Hosts table tells you.
