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
    refuse_if_held(runner, dataset, name, recursive)?;
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
pub fn hold_tags(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<Vec<String>> {
    let target = format!("{dataset}@{name}");
    let output = super::run_checked(runner, "zfs", &["holds", "-H", "--", &target], &format!("zfs holds {target}"))?;
    Ok(parse_holds(&output.stdout)?.into_iter().map(|(_, tag)| tag).collect())
}

/// How many snapshots one `zfs holds` call asks about.
const HOLDS_CHUNK: usize = 50;

/// The hold tags of the given snapshots (full `dataset@name`), one `zfs holds -H -- names...`
/// call per chunk of 50. A chunk that fails or cannot be parsed leaves its snapshots out of the
/// map: their tags stay unknown. A snapshot with no hold is absent too.
pub fn hold_tags_for(runner: &dyn CommandRunner, snapshots: &[String]) -> std::collections::HashMap<String, Vec<String>> {
    let mut map: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for chunk in snapshots.chunks(HOLDS_CHUNK) {
        let mut args = vec!["holds", "-H", "--"];
        args.extend(chunk.iter().map(String::as_str));
        let Ok(output) = super::run_checked(runner, "zfs", &args, "zfs holds") else { continue };
        let Ok(pairs) = parse_holds(&output.stdout) else { continue };
        for (name, tag) in pairs {
            map.entry(name).or_default().push(tag);
        }
    }
    map
}

/// `zfs hold -- zfsctl dataset@name`.
pub fn hold_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    super::run_checked(runner, "zfs", &["hold", "--", HOLD_TAG, &target], &format!("zfs hold {target}"))?;
    Ok(())
}

/// `zfs release -- zfsctl dataset@name`.
pub fn release_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    super::run_checked(runner, "zfs", &["release", "--", HOLD_TAG, &target], &format!("zfs release {target}"))?;
    Ok(())
}

/// The snapshots of `dataset` (and of its descendants when `recursive`) with their
/// `userrefs`: `None` when the value is not a number. One `zfs get` call.
pub fn held_in(runner: &dyn CommandRunner, dataset: &str, recursive: bool) -> Result<Vec<(String, Option<u64>)>> {
    // illumos stops reading options at the first non-option, so a `get` takes no `--` after the
    // property list; a name that could be read as an option is refused instead
    if dataset.starts_with('-') {
        return Err(AppError::Parse(format!("refusing a dataset name that starts with '-': {dataset}")));
    }
    let mut args = vec!["get"];
    if recursive {
        args.push("-r");
    } else {
        // without a depth, `zfs get -t snapshot <fs>` may not list the snapshots at all
        args.extend(["-d", "1"]);
    }
    args.extend(["-H", "-p", "-t", "snapshot", "-o", "name,value", "userrefs", dataset]);
    let output = super::run_checked(runner, "zfs", &args, &format!("zfs get userrefs {dataset}"))?;
    output
        .stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let mut fields = line.split('\t');
            match (fields.next(), fields.next()) {
                (Some(name), Some(value)) if name.contains('@') => Ok((name.to_string(), value.trim().parse::<u64>().ok())),
                _ => Err(AppError::Parse(format!("expected snapshot and userrefs in line: {line:?}"))),
            }
        })
        .collect()
}

