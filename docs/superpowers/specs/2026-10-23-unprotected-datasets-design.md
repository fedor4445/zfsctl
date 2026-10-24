# Unprotected datasets check: design

Status: draft for approval (2026-10-23). Nothing is built.

## Intent

zfsctl's mission is to protect data. It already says when a disk is failing, a pool is full or a job is overdue,
but it never says "this dataset has no safety net". A dataset can sit for months with no snapshots, or with snapshots
only on the pool that holds it, and nothing complains until the pool is gone.

This check judges every dataset on every managed host and says which ones are not protected, in red on the
Dashboard and by email.

## What "protected" means (the operator's words, then the details)

The operator: *a copy on another machine, plus snapshots.* Both must hold. Missing either makes the dataset unprotected.
How much is required is the operator's choice (see "Levels"); the text below describes the default, **Basic**.

1. **Snapshots.** An enabled snapshot job covers the dataset, and the job is current (see "Current").
2. **A copy on another machine.** An enabled job of one of these kinds covers the dataset, and is current:
   - a **replication** job whose destination is another machine (a configured host other than the source host, or a
     file target on the hub when the source host is not the hub),
   - a **restic backup** job whose repository is not on the source host's own disk (a remote scheme such as `sftp:`,
     `s3:`, `rclone:`, `b2:`, `rest:`; a plain local path does not count),
   - a **cloud mirror** (rclone) job whose remote is a real remote (`name:path`; a plain local path does not count).

A dataset has to be *known* to be covered. Anything zfsctl cannot establish is not shown as protected, and it is not
shown as unprotected either (see "Unknown").

### Current

A job counts only while it keeps running. Its latest finished run must have succeeded, and it must be recent: use the
same rule the Dashboard already uses to call a job overdue (it compares the time since the last success with the job's
schedule interval). A job that has never run does not protect anything yet. A job that is disabled does not count.

### Which datasets a job covers

- A job names a dataset on a host. It covers exactly that dataset.
- If the job is **recursive** and really sends or snapshots the whole tree, it also covers the dataset's descendants.
  How each kind treats `recursive` differs (a restic or rclone job reads one dataset's files, so descendants mounted
  inside are separate datasets and are *not* covered by it; a snapshot job with `-r` and a replication with a
  recursive send do cover descendants). The builder must check each kind in the code and cover descendants only
  where it is true. When in doubt, do not cover (a false "unprotected" is cheap, a false "protected" is not).
- A job on a child never covers its parent.

### Datasets that are themselves a copy (my call)

The destination dataset of a zfsctl replication is a copy of data that lives on the source, so it counts as covered by
that fact. Otherwise every replica would be flagged forever. (If the source is lost the replica is the survivor; the
operator can still see it in the list of datasets.)

## Levels (the operator's choice)

People want different amounts of protection, so the requirement is a setting, in a small and fixed set of choices. There
are **no per-dataset levels** (decided; a ZFS property inherited down the tree could add them later). The level applies
to everything judged as *your data*; OS data always uses Basic's copy requirement.

| Level | Copies on other machines (any kind) | Of which online |
|---|---|---|
| **Basic** (default) | at least 1 | 0 |
| **Careful** | at least 3 (the "2 on-premises plus 1 online" rule) | at least 1 |
| **Custom** | 1, 2 or 3, as chosen | 0, 1 or 2, as chosen |

Snapshots are always required. A copy counts only while its job is current.

**How recent** is a second, independent choice, used for snapshot jobs and copy jobs alike: **strict**, **normal**
(default) or **relaxed**, meaning a job may be 1, 2 or 4 of its own schedule intervals behind. The measure stays
relative to each job's own schedule, so a daily job is not judged like an hourly one. (A fixed "no snapshot older than N
hours" limit is deliberately not offered.)

**Email timing** is a third choice: **immediate** (an email when a dataset becomes unprotected and when it recovers,
plus the daily report section; default), **report** (only the daily report section) or **off** (Dashboard only: no
email of any kind about this check).

### What counts as a copy, and as an online copy

Each enabled job that is current and covers the dataset is one copy, identified by where it writes, so two jobs that
write to the same place count once:

