use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use crate::zfs::property::Property;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatasetKind {
    Filesystem,
    Volume,
    Other(String),
}

impl DatasetKind {
    fn parse(s: &str) -> DatasetKind {
        match s {
            "filesystem" => DatasetKind::Filesystem,
            "volume" => DatasetKind::Volume,
            other => DatasetKind::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dataset {
    pub name: String,
    pub kind: DatasetKind,
    pub used: u64,
    pub available: u64,
    pub referenced: u64,
    pub mountpoint: Option<String>,
    /// `quota` and `refquota` in bytes; 0 = none (`zfs list -p` prints an unset limit as 0, or `-` on a volume).
    pub quota: u64,
    pub refquota: u64,
}

pub const DATASET_LIST_ARGS: &[&str] = &[
    "list", "-t", "filesystem,volume", "-H", "-p", "-o", "name,used,avail,refer,mountpoint,type,mounted,quota,refquota",
];

/// Parses `zfs list -H -p -o name,used,avail,refer,mountpoint,type,mounted`
/// output (tab-separated, one dataset per line).
///
/// `mountpoint` alone is not enough to know where (or whether) a
/// dataset is actually mounted: `zfs` prints the `mountpoint` property
/// even when the dataset isn't mounted there (confirmed live on a
/// default FreeBSD install -- `zroot/usr` reports mountpoint `/usr`
/// with `canmount=off`, `mounted=no`; the real `/usr` belongs to a
/// different dataset). So `mountpoint` here is `Some` only when
/// `mounted` says `yes` and the value isn't `legacy` (which names no
/// real path at all -- ZFS defers mounting it to `/etc/fstab`).
pub fn parse_dataset_list(output: &str) -> Result<Vec<Dataset>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            // The two limits at the end are optional so a shorter line (an older capture) still reads.
            let ([name, used, available, referenced, mountpoint, kind, mounted], limits) = match fields.as_slice() {
                [a, b, c, d, e, f, g, rest @ ..] if rest.len() <= 2 => ([*a, *b, *c, *d, *e, *f, *g], rest),
                _ => return Err(AppError::Parse(format!("expected 9 fields in zfs list line: {line:?}"))),
            };
            let limit = |i: usize| limits.get(i).and_then(|v| super::parse_numeric_or_dash(v)).unwrap_or(0);
            let mountpoint = match (mountpoint, mounted) {
                ("-" | "none" | "legacy", _) => None,
                (mp, "yes") => Some(mp.to_string()),
                (_, _) => None,
            };
            Ok(Dataset {
                name: name.to_string(),
                used: super::parse_numeric_or_dash(used)
                    .ok_or_else(|| AppError::Parse(format!("bad used in: {line:?}")))?,
                available: super::parse_numeric_or_dash(available)
                    .ok_or_else(|| AppError::Parse(format!("bad avail in: {line:?}")))?,
                referenced: super::parse_numeric_or_dash(referenced)
                    .ok_or_else(|| AppError::Parse(format!("bad refer in: {line:?}")))?,
                mountpoint,
                kind: DatasetKind::parse(kind),
                quota: limit(0),
                refquota: limit(1),
            })
        })
        .collect()
}

pub fn list_datasets(runner: &dyn CommandRunner) -> Result<Vec<Dataset>> {
    let output = super::run_checked(runner, "zfs", DATASET_LIST_ARGS, "zfs list")?;
    parse_dataset_list(&output.stdout)
}

/// The `--` before `name` stops `zfs` from treating a dataset name that
/// starts with `-` as an option flag (argument injection).
pub fn create_dataset(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    super::run_checked(runner, "zfs", &["create", "--", name], &format!("zfs create {name}"))?;
    Ok(())
}

