use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use crate::zfs::property::Property;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolHealth {
    Online,
    Degraded,
    Faulted,
    Offline,
    Unavail,
    Removed,
    Other(String),
}

impl PoolHealth {
    pub fn parse(s: &str) -> PoolHealth {
        match s {
            "ONLINE" => PoolHealth::Online,
            "DEGRADED" => PoolHealth::Degraded,
            "FAULTED" => PoolHealth::Faulted,
            "OFFLINE" => PoolHealth::Offline,
            "UNAVAIL" => PoolHealth::Unavail,
            "REMOVED" => PoolHealth::Removed,
            other => PoolHealth::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pool {
    pub name: String,
    pub size: u64,
    pub allocated: u64,
    pub free: u64,
    pub health: PoolHealth,
}

pub const POOL_LIST_ARGS: &[&str] = &["list", "-H", "-p", "-o", "name,size,alloc,free,health"];

/// Parses `zpool list -H -p -o name,size,alloc,free,health` output
/// (tab-separated, one pool per line).
pub fn parse_pool_list(output: &str) -> Result<Vec<Pool>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let &[name, size, alloc, free, health] = fields.as_slice() else {
                return Err(AppError::Parse(format!(
                    "expected 5 fields in zpool list line: {line:?}"
                )));
            };
            Ok(Pool {
                name: name.to_string(),
                size: super::parse_numeric_or_dash(size)
                    .ok_or_else(|| AppError::Parse(format!("bad size in: {line:?}")))?,
                allocated: super::parse_numeric_or_dash(alloc)
                    .ok_or_else(|| AppError::Parse(format!("bad alloc in: {line:?}")))?,
                free: super::parse_numeric_or_dash(free)
                    .ok_or_else(|| AppError::Parse(format!("bad free in: {line:?}")))?,
                health: PoolHealth::parse(health),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct VdevLine {
    pub depth: usize,
    pub name: String,
    pub health: PoolHealth,
}

/// Parses the `config:` section of `zpool status <pool>` output into a
/// flat, indentation-depth-tagged list (2 spaces per nesting level under
/// `config:`). `zpool status` has no `-H -p -o` tabular form, so this is
/// parsed line-by-line instead.
pub fn parse_pool_status_vdevs(output: &str) -> Vec<VdevLine> {
    let mut result = Vec::new();
    let mut in_config = false;
    for line in output.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("config:") {
            in_config = true;
            continue;
        }
        if !in_config {
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        // If line has no leading whitespace, we're at a new section
        if line.chars().next().is_some_and(|c| !c.is_whitespace()) {
            break;
        }
        if trimmed.starts_with("NAME") && trimmed.contains("STATE") {
            continue;
        }
        let indent = line.len() - trimmed.len();
        let mut fields = trimmed.split_whitespace();
        let name = match fields.next() {
            Some(n) => n.to_string(),
            None => continue,
        };
        let state = fields.next().unwrap_or("UNKNOWN");
        result.push(VdevLine {
            depth: indent / 2,
            name,
            health: PoolHealth::parse(state),
        });
    }
    result
}

/// True unless `name` matches ZFS's group-vdev naming shape
/// (`mirror-N`, `raidz1-N`, `raidz2-N`, `raidz3-N`) — those are
/// intermediate tree nodes, not physical disks, and don't make sense
/// as an offline/online/replace target.
pub fn is_leaf_disk(name: &str) -> bool {
    const GROUP_PREFIXES: &[&str] = &["mirror-", "raidz1-", "raidz2-", "raidz3-"];
    !GROUP_PREFIXES.iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
    })
}

pub fn list_pools(runner: &dyn CommandRunner) -> Result<Vec<Pool>> {
    let output = super::run_checked(runner, "zpool", POOL_LIST_ARGS, "zpool list")?;
    parse_pool_list(&output.stdout)
}

/// The pool's most recent scrub/resilver, parsed from `zpool status`'s
/// `scan:` line. `None` (from `parse_pool_scan_status`) means the pool
/// has no scan history at all (e.g. freshly created).
#[derive(Debug, Clone, PartialEq)]
pub enum ScanStatus {
    /// A resilver or scrub is currently running. `summary` is the raw
    /// `scan:` line text, e.g. "resilver in progress since ...".
    /// `percent_done`/`eta` come from the continuation line `zpool
    /// status` prints under `scan:` once it has enough data to
    /// estimate progress (e.g. "1002M resilvered, 99.93% done,
    /// 00:00:00 to go") — both are `None` in the brief window right
    /// after a resilver/scrub starts, before that line appears.
    InProgress { summary: String, percent_done: Option<f64>, eta: Option<String> },
    /// The most recent scrub/resilver finished. `summary` is the raw
    /// `scan:` line text; `errors` is true if it reported a nonzero
    /// error count.
    Completed { summary: String, errors: bool },
}

/// True if `line` (a scan-summary line) reports a nonzero error count,
/// e.g. "resilvered 1002M in 00:02:40 with 3 errors on ...". Looks for
/// a number immediately before the word "error(s)" rather than
/// searching for a fixed substring, so it isn't tripped up by exact
/// wording differences between resilver/scrub/cancel summaries.
fn scan_summary_reports_errors(line: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    for pair in words.windows(2) {
        if let &[previous, word] = pair {
            if word == "errors" || word == "error" {
                if let Ok(count) = previous.parse::<u64>() {
                    return count > 0;
                }
            }
        }
    }
    false
}

/// Extracts the percent-done and ETA from a `zpool status` progress
/// line, e.g. "1002M resilvered, 99.93% done, 00:00:00 to go" ->
/// `(99.93, Some("00:00:00"))`. Returns `None` if `line` doesn't
/// contain the "% done" marker at all (not a progress line).
fn parse_percent_and_eta(line: &str) -> Option<(f64, Option<String>)> {
    let percent_marker = line.find("% done")?;
    let before_percent = &line[..percent_marker];
    let percent_str: String = before_percent
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    let percent: f64 = percent_str.parse().ok()?;
    let eta = line.rfind("to go").and_then(|to_go_idx| {
        let before_to_go = line[..to_go_idx].trim_end();
        before_to_go.rsplit(", ").next().map(|s| s.trim().to_string())
    });
    Some((percent, eta))
}

/// Parses the `scan:` line (and, for an in-progress scan, the
/// percent/ETA progress line beneath it) from `zpool status` output.
/// Returns `None` when the pool has never been scrubbed/resilvered (no
/// `scan:` line at all — the case for a freshly created pool) or
/// reports "none requested" (older `zpool` wording for the same
/// thing).
pub fn parse_pool_scan_status(output: &str) -> Option<ScanStatus> {
    let lines: Vec<&str> = output.lines().collect();
    let scan_idx = lines.iter().position(|l| l.trim_start().starts_with("scan:"))?;
    let summary = lines.get(scan_idx)?.trim_start().strip_prefix("scan:")?.trim().to_string();
    if summary.is_empty() || summary == "none requested" {
        return None;
    }
    if summary.contains("in progress") {
        // The percent/ETA progress line comes some number of lines
        // after `scan:`, ending at the first blank line (the boundary
        // `zpool status` puts before the `config:` section) — stop
        // there rather than scanning the whole remaining output, so an
        // unrelated later line can never be mistaken for this pool's
        // progress line.
        let mut percent_done = None;
        let mut eta = None;
        for line in lines.iter().skip(scan_idx + 1) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some((p, e)) = parse_percent_and_eta(trimmed) {
                percent_done = Some(p);
                eta = e;
                break;
            }
        }
        Some(ScanStatus::InProgress { summary, percent_done, eta })
    } else {
        let errors = scan_summary_reports_errors(&summary);
        Some(ScanStatus::Completed { summary, errors })
    }
}

/// Runs `zpool status <pool>` and parses both its vdev tree and its
/// most recent scrub/resilver status from the same output — `zpool
/// status` has no tabular `-H -p -o` form, and fetching both from one
/// call avoids a second round-trip for a second `zpool status` call
/// that would return the very same text.
pub fn get_pool_vdevs(runner: &dyn CommandRunner, pool: &str) -> Result<(Vec<VdevLine>, Option<ScanStatus>)> {
    get_pool_vdevs_with(runner, pool, false)
}

/// `get_pool_vdevs`, optionally with `-L` (OpenZFS on Linux): the vdevs named by their real paths instead of symlinks such
/// as `/dev/disk/by-id/...`.
pub fn get_pool_vdevs_with(runner: &dyn CommandRunner, pool: &str, real_paths: bool) -> Result<(Vec<VdevLine>, Option<ScanStatus>)> {
    let args: &[&str] = if real_paths { &["status", "-L", "--", pool] } else { &["status", "--", pool] };
    let output = super::run_checked(runner, "zpool", args, &format!("zpool status {pool}"))?;
    Ok((parse_pool_status_vdevs(&output.stdout), parse_pool_scan_status(&output.stdout)))
}

/// Creates a pool from a raw vdev spec, e.g. `&["mirror", "da0", "da1"]`
/// or `&["raidz1", "da0", "da1", "da2"]`. Guided vdev-spec construction
/// (picking disks/redundancy from a UI wizard) depends on a disk
/// inventory feature not built yet, so it is out of scope here — this
/// function takes an already-built spec.
pub fn create_pool(runner: &dyn CommandRunner, name: &str, vdev_spec: &[&str]) -> Result<()> {
    let mut args = vec!["create", "--", name];
    args.extend_from_slice(vdev_spec);
    super::run_checked(
        runner,
        "zpool",
        &args,
        &format!("zpool create {name} {}", vdev_spec.join(" ")),
    )?;
    Ok(())
}

/// The class a vdev row belongs to, from the depth-0 heading above it (`logs`, `cache`, `spares`, `special`, `dedup`), and the
/// device to give `zpool remove` when this row is a log, cache or spare device at the top level (a log mirror as a whole, a
/// single log disk, one cache or spare leaf). Anything else (data, special, dedup, the leaves of a log mirror) is `None`.
pub fn removable_device(lines: &[VdevLine], index: usize) -> Option<(String, &'static str)> {
    let line = lines.get(index)?;
    if line.depth != 1 {
        return None;
    }
    let class = lines[..index].iter().rev().find(|l| l.depth == 0).map(|l| l.name.as_str())?;
    let class = match class {
        "logs" => "log",
        "cache" => "cache",
        "spares" => "spare",
        _ => return None,
    };
    Some((line.name.clone(), class))
}

/// `zpool remove`: takes a log, cache or spare device out of the pool.
pub fn remove_vdev(runner: &dyn CommandRunner, pool: &str, device: &str) -> Result<()> {
    super::run_checked(runner, "zpool", &["remove", "--", pool, device], &format!("zpool remove {pool} {device}"))?;
    Ok(())
}

/// `zpool add -n`: what the pool would look like after adding `vdev_spec`, without changing anything. Options come before the
/// pool, as OmniOS's `zpool` wants.
pub fn add_vdev_dry_run(runner: &dyn CommandRunner, pool: &str, vdev_spec: &[&str]) -> Result<String> {
    let mut args = vec!["add", "-n", "--", pool];
    args.extend_from_slice(vdev_spec);
    let out = super::run_checked(runner, "zpool", &args, &format!("zpool add -n {pool} {}", vdev_spec.join(" ")))?;
    Ok(out.stdout)
}

/// Adds a new top-level vdev/mirror/raidz group to an existing pool,
/// e.g. `&["mirror", "da2", "da3"]`. Same raw-vdev-spec shape as
/// `create_pool` — no guided wizard, this takes an already-built spec.
pub fn add_vdev(runner: &dyn CommandRunner, pool: &str, vdev_spec: &[&str]) -> Result<()> {
    let mut args = vec!["add", "--", pool];
    args.extend_from_slice(vdev_spec);
    super::run_checked(
        runner,
        "zpool",
        &args,
        &format!("zpool add {pool} {}", vdev_spec.join(" ")),
    )?;
    Ok(())
}

pub fn scrub_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()> {
    super::run_checked(runner, "zpool", &["scrub", "--", pool], &format!("zpool scrub {pool}"))?;
    Ok(())
}

/// Runs `zpool resilver <pool>` — restarts an in-progress resilver from
/// the beginning (or starts one, if a device needs it and none is
/// running). Confirmed via `man zpool-resilver` and empirically against
/// a real `zpool` binary that `--` is accepted as an end-of-options
/// marker here, same as `scrub_pool` — no disk argument, this is
/// pool-scoped.
pub fn resilver_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()> {
    super::run_checked(runner, "zpool", &["resilver", "--", pool], &format!("zpool resilver {pool}"))?;
    Ok(())
}

pub fn destroy_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()> {
    super::run_checked(runner, "zpool", &["destroy", "--", pool], &format!("zpool destroy {pool}"))?;
    Ok(())
}

pub fn offline_disk(runner: &dyn CommandRunner, pool: &str, disk: &str) -> Result<()> {
    super::run_checked(
        runner,
        "zpool",
        &["offline", "--", pool, disk],
        &format!("zpool offline {pool} {disk}"),
    )?;
    Ok(())
}

pub fn online_disk(runner: &dyn CommandRunner, pool: &str, disk: &str) -> Result<()> {
    super::run_checked(
        runner,
        "zpool",
        &["online", "--", pool, disk],
        &format!("zpool online {pool} {disk}"),
    )?;
    Ok(())
}

pub fn replace_disk(runner: &dyn CommandRunner, pool: &str, old_disk: &str, new_disk: &str) -> Result<()> {
    super::run_checked(
        runner,
        "zpool",
        &["replace", "--", pool, old_disk, new_disk],
        &format!("zpool replace {pool} {old_disk} {new_disk}"),
    )?;
    Ok(())
}

/// Runs `zpool get all -H -p <pool>` and parses every property into a
/// flat list. There is no `-o` column-selection equivalent that would
/// narrow this down usefully — `all` is the standard way to fetch every
/// property at once, same as `zfs get all` in `dataset.rs`.
pub fn get_pool_properties(runner: &dyn CommandRunner, pool: &str) -> Result<Vec<Property>> {
    let output = super::run_checked(
        runner,
        "zpool",
        &["get", "all", "-H", "-p", "--", pool],
        &format!("zpool get all {pool}"),
    )?;
    crate::zfs::property::parse_property_list(&output.stdout)
}

/// No `--` guard here, unlike every other mutating function in this
/// file: `zpool set` doesn't recognize `--` as an end-of-options marker
/// at all — it tries to open a pool literally named `--` and fails
/// (confirmed against a real `zpool` binary: `cannot open '--': is not
/// a pool`). `set` takes no flags of its own, so there is no
/// flag-injection surface for `--` to guard against here in the first
/// place: `key`/`value` are joined into one argv token before `pool`,
/// which is never scanned for a leading `-` in this subcommand's
/// argument parsing.
pub fn set_pool_property(runner: &dyn CommandRunner, pool: &str, key: &str, value: &str) -> Result<()> {
    super::run_checked(
        runner,
        "zpool",
        &["set", &format!("{key}={value}"), pool],
        &format!("zpool set {key}={value} {pool}"),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    const ZPOOL_LIST_FIXTURE: &str =
        "tank\t4000000000000\t1200000000000\t2800000000000\tONLINE\nscratch\t500000000000\t100000000000\t400000000000\tDEGRADED\n";

    const ZPOOL_STATUS_FIXTURE: &str = "\
  pool: tank
 state: ONLINE
config:

\tNAME        STATE     READ WRITE CKSUM
\ttank        ONLINE       0     0     0
\t  mirror-0  ONLINE       0     0     0
\t    da0     ONLINE       0     0     0
\t    da1     ONLINE       0     0     0

errors: No known data errors
";

    #[test]
    fn parses_pool_list_fixture() {
        let pools = parse_pool_list(ZPOOL_LIST_FIXTURE).unwrap();
        assert_eq!(
            pools,
            vec![
                Pool { name: "tank".to_string(), size: 4_000_000_000_000, allocated: 1_200_000_000_000, free: 2_800_000_000_000, health: PoolHealth::Online },
                Pool { name: "scratch".to_string(), size: 500_000_000_000, allocated: 100_000_000_000, free: 400_000_000_000, health: PoolHealth::Degraded },
            ]
        );
    }

    #[test]
    fn rejects_malformed_pool_list_line() {
        let result = parse_pool_list("tank\tonly-two-fields\n");
        assert!(result.is_err());
    }

    #[test]
    fn parses_pool_list_line_with_dash_numeric_fields() {
        let pools = parse_pool_list("scratch\t-\t-\t-\tUNAVAIL\n").unwrap();
        assert_eq!(
            pools,
            vec![Pool {
                name: "scratch".to_string(),
                size: 0,
                allocated: 0,
                free: 0,
                health: PoolHealth::Unavail,
            }]
        );
    }

    #[test]
    fn parses_vdev_tree_from_status_fixture() {
        let vdevs = parse_pool_status_vdevs(ZPOOL_STATUS_FIXTURE);
        assert_eq!(
            vdevs,
            vec![
                VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Online },
                VdevLine { depth: 1, name: "mirror-0".to_string(), health: PoolHealth::Online },
                VdevLine { depth: 2, name: "da0".to_string(), health: PoolHealth::Online },
                VdevLine { depth: 2, name: "da1".to_string(), health: PoolHealth::Online },
            ]
        );
    }

    #[test]
    fn list_pools_uses_correct_command_and_parses_result() {
        let runner = FakeRunner::new(ZPOOL_LIST_FIXTURE);
        let pools = list_pools(&runner).unwrap();
        assert_eq!(pools.len(), 2);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].0, "zpool");
        assert_eq!(calls[0].1, vec!["list", "-H", "-p", "-o", "name,size,alloc,free,health"]);
    }

