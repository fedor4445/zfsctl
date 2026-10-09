pub mod bootenv;
pub mod accounts;
pub mod dataset;
pub mod disk;
pub mod disk_detail;
pub mod host;
pub mod host_health;
pub mod metrics;
pub mod permission;
pub mod pool;
pub mod property;
pub mod share;
pub mod share_detail;
pub mod smb;
pub mod replication;
pub mod snapshot;
pub mod snapshot_detail;

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
        let output = run_checked(&runner, "echo", &["hello"], "echo hello").unwrap();
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
}
