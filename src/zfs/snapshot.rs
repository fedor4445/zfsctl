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
/// first field is `dataset@snapshot`, split on the first `@` (dataset
/// names cannot themselves contain `@`, so this is unambiguous).
pub fn parse_snapshot_list(output: &str) -> Result<Vec<Snapshot>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let &[full_name, used] = fields.as_slice() else {
                return Err(AppError::Parse(format!(
                    "expected 2 fields in snapshot list line: {line:?}"
                )));
            };
            let (dataset, name) = full_name
                .split_once('@')
                .ok_or_else(|| AppError::Parse(format!("missing '@' in snapshot name: {line:?}")))?;
            Ok(Snapshot {
                dataset: dataset.to_string(),
                name: name.to_string(),
                used: super::parse_numeric_or_dash(used)
                    .ok_or_else(|| AppError::Parse(format!("bad used in: {line:?}")))?,
            })
        })
        .collect()
}

pub fn list_snapshots(runner: &dyn CommandRunner) -> Result<Vec<Snapshot>> {
    let output = super::run_checked(runner, "zfs", SNAPSHOT_LIST_ARGS, "zfs list -t snapshot")?;
    parse_snapshot_list(&output.stdout)
}

/// The `--` before `target` stops `zfs` from treating a dataset name
/// that starts with `-` as an option flag (argument injection).
pub fn create_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    create_snapshot_with(runner, dataset, name, false)
}

/// `zfs snapshot [-r] -- dataset@name`; `-r` snapshots the dataset and every descendant atomically.
pub fn create_snapshot_with(runner: &dyn CommandRunner, dataset: &str, name: &str, recursive: bool) -> Result<()> {
    let target = format!("{dataset}@{name}");
    let mut args = vec!["snapshot"];
    if recursive {
        args.push("-r");
    }
    args.extend(["--", &target]);
    super::run_checked(runner, "zfs", &args, &format!("zfs snapshot {target}"))?;
    Ok(())
}

pub fn destroy_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    destroy_snapshot_with(runner, dataset, name, false)
}

/// `zfs destroy [-r] -- dataset@name`; `-r` also destroys the same-named snapshot of each descendant.
pub fn destroy_snapshot_with(runner: &dyn CommandRunner, dataset: &str, name: &str, recursive: bool) -> Result<()> {
    let target = format!("{dataset}@{name}");
    let mut args = vec!["destroy"];
    if recursive {
        args.push("-r");
    }
    args.extend(["--", &target]);
    super::run_checked(runner, "zfs", &args, &format!("zfs destroy {target}"))?;
    Ok(())
}

/// The snapshots of `dataset` alone (`-d 1`), or of its whole tree when `recursive`.
pub fn list_snapshots_of(runner: &dyn CommandRunner, dataset: &str, recursive: bool) -> Result<Vec<Snapshot>> {
    let mut args: Vec<&str> = SNAPSHOT_LIST_ARGS.to_vec();
    if recursive {
        args.push("-r");
    } else {
        args.extend(["-d", "1"]);
    }
    args.push(dataset);
    let output = super::run_checked(runner, "zfs", &args, &format!("zfs list -t snapshot {dataset}"))?;
    parse_snapshot_list(&output.stdout)
}

pub fn rollback_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    super::run_checked(runner, "zfs", &["rollback", "--", &target], &format!("zfs rollback {target}"))?;
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
    super::run_checked(runner, "sh", &["-c", &pipeline], &pipeline)?;
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
    fn parses_snapshot_list_line_with_dash_used_field() {
        let snapshots = parse_snapshot_list("tank/data@old\t-\n").unwrap();
        assert_eq!(
            snapshots,
            vec![Snapshot { dataset: "tank/data".to_string(), name: "old".to_string(), used: 0 }]
        );
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
    fn create_snapshot_surfaces_command_failure() {
        let runner = FakeRunner::failing("snapshot already exists", 1);
        let result = create_snapshot(&runner, "tank/data", "manual-1");
        assert!(result.is_err());
    }

    #[test]
    fn destroy_snapshot_surfaces_command_failure() {
        let runner = FakeRunner::failing("dataset is busy", 1);
        let result = destroy_snapshot(&runner, "tank/data", "old-1");
        assert!(result.is_err());
    }

    #[test]
    fn rollback_snapshot_surfaces_command_failure() {
        let runner = FakeRunner::failing("cannot rollback, more recent snapshots exist", 1);
        let result = rollback_snapshot(&runner, "tank/data", "2026-09-01");
        assert!(result.is_err());
    }

    #[test]
    fn replicate_snapshot_surfaces_command_failure() {
        let runner = FakeRunner::failing("broken pipe", 1);
        let result = replicate_snapshot(&runner, "tank/data@snap1", None, "backup/data");
        assert!(result.is_err());
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

    fn args_of(runner: &FakeRunner) -> Vec<Vec<String>> {
        runner.calls.lock().unwrap().iter().map(|(_, a)| a.clone()).collect()
    }

    #[test]
    fn create_and_destroy_add_dash_r_only_when_recursive() {
        let r = FakeRunner::new("");
        create_snapshot_with(&r, "tank/data", "h-1", false).unwrap();
        create_snapshot_with(&r, "tank/data", "h-1", true).unwrap();
        destroy_snapshot_with(&r, "tank/data", "h-1", false).unwrap();
        destroy_snapshot_with(&r, "tank/data", "h-1", true).unwrap();
        assert_eq!(
            args_of(&r),
            vec![
                vec!["snapshot", "--", "tank/data@h-1"],
                vec!["snapshot", "-r", "--", "tank/data@h-1"],
                vec!["destroy", "--", "tank/data@h-1"],
                vec!["destroy", "-r", "--", "tank/data@h-1"],
            ]
        );
    }

    #[test]
    fn listing_one_datasets_snapshots_is_depth_one_unless_recursive() {
        let r = FakeRunner::new("tank/data@a\t1\n");
        let flat = list_snapshots_of(&r, "tank/data", false).unwrap();
        let tree = list_snapshots_of(&r, "tank/data", true).unwrap();
        assert_eq!(flat[0].name, "a");
        assert_eq!(tree.len(), 1);
        let calls = args_of(&r);
        assert_eq!(calls[0], vec!["list", "-t", "snapshot", "-H", "-p", "-o", "name,used", "-d", "1", "tank/data"]);
        assert_eq!(calls[1], vec!["list", "-t", "snapshot", "-H", "-p", "-o", "name,used", "-r", "tank/data"]);
    }
}