    #[test]
    fn list_pools_surfaces_command_failure() {
        let runner = FakeRunner::failing("permission denied", 1);
        let result = list_pools(&runner);
        assert!(result.is_err());
    }

    #[test]
    fn get_pool_vdevs_runs_status_and_parses_result() {
        let runner = FakeRunner::new(ZPOOL_STATUS_FIXTURE);
        let (vdevs, scan_status) = get_pool_vdevs(&runner, "tank").unwrap();
        assert_eq!(vdevs.len(), 4);
        assert_eq!(scan_status, None, "the fixture has no scan: line at all — a freshly created pool");
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["status".to_string(), "--".to_string(), "tank".to_string()]));
    }

    const ZPOOL_STATUS_RESILVER_IN_PROGRESS_FIXTURE: &str = "\
  pool: tpool
 state: ONLINE
status: One or more devices is currently being resilvered.  The pool will
	continue to function, possibly in a degraded state.
action: Wait for the resilver to complete.
  scan: resilver in progress since Sat Sep 26 12:09:06 2026
	4.03G / 4.04G scanned, 4.03G / 4.04G issued at 258M/s
	1002M resilvered, 99.93% done, 00:00:00 to go
config:

\tNAME        STATE     READ WRITE CKSUM
\ttpool       ONLINE       0     0     0
\t  ada1      ONLINE       0     0     0  (resilvering)

