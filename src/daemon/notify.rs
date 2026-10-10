//! The operator's notify command: run when a job fails.
//!
//! The command is configured by the operator in `config.toml` and run through
//! `sh -c`. Job data reaches it only through environment variables, never
//! through the command line, so nothing in a job's message can be executed.

use std::os::unix::process::CommandExt;
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
    // Its own process group, so a timeout can kill the command's children too, not just `sh`.
    cmd.arg("-c").arg(command).stdin(Stdio::piped()).process_group(0);
    for (k, v) in env {
        cmd.env(k, v);
    }
    crate::exec::local::prepare_child(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| NotifyError::Spawn(e.to_string()))?;
    // Feed stdin on its own thread, never joined: a command that never reads it, or a grandchild that keeps the
    // pipe open, must not block us. The thread ends when the pipe closes.
    if let Some(mut pipe) = child.stdin.take() {
        let text = stdin.to_string();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(NotifyError::Exit(status.code().unwrap_or(-1))),
            Ok(None) if started.elapsed() >= timeout => {
                // The whole group: a command like `sleep 5; msmtp -t` has children `kill` on `sh` would leave behind.
                if let Ok(pid) = i32::try_from(child.id()) {
                    let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(NotifyError::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(NotifyError::Spawn(e.to_string())),
        }
    }
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
    fn a_timeout_kills_the_commands_children_too_and_a_big_body_cannot_hang_the_caller() {
        // `sh` runs `sleep` as a child (two commands, so no exec): killing only `sh` would leave `sleep` holding the
        // stdin pipe, and writing a body larger than the pipe buffer to it would block for as long as it lives.
        let big = "x".repeat(2_000_000);
        let started = Instant::now();
        let err = run_command("sleep 20; cat > /dev/null", &[], &big, Duration::from_millis(300)).unwrap_err();
        assert_eq!(err, NotifyError::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5), "returned after {:?}", started.elapsed());
    }

    #[test]
    fn a_timeout_leaves_no_child_of_the_command_running() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let cmd = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
        assert_eq!(run_command(&cmd, &[], "", Duration::from_millis(500)).unwrap_err(), NotifyError::TimedOut);
        let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        let gone = (0..40).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err()
        });
        assert!(gone, "the sleep the command started is still alive");
    }

    #[test]
    fn a_background_child_holding_stdin_does_not_hold_up_a_command_that_has_finished() {
        let big = "x".repeat(2_000_000);
        let started = Instant::now();
        run_command("sleep 20 & exit 0", &[], &big, Duration::from_secs(10)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5), "returned after {:?}", started.elapsed());
    }

    #[test]
    fn a_failing_command_reports_its_status_and_a_missing_one_is_a_failure_too() {
        assert_eq!(run("exit 7", &notice("x"), TIMEOUT).unwrap_err(), NotifyError::Exit(7));
        assert_eq!(run("/nonexistent/notify-me", &notice("x"), TIMEOUT).unwrap_err(), NotifyError::Exit(127));
    }
}
