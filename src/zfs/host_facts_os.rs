//! The Hosts detail panel for a Linux or illumos host: uptime, CPU model, the ZFS cache, memory, swap, load and the CPU
//! share, read with the host's own unprivileged tools (`/proc` and `/sys` on Linux, `kstat`, `swap -l`, `uptime` on
//! illumos). The `Resources` reader gives the figures the Dashboard also uses; one more `sh -c` adds what only this panel
//! shows.

use super::host::{cpu_percent, parse_iostat_total, ArcStats, CpuInfo, HostSummary, L2arc, Memory};
use super::metrics::PoolIo;
use super::host_health::HostOs;
use super::host_resources_os as resources;
use crate::error::Result;
use crate::exec::CommandRunner;

pub const LINUX_EXTRAS: &str = "echo '== uptime'; cut -d' ' -f1 /proc/uptime; echo '== model'; grep -m1 'model name' /proc/cpuinfo; \
echo '== meminfo'; grep -E '^(MemFree|Unevictable|SUnreclaim):' /proc/meminfo; \
echo '== arc'; grep -E '^(size|c_max|hits|misses|l2_size|l2_hits|l2_misses) ' /proc/spl/kstat/zfs/arcstats 2>/dev/null; \
echo '== zfs'; cat /sys/module/zfs/version 2>/dev/null";

pub const ILLUMOS_EXTRAS: &str = "echo '== uptime'; kstat -p unix:0:system_misc:boot_time; date +%s; echo '== model'; kstat -p -m cpu_info -s brand | head -n 1; \
echo '== page'; pagesize; echo '== kernel'; kstat -p unix:0:system_pages:pp_kernel; \
echo '== arc'; kstat -p zfs:0:arcstats:size zfs:0:arcstats:c_max zfs:0:arcstats:hits zfs:0:arcstats:misses zfs:0:arcstats:l2_size zfs:0:arcstats:l2_hits zfs:0:arcstats:l2_misses";

/// What the extras script printed, as far as it could be read.
#[derive(Debug, Default, PartialEq)]
pub struct Extras {
    pub uptime_secs: Option<u64>,
    pub cpu_model: Option<String>,
    pub arc: Option<ArcStats>,
    pub l2arc: Option<L2arc>,
    pub zfs_version: Option<String>,
    pub mem_free: Option<u64>,
    pub wired: Option<u64>,
}

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

fn lines<'a>(all: &'a [(&str, Vec<&str>)], name: &str) -> &'a [&'a str] {
    all.iter().find(|(n, _)| *n == name).map_or(&[], |(_, l)| l.as_slice())
}

/// `size   4   4276352` (Linux) and `zfs:0:arcstats:size<TAB>1055101600` (illumos) as (name, value).
fn kstat_value(os: HostOs, line: &str) -> Option<(String, u64)> {
    let f: Vec<&str> = line.split_whitespace().collect();
    if os == HostOs::Illumos {
        let &[key, value] = f.as_slice() else { return None };
        Some((key.rsplit(':').next()?.to_string(), value.parse().ok()?))
    } else {
        let &[name, _, value] = f.as_slice() else { return None };
        Some((name.to_string(), value.parse().ok()?))
    }
}

fn meminfo_bytes(rows: &[&str], key: &str) -> Option<u64> {
    let rest = rows.iter().find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))?;
    rest.split_whitespace().next()?.parse::<u64>().ok()?.checked_mul(1024)
}

pub fn parse_extras(os: HostOs, output: &str) -> Extras {
    let all = sections(output);
    let kstats: Vec<(String, u64)> = lines(&all, "arc").iter().filter_map(|l| kstat_value(os, l)).collect();
    let get = |name: &str| kstats.iter().find(|(n, _)| n == name).map(|(_, v)| *v);
    let arc = get("size").zip(get("c_max")).map(|(size, max)| ArcStats { size, max, hits: get("hits").unwrap_or(0), misses: get("misses").unwrap_or(0) });
    let l2arc = get("l2_size").filter(|s| *s > 0).map(|size| L2arc { size, hits: get("l2_hits").unwrap_or(0), misses: get("l2_misses").unwrap_or(0) });
    let mut extras = Extras { arc, l2arc, ..Extras::default() };
    let first = |name: &str| lines(&all, name).first().map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
    if os == HostOs::Linux {
        extras.uptime_secs = first("uptime").and_then(|u| u.split('.').next()?.parse().ok());
        extras.cpu_model = first("model").and_then(|m| Some(m.split_once(':')?.1.trim().to_string())).filter(|m| !m.is_empty());
        extras.zfs_version = first("zfs");
        let mem = lines(&all, "meminfo");
        extras.mem_free = meminfo_bytes(mem, "MemFree");
        // What the kernel holds that is not given back at once: the ZFS cache, unreclaimable slab, unevictable pages.
        let (slab, unevictable) = (meminfo_bytes(mem, "SUnreclaim"), meminfo_bytes(mem, "Unevictable"));
        extras.wired = (slab.is_some() || unevictable.is_some()).then(|| extras.arc.as_ref().map_or(0, |a| a.size).saturating_add(slab.unwrap_or(0)).saturating_add(unevictable.unwrap_or(0)));
    } else {
        let uptime = lines(&all, "uptime");
        let boot = uptime.first().and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok());
        let now = uptime.get(1).and_then(|l| l.trim().parse::<u64>().ok());
        extras.uptime_secs = boot.zip(now).map(|(b, n)| n.saturating_sub(b));
        extras.cpu_model = first("model").and_then(|m| Some(m.split_once(char::is_whitespace)?.1.trim().to_string())).filter(|m| !m.is_empty());
        let page = first("page").and_then(|p| p.parse::<u64>().ok());
        extras.wired = page.zip(lines(&all, "kernel").first().and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())).map(|(p, n)| p.saturating_mul(n));
    }
    extras
}

