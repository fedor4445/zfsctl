//! Live activity and space metrics for the highlighted pool / dataset.
//!
//! Everything here is read-only `zpool`/`zfs` output parsed leniently: a
//! value a given `OpenZFS` version doesn't print (latency without `-l`, `-`
//! for an idle pool, `recordsize` on a zvol) becomes `None`/`0` instead of
//! an error, so a quirk on one host never blanks the whole panel.

use super::{parse_numeric_or_dash, run_checked};
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use std::collections::HashMap;

/// One second of pool I/O, as per-second rates.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PoolIo {
    pub read_ops: u64,
    pub write_ops: u64,
    pub read_bps: u64,
    pub write_bps: u64,
    /// Average total wait per request in nanoseconds; `None` when idle
    /// (`zpool iostat -l` prints `-`).
    pub read_latency_ns: Option<u64>,
    pub write_latency_ns: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ErrorCounts {
    pub read: u64,
    pub write: u64,
    pub cksum: u64,
}

impl ErrorCounts {
    pub fn total(self) -> u64 {
        self.read.saturating_add(self.write).saturating_add(self.cksum)
    }
}

/// The slow-moving facts shown beside the I/O graphs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PoolFacts {
    pub size: u64,
    pub allocated: u64,
    pub free: u64,
    /// Free-space fragmentation percent; `None` when the pool doesn't report one.
    pub fragmentation: Option<u8>,
    pub dedup_ratio: f64,
    pub errors: ErrorCounts,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PoolSample {
    pub io: PoolIo,
    pub facts: PoolFacts,
}

fn bad(what: &str, output: &str) -> AppError {
    AppError::Parse(format!("unexpected {what} output: {:?}", output.lines().next().unwrap_or("")))
}

fn optional_ns(field: Option<&&str>) -> Option<u64> {
    field.and_then(|s| s.parse().ok())
}

/// Parses `zpool iostat -Hpyl <pool> 1 1`: name, alloc, free, read ops,
/// write ops, read bw, write bw, then (with `-l`) total-wait read/write
/// and more columns this panel doesn't use.
pub fn parse_iostat(output: &str, pool: &str) -> Result<PoolIo> {
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if let [name, _alloc, _free, read_ops, write_ops, read_bps, write_bps, rest @ ..] = fields.as_slice() {
            if *name != pool {
                continue;
            }
            let num = |s: &str| parse_numeric_or_dash(s).ok_or_else(|| bad("zpool iostat", output));
            return Ok(PoolIo {
                read_ops: num(read_ops)?,
                write_ops: num(write_ops)?,
                read_bps: num(read_bps)?,
                write_bps: num(write_bps)?,
                read_latency_ns: optional_ns(rest.first()),
                write_latency_ns: optional_ns(rest.get(1)),
            });
        }
    }
    Err(bad("zpool iostat", output))
}

/// Parses `zpool list -Hp -o name,size,alloc,free,frag,dedupratio <pool>`.
fn parse_list_facts(output: &str) -> Result<PoolFacts> {
    let line = output.lines().find(|l| !l.trim().is_empty()).ok_or_else(|| bad("zpool list", output))?;
    let fields: Vec<&str> = line.split('\t').collect();
    let &[_, size, alloc, free, frag, dedup] = fields.as_slice() else {
        return Err(bad("zpool list", output));
    };
    Ok(PoolFacts {
        size: parse_numeric_or_dash(size).unwrap_or(0),
        allocated: parse_numeric_or_dash(alloc).unwrap_or(0),
        free: parse_numeric_or_dash(free).unwrap_or(0),
        fragmentation: frag.trim_end_matches('%').parse().ok(),
        dedup_ratio: dedup.trim_end_matches('x').parse().unwrap_or(1.0),
        errors: ErrorCounts::default(),
    })
}