errors: No known data errors
";

    const ZPOOL_STATUS_RESILVER_COMPLETED_CLEAN_FIXTURE: &str = "\
  pool: tpool
 state: ONLINE
  scan: resilvered 1002M in 00:02:40 with 0 errors on Sat Sep 26 12:11:46 2026
config:

\tNAME   STATE     READ WRITE CKSUM
\ttpool  ONLINE       0     0     0
\t  ada1 ONLINE       0     0     0

errors: No known data errors
";

    const ZPOOL_STATUS_RESILVER_COMPLETED_WITH_ERRORS_FIXTURE: &str = "\
  pool: tpool
 state: ONLINE
  scan: resilvered 500M in 00:01:00 with 2 errors on Sat Sep 26 12:11:46 2026
config:

\tNAME   STATE     READ WRITE CKSUM
\ttpool  ONLINE       0     0     0
\t  ada1 ONLINE       0     0     0

errors: 2 data errors, use '-v' for a list
";

    #[test]
    fn parse_pool_scan_status_none_when_pool_has_never_been_scanned() {
        assert_eq!(parse_pool_scan_status(ZPOOL_STATUS_FIXTURE), None);
    }

    #[test]
    fn parse_pool_scan_status_in_progress() {
        assert_eq!(
            parse_pool_scan_status(ZPOOL_STATUS_RESILVER_IN_PROGRESS_FIXTURE),
            Some(ScanStatus::InProgress {
                summary: "resilver in progress since Sat Sep 26 12:09:06 2026".to_string(),
                percent_done: Some(99.93),
                eta: Some("00:00:00".to_string()),
            })
        );
    }

    #[test]
    fn parse_percent_and_eta_extracts_both_values() {
        assert_eq!(parse_percent_and_eta("1002M resilvered, 99.93% done, 00:00:00 to go"), Some((99.93, Some("00:00:00".to_string()))));
    }

    #[test]
    fn parse_percent_and_eta_none_for_a_line_with_no_percent_done_marker() {
        assert_eq!(parse_percent_and_eta("4.03G / 4.04G scanned, 4.03G / 4.04G issued at 258M/s"), None);
    }

    #[test]
    fn parse_pool_scan_status_in_progress_with_no_percent_line_yet() {
        let fixture = "\
  pool: tpool
 state: ONLINE
  scan: resilver in progress since Sat Sep 26 12:09:06 2026
config:

\tNAME   STATE     READ WRITE CKSUM
\ttpool  ONLINE       0     0     0

errors: No known data errors
";
        assert_eq!(
            parse_pool_scan_status(fixture),
            Some(ScanStatus::InProgress {
                summary: "resilver in progress since Sat Sep 26 12:09:06 2026".to_string(),
                percent_done: None,
                eta: None,
            }),
            "a resilver that just started may not have a percent/ETA line yet"
        );
    }

    #[test]
    fn parse_pool_scan_status_completed_clean() {
        assert_eq!(
            parse_pool_scan_status(ZPOOL_STATUS_RESILVER_COMPLETED_CLEAN_FIXTURE),
            Some(ScanStatus::Completed {
                summary: "resilvered 1002M in 00:02:40 with 0 errors on Sat Sep 26 12:11:46 2026".to_string(),
                errors: false,
            })
        );
    }

    #[test]
    fn parse_pool_scan_status_completed_with_errors() {
        assert_eq!(
            parse_pool_scan_status(ZPOOL_STATUS_RESILVER_COMPLETED_WITH_ERRORS_FIXTURE),
            Some(ScanStatus::Completed {
                summary: "resilvered 500M in 00:01:00 with 2 errors on Sat Sep 26 12:11:46 2026".to_string(),
                errors: true,
            })
        );
    }

    #[test]
    fn get_pool_vdevs_includes_in_progress_scan_status() {
        let runner = FakeRunner::new(ZPOOL_STATUS_RESILVER_IN_PROGRESS_FIXTURE);
        let (_, scan_status) = get_pool_vdevs(&runner, "tpool").unwrap();
        assert_eq!(
            scan_status,
            Some(ScanStatus::InProgress {
                summary: "resilver in progress since Sat Sep 26 12:09:06 2026".to_string(),
                percent_done: Some(99.93),
                eta: Some("00:00:00".to_string()),
            })
        );
    }

    #[test]
    fn create_pool_joins_name_and_vdev_spec() {
        let runner = FakeRunner::new("");
        create_pool(&runner, "tank", &["mirror", "da0", "da1"]).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zpool".to_string(), vec!["create".to_string(), "--".to_string(), "tank".to_string(), "mirror".to_string(), "da0".to_string(), "da1".to_string()])
        );
    }

    #[test]
    fn add_vdev_joins_pool_and_vdev_spec() {
        let runner = FakeRunner::new("");
        add_vdev(&runner, "tank", &["mirror", "da2", "da3"]).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zpool".to_string(), vec!["add".to_string(), "--".to_string(), "tank".to_string(), "mirror".to_string(), "da2".to_string(), "da3".to_string()])
        );
    }

    fn lines(spec: &[(usize, &str)]) -> Vec<VdevLine> {
        spec.iter().map(|(depth, name)| VdevLine { depth: *depth, name: (*name).to_string(), health: PoolHealth::Online }).collect()
    }

    #[test]
    fn removable_device_covers_log_cache_and_spare_at_the_top_level_only() {
        let v = lines(&[(0, "tank"), (1, "mirror-0"), (2, "da0"), (2, "da1"), (0, "logs"), (1, "mirror-1"), (2, "da2"), (2, "da3"), (1, "da4"), (0, "cache"), (1, "da5"), (0, "spares"), (1, "da6"), (0, "special"), (1, "mirror-2"), (2, "da7")]);
        let at = |name: &str| v.iter().position(|l| l.name == name).unwrap();
        assert_eq!(removable_device(&v, at("mirror-1")), Some(("mirror-1".to_string(), "log")), "a log mirror goes as a whole");
        assert_eq!(removable_device(&v, at("da4")), Some(("da4".to_string(), "log")));
        assert_eq!(removable_device(&v, at("da5")), Some(("da5".to_string(), "cache")));
        assert_eq!(removable_device(&v, at("da6")), Some(("da6".to_string(), "spare")));
        for no in ["tank", "mirror-0", "da0", "da2", "da3", "logs", "cache", "spares", "special", "mirror-2", "da7"] {
            assert_eq!(removable_device(&v, at(no)), None, "{no} is not offered");
        }
        assert_eq!(removable_device(&v, 99), None);
    }

    #[test]
    fn remove_vdev_runs_zpool_remove_with_the_pool_before_the_device() {
        let runner = FakeRunner::new("");
        remove_vdev(&runner, "tank", "mirror-1").unwrap();
        assert_eq!(runner.calls.lock().unwrap()[0].1, ["remove", "--", "tank", "mirror-1"].map(String::from));
        assert!(remove_vdev(&FakeRunner::failing("cannot remove", 1), "tank", "da9").is_err());
    }

    #[test]
    fn the_dry_run_is_zpool_add_dash_n_with_options_before_the_pool() {
        let runner = FakeRunner::new("would update 'tank' to the following configuration:");
        let text = add_vdev_dry_run(&runner, "tank", &["log", "da4"]).unwrap();
        assert!(text.starts_with("would update"));
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1, ["add", "-n", "--", "tank", "log", "da4"].map(String::from));
        assert!(add_vdev_dry_run(&FakeRunner::failing("invalid vdev specification", 1), "tank", &["x"]).is_err());
    }

    #[test]
    fn add_vdev_surfaces_command_failure() {
        let runner = FakeRunner::failing("no such geom", 1);
        let result = add_vdev(&runner, "tank", &["mirror", "da2", "da3"]);
        assert!(result.is_err());
    }

    #[test]
    fn guards_against_pool_names_that_look_like_flags_in_add_vdev() {
        let runner = FakeRunner::new("");
        add_vdev(&runner, "-f", &["da2"]).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1[1], "--", "a leading '--' must precede the pool name so zpool cannot parse it as an option");
    }

    #[test]
    fn scrub_pool_runs_expected_command() {
        let runner = FakeRunner::new("");
        scrub_pool(&runner, "tank").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["scrub".to_string(), "--".to_string(), "tank".to_string()]));
    }

    #[test]
    fn destroy_pool_runs_expected_command() {
        let runner = FakeRunner::new("");
        destroy_pool(&runner, "scratch").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["destroy".to_string(), "--".to_string(), "scratch".to_string()]));
    }

    #[test]
    fn offline_disk_runs_expected_command() {
        let runner = FakeRunner::new("");
        offline_disk(&runner, "tank", "da1").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["offline".to_string(), "--".to_string(), "tank".to_string(), "da1".to_string()]));
    }

    #[test]
    fn online_disk_runs_expected_command() {
        let runner = FakeRunner::new("");
        online_disk(&runner, "tank", "da1").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["online".to_string(), "--".to_string(), "tank".to_string(), "da1".to_string()]));
    }

    #[test]
    fn replace_disk_runs_expected_command() {
        let runner = FakeRunner::new("");
        replace_disk(&runner, "tank", "da1", "da2").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zpool".to_string(), vec!["replace".to_string(), "--".to_string(), "tank".to_string(), "da1".to_string(), "da2".to_string()])
        );
    }

    #[test]
    fn create_pool_surfaces_command_failure() {
        let runner = FakeRunner::failing("no such geom", 1);
        let result = create_pool(&runner, "tank", &["mirror", "da0", "da1"]);
        assert!(result.is_err());
    }

    #[test]
    fn scrub_pool_surfaces_command_failure() {
        let runner = FakeRunner::failing("pool is busy", 1);
        let result = scrub_pool(&runner, "tank");
        assert!(result.is_err());
    }

    #[test]
    fn resilver_pool_runs_expected_command() {
        let runner = FakeRunner::new("");
        resilver_pool(&runner, "tank").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["resilver".to_string(), "--".to_string(), "tank".to_string()]));
    }

    #[test]
    fn resilver_pool_surfaces_command_failure() {
        let runner = FakeRunner::failing("no valid replicas", 1);
        let result = resilver_pool(&runner, "tank");
        assert!(result.is_err());
    }

    #[test]
    fn guards_against_pool_names_that_look_like_flags_in_resilver() {
        let runner = FakeRunner::new("");
        resilver_pool(&runner, "-f").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1[1], "--", "a leading '--' must precede the pool name so zpool cannot parse it as an option");
    }

    #[test]
    fn destroy_pool_surfaces_command_failure() {
        let runner = FakeRunner::failing("pool is busy", 1);
        let result = destroy_pool(&runner, "scratch");
        assert!(result.is_err());
    }

    #[test]
    fn offline_disk_surfaces_command_failure() {
        let runner = FakeRunner::failing("no valid replicas", 1);
        let result = offline_disk(&runner, "tank", "da1");
        assert!(result.is_err());
    }

    #[test]
    fn online_disk_surfaces_command_failure() {
        let runner = FakeRunner::failing("no such device", 1);
        let result = online_disk(&runner, "tank", "da1");
        assert!(result.is_err());
    }

    #[test]
    fn replace_disk_surfaces_command_failure() {
        let runner = FakeRunner::failing("device busy", 1);
        let result = replace_disk(&runner, "tank", "da1", "da2");
        assert!(result.is_err());
    }

    #[test]
    fn is_leaf_disk_true_for_physical_device_names() {
        assert!(is_leaf_disk("da0"));
        assert!(is_leaf_disk("nvd1"));
        assert!(is_leaf_disk("tank")); // top-level pool name itself, if ever selected
    }

    #[test]
    fn is_leaf_disk_false_for_group_vdev_names() {
        assert!(!is_leaf_disk("mirror-0"));
        assert!(!is_leaf_disk("raidz1-2"));
        assert!(!is_leaf_disk("raidz2-0"));
        assert!(!is_leaf_disk("raidz3-10"));
    }

    #[test]
    fn guards_against_pool_names_that_look_like_flags() {
        let runner = FakeRunner::new("");
        destroy_pool(&runner, "-f").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1[1], "--", "a leading '--' must precede the pool name so zpool cannot parse it as an option");
    }

    const ZPOOL_GET_PROPERTIES_FIXTURE: &str = "\
tank\tautoexpand\toff\tdefault
tank\tcomment\tmy pool\tlocal
";

    #[test]
    fn get_pool_properties_uses_correct_command_and_parses_result() {
        let runner = FakeRunner::new(ZPOOL_GET_PROPERTIES_FIXTURE);
        let props = get_pool_properties(&runner, "tank").unwrap();
        assert_eq!(props.len(), 2);
        assert_eq!(props[0].name, "autoexpand");
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zpool".to_string(), vec!["get".to_string(), "all".to_string(), "-H".to_string(), "-p".to_string(), "--".to_string(), "tank".to_string()])
        );
    }

    #[test]
    fn get_pool_properties_surfaces_command_failure() {
        let runner = FakeRunner::failing("no such pool", 1);
        let result = get_pool_properties(&runner, "tank");
        assert!(result.is_err());
    }

    #[test]
    fn set_pool_property_runs_expected_command() {
        let runner = FakeRunner::new("");
        set_pool_property(&runner, "tank", "comment", "my pool").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zpool".to_string(), vec!["set".to_string(), "comment=my pool".to_string(), "tank".to_string()])
        );
    }

    #[test]
    fn set_pool_property_surfaces_command_failure() {
        let runner = FakeRunner::failing("invalid property", 1);
        let result = set_pool_property(&runner, "tank", "bogus", "x");
        assert!(result.is_err());
    }

    #[test]
    fn guards_against_pool_names_that_look_like_flags_in_get_properties() {
        let runner = FakeRunner::new("");
        get_pool_properties(&runner, "-f").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1[4], "--", "a leading '--' must precede the pool name so zpool cannot parse it as an option");
    }

    #[test]
    fn vdev_named_like_the_header_is_kept_but_the_real_header_is_skipped() {
        let output = "  pool: tank\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\ttank        ONLINE       0     0     0\n\t  NAMEDISK  ONLINE       0     0     0\n\t  STATEDISK ONLINE       0     0     0\n";
        let names: Vec<String> = parse_pool_status_vdevs(output).into_iter().map(|v| v.name).collect();
        assert_eq!(names, vec!["tank", "NAMEDISK", "STATEDISK"]);
    }

    #[test]
    fn is_leaf_disk_only_treats_numbered_group_names_as_groups() {
        assert!(!is_leaf_disk("mirror-0"));
        assert!(!is_leaf_disk("raidz2-12"));
        assert!(is_leaf_disk("mirror-"), "no number: not a group node");
        assert!(is_leaf_disk("mirror-x"));
        assert!(is_leaf_disk("mirror-0a"));
        assert!(is_leaf_disk("da0"));
    }

    #[test]
    fn scan_summary_reports_errors_only_for_a_nonzero_count_before_the_word() {
        assert!(scan_summary_reports_errors("resilvered 1002M in 00:02:40 with 3 errors on Mon"));
        assert!(scan_summary_reports_errors("scrub repaired 0B in 00:00:01 with 1 error on Mon"));
        assert!(!scan_summary_reports_errors("resilvered 1002M in 00:02:40 with 0 errors on Mon"));
        assert!(!scan_summary_reports_errors("errors found"), "`errors` as the first word has no count before it");
        assert!(!scan_summary_reports_errors("with many errors on Mon"), "non-numeric word before `errors`");
        assert!(!scan_summary_reports_errors("scrub canceled"));
    }

    #[test]
    fn parse_pool_scan_status_none_for_empty_or_none_requested() {
        assert_eq!(parse_pool_scan_status("  pool: tank\n  scan:\nconfig:\n"), None);
        assert_eq!(parse_pool_scan_status("  pool: tank\n  scan: none requested\nconfig:\n"), None);
        assert_eq!(parse_pool_scan_status("  pool: tank\nconfig:\n"), None);
    }

    #[test]
    fn in_progress_percent_comes_only_from_lines_after_the_scan_line() {
        let before = "  pool: tank\n note: 50.0% done elsewhere\n  scan: resilver in progress since Mon\n\nconfig:\n";
        assert_eq!(
            parse_pool_scan_status(before),
            Some(ScanStatus::InProgress { summary: "resilver in progress since Mon".to_string(), percent_done: None, eta: None })
        );
        let on_scan_line = "  scan: resilver in progress, 7.0% done\n\nconfig:\n";
        match parse_pool_scan_status(on_scan_line) {
            Some(ScanStatus::InProgress { percent_done, .. }) => assert_eq!(percent_done, None, "the scan line itself is the summary, not the progress line"),
            other => panic!("{other:?}"),
        }
    }
}
