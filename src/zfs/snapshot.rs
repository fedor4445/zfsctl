use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub dataset: String,
    pub name: String,
    pub used: u64,
    /// `userrefs`: how many holds the snapshot has (0 = none, or unknown). Display only;
    /// the destroy guard reads the real tags with `hold_tags`.
    pub holds: u64,
}

/// The tag zfsctl puts on the snapshots it holds.
#[allow(dead_code)] // read by the destroy guard and the hold action, added in the next tasks
pub const HOLD_TAG: &str = "zfsctl";

pub const SNAPSHOT_LIST_ARGS: &[&str] = &["list", "-t", "snapshot", "-H", "-p", "-o", "name,used,userrefs"];

/// Parses `zfs list -t snapshot -H -p -o name,used,userrefs` output. Each line's
/// first field is `dataset@snapshot`, split on the first `@` (dataset
/// names cannot themselves contain `@`, so this is unambiguous).
pub fn parse_snapshot_list(output: &str) -> Result<Vec<Snapshot>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let &[full_name, used, holds] = fields.as_slice() else {
                return Err(AppError::Parse(format!(
                    "expected 3 fields in snapshot list line: {line:?}"
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
                holds: super::parse_numeric_or_dash(holds)
                    .ok_or_else(|| AppError::Parse(format!("bad userrefs in: {line:?}")))?,
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

/// Parses `zfs holds -H` output: `snapshot<TAB>tag<TAB>timestamp` per line, as
/// (full snapshot name, tag) pairs. The timestamp is ignored.
#[allow(dead_code)] // read by the destroy guard and the hold action, added in the next tasks
pub fn parse_holds(output: &str) -> Result<Vec<(String, String)>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let mut fields = line.split('\t');
            match (fields.next(), fields.next()) {
                (Some(name), Some(tag)) if !name.is_empty() && !tag.is_empty() => Ok((name.to_string(), tag.to_string())),
                _ => Err(AppError::Parse(format!("expected snapshot and tag in holds line: {line:?}"))),
            }
        })
        .collect()
}

/// The hold tags on `dataset@name` (`zfs holds -H -- target`).
#[allow(dead_code)] // read by the destroy guard and the hold action, added in the next tasks
pub fn hold_tags(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<Vec<String>> {
    let target = format!("{dataset}@{name}");
    let output = super::run_checked(runner, "zfs", &["holds", "-H", "--", &target], &format!("zfs holds {target}"))?;
    Ok(parse_holds(&output.stdout)?.into_iter().map(|(_, tag)| tag).collect())
}

pub fn rollback_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    super::run_checked(runner, "zfs", &["rollback", "--", &target], &format!("zfs rollback {target}"))?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    const SNAPSHOT_LIST_FIXTURE: &str = "\
tank/data@2026-09-01\t10000\t0
tank/data@2026-09-04\t2000\t0
";

    #[test]
    fn parses_snapshot_list_fixture() {
        let snapshots = parse_snapshot_list(SNAPSHOT_LIST_FIXTURE).unwrap();
        assert_eq!(
            snapshots,
            vec![
                Snapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string(), used: 10_000, holds: 0 },
                Snapshot { dataset: "tank/data".to_string(), name: "2026-09-04".to_string(), used: 2_000, holds: 0 },
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
        let snapshots = parse_snapshot_list("tank/data@old\t-\t-\n").unwrap();
        assert_eq!(
            snapshots,
            vec![Snapshot { dataset: "tank/data".to_string(), name: "old".to_string(), used: 0, holds: 0 }]
        );
    }

    #[test]
    fn list_snapshots_uses_correct_command() {
        let runner = FakeRunner::new(SNAPSHOT_LIST_FIXTURE);
        let snapshots = list_snapshots(&runner).unwrap();
        assert_eq!(snapshots.len(), 2);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1, vec!["list", "-t", "snapshot", "-H", "-p", "-o", "name,used,userrefs"]);
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
    fn guards_against_dataset_names_that_look_like_flags() {
        let runner = FakeRunner::new("");
        destroy_snapshot(&runner, "-r", "snap1").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1[1], "--", "a leading '--' must precede the target so zfs cannot parse it as an option");
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
        let r = FakeRunner::new("tank/data@a\t1\t0\n");
        let flat = list_snapshots_of(&r, "tank/data", false).unwrap();
        let tree = list_snapshots_of(&r, "tank/data", true).unwrap();
        assert_eq!(flat[0].name, "a");
        assert_eq!(tree.len(), 1);
        let calls = args_of(&r);
        assert_eq!(calls[0], vec!["list", "-t", "snapshot", "-H", "-p", "-o", "name,used,userrefs", "-d", "1", "tank/data"]);
        assert_eq!(calls[1], vec!["list", "-t", "snapshot", "-H", "-p", "-o", "name,used,userrefs", "-r", "tank/data"]);
    }

    #[test]
    fn parses_the_holds_column() {
        let s = parse_snapshot_list("tank/data@a\t10\t2\ntank/data@b\t5\t0\n").unwrap();
        assert_eq!(s[0].holds, 2);
        assert_eq!(s[1].holds, 0);
    }

    #[test]
    fn a_dash_for_holds_is_zero_held_unknown() {
        let s = parse_snapshot_list("tank/data@a\t10\t-\n").unwrap();
        assert_eq!(s[0].holds, 0);
    }

    #[test]
    fn rejects_a_line_with_two_fields() {
        assert!(parse_snapshot_list("tank/data@a\t10\n").is_err());
    }

    #[test]
    fn list_snapshots_asks_for_userrefs() {
        assert_eq!(SNAPSHOT_LIST_ARGS, ["list", "-t", "snapshot", "-H", "-p", "-o", "name,used,userrefs"]);
    }

    #[test]
    fn parse_holds_reads_name_tag_and_ignores_the_timestamp() {
        let out = "tank/data@a\tzfsctl\tFri Oct 20 10:00 2026\ntank/data@a\tkeep\tFri Oct 20 10:01 2026\n";
        assert_eq!(
            parse_holds(out).unwrap(),
            vec![("tank/data@a".to_string(), "zfsctl".to_string()), ("tank/data@a".to_string(), "keep".to_string())]
        );
        assert!(parse_holds("").unwrap().is_empty());
    }

    #[test]
    fn hold_tags_runs_zfs_holds_options_first() {
        let r = FakeRunner::new("tank/data@a\tzfsctl\tx\n");
        assert_eq!(hold_tags(&r, "tank/data", "a").unwrap(), vec!["zfsctl".to_string()]);
        assert_eq!(args_of(&r)[0], vec!["holds", "-H", "--", "tank/data@a"]);
    }
}