fn stdout_of(output: Result<crate::exec::CommandOutput>) -> Option<String> {
    output.ok().filter(crate::exec::CommandOutput::success).map(|o| o.stdout)
}

/// The rest of `host::sample_host` for a Linux or illumos host: `base` carries what is read the same everywhere (the pools,
/// the status line, the round trip, `uname -sr`). Blocks for the 1-second iostat window, like the FreeBSD path.
pub fn finish(runner: &dyn CommandRunner, os: HostOs, base: HostSummary) -> (HostSummary, PoolIo) {
    let read = |script: &str| stdout_of(runner.run_unprivileged("sh", &["-c", script]));
    let before = read(resources::script(os).unwrap_or("")).and_then(|o| resources::parse(os, &o));
    let extras = read(if os == HostOs::Illumos { ILLUMOS_EXTRAS } else { LINUX_EXTRAS }).map(|o| parse_extras(os, &o)).unwrap_or_default();
    let io = stdout_of(runner.run("zpool", &["iostat", "-Hpy", "1", "1"])).map(|s| parse_iostat_total(&s)).unwrap_or_default();
    let after = read(resources::script(os).unwrap_or("")).and_then(|o| resources::parse(os, &o));
    let percent = before.as_ref().and_then(|b| b.cpu_ticks).zip(after.as_ref().and_then(|a| a.cpu_ticks)).and_then(|(a, b)| cpu_percent(a, b));
    let now = after.or(before);
    let summary = HostSummary {
        zfs_version: extras.zfs_version,
        uptime_secs: extras.uptime_secs,
        load: now.as_ref().and_then(|r| r.load),
        arc: extras.arc,
        cpu: now.as_ref().zip(extras.cpu_model).map(|(r, model)| CpuInfo { model, cores: r.cores, percent }),
        memory: now.as_ref().map(|r| Memory { physical: r.physical, free: extras.mem_free.unwrap_or(r.free), wired: extras.wired.unwrap_or(0) }),
        swap: now.as_ref().and_then(|r| r.swap),
        l2arc: extras.l2arc,
        ..base
    };
    (summary, io)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real output of the two scripts on the Ubuntu 24 and OmniOS r151058 test hosts.
    const UBUNTU: &str = "== uptime\n598760.37\n== model\nmodel name\t: Intel(R) Xeon(R) CPU E5-2660 0 @ 2.20GHz\n== meminfo\nMemFree:          858624 kB\nUnevictable:       27160 kB\nSUnreclaim:       339480 kB\n== arc\nhits                            4    27830\nmisses                          4    0\nc_max                           4    16819331072\nsize                            4    4276352\nl2_hits                         4    0\nl2_misses                       4    0\nl2_size                         4    0\n== zfs\n2.2.2-0ubuntu9.5\n";
    const OMNIOS: &str = "== uptime\nunix:0:system_misc:boot_time\t1791728822\n1792310496\n== model\ncpu_info:0:cpu_info0:brand\tIntel(r) Xeon(r) CPU E5-2660 0 @ 2.20GHz\n== page\n4096\n== kernel\nunix:0:system_pages:pp_kernel\t623752\n== arc\nzfs:0:arcstats:size\t1055101600\nzfs:0:arcstats:c_max\t15560314880\nzfs:0:arcstats:hits\t14426686\nzfs:0:arcstats:misses\t2249722\nzfs:0:arcstats:l2_size\t0\nzfs:0:arcstats:l2_hits\t0\nzfs:0:arcstats:l2_misses\t0\n";

    #[test]
    fn linux_extras_read_uptime_model_cache_and_version() {
        let e = parse_extras(HostOs::Linux, UBUNTU);
        assert_eq!(e.uptime_secs, Some(598_760));
        assert_eq!(e.cpu_model.as_deref(), Some("Intel(R) Xeon(R) CPU E5-2660 0 @ 2.20GHz"));
        assert_eq!(e.arc, Some(ArcStats { size: 4_276_352, max: 16_819_331_072, hits: 27_830, misses: 0 }));
        assert_eq!(e.l2arc, None, "no cache device");
        assert_eq!(e.zfs_version.as_deref(), Some("2.2.2-0ubuntu9.5"));
        assert_eq!(e.mem_free, Some(858_624 * 1024));
        assert_eq!(e.wired, Some(4_276_352 + (339_480 + 27_160) * 1024));
    }

    #[test]
    fn illumos_extras_read_boot_time_brand_cache_and_kernel_pages() {
        let e = parse_extras(HostOs::Illumos, OMNIOS);
        assert_eq!(e.uptime_secs, Some(1_792_310_496 - 1_791_728_822));
        assert_eq!(e.cpu_model.as_deref(), Some("Intel(r) Xeon(r) CPU E5-2660 0 @ 2.20GHz"));
        assert_eq!(e.arc, Some(ArcStats { size: 1_055_101_600, max: 15_560_314_880, hits: 14_426_686, misses: 2_249_722 }));
        assert_eq!(e.wired, Some(623_752 * 4096));
        assert_eq!(e.zfs_version, None, "OmniOS has no `zfs version`");
    }

    #[test]
    fn a_cache_device_shows_and_unreadable_output_gives_nothing() {
        let e = parse_extras(HostOs::Linux, &UBUNTU.replace("l2_size                         4    0", "l2_size                         4    1048576"));
        assert_eq!(e.l2arc, Some(L2arc { size: 1_048_576, hits: 0, misses: 0 }));
        assert_eq!(parse_extras(HostOs::Linux, "sh: grep: not found\n"), Extras::default());
        assert_eq!(parse_extras(HostOs::Illumos, ""), Extras::default());
    }
}