/// Refuses when any snapshot `held_in` returns is held, has an unreadable count, or the check itself cannot run.
/// A dataset without snapshots (an empty list) is allowed.
fn refuse_if_any_held(runner: &dyn CommandRunner, name: &str, recursive: bool) -> Result<()> {
    let rows = crate::zfs::snapshot::held_in(runner, name, recursive).map_err(|e| AppError::Held(format!("could not check holds: {e}")))?;
    match rows.iter().find(|(_, refs)| *refs != Some(0)) {
        None => Ok(()),
        Some((full, Some(n))) => Err(AppError::Held(format!("{full} is held ({n} hold(s)); release it first"))),
        Some((full, None)) => Err(AppError::Held(format!("{full} has an unreadable hold count, so it is treated as held; release it first"))),
    }
}

pub fn destroy_dataset(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    refuse_if_any_held(runner, name, false)?;
    super::run_checked(runner, "zfs", &["destroy", "--", name], &format!("zfs destroy {name}"))?;
    Ok(())
}

/// `zfs destroy -r`: the dataset, its child datasets and all their snapshots. Never `-R`, which would also
/// destroy clones elsewhere in the pool: a dataset with clones is refused with zfs's own message.
pub fn destroy_dataset_recursive(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    refuse_if_any_held(runner, name, true)?;
    super::run_checked(runner, "zfs", &["destroy", "-r", "--", name], &format!("zfs destroy -r {name}"))?;
    Ok(())
}

/// Runs `zfs get all -H -p <dataset>` and parses every property into a
/// flat list.
pub fn get_dataset_properties(runner: &dyn CommandRunner, dataset: &str) -> Result<Vec<Property>> {
    super::not_an_option(dataset)?;
    let output = super::run_checked(
        runner,
        "zfs",
        &["get", "-H", "-p", "all", dataset],
        &format!("zfs get all {dataset}"),
    )?;
    crate::zfs::property::parse_property_list(&output.stdout)
}

