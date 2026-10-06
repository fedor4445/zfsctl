# Snapshot Schedules and Retention Design

## Summary

A second job kind, **snapshot**, for the scheduler daemon: take a ZFS snapshot of one dataset on a cron schedule and keep only the newest N snapshots that job created. It is managed from the Jobs section of the TUI, and from the Datasets section, like scrub jobs.

This is the first half of sub-project 3. **Replication** (`zfs send | zfs receive` between hosts) is a separate job kind with its own spec, because it needs its own progress and cancellation design. Nothing here depends on it.

## Context

The four planned sub-projects:

1. Shell-out terminal. **Done.**
2. Scheduler daemon plus scrub scheduling. **Done.** The daemon, `jobs.toml`, history, notify hook and the Jobs section all exist and are live-verified on the hub.
3. **Snapshot schedules with retention (this spec), then replication (later spec).**
4. File-level backups with restic.

Decisions made while brainstorming:

- **Retention is keep-last-N per job.** Each job names its snapshots `<prefix>-<timestamp>` and keeps the newest N of its own. It never touches snapshots it did not create. Hourly/daily/weekly tiers are separate jobs with separate prefixes.
- **One dataset per job, with an optional `recursive` flag** (`zfs snapshot -r`), so a tree is snapshotted atomically and pruned together.
- **Replication is its own job kind, specced later.**
- The job model gains **flat optional fields** on the existing `Job` struct, not a second table or an untyped params blob. `kind` is already a plain string so a `jobs.toml` written by a newer build still parses.

## Goals

- A snapshot schedule per dataset, on any managed host, that keeps firing with nobody logged in.
- Bounded space: a job never keeps more than `keep` of its own snapshots.
- A run can never destroy a snapshot the job did not create.
- A failure (including a snapshot that could not be destroyed) is visible in the TUI and reaches the notify command like any other failed job.
- No new sudoers entries: `zfs snapshot`, `zfs list` and `zfs destroy` already run through the allowed `/sbin/zfs`.

## Non-goals

- Replication, send/receive, bookmarks.
- Tiered retention (hourly/daily/weekly counts) inside one job.
- Matching many datasets with a glob or pattern.
- Pruning snapshots the job did not create, or pruning by age.
- Holds, clones or rollback handling beyond reporting a failed destroy.

## Design

### The job

New constant `KIND_SNAPSHOT = "snapshot"`. `Job` gains four optional fields, all skipped on serialize when unset, so existing scrub jobs and files are unchanged:

| field | meaning |
|---|---|
| `dataset` | the dataset to snapshot, e.g. `tank/data`. Required for a snapshot job. |
| `recursive` | `zfs snapshot -r` and recursive pruning. Default `false`. |
| `prefix` | the snapshot name prefix. Default `auto`. |
| `keep` | how many of the job's snapshots to keep. Required, at least 1. |

```toml
[[jobs]]
id = "snap-tank-data-hourly"
host = "local"
kind = "snapshot"
dataset = "tank/data"
recursive = true
prefix = "hourly"
keep = 24
schedule = "0 * * * *"
```

`pool` stays what it is for scrub jobs; a snapshot job leaves it unset.

### Validation (`jobs::resolve`)

`resolve` becomes kind-aware. For a snapshot job it requires:

- a `dataset` that is non-empty and uses only the ZFS name characters (`A-Za-z0-9_-:./`), with no `@` and no leading `-`;
- a `prefix` that is non-empty and uses only `A-Za-z0-9_-.`;
- `keep >= 1`;
- a known host and a parseable schedule, as for scrub.

It also rejects **two enabled snapshot jobs on the same host, dataset and prefix**: they would prune each other. The later one in `jobs.toml` is reported unsupported with that reason, the same way an unsupported kind is.

### Snapshot names

`<prefix>-<UTC timestamp>` where the timestamp is `YYYYMMDDTHHMMSSZ` (for example `hourly-20261006T193000Z`). It has no colons, sorts chronologically as a string, and is independent of the hub's local time zone, so DST never produces duplicate or out-of-order names.

### A run

Snapshot runs are synchronous and short: no watcher, no resume, no orphan handling. Under a lock on `(host, dataset)` (the existing `Locks`, keyed by a distinct string so it cannot collide with a pool name):

1. If the lock is held, record `Skipped: already running`.
2. Run `zfs snapshot [-r] -- dataset@name`.
   - If the snapshot already exists (a catch-up landing in the same second as a scheduled run), record `Skipped: already exists`.
   - Any other failure is `Failed` with the command's error, and **retention does not run**.
3. List snapshots (`zfs list -t snapshot -H -p -o name,used -r dataset` when recursive, `-d 1` otherwise; the existing parser). Keep those whose snapshot name matches `^<prefix>-\d{8}T\d{6}Z$` exactly, then group by snapshot name, newest first.
4. For each snapshot name beyond `keep`, run `zfs destroy [-r] -- dataset@name`. A failure (a hold, a clone) is remembered and the loop continues.
5. Record the result:
   - all good: `Ok`, message `created hourly-20261006T193000Z; pruned 2`;
   - any destroy failed: `Failed`, message `created X; could not destroy Y: <error>`. The snapshot exists, but the run needs attention and notifies like any failure.

History records the same `started`/`finished` events as a scrub; the existing `Last` column and failed-jobs indicator work unchanged.

### Daemon wiring

`Work::Run` is dispatched on `job.kind`: `scrub` goes to `run_scrub`, `snapshot` to the new `run_snapshot`. `Resume` is scrub-only: in `hand_out_pending`, an unfinished run whose job is a snapshot job is handed out as `Orphan` (closed as lost), never resumed, because a snapshot run records its finish before returning, so an unfinished one means the daemon died mid-run. Catch-up applies unchanged: a missed slot yields one snapshot at startup, controlled by `catch_up` (default on).

