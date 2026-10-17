# Disk health on the Dashboard (SMART, and a resilver in progress): design

Date: 2026-10-17. Status: draft for approval.

## Why

A scrub only notices damage it happens to read; a dying disk usually says so earlier through SMART. The user wants
disk health on the Dashboard, with these limits (their words, paraphrased): no fill predictions; use
`smartmontools`; there may be many hosts, so show only hosts that have a problem, one entry per host; do not run
`smartctl` often: once a day, by the daemon.

## What

1. **The daemon checks the disks once a day.** At a fixed local time (default 03:30) it runs `smartctl` on every
   configured host, the hub included, through the same runners the health monitor uses (ssh to the unprivileged
   account, sudo for the one new command). It writes the outcome to `<state_dir>/smart.json`. A check also runs
   when the daemon starts if the file is missing or older than 24 hours.
2. **The Dashboard reads the file** (like it reads the jobs and the heartbeat); the TUI never runs `smartctl`.
   Needs attention gets one line per host that has a problem, none for healthy hosts:
   `fbsd16   da1 reallocated sectors 8; ada0 SSD wear 87%   (checked 6 h ago)`.
   Several disks of one host share the line. A host's line is red (issue) if any disk's own overall verdict is
   FAILED, otherwise yellow (to watch). `Enter` on the line goes to the Disks section of that host.
3. **The daily report** (email/hook) gets the same lines in its "needs attention" part. It does not send alerts of
   its own: SMART problems are not pushed through the alert state machine (my call; the user was not asked).
4. **Setup:** a new sudoers feature `smart` for `print-sudoers`/`check-host` (`--with smart`), so `smartctl` is
   allowed with fixed arguments. `smartctl` itself comes from the OS package (`pkg install smartmontools`); the
   book says so.

## What is a problem (defaults; fixed in v1, no settings)

- Red: the drive's overall health assessment is not PASSED.
- Yellow: reallocated sectors above 0; current pending sectors above 0; offline-uncorrectable above 0; for NVMe
  "critical warning" non-zero or "available spare" below its threshold; SSD/NVMe wear (percentage used) 80 % or
  more; temperature 60 °C or more.
- Nothing, not even a note: a host without `smartctl`, a disk that does not report SMART (virtual disks, USB
  bridges), and a host the daemon could not reach (the Hosts table already shows that). A `smartctl` that fails
  on a disk that does report is "unknown" for that disk and appears as yellow ("da2: SMART unreadable").
- Output is read as JSON (`smartctl -j`, smartmontools 7 or newer). An older smartmontools is treated as "does
  not report".

## Rules

- The daemon runs `smartctl` only read-only (`--scan`, `-H`, `-A`, `-i`, `-j`); it never starts a self-test (`-t`)
  or writes to a disk. The daemon sends nothing to a disk beyond SMART reads, once a day per disk.
- A host failing or hanging does not delay the others or the rest of the daemon: each host has its own deadline
  and the check runs off the scheduler's critical path. Bad or partial output never panics the daemon.
- `smart.json` is written atomically (like `notify.json`); the Dashboard shows nothing if it is missing or
  unreadable. A line says how old the check is; a check older than 3 days, while the daemon is running, adds one
  yellow line "disk check is N days old" so a broken check cannot look like good health.
- The whole thing is on by default only for hosts where `smartctl` is found; `[daemon] smart_check = "off"` or
  `"HH:MM"` changes the time or turns it off (my call).

## Decisions made after the builder check (my calls, flagged for the user)

- **Sudo not set up for `smartctl`** (a sudo denial) counts as "not set up": silent on the Dashboard, like a host
  without `smartctl`, so upgrading does not light every host yellow on day one. `check-host --with smart` reports
  the missing feature, and the book's setup page says to add it. The daemon learns whether `smartctl` exists from
  the pinned `smartctl --scan -j` itself (command not found or denied = nothing to check).
- **Which disks report:** a disk is checked only if `--scan` lists it and its JSON says `smart_support.available`
  is true. Anything else (empty scan, no support, virtual disk, USB bridge) is silent. Only a disk that says it
  supports SMART and then fails to answer or lacks the attribute table is "SMART unreadable" (yellow).
- **Failed or missed checks keep the last good result:** `smart.json` holds one entry per host, each with its own
  `checked_at` and, when today's check failed, an `error` next to the previous result. The Dashboard keeps showing
  the previous problem line with its real age ("checked 30 h ago"), so a host rebooting at 03:30 does not make a
  problem vanish. A host that has never answered has no entry. The "disk check is N days old" line uses the newest
  `checked_at` of the whole file.
- **Sudoers:** the daemon asks for the device list with `smartctl --scan -j` and then runs
  `smartctl -j -H -A -i /dev/<dev>`. The entry pins the flags and ends in `/dev/*`, so nothing needs regenerating
  when disks change. Honest limit (found while planning): sudo's `*` also matches spaces, so it cannot forbid an
  extra flag such as `-t`. The unprivileged account already holds `zfs` and `zpool` with any arguments (root
  equivalent, as `print-sudoers` already warns), so this widens nothing in practice; `print-sudoers` says so in its
  comment and `check-host` does not report the wildcard as "too broad" for this entry. The daemon itself only ever
  sends the read-only flags.
- **Enter on a line** selects that host and the Disks section (the existing Dashboard Enter behaviour); it does
  not focus a disk. For an unreachable host the line is not shown at all (the Hosts row says so).
- **Counts:** SMART lines are ordinary Needs attention items, so they count in the verdict ("2 issues, 1 to
  watch") like every other line.
- **`smart_check` is read when the daemon starts**; changing it needs a daemon restart, and the Settings screen
  does not edit it in v1.

## Part 2: a resilver in progress (added at the user's suggestion)

While a pool resilvers (a replaced disk, or one that came back), its redundancy is reduced, so the Dashboard says
so: a yellow Needs attention line per pool, `fbsd16   tank resilvering 42%`, which disappears when it finishes.
It uses the pool status the host probe already reads every 30 seconds (no new command, no sudo, no daemon part).
The probe's `ScanSummary::Running` has to say whether the scan is a scrub or a resilver; a running scrub is not
flagged (the Scrub column already shows it). A paused resilver is shown as `resilver paused`. A pool that is
DEGRADED keeps its own red/yellow line from the pool health; the resilver line is additional, not a replacement.
Enter goes to the Pools section of that host. Tests: a status with a resilver in progress, a scrub in progress
(no line), paused, and finished.

## Not in scope

- Fill predictions, a Shares row on the Dashboard (both parked), SMART history or graphs, starting self-tests,
  per-disk detail screens, push alerts for SMART, per-host or per-disk thresholds.
- ZFS-level disk errors: those are already pool health and scrub results.

## Left to the builder

The exact `smartctl` argument set and sudoers pinning (follow how `smbstatus` is pinned in `src/exec/privilege.rs`),
the shape of `smart.json`, parsing and judging as pure modules with fixture JSON from real drives (SATA HDD, SATA
SSD, NVMe), where the daily scheduling lives in the daemon, the wording of the lines, tests in the repo's style,
the book (a short section in chapter 6 or 16, the `[daemon]` key in chapter 21, the `smart` feature in chapter 4/5
where sudoers is explained) and README.

## Done means

Spec, plan, implementation with tests (fixtures for each drive kind and for broken output), whole-branch review,
a live check on the hub (install smartmontools there; the VMs have virtual disks, so they must stay silent; force a
red and a yellow line with a fixture `smart.json`), book updated, merged and installed.