/// The pool's own READ/WRITE/CKSUM totals from `zpool status -p <pool>`:
/// the line in the `config:` table whose name is the pool. Zero when the
/// table or the line is missing (an unreadable status must not look like errors).
pub fn parse_error_totals(status: &str, pool: &str) -> ErrorCounts {
    let mut in_config = false;
    for line in status.lines() {
        if line.trim_start().starts_with("config:") {
            in_config = true;
            continue;
        }
        if !in_config {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if let [name, _state, read, write, cksum, ..] = fields.as_slice() {
            if *name == pool {
                let count = |s: &str| s.parse().unwrap_or(0);
                return ErrorCounts { read: count(read), write: count(write), cksum: count(cksum) };
            }
        }
    }
    ErrorCounts::default()
}

/// One pool sample: a 1-second iostat window plus the slow facts. Blocks
/// for about a second (the iostat window), so it belongs on a worker thread.
pub fn sample_pool(runner: &dyn CommandRunner, pool: &str) -> Result<PoolSample> {
    let io_out = run_checked(runner, "zpool", &["iostat", "-Hpyl", pool, "1", "1"], &format!("zpool iostat {pool}"))?;
    let list_out = run_checked(
        runner,
        "zpool",
        &["list", "-Hp", "-o", "name,size,alloc,free,frag,dedupratio", pool],
        &format!("zpool list {pool}"),
    )?;
    let status_out = run_checked(runner, "zpool", &["status", "-p", pool], &format!("zpool status {pool}"))?;
    let mut facts = parse_list_facts(&list_out.stdout)?;
    facts.errors = parse_error_totals(&status_out.stdout, pool);
    Ok(PoolSample { io: parse_iostat(&io_out.stdout, pool)?, facts })
}

/// Where a dataset's space goes, plus the settings that bound it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DatasetSpace {
    pub used: u64,
    pub available: u64,
    pub referenced: u64,
    pub logical_used: u64,
    pub by_dataset: u64,
    pub by_snapshots: u64,
    pub by_children: u64,
    pub by_refreservation: u64,
    pub compress_ratio: f64,
    pub compression: String,
    /// `0` means unset, as `zfs get -p` prints it.
    pub quota: u64,
    pub refquota: u64,
    pub reservation: u64,
    pub refreservation: u64,
    pub recordsize: u64,
}

pub const DATASET_SPACE_PROPS: &str = "usedbydataset,usedbysnapshots,usedbychildren,usedbyrefreservation,used,available,referenced,logicalused,compressratio,compression,quota,refquota,reservation,refreservation,recordsize";

/// Parses `zfs get -Hp -o property,value <DATASET_SPACE_PROPS> <dataset>`.
pub fn parse_dataset_space(output: &str) -> Result<DatasetSpace> {
    let values: HashMap<&str, &str> = output.lines().filter_map(|l| l.split_once('\t')).collect();
    if !values.contains_key("used") {
        return Err(bad("zfs get", output));
    }
    let num = |k: &str| values.get(k).and_then(|v| parse_numeric_or_dash(v)).unwrap_or(0);
    Ok(DatasetSpace {
        used: num("used"),
        available: num("available"),
        referenced: num("referenced"),
        logical_used: num("logicalused"),
        by_dataset: num("usedbydataset"),
        by_snapshots: num("usedbysnapshots"),
        by_children: num("usedbychildren"),
        by_refreservation: num("usedbyrefreservation"),
        compress_ratio: values.get("compressratio").and_then(|v| v.trim_end_matches('x').parse().ok()).unwrap_or(1.0),
        compression: values.get("compression").map(|v| (*v).to_string()).unwrap_or_default(),
        quota: num("quota"),
        refquota: num("refquota"),
        reservation: num("reservation"),
        refreservation: num("refreservation"),
        recordsize: num("recordsize"),
    })
}

pub fn fetch_dataset_space(runner: &dyn CommandRunner, dataset: &str) -> Result<DatasetSpace> {
    let out = run_checked(
        runner,
        "zfs",
        &["get", "-Hp", "-o", "property,value", DATASET_SPACE_PROPS, dataset],
        &format!("zfs get space {dataset}"),
    )?;
    parse_dataset_space(&out.stdout)
}

/// What the metrics panel is currently about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricsTarget {
    Pool(String),
    Dataset(String),
    /// The selected host as a whole (Hosts section).
    Host,
    /// A full `dataset@snapshot` name.
    Snapshot(String),
    /// A disk and, if it belongs to one, its pool (whose status gives its health).
    Disk { name: String, pool: Option<String> },
    /// A share on `dataset`; `smb_name` is `Some` for SMB and `None` for NFS.
    Share { dataset: String, smb_name: Option<String> },
}