### TUI

- **Jobs section:** `N` opens a snapshot-job form (Host, Dataset, Recursive (y/n), Prefix, Keep, Schedule, Catch up (y/n)). `Enter` edits using the form for the job's kind. `n` is unchanged (scrub).
- **F2:** Host (configured hosts), Dataset (the chosen host's datasets, fetched live in the same way as the scrub form's Pool picker), Schedule (the existing presets).
- **Datasets section:** `J` on a dataset opens the snapshot form prefilled with it, like `J` on a pool opens the scrub form.
- **Table:** the `Pool` column becomes `Target` (a pool or a dataset). The detail pane for a snapshot job also shows prefix, keep and recursive, and the next three run times as before.
- **Form validation** reuses `resolve`'s rules, so what the form accepts is exactly what the daemon will run, including the duplicate-prefix check against existing jobs.

### Documentation

README: a snapshot job example under Scheduled jobs, the retention rule ("a job only ever destroys snapshots it created, matched by its prefix and the timestamp pattern"), and the new keys. In-app help lists `N` and the Datasets `J`.

## Edge cases

- **Manual or other jobs' snapshots with a similar name:** only an exact `prefix-<timestamp>` match is prunable; `hourly-keep-forever` and `daily-2026...` are never touched by an `hourly` job.
- **Changing `keep` down:** the next run prunes the excess. Changing it up prunes nothing.
- **Changing `prefix`:** the old prefix's snapshots are no longer owned by the job and are left in place; this is stated in the form's description.
- **Dataset renamed or destroyed:** the snapshot step fails with the zfs error, recorded as `Failed`, retention skipped.
- **Recursive with a child added later:** the new child gets the next snapshot automatically; it has no older ones to prune.
- **Recursive prune where a child lacks an old snapshot:** `zfs destroy -r dataset@name` skips children that do not have it.
- **Clock set back:** names sort by timestamp, not by age; a backwards clock jump can make a new snapshot sort older than existing ones and be pruned first. Accepted, same as any timestamp-named scheme.
- **Unreachable host:** the zfs call fails and the run is `Failed`; the next slot tries again.

## Testing

- `jobs`: round-trip of a snapshot job through `jobs.toml`; scrub jobs serialize exactly as before; `resolve` rejects each invalid field, `keep = 0`, an unknown host, and a duplicate host+dataset+prefix.
- Snapshot run (fake runner): creates the snapshot and prunes the oldest beyond `keep`; leaves non-matching names alone; skips when locked or when the name exists; a failed snapshot skips retention; a failed destroy continues with the rest and reports `Failed`; recursive and non-recursive build the right `zfs` argument lists.
- Name generation and the exact-match pattern, including near-misses.
- Engine dispatch by kind; a `started` snapshot run is closed as lost on restart.
- TUI: the form opens, validates with `resolve`'s messages, saves and edits; `J` on a dataset prefills; F2 on Host, Dataset and Schedule; the table and detail pane render the new kind.
- **Live on the hub (verify empirically, not just via `zfs list`):** confirm on 15.1-RELEASE and 16.0-CURRENT that `zfs snapshot -r` and `zfs destroy -r dataset@snap` behave as assumed (including `--`), then run a real every-minute job on `scratch/data` with `keep = 3` and check that exactly three snapshots remain, that a hand-made snapshot with a different name survives, and that a held snapshot produces a `Failed` run with the others still pruned.

### Live results

Checked 2026-10-06 on 15.1-RELEASE-p4 (`scratch/data`) and 16.0-CURRENT (`zroot/tmp`, a throwaway snapshot), identical on both:

- `zfs snapshot -r -- dataset@name` works; `zfs list -t snapshot -H -p -o name,used -d 1 dataset` lists only that dataset's snapshots, and `-r` lists the tree's.
- A duplicate snapshot fails with `cannot create snapshot 'dataset@name': dataset already exists` (the run treats `already exists` as a skip).
- Destroying a held snapshot fails with `cannot destroy snapshot dataset@name: it's being held. Run 'zfs holds -r ...' to see holders.`
- `zfs destroy -r -- dataset@name` works after the hold is released.

- **Daemon, 2026-10-06 (hub, 15.1-RELEASE):** an every-minute job on `scratch/data` with `prefix = "live"`, `keep = 3` produced `Ok: created live-...; pruned 0` for three runs, then `pruned 1` on each later run; exactly three `live-*` snapshots remained, and a hand-made `manual-keepme` and the older `snap1/2/4` survived. With the oldest `live-*` held, the next run was `Failed: created ...; pruned 1; could not destroy live-...: ... it's being held ...` and the other excess snapshot was still pruned. All test snapshots were destroyed afterwards.
- **TUI, same day (driven through a pty on the hub):** `N` opens the snapshot form with the defaults; F2 on Dataset lists the hub's real datasets; the saved job shows `scratch/data` in the Target column and `keep 24  prefix auto` in the panel; `Enter` re-opens it in the snapshot form; a second job with the same host, dataset and prefix is refused with `Job '...' already snapshots scratch/data with prefix 'auto': they would prune each other. Use another prefix.`; `d` deletes it.

## Delivery

One implementation plan: the job model and validation, the run and retention logic, the daemon dispatch, the TUI form and keys, then docs and the live check.

## Files touched (indicative)

`src/jobs.rs`, `src/daemon/snapshot.rs` (new), `src/daemon/mod.rs`, `src/daemon/engine.rs`, `src/zfs/snapshot.rs` (recursive create and destroy), `src/ui/jobs.rs`, `src/ui/state.rs`, `src/ui/modal.rs`, `src/ui/picker.rs`, `src/ui/layout.rs`, `src/ui/hints.rs`, `README.md`.
