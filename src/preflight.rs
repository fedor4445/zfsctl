//! The backup form's preflight check: runs `restic cat config` on a host's runner and says what it found.
//! The command and the reading of its output are in `restic`; this adds the worker and the deadline.

use crate::exec::CommandRunner;
use crate::restic::{self, Preflight};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// How long the form waits for an answer before saying there was none.
pub const DEADLINE: Duration = Duration::from_secs(45);

/// Runs the check on `runner` and gives up after `deadline`. The command runs on its own thread so a hung
/// connection cannot hold the caller; a late result is simply dropped.
pub fn run(runner: Arc<dyn CommandRunner>, host: &str, repo: &str, password_file: &str, deadline: Duration) -> Preflight {
    let (tx, rx) = mpsc::channel();
    let (r, p) = (repo.to_string(), password_file.to_string());
    std::thread::spawn(move || {
        let args = restic::cat_config_args(&r, &p);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let _ = tx.send(runner.run("restic", &refs));
    });
    match rx.recv_timeout(deadline) {
        Ok(Ok(out)) => restic::classify_preflight(out.status, &out.stdout, &out.stderr, repo, host, password_file),
        Ok(Err(e)) => Preflight::Problem(format!("could not run restic on {host}: {}", restic::redact(&e.to_string(), repo))),
        Err(_) => Preflight::Problem(format!("no answer from {host} in {} s", deadline.as_secs())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;
    use crate::exec::CommandOutput;

    /// A runner whose commands take `delay` and then fail like a missing remote.
    struct SlowRunner {
        delay: Duration,
    }

    impl CommandRunner for SlowRunner {
        fn run(&self, _cmd: &str, _args: &[&str]) -> crate::error::Result<CommandOutput> {
            std::thread::sleep(self.delay);
            Ok(CommandOutput { stdout: String::new(), stderr: "Fatal: late".to_string(), status: 1 })
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _stdin: &str) -> crate::error::Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
            self.run(cmd, args)
        }
    }

    #[test]
    fn a_working_repository_runs_one_read_only_restic_call_and_is_ok() {
        let runner = Arc::new(FakeRunner::new("{\"version\":2,\"id\":\"c87554e4\"}"));
        let r = run(runner.clone(), "fb", "rclone:t1:restic-tank", "/root/.restic-password", Duration::from_secs(5));
        assert_eq!(r, Preflight::Ok);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].0, "restic");
        assert_eq!(calls[0].1, restic::cat_config_args("rclone:t1:restic-tank", "/root/.restic-password"));
    }

    #[test]
    fn a_failing_call_is_classified_and_masked() {
        let runner = Arc::new(FakeRunner::failing("Fatal: wrong password or no key found", 12));
        let r = run(runner, "fb", "/mnt/usb/restic", "/root/pw", Duration::from_secs(5));
        assert!(matches!(&r, Preflight::Problem(m) if m.contains("does not unlock")), "{r:?}");
    }

    #[test]
    fn a_runner_error_says_so_without_the_repository() {
        struct Broken;
        impl CommandRunner for Broken {
            fn run(&self, _c: &str, _a: &[&str]) -> crate::error::Result<CommandOutput> {
                Err(crate::error::AppError::Config("ssh to /mnt/usb/restic failed".to_string()))
            }
            fn run_with_stdin(&self, c: &str, a: &[&str], _s: &str) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
            fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
        }
        let r = run(Arc::new(Broken), "fb", "/mnt/usb/restic", "/root/pw", Duration::from_secs(5));
        let Preflight::Problem(m) = r else { panic!("expected a problem") };
        assert!(m.contains("could not run restic on fb") && !m.contains("/mnt/usb/restic"), "{m}");
    }

    #[test]
    fn a_target_that_never_answers_is_reported_after_the_deadline() {
        let start = std::time::Instant::now();
        let r = run(Arc::new(SlowRunner { delay: Duration::from_secs(3) }), "fb", "/r", "/pw", Duration::from_millis(100));
        assert!(start.elapsed() < Duration::from_secs(2), "must not wait for the slow command");
        assert!(matches!(&r, Preflight::Problem(m) if m.contains("no answer from fb")), "{r:?}");
    }
}
