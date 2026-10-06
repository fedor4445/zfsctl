# Scheduler Daemon and Scrub Scheduling Design

## Summary

A `zfsctl daemon` subcommand runs scheduled jobs on the hub, unattended, and a new **Jobs** section in the TUI manages them. The first job kind is **scrub**: run `zpool scrub` on a pool of any managed host on a cron schedule, watch it to completion, record the outcome, and run an optional notify command on failure. The scheduler, job store, history and notify hook are built generically so the later sub-projects (snapshot schedules and replication, restic backups) add job kinds without changing them.

## Context: the four planned sub-projects

1. Shell-out terminal. **Done** (merged).
2. **Scheduler daemon plus scrub scheduling** (this spec).
3. Snapshot schedules and ZFS replication, with retention, as new job kinds. Replication runs `zfs send` and `zfs receive` as two SSH processes piped together on the hub, so no `sh` entry is needed in any host's sudoers.
4. File-level backups with restic to local paths, S3, sftp and similar, reading from a ZFS snapshot.

Decisions made while brainstorming this sub-project:

- The daemon runs **on the hub only**, as root, and reaches other hosts over the existing SSH + sudo path. It shares a **state directory** with the TUI; there is no socket.
- Schedules are **cron expressions**, with a preset picker in the TUI.
- A scheduled run missed because the daemon was down is **caught up once** on startup, controlled by a per-job toggle that defaults to on.
- Failures are surfaced in the TUI and through an optional **notify command**; there is no built-in email or push support.

## Goals

- A scrub schedule per pool, on any managed host, that keeps firing with nobody logged in.
- A hub that was off over a scheduled time runs the missed job shortly after it boots.
- An operator can see at a glance whether the daemon is alive, what ran, what failed and what runs next.
- A failed job or a scrub that finds errors can alert the operator through a channel of their choice.
- No new sudoers entries on any managed host.

## Non-goals

- Job kinds other than scrub (sub-projects 3 and 4 add them). The job format and scheduler leave room for them.
- Automatic retry of a failed run.
- A daemon on any host other than the hub, and a socket between the TUI and the daemon.
- Built-in email, SMTP or HTTP notifications.
- A "run now" key. The Pools section already has `s` to scrub immediately.
- Pausing or cancelling a running scrub from the daemon.

## Current state (confirmed by reading the code)

- `src/main.rs` checks for root, loads `/usr/local/etc/zfsctl/config.toml` and starts the TUI. It has no subcommands.
- `zfs::pool::scrub_pool(runner, pool)` runs `zpool scrub -- <pool>` and is already verified against a real `zpool`.
- `zfs::pool::ScanStatus` is `InProgress { summary, percent_done, eta }` or `Completed { summary, errors }`, parsed from `zpool status`. The completed summary contains the completion timestamp.
- `HostInventory` builds a `Host` (name, runner, `ssh_target`) per configured host; the same runners are what the daemon uses.
- Config is TOML (`toml` crate); `Config::save` writes it. The hub has cron enabled and an `rc.d` directory at `/usr/local/etc/rc.d`.

## Design

### 1. Process and files

- **Command line:** `zfsctl` with no arguments is the TUI, as today. `zfsctl daemon` runs the scheduler in the foreground. The root check applies to both.
- **Backgrounding:** zfsctl contains no fork or daemonizing code. A new `contrib/rc.d/zfsctl` script starts it under FreeBSD's `daemon(8)`, which backgrounds it, writes the pidfile and restarts it if it crashes.
- **State directory** `/var/db/zfsctl/`, mode `0700`, created by the daemon and by the TUI on first write:
  - `jobs.toml`: job definitions. Written only by the TUI, atomically (write a temp file in the same directory, then rename).
  - `history.jsonl`: append-only, one JSON object per line, one line per run event. The daemon is the only writer. A reader skips any line that does not parse, so a write torn by a power cut loses at most one record.
  - `daemon.json`: a heartbeat the daemon rewrites every tick (atomically): pid, start time, last tick time.
- **Reload:** the daemon compares `jobs.toml`'s modification time once a second and reloads when it changes. The TUI never signals the daemon and does not need its pid.
- **Daemon health in the TUI:** the TUI reads `daemon.json`. A last tick older than 10 seconds, or a missing file, shows "daemon not running: jobs will not fire".
- **Config:** `config.toml` gains an optional `[daemon]` table with `notify_command` (string).

### 2. Jobs

`jobs.toml` is a list of tables:

```toml
[[jobs]]
id = "scrub-tank-monthly"
host = "local"
kind = "scrub"
pool = "tank"
schedule = "0 3 1 * *"
catch_up = true
enabled = true
created_at = "2026-10-06T12:00:00Z"
```

