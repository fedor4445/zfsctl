//! A whole-host summary for the Hosts section's bottom panel.
//!
//! Only the pool list is required. Everything else (OS, versions, ARC,
//! load, uptime) is best-effort: a host that lacks a sysctl, or isn't
//! FreeBSD at all, just shows `-` for that line instead of an error.

use super::metrics::PoolIo;
use super::pool::{parse_pool_list, Pool, PoolHealth, POOL_LIST_ARGS};
use super::{parse_numeric_or_dash, run_checked};
use crate::error::Result;
use crate::exec::CommandRunner;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArcStats {
    pub size: u64,
    pub max: u64,
    pub hits: u64,
    pub misses: u64,
}

impl ArcStats {
    /// Hit rate since boot as a percentage; `None` before the first access.
    #[allow(clippy::cast_precision_loss)] // display only
    pub fn hit_percent(&self) -> Option<f64> {
        let total = self.hits.saturating_add(self.misses);
        (total > 0).then(|| self.hits as f64 * 100.0 / total as f64)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct HostSummary {
    pub os: Option<String>,
    pub zfs_version: Option<String>,
    pub uptime_secs: Option<u64>,
    pub load: Option<[f64; 3]>,
    pub arc: Option<ArcStats>,
    pub pools: Vec<Pool>,
    /// First line of `zpool status -x`, e.g. `all pools are healthy`.
    pub status: Option<String>,
    /// Milliseconds one unprivileged command took to round-trip.
    pub rtt_ms: Option<u64>,
}

impl HostSummary {
    pub fn capacity(&self) -> (u64, u64) {
        self.pools.iter().fold((0, 0), |(alloc, size), p| (alloc.saturating_add(p.allocated), size.saturating_add(p.size)))
    }

    /// Pools that are not ONLINE.
    pub fn unhealthy(&self) -> usize {
        self.pools.iter().filter(|p| p.health != PoolHealth::Online).count()
    }
}

/// `zfs version` prints the userland line first, then the kernel module's.
pub fn parse_zfs_version(output: &str) -> Option<String> {
    let line = output.lines().next()?.trim();
    Some(line.strip_prefix("zfs-").unwrap_or(line).to_string()).filter(|s| !s.is_empty())
}

/// `sysctl -n vm.loadavg`: `{ 0.05 0.12 0.08 }`.
pub fn parse_loadavg(output: &str) -> Option<[f64; 3]> {
    let nums: Vec<f64> = output.trim().trim_matches(|c| c == '{' || c == '}').split_whitespace().filter_map(|t| t.parse().ok()).collect();
    <[f64; 3]>::try_from(nums).ok()
}

/// `sysctl -n kern.boottime`: `{ sec = 1791293984, usec = 618853 } Tue ...`.
pub fn parse_boottime(output: &str) -> Option<u64> {
    let after = output.split("sec =").nth(1)?;
    after.split(',').next()?.trim().parse().ok()
}

/// Four `sysctl -n` lines: size, `c_max`, hits, misses.
pub fn parse_arc(output: &str) -> Option<ArcStats> {
    let nums: Vec<u64> = output.lines().filter_map(|l| l.trim().parse().ok()).collect();
    let &[size, max, hits, misses] = nums.as_slice() else { return None };
    Some(ArcStats { size, max, hits, misses })
}

/// Sums every pool row of `zpool iostat -Hpy 1 1`: name, alloc, free,
/// read ops, write ops, read bw, write bw.
pub fn parse_iostat_total(output: &str) -> PoolIo {
    let mut total = PoolIo::default();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if let [_, _, _, rops, wops, rbps, wbps, ..] = fields.as_slice() {
            let n = |s: &str| parse_numeric_or_dash(s).unwrap_or(0);
            total.read_ops = total.read_ops.saturating_add(n(rops));
            total.write_ops = total.write_ops.saturating_add(n(wops));
            total.read_bps = total.read_bps.saturating_add(n(rbps));
            total.write_bps = total.write_bps.saturating_add(n(wbps));
        }
    }
    total
}

fn stdout_of(output: Result<crate::exec::CommandOutput>) -> Option<String> {
    output.ok().filter(crate::exec::CommandOutput::success).map(|o| o.stdout)
}

/// One host refresh: the summary plus all-pools I/O for a 1-second window.
/// Blocks for about a second, so it belongs on a worker thread.
pub fn sample_host(runner: &dyn CommandRunner) -> Result<(HostSummary, PoolIo)> {
    let pools = parse_pool_list(&run_checked(runner, "zpool", POOL_LIST_ARGS, "zpool list")?.stdout)?;
    let started = Instant::now();
    let os = stdout_of(runner.run_unprivileged("uname", &["-sr"])).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let rtt_ms = os.is_some().then(|| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    let now = stdout_of(runner.run_unprivileged("date", &["+%s"])).and_then(|s| s.trim().parse::<u64>().ok());
    let boot = stdout_of(runner.run_unprivileged("sysctl", &["-n", "kern.boottime"])).and_then(|s| parse_boottime(&s));
    let summary = HostSummary {
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
    };
    let io = stdout_of(runner.run("zpool", &["iostat", "-Hpy", "1", "1"])).map(|s| parse_iostat_total(&s)).unwrap_or_default();
    Ok((summary, io))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    // Captured from the FreeBSD 15.1 hub.
    const IOSTAT: &str = "scratch\t106278912\t145379328\t0\t0\t0\t0\ntpool\t8663826432\t12274139136\t5\t7\t1000\t2000\nzroot\t1\t2\t1\t1\t24\t48\n";

    #[test]
    fn zfs_version_keeps_the_userland_line_without_its_prefix() {
        assert_eq!(parse_zfs_version("zfs-2.4.2-1-FreeBSD_g6330a45b0\nzfs-kmod-2.4.2-1\n").as_deref(), Some("2.4.2-1-FreeBSD_g6330a45b0"));
        assert_eq!(parse_zfs_version(""), None);
    }

    #[test]
    fn loadavg_boottime_and_arc_parse_the_real_sysctl_output() {
        assert_eq!(parse_loadavg("{ 0.05 0.12 0.08 }\n"), Some([0.05, 0.12, 0.08]));
        assert_eq!(parse_loadavg("garbage"), None);
        assert_eq!(parse_boottime("{ sec = 1791293984, usec = 618853 } Tue Oct  6 09:39:44 2026\n"), Some(1_791_293_984));
        assert_eq!(parse_boottime("nope"), None);
        let arc = parse_arc("409692688\n20344946688\n1040237\n8495\n").unwrap();
        assert_eq!((arc.size, arc.max, arc.hits, arc.misses), (409_692_688, 20_344_946_688, 1_040_237, 8_495));
        assert!((arc.hit_percent().unwrap() - 99.19).abs() < 0.01);
        assert_eq!(parse_arc("1\n2\n"), None);
        assert_eq!(ArcStats::default().hit_percent(), None);
    }

    #[test]
    fn iostat_total_sums_every_pool() {
        let t = parse_iostat_total(IOSTAT);
        assert_eq!((t.read_ops, t.write_ops, t.read_bps, t.write_bps), (6, 8, 1024, 2048));
        assert_eq!(parse_iostat_total(""), PoolIo::default());
    }

    #[test]
    fn sample_host_needs_the_pool_list_but_tolerates_every_other_failure() {
        assert!(sample_host(&FakeRunner::failing("no zfs", 1)).is_err());
        let runner = FakeRunner::new("")
            .route("zpool", "list", "tank\t3000\t1000\t2000\tONLINE\nbad\t100\t50\t50\tDEGRADED\n")
            .route_failure("zpool", "status", "boom", 1);
        let (s, io) = sample_host(&runner).unwrap();
        assert_eq!((s.pools.len(), s.unhealthy(), s.capacity()), (2, 1, (1050, 3100)));
        assert_eq!((s.os, s.load, s.arc, s.status, io), (None, None, None, None, PoolIo::default()));
    }

    #[test]
    fn sample_host_reads_unprivileged_info_without_sudo_and_derives_uptime() {
        let runner = FakeRunner::new("")
            .route("zpool", "list", "tank\t3000\t1000\t2000\tONLINE\n")
            .route("uname", "-sr", "FreeBSD 15.1-RELEASE-p4\n")
            .route("date", "+%s", "1791295391\n")
            .route("sysctl", "-n", "{ sec = 1791293984, usec = 6 } Tue\n")
            .route("zpool", "status", "all pools are healthy\n")
            .route("zpool", "iostat", IOSTAT);
        let (s, io) = sample_host(&runner).unwrap();
        assert_eq!(s.os.as_deref(), Some("FreeBSD 15.1-RELEASE-p4"));
        assert_eq!(s.uptime_secs, Some(1407));
        assert_eq!(s.status.as_deref(), Some("all pools are healthy"));
        assert_eq!(io.write_bps, 2048);
        let unpriv = runner.unprivileged_calls.lock().unwrap();
        assert!(unpriv.iter().all(|(cmd, _)| ["uname", "date", "sysctl"].contains(&cmd.as_str())), "{unpriv:?}");
        assert!(runner.calls.lock().unwrap().iter().any(|(c, a)| c == "zfs" && a == &["version"]));
    }
}
