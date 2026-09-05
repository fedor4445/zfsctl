use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

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
}

pub const DATASET_LIST_ARGS: &[&str] = &[
    "list", "-t", "filesystem,volume", "-H", "-p", "-o", "name,used,avail,refer,mountpoint,type",
];

/// Parses `zfs list -H -p -o name,used,avail,refer,mountpoint,type`
/// output (tab-separated, one dataset per line).
pub fn parse_dataset_list(output: &str) -> Result<Vec<Dataset>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != 6 {
                return Err(AppError::Parse(format!(
                    "expected 6 fields in zfs list line: {line:?}"
                )));
            }
            let mountpoint = match fields[4] {
                "-" | "none" => None,
                mp => Some(mp.to_string()),
            };
            Ok(Dataset {
                name: fields[0].to_string(),
                used: super::parse_numeric_or_dash(fields[1])
                    .ok_or_else(|| AppError::Parse(format!("bad used in: {line:?}")))?,
                available: super::parse_numeric_or_dash(fields[2])
                    .ok_or_else(|| AppError::Parse(format!("bad avail in: {line:?}")))?,
                referenced: super::parse_numeric_or_dash(fields[3])
                    .ok_or_else(|| AppError::Parse(format!("bad refer in: {line:?}")))?,
                mountpoint,
                kind: DatasetKind::parse(fields[5]),
            })
        })
        .collect()
}

pub fn list_datasets(runner: &dyn CommandRunner) -> Result<Vec<Dataset>> {
    let output = runner.run("zfs", DATASET_LIST_ARGS)?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: "zfs list".to_string(),
            status: output.status,
            stderr: output.stderr,
        });
    }
    parse_dataset_list(&output.stdout)
}

/// The `--` before `name` stops `zfs` from treating a dataset name that
/// starts with `-` as an option flag (argument injection).
pub fn create_dataset(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    let output = runner.run("zfs", &["create", "--", name])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zfs create {name}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn destroy_dataset(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    let output = runner.run("zfs", &["destroy", "--", name])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zfs destroy {name}"),
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

    const ZFS_LIST_FIXTURE: &str = "\
tank/data\t100000000\t900000000\t100000000\t/tank/data\tfilesystem
tank/vol1\t50000000\t900000000\t50000000\t-\tvolume
";

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
                    mountpoint: Some("/tank/data".to_string()),
                },
                Dataset {
                    name: "tank/vol1".to_string(),
                    kind: DatasetKind::Volume,
                    used: 50_000_000,
                    available: 900_000_000,
                    referenced: 50_000_000,
                    mountpoint: None,
                },
            ]
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
            vec!["list", "-t", "filesystem,volume", "-H", "-p", "-o", "name,used,avail,refer,mountpoint,type"]
        );
    }

    #[test]
    fn parses_dataset_list_line_with_dash_numeric_fields() {
        let datasets = parse_dataset_list("tank/broken\t-\t-\t-\t-\tfilesystem\n").unwrap();
        assert_eq!(
            datasets,
            vec![Dataset {
                name: "tank/broken".to_string(),
                kind: DatasetKind::Filesystem,
                used: 0,
                available: 0,
                referenced: 0,
                mountpoint: None,
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
        assert_eq!(calls[0], ("zfs".to_string(), vec!["destroy".to_string(), "--".to_string(), "tank/olds".to_string()]));
    }

    #[test]
    fn guards_against_dataset_names_that_look_like_flags() {
        let runner = FakeRunner::new("");
        destroy_dataset(&runner, "-r").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1[1], "--", "a leading '--' must precede the dataset name so zfs cannot parse it as an option");
    }
}
