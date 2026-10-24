//! What a host is doing right now: memory, CPU, load and swap, for the Dashboard. One `sysctl -n` and one `swapinfo -k`,
//! both unprivileged, so no sudoers entry is needed. The pure maths is here; the TUI keeps the last two readings.

use super::host::{cpu_percent, parse_cpu_times, parse_loadavg, parse_swapinfo, CpuTimes, Swap};
use crate::exec::CommandRunner;

/// One reading of a host. Raw facts, so the rules (and their thresholds) live with the Dashboard.
#[derive(Debug, Clone, PartialEq)]
pub struct Resources {
    pub cores: u32,
    pub physical: u64,
    pub free: u64,
    pub inactive: u64,
    /// Size of the ZFS cache (ARC). It is memory the system gives back when asked, so it is not pressure.
    pub arc: u64,
    pub load: Option<[f64; 3]>,
    /// The cumulative `kern.cp_time` counters; the CPU share needs two readings.
    pub cpu_ticks: Option<CpuTimes>,
    pub swap: Option<Swap>,
    /// The ZFS cache counters since boot; `None` where the host has no ZFS kstats.
    pub cache: Option<CacheStats>,
}

/// The cumulative ARC and L2ARC counters (`kstat.zfs.misc.arcstats`): hits and misses, and the L2ARC's size, hits and misses. The
/// hit rate since boot is what the Hosts panel shows too.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub l2_size: u64,
    pub l2_hits: u64,
    pub l2_misses: u64,
}

impl CacheStats {
    pub fn arc_hit_percent(&self) -> Option<f64> {
        super::host::ArcStats { hits: self.hits, misses: self.misses, ..Default::default() }.hit_percent()
    }

    /// `None` when the host has no cache device (a size of 0) or it has not been read from yet.
    pub fn l2_hit_percent(&self) -> Option<f64> {
        (self.l2_size > 0).then(|| super::host::L2arc { size: self.l2_size, hits: self.l2_hits, misses: self.l2_misses }.hit_percent()).flatten()
    }
}

const SYSCTL_ARGS: [&str; 14] = [
    "-n",
    "hw.ncpu",
    "hw.physmem",
    "hw.pagesize",
    "vm.stats.vm.v_free_count",
    "vm.stats.vm.v_inactive_count",
    "kstat.zfs.misc.arcstats.size",
    "vm.loadavg",
    "kern.cp_time",
    "kstat.zfs.misc.arcstats.hits",
    "kstat.zfs.misc.arcstats.misses",
    "kstat.zfs.misc.arcstats.l2_size",
    "kstat.zfs.misc.arcstats.l2_hits",
    "kstat.zfs.misc.arcstats.l2_misses",
];

/// The lines `sysctl -n` prints for `SYSCTL_ARGS` (swap is read separately): the eight resource lines, then the five cache counters.
fn parse_sysctl(output: &str) -> Option<Resources> {
    let lines: Vec<&str> = output.lines().map(str::trim).collect();
    let &[ncpu, physmem, pagesize, free, inactive, arc, load, cp_time, hits, misses, l2_size, l2_hits, l2_misses] = lines.as_slice() else { return None };
    let page: u64 = pagesize.parse().ok().filter(|p| *p > 0)?;
    let pages = |s: &str| s.parse::<u64>().ok().map(|n| n.saturating_mul(page));
    Some(Resources {
        cores: ncpu.parse().ok()?,
        physical: physmem.parse().ok()?,
        free: pages(free)?,
        inactive: pages(inactive)?,
        arc: arc.parse().ok()?,
        load: parse_loadavg(load),
        cpu_ticks: parse_cpu_times(cp_time),
        swap: None,
        cache: (|| Some(CacheStats { hits: hits.parse().ok()?, misses: misses.parse().ok()?, l2_size: l2_size.parse().ok()?, l2_hits: l2_hits.parse().ok()?, l2_misses: l2_misses.parse().ok()? }))(),
    })
}

/// Reads the host's resources with unprivileged commands: one `sh -c` on Linux and illumos (see `host_resources_os`), two
/// on FreeBSD and a host whose OS is not known. `None` when the answer fails or does not parse (or the host is
/// unreachable); a failed `swapinfo` only costs the swap figure.
pub fn probe_resources(runner: &dyn CommandRunner, os: Option<super::host_health::HostOs>) -> Option<Resources> {
    if let Some(script) = os.and_then(super::host_resources_os::script) {
        let out = runner.run_unprivileged("sh", &["-c", script]).ok().filter(crate::exec::CommandOutput::success)?;
        return super::host_resources_os::parse(os?, &out.stdout);
    }
    let out = runner.run_unprivileged("sysctl", &SYSCTL_ARGS).ok().filter(crate::exec::CommandOutput::success)?;
    let mut resources = parse_sysctl(&out.stdout)?;
    resources.swap = runner.run_unprivileged("swapinfo", &["-k"]).ok().filter(crate::exec::CommandOutput::success).and_then(|o| parse_swapinfo(&o.stdout));
    Some(resources)
}

