//! Memory, CPU, load and swap of a Linux or illumos host, from one unprivileged `sh -c` that prints a few `/proc` or `kstat`
//! readings under `== name` headings. The result is the same `Resources` FreeBSD gives, with the same meaning: what is in
//! use is what the system cannot hand back, so the ZFS cache (and on Linux the reclaimable page cache) does not count.

use super::host::{CpuTimes, Swap};
use super::host_health::HostOs;
use super::host_resources::{CacheStats, Resources};

/// Linux: `/proc` files, no sudo. `grep -c ^processor` counts the CPUs; the ARC line is missing without the ZFS module.
pub const LINUX_SCRIPT: &str = "echo '== meminfo'; grep -E '^(MemTotal|MemAvailable|SwapTotal|SwapFree):' /proc/meminfo; \
echo '== stat'; head -n 1 /proc/stat; echo '== load'; cat /proc/loadavg; \
echo '== arc'; grep -E '^(size|hits|misses|l2_size|l2_hits|l2_misses) ' /proc/spl/kstat/zfs/arcstats 2>/dev/null; echo '== cores'; grep -c '^processor' /proc/cpuinfo";

/// illumos: `kstat` for memory, the ARC and the per-CPU tick counters, `swap -l` for the swap devices, `uptime` for the load.
pub const ILLUMOS_SCRIPT: &str = "echo '== mem'; kstat -p unix:0:system_pages:physmem unix:0:system_pages:freemem; \
echo '== page'; pagesize; echo '== arc'; kstat -p zfs:0:arcstats:size zfs:0:arcstats:hits zfs:0:arcstats:misses zfs:0:arcstats:l2_size zfs:0:arcstats:l2_hits zfs:0:arcstats:l2_misses; echo '== cores'; psrinfo | wc -l; \
echo '== load'; uptime; echo '== swap'; swap -l; \
echo '== cpu'; kstat -p -m cpu -n sys | grep -E 'cpu_ticks_(idle|user|kernel|wait)'";

pub fn script(os: HostOs) -> Option<&'static str> {
    match os {
        HostOs::Linux => Some(LINUX_SCRIPT),
        HostOs::Illumos => Some(ILLUMOS_SCRIPT),
        _ => None,
    }
}

pub fn parse(os: HostOs, output: &str) -> Option<Resources> {
    let sections = sections(output);
    match os {
        HostOs::Linux => parse_linux(&sections),
        HostOs::Illumos => parse_illumos(&sections),
        _ => None,
    }
}

/// The lines under each `== name` heading.
fn sections(output: &str) -> Vec<(&str, Vec<&str>)> {
    let mut all: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in output.lines() {
        if let Some(name) = line.strip_prefix("== ") {
            all.push((name.trim(), Vec::new()));
        } else if let Some((_, lines)) = all.last_mut() {
            lines.push(line);
        }
    }
    all
}

fn section<'a>(all: &'a [(&str, Vec<&str>)], name: &str) -> &'a [&'a str] {
    all.iter().find(|(n, _)| *n == name).map_or(&[], |(_, l)| l.as_slice())
}

/// `Key:   123 kB` as bytes.
fn meminfo_bytes(lines: &[&str], key: &str) -> Option<u64> {
    let rest = lines.iter().find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))?;
    rest.split_whitespace().next()?.parse::<u64>().ok()?.checked_mul(1024)
}

fn three_floats<'a>(mut words: impl Iterator<Item = &'a str>) -> Option<[f64; 3]> {
    let mut next = || words.next().and_then(|w| w.trim_end_matches(',').parse::<f64>().ok());
    Some([next()?, next()?, next()?])
}

fn parse_linux(all: &[(&str, Vec<&str>)]) -> Option<Resources> {
    let mem = section(all, "meminfo");
    let physical = meminfo_bytes(mem, "MemTotal")?;
    let available = meminfo_bytes(mem, "MemAvailable")?;
    let swap = meminfo_bytes(mem, "SwapTotal").zip(meminfo_bytes(mem, "SwapFree")).filter(|(total, _)| *total > 0).map(|(total, free)| Swap { total, used: total.saturating_sub(free) });
    // `cpu  user nice system idle iowait irq softirq steal`: waiting for I/O is idle, stolen time is busy.
    let cpu_ticks = section(all, "stat").first().and_then(|l| l.strip_prefix("cpu ")).and_then(|l| {
        let n: Vec<u64> = l.split_whitespace().map(|t| t.parse().ok()).collect::<Option<_>>()?;
        let at = |i: usize| n.get(i).copied().unwrap_or(0);
        (n.len() >= 4).then(|| CpuTimes { user: at(0), nice: at(1), sys: at(2).saturating_add(at(7)), intr: at(5).saturating_add(at(6)), idle: at(3).saturating_add(at(4)) })
    });
    // `size   4   4276352`: name, type, bytes.
    let counter = |name: &str| section(all, "arc").iter().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        if let &[n, _, value] = f.as_slice() { (n == name).then(|| value.parse::<u64>().ok()).flatten() } else { None }
    });
    let arc = counter("size").unwrap_or(0);
    Some(Resources {
        cores: section(all, "cores").first()?.trim().parse().ok().filter(|c| *c > 0)?,
        physical,
        free: available,
        inactive: 0,
        arc,
        load: section(all, "load").first().and_then(|l| three_floats(l.split_whitespace())),
        cpu_ticks,
        swap,
        cache: cache_stats(&counter),
    })
}

