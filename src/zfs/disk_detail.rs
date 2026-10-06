//! Live activity and health for one highlighted disk.
//!
//! Activity comes from `iostat -x`, which needs no privilege (so no sudoers
//! entry on managed hosts) and covers every GEOM provider. Health is the
//! disk's own line in its pool's `zpool status` table. The disk must show up
//! in `iostat`; a missing pool line only leaves the health row empty.

use super::metrics::{ErrorCounts, PoolIo};
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

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

/// One refresh for `disk`, a member of `pool` if given. Blocks for about a
/// second (the iostat window), so it belongs on a worker thread.
pub fn sample_disk(runner: &dyn CommandRunner, disk: &str, pool: Option<&str>) -> Result<(PoolIo, DiskLive)> {
    let out = run_checked_unprivileged(runner, &["-x", "-d", "-w", "1", "-c", "2"])?;
    let (io, mut live) = parse_iostat_x(&out, disk).ok_or_else(|| AppError::Parse(format!("{disk} not found in iostat output")))?;
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
        let (io, live) = sample_disk(&runner, "ada1", Some("tpool")).unwrap();
        assert_eq!(io.write_ops, 4);
        assert_eq!((live.state.as_deref(), live.errors.map(ErrorCounts::total)), (Some("DEGRADED"), Some(6)));
        let unpriv = runner.unprivileged_calls.lock().unwrap();
        assert_eq!(unpriv.len(), 1);
        assert_eq!(unpriv[0].1, vec!["-x", "-d", "-w", "1", "-c", "2"]);
    }

    #[test]
    fn a_disk_outside_any_pool_skips_zpool_status_and_a_missing_disk_is_an_error() {
        let runner = FakeRunner::new("").route("iostat", "-x", IOSTAT);
        let (_, live) = sample_disk(&runner, "ada0", None).unwrap();
        assert_eq!((live.state, live.errors), (None, None));
        assert!(runner.calls.lock().unwrap().iter().all(|(c, _)| c != "zpool"));
        assert!(sample_disk(&runner, "ada9", None).is_err());
        assert!(sample_disk(&FakeRunner::failing("no iostat", 1), "ada0", None).is_err());
    }
}
