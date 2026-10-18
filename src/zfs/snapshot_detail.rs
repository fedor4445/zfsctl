//! Everything the metrics panel shows about one highlighted snapshot.
//!
//! Only the snapshot's own properties are required. The dry-run destroy,
//! the sibling listing and the parent dataset's totals are best-effort, so
//! a failure in one of them leaves that line as `-` rather than blanking
//! the panel. The dry run only ever passes `-n`: it must never destroy.

use super::{parse_numeric_or_dash, run_checked};
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use std::collections::HashMap;

/// One snapshot of the same dataset, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    pub name: String,
    pub created: u64,
    pub used: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapshotDetail {
    /// The full `dataset@snapshot` name this was fetched for.
    pub full_name: String,
    pub used: u64,
    pub referenced: u64,
    pub written: u64,
    /// Creation time, seconds since the epoch.
    pub created: u64,
    /// Number of user holds; a held snapshot cannot be destroyed.
    pub holds: u64,
    pub clones: Vec<String>,
    /// Space destroying just this snapshot would free (`zfs destroy -n`).
    pub reclaim: Option<u64>,
    pub siblings: Vec<SnapshotEntry>,
    pub dataset_used: u64,
    pub dataset_snapshots_used: u64,
}

impl SnapshotDetail {
    /// Position of this snapshot among its siblings (oldest first).
    pub fn position(&self) -> Option<usize> {
        self.siblings.iter().position(|s| s.name == self.full_name)
    }

    /// Whether a real destroy would be refused.
    pub fn blocked(&self) -> bool {
        self.holds > 0 || !self.clones.is_empty()
    }
}

fn property_map(output: &str) -> HashMap<&str, &str> {
    output.lines().filter_map(|l| l.split_once('\t')).collect()
}

/// Parses the `reclaim` line of `zfs destroy -nvp`.
pub fn parse_reclaim(output: &str) -> Option<u64> {
    property_map(output).get("reclaim").and_then(|v| v.parse().ok())
}

/// Parses `zfs list -t snapshot -H -p -o name,creation,used -s creation -d 1 <dataset>`.
pub fn parse_siblings(output: &str) -> Vec<SnapshotEntry> {
    output
        .lines()
        .filter_map(|line| {
            let &[name, created, used] = line.split('\t').collect::<Vec<_>>().as_slice() else { return None };
            Some(SnapshotEntry { name: name.to_string(), created: created.parse().ok()?, used: parse_numeric_or_dash(used)? })
        })
        .collect()
}

/// Parses `zfs get -Hp -o property,value used,referenced,written,creation,clones,userrefs`.
fn parse_snapshot_props(output: &str, full_name: &str) -> Result<SnapshotDetail> {
    let values = property_map(output);
    let Some(created) = values.get("creation").and_then(|v| v.parse().ok()) else {
        return Err(AppError::Parse(format!("unexpected zfs get output for {full_name}: {:?}", output.lines().next().unwrap_or(""))));
    };
    let num = |k: &str| values.get(k).and_then(|v| parse_numeric_or_dash(v)).unwrap_or(0);
    let clones = match values.get("clones") {
        Some(v) if *v != "-" && !v.is_empty() => v.split(',').map(str::to_string).collect(),
        _ => Vec::new(),
    };
    Ok(SnapshotDetail {
        full_name: full_name.to_string(),
        used: num("used"),
        referenced: num("referenced"),
        written: num("written"),
        created,
        holds: num("userrefs"),
        clones,
        ..SnapshotDetail::default()
    })
}