/// Refuses with `AppError::Held` when `dataset@name`, or with `recursive` the same-named
/// snapshot of any descendant, has a hold. A check that cannot run, a `userrefs` that is not a
/// number or a missing row also refuses, as `AppError::HoldCheck`: unknown is never treated as unheld.
pub fn refuse_if_held(runner: &dyn CommandRunner, dataset: &str, name: &str, recursive: bool) -> Result<()> {
    let rows = held_in(runner, dataset, recursive).map_err(|e| AppError::HoldCheck(format!("on {dataset}@{name}: {e}")))?;
    let suffix = format!("@{name}");
    let prefix = format!("{dataset}/");
    let matching: Vec<(String, Option<u64>)> = rows
        .into_iter()
        .filter(|(full, _)| {
            let Some(ds) = full.strip_suffix(&suffix) else { return false };
            ds == dataset || (recursive && ds.starts_with(&prefix))
        })
        .collect();
    // no row for the target means the check saw nothing: unknown, never allowed
    let target_seen = if recursive { !matching.is_empty() } else { matching.iter().any(|(f, _)| f == &format!("{dataset}{suffix}")) };
    if !target_seen {
        return Err(AppError::HoldCheck(format!("no row for {dataset}@{name}")));
    }
    let blocked: Vec<(String, Option<u64>)> = matching.into_iter().filter(|(_, refs)| *refs != Some(0)).collect();
    let Some((full, refs)) = blocked.first() else { return Ok(()) };
    let more = if blocked.len() > 1 { format!(" (and {} more)", blocked.len() - 1) } else { String::new() };
    let Some(n) = refs else {
        return Err(AppError::HoldCheck(format!("{full} has an unreadable hold count, so it is treated as held{more}")));
    };
    let (ds, snap) = full.split_once('@').unwrap_or((full, ""));
    let tags = match hold_tags(runner, ds, snap) {
        Ok(t) => match crate::display_text::tag_list(&t) {
            l if l.is_empty() => String::new(),
            l => format!(" ({l})"),
        },
        _ => String::new(),
    };
    let plural = if *n == 1 { "" } else { "s" };
    Err(AppError::Held(format!("{full} has {n} hold{plural}{tags}; release it first{more}")))
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
        let result = parse_snapshot_list("tank-data-no-at-sign\t10000\t0\n");
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
        let runner = FakeRunner::new("").route("zfs", "get", "tank/data@old-1\t0\n");
        destroy_snapshot(&runner, "tank/data", "old-1").unwrap();
        assert_eq!(destroy_calls(&runner), vec![vec!["destroy", "--", "tank/data@old-1"]]);
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
        // the holds check takes no `--` (illumos), so a dataset that looks like a flag is refused before anything runs
        let runner = FakeRunner::new("").route("zfs", "get", "tank/data@snap1\t0\n");
        assert!(destroy_snapshot(&runner, "-r", "snap1").is_err());
        assert!(destroy_calls(&runner).is_empty());
        destroy_snapshot(&runner, "tank/data", "snap1").unwrap();
        assert_eq!(destroy_calls(&runner)[0][1], "--", "a leading '--' must precede the target so zfs cannot parse it as an option");
    }

    fn destroy_calls(runner: &FakeRunner) -> Vec<Vec<String>> {
        args_of(runner).into_iter().filter(|a| a[0] == "destroy").collect()
    }

    fn args_of(runner: &FakeRunner) -> Vec<Vec<String>> {
        runner.calls.lock().unwrap().iter().map(|(_, a)| a.clone()).collect()
    }

    #[test]
    fn create_and_destroy_add_dash_r_only_when_recursive() {
        let r = FakeRunner::new("").route("zfs", "get", "tank/data@h-1\t0\n");
        create_snapshot_with(&r, "tank/data", "h-1", false).unwrap();
        create_snapshot_with(&r, "tank/data", "h-1", true).unwrap();
        destroy_snapshot_with(&r, "tank/data", "h-1", false).unwrap();
        destroy_snapshot_with(&r, "tank/data", "h-1", true).unwrap();
        assert_eq!(
            args_of(&r).into_iter().filter(|a| a[0] != "get").collect::<Vec<_>>(),
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

    /// Answers `zfs holds` with one `name<TAB>tag<TAB>time` line per requested name; fails when
    /// a requested name is `fail_on`. Records every call's arguments.
    struct HoldsRunner {
        fail_on: Option<String>,
        calls: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl CommandRunner for HoldsRunner {
        fn run(&self, _cmd: &str, args: &[&str]) -> Result<crate::exec::CommandOutput> {
            self.calls.lock().unwrap().push(args.iter().map(|a| (*a).to_string()).collect());
            let names = &args[3..];
            if self.fail_on.as_deref().is_some_and(|f| names.contains(&f)) {
                return Ok(crate::exec::CommandOutput { stdout: String::new(), stderr: "boom".into(), status: 1 });
            }
            Ok(crate::exec::CommandOutput { stdout: names.iter().map(|n| format!("{n}\tkeep\tx\n")).collect::<Vec<_>>().concat(), stderr: String::new(), status: 0 })
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _: &str) -> Result<crate::exec::CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<crate::exec::CommandOutput> {
            self.run(cmd, args)
        }
    }

    fn snaps(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("tank/data@s{i}")).collect()
    }

    #[test]
    fn hold_tags_for_asks_once_for_50_snapshots_and_twice_for_51() {
        for (n, calls) in [(1, 1), (50, 1), (51, 2), (100, 2), (101, 3)] {
            let r = HoldsRunner { fail_on: None, calls: std::sync::Mutex::default() };
            let map = hold_tags_for(&r, &snaps(n));
            assert_eq!(r.calls.lock().unwrap().len(), calls, "{n}");
            assert_eq!(map.len(), n);
            assert_eq!(map["tank/data@s0"], vec!["keep".to_string()]);
        }
        let r = HoldsRunner { fail_on: None, calls: std::sync::Mutex::default() };
        hold_tags_for(&r, &snaps(2));
        assert_eq!(r.calls.lock().unwrap()[0], vec!["holds", "-H", "--", "tank/data@s0", "tank/data@s1"]);
        assert!(hold_tags_for(&r, &[]).is_empty());
        assert_eq!(r.calls.lock().unwrap().len(), 1, "no names, no call");
    }

    #[test]
    fn a_failed_chunk_leaves_only_its_own_snapshots_out() {
        let r = HoldsRunner { fail_on: Some("tank/data@s60".into()), calls: std::sync::Mutex::default() };
        let map = hold_tags_for(&r, &snaps(80));
        assert_eq!(map.len(), 50, "the second chunk (s50..s79) is missing");
        assert!(map.contains_key("tank/data@s49") && !map.contains_key("tank/data@s50"));
        let r = FakeRunner::failing("boom", 1);
        assert!(hold_tags_for(&r, &snaps(3)).is_empty());
    }

    #[test]
    fn hold_and_release_run_the_exact_commands() {
        let r = FakeRunner::new("");
        hold_snapshot(&r, "tank/data", "a").unwrap();
        release_snapshot(&r, "tank/data", "a").unwrap();
        assert_eq!(
            args_of(&r),
            vec![vec!["hold", "--", "zfsctl", "tank/data@a"], vec!["release", "--", "zfsctl", "tank/data@a"]]
        );
    }

    #[test]
    fn refuse_if_held_allows_an_unheld_snapshot() {
        let r = FakeRunner::new("").route("zfs", "get", "tank/data@a\t0\n");
        refuse_if_held(&r, "tank/data", "a", false).unwrap();
        assert_eq!(
            args_of(&r)[0],
            vec!["get", "-d", "1", "-H", "-p", "-t", "snapshot", "-o", "name,value", "userrefs", "tank/data"]
        );
    }

    #[test]
    fn only_the_exact_name_on_the_exact_dataset_counts() {
        let out = "tank/data@a\t0\ntank/data@ba\t3\ntank/data2@a\t3\ntank/database/x@a\t3\n";
        for recursive in [false, true] {
            let r = FakeRunner::new("").route("zfs", "get", out);
            refuse_if_held(&r, "tank/data", "a", recursive).unwrap();
        }
    }

    #[test]
    fn a_get_that_answers_without_the_target_row_is_refused_in_both_modes() {
        for out in ["", "tank/data@b\t0\n", "tank/data/sub@a\t0\n"] {
            let r = FakeRunner::new("").route("zfs", "get", out);
            match refuse_if_held(&r, "tank/data", "a", false) {
                Err(AppError::HoldCheck(m)) => assert!(m.contains("no row for tank/data@a"), "{m}"),
                other => panic!("expected HoldCheck, got {other:?}"),
            }
            assert!(destroy_snapshot_with(&r, "tank/data", "a", false).is_err());
            assert!(destroy_calls(&r).is_empty());
        }
        for out in ["", "tank/data@b\t0\n", "tank/data2@a\t0\n"] {
            let r = FakeRunner::new("").route("zfs", "get", out);
            assert!(matches!(refuse_if_held(&r, "tank/data", "a", true), Err(AppError::HoldCheck(_))));
            assert!(destroy_snapshot_with(&r, "tank/data", "a", true).is_err());
            assert!(destroy_calls(&r).is_empty());
        }
    }

    #[test]
    fn refuse_if_held_names_the_holder_and_the_tags() {
        let r = FakeRunner::new("")
            .route("zfs", "get", "tank/data@a\t1\n")
            .route("zfs", "holds", "tank/data@a\tkeep\tx\n");
        match refuse_if_held(&r, "tank/data", "a", false) {
            Err(AppError::Held(m)) => {
                assert!(m.contains("tank/data@a"), "{m}");
                assert!(m.contains("1 hold"), "{m}");
                assert!(m.contains("keep"), "{m}");
            }
            other => panic!("expected Held, got {other:?}"),
        }
    }

    #[test]
    fn a_recursive_check_looks_at_descendants_of_the_same_name_only() {
        let out = "tank/data@a\t0\ntank/data/sub@a\t2\ntank/data/sub@b\t5\n";
        let r = FakeRunner::new("").route("zfs", "get", out).route("zfs", "holds", "tank/data/sub@a\tkeep\tx\n");
        match refuse_if_held(&r, "tank/data", "a", true) {
            Err(AppError::Held(m)) => {
                assert!(m.contains("tank/data/sub@a"), "{m}");
                assert!(!m.contains("@b"), "{m}");
            }
            other => panic!("expected Held, got {other:?}"),
        }
        assert_eq!(
            args_of(&r)[0],
            vec!["get", "-r", "-H", "-p", "-t", "snapshot", "-o", "name,value", "userrefs", "tank/data"]
        );
        let r = FakeRunner::new("").route("zfs", "get", out);
        refuse_if_held(&r, "tank/data", "a", false).unwrap();
    }

    #[test]
    fn if_the_check_cannot_run_the_destroy_is_refused() {
        let r = FakeRunner::failing("boom", 1);
        match refuse_if_held(&r, "tank/data", "a", false) {
            Err(AppError::HoldCheck(m)) => assert!(m.contains("on tank/data@a"), "{m}"),
            other => panic!("expected HoldCheck, got {other:?}"),
        }
        assert!(destroy_snapshot_with(&r, "tank/data", "a", false).is_err());
        assert!(destroy_calls(&r).is_empty());
    }

    #[test]
    fn an_unreadable_userrefs_value_refuses() {
        let r = FakeRunner::new("").route("zfs", "get", "tank/data@a\t-\n");
        assert!(matches!(refuse_if_held(&r, "tank/data", "a", false), Err(AppError::HoldCheck(_))));
        assert!(destroy_snapshot(&r, "tank/data", "a").is_err());
        assert!(destroy_calls(&r).is_empty());
    }

    #[test]
    fn destroy_snapshot_never_uses_the_deferred_flag() {
        let r = FakeRunner::new("").route("zfs", "get", "tank/data@a\t0\n");
        destroy_snapshot_with(&r, "tank/data", "a", false).unwrap();
        destroy_snapshot_with(&r, "tank/data", "a", true).unwrap();
        let d = destroy_calls(&r);
        assert_eq!(d, vec![vec!["destroy", "--", "tank/data@a"], vec!["destroy", "-r", "--", "tank/data@a"]]);
        assert!(d.iter().all(|a| !a.contains(&"-d".to_string())));
    }

    #[test]
    fn a_held_text_has_no_control_or_bidi_character_and_drops_empty_tags() {
        let r = FakeRunner::new("")
            .route("zfs", "get", "tank/data@a\t3\n")
            .route("zfs", "holds", "tank/data@a\ta\u{1b}[31mb\u{202e}\tx\ntank/data@a\t\u{1b}\u{7}\tx\ntank/data@a\tkeep\tx\n");
        match refuse_if_held(&r, "tank/data", "a", false) {
            Err(AppError::Held(m)) => {
                assert!(!m.chars().any(|c| c.is_control() || c == '\u{202e}'), "{m:?}");
                assert!(m.contains("(a [31mb, keep)"), "{m}");
            }
            other => panic!("expected Held, got {other:?}"),
        }
    }

    #[test]
    fn a_name_that_looks_like_a_flag_is_a_hold_check_failure() {
        let r = FakeRunner::new("");
        assert!(matches!(refuse_if_held(&r, "-x", "a", false), Err(AppError::HoldCheck(_))));
    }
}