- replication to a host: `dest_host` + `dest_dataset`; to a file target on the hub: `dest_dir` (another machine only
  when the source host is not the hub). On-premises.
- restic backup: `repository`. **Online** when it starts with `s3:`, `b2:`, `azure:`, `gs:`, `swift:` or `rclone:`;
  on-premises when it starts with `sftp:` or `rest:`. A plain path is not a copy on another machine.
- cloud mirror: `remote`. Always online (when it is a real remote, see above).

A replica (the destination of a current replication job) is protected by its source and is not asked for more.

### Where the choices live

`config.toml`, a new `[protection]` table: `level` (`basic`, `careful`, `custom`), `copies` and `online` (used by
`custom`), `recent` (`strict`, `normal`, `relaxed`), `email` (`immediate`, `report`, `off`). Every key is optional and the
defaults are Basic, normal, immediate. In the TUI, a **Data protection** group in the **Settings** section edits them
like the other settings; the `copies` and `online` rows are only shown when the level is Custom.

## What is judged

Every filesystem and volume on every configured host (including `local`, the hub), except snapshots. No exclusion
list and no mute list: the operator said flag everything and deal with the odd ones by hand (swap, scratch trees).

### Two tiers

- **Data** (the operator's personal and business data): an unprotected one is **Issue** level (red).
- **OS data**: **Watch** level (amber). Still listed, quieter. This is a ranking aid only, not an exclusion.

How to tell them apart is a rule on the dataset name (my call): OS data is the boot environment tree (`<pool>/ROOT` and
below), `<pool>/tmp`, `<pool>/var` and below, `<pool>/usr/ports`, `<pool>/usr/src`, `<pool>/usr/obj`, and volumes named
`swap` or `dump`. Everything else is data (including `<pool>/usr/home`). Keep the rule in one small function with
tests so it can be adjusted.

### Grace for new datasets

A dataset created less than 24 hours ago is not judged yet (neither Dashboard nor email). This needs the dataset's
`creation` time, which the storage check does not collect yet (see "Where it lives").

### Unknown is not protected and not unprotected

If a host is unreachable, or the storage check has no answer for it, its datasets are not listed as unprotected:
the host already shows its own state, and saying "unprotected" about data zfsctl cannot see would be noise. When the
host's stored dataset list is stale, say nothing about that host's datasets. Use whatever staleness rule the Dashboard
already applies to storage data; if there is none, treat a list older than two days as stale (the builder's call, to
be named in the code).

## What the operator sees

### Dashboard (decided: option A, plain)

One line per unprotected dataset in the existing **Needs attention** list, drawn like every other item: a coloured dot
(red for data, amber for OS data), the subject `host: dataset`, and what is missing in a short phrase:

- `no snapshot job` / `snapshot job has not run yet` / `snapshots stopped 3 days ago` / `snapshot job failing`
- `no copy job` / `copy job has not run yet` / `copy on another machine stopped 3 days ago` / `copy job failing` when none is current;
  `1 of 3 copies on other machines` when some are current but fewer than the level asks; `no online copy` /
  `0 of 2 online copies` when the online requirement is not met

Sorted with the other items by level, data before OS data. Selecting a line goes to the **Jobs** section (the place
where the fix is made), like the existing items. No red background bars, no banners, no logo: the operator prefers a
plain TUI, and chose the plain style on purpose.

The header verdict (`● all quiet`, `● 4 need attention`) counts these lines like any other item.

### Email

Both, on top of the existing alert machinery (the health monitor's tracker with its alert, escalation, reminder and
recovery behaviour, and the daily report):

- **When a dataset becomes unprotected** (after the grace; only when the email timing is `immediate`), one message per host and pass listing every dataset that
  newly became unprotected, so creating a pool with six datasets yields one email, not six. Subject and body in the
  existing style.
- **When it recovers**, a recovery message in the same way the monitor reports other recoveries.
- **The daily report** (when the email timing is `immediate` or `report`) gets a section, "Unprotected data", listing every dataset currently unprotected with how long it
  has been so, data first. When none, one line saying all datasets are protected.

Reminders follow whatever the existing tracker does for other alerts; do not invent a second schedule.

## Where it lives (builder's map; check against the code)

- **Judging** is a pure function over: the jobs, the run history, each host's datasets with their creation times, and
  "now". No remote calls of its own, so it is cheap to run on every Dashboard build and every monitor pass, and easy to
  test with fixed inputs.
- **Datasets and creation times.** `storage_check` already runs one `zfs list` per host
  (`-t filesystem,volume -o name,used,referenced,quota,refquota,type`) but stores only datasets with a quota. Add the
  creation time (`creation`, seconds with `-p`), and store the full list of names, types and creation times
  (compact; thousands of datasets must stay fine). Check the option order on FreeBSD, Linux and OmniOS: illumos has a
  strict argument order, and `zfs list -p -o creation` must be verified on all three before relying on it.
- **Config and Settings.** `config.rs` holds the config structs (`NotifyConfig` and `DashboardConfig` are the pattern:
  optional keys, a `resolve()` that validates and fills defaults); `ui/settings.rs` builds the rows and the validated
  `config_edit::Change`s for the Settings section.
- **Dashboard.** `ui/dashboard.rs` builds `Item`s for Needs attention from host rows; add these items there, with
  `goto: Section::Jobs`.
- **Monitor / report.** `daemon/monitor.rs` and `daemon/health_text.rs` own the tracker, `alert_message` and
  `report_message`.
- **Docs.** The book chapters on alerts and the Dashboard describe Needs attention and the report; update them and
  retake the Dashboard capture.

## Goals

- Never say "protected" without a recent successful snapshot job and a recent successful copy job behind it.
- Say what is missing, in words, per dataset.
- Reach the operator by email even when they are not looking at the screen.
- One new email per host per pass, not one per dataset.

## Non-goals

- Checking that a copy is intact (restore tests, scrubs of the destination, checksum comparison). The operator will
  do that by hand when the check fires.
- An exclude or mute list, per-dataset levels, and automatically creating jobs to fix a gap.
- Counting snapshots taken by hand, holds, or `zfs send` runs outside zfsctl as protection.
- A new column in the Datasets section (possible later; not part of this).

## Anti-goals (a failure even if it technically works)

- A permanent flood: replicas, brand-new datasets, or one email per dataset.
- A dataset shown as protected because a job exists on paper while it has been failing for days.
- Anything that changes data or touches a host: this feature only reads.

## Decisions I made, for the operator to check

1. OS tier by name (the rule above), Watch level.
2. Replication destinations count as covered by their source.
3. A restic repository or rclone remote counts as "another machine" only when it is remote, not a local path.
4. "Current" reuses the Dashboard's overdue rule, not a new threshold.
5. Unknown (unreachable host, stale check data) says nothing about a host's datasets.
6. One summarizing email per host per pass.
7. The Needs-attention line goes to the Jobs section.
8. "Copies on other machines" counts every kind (so Careful is 3 copies, at least one online); the operator's "2 on-premises plus 1 online" is not enforced as two separate on-premises copies.
9. `sftp:` and `rest:` restic repositories count as on-premises, however far away they really are.
10. OS data always uses Basic's copy requirement but the chosen "how recent".
11. Copies are identified by where they write; two jobs to one place are one copy.

## Tests the design asks for

- The judging function over fixed inputs: each kind of job, recursive and not, disabled, never run, failing, stale,
  current; a replica; the grace boundary (23 h and 25 h); the OS-name rule; unknown host; each level (Basic, Careful,
  Custom) with too few copies, enough copies and no online copy; two jobs to one destination counting once; each
  "how recent" step at its boundary.
- The `[protection]` config: defaults, every valid value, and a clear error for a wrong one.
- The Settings rows: shown, edited, saved, and Custom's two rows hidden for the other levels.
- The Dashboard shows the lines in the right order and colours, and the header count includes them.
- One email for several datasets, a recovery email, and the daily-report section (with none, and with several).
- The storage check's new `zfs list` columns on FreeBSD, Linux and illumos output shapes.
- A live check on the hub and on two test boxes before it is called done (a snapshot-only dataset must show as
  unprotected; adding a replication job must clear it).
