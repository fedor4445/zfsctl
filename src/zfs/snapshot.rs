use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub dataset: String,
    pub name: String,
    pub used: u64,
}

pub const SNAPSHOT_LIST_ARGS: &[&str] = &["list", "-t", "snapshot", "-H", "-p", "-o", "name,used"];

/// Parses `zfs list -t snapshot -H -p -o name,used` output. Each line's
/// first field is `dataset@snapshot`, split on the last `@`.
pub fn parse_snapshot_list(output: &str) -> Result<Vec<Snapshot>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != 2 {
                return Err(AppError::Parse(format!(
                    "expected 2 fields in snapshot list line: {line:?}"
                )));
            }
            let (dataset, name) = fields[0]
                .split_once('@')
                .ok_or_else(|| AppError::Parse(format!("missing '@' in snapshot name: {line:?}")))?;
            Ok(Snapshot {
                dataset: dataset.to_string(),
                name: name.to_string(),
                used: fields[1]
                    .parse()
                    .map_err(|_| AppError::Parse(format!("bad used in: {line:?}")))?,
            })
        })
        .collect()
}

pub fn list_snapshots(runner: &dyn CommandRunner) -> Result<Vec<Snapshot>> {
    let output = runner.run("zfs", SNAPSHOT_LIST_ARGS)?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: "zfs list -t snapshot".to_string(),
            status: output.status,
            stderr: output.stderr,
        });
    }
    parse_snapshot_list(&output.stdout)
}

/// The `--` before `target` stops `zfs` from treating a dataset name
/// that starts with `-` as an option flag (argument injection).
pub fn create_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    let output = runner.run("zfs", &["snapshot", "--", &target])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zfs snapshot {target}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn destroy_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    let output = runner.run("zfs", &["destroy", "--", &target])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zfs destroy {target}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn rollback_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    let output = runner.run("zfs", &["rollback", "--", &target])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zfs rollback {target}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

/// Returns true if `s` contains only characters valid in a ZFS
/// dataset/snapshot name (alphanumerics, `_-./:@`). This is enforced
/// here, not merely assumed, because `replicate_snapshot` interpolates
/// `source`/`dest_dataset` directly into a shell command line — an
/// unvalidated value containing shell metacharacters (`;`, `` ` ``,
/// `$(...)`, `|`, etc.) would be a command-injection vector.
fn is_safe_zfs_target(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.' | '/' | '@'))
}

/// Returns true if `s` contains only characters valid in an SSH
/// destination (hostname/IP, optional `user@`, optional `:port`, or a
/// bracketed IPv6 literal). Same rationale as `is_safe_zfs_target`:
/// `host` is interpolated into the same shell command line.
fn is_safe_host(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '@' | ':' | '[' | ']'))
}

