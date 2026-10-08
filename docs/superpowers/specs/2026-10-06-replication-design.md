# Replication Design

## Summary

A third job kind, **replicate**, for the scheduler daemon: copy the newest snapshot with a given prefix from a dataset on one configured host to a dataset on another configured host (`zfs send | zfs receive`), or to a directory on the hub as stream files. Runs are incremental, resumable after an interruption, show progress in the Jobs section, and can be cancelled from it.

This is the second half of sub-project 3. It builds on snapshot jobs (which create and prune the snapshots it sends) and on the daemon, history and Jobs UI that already exist. Snapshot jobs and replication jobs are independent: replication never creates, destroys or rolls back anything on either side.

## Context

1. Shell-out terminal. **Done.**
2. Scheduler daemon plus scrub scheduling. **Done.**
3. Snapshot schedules with retention. **Done.** Replication (this spec) is the other half.
4. File-level backups with restic: later.

Decisions made while brainstorming:

- **Topology:** any two configured hosts, where the hub is the host `local`. So hub to VM, VM to hub, VM to VM and one local pool to another all work the same way. In addition, a **directory on the hub** can be a destination.
- **What is sent:** the newest snapshot named `<prefix>-<timestamp>` on the source dataset, incrementally from the newest snapshot already on the destination (or already written to the file directory). It never creates or destroys snapshots.
- **File targets** are on the hub only, with full and incremental stream files. The hub runs as root and writes the files itself, so no remote host needs a new sudoers entry.
- **Interruptions resume.** Host-to-host receives use `zfs receive -s`; the next run continues from the destination's resume token. File targets cannot resume and restart from scratch.
- **The hub runs both ends** as two child processes and copies the bytes between them. Not a `sh -c` pipeline, and no host-to-host SSH.

## Goals

- A replication schedule from any managed dataset to any other managed host, or to a hub directory, that keeps running with nobody logged in.
- A destination that is behind catches up in one incremental send.
- A transfer that takes hours can be watched (bytes, rate, estimate) and cancelled, and an interrupted one continues rather than restarting.
- Nothing on either side is ever destroyed, rolled back or mounted by default.
- No new sudoers entries: only `/sbin/zfs` is used on managed hosts.

## Non-goals

- Recursive (`-R`) sends. One dataset per job; a tree is several jobs.
- Pruning snapshots on the destination, or on the source.
- Encrypted raw sends (`-w`), compression flags, bandwidth limits.
- File destinations on remote hosts.
- Restoring from stream files (documented as `zfs receive < file`, in order; no UI).

## Design

### The job

New constant `KIND_REPLICATE = "replicate"`. `Job` gains flat optional fields (all skipped on serialize when unset, so existing files are unchanged):

| field | meaning |
|---|---|
| `dataset` | (reused from snapshot jobs) the source dataset. |
| `prefix` | (reused) which snapshots to send; default `auto`. |
| `dest_host` | a configured host name for host-to-host, or the keyword `file`. Required. |
| `dest_dataset` | the destination dataset (host-to-host). |
| `dest_dir` | an absolute directory on the hub (file target). |
| `force` | adds `-F` to the receive. Default `false`. |

`host` is the **source** host. `keep` and `recursive` are unused.

```toml
[[jobs]]
id = "repl-tank-data-to-vm"
host = "local"
kind = "replicate"
dataset = "tank/data"
prefix = "hourly"
dest_host = "fbsd16"
dest_dataset = "backup/tank-data"
schedule = "30 * * * *"

[[jobs]]
id = "repl-tank-data-to-usb"
host = "local"
kind = "replicate"
dataset = "tank/data"
prefix = "daily"
dest_host = "file"
dest_dir = "/mnt/usb/zfsctl"
schedule = "0 4 * * *"
```

### Validation (`jobs::resolve`)

For a replicate job: `dataset` and `prefix` valid as for snapshot jobs; `host` known; if `dest_host == "file"` then `dest_dir` is an absolute path with no `..` component, no NUL and only `A-Za-z0-9_-./ ` (no leading `-`), and `dest_dataset` is unset; otherwise `dest_host` is a known host, `dest_dataset` a valid dataset, and `dest_dir` unset. Source and destination must not be the same host and dataset. (Review ruling: only the destination is locked at run time, so two jobs may send from one source dataset to different destinations; `force` applies only to incremental or resumed receives, never to a full send.) Two enabled replicate jobs may not share a destination (same `dest_host` and `dest_dataset`, or the same `dest_dir`): they would corrupt each other's incrementals. A clash flags the later job, like snapshot jobs.

