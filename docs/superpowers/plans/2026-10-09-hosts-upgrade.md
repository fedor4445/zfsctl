# Hosts Upgrade Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Hosts list shows each host's address and a health color/state from a background probe of every host; the selected host's bottom panel gains CPU, memory, swap, L2ARC and per-pool detail.

**Architecture:** Pure parsers and the probe live in two modules: `src/zfs/host.rs` (system figures, extended `sample_host`) and a new `src/zfs/host_health.rs` (`HostProbe`, `assess`, `probe_host`). `App` keeps a per-host probe state (keyed by host name), a scheduler decides which hosts are due, and the event loop starts the probes on worker threads, exactly like the existing account/pool fetches. The Hosts table and the host panel are pure functions of that state.

**Tech Stack:** Rust, ratatui, existing `CommandRunner`/`FakeRunner`, scripted test doubles with real FreeBSD output captured on 2026-10-09 (hub: 15.1, `fbsd16`: 16.0-CURRENT).

**Spec:** `docs/superpowers/specs/2026-10-09-hosts-upgrade-design.md`

## Global Constraints

- No sudoers change: the only new commands are unprivileged `sysctl -n …` and `swapinfo -k` (README lists `swapinfo`); `zpool list`/`zpool status` already go through the allowlist.
- Every new figure is best-effort: a missing sysctl, a non-FreeBSD host or an unparsable value gives `None`/`-`, never an error and never a panic.
- A probe never blocks the UI: one worker thread per probe, at most one in flight per host, results carry the host name and are discarded for a host that no longer exists.
- An unreachable host costs one SSH `ConnectTimeout` (10 s): the probe's first command failing at the SSH level (exit status 255) stops the probe.
- Health: Critical = a pool not ONLINE or any pool ≥ 90% full; Warning = `Unavailable`, any pool error count > 0, a last scrub that finished with errors, any pool ≥ 80% full, or the latest run of an enabled job on that host is `Failed`; the worst level wins, the reason is the first of the worst level, in the order listed.
- Lints: `pedantic` plus `unwrap_used`/`indexing_slicing` warnings: no `unwrap()`/indexing in non-test code. Baseline: `cargo test` = 1159 passed; `cargo clippy` = exactly 1 warning (`method get is never used`).
- Commit messages end with: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. Work on branch `hosts-upgrade`; merge into `master` with `--no-ff` only after the user chooses to; no remote.

## Rulings made while planning

1. **A process that cannot be spawned is `Unavailable`, not `Disconnected`** (the spec said both): a local host without `zpool`, or a missing `ssh` binary, is something to fix, not an unreachable machine; it shows as `cannot inspect: <reason>`. Only exit status 255 from the SSH client means Disconnected.
2. **"Last scrub" is shown as `zpool status` prints it** (`Thu Oct  8 19:48:07 2026`), not as an age: the printed time carries no time zone, so an age would be a guess (the spec allowed this fallback).
3. **The "+N more" notice is the panel's last line**, not part of the title (the title is built before the panel knows how much room it has).
4. **Probe state is keyed by host name**, not index, so it stays correct whatever order hosts arrive in.
5. **The host panel uses its own layout** (graphs 20%/20%, text 60%) because the shared one gives the text only 40% of the width.

## Review Focus

1. A host whose probe is slower than the 30 s interval, or that is removed/renamed while a probe runs: no stacking, no stale result applied.
2. `Unavailable` vs `Disconnected`: sudo refused, no `zpool`, an ssh failure text, a spawn failure.
3. Health boundaries and reason order: 79/80/89/90% capacity, errors 0 vs >0, a failed job followed by a success, a disabled job, another host's job, a pool at 90% that is also DEGRADED.
4. `zpool status` oddities: a pool with no `scan:` line, `scrub canceled`, a scrub in progress, large error counts with a suffix (`1.2K`), several pools, a pool whose name is a prefix of another.
5. Narrow and short terminals: the Hosts table drops columns in a fixed order; the host panel never clips silently.

## File Structure

- Create `src/zfs/host_health.rs` — `HostProbe`, `PoolProbe`, `ScanSummary`, `Level`, `Assessment`, `parse_pool_list_frag`, `parse_status_all`, `probe_host`, `assess`.
- Modify `src/zfs/host.rs` — new parsers and `HostSummary` fields, `sample_host` extension.
- Modify `src/zfs/mod.rs` — `pub mod host_health;`.
- Modify `src/ui/state.rs` — `host_addresses`, `host_probes`, `WorkerMsg::HostHealth`, scheduling, assessment.
- Modify `src/ui/mod.rs` — address setup, `poll_host_probes`, new-host address.
- Modify `src/ui/layout.rs` — `draw_hosts`.
- Modify `src/ui/metrics.rs` — host panel lines, layout, stale marker.
- Modify `README.md`, the spec (live results).

---

### Task 1: System-figure parsers

**Files:**
- Modify: `src/zfs/host.rs` (new types and parsers above `stdout_of`; tests in its `mod tests`)

**Interfaces:**
- Produces: `CpuTimes { user, nice, sys, intr, idle: u64 }`, `parse_cpu_times(&str) -> Option<CpuTimes>`, `cpu_percent(before: CpuTimes, after: CpuTimes) -> Option<f64>`, `Memory { physical, free, wired: u64 }` (bytes), `Swap { total, used: u64 }` (bytes), `L2arc { size, hits, misses: u64 }` with `hit_percent(&self) -> Option<f64>`, `parse_core_sysctls(&str) -> Option<CoreFacts>` where `CoreFacts { model: String, cores: u32, memory: Memory }`, `parse_swapinfo(&str) -> Option<Swap>`, `parse_l2arc(&str) -> Option<L2arc>`.

- [ ] **Step 1: Branch and failing tests**

```bash
git switch -c hosts-upgrade
```

Append inside `mod tests` of `src/zfs/host.rs`:

```rust
    // Captured on 2026-10-09: the FreeBSD 15.1 hub (36 cores), then fbsd16 (16.0-CURRENT, 4 cores).
    const CORE_HUB: &str = "Intel(R) Xeon(R) CPU E5-2660 0 @ 2.20GHz\n36\n21418688512\n4096\n2563464\n1706935\n";
    const CORE_16: &str = "Intel Xeon E312xx (Sandy Bridge, IBRS update)\n4\n8534835200\n4096\n1627099\n304669\n";

    #[test]
    fn core_sysctls_give_the_model_cores_and_memory_in_bytes() {
        let hub = parse_core_sysctls(CORE_HUB).unwrap();
        assert_eq!((hub.model.as_str(), hub.cores), ("Intel(R) Xeon(R) CPU E5-2660 0 @ 2.20GHz", 36));
        assert_eq!(hub.memory, Memory { physical: 21_418_688_512, free: 2_563_464 * 4096, wired: 1_706_935 * 4096 });
        let vm = parse_core_sysctls(CORE_16).unwrap();
        assert_eq!((vm.model.as_str(), vm.cores, vm.memory.physical), ("Intel Xeon E312xx (Sandy Bridge, IBRS update)", 4, 8_534_835_200));
    }

    #[test]
    fn core_sysctls_that_are_incomplete_or_garbled_give_nothing() {
        assert_eq!(parse_core_sysctls(""), None);
        assert_eq!(parse_core_sysctls("model\n4\n"), None, "too few lines");
        assert_eq!(parse_core_sysctls("model\nfour\n1\n4096\n1\n1\n"), None, "cores must be a number");
        assert_eq!(parse_core_sysctls("model\n4\n1000\n0\n1\n1\n"), None, "a page size of 0 is not a page size");
    }

    #[test]
    fn cpu_times_are_five_counters_on_one_line() {
        assert_eq!(parse_cpu_times("5976 0 31301 3223 74232938\n"), Some(CpuTimes { user: 5976, nice: 0, sys: 31301, intr: 3223, idle: 74_232_938 }));
        assert_eq!(parse_cpu_times("1 2 3\n"), None);
        assert_eq!(parse_cpu_times("a b c d e"), None);
    }

    #[test]
    fn cpu_usage_is_the_non_idle_share_of_the_counters_that_moved() {
        let a = CpuTimes { user: 100, nice: 0, sys: 100, intr: 0, idle: 800 };
        let b = CpuTimes { user: 150, nice: 0, sys: 150, intr: 0, idle: 900 };
        assert_eq!(cpu_percent(a, b), Some(50.0), "100 busy ticks out of 200");
        assert_eq!(cpu_percent(a, a), None, "no ticks passed: no figure, not 0%");
        let idle_only = CpuTimes { idle: 1000, ..a };
        assert_eq!(cpu_percent(a, idle_only), Some(0.0));
        let wrapped = CpuTimes { user: 10, nice: 0, sys: 10, intr: 0, idle: 10 };
        assert_eq!(cpu_percent(a, wrapped), None, "counters that went backwards (a reboot between samples) give no figure");
    }

    #[test]
    fn swapinfo_reports_total_and_used_in_bytes_or_nothing_without_swap() {
        let one = "Device          1K-blocks     Used    Avail Capacity\n/dev/ada0p3       2097152        0  2097152     0%\n";
        assert_eq!(parse_swapinfo(one), Some(Swap { total: 2_097_152 * 1024, used: 0 }));
        let two = "Device          1K-blocks     Used    Avail Capacity\n/dev/ada0p3       1000      100      900    10%\n/dev/ada1p3       1000      300      700    30%\nTotal             2000      400     1600    20%\n";
        assert_eq!(parse_swapinfo(two), Some(Swap { total: 2000 * 1024, used: 400 * 1024 }), "the Total row wins when there are several devices");
        assert_eq!(parse_swapinfo("Device          1K-blocks     Used    Avail Capacity\n"), None);
        assert_eq!(parse_swapinfo(""), None);
    }

    #[test]
    fn l2arc_is_three_kstat_lines_with_a_hit_rate() {
        assert_eq!(parse_l2arc("0\n0\n0\n"), Some(L2arc { size: 0, hits: 0, misses: 0 }));
        let l2 = parse_l2arc("12884901888\n600\n400\n").unwrap();
        assert_eq!(l2.hit_percent(), Some(60.0));
        assert_eq!(L2arc { size: 1, hits: 0, misses: 0 }.hit_percent(), None);
        assert_eq!(parse_l2arc("1\n2\n"), None);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test core_sysctls cpu_ swapinfo l2arc 2>&1 | grep -E "^error" | head -3` (separate filters if cargo rejects several)
Expected: compile errors (`parse_core_sysctls`, `CpuTimes` … not found).

- [ ] **Step 3: Implement** — add above `fn stdout_of` in `src/zfs/host.rs`:

```rust
/// `kern.cp_time`: the five cumulative tick counters `user nice sys intr idle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub sys: u64,
    pub intr: u64,
    pub idle: u64,
}

impl CpuTimes {
    fn total(self) -> u64 {
        self.user.saturating_add(self.nice).saturating_add(self.sys).saturating_add(self.intr).saturating_add(self.idle)
    }
}

pub fn parse_cpu_times(output: &str) -> Option<CpuTimes> {
    let nums: Vec<u64> = output.split_whitespace().map(|t| t.parse().ok()).collect::<Option<Vec<u64>>>()?;
    let &[user, nice, sys, intr, idle] = nums.as_slice() else { return None };
    Some(CpuTimes { user, nice, sys, intr, idle })
}

