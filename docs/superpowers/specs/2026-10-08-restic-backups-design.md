# Restic Backups — Design

Date: 2026-10-08

## Goal

A fourth scheduled job kind, **backup**: back up one ZFS dataset's files with `restic` to a repository (external drive, sftp, rclone remotes), reading from a consistent ZFS snapshot, on a cron schedule, with retention, progress and cancel. Managed from the Jobs section like the other kinds. It complements replication (which moves ZFS streams): restic gives file-level, deduplicated, encrypted, restorable-by-file backups to places that are not ZFS.

## Scope

- Runs on the job's host through that host's `CommandRunner` (`local` = the hub; remote hosts over SSH + `sudo -n`).
- One dataset per job; child datasets are not included (make one job per dataset).
- Credentials: a password file on the job's host (`--password-file`), repository via `-r`. zfsctl stores and transmits no secret. Cloud keys go in an rclone remote's own config (`-r rclone:remote:path`); direct S3/Azure/B2 environment variables are not supported (an environment cannot pass through `sudo -n` without allowing `env`, which would make the sudoers entry root-equivalent).

Non-goals (YAGNI): restoring from the UI, browsing a repository, `restic init`, `check`, `unlock`, repository migration, backing up zvols, recursive child datasets, direct S3 keys, a separate prune schedule, creating or destroying snapshots.

## Decisions made while brainstorming

1. **Where restic runs:** on the job's host (not hub-only, not pulled over sshfs).
2. **Credentials:** password file on the host; no environment variables, no secret store.
3. **Retention:** a policy in the job; after a successful backup the run forgets and prunes this job's own backups.
4. **Stable path:** restic matches a file against its parent backup by path, so a source path that changes per run (`.zfs/snapshot/<name>`) makes restic re-read and re-hash every file every run (verified with restic 0.19.1: even with `--parent` and ignoring inode/ctime, a different root path gives "200 new, 0 unmodified"). Each run therefore clones the snapshot to a fixed read-only mountpoint and backs that up. A nullfs mount (needs `mount`/`umount` in sudoers) and reading `.zfs/snapshot` directly (full re-read every run) were rejected.

## The job

`KIND_BACKUP = "backup"`. New optional flat fields on `Job`, skipped on serialize when unset so existing files are unchanged (`dataset`, `prefix`, `schedule`, `catch_up`, `host` are reused):