### The runner can hand out a command

`CommandRunner` gains `fn command(&self, cmd: &str, args: &[&str]) -> Result<std::process::Command>`: the same command the runner would run (locally `cmd args`; over SSH `ssh ... sudo -n cmd args`, built by the same `build_args` and quoting), unspawned, so the caller can wire its pipes. The default implementation returns an error, so existing fakes are unaffected; `LocalRunner` and `SshRunner` implement it. Tests use a fake whose `command` returns `printf`/`cat` commands.

### A host-to-host run

Under locks on `(source host, source dataset)` and `(dest host, dest dataset)` (distinct key shapes from scrub and snapshot locks):

1. **Resume.** `zfs get -H -p -o value receive_resume_token <dest>`. A token (not `-`) means an interrupted receive: the send side is `zfs send -t <token>` and no snapshot selection is done.
2. **Select.** Otherwise list the source's snapshots with the prefix (`zfs list -t snapshot -H -p -o name,guid,createtxg -d 1 <src>`, filtered by `jobs`' exact pattern) and take the newest. List the destination's snapshots the same way (a destination that does not exist yet has none). If the destination already has the newest source snapshot (by guid), the run is `Skipped: up to date`.
3. **Base.** The newest snapshot present on both sides by guid is the incremental base: `zfs send -i <base> <src>@<new>`. If none, a full `zfs send <src>@<new>`.
4. **Estimate.** `zfs send -nvP [-i base] <snap>` (or `-t token`) gives the size for progress. If it cannot be parsed the run continues without a total.
5. **Transfer.** Spawn `zfs send ...` through the source runner and `zfs receive -s -u [-F] -- <dest>` through the destination runner; a pump thread copies the send's stdout to the receive's stdin. The destination is received **unmounted** (`-u`).
6. **Finish.** Both exit statuses are checked (the receive's stderr is the message on failure). Success is `Ok: sent <snap> (incremental from <base> | full), <bytes>`.

A destination that was written to since its newest snapshot makes the receive fail with zfs's own message ("destination has been modified…" or "most recent snapshot … does not match incremental source"), shown as the failure; `force` adds `-F` for jobs that want the destination rolled back to the base.

### A file run

Under a lock on `dest_dir` only (the source is not locked, as for host-to-host):

1. Select the newest source snapshot with the prefix.
2. Read `replication.json` in the state dir (`{job_id: {snapshot, guid, dir}}`; the record only counts while `dest_dir` still holds the stream that ends at that snapshot). If it names a snapshot that still exists on the source and is older than the new one, send incrementally from it; otherwise a full stream. If the newest is already recorded, `Skipped: up to date`.
3. The hub spawns `zfs send ...` through the source runner and writes its stdout to `<dir>/<name>@<snap>.zfs.partial` (full) or `<dir>/<name>@<from>..<to>.zfs.partial` (incremental), where `<name>` is the source dataset with `/` replaced by `-`, then fsyncs and renames the file without `.partial`, then updates `replication.json`. A cancelled or failed run removes the `.partial`.
4. The directory is created (mode 0700) if missing.

`replication.json` stores `{snapshot, guid, dir}` per job, not just a snapshot name (planning ruling for plan 2): a name alone cannot tell a snapshot destroyed and recreated under the same name from the one that was written, and `dir` makes an edited `dest_dir` start with a full stream. An unreadable file counts as nothing recorded.

### Progress

The pump counts bytes and, once a second, atomically rewrites `<state>/progress/<run_id>.json`: `{ "job_id", "bytes", "total" (or null), "started_at", "updated_at" }`. The file is removed when the run ends. The TUI reads these files like it reads history: the job panel shows `sent 12.3 GiB of 40.0 GiB (30%)  85 MiB/s  eta 5m` for a running job, with the rate taken from the last few samples.

### Cancel

`c` on a running replication job in the Jobs section writes `<state>/cancel/<job_id>`. The daemon's one-second loop checks for those files; for a running job it sets that run's cancel flag, deletes the file and the pump stops: both children are killed, a file run deletes its `.partial`, and the run is recorded with a new result **`Cancelled`**. `Cancelled` is not counted by the failed-jobs indicator and does not notify. The destination keeps its resume token, so the next run continues. A cancel file for a job that is not running is deleted.

### Restart

A daemon killed mid-transfer leaves a `started` event and dead children. On restart the run is closed as lost (as for scrubs and snapshots; replication is never resumed in place), and the next scheduled run resumes from the destination's token. A catch-up covers a slot missed while down.

### History and results

`RunResult` gains `Cancelled`. History events use it; the Jobs table's Last column shows `cancelled`, and `run_summary` and `last_result_text` handle it. Unknown results in an older binary's history reader are an existing concern of the history format and are not addressed here.

### TUI

- `R` in Jobs opens the replication form: Source host, Source dataset, Prefix, Destination host (or `file`), Destination dataset or directory, Force (y/n), Schedule, Catch up (y/n). F2: Source host and Destination host list the configured hosts (the latter also `file`); the two dataset fields list the chosen host's datasets live (existing mechanism); Schedule lists the presets. `Enter` edits a replicate job in this form.
- The form refuses what `resolve` refuses, plus a clashing destination.
- `c` appears in the hints for a running replicate job.
- The Target column shows `src → dest`; the job panel shows prefix, destination, force and, while running, the progress line.
- `Space` refuses to enable a job that would clash on its destination, as for snapshot jobs.

### Documentation

README: a "Replication" subsection under Scheduled jobs (both example jobs, the incremental and resume behaviour, "nothing is destroyed or mounted", restoring from stream files), the new keys, and a note that `replicate_snapshot` was removed. The in-app help lists `R` and `c`.

## Retired code

`zfs::snapshot::replicate_snapshot` (the `sh -c` pipeline) and its two validators, which have no callers and would invite the quoting and `sh` problems this design avoids, are removed along with their tests. Their three dead-code clippy warnings go with them.

## Edge cases

- **Destination missing:** the first run does a full send; the receive creates the dataset. A missing parent dataset makes the receive fail with zfs's message.
- **Destination ahead or diverged:** fails with zfs's message unless `force` is set.
- **Source snapshot pruned between runs:** the base is chosen by guid among what both sides still have, so a prune on the source only costs a longer incremental or a full send, never a wrong one. If no common snapshot exists but the destination has snapshots, the receive fails (full into an existing dataset) with zfs's message.
- **Resume token stale** (the source snapshot it needs is gone): `zfs send -t` fails; the run reports it, and the failure message says to run `zfs receive -A <dest>` on the destination to discard it. The job does not discard it automatically.
- **Two jobs, one source:** allowed (different destinations).
- **Network drop:** the send or receive exits non-zero; the run is `Failed`; the next run resumes.
- **Disk full on the hub (file target):** the write error fails the run and removes the `.partial`.
- **A transfer longer than the schedule interval:** the lock makes the next slot `Skipped: already running`; that slot is not lost, since the following run sends the newest snapshot.
- **Progress file left by a crash:** removed on the next start, with any `.partial` files of unfinished runs.

## Testing

- `jobs`: round trip; scrub and snapshot jobs unchanged; `resolve` rejects each bad field (`dest_dir` relative, with `..`, leading `-`; both or neither destination; source equal to destination; unknown hosts) and the shared-destination clash.
- Selection logic (pure, on parsed listings): newest-with-prefix, common base by guid, up to date, no common snapshot, token present.
- A host-to-host run with a fake runner whose `command` returns scripted `printf`/`cat` processes: correct argument lists for full, incremental, resume and `force`; success, send failure, receive failure, and `Skipped`.
- The pump: bytes counted, progress file written, cancel kills both children and records `Cancelled`; a file run's `.partial` is removed on cancel and failure and renamed on success; `replication.json` updates only on success.
- `RunResult::Cancelled` is not counted as failed and does not notify.
- Engine/daemon: dispatch by kind; an unfinished replicate run is closed as lost; startup removes stale progress and `.partial` files.
- TUI: the form opens, validates with `resolve`'s rules, saves and edits; F2 on each field; `c` writes the cancel file only for a running job; the job panel renders progress; the Target column and hints.
- **Live (verify empirically, not just via `zfs list`), before and after implementation:** on 15.1 and 16.0-CURRENT confirm the exact output of `zfs send -nvP`, `zfs get -H -p -o value receive_resume_token`, `zfs list -o name,guid,createtxg -d 1`, `zfs receive -s -u`, `zfs send -t`, and the error texts for a modified destination. Then run: a full send hub to VM, an incremental, an up-to-date skip, a cancel mid-transfer and the resume after it (use a few GB of incompressible data on a scratch dataset so the cancel lands mid-transfer), a modified destination with and without `force`, a VM-to-hub pull, and a file target with a restore (`zfs receive < file`) into a scratch dataset, checking the data matches.

### Live results

Checked 2026-10-06 on 15.1-RELEASE (hub, `scratch/repl-*`, piped locally through `sh -c`) and 16.0-CURRENT (VM, `zroot/home/fbsd` -> `zroot/repl-dst`, driven as two separate `ssh ... sudo zfs send` / `sudo zfs receive` processes piped on the workstation: the VM's sudoers does not allow `sh`, which is exactly why the design pipes the processes itself). Identical on both:

- `zfs send -nvP -- ds@snap` prints `full<TAB>ds@snap<TAB>bytes` and `size<TAB>bytes` on **stdout** (nothing on stderr). Incremental: `incremental<TAB>ds@base<TAB>ds@snap<TAB>bytes` then `size<TAB>bytes`. `zfs send -nvP -t <token>` prints a `resume token contents:` nvlist block, then the same `full`/`incremental` and `size` lines, so "the line starting `size<TAB>`" is the stable thing to parse.
- `zfs send -i @base -- ds@snap` is accepted; `zfs send -t <token>` has no snapshot argument.
- `zfs list -t snapshot -H -p -o name,guid,createtxg -d 1 ds` prints `ds@name<TAB>guid<TAB>createtxg`. On a missing dataset (list, get or receive -A) the error is `cannot open 'ds': dataset does not exist`.
- `zfs get -H -p -o value receive_resume_token ds` prints `-` when there is none.
- `zfs receive -s -u -- ds` leaves the dataset unmounted (`mounted` is `no`) and `receive_resume_token` is `-` after a complete receive.
- An interrupted receive (stream cut off) fails with `cannot receive incremental stream: checksum mismatch or incomplete stream. Partially received snapshot is saved.` plus a `zfs send -t <token>` hint, and `receive_resume_token` is then a long `1-...` string. Sending with `-t <token>` into the same dataset completes it, after which the token is `-` again.
- `zfs receive -A ds` with no token: `'ds' does not have any resumable receive state to abort`.
- A full stream into an existing dataset fails with `cannot receive new filesystem stream: destination 'ds' exists` / `must specify -F to overwrite it`.
- **A destination is "modified" only when data was written to it since its newest snapshot.** Taking a new, empty snapshot on the destination does *not* stop the next incremental from being received. Writing a file into the (mounted) destination makes the receive fail with `cannot receive incremental stream: destination ds has been modified` / `since most recent snapshot` (two lines), and the same receive with `-F` succeeds, rolling the destination back. The sending side adds `warning: cannot send ...: signal received`, which is noise.

### Live results: the daemon and the TUI (2026-10-06, hub 15.1-RELEASE with the 16.0-CURRENT VM `fbsd16`)

- **Pull, VM to hub:** `Ok: sent pull-... (full), 65.3 KiB`, received unmounted; the next run `Skipped: up to date`; the snapshot guid is identical on both sides.
- **Full, VM to the same VM through the hub (two SSH processes, 801.5 MiB):** the Jobs panel showed `sent 38.2 MiB of 801.5 MiB (4%)  38.2 MiB/s  eta 19s`, later `34%  67.6 MiB/s`; the `c` hint appeared only while it ran.
- **Cancel from the TUI:** `c` recorded `Cancelled: cancelled after 368.7 MiB; the next run resumes` within about a second; the Last column read `cancelled`; no `zfs send`/`zfs receive` processes were left on either host; the destination held a resume token.
- **Resume:** the next run was `Ok: resumed an interrupted transfer, 435.5 MiB` (368.7 + 435.5 = the 801.5 MiB total); destination guid equal to the source's, token back to `-`, `mounted` `no`.
- **Incremental:** after 100 MiB of new data and a new snapshot, `Ok: sent ... (incremental from ...), 100.2 MiB`, then `Skipped: up to date`.
- **Daemon restart mid-transfer (800 MiB):** progress and cancel files were gone after the restart; the unfinished run was closed `failed: lost`; no stray processes; the next run `Ok: resumed an interrupted transfer, 663.2 MiB` with matching guids. (The lost run's message used to say "the job was removed or can no longer run"; it now says the daemon stopped.)
- **Modified destination (hub, local to local):** a file written into the destination made the run `Failed: receive failed: cannot receive incremental stream: destination scratch/repl-dst has been modified since most recent snapshot` (both lines of zfs's message kept); with `force = true` the next run was `Ok: sent ... (incremental from ...)` and the destination was rolled back (the written file was gone).
- **TUI form:** `R` opens the form with the defaults; F2 lists the configured hosts on both host fields (the destination one also offers `file`), the hub's real datasets for the source and the *destination host's* real datasets for the destination (typing `fbsd16` first); the saved job showed its source in the Target column; `Enter` re-opened it in the form; a second job with the same destination was refused (`... cannot run alongside this one: same destination as job '...': their incrementals would corrupt each other`); `d` deleted it.
- **Noted, not a defect:** the rate is computed over each one-second sample, so a stalled second shows no rate or ETA until the next sample (one sample in the restart test showed `rate: 0`).

## Delivery

Two implementation plans, one spec:

1. **Core:** the runner `command`, the job model and validation, selection logic, the host-to-host run with progress and cancel, `Cancelled`, daemon wiring and restart handling, the replication form and `c`, removal of `replicate_snapshot`, docs, and the live checks for host-to-host.
2. **File targets:** the file run, `replication.json`, `.partial` handling, the form's `file` option end to end, docs and the live file/restore check.

## Files touched (indicative)

`src/exec/mod.rs`, `src/exec/local.rs`, `src/exec/ssh.rs`, `src/jobs.rs`, `src/daemon/replicate.rs` (new), `src/daemon/mod.rs`, `src/daemon/engine.rs`, `src/daemon/history.rs`, `src/zfs/snapshot.rs`, `src/ui/jobs.rs`, `src/ui/state.rs`, `src/ui/modal.rs`, `src/ui/picker.rs`, `src/ui/layout.rs`, `src/ui/hints.rs`, `README.md`.

### File targets (live, 2026-10-08)

On the hub (FreeBSD 15.1), daemon running the plan 2 build, jobs created through the TUI form (`R`, destination host `file`):

- **Full, increment, up to date:** a 3 MiB scratch dataset wrote `scratch-fsrc@hourly-…A.zfs` (3,201,416 bytes; `zfs send -nvP` estimated 3,198,384, the difference is the stream header) with no `.partial`, then `scratch-fsrc@hourly-…A..hourly-…B.zfs` (1 MiB) when a second snapshot appeared, then `skipped: up to date`.
- **Restore:** `zfs receive scratch/frestore < full`, then `< increment`: both snapshots appeared and `sha256` of both data files matched the source.
- **Snapshot destroyed and recreated under the same name** (new guid): the next run wrote a *full* stream (5.3 MB), recorded the new guid, then `up to date`. An increment from the old base would have been corrupt.
- **Cancel mid-transfer over the real SSH runner** (`fbsd16t1`, 400 MiB, `c` pressed while the progress line showed `sent … of 400.8 MiB`): `Cancelled: cancelled after 74.9 MiB; the next run starts again`, the directory (created by the daemon, mode `drwx------`) had no `.partial`, `replication.json` unchanged; the next run wrote the full 401.0 MiB stream (420,497,736 bytes vs an estimate of 420,219,176).
- **Daemon killed mid-write** (`kill -9` as soon as a `.partial` appeared; the rc supervisor restarted it): after the restart the `.partial` was gone, the unfinished run was closed as lost, and the following run wrote the 150.4 MiB increment.
- Files were mode 0600, the created directory 0700.
- Not tested live: a full disk (covered by `pump_to_file`'s write-error test).
- **Review fixes (2026-10-08):** the `.partial` is created fresh (unlink, then exclusive create) and a group- or world-writable `dest_dir` is refused; `dest_dir` must be canonical; a recorded snapshot whose stream file is gone from the directory counts as unrecorded (full stream); a complete stream with the same name is kept as `<name>.old-N`, never overwritten. Known and documented rather than fixed: the host is not part of the file name, `tank/a-b` and `tank-a/b` flatten alike, symlinked aliases of one directory are not detected, and deleting a job keeps its `replication.json` entry.