/// Non-idle share of the ticks between two readings. `None` when no ticks passed or a counter went
/// backwards (a reboot between the samples): no figure is better than a wrong one.
#[allow(clippy::cast_precision_loss)] // display only
pub fn cpu_percent(before: CpuTimes, after: CpuTimes) -> Option<f64> {
    let moved = |a: u64, b: u64| b.checked_sub(a);
    let total = moved(before.total(), after.total())?;
    let idle = moved(before.idle, after.idle)?;
    (total > 0 && idle <= total).then(|| (total - idle) as f64 * 100.0 / total as f64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    pub physical: u64,
    pub free: u64,
    pub wired: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreFacts {
    pub model: String,
    pub cores: u32,
    pub memory: Memory,
}

/// Six `sysctl -n` lines: `hw.model`, `hw.ncpu`, `hw.physmem`, `hw.pagesize`, `v_free_count`, `v_wire_count`.
pub fn parse_core_sysctls(output: &str) -> Option<CoreFacts> {
    let lines: Vec<&str> = output.lines().map(str::trim).collect();
    let &[model, ncpu, physmem, pagesize, free_pages, wire_pages] = lines.as_slice() else { return None };
    let page: u64 = pagesize.parse().ok().filter(|p| *p > 0)?;
    let pages = |s: &str| s.parse::<u64>().ok().map(|n| n.saturating_mul(page));
    Some(CoreFacts {
        model: Some(model).filter(|m| !m.is_empty())?.to_string(),
        cores: ncpu.parse().ok()?,
        memory: Memory { physical: physmem.parse().ok()?, free: pages(free_pages)?, wired: pages(wire_pages)? },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swap {
    pub total: u64,
    pub used: u64,
}

/// `swapinfo -k`: the `Total` row when there are several devices, else the one device's row; `None`
/// when the host has no swap (header only).
pub fn parse_swapinfo(output: &str) -> Option<Swap> {
    let rows: Vec<Vec<&str>> = output.lines().skip(1).map(|l| l.split_whitespace().collect::<Vec<_>>()).filter(|f| f.len() >= 4).collect();
    let row = rows.iter().find(|f| f.first() == Some(&"Total")).or_else(|| rows.first())?;
    let kib = |s: &str| s.parse::<u64>().ok().map(|n| n.saturating_mul(1024));
    Some(Swap { total: kib(row.get(1)?)?, used: kib(row.get(2)?)? })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct L2arc {
    pub size: u64,
    pub hits: u64,
    pub misses: u64,
}

impl L2arc {
    #[allow(clippy::cast_precision_loss)] // display only
    pub fn hit_percent(&self) -> Option<f64> {
        let total = self.hits.saturating_add(self.misses);
        (total > 0).then(|| self.hits as f64 * 100.0 / total as f64)
    }
}

/// Three `sysctl -n` lines: `l2_size`, `l2_hits`, `l2_misses`.
pub fn parse_l2arc(output: &str) -> Option<L2arc> {
    let nums: Vec<u64> = output.lines().filter_map(|l| l.trim().parse().ok()).collect();
    let &[size, hits, misses] = nums.as_slice() else { return None };
    Some(L2arc { size, hits, misses })
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"; cargo clippy 2>&1 | grep -E "^warning" | grep -v "generated\|never used"`
Expected: all pass; clippy shows only dead-code warnings for the not-yet-used helpers.

- [ ] **Step 5: Commit**

```bash
git add src/zfs/host.rs
git commit -m "feat: parsers for CPU, memory, swap and L2ARC figures

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Probe types, `zpool status` parsing, `probe_host`, `assess`

**Files:**
- Create: `src/zfs/host_health.rs`
- Modify: `src/zfs/mod.rs` (add `pub mod host_health;` after `pub mod host;`)

**Interfaces:**
- Consumes: `pool::{Pool, PoolHealth, ScanStatus, parse_pool_scan_status}`, `CommandRunner`.
- Produces:
  - `ScanSummary::{Never, Running { percent: Option<u8> }, Finished { when: String, errors: u64 }, Canceled}`
  - `PoolProbe { name: String, health: PoolHealth, capacity_percent: u8, frag_percent: Option<u8>, errors: Option<u64>, scan: Option<ScanSummary> }`
  - `HostProbe::{Disconnected, Unavailable { reason: String }, Reached { rtt_ms: u64, pools: Vec<PoolProbe> }}`
  - `Level::{Probing, Disconnected, Critical, Warning, Ok}`, `Assessment { level: Level, reason: String }`
  - `parse_pool_list_frag(&str) -> Result<Vec<(Pool, Option<u8>)>>`, `parse_status_all(&str) -> Vec<StatusFacts>` with `StatusFacts { name: String, errors: Option<u64>, scan: ScanSummary }`
  - `probe_host(&dyn CommandRunner) -> HostProbe`, `assess(Option<&HostProbe>, job_failed: bool) -> Assessment`

- [ ] **Step 1: Write the failing tests.** Create `src/zfs/host_health.rs` containing only this test module (it will not compile until Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::CommandOutput;
    use crate::zfs::pool::PoolHealth;
    use std::sync::Mutex;

    // Real `zpool list -Hp -o name,size,alloc,free,frag,health` and `zpool status` output, hub, 2026-10-09.
    const LIST: &str = "scratch\t251658240\t106573824\t145084416\t39\tONLINE\ntpool\t20937965568\t8663924736\t12274040832\t2\tONLINE\nzroot\t45634027520\t11182317568\t34451709952\t33\tONLINE\n";
    const STATUS: &str = "  pool: scratch\n state: ONLINE\n  scan: scrub repaired 0B in 00:00:01 with 0 errors on Thu Oct  8 19:48:07 2026\nconfig:\n\n\tNAME                            STATE     READ WRITE CKSUM\n\tscratch                         ONLINE       0     0     0\n\t  mirror-0                      ONLINE       0     0     0\n\t    /root/zfsctl-scratch/vdev3  ONLINE       0     0     0\n\nerrors: No known data errors\n\n  pool: tpool\n state: ONLINE\n  scan: resilvered 501M in 00:00:30 with 0 errors on Sat Sep 26 13:15:14 2026\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\ttpool       ONLINE       0     0     0\n\t  raidz2-0  ONLINE       0     0     0\n\nerrors: No known data errors\n\n  pool: zroot\n state: ONLINE\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\tzroot       ONLINE       0     0     0\n\t  ada0p4    ONLINE       0     0     0\n\nerrors: No known data errors\n";

    fn pool(name: &str, health: PoolHealth, capacity: u8) -> PoolProbe {
        PoolProbe { name: name.to_string(), health, capacity_percent: capacity, frag_percent: Some(1), errors: Some(0), scan: Some(ScanSummary::Never) }
    }

    fn reached(pools: Vec<PoolProbe>) -> HostProbe {
        HostProbe::Reached { rtt_ms: 5, pools }
    }

    #[test]
    fn the_pool_list_with_fragmentation_parses_including_a_dash() {
        let rows = parse_pool_list_frag(LIST).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[0].0.name.as_str(), rows[0].0.size, rows[0].0.allocated, rows[0].1), ("scratch", 251_658_240, 106_573_824, Some(39)));
        assert_eq!(rows[2].1, Some(33));
        let dash = parse_pool_list_frag("tank\t100\t10\t90\t-\tONLINE\n").unwrap();
        assert_eq!(dash[0].1, None, "a dash means unknown fragmentation");
        assert!(parse_pool_list_frag("tank\t100\t10\n").is_err());
        assert_eq!(parse_pool_list_frag("").unwrap().len(), 0);
    }

    #[test]
    fn zpool_status_gives_each_pools_errors_and_last_scrub() {
        let facts = parse_status_all(STATUS);
        assert_eq!(facts.len(), 3);
        assert_eq!(facts[0].name, "scratch");
        assert_eq!(facts[0].errors, Some(0));
        assert_eq!(facts[0].scan, ScanSummary::Finished { when: "Thu Oct  8 19:48:07 2026".to_string(), errors: 0 });
        assert_eq!(facts[1].scan, ScanSummary::Finished { when: "Sat Sep 26 13:15:14 2026".to_string(), errors: 0 }, "a resilver counts as the last scan");
        assert_eq!(facts[2].scan, ScanSummary::Never, "no scan: line at all");
    }

    #[test]
    fn scan_variants_and_error_counts_are_read_from_their_own_pool_block() {
        let status = "  pool: a\n state: DEGRADED\n  scan: scrub in progress since Thu Oct  8 19:00:00 2026\n\t1G scanned at 100M/s, 500M issued at 50M/s, 2G total\n\t0B repaired, 24.40% done, 00:00:30 to go\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\ta     DEGRADED  3  0  1.2K\n\n  pool: ab\n state: ONLINE\n  scan: scrub repaired 0B in 00:00:01 with 4 errors on Thu Oct  8 19:48:07 2026\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\tab    ONLINE  0  0  0\n\n  pool: c\n state: ONLINE\n  scan: scrub canceled on Thu Oct  8 19:48:07 2026\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\tc     ONLINE  0  0  0\n";
        let facts = parse_status_all(status);
        assert_eq!(facts.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["a", "ab", "c"], "a pool whose name is a prefix of another stays separate");
        assert!(matches!(facts[0].scan, ScanSummary::Running { percent: Some(24) }), "{:?}", facts[0].scan);
        assert_eq!(facts[0].errors, Some(1 + 3), "READ 3 plus a CKSUM token with a suffix (counted as at least 1)");
        assert_eq!(facts[1].scan, ScanSummary::Finished { when: "Thu Oct  8 19:48:07 2026".to_string(), errors: 4 });
        assert_eq!(facts[2].scan, ScanSummary::Canceled);
    }

    /// A runner answering `zpool list` and `zpool status` from fixed outputs, recording the commands.
    struct Answers {
        list: CommandOutput,
        status: CommandOutput,
        calls: Mutex<Vec<String>>,
    }

    fn out(stdout: &str, stderr: &str, status: i32) -> CommandOutput {
        CommandOutput { stdout: stdout.to_string(), stderr: stderr.to_string(), status }
    }

    impl crate::exec::CommandRunner for Answers {
        fn run(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
            self.calls.lock().unwrap().push(format!("{cmd} {}", args.first().copied().unwrap_or("")));
            match (cmd, args.first().copied()) {
                ("zpool", Some("list")) => Ok(self.list.clone()),
                ("zpool", Some("status")) => Ok(self.status.clone()),
                other => panic!("unexpected {other:?}"),
            }
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _: &str) -> crate::error::Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
            self.run(cmd, args)
        }
    }

    fn answers(list: CommandOutput, status: CommandOutput) -> Answers {
        Answers { list, status, calls: Mutex::new(vec![]) }
    }

    #[test]
    fn a_reachable_host_gives_pools_with_their_status_joined_by_name() {
        let runner = answers(out(LIST, "", 0), out(STATUS, "", 0));
        let HostProbe::Reached { pools, .. } = probe_host(&runner) else { panic!() };
        assert_eq!(pools.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["scratch", "tpool", "zroot"]);
        assert_eq!(pools[0].capacity_percent, 42);
        assert_eq!(pools[0].frag_percent, Some(39));
        assert_eq!(pools[0].errors, Some(0));
        assert_eq!(pools[2].scan, Some(ScanSummary::Never));
    }

    #[test]
    fn an_ssh_level_failure_is_disconnected_and_skips_the_second_command() {
        let runner = answers(out("", "ssh: connect to host 10.0.0.9 port 22: Connection timed out", 255), out(STATUS, "", 0));
        assert_eq!(probe_host(&runner), HostProbe::Disconnected);
        assert_eq!(runner.calls.lock().unwrap().len(), 1, "one timeout, not two");
    }

    #[test]
    fn any_other_failure_is_unavailable_with_the_reason() {
        let sudo = answers(out("", "sudo: a password is required\n", 1), out("", "", 0));
        assert_eq!(probe_host(&sudo), HostProbe::Unavailable { reason: "sudo: a password is required".to_string() });
        let none = answers(out("", "", 127), out("", "", 0));
        assert!(matches!(probe_host(&none), HostProbe::Unavailable { reason } if reason.contains("127")));
        let garbled = answers(out("tank\t1\n", "", 0), out("", "", 0));
        assert!(matches!(probe_host(&garbled), HostProbe::Unavailable { .. }));
    }

    #[test]
    fn a_process_that_cannot_be_spawned_is_unavailable_not_disconnected() {
        struct Broken;
        impl crate::exec::CommandRunner for Broken {
            fn run(&self, cmd: &str, _: &[&str]) -> crate::error::Result<CommandOutput> {
                Err(crate::error::AppError::Spawn { cmd: cmd.to_string(), source: std::io::Error::other("no such file") })
            }
            fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
            fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
        }
        assert!(matches!(probe_host(&Broken), HostProbe::Unavailable { reason } if reason.contains("zpool")));
    }

    #[test]
    fn when_zpool_status_fails_the_pools_still_count_but_errors_and_scans_are_unknown() {
        let runner = answers(out(LIST, "", 0), out("", "boom", 1));
        let HostProbe::Reached { pools, .. } = probe_host(&runner) else { panic!() };
        assert_eq!(pools.len(), 3);
        assert!(pools.iter().all(|p| p.errors.is_none() && p.scan.is_none()));
    }

    fn level(probe: Option<&HostProbe>, job_failed: bool) -> (Level, String) {
        let a = assess(probe, job_failed);
        (a.level, a.reason)
    }

    #[test]
    fn health_levels_and_reasons() {
        assert_eq!(level(None, false), (Level::Probing, "probing".to_string()));
        assert_eq!(level(Some(&HostProbe::Disconnected), true), (Level::Disconnected, "unreachable".to_string()));
        assert_eq!(level(Some(&HostProbe::Unavailable { reason: "sudo: a password is required".into() }), false), (Level::Warning, "cannot inspect: sudo: a password is required".to_string()));
        assert_eq!(level(Some(&reached(vec![pool("tank", PoolHealth::Online, 10)])), false), (Level::Ok, "ok".to_string()));
        assert_eq!(level(Some(&reached(vec![])), false), (Level::Ok, "ok".to_string()), "no pools is not a problem");
    }

    #[test]
    fn capacity_boundaries_are_79_80_89_90() {
        let at = |c| level(Some(&reached(vec![pool("tank", PoolHealth::Online, c)])), false);
        assert_eq!(at(79).0, Level::Ok);
        assert_eq!(at(80), (Level::Warning, "tank 80% full".to_string()));
        assert_eq!(at(89).0, Level::Warning);
        assert_eq!(at(90), (Level::Critical, "tank 90% full".to_string()));
        assert_eq!(at(100).0, Level::Critical);
    }

    #[test]
    fn a_pool_that_is_not_online_is_critical_and_outranks_everything_else() {
        let mut p = pool("tank", PoolHealth::Degraded, 95);
        p.errors = Some(5);
        assert_eq!(level(Some(&reached(vec![p])), true), (Level::Critical, "tank DEGRADED".to_string()), "the first reason of the worst level");
        for health in [PoolHealth::Faulted, PoolHealth::Offline, PoolHealth::Unavail, PoolHealth::Removed] {
            assert_eq!(level(Some(&reached(vec![pool("t", health, 1)])), false).0, Level::Critical);
        }
        assert_eq!(level(Some(&reached(vec![pool("t", PoolHealth::Other("SUSPENDED".into()), 1)])), false), (Level::Critical, "t SUSPENDED".to_string()));
    }

    #[test]
    fn warnings_in_order_errors_then_scrub_then_capacity_then_job() {
        let mut errs = pool("a", PoolHealth::Online, 85);
        errs.errors = Some(3);
        errs.scan = Some(ScanSummary::Finished { when: "x".into(), errors: 2 });
        assert_eq!(level(Some(&reached(vec![errs])), true), (Level::Warning, "a 3 errors".to_string()));
        let mut scrub = pool("b", PoolHealth::Online, 85);
        scrub.scan = Some(ScanSummary::Finished { when: "x".into(), errors: 2 });
        assert_eq!(level(Some(&reached(vec![scrub])), true), (Level::Warning, "scrub found errors on b".to_string()));
        assert_eq!(level(Some(&reached(vec![pool("c", PoolHealth::Online, 85)])), true), (Level::Warning, "c 85% full".to_string()));
        assert_eq!(level(Some(&reached(vec![pool("d", PoolHealth::Online, 10)])), true), (Level::Warning, "job failed".to_string()));
        let mut clean = pool("e", PoolHealth::Online, 10);
        clean.scan = Some(ScanSummary::Finished { when: "x".into(), errors: 0 });
        clean.errors = Some(0);
        assert_eq!(level(Some(&reached(vec![clean])), false).0, Level::Ok);
        let mut unknown = pool("f", PoolHealth::Online, 10);
        unknown.errors = None;
        unknown.scan = None;
        assert_eq!(level(Some(&reached(vec![unknown])), false).0, Level::Ok, "unknown errors/scan never raise a warning");
    }

    #[test]
    fn the_worst_pool_decides_when_there_are_several() {
        let probe = reached(vec![pool("fine", PoolHealth::Online, 10), pool("full", PoolHealth::Online, 91), pool("warm", PoolHealth::Online, 85)]);
        assert_eq!(level(Some(&probe), false), (Level::Critical, "full 91% full".to_string()));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test host_health 2>&1 | grep -E "^error" | head -3` after adding `pub mod host_health;` to `src/zfs/mod.rs`.
Expected: compile errors (`probe_host`, `assess`, … not found).

- [ ] **Step 3: Implement** — put this above the test module in `src/zfs/host_health.rs`:

```rust
//! Whether a managed host is reachable and healthy, from one cheap probe (`zpool list` + `zpool status`),
//! and the pure rules that turn a probe (plus "did a job of this host fail?") into a level and a reason.

use super::pool::{parse_pool_scan_status, Pool, PoolHealth, ScanStatus};
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanSummary {
    Never,
    Running { percent: Option<u8> },
    /// The last scrub or resilver; `when` is as `zpool status` prints it (no time zone, so no age).
    Finished { when: String, errors: u64 },
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolProbe {
    pub name: String,
    pub health: PoolHealth,
    pub capacity_percent: u8,
    pub frag_percent: Option<u8>,
    /// Read + write + checksum errors of the pool's top-level line; `None` when `zpool status` was unavailable.
    pub errors: Option<u64>,
    pub scan: Option<ScanSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostProbe {
    /// The SSH client could not reach the host (exit status 255).
    Disconnected,
    /// The host answered but cannot be inspected (sudo refused, no `zpool`, unparsable output).
    Unavailable { reason: String },
    Reached { rtt_ms: u64, pools: Vec<PoolProbe> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Probing,
    Disconnected,
    Critical,
    Warning,
    Ok,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub level: Level,
    pub reason: String,
}

const LIST_ARGS: &[&str] = &["list", "-Hp", "-o", "name,size,alloc,free,frag,health"];

/// Parses `zpool list -Hp -o name,size,alloc,free,frag,health`; fragmentation `-` is `None`.
pub fn parse_pool_list_frag(output: &str) -> Result<Vec<(Pool, Option<u8>)>> {
    let mut rows = Vec::new();
    for line in output.lines().filter(|l| !l.trim().is_empty()) {
        let fields: Vec<&str> = line.split('\t').collect();
        let &[name, size, alloc, free, frag, health] = fields.as_slice() else {
            return Err(AppError::Parse(format!("expected 6 fields in zpool list line: {line:?}")));
        };
        let num = |s: &str| super::parse_numeric_or_dash(s).ok_or_else(|| AppError::Parse(format!("bad number {s:?} in: {line:?}")));
        let pool = Pool { name: name.to_string(), size: num(size)?, allocated: num(alloc)?, free: num(free)?, health: PoolHealth::parse(health) };
        rows.push((pool, frag.parse::<u8>().ok()));
    }
    Ok(rows)
}

/// What `zpool status` says about one pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFacts {
    pub name: String,
    pub errors: Option<u64>,
    pub scan: ScanSummary,
}

/// A READ/WRITE/CKSUM token: a number, or a count with a suffix (`1.2K`), which is at least 1.
fn count_token(token: &str) -> u64 {
    token.parse().unwrap_or_else(|_| u64::from(token != "0"))
}

fn scan_summary(block: &str) -> ScanSummary {
    match parse_pool_scan_status(block) {
        None => ScanSummary::Never,
        Some(ScanStatus::InProgress { percent_done, .. }) => ScanSummary::Running { percent: percent_done.map(|p| p.clamp(0.0, 100.0) as u8) },
        Some(ScanStatus::Completed { summary, errors }) => {
            if summary.contains("canceled") {
                return ScanSummary::Canceled;
            }
            let when = summary.split_once(" on ").map_or_else(|| summary.clone(), |(_, w)| w.trim().to_string());
            let n = summary.split(" with ").nth(1).and_then(|rest| rest.split_whitespace().next()).and_then(|t| t.parse().ok());
            ScanSummary::Finished { when, errors: n.unwrap_or_else(|| u64::from(errors)) }
        }
    }
}

/// Splits `zpool status` (all pools) at its `pool:` lines and reads each block on its own.
pub fn parse_status_all(output: &str) -> Vec<StatusFacts> {
    let mut blocks: Vec<(String, Vec<&str>)> = Vec::new();
    for line in output.lines() {
        if let Some(name) = line.trim_start().strip_prefix("pool:") {
            blocks.push((name.trim().to_string(), vec![line]));
        } else if let Some((_, lines)) = blocks.last_mut() {
            lines.push(line);
        }
    }
    blocks
        .into_iter()
        .map(|(name, lines)| {
            let text = lines.join("\n");
            let after_header = lines.iter().skip_while(|l| !l.trim_start().starts_with("NAME")).skip(1);
            let errors = after_header.into_iter().find_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                (f.first() == Some(&name.as_str())).then(|| f.get(2..5).map(|t| t.iter().map(|x| count_token(x)).sum::<u64>())).flatten()
            });
            StatusFacts { scan: scan_summary(&text), name, errors }
        })
        .collect()
}

fn first_line(text: &str) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string()
}

/// One probe: `zpool list` first (its duration is the round trip); an SSH-level failure ends it there.
pub fn probe_host(runner: &dyn CommandRunner) -> HostProbe {
    let started = Instant::now();
    let listed = match runner.run("zpool", LIST_ARGS) {
        Ok(out) => out,
        Err(e) => return HostProbe::Unavailable { reason: e.to_string() },
    };
    let rtt_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if listed.status == 255 {
        return HostProbe::Disconnected;
    }
    if !listed.success() {
        let why = first_line(&listed.stderr);
        return HostProbe::Unavailable { reason: if why.is_empty() { format!("zpool list exited {}", listed.status) } else { why } };
    }
    let rows = match parse_pool_list_frag(&listed.stdout) {
        Ok(rows) => rows,
        Err(e) => return HostProbe::Unavailable { reason: e.to_string() },
    };
    let facts = runner.run("zpool", &["status"]).ok().filter(crate::exec::CommandOutput::success).map(|o| parse_status_all(&o.stdout));
    let pools = rows
        .into_iter()
        .map(|(pool, frag)| {
            let status = facts.as_ref().and_then(|all| all.iter().find(|f| f.name == pool.name));
            let capacity = if pool.size == 0 { 0 } else { u8::try_from(u128::from(pool.allocated) * 100 / u128::from(pool.size)).unwrap_or(100).min(100) };
            PoolProbe { name: pool.name, health: pool.health, capacity_percent: capacity, frag_percent: frag, errors: status.and_then(|s| s.errors), scan: status.map(|s| s.scan.clone()) }
        })
        .collect();
    HostProbe::Reached { rtt_ms, pools }
}

fn health_text(health: &PoolHealth) -> String {
    match health {
        PoolHealth::Other(s) => s.clone(),
        other => format!("{other:?}").to_uppercase(),
    }
}

/// The level and the reason to show for a host. The worst applicable level wins, and its reason is the
/// first applicable one, in the order: not ONLINE, 90% full | errors, scrub errors, 80% full, job failed.
pub fn assess(probe: Option<&HostProbe>, job_failed: bool) -> Assessment {
    let say = |level, reason: &str| Assessment { level, reason: reason.to_string() };
    let Some(probe) = probe else { return say(Level::Probing, "probing") };
    let (pools, rtt_known) = match probe {
        HostProbe::Disconnected => return say(Level::Disconnected, "unreachable"),
        HostProbe::Unavailable { reason } => return say(Level::Warning, &format!("cannot inspect: {reason}")),
        HostProbe::Reached { pools, rtt_ms } => (pools, *rtt_ms),
    };
    let _ = rtt_known;
    if let Some(p) = pools.iter().find(|p| p.health != PoolHealth::Online) {
        return say(Level::Critical, &format!("{} {}", p.name, health_text(&p.health)));
    }
    if let Some(p) = pools.iter().find(|p| p.capacity_percent >= 90) {
        return say(Level::Critical, &format!("{} {}% full", p.name, p.capacity_percent));
    }
    if let Some(p) = pools.iter().find(|p| p.errors.is_some_and(|e| e > 0)) {
        return say(Level::Warning, &format!("{} {} errors", p.name, p.errors.unwrap_or(0)));
    }
    if let Some(p) = pools.iter().find(|p| matches!(p.scan, Some(ScanSummary::Finished { errors, .. }) if errors > 0)) {
        return say(Level::Warning, &format!("scrub found errors on {}", p.name));
    }
    if let Some(p) = pools.iter().find(|p| p.capacity_percent >= 80) {
        return say(Level::Warning, &format!("{} {}% full", p.name, p.capacity_percent));
    }
    if job_failed {
        return say(Level::Warning, "job failed");
    }
    say(Level::Ok, "ok")
}
```

Make `PoolHealth::parse` reachable: change `fn parse(s: &str)` in `src/zfs/pool.rs` to `pub fn parse(s: &str)` (it is private today). Remove the `rtt_known` placeholder lines in `assess` if clippy objects (use `HostProbe::Reached { pools, .. } => pools`).

- [ ] **Step 4: Run the tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"; cargo clippy 2>&1 | grep -E "^warning" | grep -v "generated\|never used"`
Expected: all pass (the errors test expects `1 + 3 = 4`: READ 3 plus CKSUM `1.2K` counted as 1).

- [ ] **Step 5: Commit**

```bash
git add src/zfs/host_health.rs src/zfs/mod.rs src/zfs/pool.rs
git commit -m "feat: host probe, zpool status parsing and the health rules

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Extend `sample_host` (CPU, memory, swap, L2ARC, pool rows)

**Files:**
- Modify: `src/zfs/host.rs` — `HostSummary` fields (~line 33), `sample_host` (~line 105), tests.

**Interfaces:**
- Consumes: Task 1 parsers, Task 2 `parse_pool_list_frag`/`parse_status_all`/`PoolProbe`/`ScanSummary`/`HostProbe`.
- Produces: `HostSummary { cpu: Option<CpuInfo>, memory: Option<Memory>, swap: Option<Swap>, l2arc: Option<L2arc>, pool_rows: Vec<PoolProbe>, .. }` with `CpuInfo { model: String, cores: u32, percent: Option<f64> }`; the `pools: Vec<Pool>` field keeps its meaning (the existing consumers do not change).

- [ ] **Step 1: Write the failing tests** — in `mod tests` of `src/zfs/host.rs`, add a scripted runner (the existing `FakeRunner` routes by first argument only, and every `sysctl` call starts with `-n`):

```rust
    use std::sync::Mutex;

    /// Answers each command by the longest registered prefix of `cmd args...`, recording every call.
    struct Scripted {
        answers: Vec<(String, Option<String>)>,
        calls: Mutex<Vec<String>>,
        cp_time: Mutex<Vec<String>>,
    }

    impl Scripted {
        fn new() -> Self {
            Scripted { answers: Vec::new(), calls: Mutex::new(vec![]), cp_time: Mutex::new(vec![]) }
        }
        fn on(mut self, prefix: &str, stdout: &str) -> Self {
            self.answers.push((prefix.to_string(), Some(stdout.to_string())));
            self
        }
        fn failing(mut self, prefix: &str) -> Self {
            self.answers.push((prefix.to_string(), None));
            self
        }
        /// Successive `kern.cp_time` readings.
        fn cpu(self, readings: &[&str]) -> Self {
            *self.cp_time.lock().unwrap() = readings.iter().rev().map(|s| (*s).to_string()).collect();
            self
        }
    }

    impl CommandRunner for Scripted {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<crate::exec::CommandOutput> {
            let line = format!("{cmd} {}", args.join(" "));
            self.calls.lock().unwrap().push(line.clone());
            let ok = |s: String| Ok(crate::exec::CommandOutput { stdout: s, stderr: String::new(), status: 0 });
            if line == "sysctl -n kern.cp_time" {
                return match self.cp_time.lock().unwrap().pop() {
                    Some(s) => ok(s),
                    None => Ok(crate::exec::CommandOutput { stdout: String::new(), stderr: "no".into(), status: 1 }),
                };
            }
            let best = self.answers.iter().filter(|(p, _)| line.starts_with(p.as_str())).max_by_key(|(p, _)| p.len());
            match best {
                Some((_, Some(s))) => ok(s.clone()),
                _ => Ok(crate::exec::CommandOutput { stdout: String::new(), stderr: "unavailable".into(), status: 1 }),
            }
        }
        fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> Result<crate::exec::CommandOutput> {
            self.run(c, a)
        }
        fn run_unprivileged(&self, c: &str, a: &[&str]) -> Result<crate::exec::CommandOutput> {
            self.run(c, a)
        }
    }

    const LIST: &str = "scratch\t251658240\t106573824\t145084416\t39\tONLINE\nzroot\t45634027520\t11182317568\t34451709952\t33\tONLINE\n";
    const STATUS: &str = "  pool: scratch\n state: ONLINE\n  scan: scrub repaired 0B in 00:00:01 with 0 errors on Thu Oct  8 19:48:07 2026\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\tscratch  ONLINE  0  0  0\n\n  pool: zroot\n state: ONLINE\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\tzroot  ONLINE  0  0  0\n";
    const SWAP: &str = "Device          1K-blocks     Used    Avail Capacity\n/dev/ada0p3       2097152   524288  1572864    25%\n";

    fn full_host() -> Scripted {
        Scripted::new()
            .on("zpool list", LIST)
            .on("zpool status", STATUS)
            .on("zpool iostat", "scratch\t1\t2\t0\t0\t0\t0\n")
            .on("uname", "FreeBSD 15.1-RELEASE-p4\n")
            .on("date", "1791500000\n")
            .on("sysctl -n kern.boottime", "{ sec = 1791413600, usec = 1 } Wed\n")
            .on("sysctl -n vm.loadavg", "{ 0.10 0.20 0.30 }\n")
            .on("sysctl -n kstat.zfs.misc.arcstats.size", "100\n200\n30\n10\n")
            .on("sysctl -n hw.model", "Intel(R) Xeon(R) CPU E5-2660 0 @ 2.20GHz\n36\n21418688512\n4096\n2563464\n1706935\n")
            .on("sysctl -n kstat.zfs.misc.arcstats.l2_size", "0\n0\n0\n")
            .on("swapinfo", SWAP)
            .on("zfs version", "zfs-2.4.2-1-FreeBSD\nzfs-kmod-2.4.2-1\n")
            .cpu(&["100 0 100 0 800", "150 0 150 0 900"])
    }

    #[test]
    fn the_sample_carries_cpu_memory_swap_and_pool_rows() {
        let (summary, _) = sample_host(&full_host()).unwrap();
        let cpu = summary.cpu.as_ref().unwrap();
        assert_eq!((cpu.model.as_str(), cpu.cores, cpu.percent), ("Intel(R) Xeon(R) CPU E5-2660 0 @ 2.20GHz", 36, Some(50.0)));
        assert_eq!(summary.memory.unwrap().physical, 21_418_688_512);
        assert_eq!(summary.swap, Some(Swap { total: 2_097_152 * 1024, used: 524_288 * 1024 }));
        assert_eq!(summary.pools.len(), 2, "the existing pools field still works");
        assert_eq!(summary.pool_rows.iter().map(|p| (p.name.as_str(), p.frag_percent)).collect::<Vec<_>>(), [("scratch", Some(39)), ("zroot", Some(33))]);
        assert!(matches!(summary.pool_rows[0].scan, Some(crate::zfs::host_health::ScanSummary::Finished { .. })));
    }

    #[test]
    fn l2arc_is_only_reported_when_a_cache_device_exists() {
        let (none, _) = sample_host(&full_host()).unwrap();
        assert_eq!(none.l2arc, None, "l2_size 0: no cache device, no line");
        let with = full_host().on("sysctl -n kstat.zfs.misc.arcstats.l2_size", "12884901888\n600\n400\n");
        let (summary, _) = sample_host(&with).unwrap();
        assert_eq!(summary.l2arc, Some(L2arc { size: 12_884_901_888, hits: 600, misses: 400 }));
    }

    #[test]
    fn the_cpu_usage_is_measured_around_the_iostat_window_with_no_extra_wait() {
        let host = full_host();
        sample_host(&host).unwrap();
        let calls = host.calls.lock().unwrap().clone();
        let at = |p: &str| calls.iter().position(|c| c.starts_with(p)).unwrap_or_else(|| panic!("no {p} in {calls:?}"));
        let first = calls.iter().position(|c| c == "sysctl -n kern.cp_time").unwrap();
        let last = calls.iter().rposition(|c| c == "sysctl -n kern.cp_time").unwrap();
        assert!(first < at("zpool iostat") && at("zpool iostat") < last, "{calls:?}");
    }

    #[test]
    fn every_new_figure_is_best_effort_and_a_missing_one_leaves_the_rest() {
        let host = full_host().failing("sysctl -n hw.model").failing("swapinfo").failing("sysctl -n kern.cp_time").failing("zpool status");
        let (summary, _) = sample_host(&host).unwrap();
        assert_eq!((summary.cpu.clone(), summary.memory, summary.swap), (None, None, None));
        assert_eq!(summary.pools.len(), 2, "the required part still works");
        assert!(summary.pool_rows.iter().all(|p| p.errors.is_none() && p.scan.is_none()), "no zpool status: unknown, not clean");
        assert_eq!(summary.os.as_deref(), Some("FreeBSD 15.1-RELEASE-p4"));
    }

    #[test]
    fn a_cpu_model_without_a_usage_reading_still_shows_the_model_and_cores() {
        let host = full_host().failing("sysctl -n kern.cp_time");
        let (summary, _) = sample_host(&host).unwrap();
        let cpu = summary.cpu.unwrap();
        assert_eq!((cpu.cores, cpu.percent), (36, None));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test the_sample_carries 2>&1 | grep -E "^error" | head -3`
Expected: compile errors (`summary.cpu` etc. not found).

- [ ] **Step 3: Implement.** In `src/zfs/host.rs`:

1. Add to `HostSummary`:

```rust
    pub cpu: Option<CpuInfo>,
    pub memory: Option<Memory>,
    pub swap: Option<Swap>,
    /// Only when a cache device exists (`l2_size` above 0).
    pub l2arc: Option<L2arc>,
    /// Per-pool detail for the panel (capacity, fragmentation, health, errors, last scrub).
    pub pool_rows: Vec<crate::zfs::host_health::PoolProbe>,
```

and above it `#[derive(Debug, Clone, PartialEq)] pub struct CpuInfo { pub model: String, pub cores: u32, pub percent: Option<f64> }`.

2. Replace the body of `sample_host` with:

```rust
pub fn sample_host(runner: &dyn CommandRunner) -> Result<(HostSummary, PoolIo)> {
    use crate::zfs::host_health::{parse_pool_list_frag, parse_status_all, PoolProbe};
    let rows = parse_pool_list_frag(&run_checked(runner, "zpool", &["list", "-Hp", "-o", "name,size,alloc,free,frag,health"], "zpool list")?.stdout)?;
    let status = stdout_of(runner.run("zpool", &["status"])).map(|s| parse_status_all(&s));
    let pool_rows: Vec<PoolProbe> = rows
        .iter()
        .map(|(pool, frag)| {
            let facts = status.as_ref().and_then(|all| all.iter().find(|f| f.name == pool.name));
            let capacity = if pool.size == 0 { 0 } else { u8::try_from(u128::from(pool.allocated) * 100 / u128::from(pool.size)).unwrap_or(100).min(100) };
            PoolProbe { name: pool.name.clone(), health: pool.health.clone(), capacity_percent: capacity, frag_percent: *frag, errors: facts.and_then(|f| f.errors), scan: facts.map(|f| f.scan.clone()) }
        })
        .collect();
    let pools: Vec<Pool> = rows.into_iter().map(|(p, _)| p).collect();
    let started = Instant::now();
    let os = stdout_of(runner.run_unprivileged("uname", &["-sr"])).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let rtt_ms = os.is_some().then(|| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    let now = stdout_of(runner.run_unprivileged("date", &["+%s"])).and_then(|s| s.trim().parse::<u64>().ok());
    let boot = stdout_of(runner.run_unprivileged("sysctl", &["-n", "kern.boottime"])).and_then(|s| parse_boottime(&s));
    let core = stdout_of(runner.run_unprivileged(
        "sysctl",
        &["-n", "hw.model", "hw.ncpu", "hw.physmem", "hw.pagesize", "vm.stats.vm.v_free_count", "vm.stats.vm.v_wire_count"],
    ))
    .and_then(|s| parse_core_sysctls(&s));
    let l2arc = stdout_of(runner.run_unprivileged("sysctl", &["-n", "kstat.zfs.misc.arcstats.l2_size", "kstat.zfs.misc.arcstats.l2_hits", "kstat.zfs.misc.arcstats.l2_misses"]))
        .and_then(|s| parse_l2arc(&s))
        .filter(|l| l.size > 0);
    let summary_without_cpu = HostSummary {
        os,
        zfs_version: stdout_of(runner.run("zfs", &["version"])).and_then(|s| parse_zfs_version(&s)),
        uptime_secs: now.zip(boot).map(|(n, b)| n.saturating_sub(b)),
        load: stdout_of(runner.run_unprivileged("sysctl", &["-n", "vm.loadavg"])).and_then(|s| parse_loadavg(&s)),
        arc: stdout_of(runner.run_unprivileged(
            "sysctl",
            &["-n", "kstat.zfs.misc.arcstats.size", "kstat.zfs.misc.arcstats.c_max", "kstat.zfs.misc.arcstats.hits", "kstat.zfs.misc.arcstats.misses"],
        ))
        .and_then(|s| parse_arc(&s)),
        pools,
        status: stdout_of(runner.run("zpool", &["status", "-x"])).and_then(|s| s.lines().next().map(|l| l.trim().to_string())),
        rtt_ms,
        cpu: None,
        memory: core.as_ref().map(|c| c.memory),
        swap: stdout_of(runner.run_unprivileged("swapinfo", &["-k"])).and_then(|s| parse_swapinfo(&s)),
        l2arc,
        pool_rows,
    };
    // The CPU usage needs two readings; the 1-second iostat window below sits between them, so the
    // measurement costs no extra waiting.
    let cpu_before = stdout_of(runner.run_unprivileged("sysctl", &["-n", "kern.cp_time"])).and_then(|s| parse_cpu_times(&s));
    let io = stdout_of(runner.run("zpool", &["iostat", "-Hpy", "1", "1"])).map(|s| parse_iostat_total(&s)).unwrap_or_default();
    let cpu_after = stdout_of(runner.run_unprivileged("sysctl", &["-n", "kern.cp_time"])).and_then(|s| parse_cpu_times(&s));
    let percent = cpu_before.zip(cpu_after).and_then(|(a, b)| cpu_percent(a, b));
    let summary = HostSummary { cpu: core.map(|c| CpuInfo { model: c.model, cores: c.cores, percent }), ..summary_without_cpu };
    Ok((summary, io))
}
```

3. Existing code that builds a `HostSummary { .. }` literal elsewhere (search `HostSummary {`): add `..Default::default()` or the new fields (`HostSummary` derives `Default`).

- [ ] **Step 4: Run all tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"; cargo clippy 2>&1 | grep -E "^warning" | grep -v "generated\|never used"`
Expected: all pass; the pre-existing `sample_host` tests still pass.

- [ ] **Step 5: Commit**

```bash
git add -A src
git commit -m "feat: host sample carries CPU, memory, swap, L2ARC and per-pool rows

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: App state, scheduling and the event-loop hook

**Files:**
- Modify: `src/ui/state.rs` — new `WorkerMsg::HostHealth`, fields, methods, drain arm, tests.
- Modify: `src/ui/mod.rs` — address setup after `App::new(...)`, `poll_host_probes`, address on add-host (`apply_pending_new_hosts`).

**Interfaces:**
- Consumes: `host_health::{HostProbe, probe_host, assess, Assessment}`, `jobs_view.jobs` / `last_run`.
- Produces: `App::host_addresses: Vec<String>`, `App::set_host_addresses(Vec<String>)`, `App::host_address(i) -> &str`, `pub fn address_of(ssh_target: Option<&str>) -> String` (in `src/ui/mod.rs` or `hosts.rs`), `App::host_probes: HashMap<String, HostProbeState>`, `HostProbeState { last: Option<HostProbe>, in_flight: bool, finished_at: Option<Instant> }`, `App::due_host_probes(now) -> Vec<usize>`, `App::start_host_probe(index, runner)`, `App::host_assessment(index) -> Assessment`, constants `HOST_PROBE_INTERVAL = 30 s`, `HOST_PROBE_STAGGER = 1 s`.

- [ ] **Step 1: Write the failing tests** — append in `mod tests` of `src/ui/state.rs`:

```rust
    fn probes_app() -> App {
        App::new(vec!["local".to_string(), "fbsd16".to_string(), "fbsd16t1".to_string()])
    }

    #[test]
    fn address_of_strips_the_user_and_names_local_for_the_hub() {
        assert_eq!(address_of(None), "local");
        assert_eq!(address_of(Some("fbsd@172.16.10.61")), "172.16.10.61");
        assert_eq!(address_of(Some("172.16.10.62")), "172.16.10.62");
        assert_eq!(address_of(Some("root@nas.example.com")), "nas.example.com");
        assert_eq!(address_of(Some("")), "-");
    }

    #[test]
    fn addresses_default_to_local_and_a_dash_and_follow_set_host_addresses() {
        let mut app = probes_app();
        assert_eq!((app.host_address(0), app.host_address(1)), ("local", "-"));
        app.set_host_addresses(vec!["local".into(), "172.16.10.60".into(), "172.16.10.61".into()]);
        assert_eq!(app.host_address(1), "172.16.10.60");
        assert_eq!(app.host_address(9), "-", "an index with no host");
    }

    #[test]
    fn nothing_is_due_before_the_stagger_and_every_host_is_due_once_it_has_passed() {
        let app = probes_app();
        let t0 = app.probe_epoch;
        assert_eq!(app.due_host_probes(t0), vec![0], "host 0 immediately, the others one second apart");
        assert_eq!(app.due_host_probes(t0 + Duration::from_secs(1)), vec![0, 1]);
        assert_eq!(app.due_host_probes(t0 + Duration::from_secs(5)), vec![0, 1, 2]);
    }

    #[test]
    fn a_host_is_not_probed_again_while_a_probe_runs_or_before_the_interval() {
        let mut app = probes_app();
        let t0 = app.probe_epoch;
        app.start_host_probe(0, std::sync::Arc::new(FakeRunner::new("")));
        assert!(app.host_probes["local"].in_flight);
        assert!(!app.due_host_probes(t0 + Duration::from_secs(600)).contains(&0), "one probe in flight per host");
        wait_until(&mut app, |a| !a.host_probes["local"].in_flight);
        let done = app.host_probes["local"].finished_at.unwrap();
        assert!(!app.due_host_probes(done + Duration::from_secs(29)).contains(&0), "not before the 30 s interval");
        assert!(app.due_host_probes(done + Duration::from_secs(30)).contains(&0), "due again after it");
    }

    #[test]
    fn a_probe_result_is_stored_for_its_host_and_assessed() {
        let mut app = probes_app();
        app.tx.send(WorkerMsg::HostHealth("fbsd16".to_string(), crate::zfs::host_health::HostProbe::Disconnected)).unwrap();
        app.drain_worker_messages();
        let a = app.host_assessment(1);
        assert_eq!((a.level, a.reason.as_str()), (crate::zfs::host_health::Level::Disconnected, "unreachable"));
        assert_eq!(app.host_assessment(2).level, crate::zfs::host_health::Level::Probing, "never probed");
    }

    #[test]
    fn a_result_for_a_host_that_no_longer_exists_is_discarded() {
        let mut app = probes_app();
        app.tx.send(WorkerMsg::HostHealth("ghost".to_string(), crate::zfs::host_health::HostProbe::Disconnected)).unwrap();
        app.drain_worker_messages();
        assert!(!app.host_probes.contains_key("ghost"));
    }

    #[test]
    fn a_failed_job_of_that_host_warns_only_for_an_enabled_job_whose_latest_run_failed() {
        use crate::daemon::history::{Event, EventKind, History, RunResult};
        let dir = tempfile::tempdir().unwrap();
        let mut a = scrub_job("a");
        a.host = "fbsd16".to_string();
        let mut off = scrub_job("off");
        off.host = "fbsd16t1".to_string();
        off.enabled = false;
        let mut other = scrub_job("other");
        other.host = "local".to_string();
        crate::jobs::save(&dir.path().join("jobs.toml"), &[a, off, other]).unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        let ev = |id: &str, run: &str, at: &str, result| Event { job_id: id.into(), run_id: run.into(), event: EventKind::Finished, at: chrono::DateTime::parse_from_rfc3339(at).unwrap().with_timezone(&chrono::Utc), scheduled_for: None, result: Some(result), message: String::new(), catch_up: false, summary: None };
        h.append(&ev("a", "a1", "2026-10-09T01:00:00Z", RunResult::Failed)).unwrap();
        h.append(&ev("off", "o1", "2026-10-09T01:00:00Z", RunResult::Failed)).unwrap();
        h.append(&ev("other", "x1", "2026-10-09T01:00:00Z", RunResult::Ok)).unwrap();
        let mut app = probes_app();
        app.state_dir = dir.path().to_path_buf();
        app.refresh_jobs();
        let healthy = || crate::zfs::host_health::HostProbe::Reached { rtt_ms: 1, pools: vec![] };
        for name in ["local", "fbsd16", "fbsd16t1"] {
            app.tx.send(WorkerMsg::HostHealth(name.to_string(), healthy())).unwrap();
        }
        app.drain_worker_messages();
        assert_eq!(app.host_assessment(1).reason, "job failed", "fbsd16 has an enabled job whose latest run failed");
        assert_eq!(app.host_assessment(2).reason, "ok", "its only failing job is disabled");
        assert_eq!(app.host_assessment(0).reason, "ok", "another host's failure is not this host's");
        h.append(&ev("a", "a2", "2026-10-09T02:00:00Z", RunResult::Ok)).unwrap();
        app.refresh_jobs();
        assert_eq!(app.host_assessment(1).reason, "ok", "a later success clears it");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test address_of 2>&1 | grep -E "^error" | head -3`
Expected: compile errors (`address_of`, `host_address`, `due_host_probes`, `WorkerMsg::HostHealth` …).

- [ ] **Step 3: Implement.**

`src/ui/state.rs`:
1. `use std::collections::HashMap;` if not present. Add constants near the other durations: `const HOST_PROBE_INTERVAL: Duration = Duration::from_secs(30); const HOST_PROBE_STAGGER: Duration = Duration::from_secs(1);`.
2. Add `HostHealth(String, crate::zfs::host_health::HostProbe),` to `enum WorkerMsg` (the existing `HostProbe(String, String, Result<()>)` is the Add Host reachability check: do not touch it).
3. Add the struct and free fn:

```rust
/// What the probe loop knows about one host.
#[derive(Debug, Default)]
pub struct HostProbeState {
    pub last: Option<crate::zfs::host_health::HostProbe>,
    pub in_flight: bool,
    pub finished_at: Option<Instant>,
}

/// The address shown for a host: the host part of its `ssh_target` as configured (no lookup), `local` for the hub.
pub fn address_of(ssh_target: Option<&str>) -> String {
    match ssh_target {
        None => "local".to_string(),
        Some(t) => {
            let host = t.rsplit_once('@').map_or(t, |(_, h)| h).trim();
            if host.is_empty() { "-".to_string() } else { host.to_string() }
        }
    }
}
```

4. App fields (with doc comments) and init: `pub host_addresses: Vec<String>` (init: `names.iter().map(|n| if n == "local" { "local".to_string() } else { "-".to_string() }).collect()`), `pub host_probes: HashMap<String, HostProbeState>` (empty), `pub probe_epoch: Instant` (`Instant::now()`).
5. Methods on `App`:

```rust
    pub fn set_host_addresses(&mut self, addresses: Vec<String>) {
        self.host_addresses = addresses;
    }

    pub fn host_address(&self, index: usize) -> &str {
        self.host_addresses.get(index).map_or("-", String::as_str)
    }

    /// Hosts whose probe is due: never probed (host `i` becomes due `i` seconds after start, so the hosts are
    /// not all probed in the same second) or last finished at least the interval ago; never while one is running.
    pub fn due_host_probes(&self, now: Instant) -> Vec<usize> {
        self.hosts
            .iter()
            .enumerate()
            .filter(|(i, name)| {
                let state = self.host_probes.get(*name);
                if state.is_some_and(|s| s.in_flight) {
                    return false;
                }
                match state.and_then(|s| s.finished_at) {
                    None => now >= self.probe_epoch + HOST_PROBE_STAGGER * u32::try_from(*i).unwrap_or(u32::MAX),
                    Some(done) => now.duration_since(done) >= HOST_PROBE_INTERVAL,
                }
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Starts one probe on a worker thread (the event loop resolves the runner; `App` cannot).
    pub fn start_host_probe(&mut self, index: usize, runner: Arc<dyn CommandRunner>) {
        let Some(name) = self.hosts.get(index).cloned() else { return };
        self.host_probes.entry(name.clone()).or_default().in_flight = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let probe = crate::zfs::host_health::probe_host(runner.as_ref());
            let _ = tx.send(WorkerMsg::HostHealth(name, probe));
        });
    }

    /// The health of host `index` from its latest probe and from the Jobs data (an enabled job of this host
    /// whose latest run failed).
    pub fn host_assessment(&self, index: usize) -> crate::zfs::host_health::Assessment {
        let Some(name) = self.hosts.get(index) else { return crate::zfs::host_health::assess(None, false) };
        let job_failed = self.jobs_view.jobs.iter().any(|j| j.enabled && &j.host == name && self.jobs_view.last_run(&j.id).is_some_and(|r| r.result == Some(crate::daemon::history::RunResult::Failed)));
        crate::zfs::host_health::assess(self.host_probes.get(name).and_then(|s| s.last.as_ref()), job_failed)
    }
```

6. In `drain_worker_messages` add the arm (before `WorkerMsg::MutationDone`):

```rust
                WorkerMsg::HostHealth(name, probe) => {
                    // A result for a host that was removed or renamed while the probe ran is dropped.
                    if self.hosts.contains(&name) {
                        let state = self.host_probes.entry(name).or_default();
                        state.in_flight = false;
                        state.last = Some(probe);
                        state.finished_at = Some(Instant::now());
                    }
                }
```

`src/ui/mod.rs`:
1. Re-export/define `address_of` use: `use crate::ui::state::address_of;` (or call `state::address_of`). After the `App` is created in `run`/where `App::new(` is called (find with `grep -n "App::new" src/ui/mod.rs`) and the inventory exists, call `app.set_host_addresses(inventory.hosts.iter().map(|h| state::address_of(h.ssh_target.as_deref())).collect());`.
2. In `apply_pending_new_hosts`, after `app.hosts.push(name.clone());` add `app.host_addresses.push(state::address_of(Some(&target)));` BEFORE `target` is moved into `apply_new_host` (clone it first).
3. Add:

```rust
/// Starts the host health probes that are due (one worker per host, never two at once for a host).
fn poll_host_probes(app: &mut App, inventory: &HostInventory) {
    for index in app.due_host_probes(Instant::now()) {
        if let Some(host) = inventory.hosts.get(index) {
            app.start_host_probe(index, host.runner.clone());
        }
    }
}
```
and call `poll_host_probes(app, inventory);` in `event_loop` next to `apply_pending_pool_fetches(app, inventory);`.

- [ ] **Step 4: Run all tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"; cargo clippy 2>&1 | grep -E "^warning" | grep -v "generated\|never used"`
Expected: all pass; the existing `apply_pending_new_hosts_applies_every_queued_host_keeping_both_lists_aligned` test still passes (extend it to assert `host_addresses` stays aligned: one extra assert per added host).

- [ ] **Step 5: Commit**

```bash
git add -A src
git commit -m "feat: probe every host in the background and assess its health

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: The Hosts table

**Files:**
- Modify: `src/ui/layout.rs` — `draw_hosts` (~line 219) and tests.

**Interfaces:**
- Consumes: `App::host_assessment`, `App::host_address`, `App::host_probes`, `host_health::{Level, HostProbe}`.
- Produces: `draw_hosts` renders the table described in the spec.

- [ ] **Step 1: Write the failing tests** — append in `mod tests` of `src/ui/layout.rs` (it has `render`, `buffer_of`):

```rust
    fn hosts_app() -> App {
        let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string(), "fbsd16t1".to_string(), "deadbox".to_string()]);
        app.set_host_addresses(vec!["local".into(), "172.16.10.60".into(), "172.16.10.61".into(), "172.16.10.99".into()]);
        let probe = |p: crate::zfs::host_health::HostProbe| crate::ui::state::HostProbeState { last: Some(p), in_flight: false, finished_at: Some(std::time::Instant::now()) };
        let pool = |n: &str, h: crate::zfs::pool::PoolHealth, c: u8| crate::zfs::host_health::PoolProbe { name: n.to_string(), health: h, capacity_percent: c, frag_percent: None, errors: Some(0), scan: None };
        use crate::zfs::host_health::HostProbe;
        app.host_probes.insert("local".into(), probe(HostProbe::Reached { rtt_ms: 3, pools: vec![pool("zroot", crate::zfs::pool::PoolHealth::Online, 20)] }));
        app.host_probes.insert("fbsd16".into(), probe(HostProbe::Reached { rtt_ms: 12, pools: vec![pool("tank", crate::zfs::pool::PoolHealth::Degraded, 20)] }));
        app.host_probes.insert("fbsd16t1".into(), probe(HostProbe::Reached { rtt_ms: 7, pools: vec![pool("tank", crate::zfs::pool::PoolHealth::Online, 85)] }));
        app.host_probes.insert("deadbox".into(), probe(HostProbe::Disconnected));
        app
    }

    /// The foreground color of the first cell of `needle` on the first row that contains it.
    fn fg_of(app: &mut App, needle: &str, w: u16, h: u16) -> Option<ratatui::style::Color> {
        let rows = render(app, w, h);
        let buf = buffer_of(app, w, h);
        for (y, row) in rows.iter().enumerate() {
            if let Some(x) = row.find(needle) {
                let col = row[..x].chars().count();
                return buf[(u16::try_from(col).ok()?, u16::try_from(y).ok()?)].fg.into();
            }
        }
        None
    }

    #[test]
    fn the_hosts_table_shows_name_address_state_and_rtt() {
        let mut app = hosts_app();
        app.selected_host = 0;
        let text = render(&mut app, 120, 30).join("\n");
        for want in ["Name", "Address", "State", "Rtt", "fbsd16", "172.16.10.60", "tank DEGRADED", "tank 85% full", "unreachable", "12ms", "ok"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        assert!(text.contains("local"), "the hub's address column says local");
    }

    #[test]
    fn health_colors_the_marker_and_name_and_the_unreachable_host_is_red() {
        let mut app = hosts_app();
        app.selected_host = 0;
        let theme = app.theme.clone();
        assert_eq!(fg_of(&mut app, "fbsd16 ", 120, 30), theme.err.fg, "a degraded pool is red");
        assert_eq!(fg_of(&mut app, "fbsd16t1", 120, 30), theme.warn.fg, "85% full is yellow");
        assert_eq!(fg_of(&mut app, "deadbox", 120, 30), theme.err.fg, "unreachable is red");
    }

    #[test]
    fn a_host_that_has_not_been_probed_is_dim_and_says_probing() {
        let mut app = App::new(vec!["local".to_string(), "new".to_string()]);
        let text = render(&mut app, 120, 30).join("\n");
        assert!(text.contains("probing"), "{text}");
    }

    #[test]
    fn narrow_terminals_drop_rtt_then_address_but_keep_name_and_state() {
        let mut app = hosts_app();
        let wide = render(&mut app, 140, 30).join("\n");
        assert!(wide.contains("Rtt") && wide.contains("Address"));
        let mid = render(&mut app, 70, 30).join("\n");
        assert!(!mid.contains("Rtt") && mid.contains("Address") && mid.contains("State"), "{mid}");
        let narrow = render(&mut app, 50, 30).join("\n");
        assert!(!narrow.contains("Address") && narrow.contains("Name") && narrow.contains("State") && narrow.contains("unreachable"), "{narrow}");
    }

    #[test]
    fn the_single_host_hint_and_the_selection_are_kept() {
        let mut app = App::new(vec!["local".to_string()]);
        let text = render(&mut app, 120, 30).join("\n");
        assert!(text.contains("Only this host is configured"), "{text}");
        let mut many = hosts_app();
        many.selected_host = 1;
        let buf = buffer_of(&mut many, 120, 30);
        let rows = render(&mut many, 120, 30);
        let y = rows.iter().position(|r| r.contains("172.16.10.60")).unwrap();
        let x = rows[y].find("172.16.10.60").unwrap();
        let selected = buf[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())].bg;
        let other = {
            let y2 = rows.iter().position(|r| r.contains("172.16.10.61")).unwrap();
            buf[(u16::try_from(x).unwrap(), u16::try_from(y2).unwrap())].bg
        };
        assert_ne!(selected, other, "the selected host's row looks selected");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test the_hosts_table 2>&1 | grep -E "panicked|FAILED" | head -3`
Expected: FAIL (names only today).

- [ ] **Step 3: Implement.** Replace `draw_hosts` in `src/ui/layout.rs` with:

```rust
fn draw_hosts(f: &mut Frame, app: &App, area: Rect) {
    use crate::zfs::host_health::{HostProbe, Level};
    let theme = app.theme.clone();
    // Name and State always show; Address then Rtt only while they fit (Rtt is the first to go).
    let inner = area.width.saturating_sub(2);
    let (name_w, addr_w, rtt_w, state_min) = (16_u16, 18_u16, 6_u16, 22_u16);
    let show_addr = inner >= name_w + state_min + addr_w + 2;
    let show_rtt = show_addr && inner >= name_w + state_min + addr_w + rtt_w + 3;
    let rows: Vec<Row> = app
        .hosts
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let assessment = app.host_assessment(i);
            let (marker_style, name_style, state_style) = match assessment.level {
                Level::Ok => (theme.ok, Style::default(), Style::default()),
                Level::Warning => (theme.warn, theme.warn, theme.warn),
                Level::Critical | Level::Disconnected => (theme.err, theme.err, theme.err),
                Level::Probing => (theme.dim(), theme.dim(), theme.dim()),
            };
            let rtt = match app.host_probes.get(name).and_then(|s| s.last.as_ref()) {
                Some(HostProbe::Reached { rtt_ms, .. }) => format!("{rtt_ms}ms"),
                _ => "-".to_string(),
            };
            let mut cells = vec![Cell::from(Line::from(vec![Span::styled("● ", marker_style), Span::styled(name.clone(), name_style)]))];
            if show_addr {
                cells.push(Cell::from(app.host_address(i).to_string()));
            }
            cells.push(Cell::from(Span::styled(assessment.reason, state_style)));
            if show_rtt {
                cells.push(Cell::from(rtt));
            }
            let row = Row::new(cells);
            if i == app.selected_host { row.style(theme.selected_row()) } else { row }
        })
        .collect();
    let mut widths = vec![Constraint::Length(name_w)];
    let mut headers = vec!["Name"];
    if show_addr {
        widths.push(Constraint::Length(addr_w));
        headers.push("Address");
    }
    widths.push(Constraint::Min(state_min));
    headers.push("State");
    if show_rtt {
        widths.push(Constraint::Length(rtt_w));
        headers.push("Rtt");
    }
    let table_area = if app.hosts.len() == 1 {
        let [table_part, hint_part] = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(0), Constraint::Length(4)]).areas(area);
        let hint = Paragraph::new("Only this host is configured. Press 'n' to add another host.".to_string()).wrap(Wrap { trim: false }).style(theme.dim());
        f.render_widget(hint, hint_part);
        table_part
    } else {
        area
    };
    let table = Table::new(rows, widths)
        .header(Row::new(headers))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title("Hosts"));
    f.render_widget(table, table_area);
}
```

Add `Cell` to the `ratatui::widgets` imports of `layout.rs` if it is not there.

- [ ] **Step 4: Run all tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"; cargo clippy 2>&1 | grep -E "^warning" | grep -v "generated\|never used"`
Expected: all pass (existing Hosts tests that assert the plain list adapt: the host names are still present).

- [ ] **Step 5: Commit**

```bash
git add -A src
git commit -m "feat: the Hosts list is a table with address, health color, state and rtt

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 6: The bottom panel

**Files:**
- Modify: `src/ui/metrics.rs` — `host_lines` (~line 298) replaced by `host_panel`, the host branch of `draw_body` (~line 387), the title in `draw` (~line 368), tests.

**Interfaces:**
- Consumes: Task 3's `HostSummary` fields.
- Produces: `fn host_panel(h: &HostSummary, theme: &Theme, stale: bool) -> HostPanel { system: Vec<Line<'static>>, zfs: Vec<Line<'static>>, pools: Vec<Line<'static>> }` and `fn draw_host_panel(f, view, last, host, theme, area)`.

- [ ] **Step 1: Write the failing tests** — in `mod tests` of `src/ui/metrics.rs`:

```rust
    use crate::zfs::host::{CpuInfo, L2arc, Memory, Swap};
    use crate::zfs::host_health::{PoolProbe, ScanSummary};
    use crate::zfs::pool::PoolHealth;

    fn rich_host() -> HostSummary {
        HostSummary {
            os: Some("FreeBSD 15.1-RELEASE-p4".into()),
            zfs_version: Some("2.4.2-1".into()),
            uptime_secs: Some(90_000),
            load: Some([0.1, 0.2, 0.3]),
            rtt_ms: Some(4),
            status: Some("all pools are healthy".into()),
            cpu: Some(CpuInfo { model: "Intel(R) Xeon(R) CPU E5-2660 0 @ 2.20GHz".into(), cores: 36, percent: Some(12.4) }),
            memory: Some(Memory { physical: 21_418_688_512, free: 10_000_000_000, wired: 7_000_000_000 }),
            swap: Some(Swap { total: 2_147_483_648, used: 524_288 }),
            l2arc: Some(L2arc { size: 12_884_901_888, hits: 600, misses: 400 }),
            arc: Some(crate::zfs::host::ArcStats { size: 1 << 30, max: 2 << 30, hits: 90, misses: 10 }),
            pools: vec![],
            pool_rows: vec![
                PoolProbe { name: "scratch".into(), health: PoolHealth::Online, capacity_percent: 42, frag_percent: Some(39), errors: Some(0), scan: Some(ScanSummary::Finished { when: "Thu Oct  8 19:48:07 2026".into(), errors: 0 }) },
                PoolProbe { name: "tank".into(), health: PoolHealth::Degraded, capacity_percent: 91, frag_percent: None, errors: Some(3), scan: Some(ScanSummary::Running { percent: Some(34) }) },
                PoolProbe { name: "zroot".into(), health: PoolHealth::Online, capacity_percent: 24, frag_percent: Some(33), errors: Some(0), scan: Some(ScanSummary::Never) },
            ],
        }
    }

    fn text_of(lines: &[Line]) -> String {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn the_panel_names_cpu_memory_swap_and_l2arc() {
        let panel = host_panel(&rich_host(), &Theme::ansi(), false);
        let system = text_of(&panel.system);
        for want in ["Intel(R) Xeon(R) CPU E5-2660 0 @ 2.20GHz", "36 cores", "12%", "Memory", "20.0G", "Swap", "512K / 2.0G"] {
            assert!(system.contains(want), "missing {want:?} in\n{system}");
        }
        let zfs = text_of(&panel.zfs);
        assert!(zfs.contains("L2ARC") && zfs.contains("12.0G") && zfs.contains("60.0%"), "{zfs}");
    }

    #[test]
    fn missing_figures_show_a_dash_and_l2arc_is_absent_without_a_cache_device() {
        let mut h = rich_host();
        h.cpu = None;
        h.memory = None;
        h.swap = None;
        h.l2arc = None;
        let panel = host_panel(&h, &Theme::ansi(), false);
        let all = format!("{}\n{}", text_of(&panel.system), text_of(&panel.zfs));
        assert!(!all.contains("L2ARC"), "{all}");
        assert!(all.contains("CPU     -") && all.contains("Memory  -") && all.contains("Swap    none"), "{all}");
    }

    #[test]
    fn each_pool_gets_a_row_with_its_capacity_fragmentation_health_and_last_scrub() {
        let panel = host_panel(&rich_host(), &Theme::ansi(), false);
        let rows = text_of(&panel.pools);
        for want in ["scratch", "42%", "39%", "ONLINE", "Thu Oct  8 19:48:07 2026", "tank", "91%", "DEGRADED", "running 34%", "never"] {
            assert!(rows.contains(want), "missing {want:?} in\n{rows}");
        }
        assert!(rows.contains("tank") && rows.lines().any(|l| l.contains("tank") && l.contains(" - ")), "an unknown fragmentation is a dash:\n{rows}");
    }

    #[test]
    fn a_pool_that_is_not_online_or_has_scrub_errors_is_drawn_in_the_error_color() {
        let theme = Theme::ansi();
        let panel = host_panel(&rich_host(), &theme, false);
        let tank = panel.pools.iter().find(|l| text_of(std::slice::from_ref(l)).contains("tank")).unwrap();
        assert!(tank.spans.iter().any(|s| s.style == theme.err), "{tank:?}");
        let scratch = panel.pools.iter().find(|l| text_of(std::slice::from_ref(l)).contains("scratch")).unwrap();
        assert!(!scratch.spans.iter().any(|s| s.style == theme.err));
    }

    #[test]
    fn a_stale_panel_is_drawn_dim() {
        let theme = Theme::ansi();
        let fresh = host_panel(&rich_host(), &theme, false);
        let stale = host_panel(&rich_host(), &theme, true);
        assert!(stale.system.iter().all(|l| l.spans.iter().all(|s| s.style.add_modifier.contains(ratatui::style::Modifier::DIM) || s.style == theme.dim())), "{:?}", stale.system);
        assert!(!fresh.system.iter().all(|l| l.spans.iter().all(|s| s.style == theme.dim())));
    }
```

and in `src/ui/layout.rs` tests (it has `render`, `app_in_pools`-style helpers):

```rust
    fn host_panel_app(w_extra: ()) -> App {
        let _ = w_extra;
        let mut app = App::new(vec!["local".to_string()]);
        let target = MetricsTarget::Host;
        app.metrics.retarget(Some(target.clone()));
        app.metrics.apply(&target, Ok(crate::zfs::metrics::TargetSample { pool: crate::zfs::metrics::PoolSample::default(), space: None, host: Some(crate::ui::metrics::tests_support::rich_host()), snapshot: None, disk: None, share: None }), std::time::Instant::now());
        app
    }

    #[test]
    fn the_host_panel_shows_two_columns_when_wide_and_one_when_narrow_and_says_when_it_cuts_pool_rows() {
        let mut wide = host_panel_app(());
        let text = render(&mut wide, 190, 40).join("\n");
        for want in ["Metrics — host local", "CPU", "36 cores", "Memory", "Swap", "L2ARC", "scratch", "tank", "zroot"] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        let mut narrow = host_panel_app(());
        let text = render(&mut narrow, 90, 40).join("\n");
        assert!(text.contains("CPU") && text.contains("Swap"), "one column still shows everything:\n{text}");
        let mut short = host_panel_app(());
        let text = render(&mut short, 190, 24).join("\n");
        assert!(text.contains("more pool row") || text.contains("scratch"), "never silently clipped:\n{text}");
    }
```

(Expose the fixture to the layout test by moving `rich_host` into a `#[cfg(test)] pub(crate) mod tests_support` in `src/ui/metrics.rs` and calling it from both modules.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test the_panel_names 2>&1 | grep -E "^error" | head -3`
Expected: compile errors (`host_panel` not found).

- [ ] **Step 3: Implement.** In `src/ui/metrics.rs`:

1. Replace `host_lines` with:

```rust
pub struct HostPanel {
    pub system: Vec<Line<'static>>,
    pub zfs: Vec<Line<'static>>,
    pub pools: Vec<Line<'static>>,
}

fn scan_text(scan: Option<&ScanSummary>) -> (String, bool) {
    match scan {
        None => ("-".to_string(), false),
        Some(ScanSummary::Never) => ("never".to_string(), false),
        Some(ScanSummary::Running { percent: Some(p) }) => (format!("running {p}%"), false),
        Some(ScanSummary::Running { percent: None }) => ("running".to_string(), false),
        Some(ScanSummary::Canceled) => ("canceled".to_string(), false),
        Some(ScanSummary::Finished { when, errors: 0 }) => (when.clone(), false),
        Some(ScanSummary::Finished { when, errors }) => (format!("{when}, found {errors} errors"), true),
    }
}

/// The Hosts panel's text, split into the System and ZFS columns and the per-pool rows. A stale panel (the
/// last sample failed) is drawn dim.
pub fn host_panel(h: &HostSummary, theme: &Theme, stale: bool) -> HostPanel {
    let dash = || "-".to_string();
    let dim = |s: Style| if stale { theme.dim() } else { s };
    let (alloc, size) = h.capacity();
    let unhealthy = h.unhealthy();
    let pools = if unhealthy == 0 { format!("{} (all ONLINE)", h.pools.len()) } else { format!("{}, {unhealthy} not ONLINE", h.pools.len()) };
    let arc = h.arc.as_ref().map_or_else(dash, |a| {
        let hit = a.hit_percent().map_or_else(dash, |p| format!("{p:.1}%"));
        format!("{} / {}  hit {hit}", human_size(a.size), human_size(a.max))
    });
    let cpu = h.cpu.as_ref().map_or_else(dash, |c| {
        let usage = c.percent.map_or_else(String::new, |p| format!("  {p:.0}%"));
        format!("{} ({} cores){usage}", c.model, c.cores)
    });
    let memory = h.memory.as_ref().map_or_else(dash, |m| format!("{}  free {}  wired {}", human_size(m.physical), human_size(m.free), human_size(m.wired)));
    let swap = h.swap.as_ref().map_or_else(|| "none".to_string(), |s| format!("{} / {}", human_size(s.used), human_size(s.total)));
    let status = h.status.clone().unwrap_or_else(dash);
    let status_style = if status.contains("healthy") { Style::default() } else { theme.warn };
    let line = |style: Style, text: String| Line::from(Span::styled(text, dim(style)));
    let system = vec![
        line(Style::default(), format!("OS      {}", h.os.clone().unwrap_or_else(dash))),
        line(Style::default(), format!("Up      {}   rtt {}", h.uptime_secs.map_or_else(dash, duration_text), h.rtt_ms.map_or_else(dash, |ms| format!("{ms}ms")))),
        line(Style::default(), format!("Load    {}", h.load.map_or_else(dash, |[a, b, c]| format!("{a:.2} {b:.2} {c:.2}")))),
        line(Style::default(), format!("CPU     {cpu}")),
        line(Style::default(), format!("Memory  {memory}")),
        line(Style::default(), format!("Swap    {swap}")),
    ];
    let mut zfs = vec![
        line(Style::default(), format!("ZFS     {}", h.zfs_version.clone().unwrap_or_else(dash))),
        line(if unhealthy == 0 { Style::default() } else { theme.err }, format!("Pools   {pools}")),
        line(Style::default(), format!("Space   {}  {} / {}", percent(alloc, size), human_size(alloc), human_size(size))),
        line(Style::default(), format!("ARC     {arc}")),
    ];
    if let Some(l2) = &h.l2arc {
        let hit = l2.hit_percent().map_or_else(dash, |p| format!("{p:.1}%"));
        zfs.push(line(Style::default(), format!("L2ARC   {}  hit {hit}", human_size(l2.size))));
    }
    zfs.push(line(status_style, format!("Status  {status}")));
    let rows = h
        .pool_rows
        .iter()
        .map(|p| {
            let (scrub, scrub_bad) = scan_text(p.scan.as_ref());
            let frag = p.frag_percent.map_or_else(dash, |f| format!("{f}%"));
            let bad = p.health != PoolHealth::Online || p.errors.is_some_and(|e| e > 0) || scrub_bad;
            let health = format!("{:?}", p.health).to_uppercase();
            line(if bad { theme.err } else if p.capacity_percent >= 80 { theme.warn } else { Style::default() }, format!("{:<14} {:>4}%  frag {:>4}  {:<9} scrub {scrub}", p.name, p.capacity_percent, frag, health))
        })
        .collect();
    HostPanel { system, zfs, pools: rows }
}

/// Draws the host panel: the two graphs (20% each) and the text (60%): System and ZFS side by side when the
/// text is wide enough, stacked otherwise, then one row per pool. Pool rows that do not fit are replaced by a
/// final "+N more pool rows" line, never clipped silently.
fn draw_host_panel(f: &mut Frame, view: &MetricsView, last: &PoolIo, host: &HostSummary, stale: bool, theme: &Theme, area: Rect) {
    let [read, write, text] = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(20), Constraint::Percentage(20), Constraint::Percentage(60)]).areas(area);
    let reads: Vec<u64> = view.io_history.iter().map(|s| s.read_bps).collect();
    let writes: Vec<u64> = view.io_history.iter().map(|s| s.write_bps).collect();
    draw_rate(f, read, "Read ", &reads, last.read_ops, last.read_latency_ns, theme.info);
    draw_rate(f, write, "Write", &writes, last.write_ops, last.write_latency_ns, theme.warn);
    let panel = host_panel(host, theme, stale);
    let two_columns = text.width >= 72;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if two_columns {
        let height = panel.system.len().max(panel.zfs.len());
        let [left, right] = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(Rect { height: u16::try_from(height).unwrap_or(u16::MAX).min(text.height), ..text });
        f.render_widget(Paragraph::new(panel.system.clone()), left);
        f.render_widget(Paragraph::new(panel.zfs.clone()), right);
        lines.push(Line::raw(""));
        let used = u16::try_from(height).unwrap_or(u16::MAX).min(text.height);
        let below = Rect { y: text.y + used, height: text.height - used, ..text };
        draw_pool_rows(f, &panel.pools, below);
    } else {
        lines.extend(panel.system.clone());
        lines.extend(panel.zfs.clone());
        let used = u16::try_from(lines.len()).unwrap_or(u16::MAX).min(text.height);
        f.render_widget(Paragraph::new(lines), Rect { height: used, ..text });
        draw_pool_rows(f, &panel.pools, Rect { y: text.y + used, height: text.height - used, ..text });
    }
}

fn draw_pool_rows(f: &mut Frame, rows: &[Line<'static>], area: Rect) {
    let room = usize::from(area.height);
    let mut shown: Vec<Line<'static>> = rows.iter().take(room).cloned().collect();
    if rows.len() > room && room > 0 {
        let hidden = rows.len() - (room - 1);
        shown.truncate(room - 1);
        shown.push(Line::raw(format!("+{hidden} more pool row{}: enlarge the terminal", if hidden == 1 { "" } else { "s" })));
    }
    f.render_widget(Paragraph::new(shown), area);
}
```

(`ScanSummary`, `PoolHealth` imports added at the top of the file.)

2. In `draw_body` change the host branch to `draw_host_panel(f, view, last, host, view.error.is_some(), theme, area);`.
3. In `draw`'s title match: `Some(MetricsTarget::Host) => format!("Metrics — host {} (all pools){}", ctx.host_name, if view.error.is_some() && view.host.is_some() { " (stale)" } else { "" }),`.
4. Move the test fixture `rich_host` into `#[cfg(test)] pub(crate) mod tests_support { … }` in `src/ui/metrics.rs`, keep `use` of it in the tests.

- [ ] **Step 4: Run all tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"; cargo clippy 2>&1 | grep -E "^warning" | grep -v "generated\|never used"`
Expected: all pass. Fix text widths in tests only if a literal differs by padding; behavior must match the tests.

- [ ] **Step 5: Commit**

```bash
git add -A src
git commit -m "feat: the Hosts panel shows CPU, memory, swap, L2ARC and per-pool rows

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Docs and live verification

**Files:**
- Modify: `README.md` — the Hosts section, the unprivileged-command list (line ~61: add `swapinfo`).
- Modify: `docs/superpowers/specs/2026-10-09-hosts-upgrade-design.md` (live results).

- [ ] **Step 1: README.** In the unprivileged-command sentence add `swapinfo` next to `sysctl` (and `hostname` is already there). Replace the Hosts section text with a description of: the table (Name, Address, State, Rtt; marker/name colors Ok normal, Warning yellow, Critical and unreachable red, probing dim), the five health rules with their thresholds, the 30 s probe (staggered, one per host at a time, an unreachable host costs one 10 s timeout), the address being the configured one with no lookup, and the bottom panel's new lines (CPU model/cores/usage, memory, swap, L2ARC when a cache device exists, one row per pool with used %, fragmentation, health and last scrub; last scrub shown as `zpool status` prints it; "+N more pool rows" when the panel is too short).

- [ ] **Step 2: Deploy and compare with ground truth** (deploy per [[project-zfsctl-freebsd-test-box]]: `scp` the changed `src/` files and `README.md` to `fb@172.16.10.90:/home/fb/zfsctl/`, `cargo test && cargo build --release` there; do NOT replace the daemon binary for this feature unless the daemon is touched; run the TUI with `sudo ./zfsctl/target/release/zfsctl` through the pexpect driver, see [[reference-driving-tui-with-pexpect-pyte]]). For each of `local`, `fbsd16`, `fbsd16t1`, `fbsd16t2`: Hosts section: record the table (address, state, rtt) and the panel; compare each new figure with the same value read by hand on that host: `sysctl -n hw.model hw.ncpu`, CPU % against `top -bn1 | grep CPU` (same order of magnitude; the sample is a 1-second window), `sysctl hw.physmem` and `vm.stats.vm.v_free_count` × `hw.pagesize`, `swapinfo -k`, `zpool list -Hp -o name,frag`, `zpool status` scan lines.

- [ ] **Step 3: Health states live.**
  1. **Warning/Critical:** on the hub create a throwaway file-backed pool (`truncate -s 80m /var/tmp/zit-h.img; zpool create zit-h /var/tmp/zit-h.img`), fill it past 80% then past 90% (`dd if=/dev/zero of=/zit-h/x bs=1m count=…`), and watch the hub's row go yellow then red within ~30 s with `zit-h 8x% full`; then `zpool offline`/damage is NOT needed: instead `zpool scrub zit-h` and confirm the panel's scrub column reads `running N%` then a date. Destroy the pool and the image afterwards.
  2. **Degraded:** `truncate -s 80m /var/tmp/zit-m1.img /var/tmp/zit-m2.img; zpool create zit-m mirror /var/tmp/zit-m1.img /var/tmp/zit-m2.img; zpool offline zit-m /var/tmp/zit-m2.img` → the hub row is red with `zit-m DEGRADED`; `zpool online` and `zpool destroy` afterwards.
  3. **Job failed:** add a disabled-then-enabled scrub job on a pool that does not exist for host `fbsd16` in `jobs.toml` and let the daemon fail it once (the daemon must run the new binary only if it changed: it did not), or write a `Failed` run into the history by hand on a scratch state dir; confirm `job failed` appears on that host's row and disappears after a later `Ok` run.
  4. **Disconnected:** add a host entry in a throwaway `config.toml` (or use `n` in the TUI) pointing at an address that does not answer (`fbsd@172.16.10.250`): the row shows `probing`, then `unreachable` within about 10 s, and the UI stays responsive meanwhile (keys still work).
  5. **Stale panel:** select a host, then block it (for example `pfctl`-free: point a test host at a dead address after one good sample is not possible, so instead stop `sshd` is NOT allowed): verify the stale path with the unit/render tests only and say so in the final report.

- [ ] **Step 4: Record and finish.** Append `## Live results (2026-10-09)` to the spec with what steps 2-3 showed (figures compared, states seen, the 10 s disconnect, what was not tested live), then `cargo test` and:

```bash
git add README.md docs
git commit -m "docs: hosts upgrade; live results

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

Then one fresh whole-branch review on the most capable model with the Review Focus section verbatim, a single fix pass for Critical/Important findings (each with a failing test first), update the memory notes, and finish with superpowers:finishing-a-development-branch (the merge into `master` is the user's choice).
