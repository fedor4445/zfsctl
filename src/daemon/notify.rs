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

#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Runs `command` through `sh -c` with `env` added and `stdin` on its standard input, waiting up to `timeout`.
/// Output goes to the daemon's own stdout/stderr, which the rc script sends to the log. A command that
/// outlives the timeout is killed. The command sees the package directories on `PATH` and the account's home.
pub fn run_command(command: &str, env: &[(&str, &str)], stdin: &str, timeout: Duration) -> Result<(), NotifyError> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command).stdin(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    crate::exec::local::prepare_child(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| NotifyError::Spawn(e.to_string()))?;
    // Feed stdin on its own thread: a command that never reads it must not block us, nor a full pipe.
    let feeder = child.stdin.take().map(|mut pipe| {
        let text = stdin.to_string();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = pipe.write_all(text.as_bytes());
        })
    });
    let started = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(status)) => break Err(NotifyError::Exit(status.code().unwrap_or(-1))),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(NotifyError::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => break Err(NotifyError::Spawn(e.to_string())),
        }
    };
    if let Some(feeder) = feeder {
        let _ = feeder.join();
    }
    result
}

/// Runs `command` with the job notice in its environment (and nothing on stdin).
pub fn run(command: &str, notice: &Notice, timeout: Duration) -> Result<(), NotifyError> {
    let env = [("ZFSCTL_JOB", notice.job.as_str()), ("ZFSCTL_HOST", notice.host.as_str()), ("ZFSCTL_KIND", notice.kind.as_str()), ("ZFSCTL_RESULT", notice.result.as_str()), ("ZFSCTL_MESSAGE", notice.message.as_str())];
    run_command(command, &env, "", timeout)
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
