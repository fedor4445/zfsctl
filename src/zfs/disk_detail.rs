//! Live activity and health for one highlighted disk.
//!
//! Activity comes from `iostat -x`, which needs no privilege (so no sudoers
//! entry on managed hosts) and covers every GEOM provider. Health is the
//! disk's own line in its pool's `zpool status` table. The disk must show up
//! in `iostat`; a missing pool line only leaves the health row empty.

use super::metrics::{ErrorCounts, PoolIo};
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use crate::zfs::host_health::HostOs;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiskLive {
    /// The pool member's state (`ONLINE`, `DEGRADED`, ...); `None` for a disk outside any pool.
    pub state: Option<String>,
    pub errors: Option<ErrorCounts>,
    pub busy_percent: Option<f64>,
    pub queue: Option<u64>,
}

/// One `iostat -x` row for `disk` from the last block of the output (the
/// first block is the average since boot, so it is skipped).
/// Columns: device r/s w/s kr/s kw/s ms/r ms/w ms/o ms/t qlen %b.
pub fn parse_iostat_x(output: &str, disk: &str) -> Option<(PoolIo, DiskLive)> {
    let lines: Vec<&str> = output.lines().collect();
    let start = lines.iter().rposition(|l| l.trim_start().starts_with("device"))?;
    let fields: Vec<&str> = lines.get(start + 1..)?.iter().map(|l| l.split_whitespace().collect::<Vec<_>>()).find(|f| f.first() == Some(&disk))?;
    let [_, rs, ws, krs, kws, msr, msw, _, _, qlen, busy] = fields.as_slice() else { return None };
    let num = |s: &str| s.parse::<f64>().ok();
    let (rs, ws) = (num(rs)?, num(ws)?);
    let ns = |ms: &str, ops: f64| (ops > 0.0).then(|| num(ms)).flatten().map(|ms| to_u64(ms * 1e6));
    let io = PoolIo {
        read_ops: to_u64(rs),
        write_ops: to_u64(ws),
        read_bps: to_u64(num(krs)? * 1024.0),
        write_bps: to_u64(num(kws)? * 1024.0),
        read_latency_ns: ns(msr, rs),
        write_latency_ns: ns(msw, ws),
    };
    let live = DiskLive { busy_percent: num(busy), queue: qlen.parse().ok(), ..DiskLive::default() };
    Some((io, live))
}

/// The last block of illumos `iostat -xn 1 2`, one row for `disk`.
/// Columns: `r/s w/s kr/s kw/s wait actv wsvc_t asvc_t %w %b device`. The device keeps one service time (`asvc_t`),
/// not separate read and write ones, so both latencies show it.
pub fn parse_iostat_illumos(output: &str, disk: &str) -> Option<(PoolIo, DiskLive)> {
    let lines: Vec<&str> = output.lines().collect();
    let start = lines.iter().rposition(|l| l.trim_start().starts_with("r/s"))?;
    let fields: Vec<&str> = lines.get(start + 1..)?.iter().map(|l| l.split_whitespace().collect::<Vec<_>>()).find(|f| f.last() == Some(&disk))?;
    let [rs, ws, krs, kws, _, actv, _, asvc, _, busy, _] = fields.as_slice() else { return None };
    let num = |s: &str| s.parse::<f64>().ok();
    let (rs, ws, asvc) = (num(rs)?, num(ws)?, num(asvc)?);
    let ns = |ops: f64| (ops > 0.0).then(|| to_u64(asvc * 1e6));
    let io = PoolIo { read_ops: to_u64(rs), write_ops: to_u64(ws), read_bps: to_u64(num(krs)? * 1024.0), write_bps: to_u64(num(kws)? * 1024.0), read_latency_ns: ns(rs), write_latency_ns: ns(ws) };
    Some((io, DiskLive { busy_percent: num(busy), queue: num(actv).map(to_u64), ..DiskLive::default() }))
}

