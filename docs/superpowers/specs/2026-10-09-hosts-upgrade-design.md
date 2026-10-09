# Hosts Upgrade — Design

Date: 2026-10-09

## Goal

Make the Hosts section show, at a glance, which managed hosts are reachable and healthy, and give the selected host a fuller bottom panel (CPU, memory, swap, L2ARC, per-pool detail). Today the list is names only, and only the selected host is ever sampled, so nothing can say a host is down or a pool is degraded until you select it.

## Scope

- A background health probe of **every** host, with the result shown in the Hosts list (marker and name color, address, state, round-trip time).
- New best-effort lines in the Hosts bottom panel: CPU model, cores and live usage; memory; swap; L2ARC; one row per pool (used %, fragmentation, health, last scrub).
- No sudoers change: only unprivileged `sysctl` and `swapinfo` are added, plus the `zpool` commands already allowed.

Non-goals (YAGNI): historical graphs of CPU/memory, alerting beyond the existing notify hook, resolving hostnames to IPs, temperatures/SMART, per-core CPU, ZFS txg or I/O queue statistics, changing how Pools/Datasets sample.

## Decisions made while brainstorming

1. **Health inputs:** disconnected (probe fails), a pool not ONLINE, pool errors or scrub problems, a pool nearly full (80% yellow, 90% red), and a failed job on that host. All five chosen.
2. **Bottom panel additions:** CPU (model, cores, live usage), memory and swap, ARC hit ratio and L2ARC, pool detail (fragmentation, scrub state, last scrub). All four chosen.
3. **Address:** the host part of the configured `ssh_target`, shown as configured (no DNS lookup); `local` shows `local`.

## The health probe

`zfs::host::probe_host(runner) -> HostProbe`, run on a worker thread per host:

1. `zpool list -Hp -o name,size,alloc,free,frag,health` through the host's runner (with `sudo -n` over SSH). Its duration is the round-trip time.
2. If step 1 failed **at the transport level** (the SSH client exited 255, or the process could not be spawned) the result is `Disconnected` and nothing else runs, so an unreachable host costs one `ConnectTimeout` (10 s), not several. Any other failure (sudo refused, no `zpool`) is `Unavailable { reason }`: the host answers but cannot be inspected; shown as a warning with the reason.
3. `zpool status` (all pools, one command) gives each pool's state, the read/write/checksum error counts of its top-level line, and its `scan:` line.

```
HostProbe::Disconnected
HostProbe::Unavailable { reason }
HostProbe::Reached { rtt_ms, pools: Vec<PoolProbe> }
PoolProbe { name, health, capacity_percent, frag_percent: Option<u8>, errors: u64, scan: ScanSummary }
ScanSummary { Never | Running { percent } | Finished { when, errors } | Canceled }
```

**Cadence:** once at startup, then about every 30 s per host, staggered by host index so the hosts are not all probed in the same second. At most one probe is in flight per host; a tick that finds one running does nothing. The result message carries the host index and name, and is discarded if that host no longer exists or its name changed (Add Host hot-reload keeps indices stable; they are append-only).

