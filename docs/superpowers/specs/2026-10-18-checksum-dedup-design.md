# Checksum and deduplication: design

Date: 2026-10-18. Status: draft for approval. Slice 3 of 4.

## Why

`checksum` and `dedup` are ordinary dataset properties that zfsctl already lists and edits as free text. Both are easy to
get wrong: `checksum=off` removes ZFS's integrity guarantee for new writes, and `dedup=on` costs a lot of RAM and cannot be
cheaply undone. Admins should be able to choose them knowingly, from the real list of values, with the consequences said
before they commit.

## Goals

1. **Value pickers.** `Enter` on `checksum` opens a list: `on` (the default, fletcher4), `fletcher4`, `sha256`, `sha512`,
   `skein`, `edonr`, `blake3`, each with one line (speed, strength, "needs OpenZFS 2.x" where it applies), and the list is
   limited to what the host supports (an unsupported value is not shown; if the host's list cannot be known, show the
   common ones and let the command's own error stand). `Enter` on `dedup` opens: `off`, `on`, `verify`, `sha256`,
   `sha256,verify`, `sha512`, `sha512,verify`, `skein`, `skein,verify`, `edonr,verify`, `blake3`, `blake3,verify`.
2. **Guards.**
   - `checksum=off` is **allowed behind a typed confirm** (type the dataset name; decided by the user 2026-10-18): the text
     says there is no integrity checking for new writes and silent corruption will not be detected or repaired. It is judged in
     the shared apply path, so the free-text form needs the same confirm.
   - Every checksum change says it applies **only to data written afterwards** and offers a reminder to scrub (does not
     start one).
   - Turning `dedup` on is a typed confirm (type the dataset name): the text states that the dedup table must fit in RAM
     to stay fast, that turning it off later does not undeduplicate existing data, and shows the pool's current dedup ratio
     and, when the host can say, the table size (`zpool status -D`, read on demand when the form opens, with a timeout).
     Turning it off is a plain confirm.
   - A `dedup` value that is not `off` on a pool with less than 8 GiB of RAM free-and-cache on the host is a warning line in
     the form, not a refusal.
3. **Dashboard lines.** Per host, a yellow line when any dataset has `dedup` on and the pool's dedup ratio is below 1.10x
   (`tank dedup is on but saves only 1.02x`: paying RAM for nothing), and a yellow line when any dataset has a
   non-default `checksum` of `off` (set outside zfsctl): `tank/scratch checksum is off`. Both from one `zfs get -H -p -t
   filesystem,volume checksum,dedup` and `zpool list -o name,dedupratio` in the daily-check pass (no per-refresh cost),
   stored in the existing state file.
4. **Panel.** The Datasets panel shows `Dedup` and `Checksum` when they are not the defaults; the Pools panel already shows
   the dedup ratio.

## Non-goals

- Enabling `fast dedup` features (OpenZFS 2.3 `feature@fast_dedup`, `dedup_table_quota`), pruning the DDT, `zdb -S`
  simulation. A later slice could add the table quota.
- Converting existing data (rewrite to a new checksum).

## Constraints

- `zfs set`/`zfs get`/`zpool` only; all three OSes; no new sudoers entries.
- The pickers reuse the existing picker/F2 machinery for property forms; unknown properties keep the free-text form.

## Left to the builder

Where the host's supported checksum list comes from (probing `zfs set` is not allowed: derive from the OpenZFS version in
the host summary, with a table in code), the wording, the RAM check source (the Resources reading), tests, book chapter 09,
README.

## Done means

Spec, plan, tests, whole-branch review, live check on the hub and a VM (change a scratch dataset's checksum and dedup; see
the refusals and confirms; see the Dashboard lines appear and clear), merge, install.

## Decisions from the builder check (my calls)

- **Supported values** come from a table in code keyed by the OpenZFS version in the host summary: `on`, `fletcher4`, `sha256`,
  `sha512` always; `skein`, `edonr` from 0.8; `blake3` from 2.2. A host whose version is unknown (illumos has no `zfs version`)
  is offered only `on`, `fletcher4`, `sha256`, `sha512`. Dedup values follow the table; `edonr` only as `edonr,verify`. On an
  encrypted dataset the list is limited to what ZFS allows (verified on a VM; interplay with slice 4).
- **The guard is in the apply path, not the picker.** The picker and the free-text property form go through one validation
  step in the set-property path, so `dedup=on` and `checksum=off` always need the typed confirm.
- **`zpool status -D`** is read asynchronously when the confirm opens, 10-second limit; on timeout or failure the confirm
  says "table size unknown" and the user can still go ahead.
- **RAM warning:** the host's physical RAM (the Resources reading exists for all three OSes) below 16 GiB is a warning line,
  never a refusal.
- **Dashboard.** Dedup: one line per pool, only for `local`/`received` `dedup` values (not inherited copies), only when the pool
  has more than 10 GiB allocated (a pool where dedup was just enabled does not flag). Checksum off: one line per host naming
  up to three datasets and a count.
- The Datasets panel shows Dedup/Checksum when not default; both are added to the panel's property read.

## Decided by the user (2026-10-18)

- `checksum=off` is allowed behind the typed confirm, with the persistent Dashboard line `tank/scratch checksum is off`.

## Shared by all four slices: the storage reading (my call, from the builder check)

The four slices add readings per host (quota use, checksum/dedup, key status, pool device classes). They share **one** new
check, built in slice 1 and extended by the others, so they do not each edit `HostChecks`:

- A third check next to disks and shares: `daemon/storage_check.rs`, state file `storage.json` in the state directory,
  `HostChecks` gains a `storage` field (merged like `merge_disks`/`merge_shares`), the same running marker, and `c` on demand
  in Hosts runs all three. Per host it holds the datasets' limits and use, the `checksum`/`dedup`/`keystatus`/`encryptionroot`
  values and the pool facts each slice needs; each slice adds its own fields (all `#[serde(default)]`).
- **Cadence:** every 10 minutes, not daily (a quota line must clear soon after the fix). It is one `zfs get`/`zfs list` plus
  one `zpool status` per host, read-only, in its own loop apart from the 03:30 daily pass.
- New thresholds live under `[dashboard]` (`config.rs` `DashboardConfig`, `config_edit.rs`, the Settings screen):
  `quota_warn` (90), `quota_crit` (98), `dedup_min_ratio` (1.10). The lines also appear in the daily report email.
- Every form validates against **that host's** runner (its pool space, its OpenZFS version), never the hub's.
- Dashboard lines read `storage.json` like they read `smart.json`; a stale file (older than 3x the cadence) shows the same
  "check is N old" notice style as the disk check.