/// One full panel refresh for `full_name` (`dataset@snapshot`).
pub fn sample_snapshot(runner: &dyn CommandRunner, full_name: &str) -> Result<SnapshotDetail> {
    let Some((dataset, _)) = full_name.split_once('@') else {
        return Err(AppError::Parse(format!("not a snapshot name: {full_name:?}")));
    };
    let props = run_checked(
        runner,
        "zfs",
        &["get", "-Hp", "-o", "property,value", "used,referenced,written,creation,clones,userrefs", full_name],
        &format!("zfs get {full_name}"),
    )?;
    let mut detail = parse_snapshot_props(&props.stdout, full_name)?;
    // `-n` makes this a dry run; `-v -p` print the `reclaim` figure in parseable form.
    detail.reclaim = runner
        .run("zfs", &["destroy", "-nvp", "--", full_name])
        .ok()
        .filter(crate::exec::CommandOutput::success)
        .and_then(|o| parse_reclaim(&o.stdout));
    detail.siblings = runner
        .run("zfs", &["list", "-t", "snapshot", "-H", "-p", "-o", "name,creation,used", "-s", "creation", "-d", "1", "--", dataset])
        .ok()
        .filter(crate::exec::CommandOutput::success)
        .map(|o| parse_siblings(&o.stdout))
        .unwrap_or_default();
    if let Some(out) = runner
        .run("zfs", &["get", "-Hp", "-o", "property,value", "used,usedbysnapshots", dataset])
        .ok()
        .filter(crate::exec::CommandOutput::success)
    {
        let totals = property_map(&out.stdout);
        let num = |k: &str| totals.get(k).and_then(|v| parse_numeric_or_dash(v)).unwrap_or(0);
        detail.dataset_used = num("used");
        detail.dataset_snapshots_used = num("usedbysnapshots");
    }
    Ok(detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    // Captured from the FreeBSD 15.1 hub.
    const PROPS: &str = "used\t65536\nreferenced\t98304\nwritten\t98304\ncreation\t1788654448\nclones\t-\nuserrefs\t0\n";
    const SIBLINGS: &str = "scratch/data@snap1\t1788654448\t65536\nscratch/data@snap2\t1788654643\t65536\nscratch/data@snap4\t1790993259\t4096\n";

    #[test]
    fn props_parse_sizes_age_holds_and_clones() {
        let d = parse_snapshot_props(PROPS, "scratch/data@snap1").unwrap();
        assert_eq!((d.used, d.referenced, d.written, d.created, d.holds), (65536, 98304, 98304, 1_788_654_448, 0));
        assert!(d.clones.is_empty() && !d.blocked());
        let held = parse_snapshot_props("creation\t5\nuserrefs\t2\nclones\ttank/a,tank/b\n", "t@s").unwrap();
        assert_eq!((held.holds, held.clones.len(), held.blocked()), (2, 2, true));
        assert!(parse_snapshot_props("nonsense", "t@s").is_err());
    }

    #[test]
    fn reclaim_comes_from_the_dry_run_output() {
        assert_eq!(parse_reclaim("destroy\tscratch/data@snap1\nreclaim\t65536\n"), Some(65536));
        assert_eq!(parse_reclaim("destroy\tx\n"), None);
    }

    #[test]
    fn siblings_keep_creation_order_and_skip_malformed_lines() {
        let s = parse_siblings(&format!("{SIBLINGS}garbage\n"));
        assert_eq!(s.len(), 3);
        assert_eq!((s[2].name.as_str(), s[2].used), ("scratch/data@snap4", 4096));
    }

    #[test]
    fn sample_snapshot_gathers_everything_and_only_ever_dry_runs_destroy() {
        let runner = FakeRunner::new("")
            .route("zfs", "get", PROPS)
            .route("zfs", "destroy", "destroy\tscratch/data@snap2\nreclaim\t4096\n")
            .route("zfs", "list", SIBLINGS);
        let d = sample_snapshot(&runner, "scratch/data@snap2").unwrap();
        assert_eq!((d.reclaim, d.siblings.len(), d.position()), (Some(4096), 3, Some(1)));
        let calls = runner.calls.lock().unwrap();
        let destroys: Vec<_> = calls.iter().filter(|(_, a)| a.first().is_some_and(|f| f == "destroy")).collect();
        assert_eq!(destroys.len(), 1);
        assert_eq!(destroys[0].1, vec!["destroy", "-nvp", "--", "scratch/data@snap2"]);
        assert!(calls.iter().any(|(_, a)| a.last().is_some_and(|l| l == "scratch/data")), "dataset totals are fetched for the parent");
    }

    #[test]
    fn a_failing_extra_command_leaves_only_its_own_field_empty() {
        let runner = FakeRunner::new("").route("zfs", "get", PROPS).route_failure("zfs", "destroy", "boom", 1).route_failure("zfs", "list", "boom", 1);
        let d = sample_snapshot(&runner, "scratch/data@snap1").unwrap();
        assert_eq!((d.reclaim, d.siblings.len(), d.used), (None, 0, 65536));
        assert!(sample_snapshot(&FakeRunner::failing("no such snapshot", 1), "a@b").is_err());
        assert!(sample_snapshot(&runner, "no-at-sign").is_err());
    }
}