/// Memory in use as a percentage, not counting what the system can hand back at once: free pages, inactive pages and the
/// ZFS cache. On FreeBSD with ZFS the cache fills most of the RAM by design, so counting it would alarm all the time.
pub fn memory_used_percent(r: &Resources) -> u8 {
    if r.physical == 0 {
        return 0;
    }
    let available = r.free.saturating_add(r.inactive).saturating_add(r.arc).min(r.physical);
    u8::try_from((u128::from(r.physical - available) * 100 / u128::from(r.physical)).min(100)).unwrap_or(100)
}

/// Swap in use as a percentage; `None` when the host has no swap.
pub fn swap_used_percent(r: &Resources) -> Option<u8> {
    let swap = r.swap.filter(|s| s.total > 0)?;
    Some(u8::try_from((u128::from(swap.used) * 100 / u128::from(swap.total)).min(100)).unwrap_or(100))
}

/// The busy share of the CPU between two readings, rounded; `None` when either lacks the counters or one went backwards.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=100
pub fn cpu_percent_between(before: &Resources, after: &Resources) -> Option<u8> {
    cpu_percent(before.cpu_ticks?, after.cpu_ticks?).map(|p| p.round().clamp(0.0, 100.0) as u8)
}

/// The host's offset from UTC in seconds, from `date +%z` (`+0200`). `zpool status` prints local times without a zone, so
/// this is what turns "last scrub" into an age. `None` for anything that is not a real offset.
pub fn utc_offset_secs(runner: &dyn CommandRunner) -> Option<i32> {
    let out = runner.run_unprivileged("date", &["+%z"]).ok().filter(crate::exec::CommandOutput::success)?;
    let text = out.stdout.trim();
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (hours, minutes): (i32, i32) = (digits[..2].parse().ok()?, digits[2..].parse().ok()?);
    (hours <= 14 && minutes < 60).then_some(sign * (hours * 3600 + minutes * 60))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    const GIB: u64 = 1 << 30;
    // hw.ncpu, hw.physmem (20 GiB), hw.pagesize, free pages (2 GiB), inactive pages (3 GiB), ARC size (10 GiB), loadavg, cp_time.
    const SYSCTL: &str = "4\n21474836480\n4096\n524288\n786432\n10737418240\n{ 0.05 0.12 0.08 }\n1000 0 200 10 8790\n9990\n10\n2097152\n300\n100\n";
    const SWAPINFO: &str = "Device          1K-blocks     Used    Avail Capacity\n/dev/da0p2        2097152   524288  1572864    25%\n";

    fn resources(free: u64, inactive: u64, arc: u64) -> Resources {
        Resources { cores: 4, physical: 20 * GIB, free, inactive, arc, load: None, cpu_ticks: None, swap: None, cache: None }
    }

    #[test]
    fn a_host_without_a_cache_device_has_no_l2arc_hit_rate_and_one_without_accesses_no_arc_rate() {
        assert_eq!(CacheStats { hits: 5, misses: 5, l2_size: 0, l2_hits: 0, l2_misses: 0 }.l2_hit_percent(), None);
        assert_eq!(CacheStats::default().arc_hit_percent(), None);
        assert_eq!(CacheStats { l2_size: 1, ..CacheStats::default() }.l2_hit_percent(), None, "nothing read from it yet");
    }

    #[test]
    fn memory_used_ignores_free_inactive_and_arc() {
        assert_eq!(memory_used_percent(&resources(2 * GIB, 3 * GIB, 10 * GIB)), 25);
        assert_eq!(memory_used_percent(&resources(0, 0, 0)), 100);
    }

    #[test]
    fn an_arc_filled_host_is_not_under_memory_pressure() {
        // ZFS has taken nearly all the RAM for its cache, which it gives back when asked: that is not pressure.
        assert_eq!(memory_used_percent(&resources(GIB, 3 * GIB, 19 * GIB)), 0, "no underflow when the parts exceed the total");
        assert_eq!(memory_used_percent(&Resources { physical: 0, ..resources(0, 0, 0) }), 0, "a host with no RAM reading is not divided by zero");
    }

    #[test]
    fn swap_percent_is_none_without_swap() {
        assert_eq!(swap_used_percent(&resources(0, 0, 0)), None);
        let with = Resources { swap: Some(crate::zfs::host::Swap { total: 2_097_152 * 1024, used: 524_288 * 1024 }), ..resources(0, 0, 0) };
        assert_eq!(swap_used_percent(&with), Some(25));
        let empty = Resources { swap: Some(crate::zfs::host::Swap { total: 0, used: 0 }), ..resources(0, 0, 0) };
        assert_eq!(swap_used_percent(&empty), None, "a zero-size swap is no swap");
    }

    #[test]
    fn cpu_percent_between_two_readings_is_the_busy_share() {
        let ticks = |busy: u64, idle: u64| Some(crate::zfs::host::CpuTimes { user: busy, nice: 0, sys: 0, intr: 0, idle });
        let before = Resources { cpu_ticks: ticks(100, 900), ..resources(0, 0, 0) };
        let after = Resources { cpu_ticks: ticks(160, 940), ..resources(0, 0, 0) };
        assert_eq!(cpu_percent_between(&before, &after), Some(60), "60 busy ticks of 100");
        assert_eq!(cpu_percent_between(&after, &before), None, "a counter that went backwards (a reboot) gives no figure");
        assert_eq!(cpu_percent_between(&before, &resources(0, 0, 0)), None, "a missing reading gives no figure");
    }

    #[test]
    fn probe_resources_makes_exactly_two_unprivileged_calls() {
        let runner = FakeRunner::new("").route("sysctl", "-n", SYSCTL).route("swapinfo", "-k", SWAPINFO);
        let r = probe_resources(&runner, None).expect("both commands answered");
        assert_eq!((r.cores, r.physical, r.free, r.inactive, r.arc), (4, 20 * GIB, 2 * GIB, 3 * GIB, 10 * GIB));
        assert_eq!(r.load, Some([0.05, 0.12, 0.08]));
        assert_eq!(r.cpu_ticks.map(|t| (t.user, t.idle)), Some((1000, 8790)));
        assert_eq!(swap_used_percent(&r), Some(25));
        let calls = runner.unprivileged_calls.lock().unwrap().clone();
        let names: Vec<&str> = calls.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(names, ["sysctl", "swapinfo"]);
        assert_eq!(calls[0].1, ["-n", "hw.ncpu", "hw.physmem", "hw.pagesize", "vm.stats.vm.v_free_count", "vm.stats.vm.v_inactive_count", "kstat.zfs.misc.arcstats.size", "vm.loadavg", "kern.cp_time", "kstat.zfs.misc.arcstats.hits", "kstat.zfs.misc.arcstats.misses", "kstat.zfs.misc.arcstats.l2_size", "kstat.zfs.misc.arcstats.l2_hits", "kstat.zfs.misc.arcstats.l2_misses"]);
        assert_eq!(r.cache, Some(CacheStats { hits: 9990, misses: 10, l2_size: 2_097_152, l2_hits: 300, l2_misses: 100 }));
        assert_eq!(r.cache.and_then(|c| c.arc_hit_percent()).map(|p| (p * 10.0).round()), Some(999.0));
        assert_eq!(r.cache.and_then(|c| c.l2_hit_percent()), Some(75.0));
        // `FakeRunner::run_unprivileged` itself refuses any command not registered as unprivileged, so no sudo is involved.
    }

    #[test]
    fn a_failed_or_unreadable_sysctl_gives_none_and_a_failed_swapinfo_only_loses_swap() {
        assert_eq!(probe_resources(&FakeRunner::failing("sysctl: unknown oid", 1), None), None);
        assert_eq!(probe_resources(&FakeRunner::new("garbage\n"), None), None);
        let no_swap = FakeRunner::new("").route("sysctl", "-n", SYSCTL);
        let r = probe_resources(&no_swap, None).expect("the sysctl answered");
        assert_eq!(r.swap, None, "swapinfo answered with nothing: no swap figure, but the rest stays");
    }

    #[test]
    fn linux_and_illumos_hosts_are_read_with_one_sh_call_and_no_sysctl() {
        use super::super::host_health::HostOs;
        let linux = "== meminfo\nMemTotal:       32850256 kB\nMemAvailable:   31218412 kB\nSwapTotal:             0 kB\nSwapFree:              0 kB\n== stat\ncpu  1 2 3 4 5 0 0 0 0 0\n== load\n0.04 0.02 0.00 1/756 1\n== arc\n== cores\n4\n";
        let runner = FakeRunner::new("").route("sh", "-c", linux);
        let r = probe_resources(&runner, Some(HostOs::Linux)).expect("a Linux answer");
        assert_eq!((r.cores, r.load), (4, Some([0.04, 0.02, 0.0])));
        let names: Vec<String> = runner.calls.lock().unwrap().iter().map(|(c, _)| c.clone()).collect();
        assert_eq!(names, ["sh"], "no sysctl or swapinfo on Linux");
        assert_eq!(probe_resources(&FakeRunner::failing("sh: not found", 127), Some(HostOs::Illumos)), None);
    }

    #[test]
    fn utc_offsets_parse_both_signs_and_reject_garbage() {
        let at = |text: &str| utc_offset_secs(&FakeRunner::new(text));
        assert_eq!(at("+0200\n"), Some(7200));
        assert_eq!(at("-0430\n"), Some(-16_200));
        assert_eq!(at("+0000\n"), Some(0));
        for bad in ["", "UTC", "+02", "+2000x", "0200", "+0260", "+1500"] {
            assert_eq!(at(bad), None, "{bad:?}");
        }
        assert_eq!(utc_offset_secs(&FakeRunner::failing("no date", 1)), None);
    }
}
