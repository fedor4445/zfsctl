# Hybrid pools (log, cache, special, dedup, spare): design

Date: 2026-10-18. Status: draft for approval. Slice 2 of 4.

## Why

A hybrid pool puts a few fast devices next to the big slow ones: a **log** (SLOG, absorbs synchronous writes), a **cache**
(L2ARC, a second-level read cache), a **special** vdev (metadata and small blocks), a **dedup** vdev (the dedup table) and
hot **spares**. Today zfsctl can create a pool and add vdevs from a Disks selection, but only as stripe/mirror/raidz data
vdevs; a new SSD cannot be added as a log or cache without typing `zpool add` by hand, the Pools screen does not show which
devices play which role, and nothing stops the one dangerous mistake (a lone special vdev). The user asked for the option to
set up a new disk as one of these.

## Goals

1. **A Role in "Add vdev"**, both from the Disks section (`AddVdevFromDisks`, which today has only the redundancy picker) and
   from the Pools section (`AddVdev`, raw spec): `data` (today's behaviour), `log`, `cache`, `special`, `dedup`, `spare`.
   The role plus the redundancy picker build the spec `zpool add pool [role] [mirror] disks…`; the confirm shows the exact
   `zpool add …` line. Rules, enforced before anything runs: `special` and `dedup` require a mirror (two or more disks) or
   raidz matching the pool's data redundancy, never a single disk, because losing one of them loses the pool; `log` may be a
   single disk or a mirror (a mirror is advised and said so); `cache` and `spare` are stripes of one or more disks (no
   redundancy choice; `cache` is never mirrored); `data` as today. Disks that are in use are refused as now.
2. **Remove log, cache and spare devices** (`zpool remove`), from the Pools vdev view: these are safe and reversible, so they
   get a normal confirm. Removing a special/dedup/data vdev is **not offered** (it needs device removal support and has
   conditions; out of scope, said plainly in the status line).
3. **Show the roles.** The pool's vdev view groups devices under their class (the headings `zpool status` prints: `logs`,
   `cache`, `spares`, `special`, `dedup`) and the Pools panel lines show per-class sizes from `zpool list -v`. The existing
   L2ARC hit-rate line stays. A host with no such devices looks as it does now.
4. **Special-class tuning**: `special_small_blocks` gets a guided edit (a size like `32K`, or `0`; powers of two up to the
   dataset's recordsize; the form says that only new writes are affected and that datasets with a recordsize at or below
   the value will put *all* their data on the special vdev).
5. **Dashboard lines.** A pool whose special or dedup vdev is a single disk (or not redundant) is a red Needs attention line
   `tank special vdev has no redundancy`; a log device that is `FAULTED`/`UNAVAIL` is a yellow line `tank log device faulted`
   (the pool keeps working); a degraded cache device is a yellow line. These come from the pool facts the health probe
   already reads (`zpool status`), extended to keep the class of each leaf.

## Non-goals

- Creating a pool with special/log/cache vdevs in the same step (add them after; a later slice can add roles to Create pool).
- Removing special/dedup/data vdevs, `zpool attach`/`detach` to turn a single log into a mirror (maybe later), L2ARC
  persistence tuning, `zpool split`.
- Choosing which SSD to use: that stays the Disks section's job.

## Constraints

- `zpool add`, `zpool remove`, `zpool list -v`, `zpool status` only: core sudoers, all three OSes (spec words are the same).
  `special`/`dedup` exist from OpenZFS 0.8; on an older pool the command fails and its error is shown verbatim.
- Adding any vdev is irreversible for data/special/dedup: the two-key confirm applies, with the exact command visible.
  A dry run (`zpool add -n`) is shown in the confirm text so the user sees the resulting layout first.
- Unknown/odd `zpool status` layouts must degrade to "no class heading found", never to a wrong role.

## Left to the builder

Parsing the class headings, the form layout, how roles map to the Disks section's selection (a role on a multi-disk
selection), the exact wording, tests with real `zpool status` captures (hub, `ub24`, `omnios`; build a hybrid test pool on
a VM from file-backed or spare disks), and the book (08 Pools, 11 Disks, 06 Dashboard), README.

## Done means

Spec, plan, tests, whole-branch review, live check on a VM with spare disks (add a mirrored special, a log, a cache, a spare;
see the Pools view and the Dashboard lines; remove the log and cache; try the single-disk special refusal), merge, install.
