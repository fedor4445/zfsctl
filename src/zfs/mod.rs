pub mod bootenv;
pub mod accounts;
pub mod dataset;
pub mod disk;
pub mod disk_detail;
pub mod encryption;
pub mod host;
pub mod host_health;
pub mod host_resources;
pub mod ashift;
pub mod integrity;
pub mod local_settings;
pub mod sync;
pub mod metrics;
pub mod net;
pub mod permission;
pub mod pool;
pub mod pool_classes;
pub mod pool_info;
pub mod property;
pub mod quota;
pub mod vdev_role;
pub mod share;
pub mod share_detail;
pub mod smb;
pub mod replication;
pub mod name_filter;
pub mod snapshot;
pub mod snapshot_detail;
pub mod snapshot_browse;
pub mod snapshot_changes;
pub mod snapshot_diff;
pub mod snapshot_restore;

use crate::error::{AppError, Result};
use crate::exec::{CommandOutput, CommandRunner};

/// Parses a `-H -p` numeric column that may show `-` when the value is
/// unavailable (e.g. `zpool list` prints `-` for SIZE/ALLOC/FREE on a
/// FAULTED/UNAVAIL pool). Treats `-` as `0` rather than a parse error,
/// so unhealthy pools/datasets/snapshots still show up in listings.
pub(crate) fn parse_numeric_or_dash(s: &str) -> Option<u64> {
    if s == "-" {
        Some(0)
    } else {
        s.parse().ok()
    }
}

/// Runs `cmd args` via `runner` and maps a nonzero exit to
/// `AppError::CommandFailed`, using `display` as the human-readable
/// command description in the error (since `args` may include `--`
/// guards or a shell pipeline that isn't worth showing verbatim).
/// A name that starts with `-` would be read as an option by a command that takes no `--` (a `get`, whose dataset comes last
/// because illumos reads a `--` after the property list as a name).
pub(crate) fn not_an_option(name: &str) -> Result<()> {
    if name.starts_with('-') {
        return Err(AppError::Parse(format!("not a dataset or pool name: {name}")));
    }
    Ok(())
}

/// Collapses the success/failure-check block that was previously
/// hand-duplicated at every call site in `pool.rs`/`dataset.rs`/
/// `snapshot.rs`.
pub(crate) fn run_checked(
    runner: &dyn CommandRunner,
    cmd: &str,
    args: &[&str],
    display: &str,
) -> Result<CommandOutput> {
    let output = runner.run(cmd, args)?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: display.to_string(),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    #[test]
    fn run_checked_returns_output_on_success() {
        let runner = FakeRunner::new("hello\n");
        let output = run_checked(&runner, "zpool", &["list"], "zpool list").unwrap();
        assert_eq!(output.stdout, "hello\n");
    }

    #[test]
    fn run_checked_maps_failure_to_command_failed_with_display_string() {
        let runner = FakeRunner::failing("boom", 3);
        let result = run_checked(&runner, "zpool", &["destroy", "--", "tank"], "zpool destroy tank");
        match result {
            Err(AppError::CommandFailed { cmd, status, stderr }) => {
                assert_eq!(cmd, "zpool destroy tank");
                assert_eq!(status, 3);
                assert_eq!(stderr, "boom");
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }

    /// illumos's `zfs` and `zpool` stop reading options at the first argument that is not one, so in a `get` the `--` that
    /// follows the property list is taken for a dataset name (found on `OmniOS`: `zfs get all -H -p -- ds`). In a `get` the
    /// dataset comes last, with no `--`; a name that starts with `-` is refused by the caller instead.
    #[test]
    fn no_get_call_puts_a_double_dash_after_the_property_list() {
        fn walk(dir: &std::path::Path, hits: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, hits);
                } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("zfs/mod.rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    for (n, line) in text.lines().enumerate() {
                        if let Some(i) = line.find("[\"get\",") {
                            let array = line[i..].split(']').next().unwrap_or("");
                            if array.contains("\"--\"") {
                                hits.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                            }
                        }
                    }
                }
            }
        }
        let mut hits = Vec::new();
        walk(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")), &mut hits);
        assert!(hits.is_empty(), "{}", hits.join("\n"));
    }
}