| field | meaning |
|---|---|
| `repository` | restic's `-r` value. Required. No credentials inside it. |
| `password_file` | absolute path on the job's host. Required. |
| `retention` | optional retention text (the TOML name: the snapshot job's numeric `keep` already uses that word; the form label stays "Keep"): tokens `<n>l` (last), `<n>d`, `<n>w`, `<n>m`, `<n>y`, e.g. `7d 4w 12m 3y`. Blank = nothing is ever forgotten. |

`resolve` rules for a backup job: `dataset` valid, `prefix` valid, `host` known, `repository` non-empty with no NUL/newline and not starting with `-`, `password_file` an absolute path with no `..`/NUL/newline, `retention` parses (each count 1..=9999, no duplicate unit), schedule valid. Two enabled backup jobs may not share a clone name (same host and job id cannot happen; same job id is unique by construction), but several jobs may back up the same dataset to different repositories.

## A run

Under a lock on `(host, "#bak@<job id>")`:

1. **Leftovers.** If the clone dataset `<pool>/zit-restic-<job id>` exists (a crashed run), `zfs destroy` it first. (Self-healing before each run; there is no startup scan because the hosts may be remote.)
2. **Select.** List the dataset's snapshots with `name,guid,createtxg,creation` and take the newest `<prefix>-<timestamp>` (`is_own`). None: `Skipped: no snapshots with prefix`.
3. **Already done?** `restic snapshots -r <repo> --password-file <pf> --tag zfsctl:<job id> --tag zfsctl-snapshot:<name> --json` (both tags must match: one comma-separated `--tag=a,b` is AND; repeated `--tag` flags would be OR) returns a snapshot: `Skipped: up to date (<name>)`. Failure here is a failed run with restic's message (this is also where an uninitialised repository or wrong password shows up).
4. **Clone.** `zfs clone -o mountpoint=/var/run/zfsctl/<job id> -o readonly=on -o com.sun:auto-snapshot=false <dataset>@<snap> <pool>/zit-restic-<job id>` (`canmount` defaults to on). Failure (for example a locked encrypted dataset) is a failed run with zfs's message.
5. **Backup.** `restic backup -r <repo> --password-file <pf> --host <job host> --tag zfsctl:<job id> --tag zfsctl-snapshot:<name> --time "<snapshot creation, local time of the host>" --json /var/run/zfsctl/<job id>`. The run reads restic's JSON lines: `status` messages feed the progress file (`bytes_done`, `total_bytes`), the final `summary` message gives the counts. `--time` makes restic's history, and so daily/weekly retention, follow the age of the data, not when the run happened.
6. **Always clean up.** `zfs destroy -r` the clone (a pool-wide recursive snapshot may have given it snapshots of its own; the name is zfsctl's), retried up to five times two seconds apart (after a cancel over SSH the remote restic may take a moment to let go) whether the backup succeeded, failed or was cancelled. A failed destroy is reported in the run message (the next run retries it).
7. **Forget.** (Every restic option is passed as `--flag=value`, so a value can never be read as an option, and the keep flags are `--keep-last=N` and so on.) Only after a successful backup and only if `retention` is set: `restic forget -r <repo> --password-file <pf> --prune --tag zfsctl:<job id> --host <job host> --keep-last/--keep-daily/--keep-weekly/--keep-monthly/--keep-yearly …`. A forget failure makes the run `Failed: backed up <snap> but forget failed: <message>` (the backup exists).
8. **Result.** `Ok: backed up <snap> (<new> new, <changed> changed, <unmodified> unmodified), <bytes> added`; the repository is never part of a message.

The run never creates, destroys or modifies a snapshot of the source.

### Progress and cancel

The existing machinery: restic is spawned through `runner.command("restic", …)`, its stdout is read line by line on the hub, and a progress file is written once a second. `c` kills the restic process (over SSH, the `ssh` client; the remote restic ends when its output pipe closes). A cancelled run records `Cancelled`, destroys the clone, and the next run starts again (restic itself reuses what was already uploaded). A killed restic leaves a stale repository lock (seen live): a backup tolerates it but `forget --prune` fails with `repository is already locked`. When forget fails with that message the run runs `restic unlock` (stale locks only, never `--remove-all`) once and retries the forget once; any other forget failure is not retried.

### Snapshot jobs and the clone

While a clone exists its origin snapshot cannot be destroyed. A snapshot job's prune that reaches that snapshot gets zfs's "has dependent clones" error. A prune that hits a snapshot with a dependent clone must treat it as in use: leave it, say so in the run message, and not fail the run; the next slot prunes it. (Requires a change in the snapshot job's prune; decided in the plan.)

### Restart

An unfinished backup run is closed as lost (as for the other kinds); the leftover clone is destroyed by the next run's step 1.

## Privileges

- `local`: none.
- Remote host: restic installed and one sudoers entry for `/usr/local/bin/restic` (a path that can vary; documented with the other binaries). It is as broad as `zfs` or `bectl`: restic running as root reads any file and writes anywhere, so the README says plainly that this grants root-equivalent access to whoever holds the SSH key. The clone uses `zfs`, already allowed.

## UI

- `B` in Jobs opens the backup form: Host, Dataset, Prefix, Repository, Password file, Keep, Schedule, Catch up. F2: Host (configured hosts), Dataset (live list for the chosen host), Schedule (presets). `Enter` edits; `Space`/`d` as for other jobs. `c` cancels a running backup (like replication).
- The Target column shows the source dataset; the details never show the repository's credentials (there are none by contract).
- In-app help and hints list `B`.
- README: a "Backups with restic" subsection under Scheduled jobs: a sample job, the password file (`chmod 600`, root), `restic init` once, the repository-string caveat (no `user:pass@`), the sudoers line, retention syntax, the clone, `restic unlock`, restoring with `restic restore` (the backed-up path is `/var/run/zfsctl/<job id>`), and that nothing is deleted without a `keep` policy.

## Edge cases

- **Repository not initialised / wrong password / unreachable:** step 3 fails the run with restic's message. No clone is made.
- **No snapshots, or the newest is already backed up:** skipped, nothing created.
- **Encrypted dataset with its key unloaded, or a clone that cannot be made:** failed with zfs's message.
- **A leftover clone from a crash:** destroyed by the next run; until then it pins its snapshot.
- **Two schedule slots overlapping:** the lock makes the later one `Skipped: already running`.
- **Time zones:** `--time` is passed in the host's local time; the host is where restic parses it.
- **Dataset renamed or destroyed between runs:** the job fails at selection; restic's history is untouched.
- **Large first backup:** it simply takes long; the progress file shows it; cancel and resume work because restic reuses uploaded data.
- **`retention` with a very short policy on a repository shared with other tools:** only snapshots with this job's tag and host are forgotten.

## Testing

- `jobs`: round trip, `resolve` for each bad field, `retention` parser (valid, duplicate units, zero, junk), other kinds unchanged.
- The run with scripted `zfs` and `restic` doubles: exact argument lists for each step in order; skip when no snapshot / already backed up; failure at each step; clone destroyed after success, failure, cancel; leftover clone destroyed first; forget only after success and only with a policy; no repository in any message; the JSON status/summary parsing.
- Progress file written and removed; cancel kills restic and records `Cancelled`.
- Snapshot-job prune leaves a snapshot with a dependent clone and succeeds.
- TUI: the form opens, validates with `resolve`'s rules, saves and edits; F2 on each field; hints and help.
- **Live (verify empirically):** `pkg install restic` on the hub, then on `fbsd16t1` for the remote path (with the sudoers entry). Confirm the exact FreeBSD restic version's `--json` output, `--time` format, and that a second run with unchanged data reports `0 new, 0 changed, N unmodified` and does not re-read (the claim that clone inodes and ctimes are stable); full backup, incremental, restore with matching checksums, cancel mid-backup, `kill -9` mid-run and the leftover clone cleanup, `forget` with a small policy, an uninitialised repository, a wrong password, and a repository on an external-drive-like local path.

## Delivery

One implementation plan (the pieces share one job kind and are small): the job model and `retention` parser, the run (with a restic double), snapshot-prune handling of a dependent clone, daemon wiring, the form, docs and the live checks.

## Files touched (indicative)

`src/jobs.rs`, `src/daemon/backup.rs` (new), `src/daemon/mod.rs`, `src/daemon/snapshot.rs`, `src/zfs/snapshot.rs`, `src/ui/jobs.rs`, `src/ui/state.rs`, `src/ui/modal.rs`, `src/ui/picker.rs`, `src/ui/layout.rs`, `src/ui/hints.rs`, `README.md`.

## Live results (2026-10-08)

On the hub (FreeBSD 15.1, restic 0.19.1 from `pkg`, the same version as on the workstation), jobs created through the TUI's `B` form, repository and data on the `scratch` pool and `/var/tmp`:

- **Real formats:** `zfs get -H -p -o value creation <snap>` gives an epoch, `date -r <epoch> "+%Y-%m-%d %H:%M:%S"` the host's local time; `restic snapshots --json` prints `[]` for no match. The restic snapshot carried both tags, the stable path `/var/run/zfsctl/<job id>` and the snapshot's *creation* time (05:58:50), not the run time.
- **The stable-path claim, on real ZFS:** a clone and its snapshot directory report the same inode and ctime (`ino=2`, equal `ctime`). A first backup said `6 new, 0 changed, 0 unmodified, 2.9 MiB added`; after adding one file and a second snapshot, `1 new, 0 changed, 6 unmodified, 504 KiB added`. Unchanged files were skipped.
- **Skip, restore, retention:** the next slot with nothing new was `skipped: up to date`; `restic restore latest` matched the source checksums (7 files); `retention = "1l"` left exactly one backup.
- **Snapshot job vs a pinned snapshot:** with a hand-made clone of the old snapshot, the snapshot job's run was `Ok: … pruned 0; kept 1 in use by a clone`; after the clone was destroyed the next run `pruned 2`.
- **Errors** (each `Failed`, no clone or mount left behind): wrong password, a repository that was never initialised, a dataset that does not exist. These showed raw JSON at first (restic prints `exit_error` on stderr); fixed.
- **Cancel** (700 MiB, `c` pressed while the progress line read `sent 8.2 MiB of 700.0 MiB (1%)`): `Cancelled: cancelled after 138.3 MiB`, clone and mount gone, no half-written snapshot.
- **`kill -9` of the daemon mid-backup:** the clone and the orphaned restic child survived; the supervisor restarted the daemon after about 10 s, the unfinished run was closed as lost, and the next run destroyed the leftover clone itself and backed up (700.0 MiB).
- **Defects found live and fixed:** (1) the daemon, started by rc with a minimal `PATH`, could not find `restic` in `/usr/local/bin`: `LocalRunner` now appends `/usr/local/sbin:/usr/local/bin` to the `PATH` of every child; (2) restic's JSON errors on stderr were shown raw; (3) a cancelled or killed run left a stale lock that made every later `forget --prune` fail (see "Progress and cancel"). After the fix: 2 stale locks, a run with retention was `Ok` and left 0 locks.
- Not tested live: a remote host (needs restic and a sudoers entry on a VM, which needs root there), rclone and sftp repositories.

### Review fixes (2026-10-08)

- `--host` is the machine's real hostname (`hostname`, run unprivileged; the job host's label is the fallback), and the "already backed up?" query is limited to it, so two hubs that share one repository neither skip nor forget each other's backups.
- Every restic call that touches the repository carries `--retry-lock=10m` so jobs sharing a repository take turns (restic 0.16 or newer).
- Restic's error text has the repository replaced by `<repository>`; per-file errors (exit 3) are summarised as "N files could not be read, first: …"; every failure message is capped at 400 characters.
- Known, deferred: a leftover clone is only found under the job's current pool and id (editing the dataset to another pool, or deleting the job, after a crash leaves it); editing a job's host keeps its id; `c` during forget is accepted but not acted on; a hand-edited job id of only dots would be accepted.
