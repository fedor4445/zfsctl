# Managing Linux and illumos hosts from the FreeBSD hub: design

Date: 2026-10-18. Status: draft for approval. Scope: the first two phases (A and B below); later phases are listed so the
design has a direction.

## Why

The user wants zfsctl to manage ZFS on FreeBSD, Linux and illumos, with the hub staying FreeBSD (managed *hosts*, not a
new hub). Test hosts exist: Ubuntu 24 (`ub@172.16.10.52`, host `ub24`, pool `ztest`) and OmniOS r151058
(`omni@172.16.10.80`, host `omnios`, pools `rpool` and `tank`), both reachable from the hub's root key and with a
core-only sudoers entry (`zpool`, `zfs`).

## What we found (live, 2026-10-18, hub build 5102047)

Without any change to zfsctl, with the two hosts added to `config.toml`:

- **Works as is:** Hosts and Dashboard rows (state, pools, capacity, scrub, errors, "tank 96% full" on OmniOS is a
  correct red line), Pools, Datasets, Snapshots. `zpool list -Hp`, `zfs list -H -p -o ...` and `zpool status` have the
  same columns and shape on OmniOS (and the same on Ubuntu with its pool). NFS shares set with `sharenfs` are listed
  (the OmniOS share `rpool/export/nfs-test` showed).
- **Not available, reported badly:** Memory, CPU, Load and Swap show `?` (the probe reads FreeBSD's `sysctl` and
  `swapinfo`, which OmniOS lacks and Linux has under other names); the host Type reads `-`; Disks (`geom disk list`) and
  Boot Envs (`bectl list`) fail with "sudo: a password is required", which is true but means nothing to the user; Shares
  says "NFS unavailable: nfsd not running; SMB unavailable" (FreeBSD `service` semantics), which is wrong on both.

## Adaptive sections (decided 2026-10-18 at the user's request)

A section stays in the sidebar for every host, and **its backend is chosen by the host's OS**: where the OS has an
equivalent, the section works with it (same screen, same keys where the meaning is the same); only where there is no
equivalent does it say `not supported on <OS> yet`. Equivalents found on the test hosts (all readable without sudo):

| Section | FreeBSD (today) | Linux (Ubuntu) | illumos (OmniOS) |
|---|---|---|---|
| Disks | `geom disk list` | `lsblk -b -d -n -P -o NAME,SIZE,TYPE,MODEL,SERIAL,ROTA` | `iostat -En` (vendor, product, serial, size); `diskinfo` needs root |
| Boot Envs | `bectl` | none by default (not supported) | `beadm list -H` (readable without sudo; create/activate/destroy need a sudoers entry for `beadm`) |
| Shares: service state | `service nfsd/samba_server onestatus` | `systemctl is-active nfs-server smbd` | `svcs -H -o state svc:/network/nfs/server svc:/network/smb/server` |
| Shares: the shares | `sharenfs` + managed `smb4.conf` | `sharenfs` + Samba (`smb.conf`), later | `sharenfs` and native `sharesmb`, later |
| ACL | `getfacl`/`setfacl` (NFSv4 on ZFS) | `getfacl`/`setfacl` (POSIX) | `ls -V`, `chmod A...` (NFSv4), later |

Phase A builds the seam and the first three rows' read side (Disks, Boot Envs list, service state); creating and changing
things through an equivalent (for example `beadm create`) follows once its listing works. Rows marked "later" are not in
phase A and show `not supported on <OS> yet` until built.

## Goals

1. **Phase A: know the OS.** Detect each host's OS once (`uname -s`: `FreeBSD`, `Linux`, `SunOS`) and keep it with the
   host's probe state. Everything OS-specific keys off it.
2. **Phase A: one backend per OS for each section, and an honest fallback.** The section code asks a per-OS backend
   (a function of the host's OS and the section) for its data. Where the backend exists it is used (table above);
   where it does not, the section shows `not supported on <OS> yet`, its keys are not offered in the hint bar, and no
   command that cannot work is run (so no `sudo` errors in the TUI or the daemon log). The daily shares check uses the
   service-state backend of the host's OS and is silent where there is none.
3. **Phase A: sudoers by OS.** `print-sudoers` and `check-host` choose the default feature set from the host's OS (Linux
   and illumos: `core` only for now; FreeBSD: as today), print the right install path (`/etc/sudoers.d/zfsctl` on Linux
   and OmniOS), and say which features do not apply to that OS if asked for.
4. **Phase B: resources.** Memory, CPU, Load and Swap for Linux (from `/proc/meminfo`, `/proc/stat`, `/proc/loadavg`,
   the ARC size from `/proc/spl/kstat/zfs/arcstats`) and for illumos (from `kstat`, `vmstat`, `swap -s`, `uptime`,
   `psrinfo`), read without sudo, in the same units and with the same "in use, not counting what can be given back"
   meaning as on FreeBSD (the ARC and cache pages are not "in use" on any OS). The Dashboard and Hosts rows then work
   everywhere. The host Type (`metal`/`vm`/`jail`) gets its Linux and illumos sources if cheap, else stays `-`.

## Non-goals (later phases, each its own design)

- Phase C: SMART on both (`smartctl` and its JSON are the same; only the sudoers path and the scan differ), changing
  boot environments with `beadm`, managing shares on Linux (Samba, `exportfs`) and illumos (`sharesmb`, `sharectl`).
- Phase D: ACLs on illumos (NFSv4 through `ls -V`/`chmod`) and Linux (POSIX), jobs (scrub, snapshot, replication,
  restic, rclone) on these hosts, a Linux or illumos *hub*.
- No change to how FreeBSD hosts behave. No new privileged commands in phases A and B.

## Constraints

- Read-only everywhere in phases A and B; all new readers run without sudo.
- A host that cannot be classified (`uname` fails, an OS we do not know) is treated as FreeBSD for the commands that
  already work (today's behaviour), and marked `unknown OS` in its detail, so an existing setup never changes meaning.
- The first probe of a host asks for the OS together with what it asks for today (no extra round trip per probe; the
  OS is asked once, as the host type is).
- Hosts whose pools list fine keep working during the change; a probe that cannot read a resource shows `?`, never a
  guessed number.
- Tests use fixtures captured from the real hosts (OmniOS `kstat`/`vmstat`/`swap -s` output, Ubuntu `/proc` files) in
  addition to unit tests; FreeBSD fixtures stay as they are.

## Left to the builder

The shape of the OS value (an enum next to `HostKind`), where the per-OS command tables live, how `Section` support is
expressed (a function of OS and section), the exact illumos commands for memory and CPU after trying them on `omnios`,
the wording of "not supported" messages, tests, and the book (chapter 5 "Remote hosts": which OSes and what works; a new
short chapter or appendix section "Linux and illumos"; the sudoers part), README.

## Done means

Spec, plan, implementation with tests, whole-branch review, live check on `ub24` and `omnios` (Disks lists the real
disks on both; Boot Envs lists the OmniOS boot environments and says "not supported on Linux yet" on Ubuntu; Shares shows
the right service state on both; Dashboard and Hosts rows show real memory, CPU, load, swap once phase B is done; no
sudo errors in the TUI or the daemon log), `print-sudoers` and `check-host` verified against both hosts, book updated,
merged and installed.