- `id` is unique and chosen by the TUI. `host` is a host name from `config.toml` (`local` for the hub). `kind` selects the behavior and which other keys apply (`pool` for scrub).
- A job whose `kind` the daemon does not know, or whose host is not in the config, is **never run**; the Jobs list shows it as unsupported with the reason. This keeps a newer `jobs.toml` safe under an older daemon.
- `created_at` is optional. A catch-up never reaches back before it, so a new job does not fire for a slot that passed before it existed. A job with neither `created_at` nor history uses the time the daemon first loaded it as its baseline.
- `jobs.toml` can name only job kinds the daemon implements; it can never contain a command to execute. The only configured command anywhere is `notify_command`.

### 3. Schedule parsing

- A small parser of our own, in its own module: five fields (minute, hour, day of month, month, day of week), each accepting `*`, a number, a comma list, a range `a-b` and a step `*/n` or `a-b/n`. Day of week is `0`-`6` with `0` Sunday. Names and `@` macros are not supported; the TUI's presets generate the numeric form.
- When both day-of-month and day-of-week are restricted, a time matches if **either** matches (standard cron behavior).
- Times are interpreted in the **hub's local time zone** (the `chrono` crate). A local time that does not exist (the spring DST jump) is skipped; one that occurs twice (autumn) fires once.
- The parser exposes `next_after(expr, instant)` and `prev_before(expr, instant)`; an invalid expression is an error the form shows, never a panic.

### 4. Scheduler

- A tick runs once per second, in the daemon's main thread.
- **Startup catch-up:** for each enabled job, take its most recent scheduled time at or before now. If that time is later than the job's last recorded run and `catch_up` is on, run the job once now, marked `catch_up` in the history. The count of missed runs does not matter: at most one catch-up run.
- **Normal firing:** otherwise a job fires when now reaches its next scheduled time.
- **Overlap:** each run executes on its own thread. A lock per `(host, pool)` stops two jobs touching one pool at once. A run that finds the lock held, or a pool already scanning, is recorded as `skipped` with the reason, never queued.
- **Reload:** when `jobs.toml` changes, the job set is replaced; a run already in flight finishes under its old definition.

### 5. Scrub job

1. **Pre-check:** read `zpool status <pool>`. If a scrub or resilver is in progress, record `skipped: already scanning` and stop. Remember the pre-start scan summary.
2. **Start:** `scrub_pool(runner, pool)` (that is, `zpool scrub -- <pool>`). A "currently scrubbing" error from a race is also a `skipped`, not a failure. Any other error is a `failed` run.
3. **Watch:** a scrub is asynchronous. Record `started`, then poll `zpool status` about every 30 seconds.
   - Still `InProgress`: keep watching.
   - A `Completed` status whose summary **differs from the pre-start summary**: record `finished`. The completion line contains a timestamp, so it differs from the previous scrub's even when the new scrub finished before the first poll.
   - `Completed` with the **same** summary as before the start for three polls in a row: record `failed: scrub did not start`.
4. **Result:** `ok` when the completed scan reports no errors; `failed` when it does, with the raw summary as the message. An unreachable host, a vanished pool or a command failure during watching is `failed` with the error text.
5. **Daemon restart mid-scrub:** at startup, any run with `started` and no `finished` is re-attached: the watcher resumes from the current `zpool status`, using the summary recorded at the start. A run whose pool has vanished, or that cannot be resolved after the host stays unreachable for 10 minutes, is recorded as `failed: lost`.

### 6. History

- One JSON object per line: `job_id`, `run_id`, `event` (`started`, `finished`, `skipped`), `at` (UTC, RFC 3339), `scheduled_for`, `result` (`ok`, `failed`, `skipped`), `message`, `catch_up`.
- The daemon prunes to the most recent 500 runs per job, atomically, on startup and once a day.
- Readers (the TUI) reconstruct a run by `run_id`; a `started` with no matching `finished` is a running run.

### 7. Notify hook

- When a run ends `failed`, the daemon runs `[daemon] notify_command` through `sh -c` with these environment variables: `ZFSCTL_JOB`, `ZFSCTL_HOST`, `ZFSCTL_KIND`, `ZFSCTL_RESULT`, `ZFSCTL_MESSAGE`.
- The command has a 30-second timeout. It is killed at the timeout, and its output and exit status go to the daemon log. A failing hook never fails the job or stops the scheduler.
- It is a local, operator-configured command on the hub, the same trust as everything else in `config.toml`. No job data is ever interpolated into the command line; it travels only in the environment.

### 8. TUI: the Jobs section (plan 2)

*Amended after plan 1 shipped, once the existing form, picker and mutation code had been read. Three things differ from the first draft: a job is created from the Pools section instead of through a live pool picker; the Schedule field is one text field with an `F2` preset list instead of a preset picker plus a separate custom field; and the bottom panel is drawn directly from the loaded data instead of through the metrics panel's sampler.*

