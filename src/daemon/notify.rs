//! The operator's notify command: run when a job fails.
//!
//! The command is configured by the operator in `config.toml` and run through
//! `sh -c`. Job data reaches it only through environment variables, never
//! through the command line, so nothing in a job's message can be executed.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub job: String,
    pub host: String,
    pub kind: String,
    pub result: String,
    pub message: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum NotifyError {
    Spawn(String),
    TimedOut,
    Exit(i32),
}

impl std::fmt::Display for NotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotifyError::Spawn(e) => write!(f, "could not start: {e}"),
            NotifyError::TimedOut => write!(f, "timed out"),
            NotifyError::Exit(code) => write!(f, "exited with status {code}"),
        }
    }
}

/// Runs `command` with the notice in its environment and waits up to `timeout`.
/// Its output goes to the daemon's own stdout/stderr, which the rc script
/// sends to the log. A command that outlives the timeout is killed.
pub fn run(command: &str, notice: &Notice, timeout: Duration) -> Result<(), NotifyError> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("ZFSCTL_JOB", &notice.job)
        .env("ZFSCTL_HOST", &notice.host)
        .env("ZFSCTL_KIND", &notice.kind)
        .env("ZFSCTL_RESULT", &notice.result)
        .env("ZFSCTL_MESSAGE", &notice.message)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| NotifyError::Spawn(e.to_string()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(NotifyError::Exit(status.code().unwrap_or(-1))),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(NotifyError::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(NotifyError::Spawn(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(message: &str) -> Notice {
        Notice { job: "scrub-tank".into(), host: "local".into(), kind: "scrub".into(), result: "failed".into(), message: message.into() }
    }

    #[test]
    fn the_notice_arrives_in_environment_variables() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let cmd = format!("printf '%s|%s|%s|%s|%s' \"$ZFSCTL_JOB\" \"$ZFSCTL_HOST\" \"$ZFSCTL_KIND\" \"$ZFSCTL_RESULT\" \"$ZFSCTL_MESSAGE\" > {}", out.display());
        run(&cmd, &notice("2 errors"), TIMEOUT).unwrap();
        assert_eq!(std::fs::read_to_string(out).unwrap(), "scrub-tank|local|scrub|failed|2 errors");
    }

    #[test]
    fn job_data_is_never_executed_as_part_of_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let canary = dir.path().join("pwned");
        let evil = format!("$(touch {}); `touch {}`", canary.display(), canary.display());
        run(&format!("printf '%s' \"$ZFSCTL_MESSAGE\" > {}", out.display()), &notice(&evil), TIMEOUT).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), evil, "the message is passed through verbatim");
        assert!(!canary.exists(), "and was not run");
    }

    #[test]
    fn a_command_that_outlives_the_timeout_is_killed() {
        let started = Instant::now();
        let err = run("exec sleep 30", &notice("x"), Duration::from_millis(300)).unwrap_err();
        assert_eq!(err, NotifyError::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5), "returned promptly, not after 30s");
    }

    #[test]
    fn a_failing_command_reports_its_status_and_a_missing_one_is_a_failure_too() {
        assert_eq!(run("exit 7", &notice("x"), TIMEOUT).unwrap_err(), NotifyError::Exit(7));
        assert_eq!(run("/nonexistent/notify-me", &notice("x"), TIMEOUT).unwrap_err(), NotifyError::Exit(127));
    }
}