/// The last block of Linux (sysstat) `iostat -dx 1 2`, one row for `disk`. The columns differ between sysstat versions,
/// so they are found by the names in the header: `r/s w/s rkB/s wkB/s r_await w_await aqu-sz` (`avgqu-sz`) `%util`.
pub fn parse_iostat_linux(output: &str, disk: &str) -> Option<(PoolIo, DiskLive)> {
    let lines: Vec<&str> = output.lines().collect();
    let start = lines.iter().rposition(|l| l.trim_start().starts_with("Device"))?;
    let header: Vec<&str> = lines.get(start)?.split_whitespace().collect();
    let fields: Vec<&str> = lines.get(start + 1..)?.iter().map(|l| l.split_whitespace().collect::<Vec<_>>()).find(|f| f.first() == Some(&disk))?;
    let col = |names: &[&str]| names.iter().find_map(|n| header.iter().position(|h| h == n)).and_then(|i| fields.get(i)).and_then(|s| s.parse::<f64>().ok());
    let (rs, ws) = (col(&["r/s"])?, col(&["w/s"])?);
    let ns = |ms: Option<f64>, ops: f64| (ops > 0.0).then_some(ms).flatten().map(|ms| to_u64(ms * 1e6));
    let io = PoolIo {
        read_ops: to_u64(rs),
        write_ops: to_u64(ws),
        read_bps: to_u64(col(&["rkB/s"])? * 1024.0),
        write_bps: to_u64(col(&["wkB/s"])? * 1024.0),
        read_latency_ns: ns(col(&["r_await", "await"]), rs),
        write_latency_ns: ns(col(&["w_await", "await"]), ws),
    };
    Some((io, DiskLive { busy_percent: col(&["%util"]), queue: col(&["aqu-sz", "avgqu-sz"]).map(to_u64), ..DiskLive::default() }))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped non-negative display values
fn to_u64(v: f64) -> u64 {
    v.max(0.0).round() as u64
}

/// Whether a `zpool status` leaf name refers to `disk`: the bare device, a
/// `/dev/` path, or one of its partitions (`ada1p3`).
pub fn leaf_matches(leaf: &str, disk: &str) -> bool {
    let leaf = leaf.strip_prefix("/dev/").unwrap_or(leaf);
    leaf == disk || leaf.strip_prefix(disk).is_some_and(|rest| rest.strip_prefix('p').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
}

/// The disk's state and error counts from the `config:` table of `zpool status -p`.
pub fn parse_leaf(status: &str, disk: &str) -> Option<(String, ErrorCounts)> {
    let mut in_config = false;
    for line in status.lines() {
        if line.trim_start().starts_with("config:") {
            in_config = true;
            continue;
        }
        if !in_config {
            continue;
        }
        if let [name, state, read, write, cksum, ..] = line.split_whitespace().collect::<Vec<_>>().as_slice() {
            if leaf_matches(name, disk) {
                let count = |s: &str| s.parse().unwrap_or(0);
                return Some(((*state).to_string(), ErrorCounts { read: count(read), write: count(write), cksum: count(cksum) }));
            }
        }
    }
    None
}

type IostatParser = fn(&str, &str) -> Option<(PoolIo, DiskLive)>;

/// One refresh for `disk`, a member of `pool` if given. Blocks for about a
/// second (the iostat window), so it belongs on a worker thread.
pub fn sample_disk(runner: &dyn CommandRunner, disk: &str, pool: Option<&str>, os: Option<HostOs>) -> Result<(PoolIo, DiskLive)> {
    // `iostat` is not the same tool everywhere: the FreeBSD one takes -w and -c, the others an interval and a count.
    let (args, parse): (&[&str], IostatParser) = match os {
        Some(HostOs::Illumos) => (&["-xn", "1", "2"], parse_iostat_illumos),
        Some(HostOs::Linux) => (&["-dx", "1", "2"], parse_iostat_linux),
        _ => (&["-x", "-d", "-w", "1", "-c", "2"], parse_iostat_x),
    };
    let out = run_checked_unprivileged(runner, args)?;
    let (io, mut live) = parse(&out, disk).ok_or_else(|| AppError::Parse(format!("{disk} not found in iostat output")))?;
    if let Some(pool) = pool {
        if let Some(status) = runner.run("zpool", &["status", "-p", "--", pool]).ok().filter(crate::exec::CommandOutput::success) {
            if let Some((state, errors)) = parse_leaf(&status.stdout, disk) {
                live.state = Some(state);
                live.errors = Some(errors);
            }
        }
    }
    Ok((io, live))
}

fn run_checked_unprivileged(runner: &dyn CommandRunner, args: &[&str]) -> Result<String> {
    let out = runner.run_unprivileged("iostat", args)?;
    if out.success() {
        Ok(out.stdout)
    } else {
        Err(AppError::CommandFailed { cmd: "iostat -x".to_string(), status: out.status, stderr: out.stderr })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    // Captured from the FreeBSD 15.1 hub: the since-boot block, then a live one-second block.
    const IOSTAT: &str = "                        extended device statistics  \ndevice       r/s     w/s     kr/s     kw/s  ms/r  ms/w  ms/o  ms/t qlen  %b  \nada1           9       9    900.0    900.0     9     9     9     9    9  99 \n                        extended device statistics  \ndevice       r/s     w/s     kr/s     kw/s  ms/r  ms/w  ms/o  ms/t qlen  %b  \nada0           5      10    208.8    258.9     2     1     4     2    0   1 \nada1           0       4      3.5      0.9     1     3     6     1    2  12 \npass0          0       0      0.0      0.0     0     0     0     0    0   0 \n";
    const STATUS: &str = "  pool: tpool\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\ttpool       ONLINE       0     0     0\n\t  raidz2-0  ONLINE       0     0     0\n\t    ada1    DEGRADED     1     2     3\n\t    ada10p3 ONLINE       0     0     0\n\nerrors: No known data errors\n";

    #[test]
    fn iostat_x_reads_the_live_block_not_the_since_boot_one() {
        let (io, live) = parse_iostat_x(IOSTAT, "ada1").unwrap();
        assert_eq!((io.read_ops, io.write_ops, io.read_bps, io.write_bps), (0, 4, 3584, 922));
        assert_eq!((io.read_latency_ns, io.write_latency_ns), (None, Some(3_000_000)), "no reads, so no read latency");
        assert_eq!((live.busy_percent, live.queue), (Some(12.0), Some(2)));
        assert!(parse_iostat_x(IOSTAT, "ada9").is_none());
        assert!(parse_iostat_x("", "ada1").is_none());
    }

    #[test]
    fn leaf_names_match_the_device_a_path_or_its_partitions_only() {
        assert!(leaf_matches("ada1", "ada1"));
        assert!(leaf_matches("/dev/ada1", "ada1"));
        assert!(leaf_matches("ada1p3", "ada1"));
        assert!(!leaf_matches("ada10", "ada1"), "a different disk");
        assert!(!leaf_matches("ada10p3", "ada1"));
        assert!(!leaf_matches("ada1p", "ada1"));
        assert!(!leaf_matches("ada1pool", "ada1"));
    }

    #[test]
    fn leaf_state_and_errors_come_from_the_disks_own_line() {
        let (state, errors) = parse_leaf(STATUS, "ada1").unwrap();
        assert_eq!((state.as_str(), errors), ("DEGRADED", ErrorCounts { read: 1, write: 2, cksum: 3 }));
        assert!(parse_leaf(STATUS, "ada7").is_none());
        assert!(parse_leaf("  pool: x\n ada1 ONLINE 9 9 9\n", "ada1").is_none(), "only the config table counts");
    }

    #[test]
    fn sample_disk_uses_unprivileged_iostat_and_the_pools_status() {
        let runner = FakeRunner::new("").route("iostat", "-x", IOSTAT).route("zpool", "status", STATUS);
        let (io, live) = sample_disk(&runner, "ada1", Some("tpool"), None).unwrap();
        assert_eq!(io.write_ops, 4);
        assert_eq!((live.state.as_deref(), live.errors.map(ErrorCounts::total)), (Some("DEGRADED"), Some(6)));
        let unpriv = runner.unprivileged_calls.lock().unwrap();
        assert_eq!(unpriv.len(), 1);
        assert_eq!(unpriv[0].1, vec!["-x", "-d", "-w", "1", "-c", "2"]);
    }

    #[test]
    fn a_disk_outside_any_pool_skips_zpool_status_and_a_missing_disk_is_an_error() {
        let runner = FakeRunner::new("").route("iostat", "-x", IOSTAT);
        let (_, live) = sample_disk(&runner, "ada0", None, None).unwrap();
        assert_eq!((live.state, live.errors), (None, None));
        assert!(runner.calls.lock().unwrap().iter().all(|(c, _)| c != "zpool"));
        assert!(sample_disk(&runner, "ada9", None, None).is_err());
        assert!(sample_disk(&FakeRunner::failing("no iostat", 1), "ada0", None, None).is_err());
    }

    // Captured from OmniOS r151058 (`iostat -xn 1 2`) and Ubuntu 24.04 (`iostat -dx 1 2`).
    const IOSTAT_ILLUMOS: &str = "                    extended device statistics              \n    r/s    w/s   kr/s   kw/s wait actv wsvc_t asvc_t  %w  %b device\n    4.6    1.6   46.0   14.4  0.0  0.0    0.0    0.5   0   0 c2t0d0\n  208.2   32.9 5823.5   33.4  0.0  0.1    0.0    0.4   0   9 c1t1d0\n                    extended device statistics              \n    r/s    w/s   kr/s   kw/s wait actv wsvc_t asvc_t  %w  %b device\n    6.0    0.0   33.9    0.0  0.0  0.0    0.0    0.6   0   0 c2t0d0\n  208.2   32.9 5824.0   33.5  0.0  1.4    0.0    0.4   0   9 c1t1d0\n";
    const IOSTAT_LINUX: &str = "Linux 6.8.0-146-generic (ub24) \t10/20/2026 \t_x86_64_\t(32 CPU)\n\nDevice            r/s     rkB/s   rrqm/s  %rrqm r_await rareq-sz     w/s     wkB/s   wrqm/s  %wrqm w_await wareq-sz     d/s     dkB/s   drqm/s  %drqm d_await dareq-sz     f/s f_await  aqu-sz  %util\nsda              9.00      9.00     0.00   0.00    9.00     9.00    9.00      9.00     0.00   0.00    9.00     9.00    0.00      0.00     0.00   0.00    0.00     0.00    0.00    0.00    9.00  99.00\n\nDevice            r/s     rkB/s   rrqm/s  %rrqm r_await rareq-sz     w/s     wkB/s   wrqm/s  %wrqm w_await wareq-sz     d/s     dkB/s   drqm/s  %drqm d_await dareq-sz     f/s f_await  aqu-sz  %util\nsda              2.00     16.00     0.00   0.00    1.50     8.00    0.00      0.00     0.00   0.00    0.00     0.00    0.00      0.00     0.00   0.00    0.00     0.00    0.00    0.00    3.40  12.50\n";

    #[test]
    fn illumos_iostat_reads_the_live_block_by_the_device_name_in_the_last_column() {
        let (io, live) = parse_iostat_illumos(IOSTAT_ILLUMOS, "c1t1d0").unwrap();
        assert_eq!((io.read_ops, io.write_ops, io.read_bps, io.write_bps), (208, 33, 5_963_776, 34_304));
        assert_eq!((io.read_latency_ns, io.write_latency_ns), (Some(400_000), Some(400_000)));
        assert_eq!((live.busy_percent, live.queue), (Some(9.0), Some(1)));
        let (io, _) = parse_iostat_illumos(IOSTAT_ILLUMOS, "c2t0d0").unwrap();
        assert_eq!(io.write_latency_ns, None, "no writes, so no write latency");
        assert!(parse_iostat_illumos(IOSTAT_ILLUMOS, "c9t9d9").is_none() && parse_iostat_illumos("", "c1t1d0").is_none());
    }

    #[test]
    fn linux_iostat_finds_its_columns_by_header_name_in_the_last_block() {
        let (io, live) = parse_iostat_linux(IOSTAT_LINUX, "sda").unwrap();
        assert_eq!((io.read_ops, io.write_ops, io.read_bps, io.write_bps), (2, 0, 16_384, 0));
        assert_eq!((io.read_latency_ns, io.write_latency_ns), (Some(1_500_000), None));
        assert_eq!((live.busy_percent, live.queue), (Some(12.5), Some(3)));
        assert!(parse_iostat_linux(IOSTAT_LINUX, "sdb").is_none() && parse_iostat_linux("", "sda").is_none());
    }

    #[test]
    fn each_os_gets_the_iostat_arguments_its_tool_accepts() {
        let runner = FakeRunner::new("").route("iostat", "-xn", IOSTAT_ILLUMOS);
        assert!(sample_disk(&runner, "c1t1d0", None, Some(HostOs::Illumos)).is_ok());
        assert_eq!(runner.unprivileged_calls.lock().unwrap()[0].1, vec!["-xn", "1", "2"]);
        let runner = FakeRunner::new("").route("iostat", "-dx", IOSTAT_LINUX);
        assert!(sample_disk(&runner, "sda", None, Some(HostOs::Linux)).is_ok());
        assert_eq!(runner.unprivileged_calls.lock().unwrap()[0].1, vec!["-dx", "1", "2"]);
    }
}