**Section and data.**

- A new **Jobs** section after Shares. Unlike the other sections it is global: it lists the jobs of every host, because `jobs.toml` is one file for the whole hub.
- `App` gains a `JobsView` holding the jobs from `jobs.toml`, the runs reconstructed from `history.jsonl`, the heartbeat from `daemon.json`, and for each job the reason it cannot run, if any (from `jobs::resolve` against the configured hosts). `App` also gains a `jobs_table` selection and a `state_dir` (default `/var/db/zfsctl`; tests use a temp directory).
- These are small local files, so `App::refresh_jobs()` reads them synchronously, with no runner and no worker thread. It refreshes every 2 seconds while the Jobs section is on screen and every 10 seconds otherwise, so the failure indicator below is right from any section. `r` refreshes at once.
- If `jobs.toml` cannot be read, the section keeps the data it has, shows the error on its first line, and refuses to write (a write would overwrite a file the operator may be mid-way through fixing).

**Writes.** Three new `MutationCall` variants, `SaveJob`, `DeleteJob` and `SetJobEnabled`, reuse the existing confirm modal, status message and post-mutation refresh. They ignore the runner. Each does a read-modify-write of `jobs.toml` through the atomic `jobs::save`, so edits made to the file by hand are not clobbered. The TUI is the only writer of `jobs.toml`. `DeleteJob` is a warning-tone confirmation; the other two are neutral. A deleted job's past runs stay in the history until pruned.

**Job form.** One new `FormKind::Job` with the fields `[host, pool, schedule, catch up]`.