**Health level** (pure function of the latest probe, the host's name and the Jobs data):

| level | when | color |
|---|---|---|
| Probing | no probe has finished yet | dim |
| Disconnected | `HostProbe::Disconnected` | red |
| Critical | any pool not `ONLINE`, or any pool at 90% or more | red |
| Warning | `Unavailable`; any pool error count above 0; a last scrub that finished with errors; any pool at 80% or more; the latest run of an enabled job whose `host` is this host is `Failed` | yellow |
| Ok | none of the above | normal |

The worst applicable level wins, and the State column names the first reason of the worst level (`unreachable`, `tank DEGRADED`, `tank 91% full`, `tank 3 errors`, `scrub found errors on tank`, `job failed`, `cannot inspect: <reason>`, `ok`, `probing`). A job failure counts only for an **enabled** job whose **latest** run failed (an older failure followed by a success does not count; a disabled job does not count; this is the same rule the Jobs section's failed-jobs indicator uses).

A failed probe never clears the selected host's bottom panel: the previous good panel data stays, drawn dim with "(stale)" in its title, until the next sample succeeds.

## The Hosts list

A table replaces the plain list:

| column | content |
|---|---|
| Name | `● <name>` — marker and name in the health color; the selected row keeps the selection style |
| Address | host part of `ssh_target` (`user@` and any port stripped), or `local` |
| State | the reason text above |
| Rtt | `12ms`, `-` while unknown or disconnected |

`App` keeps `host_addresses` and `host_health` in step with `hosts` (including a host added with `n`). The one-host hint ("Only this host is configured. Press 'n' to add another host.") stays. Selection (`Up`/`Down`) and the Add Host flow are unchanged.

## The bottom panel (selected host)

`sample_host` keeps its single ~1 s blocking call; `HostSummary` gains best-effort fields (every one `Option`, a missing value shows `-`, a non-FreeBSD host just shows dashes):

- **CPU:** `hw.model`, `hw.ncpu`, and usage % from two `kern.cp_time` readings, one before and one after the existing 1-second `zpool iostat` window (usage = 100 * (non-idle delta) / (total delta), over the five `user nice sys intr idle` counters).
- **Memory:** `hw.physmem`, `vm.stats.vm.v_free_count`, `vm.stats.vm.v_wire_count`, `hw.pagesize` → physical, free, wired.
- **Swap:** `swapinfo -k` total row → used / total, or `none`.
- **ARC / L2ARC:** the existing ARC line; plus `kstat.zfs.misc.arcstats.l2_size`, `l2_hits`, `l2_misses` shown as `L2ARC 12.0G  hit 64%` only when `l2_size` is above 0.
- **Pools:** one row each from the `zpool list`/`zpool status` data: name, used %, fragmentation, health, last scrub (`3d ago, 0 errors`, `running 34%`, `never`, `found 2 errors`).

**Layout:** two text columns (System: OS, Up/rtt, Load, CPU, Memory, Swap; ZFS: version, Pools, Space, ARC/L2ARC, Status) with the per-pool rows beneath, when the panel is wide enough; otherwise one column. If the panel is too short for everything, the pool rows are cut first and the title says `(+N more lines: enlarge the terminal)`, so nothing is clipped silently. The exact widths are fixed in the plan against real panel sizes. The graphs keep working as today.

## Privileges and compatibility

- New unprivileged commands: `sysctl -n hw.model hw.ncpu hw.physmem hw.pagesize kern.cp_time vm.stats.vm.v_free_count vm.stats.vm.v_wire_count kstat.zfs.misc.arcstats.l2_size …` and `swapinfo -k`. README adds `swapinfo` to the list of commands a forced-command SSH key must allow; `sysctl` is already on it.
- `zpool list`/`zpool status` go through the existing `sudo -n` allowlist (already allowed).
- A host that is not FreeBSD or lacks a sysctl shows `-` for that line.

## Edge cases

- **Host unreachable at startup:** shows `probing` until the first result, then `unreachable` after at most 10 s.
- **Probe slower than the interval:** the next tick is skipped, not stacked.
- **Host removed/renamed while a probe runs:** the late result is discarded.
- **Zero pools:** `zpool list` succeeds with no rows: health Ok, pool table says "no pools".
- **`zpool status` fails but `zpool list` worked:** pool health and capacity still count; error and scrub data are unknown (no warning for them), and the State is not Disconnected.
- **Clock/time zone:** "3d ago" is computed from the host's own `date +%s` already used for uptime, not the hub's clock; scrub times parsed from `zpool status` use that host's local time zone as printed, so they are shown as printed when the age cannot be derived.
- **Jobs data not loaded yet:** no job-failure warnings until it is.

## Testing

- Parsers (pure, with real FreeBSD 15.1 and 16.0-CURRENT output captured from the hub and VMs): `hw.model`, `hw.ncpu`, the `kern.cp_time` usage delta (including idle-only, a counter that did not move, and a wrap), memory sysctls, `swapinfo` (with swap, without), L2ARC kstats, `zpool list` with `frag` (including `-`), and `zpool status` for several pools (scan states: never, running, finished with and without errors, canceled; error counts).
- Health rules: each rule and its boundaries (79/80/89/90%), errors versus none, a failed job versus an older failure followed by a success, a disabled job, another host's job, `Unavailable`, worst-level-wins and reason order.
- Probe machinery: exit 255 on the first command skips the second; any other failure is `Unavailable`; one probe in flight per host; staggering; a result for a gone host is discarded; a failed probe keeps the previous panel data and marks it stale.
- Render: the Hosts table at several widths (columns drop in a fixed order), colors per level, the panel in one and two columns, and the "(+N more lines)" notice on a short panel.
- **Live (verify empirically):** on the hub's four hosts, compare each new figure with the same value read by hand (`sysctl`, `swapinfo`, `zpool list -o frag`, `zpool status`); degrade something harmless to see Critical/Warning (for example a file-backed throwaway pool on the hub with a corrupted/offlined vdev, never a real pool), and point a host entry at an address that does not answer to see Disconnected within about 10 s without freezing the UI.

## Delivery

One implementation plan: the new parsers and `probe_host`, the health function, App state and the probe scheduling in the event loop, the Hosts table, the extended panel and its layout, README, and the live checks.

## Files touched (indicative)

`src/zfs/host.rs`, `src/zfs/pool.rs` (reuse/extend the scan parser), `src/zfs/mod.rs`, `src/ui/state.rs` (health/address state, `WorkerMsg::HostProbe`), `src/ui/mod.rs` (probe scheduling), `src/ui/layout.rs` (`draw_hosts`), `src/ui/metrics.rs` (`host_lines`, panel layout), `src/ui/hints.rs` (if hints change), `README.md`.

## Live results (2026-10-09)

- Hub, `fbsd16`, `fbsd16t1` figures compared with the same values read by hand: cores, physical memory, swap, per-pool fragmentation and capacity, scrub dates all matched (free memory moves between reads).
- Probing is staggered; all four hosts reach `ok` within ~10 s of start. A configured host at an address that does not answer showed `probing`, then `unreachable` about 15 s after start (stagger plus one 10 s SSH timeout), with the UI responsive throughout.
- A throwaway file-backed mirror with one side offline showed the hub red with `zit-m DEGRADED`; a throwaway pool at 86% showed it yellow with `zit-h 86% full`. Both pools and the temporary host entry were removed afterwards.
- Not tested live: the 90% red state (a small file-backed pool never exceeds ~50% `cap` because of ZFS slop space; covered by unit tests at 89/90), a failed job, the stale (dimmed) panel, the `no answer` state, device-level errors; all covered by unit/render tests only.
- Review fixes made after the live run (device-level errors, panel never clips silently, hung or panicking probes) were re-deployed and the panel re-checked on the hub at 170x48 only; small terminal sizes are covered by render tests at 80x24 and 100x30.
