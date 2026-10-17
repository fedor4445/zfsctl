# Cloud mirror job (rclone) — Design

Date: 2026-10-17

## Intent

The user wants their files and folders in a cloud bucket (AWS S3 and Backblaze B2 first) as **plain files they can browse and download one at a time**, with no tool needed to read them. Restic over an rclone remote already exists but stores encrypted, deduplicated blocks; this is the plain-files counterpart. Why now: it was floated on 2026-10-10 as "future plan, maybe"; the user picked it as the next FreeBSD feature on 2026-10-17.

## What it does

A new job kind (`kind = "rclone"`; the TUI calls it a cloud mirror job) that, on a cron schedule, on a host:

1. Takes the same source as the restic backup job: a fresh snapshot of the dataset each run (`source = "new"`, with the same `keep`/prefix handling for the job's own snapshots), a named snapshot, or the newest `<prefix>-<timestamp>` snapshot; plus an optional `path` folder inside the dataset. The snapshot is cloned and mounted read-only exactly as the backup job does, and the clone is always destroyed afterwards.
2. Mirrors that tree to `<remote>/current` with `rclone sync`, so the main folder always equals the server.
3. Anything that sync would replace or delete is first **moved**, never discarded, into `<remote>/old/<UTC run time>/` (`--backup-dir`, one folder per run named `YYYYMMDDTHHMMSSZ`, so two runs the same day cannot overwrite each other's older copies).

`<remote>` is an rclone remote path the user configured on that host with `rclone config`, for example `aws:mybucket/photos` or `b2:mybucket/photos`. zfsctl does not know or care which provider it is.

## Decisions made with the user

- Plain, browsable files; no encryption or dedup by zfsctl (an rclone `crypt` remote would work but is then not browsable; the book says so).
- The main folder mirrors the server; replaced or deleted files go to `old/<run time>/`.
- zfsctl never expires or deletes anything in `old/`. The user deletes old versions themselves.
- A per-job optional speed limit field (rclone `--bwlimit`). No time-of-day limits: the schedule covers that.
- A check button (F5) in the form, like the backup form's.
- Starting providers: AWS S3 and Backblaze B2, via rclone remotes. Any other rclone remote may work but is untested.
- Data safety is the user's anti-goal: **losing data in the bucket would be the failure**, even if everything else works.

## Safety rules (the builder must keep every one)

- zfsctl only ever runs `rclone sync` (with `--backup-dir`), `rclone lsf`/`lsd`-style read-only listings and `rclone size`-style checks. It never runs `delete`, `purge`, `deletefile`, `cleanup` or `move` as a command of its own.
- The backup directory is on the same remote as the destination, outside the destination (`old/` and `current/` are siblings under the job's remote path), so a replaced file can never be overwritten by the sync that moves it.
- The run is refused, failing with a clear message and touching nothing, when the source tree is empty or the clone/mount failed or is unreadable. An empty source must never look like "everything was deleted" (even though sync would only move files to `old/`, it would leave the main folder empty and noisy).
- No `--delete-excluded`, no filters that could hide files, no `--no-traverse`-style shortcuts that risk wrong deletes; `--max-delete` is not needed because deletes are moves.
- A remote string must be `name:path` with the name made of letters, digits, `_`, `-`; it must not start with `-` or `:` (rclone connection strings like `:s3,access_key_id=...:` would put credentials in `jobs.toml`) and must contain no `,` before the first `:`. It must not contain credentials of any kind. Credentials live only in `rclone.conf` of the user that runs rclone on the host (root).
- Error text shown in the TUI, history and mail is redacted of the remote's path secrets the way the restic job redacts its repository.
- The job refuses to be saved if two enabled cloud-mirror jobs have overlapping destinations (same remote path, or one inside the other), mirroring the replication/backup clash rules.
- Cancel (`c`) stops the run within seconds, destroys the clone, and is recorded as cancelled, not failed; a half-finished run leaves only files moved to `old/` or copied to `current/`, never a deleted file.

## Form and display

- Fields: Host, Dataset, Source (new/@snapshot/prefix as the backup form), Path inside the dataset, Remote, Speed limit (blank = none; a number with optional K/M/G suffix per second, validated), Keep (for `new` source snapshots, same default as backup), Schedule, Catch up.
- F2 helps where it helps for the backup job (datasets, snapshots, hosts). F5 checks the remote: it lists the remote path read-only (`rclone lsf` with a short depth limit and a deadline), reports "remote not found" / "bucket not found" / "access denied" / "ok" in plain words, and adds a Note about `rclone.conf` (location, mode and owner, like the password-file note) and a warning if the path looks like it overlaps `old/`.
- A save that newly arms something destructive asks for confirmation like the other job kinds: this job moves files within the bucket and replaces them; say so and name the remote.
- Job panel: progress (bytes done/total, rate, ETA) like replication and backup; last-run message summarises counts (files copied, replaced and moved to `old/<time>`, deleted from main and moved).
- `jobs.toml` keys for this kind: reuse `dataset`, `source`, `snapshot`, `prefix`, `keep`, `path`, `schedule`, `catch_up`; new keys `remote` and `bwlimit`. Old files load unchanged. The config-key reference in the book and its test (`documented_config_keys`) are updated.

## Privilege and installation

- `rclone` becomes a new opt-in sudoers feature (`Feature::Rclone`), listed by `print-sudoers`/`check-host` (`--with rclone`), exactly as `restic` is; not in the default set. It runs as root through the usual sudo path with the daemon's HOME fix already merged.
- rclone is a user-installed package (`pkg install rclone`), not shipped by zfsctl; the book explains this.

## Documentation

- Book: a section in the backups chapter (or a new chapter, builder's call) with the job form, the old/ layout, how to restore (browse the bucket, or `rclone copy` by hand), the safety table, and a **cloud account guide**: create an AWS IAM user whose policy allows only list/get/put/delete on one bucket, or a Backblaze application key restricted to one bucket with list/read/write/delete; note that the key needs delete rights because moving a file is a copy followed by a delete; optional tip: turn on bucket versioning as a second safety net; how to run `rclone config` as root on the host. README, `--help` text and the commands/keys references updated; the book-check tests stay green.

## Testing

- Unit tests with the fake runner: argument building for sync with `--backup-dir` and `--bwlimit`; remote and speed-limit validation (including connection-string and credential-bearing remotes); empty-source and failed-clone refusal; clone always destroyed; cancel; redaction; overlap rule; old `jobs.toml` loads.
- Live on the hub with rclone installed there and a **local-directory remote** (no cloud): first run copies, second run after changing/deleting/adding files moves the old versions into `old/<time>/` and keeps nothing lost, an empty source is refused, cancel works, F5 reports each outcome; the TUI driven read-only except the form being tested.
- **Real S3 and B2 cannot be tested until the user has an account.** The final report must say so; one live pass against each cloud is a follow-up the user does when they have keys.

## Out of scope

- Expiring or pruning `old/` (user does it by hand); restore from the TUI; encryption by zfsctl; cloud-side lifecycle rules; providers other than the rclone remote the user names; bandwidth schedules by time of day; mirroring several datasets in one job; two-way sync.

## Left to the builder

- Exact rclone flags beyond those named (stats format, transfers/checkers counts, retries), how progress is parsed, the speed-limit grammar details, and whether the book section is a new chapter.
