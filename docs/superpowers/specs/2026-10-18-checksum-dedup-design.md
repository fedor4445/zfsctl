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
   - `checksum=off` is **refused** (not offered): the plan says why in one line. (Users who truly want it have the shell.)
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