/// Replicates `source` (a `dataset@snapshot` name) to `dest_dataset`.
/// `dest_host` is the SSH destination for the receiving side, or `None`
/// to receive locally on the same host that `runner` targets.
pub fn replicate_snapshot(
    runner: &dyn CommandRunner,
    source: &str,
    dest_host: Option<&str>,
    dest_dataset: &str,
) -> Result<()> {
    if !is_safe_zfs_target(source) {
        return Err(AppError::Parse(format!("invalid snapshot name for replication: {source:?}")));
    }
    if !is_safe_zfs_target(dest_dataset) {
        return Err(AppError::Parse(format!("invalid destination dataset name for replication: {dest_dataset:?}")));
    }
    if let Some(host) = dest_host {
        if !is_safe_host(host) {
            return Err(AppError::Parse(format!("invalid destination host for replication: {host:?}")));
        }
    }

    // `--` before each caller-supplied value stops the receiving command
    // from treating a value that starts with `-` as an option flag
    // (argument injection) — applies to `zfs send`/`zfs receive` and, for
    // the remote leg, to `ssh` itself.
    let pipeline = match dest_host {
        None => format!("zfs send -- {source} | zfs receive -- {dest_dataset}"),
        Some(host) => format!("zfs send -- {source} | ssh -- {host} zfs receive -- {dest_dataset}"),
    };
    let output = runner.run("sh", &["-c", &pipeline])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: pipeline,
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

    const SNAPSHOT_LIST_FIXTURE: &str = "\
tank/data@2026-09-01\t10000
tank/data@2026-09-04\t2000
";

    #[test]
    fn parses_snapshot_list_fixture() {
        let snapshots = parse_snapshot_list(SNAPSHOT_LIST_FIXTURE).unwrap();
        assert_eq!(
            snapshots,
            vec![
                Snapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string(), used: 10_000 },
                Snapshot { dataset: "tank/data".to_string(), name: "2026-09-04".to_string(), used: 2_000 },
            ]
        );
    }

    #[test]
    fn rejects_line_missing_at_sign() {
        let result = parse_snapshot_list("tank-data-no-at-sign\t10000\n");
        assert!(result.is_err());
    }

    #[test]
    fn list_snapshots_uses_correct_command() {
        let runner = FakeRunner::new(SNAPSHOT_LIST_FIXTURE);
        let snapshots = list_snapshots(&runner).unwrap();
        assert_eq!(snapshots.len(), 2);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1, vec!["list", "-t", "snapshot", "-H", "-p", "-o", "name,used"]);
    }

    #[test]
    fn create_snapshot_joins_dataset_and_name() {
        let runner = FakeRunner::new("");
        create_snapshot(&runner, "tank/data", "manual-1").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["snapshot".to_string(), "--".to_string(), "tank/data@manual-1".to_string()]));
    }

    #[test]
    fn destroy_snapshot_joins_dataset_and_name() {
        let runner = FakeRunner::new("");
        destroy_snapshot(&runner, "tank/data", "old-1").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["destroy".to_string(), "--".to_string(), "tank/data@old-1".to_string()]));
    }

    #[test]
    fn rollback_snapshot_joins_dataset_and_name() {
        let runner = FakeRunner::new("");
        rollback_snapshot(&runner, "tank/data", "2026-09-01").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["rollback".to_string(), "--".to_string(), "tank/data@2026-09-01".to_string()]));
    }

    #[test]
    fn guards_against_dataset_names_that_look_like_flags() {
        let runner = FakeRunner::new("");
        destroy_snapshot(&runner, "-r", "snap1").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1[1], "--", "a leading '--' must precede the target so zfs cannot parse it as an option");
    }

    #[test]
    fn replicate_snapshot_local_destination_builds_expected_pipeline() {
        let runner = FakeRunner::new("");
        replicate_snapshot(&runner, "tank/data@snap1", None, "backup/data").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("sh".to_string(), vec!["-c".to_string(), "zfs send -- tank/data@snap1 | zfs receive -- backup/data".to_string()])
        );
    }

    #[test]
    fn replicate_snapshot_remote_destination_builds_expected_pipeline() {
        let runner = FakeRunner::new("");
        replicate_snapshot(&runner, "tank/data@snap1", Some("backup-host"), "backup/data").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            (
                "sh".to_string(),
                vec!["-c".to_string(), "zfs send -- tank/data@snap1 | ssh -- backup-host zfs receive -- backup/data".to_string()]
            )
        );
    }

    #[test]
    fn replicate_snapshot_rejects_source_with_shell_metacharacters() {
        let runner = FakeRunner::new("");
        let result = replicate_snapshot(&runner, "tank/data@snap1; rm -rf /", None, "backup/data");
        assert!(result.is_err());
        assert!(runner.calls.lock().unwrap().is_empty(), "must reject before ever running a command");
    }

    #[test]
    fn replicate_snapshot_rejects_dest_dataset_with_shell_metacharacters() {
        let runner = FakeRunner::new("");
        let result = replicate_snapshot(&runner, "tank/data@snap1", None, "backup/data`whoami`");
        assert!(result.is_err());
        assert!(runner.calls.lock().unwrap().is_empty(), "must reject before ever running a command");
    }

    #[test]
    fn replicate_snapshot_rejects_host_with_shell_metacharacters() {
        let runner = FakeRunner::new("");
        let result = replicate_snapshot(&runner, "tank/data@snap1", Some("host; rm -rf /"), "backup/data");
        assert!(result.is_err());
        assert!(runner.calls.lock().unwrap().is_empty(), "must reject before ever running a command");
    }
}
