# Quotas and reservations: design

Date: 2026-10-18. Status: draft for approval. Slice 1 of 4 (quotas and reservations, hybrid pools, checksum and dedup,
encryption). Each slice has its own spec, plan, branch and review.

## Why

An admin who shares a pool among people or services caps them with `quota` and `refquota` and protects what must never
run out with `reservation` and `refreservation`. zfsctl shows these properties (the Datasets property tab, the panel's
Quota/Ref/Refres lines) and edits them through the general free-text property form, which accepts anything and explains
nothing. The user does not have a story that needs this; it is for admins who will, so it should be easy to do right and
hard to do wrong.

## Goals

1. **A guided edit for the four properties** (`quota`, `refquota`, `reservation`, `refreservation`) on a dataset:
   `Enter` on one of those rows opens a form that takes a size (`10G`, `500M`, `1.5T`; K/M/G/T/P, powers of 1024) or `none`,
   shows the current value and what the property means in one line, and refuses before running anything when:
   the size does not parse; the new quota (or refquota) is below what the dataset already uses (ZFS refuses this too, but
   the message should be ours); a reservation is larger than the dataset's quota; a reservation is larger than the space the
   pool still has available (it would fail, or starve its siblings); the dataset is a volume (zvols use `volsize` and
   `refreservation` only: `quota`/`refquota`/`reservation` are not offered for them).
2. **See quota use at a glance**: the Datasets list gets a **Quota** column (the limit closest to being hit, `-` when none)
   and the panel's Used line is judged against it when there is one (a dataset at 96% of
   a 10G quota reads 96%, not 4% of the pool).
3. **Dashboard lines.** Needs attention gets, per host, one line for the worst dataset near its limit: yellow from 90% of
   its quota, red from 98%, text like `tank/home 94% of its 100G quota`. The thresholds are `[dashboard] quota_warn` and
   `quota_crit` (defaults 90 and 98) and are editable in the Settings screen like the others. A dataset with no quota is
   never listed. Hosts are scanned by the same pass that already reads datasets for the daily check (one
   `zfs list -H -p -o name,used,quota,refquota,refreservation,reservation,type`), not by a new probe per refresh; the
   result goes into the existing state file so the Dashboard reads it like SMART and shares.
4. **Dropped (my call):** a "reservation not fully backed" line. ZFS refuses to set a reservation it cannot back, so the case
   is rare and not reliably visible from `zfs list`.

## Non-goals

- Quotas on users and groups (`userquota@`), project quotas, `volsize`.
- Setting a quota on many datasets at once, or presets/templates.
- Changing anything on the hub's own datasets differently from any other host (same code path).

## Constraints

- Works through `zfs set`/`zfs get`/`zfs list` only: no new sudoers entry, works on FreeBSD, Linux and illumos hosts.
- Raising or removing a limit is a plain confirm; **lowering a quota or raising a reservation** gets the two-key confirm
  (the user's data-safety rule), because they can make writes fail for the people using the dataset.
- Value parsing and the arithmetic live in one pure module with tests; the form is thin.
- The general property form keeps working for every other property and for these four if someone prefers it.

## Left to the builder

Where the size parser lives, the column width rules on narrow terminals (the new column drops first), the exact wording,
whether the Dashboard scan shares code with `shares_check`/`smart_check`, tests, and the book chapters (09 Datasets, 06
Dashboard, 21 reference config) and README.

## Done means

Spec, plan, tests, whole-branch review, live check on the hub and a VM (set a 1G quota on a scratch dataset, fill it past
90% and see the Dashboard line, then remove the quota and see it go; try the refusals), merge and install.

## Decisions from the builder check (my calls)

- **Arithmetic.** `quota` is compared with `used` (dataset + descendants + snapshots); `refquota` with `referenced`. A
  dataset's percentage is the larger of `used/quota` and `referenced/refquota`; the column and the line show that limit.
  `zfs list -p` prints unset as `0` (or `-`); the display is `-`.
- **Refusals.** New quota < `used`, or new refquota < `referenced`: refused. `reservation` > `quota`, `refreservation` >
  `refquota` and `refreservation` > `quota`: refused; other pairs are left to ZFS and its error is shown. Which pairs ZFS
  itself rejects is **verified empirically on a VM before the plan** (project rule). Pool space: the increase (new minus old
  reservation) must be <= the dataset's `available`.
- **Confirm tiers.** A change that leaves under 10% headroom over current use (writes would soon fail), or raises a reservation
  by more than 10% of the pool's available space, is the two-key confirm. Everything else is plain. Removing a limit is plain.
- **Volumes.** Offered: `refreservation` (`none` is allowed and normal for sparse volumes). `reservation` and the quotas are
  not offered for volumes; verify on a VM whether `reservation` is settable on one and relax if so.
- **Where.** The guided edit is `Enter` on these rows of the dataset Properties tab (the pool root dataset included). The
  Datasets panel's Quota line shows only `quota` today; it is extended to show `refquota`.

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
