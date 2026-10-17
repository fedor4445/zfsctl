# The Dashboard: one screen for the health of every zfsctl component

Date: 2026-10-17. Status: design, agreed in conversation with the owner; not built.

## Intent and why

zfsctl has grown to nine sections (Hosts, Pools, Datasets, Snapshots, Disks, Shares, Boot Envs, Jobs, Cloud, Alerts).
To answer "is anything wrong, or about to be?" the owner now has to visit several of them and remember what normal looks
like. The owner said plainly that their memory is not up to it. The Dashboard is a first tab that answers that question
at a glance: every component of zfsctl, one coloured line each, the problems on top.

It is a **display only**. Nothing on it changes anything. To fix a problem the owner goes to the section that owns it.

"Pretty but pragmatic" (the owner's words) is a requirement, not decoration: it must look good enough that opening it is a
pleasure, and every pretty element must carry information and degrade gracefully (see Look).

## Goals

- A new first section, **Dashboard**, before Hosts. It is the screen zfsctl opens on (my call, easy to reverse).
- One large borderless screen with these parts, top to bottom:
  1. A header line: name, the hub, the date and time, and a verdict ("all quiet", or "2 issues, 1 to watch").
  2. **Needs attention**: every current problem and every "watch" item, worst first, each with where it is, what it is
     and, when known, since when. Empty when all is well (it then says so in green).
  3. **Hosts**, one row per host: reachability, state, pools, space, memory, CPU, load, last scrub, disk errors.
  4. **Daemon**: running or not, heartbeat age.
  5. **Jobs**: counts by kind (scrub, snapshot, replicate, backup, cloud mirror), failed ones named.
  6. **Cloud**: each rclone remote, its jobs, the last result, and the bucket size only if it was measured this session.
  7. **Alerts**: whether the monitor is running, the last alert and the last daily report.
  8. **Shares**: whether NFS and Samba are available on each host. Lowest priority: see "Left to the builder".
- Colour-coded by severity, with a glyph and a word as well, so nothing depends on colour alone (the existing rule of the
  TUI). It must work in every theme including `mono`.
- "Future issues" means **early warnings from today's numbers**, not predictions: memory at 98%, a pool at 85%, a scrub
  that is 40 days old. They are the "to watch" level (yellow); a problem that is happening is "issue" (red).
- Hardware and resource health is the point: space, memory, CPU, scrubs, disk errors, plus the state of zfsctl's own parts.

## Non-goals

- No actions, no forms, no editing from the Dashboard. (Navigation only: see Keys.)
- No trend lines, no "fills in N days" prediction. That needs a history of samples that zfsctl does not keep. Possible later.
- No SMART or disk wear. It needs smartmontools and another sudoers line per host. Disk health here is what ZFS itself reports.
- No list of jobs by load, no "too many jobs" analysis. Memory and CPU are shown as plain coloured readings.
- No new sudoers entries on any host. No new privileged commands.
- No cloud API traffic of its own: the Dashboard never measures a bucket (that stays `s` in Cloud, because requests cost money).
- It does not replace Alerts and does not send anything. See "Known gap".

## Anti-goals (a failure even if it technically works)

- A screen that is yellow all the time. On FreeBSD with ZFS the ARC cache fills most of the RAM by design and gives it back
  when asked. A memory figure that counts it as used would alarm permanently and teach the owner to ignore the screen. The memory
  reading must not treat reclaimable ARC as pressure.
- A screen that looks healthy when it does not know. Missing, stale or unanswered data is shown as unknown (dim, `?`),
  never green. "Never green by default."
- A screen that loads the hosts or the cloud: it must reuse the probes that already run and add at most one extra ssh round
  trip per host per probe.
- Two truths: the Dashboard saying red while Alerts is silent. This is accepted for v1 only because it is stated.

## Constraints and facts about the existing code

- The host probe already runs about every 30 s per host (`zfs::host_health::probe_host`, `zpool list` + `zpool status`) and
  gives pool health, capacity (levels 80% warning, 90% critical), error counts and the last scan summary, and `assess()` turns
  it into a level and a reason. Reuse it; extend it, do not duplicate it.
- `ScanSummary::Finished.when` is the text `zpool status` prints, with no time zone, so today there is no scrub *age*. A scrub
  age needs one more fact: either `zpool status -j` timestamps where the host's OpenZFS has them, or parsing the text plus the
  host's UTC offset (`date +%z`, unprivileged). The builder chooses; both hosts' versions must work (the VMs run FreeBSD 16-current).
- Memory, CPU, load and swap are read today by `zfs::host::sample_host` for the **selected host only**, with unprivileged
  `sysctl`/`swapinfo` (which need no sudo and are already in the command registry). CPU percent needs two `kern.cp_time`
  readings; the Dashboard should compute it from the difference between two successive probes (30 s apart) rather than
  waiting inside a probe. The new readings must come with the existing probe in **one** extra `sysctl -n` call at most.
- Job results, the daemon heartbeat and the alert events come from the state directory (`jobs_view`, `alerts_view`), read
  locally every 2 to 10 s; the cloud rows come from `ui::cloud` (remote list asked on opening, then every minute).
- The TUI draws with ratatui; sections live in `ui::state::Section`; there are hint, help and book drift tests that will need
  the new section added in every list that names sections.

## Decisions (the owner's, as made)

- Own tab, first in the sidebar, borderless, one big screen.
- Components: hosts, disks and scrubs, daemon, jobs, cloud connections, alerts and mail, shares.
- Hardware/resource warnings: space, memory, CPU, scrubs, disk errors.
- Memory and CPU are shown as coloured values, not as a separate warning list.
- Visual only; fixing means going to the right tab.
- "Pretty but pragmatic."

## Look

- A header line with the verdict on the right, then sections each introduced by a dim capitalised heading with a thin rule
  (`HOSTS ───────`). No boxes, no side borders.
- Status marker per line: the TUI's existing glyphs (`●` ok, `▲` warning, `✖` critical, `○` off or unknown) plus a word.
- Percentages (space, memory, CPU) get a thin inline gauge where the terminal is wide enough, e.g. `━━━━╌╌╌╌╌╌ 43%`,
  coloured by level. Below a width threshold the gauges go and the number stays; below another, columns drop in a fixed order
  (the Hosts table already does this; follow its approach). It must stay usable at 80 columns and not break at 60.
- Needs attention is capped (say 6 lines, then "+3 more") so the screen never scrolls; the Hosts table is the only part that
  may scroll, and only with many hosts.
- Colours come from the theme. Nothing hard-coded.

## Levels and defaults

"Issue" is red, "watch" is yellow, fine is green, unknown is dim.

| Reading | Watch (yellow) | Issue (red) |
|---|---|---|
| Pool space | 80% | 90% (existing rule, kept) |
| Pool state, pool or scrub errors | scrub found errors, error counters above 0 | degraded, faulted, unavailable (existing rules) |
| Memory in use (ARC excluded) | 90% | 97% |
| CPU, sustained over two probes | 95% | none (CPU alone is never red) |
| Swap in use | above 50% | above 90% |
| Last scrub age | 35 days, or never | 90 days |
| Host unreachable, login refused | | red |
| Daemon not running, heartbeat stale | | red |
| A job whose latest run failed | | red |
| Cloud remote whose latest run failed | | red |
| Alert monitor not running while alerts are configured | yellow | |

These numbers are my defaults, not the owner's; they are marked as such. The new ones (memory, CPU, swap, scrub age) are
overridable in `config.toml` under a new `[dashboard]` section; the existing space rule keeps its constants.

## Keys

- `Tab` / `Shift+Tab` move between sections as everywhere. `r` refreshes.
- My call, droppable: `Up`/`Down` select a line of Needs attention and `Enter` jumps to the section that owns it (navigation
  only). If this complicates the build it is cut and the screen has no selection at all.

## Left to the builder

- Exact column set and widths at each terminal width, the gauge glyphs, the order columns drop in.
- How to compute memory use without ARC, and how to get scrub age (see Constraints); both need a short note on the choice.
- Whether the Shares row is worth its cost: per-host `service` checks cost sudoers-covered calls on every probe. If it is
  costly, show shares for the hub only, or drop the row, and say so in the book. Everything else is required.
- The names of the `[dashboard]` keys, the new module layout (a pure `ui::dashboard` that turns the existing facts into rows and
  levels is the obvious shape, fully unit-tested), the book chapter's placement and numbering.

## Calls made after the builder check (mine, for the owner to veto)

1. **Where the rules live.** The Dashboard has its own rule layer (`ui::dashboard`). For a host's state it starts from the
   existing `assess()` result and may only *raise* it with the new readings (memory, CPU, swap, scrub age); it never
   lowers it. `assess()`, the Hosts tab and the alert monitor are **unchanged**, so the Hosts tab looks exactly as it does
   today. The table's "failed job is red" means the *job's own line* (Jobs, Needs attention) is red; the host's state keeps
   whatever `assess()` says ("job failed", yellow).
2. **Probe cost.** Only the TUI collects the new readings, through a separate function in the same worker that already
   probes each host; the daemon's probes are untouched. Up to two extra unprivileged calls per host per probe: one
   `sysctl -n` for memory, CPU ticks and load, and `swapinfo -k` for swap. The host's UTC offset (for scrub age, if needed)
   is read once per host per session and kept. Still no new sudoers entries.
3. **Never scrubbed.** A pool that has never been scrubbed is yellow ("never scrubbed"), because it is true and worth
   knowing. To avoid a permanently yellow screen for pools nobody wants scrubbed (scratch pools), `[dashboard]` has
   `scrub_ignore = ["scratch"]`, a list of pool names. A resilver does **not** reset the scrub age: only scrubs count.
4. **Unknown data.** While any host has not answered its first probe, the verdict says "checking..." (dim), not "all quiet".
   After that, a host that cannot be inspected or does not answer is a "to watch" (yellow) line in Needs attention, so
   unknown is never counted as fine. Disabled jobs are left out of the job counts. The Cloud remote list is asked when the
   Dashboard opens (the Dashboard asks for it itself, like the Cloud tab does).

## Known gap

The health monitor that sends mail (Alerts) keeps its own, smaller set of rules and is unchanged. Memory, CPU and scrub-age
warnings therefore appear on the Dashboard but do not send mail. Feeding the new rules into the monitor is a sensible next step
and is out of scope here.

## Done means

- All tests pass, including new unit tests for every rule (boundaries, unknown data, ARC not alarming), render tests at wide,
  80 and 60 columns in more than one theme, and the existing hint, help and book drift tests updated.
- Verified live on the hub and a VM without stressing them: thresholds forced low through `config.toml` to see yellow and red,
  a host made unreachable to see the red row, the daemon stopped to see its line.
- The book has a Dashboard chapter or section with a real capture; README, keys reference and help text mention it.