/// No `--` guard here, unlike every other mutating function in this
/// file: `zfs set` doesn't recognize `--` as an end-of-options marker
/// at all — it tries to open a dataset literally named `--` and fails
/// (confirmed against a real `zfs` binary: `cannot open '--': dataset
/// does not exist`). `set` takes no flags of its own, so there is no
/// flag-injection surface for `--` to guard against here in the first
/// place: `key`/`value` are joined into one argv token before
/// `dataset`, which is never scanned for a leading `-` in this
/// subcommand's argument parsing.
pub fn set_dataset_property(runner: &dyn CommandRunner, dataset: &str, key: &str, value: &str) -> Result<()> {
    super::run_checked(
        runner,
        "zfs",
        &["set", &format!("{key}={value}"), dataset],
        &format!("zfs set {key}={value} {dataset}"),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    const ZFS_LIST_FIXTURE: &str = "\
tank/data\t100000000\t900000000\t100000000\t/tank/data\tfilesystem\tyes
tank/vol1\t50000000\t900000000\t50000000\t-\tvolume\t-
";

    #[test]
    fn dataset_list_reads_quota_and_refquota_with_zero_or_dash_meaning_none() {
        let out = "tank\t10\t20\t5\t/tank\tfilesystem\tyes\t10737418240\t0\ntank/vol\t1\t2\t1\t-\tvolume\t-\t-\t-\ntank/r\t1\t2\t1\t/r\tfilesystem\tyes\t0\t4294967296\n";
        let ds = parse_dataset_list(out).unwrap();
        assert_eq!((ds[0].quota, ds[0].refquota), (10 << 30, 0));
        assert_eq!((ds[1].quota, ds[1].refquota), (0, 0), "a volume prints - for both");
        assert_eq!((ds[2].quota, ds[2].refquota), (0, 4 << 30));
        let old = parse_dataset_list("tank\t10\t20\t5\t/tank\tfilesystem\tyes\n").unwrap();
        assert_eq!((old[0].quota, old[0].refquota), (0, 0), "a line without the limit columns still reads");
    }

    #[test]
    fn parses_dataset_list_fixture() {
        let datasets = parse_dataset_list(ZFS_LIST_FIXTURE).unwrap();
        assert_eq!(
            datasets,
            vec![
                Dataset {
                    name: "tank/data".to_string(),
                    kind: DatasetKind::Filesystem,
                    used: 100_000_000,
                    available: 900_000_000,
                    referenced: 100_000_000,
                    mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0,
                },
                Dataset {
                    name: "tank/vol1".to_string(),
                    kind: DatasetKind::Volume,
                    used: 50_000_000,
                    available: 900_000_000,
                    referenced: 50_000_000,
                    mountpoint: None, quota: 0, refquota: 0,
                },
            ]
        );
    }

    #[test]
    fn parses_dataset_list_fixture_with_mounted_column_treats_unmounted_filesystem_as_no_mountpoint() {
        // zfs list prints the `mountpoint` property regardless of
        // whether the dataset is actually mounted there (confirmed live
        // on a default FreeBSD install: zroot/usr has mountpoint=/usr,
        // canmount=off, mounted=no -- the real /usr is a different
        // dataset's mountpoint entirely). Code that treats the printed
        // mountpoint as a real, currently-mounted path without checking
        // `mounted` points at the wrong filesystem.
        let fixture = "zroot/usr\t100\t200\t100\t/usr\tfilesystem\tno\n";
        let datasets = parse_dataset_list(fixture).unwrap();
        assert_eq!(
            datasets,
            vec![Dataset { name: "zroot/usr".to_string(), kind: DatasetKind::Filesystem, used: 100, available: 200, referenced: 100, mountpoint: None, quota: 0, refquota: 0 }]
        );
    }

    #[test]
    fn parses_dataset_list_fixture_with_mounted_column_treats_legacy_mountpoint_as_no_mountpoint() {
        let fixture = "tank/legacy\t100\t200\t100\tlegacy\tfilesystem\tyes\n";
        let datasets = parse_dataset_list(fixture).unwrap();
        assert_eq!(
            datasets,
            vec![Dataset { name: "tank/legacy".to_string(), kind: DatasetKind::Filesystem, used: 100, available: 200, referenced: 100, mountpoint: None, quota: 0, refquota: 0 }]
        );
    }

    #[test]
    fn rejects_malformed_dataset_list_line() {
        let result = parse_dataset_list("tank/data\tnot-enough-fields\n");
        assert!(result.is_err());
    }

    #[test]
    fn list_datasets_uses_correct_command() {
        let runner = FakeRunner::new(ZFS_LIST_FIXTURE);
        let datasets = list_datasets(&runner).unwrap();
        assert_eq!(datasets.len(), 2);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].0, "zfs");
        assert_eq!(
            calls[0].1,
            vec!["list", "-t", "filesystem,volume", "-H", "-p", "-o", "name,used,avail,refer,mountpoint,type,mounted,quota,refquota"]
        );
    }

    #[test]
    fn parses_dataset_list_line_with_dash_numeric_fields() {
        let datasets = parse_dataset_list("tank/broken\t-\t-\t-\t-\tfilesystem\tno\n").unwrap();
        assert_eq!(
            datasets,
            vec![Dataset {
                name: "tank/broken".to_string(),
                kind: DatasetKind::Filesystem,
                used: 0,
                available: 0,
                referenced: 0,
                mountpoint: None, quota: 0, refquota: 0,
            }]
        );
    }

    #[test]
    fn create_dataset_runs_expected_command() {
        let runner = FakeRunner::new("");
        create_dataset(&runner, "tank/newds").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["create".to_string(), "--".to_string(), "tank/newds".to_string()]));
    }

    #[test]
    fn destroy_dataset_runs_expected_command() {
        let runner = FakeRunner::new("");
        destroy_dataset(&runner, "tank/olds").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[1], ("zfs".to_string(), vec!["destroy".to_string(), "--".to_string(), "tank/olds".to_string()]));
    }

    #[test]
    fn destroying_a_dataset_with_a_held_snapshot_is_refused_and_runs_no_destroy() {
        for (recursive, held) in [(false, "tank/olds@a\t0\ntank/olds@b\t1\n"), (true, "tank/olds@a\t0\ntank/olds/kid@b\t2\n"), (false, "tank/olds@a\tx\n")] {
            let runner = FakeRunner::new("").route("zfs", "get", held);
            let result = if recursive { destroy_dataset_recursive(&runner, "tank/olds") } else { destroy_dataset(&runner, "tank/olds") };
            match result {
                Err(crate::error::AppError::Held(m)) => assert!(m.contains("release it first"), "{m}"),
                other => panic!("expected Held, got {other:?}"),
            }
            assert!(runner.calls.lock().unwrap().iter().all(|c| c.1[0] != "destroy"));
        }
    }

    #[test]
    fn destroying_a_dataset_refuses_when_the_hold_check_fails() {
        let runner = FakeRunner::new("").route_failure("zfs", "get", "boom", 1);
        assert!(matches!(destroy_dataset(&runner, "tank/olds"), Err(crate::error::AppError::Held(m)) if m.contains("could not check holds")));
        assert!(runner.calls.lock().unwrap().iter().all(|c| c.1[0] != "destroy"));
    }

    #[test]
    fn destroy_dataset_checks_own_snapshots_and_recursive_checks_the_tree() {
        let runner = FakeRunner::new("");
        destroy_dataset(&runner, "tank/olds").unwrap();
        destroy_dataset_recursive(&runner, "tank/olds").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert!(calls[0].1.contains(&"-d".to_string()) && !calls[0].1.contains(&"-r".to_string()));
        assert!(calls[2].1.contains(&"-r".to_string()));
    }

    #[test]
    fn create_dataset_surfaces_command_failure() {
        let runner = FakeRunner::failing("dataset already exists", 1);
        let result = create_dataset(&runner, "tank/newds");
        assert!(result.is_err());
    }

    #[test]
    fn destroy_dataset_surfaces_command_failure() {
        let runner = FakeRunner::failing("dataset is busy", 1);
        let result = destroy_dataset(&runner, "tank/olds");
        assert!(result.is_err());
    }

    #[test]
    fn refuses_dataset_names_that_look_like_flags() {
        let runner = FakeRunner::new("");
        assert!(destroy_dataset(&runner, "-r").is_err(), "a name that could be read as an option is refused before any destroy");
        assert!(runner.calls.lock().unwrap().iter().all(|c| c.1[0] != "destroy"));
    }

    const ZFS_GET_PROPERTIES_FIXTURE: &str = "\
tank/data\tcompression\tlz4\tlocal
tank/data\tquota\t0\tdefault
";

    #[test]
    fn get_dataset_properties_uses_correct_command_and_parses_result() {
        let runner = FakeRunner::new(ZFS_GET_PROPERTIES_FIXTURE);
        let props = get_dataset_properties(&runner, "tank/data").unwrap();
        assert_eq!(props.len(), 2);
        assert_eq!(props[0].name, "compression");
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zfs".to_string(), vec!["get".to_string(), "-H".to_string(), "-p".to_string(), "all".to_string(), "tank/data".to_string()])
        );
    }

    #[test]
    fn get_dataset_properties_surfaces_command_failure() {
        let runner = FakeRunner::failing("dataset does not exist", 1);
        let result = get_dataset_properties(&runner, "tank/data");
        assert!(result.is_err());
    }

    #[test]
    fn set_dataset_property_runs_expected_command() {
        let runner = FakeRunner::new("");
        set_dataset_property(&runner, "tank/data", "compression", "zstd").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zfs".to_string(), vec!["set".to_string(), "compression=zstd".to_string(), "tank/data".to_string()])
        );
    }

    #[test]
    fn set_dataset_property_surfaces_command_failure() {
        let runner = FakeRunner::failing("invalid property", 1);
        let result = set_dataset_property(&runner, "tank/data", "bogus", "x");
        assert!(result.is_err());
    }

    #[test]
    fn guards_against_dataset_names_that_look_like_flags_in_get_properties() {
        let runner = FakeRunner::new("");
        assert!(get_dataset_properties(&runner, "-r").is_err());
        assert!(runner.calls.lock().unwrap().is_empty(), "a name that looks like a flag is refused (a get takes no `--`: illumos reads it as a name)");
    }
}