- **Host:** free text with an `F2` picker listing the configured host names. The names come from config, not a static list, so the picker is built from data the form carries.
- **Pool:** free text. There is no live pool picker; a mistyped pool gives a run that fails with the real `zpool` error, visible in the history.
- **Schedule:** one text field. `F2` lists presets as cron text with a label (`0 * * * *  (Hourly)`, `0 2 * * *  (Daily, 02:00)`, `0 3 * * 0  (Weekly, Sunday 03:00)`, `0 3 1 * *  (Monthly, 1st 03:00)`); selecting one inserts only the cron part. A custom expression is typed over it.
- **Catch up:** `y` or `n`, default `y`.
- **Live preview:** below the fields the form shows what the schedule means (the preset's name, or "custom schedule") and the next three run times in the hub's local time, or the parse error. It is computed from the field values on every frame.
- **Submit:** a schedule that does not parse, a host that is not configured, or an empty pool keeps the form open and shows the reason.
- **Identity on edit:** an edit keeps the job's `id`, `created_at` and `enabled`. A new job gets `created_at` set to now and the id `scrub-<host>-<pool>`, with a numeric suffix if that id exists.

**Keys.**

- In **Pools** (middle pane): `J` opens the job form for the selected pool, with host and pool filled in, schedule `0 3 * * 0`, catch up `y`.
- In **Jobs**: `n` new (host defaults to the selected host), `Enter` edit, `d` delete (confirm), `Space` enable or disable. Edit has one key, not an `e` alias too: the hint-drift guard requires every working key to be advertised, and one advertised key per action.
- The hint bar and Help text list these, and the existing hint-drift tests guard them.

**Display.**

- Columns: job (`id`), host, pool, schedule (the preset's name or the cron text), next run, last result with its age (`ok 2h ago`, `failed 1d ago`, `skipped`, `running`, or `never`).
- A disabled job is dimmed. A job that cannot run (unknown kind, host not in `config.toml`, bad schedule) is red with the reason in place of the next run.
- The section title shows the daemon's state from the heartbeat: `daemon running (pid N)`, or, in the error style, `daemon NOT running: jobs will not fire`. Changes are saved either way.
- The status line shows `N jobs failed — see Jobs` (`1 job failed` for one) while the most recent run of any enabled job failed, from any section.
- The bottom half of the center column holds a Jobs panel, drawn directly from the loaded data (it does not use the metrics panel's sampler): on the left the selected job's last 8 runs (time, result, duration, message); on the right its schedule in words and as cron, its next three run times, and its flags (`enabled`, `catch up`, and any reason it cannot run).
- Times are the hub's local time. A schedule with no future time (`0 0 31 2 *`) shows `never`.

### 9. Security

- The daemon runs as root and runs only job kinds compiled into it, with arguments taken from typed fields (`pool`), never from a free-form command. A tampered `jobs.toml` can at worst start scrubs on pools of configured hosts.
- The state directory is `0700` root; `jobs.toml` and `history.jsonl` are `0600`.
- The pool name is passed as a separate argument after `--`, as `scrub_pool` already does.
- No change to any host's sudoers: scrubs use `zpool`, which every managed host already allows.

## Edge cases

- **Clock changes:** a wall-clock jump backwards must not fire a job twice. The scheduler fires a job at most once per scheduled time, remembered by `scheduled_for`.
- **Two daemons:** if a second `zfsctl daemon` finds a live heartbeat, it exits with an error rather than double-firing every job.
- **Malformed `jobs.toml`:** a parse error keeps the previous job set running and is shown in the TUI and the log; it never empties the schedule.
- **Disabled job:** never fires and never catches up.
- **Host removed from config:** its jobs show as unsupported and do not run.
- **Two TUIs writing at once:** the last read-modify-write wins; each change re-reads the file first, so it never overwrites an unrelated edit made a moment earlier.
- **Daemon not running:** the Jobs section says so in its title; saves still work and take effect when the daemon starts.
- **Job with no history:** `never` in the last-result column.

## Testing

Unit tests, with an injected clock and the existing `FakeRunner`:

- Cron parser: every field form, the day-of-month/day-of-week rule, invalid input, the step and range edges, and DST (a nonexistent and an ambiguous local time).
- Scheduler: catch-up fires exactly once however many runs were missed, a disabled job does not catch up, overlap is recorded as `skipped`, and a backwards clock jump does not double-fire.
- Scrub watcher: in progress then completed; completed with errors; completed before the first poll (summary differs from the pre-start one); summary unchanged for three polls (`did not start`); a restart mid-scrub re-attaches; an unreachable host.
- History: a torn last line is skipped, pruning keeps the newest 500, and run reconstruction.
- Atomic write of `jobs.toml`; a malformed file leaves the previous job set in place.
- Notify: environment variables are set, the timeout kills the command, a failing hook does not fail the job, and job data never appears in the command line.

Live on the hub, with the pexpect + pyte setup and a one-minute schedule on the `scratch` pool: the daemon starts and writes a heartbeat; a scrub fires on schedule and its result shows in the TUI; stopping the daemon across a scheduled time and restarting it triggers one catch-up; a deliberately failing `notify_command` runs without harming the job.

The Jobs section (plan 2) adds:

- Pure helpers: the schedule description and the next-three-times preview (including an impossible date and the parse error), the preset list and label stripping, and id generation with a collision suffix.
- `App` with a temp state directory: the section loads jobs, runs and the heartbeat; a stale or missing heartbeat reads as "not running"; an unreadable `jobs.toml` keeps the old data and refuses to write; the failed-job count counts only enabled jobs whose latest run failed.
- The three mutations: `SaveJob` creates and edits (an edit keeps `id`, `created_at` and `enabled`), `DeleteJob` removes only its job, `SetJobEnabled` flips only the flag, and each leaves hand-made edits to other jobs intact.
- Keys and hints: `J` in Pools pre-fills host and pool; `n`, `e`, `Enter`, `d` and `Space` in Jobs; a bad schedule, an unknown host or an empty pool keeps the form open with the reason; the existing hint-drift tests pass with the new keys.
- Rendering (`TestBackend`): the table columns, a disabled and an unsupported row, the daemon title in both states, the `N jobs failed` indicator, the form's preview line, and the Jobs panel.

Live on the hub with the daemon running: create a one-minute job on the `scratch` pool from the Pools section with `J`, watch its last result appear in the Jobs section, disable it, delete it, and confirm `jobs.toml` is left clean.

## Delivery: two implementation plans

This spec is delivered as two plans, each producing working, testable software on its own:

1. **Daemon core:** the `daemon` subcommand, the cron parser, the job store, the scheduler (catch-up, overlap), the scrub job and watcher, history, the notify hook and the `rc.d` script. It is usable with a hand-edited `jobs.toml`.
2. **TUI Jobs section** (section 8): the section, its form with the `F2` preset and host pickers and the live preview, the `J` key in Pools, the status indicators and the Jobs panel. It depends on plan 1's job store and history reader. Plan 1 is merged.

## Dependencies

Two new crates: `chrono` (local-time schedule evaluation) and `serde_json` (history lines).

## Files touched (indicative)

- Plan 1 (merged): `src/daemon/` (cron, scheduler, scrub, history, notify, engine, heartbeat, runtime), `src/jobs.rs`, `contrib/rc.d/zfsctl`, `src/main.rs`, `src/config.rs`, `Cargo.toml`, `README.md`.
- Plan 2: new `src/ui/jobs.rs` (the `JobsView`, loading, schedule description and preview helpers, the preset list); modified `src/ui/state.rs` (section, selection, keys, form submit), `src/ui/modal.rs` (`FormKind::Job`, the three `MutationCall`s), `src/ui/picker.rs` (host and preset pickers), `src/ui/layout.rs` (the table, panel, form preview, status indicator), `src/ui/hints.rs`, `src/ui/mod.rs` (periodic refresh), `README.md`.