/// The five counters, or `None` when any is missing (a host without the ZFS module).
fn cache_stats(counter: &dyn Fn(&str) -> Option<u64>) -> Option<CacheStats> {
    Some(CacheStats { hits: counter("hits")?, misses: counter("misses")?, l2_size: counter("l2_size")?, l2_hits: counter("l2_hits")?, l2_misses: counter("l2_misses")? })
}

fn parse_illumos(all: &[(&str, Vec<&str>)]) -> Option<Resources> {
    let kstat = |lines: &[&str], suffix: &str| lines.iter().find_map(|l| l.split_whitespace().next()?.ends_with(suffix).then(|| l.split_whitespace().nth(1)?.parse::<u64>().ok()).flatten());
    let mem = section(all, "mem");
    let page: u64 = section(all, "page").first()?.trim().parse().ok().filter(|p| *p > 0)?;
    let physical = kstat(mem, ":physmem")?.checked_mul(page)?;
    let free = kstat(mem, ":freemem")?.saturating_mul(page);
    // `swap -l`: path, dev, swaplo, blocks (512 bytes), free blocks; several devices add up.
    let (mut total, mut unused) = (0u64, 0u64);
    for l in section(all, "swap").iter().skip(1) {
        let f: Vec<&str> = l.split_whitespace().collect();
        if let [_, _, _, blocks, free] = f.as_slice() {
            total = total.saturating_add(blocks.parse::<u64>().ok()?.saturating_mul(512));
            unused = unused.saturating_add(free.parse::<u64>().ok()?.saturating_mul(512));
        }
    }
    // The tick counters of every CPU add up: `cpu:0:sys:cpu_ticks_idle  543779322`.
    let mut ticks = CpuTimes { user: 0, nice: 0, sys: 0, intr: 0, idle: 0 };
    let mut seen = false;
    for l in section(all, "cpu") {
        let mut w = l.split_whitespace();
        let (Some(name), Some(value)) = (w.next(), w.next().and_then(|v| v.parse::<u64>().ok())) else { continue };
        match name.rsplit(':').next() {
            Some("cpu_ticks_user") => ticks.user = ticks.user.saturating_add(value),
            Some("cpu_ticks_kernel") => ticks.sys = ticks.sys.saturating_add(value),
            Some("cpu_ticks_idle" | "cpu_ticks_wait") => ticks.idle = ticks.idle.saturating_add(value),
            _ => continue,
        }
        seen = true;
    }
    let load = section(all, "load").first().and_then(|l| l.split("load average:").nth(1)).and_then(|l| three_floats(l.split_whitespace()));
    Some(Resources {
        cores: section(all, "cores").first()?.trim().parse().ok().filter(|c| *c > 0)?,
        physical,
        free,
        inactive: 0,
        arc: kstat(section(all, "arc"), ":size").unwrap_or(0),
        load,
        cpu_ticks: seen.then_some(ticks),
        swap: (total > 0).then_some(Swap { total, used: total.saturating_sub(unused) }),
        cache: cache_stats(&|name: &str| kstat(section(all, "arc"), &format!(":{name}"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real output from the Ubuntu 24 test host (32 GiB, no swap, a small pool) and from OmniOS r151058 (16 GiB, 4 GiB zvol swap).
    const UBUNTU: &str = "== meminfo\nMemTotal:       32850256 kB\nMemAvailable:   31218412 kB\nSwapTotal:             0 kB\nSwapFree:              0 kB\n== stat\ncpu  5874690 39295 3026389 1820649091 3477991 0 149848 45990 0 0\n== load\n0.04 0.02 0.00 1/756 184609\n== arc\nsize                            4    4276352\nhits                            4    9990\nmisses                          4    10\nl2_size                         4    0\nl2_hits                         4    0\nl2_misses                       4    0\n== cores\n32\n";
    const OMNIOS: &str = "== mem\nunix:0:system_pages:physmem\t4192121\nunix:0:system_pages:freemem\t3491395\n== page\n4096\n== arc\nzfs:0:arcstats:size\t1054600016\nzfs:0:arcstats:hits\t14426686\nzfs:0:arcstats:misses\t2249722\nzfs:0:arcstats:l2_size\t2097152\nzfs:0:arcstats:l2_hits\t300\nzfs:0:arcstats:l2_misses\t100\n== cores\n       2\n== load\n20:58:26    up 6 day(s), 10:31,  0 users,  load average: 0.07, 0.04, 0.04\n== swap\nswapfile             dev    swaplo   blocks     free\n/dev/zvol/dsk/rpool/swap 267,2         8  4194296  4194296\n== cpu\ncpu:0:sys:cpu_ticks_idle\t543779322\ncpu:0:sys:cpu_ticks_kernel\t12437866\ncpu:0:sys:cpu_ticks_user\t93386\ncpu:0:sys:cpu_ticks_wait\t0\ncpu:1:sys:cpu_ticks_idle\t555320299\ncpu:1:sys:cpu_ticks_kernel\t833310\ncpu:1:sys:cpu_ticks_user\t154646\ncpu:1:sys:cpu_ticks_wait\t0\n";

    #[test]
    fn a_linux_host_reads_like_a_freebsd_one_with_available_memory_as_free() {
        let r = parse(HostOs::Linux, UBUNTU).unwrap();
        assert_eq!((r.cores, r.physical, r.free, r.inactive, r.arc), (32, 32_850_256 * 1024, 31_218_412 * 1024, 0, 4_276_352));
        assert_eq!(r.load, Some([0.04, 0.02, 0.0]));
        assert_eq!(r.cache, Some(CacheStats { hits: 9990, misses: 10, l2_size: 0, l2_hits: 0, l2_misses: 0 }));
        assert_eq!(r.swap, None, "SwapTotal 0 is no swap");
        // idle = idle + iowait; sys includes steal; intr = irq + softirq.
        assert_eq!(r.cpu_ticks, Some(CpuTimes { user: 5_874_690, nice: 39_295, sys: 3_026_389 + 45_990, intr: 149_848, idle: 1_820_649_091 + 3_477_991 }));
    }

    #[test]
    fn linux_swap_in_use_is_total_minus_free_and_a_missing_arc_is_zero() {
        let text = UBUNTU.replace("SwapTotal:             0 kB\nSwapFree:              0 kB", "SwapTotal:       2097152 kB\nSwapFree:        1572864 kB").replace("size                            4    4276352\nhits                            4    9990\nmisses                          4    10\nl2_size                         4    0\nl2_hits                         4    0\nl2_misses                       4    0\n", "");
        let r = parse(HostOs::Linux, &text).unwrap();
        assert_eq!(r.swap, Some(Swap { total: 2_097_152 * 1024, used: 524_288 * 1024 }));
        assert_eq!(r.arc, 0, "a host without the ZFS module's kstats has no cache to discount");
        assert_eq!(r.cache, None, "and no counters");
    }

    #[test]
    fn an_illumos_host_sums_its_cpus_and_reads_swap_from_the_devices() {
        let r = parse(HostOs::Illumos, OMNIOS).unwrap();
        assert_eq!((r.cores, r.physical, r.free, r.arc), (2, 4_192_121 * 4096, 3_491_395 * 4096, 1_054_600_016));
        assert_eq!(r.load, Some([0.07, 0.04, 0.04]));
        assert_eq!(r.cache, Some(CacheStats { hits: 14_426_686, misses: 2_249_722, l2_size: 2_097_152, l2_hits: 300, l2_misses: 100 }));
        assert_eq!(r.swap, Some(Swap { total: 4_194_296 * 512, used: 0 }));
        assert_eq!(r.cpu_ticks, Some(CpuTimes { user: 93_386 + 154_646, nice: 0, sys: 12_437_866 + 833_310, intr: 0, idle: 543_779_322 + 555_320_299 }));
    }

    #[test]
    fn output_that_is_not_an_answer_gives_none_not_a_guess() {
        for os in [HostOs::Linux, HostOs::Illumos] {
            assert_eq!(parse(os, ""), None, "{os:?}");
            assert_eq!(parse(os, "sh: grep: not found\n"), None, "{os:?}");
        }
        assert_eq!(parse(HostOs::FreeBsd, UBUNTU), None);
        assert_eq!(script(HostOs::FreeBsd), None);
    }

    #[test]
    fn a_missing_reading_costs_only_that_figure() {
        let no_load = UBUNTU.replace("0.04 0.02 0.00 1/756 184609\n", "");
        assert_eq!(parse(HostOs::Linux, &no_load).unwrap().load, None);
        let no_cpu = OMNIOS.replace("cpu_ticks", "cpu_x");
        assert_eq!(parse(HostOs::Illumos, &no_cpu).unwrap().cpu_ticks, None);
    }
}