impl MetricsTarget {
    /// The pool whose I/O the panel graphs: a dataset has no iostat of its
    /// own, so it shows its pool's. A host graphs all its pools together.
    pub fn pool(&self) -> Option<&str> {
        match self {
            MetricsTarget::Pool(p) => Some(p),
            MetricsTarget::Dataset(d) | MetricsTarget::Share { dataset: d, .. } => Some(d.split('/').next().unwrap_or(d)),
            MetricsTarget::Host | MetricsTarget::Snapshot(_) | MetricsTarget::Disk { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetSample {
    pub pool: PoolSample,
    /// Present only for a dataset target.
    pub space: Option<DatasetSpace>,
    /// Present only for a host target; `pool.io` is then the all-pools total.
    pub host: Option<super::host::HostSummary>,
    /// Present only for a snapshot target; `pool` is then an unused default.
    pub snapshot: Option<super::snapshot_detail::SnapshotDetail>,
    /// Present only for a disk target; `pool.io` is then that disk's activity.
    pub disk: Option<super::disk_detail::DiskLive>,
    /// Present only for a share target (alongside `space`).
    pub share: Option<super::share_detail::ShareLive>,
}

/// One full panel refresh. Blocks for about a second; run it off the UI thread.
pub fn sample_target(runner: &dyn CommandRunner, target: &MetricsTarget) -> Result<TargetSample> {
    let Some(pool_name) = target.pool() else {
        let mut sample = TargetSample { pool: PoolSample::default(), space: None, host: None, snapshot: None, disk: None, share: None };
        if let MetricsTarget::Snapshot(name) = target {
            sample.snapshot = Some(super::snapshot_detail::sample_snapshot(runner, name)?);
        } else if let MetricsTarget::Disk { name, pool } = target {
            let (io, live) = super::disk_detail::sample_disk(runner, name, pool.as_deref())?;
            sample.pool.io = io;
            sample.disk = Some(live);
        } else {
            let (host, io) = super::host::sample_host(runner)?;
            sample.pool.io = io;
            sample.host = Some(host);
        }
        return Ok(sample);
    };
    let pool = sample_pool(runner, pool_name)?;
    let space = match target {
        MetricsTarget::Dataset(d) | MetricsTarget::Share { dataset: d, .. } => Some(fetch_dataset_space(runner, d)?),
        _ => None,
    };
    let share = match target {
        MetricsTarget::Share { dataset, smb_name } => Some(super::share_detail::sample_share(runner, dataset, smb_name.as_deref())),
        _ => None,
    };
    Ok(TargetSample { pool, space, host: None, snapshot: None, disk: None, share })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    #[test]
    fn a_dataset_target_graphs_its_pools_io() {
        assert_eq!(MetricsTarget::Dataset("tank/a/b".into()).pool(), Some("tank"));
        assert_eq!(MetricsTarget::Dataset("tank".into()).pool(), Some("tank"));
        assert_eq!(MetricsTarget::Pool("tank".into()).pool(), Some("tank"));
        assert_eq!(MetricsTarget::Host.pool(), None);
    }

    #[test]
    fn sample_target_fetches_dataset_space_only_for_datasets() {
        let runner = FakeRunner::new(SPACE)
            .route("zpool", "iostat", IOSTAT_BUSY)
            .route("zpool", "list", "tank\t3000\t1000\t2000\t12\t1.00\n")
            .route("zpool", "status", "config:\n\ttank ONLINE 0 0 0\n");
        assert!(sample_target(&runner, &MetricsTarget::Pool("tank".into())).unwrap().space.is_none());
        let s = sample_target(&runner, &MetricsTarget::Dataset("tank/data".into())).unwrap();
        assert_eq!(s.space.unwrap().used, 105_234_432);
        assert_eq!(s.pool.io.read_ops, 840);
    }

    // Captured from FreeBSD 15.1 (OpenZFS 2.4), `zpool iostat -Hpyl scratch 1 1`
    const IOSTAT_IDLE: &str = "scratch\t106217472\t145440768\t0\t0\t0\t0\t-\t-\t-\t-\t-\t-\t-\t-\t-\t-\t-\n";
    const IOSTAT_BUSY: &str = "tank\t1000\t2000\t840\t12\t88080384\t4096\t1200000\t3400000\t0\t0\t0\t0\t0\t0\t0\t0\t0\n";
    const STATUS: &str = "  pool: scratch\n state: ONLINE\n  scan: scrub repaired 0B in 00:00:00 with 0 errors on Sat Sep 26 11:53:24 2026\nconfig:\n\n\tNAME                            STATE     READ WRITE CKSUM\n\tscratch                         ONLINE       3     1     7\n\t  mirror-0                      ONLINE       0     0     0\n\t    /root/zfsctl-scratch/vdev3  ONLINE       9     9     9\n\nerrors: No known data errors\n";
    const SPACE: &str = "usedbydataset\t105046016\nusedbysnapshots\t188416\nusedbychildren\t0\nusedbyrefreservation\t0\nused\t105234432\navailable\t19611648\nreferenced\t105046016\nlogicalused\t105027072\ncompressratio\t1.00\nquota\t0\nrefquota\t0\nreservation\t0\nrefreservation\t0\nrecordsize\t131072\ncompression\ton\n";

    #[test]
    fn iostat_idle_pool_has_zero_rates_and_no_latency() {
        assert_eq!(parse_iostat(IOSTAT_IDLE, "scratch").unwrap(), PoolIo::default());
    }

    #[test]
    fn iostat_reads_ops_bandwidth_and_total_wait_latency() {
        let io = parse_iostat(IOSTAT_BUSY, "tank").unwrap();
        assert_eq!((io.read_ops, io.write_ops, io.read_bps, io.write_bps), (840, 12, 88_080_384, 4096));
        assert_eq!((io.read_latency_ns, io.write_latency_ns), (Some(1_200_000), Some(3_400_000)));
    }

    #[test]
    fn iostat_without_latency_columns_still_parses() {
        let io = parse_iostat("tank\t1\t2\t5\t6\t7\t8\n", "tank").unwrap();
        assert_eq!((io.read_ops, io.write_bps, io.read_latency_ns), (5, 8, None));
    }

    #[test]
    fn iostat_for_a_different_pool_or_garbage_is_an_error() {
        assert!(parse_iostat(IOSTAT_BUSY, "other").is_err());
        assert!(parse_iostat("", "tank").is_err());
        assert!(parse_iostat("tank\t1\t2\tx\t6\t7\t8\n", "tank").is_err());
    }

    #[test]
    fn list_facts_parse_size_fragmentation_and_dedup() {
        let f = parse_list_facts("scratch\t251658240\t106217472\t145440768\t32\t1.00\n").unwrap();
        assert_eq!((f.size, f.allocated, f.free, f.fragmentation), (251_658_240, 106_217_472, 145_440_768, Some(32)));
        assert!((f.dedup_ratio - 1.0).abs() < f64::EPSILON);
        let f = parse_list_facts("t\t1\t1\t0\t-\t2.50x\n").unwrap();
        assert_eq!(f.fragmentation, None, "`-` means the pool reports no fragmentation");
        assert!((f.dedup_ratio - 2.5).abs() < f64::EPSILON);
        assert!(parse_list_facts("only\ttwo\n").is_err());
    }

    #[test]
    fn error_totals_come_from_the_pool_line_not_its_children() {
        assert_eq!(parse_error_totals(STATUS, "scratch"), ErrorCounts { read: 3, write: 1, cksum: 7 });
        assert_eq!(parse_error_totals(STATUS, "scratch").total(), 11);
        assert_eq!(parse_error_totals(STATUS, "missing"), ErrorCounts::default());
        assert_eq!(parse_error_totals("", "scratch"), ErrorCounts::default());
    }

    #[test]
    fn error_totals_ignore_a_pool_name_that_appears_before_config() {
        let status = "  pool: scratch\n scratch ONLINE 9 9 9\nconfig:\n\tscratch ONLINE 1 2 3\n";
        assert_eq!(parse_error_totals(status, "scratch"), ErrorCounts { read: 1, write: 2, cksum: 3 });
    }

    #[test]
    fn dataset_space_parses_the_real_zfs_get_output() {
        let s = parse_dataset_space(SPACE).unwrap();
        assert_eq!((s.by_dataset, s.by_snapshots, s.by_children, s.by_refreservation), (105_046_016, 188_416, 0, 0));
        assert_eq!((s.used, s.available, s.referenced, s.logical_used), (105_234_432, 19_611_648, 105_046_016, 105_027_072));
        assert_eq!((s.quota, s.refquota, s.reservation, s.recordsize), (0, 0, 0, 131_072));
        assert_eq!(s.compression, "on");
        assert!((s.compress_ratio - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dataset_space_tolerates_dashes_and_an_x_suffixed_ratio() {
        let s = parse_dataset_space("used\t10\nrecordsize\t-\ncompressratio\t1.85x\nquota\t-\n").unwrap();
        assert_eq!((s.used, s.recordsize, s.quota), (10, 0, 0));
        assert!((s.compress_ratio - 1.85).abs() < 1e-9);
        assert!(parse_dataset_space("nonsense").is_err());
    }

    #[test]
    fn sample_pool_runs_iostat_list_and_status_for_that_pool() {
        let runner = FakeRunner::new("")
            .route("zpool", "iostat", IOSTAT_BUSY)
            .route("zpool", "list", "tank\t3000\t1000\t2000\t12\t1.00\n")
            .route("zpool", "status", "config:\n\ttank ONLINE 0 0 4\n");
        let s = sample_pool(&runner, "tank").unwrap();
        assert_eq!(s.io.read_ops, 840);
        assert_eq!((s.facts.size, s.facts.fragmentation, s.facts.errors.cksum), (3000, Some(12), 4));
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1, vec!["iostat", "-Hpyl", "tank", "1", "1"]);
        assert!(calls.iter().all(|(cmd, _)| cmd == "zpool"));
    }

    #[test]
    fn sample_pool_reports_a_failing_command_as_an_error() {
        assert!(sample_pool(&FakeRunner::failing("no such pool", 1), "tank").is_err());
    }

    #[test]
    fn fetch_dataset_space_asks_for_exactly_the_documented_properties() {
        let runner = FakeRunner::new(SPACE);
        let s = fetch_dataset_space(&runner, "scratch/data").unwrap();
        assert_eq!(s.used, 105_234_432);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].0, "zfs");
        assert_eq!(calls[0].1, vec!["get", "-Hp", "-o", "property,value", DATASET_SPACE_PROPS, "scratch/data"]);
    }
}
