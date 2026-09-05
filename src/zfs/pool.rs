use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

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
    fn parse(s: &str) -> PoolHealth {
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
            if fields.len() != 5 {
                return Err(AppError::Parse(format!(
                    "expected 5 fields in zpool list line: {line:?}"
                )));
            }
            Ok(Pool {
                name: fields[0].to_string(),
                size: super::parse_numeric_or_dash(fields[1])
                    .ok_or_else(|| AppError::Parse(format!("bad size in: {line:?}")))?,
                allocated: super::parse_numeric_or_dash(fields[2])
                    .ok_or_else(|| AppError::Parse(format!("bad alloc in: {line:?}")))?,
                free: super::parse_numeric_or_dash(fields[3])
                    .ok_or_else(|| AppError::Parse(format!("bad free in: {line:?}")))?,
                health: PoolHealth::parse(fields[4]),
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
        if line.chars().next().map(|c| !c.is_whitespace()).unwrap_or(false) {
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

pub fn list_pools(runner: &dyn CommandRunner) -> Result<Vec<Pool>> {
    let output = runner.run("zpool", POOL_LIST_ARGS)?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: "zpool list".to_string(),
            status: output.status,
            stderr: output.stderr,
        });
    }
    parse_pool_list(&output.stdout)
}

/// Runs `zpool status <pool>` and parses its vdev tree. Separate from
/// `list_pools` because `zpool status` has no tabular `-H -p -o` form.
pub fn get_pool_vdevs(runner: &dyn CommandRunner, pool: &str) -> Result<Vec<VdevLine>> {
    let output = runner.run("zpool", &["status", "--", pool])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool status {pool}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(parse_pool_status_vdevs(&output.stdout))
}

/// Creates a pool from a raw vdev spec, e.g. `&["mirror", "da0", "da1"]`
/// or `&["raidz1", "da0", "da1", "da2"]`. Guided vdev-spec construction
/// (picking disks/redundancy from a UI wizard) depends on the disk
/// inventory built in Plan 5, so it is out of scope here — this function
/// takes an already-built spec.
pub fn create_pool(runner: &dyn CommandRunner, name: &str, vdev_spec: &[&str]) -> Result<()> {
    let mut args = vec!["create", "--", name];
    args.extend_from_slice(vdev_spec);
    let output = runner.run("zpool", &args)?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool create {name} {}", vdev_spec.join(" ")),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn scrub_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()> {
    let output = runner.run("zpool", &["scrub", "--", pool])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool scrub {pool}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn destroy_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()> {
    let output = runner.run("zpool", &["destroy", "--", pool])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool destroy {pool}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn offline_disk(runner: &dyn CommandRunner, pool: &str, disk: &str) -> Result<()> {
    let output = runner.run("zpool", &["offline", "--", pool, disk])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool offline {pool} {disk}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn online_disk(runner: &dyn CommandRunner, pool: &str, disk: &str) -> Result<()> {
    let output = runner.run("zpool", &["online", "--", pool, disk])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool online {pool} {disk}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn replace_disk(runner: &dyn CommandRunner, pool: &str, old_disk: &str, new_disk: &str) -> Result<()> {
    let output = runner.run("zpool", &["replace", "--", pool, old_disk, new_disk])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool replace {pool} {old_disk} {new_disk}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
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
        let vdevs = get_pool_vdevs(&runner, "tank").unwrap();
        assert_eq!(vdevs.len(), 4);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["status".to_string(), "--".to_string(), "tank".to_string()]));
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
    fn guards_against_pool_names_that_look_like_flags() {
        let runner = FakeRunner::new("");
        destroy_pool(&runner, "-f").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1[1], "--", "a leading '--' must precede the pool name so zpool cannot parse it as an option");
    }
}
