//! The backup form's preflight check: runs `restic cat config` on a host's runner and says what it found.
//! The command and the reading of its output are in `restic`; this adds the worker and the deadline.

use crate::exec::{CommandOutput, CommandRunner};
use crate::restic::{self, Preflight};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// How long the form waits for an answer before saying there was none.
pub const DEADLINE: Duration = Duration::from_secs(45);

/// What the check found: the repository finding, and advice about the password file when there is some.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    pub result: Preflight,
    /// About the file's mode and owner, read without sudo. `None` when it is fine (or the check failed first).
    pub file_note: Option<String>,
}

impl PreflightReport {
    fn problem(message: String) -> Self {
        PreflightReport { result: Preflight::Problem(message), file_note: None }
    }
}

/// Runs the check on `runner` and gives up after `deadline`. The commands run on their own thread so a hung
/// connection cannot hold the caller; a late result is simply dropped.
pub fn run(runner: Arc<dyn CommandRunner>, host: &str, repo: &str, password_file: &str, deadline: Duration) -> PreflightReport {
    let (tx, rx) = mpsc::channel();
    let (r, p) = (repo.to_string(), password_file.to_string());
    std::thread::spawn(move || {
        let args = restic::cat_config_args(&r, &p);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let checked = runner.run("restic", &refs);
        // The permissions are read afterwards and without sudo: a host whose ssh user cannot see the file is not checked.
        let stat_args = restic::password_file_stat_args(&p);
        let stat_refs: Vec<&str> = stat_args.iter().map(String::as_str).collect();
        let _ = tx.send((checked, runner.run_unprivileged("stat", &stat_refs)));
    });
    match rx.recv_timeout(deadline) {
        Ok((Ok(out), stat)) => {
            let result = restic::classify_preflight(out.status, &out.stdout, &out.stderr, repo, host, password_file);
            let file_note = file_note(&result, stat, password_file);
            PreflightReport { result, file_note }
        }
        Ok((Err(e), _)) => PreflightReport::problem(format!("could not run restic on {host}: {}", restic::redact(&e.to_string(), repo))),
        Err(_) => PreflightReport::problem(format!("no answer from {host} in {} s", deadline.as_secs())),
    }
}

/// Advice from what `stat` said about the password file. A file whose permissions cannot be read is only worth
/// a word when the repository opened, so the file exists and root can read it.
fn file_note(result: &Preflight, stat: crate::error::Result<CommandOutput>, path: &str) -> Option<String> {
    let read = stat.ok().filter(CommandOutput::success).and_then(|o| restic::parse_password_file_stat(&o.stdout));
    match read {
        Some((mode, owner)) => restic::password_file_permission_note(path, mode, &owner),
        None if *result == Preflight::Ok => Some(restic::PASSWORD_FILE_UNCHECKED.to_string()),
        None => None,
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
    fn a_working_repository_runs_one_read_only_restic_call_and_a_read_only_stat_and_is_ok() {
        let runner = Arc::new(FakeRunner::new("{\"version\":2,\"id\":\"c87554e4\"}"));
        let r = run(runner.clone(), "fb", "rclone:t1:restic-tank", "/root/.restic-password", Duration::from_secs(5)).result;
        assert_eq!(r, Preflight::Ok);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0].0, "restic");
        assert_eq!(calls[0].1, restic::cat_config_args("rclone:t1:restic-tank", "/root/.restic-password"));
        assert_eq!((calls[1].0.as_str(), calls[1].1.clone()), ("stat", restic::password_file_stat_args("/root/.restic-password")));
    }

    #[test]
    fn a_failing_call_is_classified_and_masked() {
        let runner = Arc::new(FakeRunner::failing("Fatal: wrong password or no key found", 12));
        let r = run(runner, "fb", "/mnt/usb/restic", "/root/pw", Duration::from_secs(5)).result;
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
        let r = run(Arc::new(Broken), "fb", "/mnt/usb/restic", "/root/pw", Duration::from_secs(5)).result;
        let Preflight::Problem(m) = r else { panic!("expected a problem") };
        assert!(m.contains("could not run restic on fb") && !m.contains("/mnt/usb/restic"), "{m}");
    }

    #[test]
    fn a_target_that_never_answers_is_reported_after_the_deadline() {
        let start = std::time::Instant::now();
        let r = run(Arc::new(SlowRunner { delay: Duration::from_secs(3) }), "fb", "/r", "/pw", Duration::from_millis(100)).result;
        assert!(start.elapsed() < Duration::from_secs(2), "must not wait for the slow command");
        assert!(matches!(&r, Preflight::Problem(m) if m.contains("no answer from fb")), "{r:?}");
    }

    /// A host that answers `restic` with `restic` and `stat` (run without sudo) with `stat`.
    struct Scripted {
        restic: CommandOutput,
        stat: Option<CommandOutput>,
        unprivileged: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    }

    impl Scripted {
        fn new(restic_ok: bool, stat: Option<&str>) -> Arc<Self> {
            let restic = if restic_ok {
                CommandOutput { stdout: "{\"version\":2}".to_string(), stderr: String::new(), status: 0 }
            } else {
                CommandOutput { stdout: String::new(), stderr: "Fatal: wrong password or no key found".to_string(), status: 12 }
            };
            let stat = stat.map(|out| CommandOutput { stdout: out.to_string(), stderr: String::new(), status: 0 });
            Arc::new(Scripted { restic, stat, unprivileged: std::sync::Mutex::new(Vec::new()) })
        }
    }

    impl CommandRunner for Scripted {
        fn run(&self, cmd: &str, _args: &[&str]) -> crate::error::Result<CommandOutput> {
            assert_eq!(cmd, "restic", "only restic runs through sudo");
            Ok(self.restic.clone())
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _stdin: &str) -> crate::error::Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
            self.unprivileged.lock().unwrap().push((cmd.to_string(), args.iter().map(|a| (*a).to_string()).collect()));
            Ok(self.stat.clone().unwrap_or(CommandOutput { stdout: String::new(), stderr: "stat: /root/pw: Permission denied".to_string(), status: 1 }))
        }
    }

    fn report(host: &Arc<Scripted>) -> PreflightReport {
        run(host.clone(), "fb", "/mnt/usb/restic", "/root/pw", Duration::from_secs(5))
    }

    #[test]
    fn a_root_only_password_file_gets_no_note_and_the_stat_runs_without_sudo() {
        let host = Scripted::new(true, Some("600 root\n"));
        assert_eq!(report(&host), PreflightReport { result: Preflight::Ok, file_note: None });
        assert_eq!(*host.unprivileged.lock().unwrap(), vec![("stat".to_string(), vec!["-f".to_string(), "%Lp %Su".to_string(), "/root/pw".to_string()])]);
    }

    #[test]
    fn a_world_readable_or_foreign_password_file_gets_a_note_even_when_the_repository_opens() {
        let r = report(&Scripted::new(true, Some("644 root")));
        assert_eq!(r.result, Preflight::Ok);
        assert!(r.file_note.is_some_and(|n| n.contains("mode 644") && n.contains("chmod 600 /root/pw")));
        let r = report(&Scripted::new(true, Some("600 fb")));
        assert!(r.file_note.is_some_and(|n| n.contains("owned by fb")));
    }

    #[test]
    fn the_note_also_shows_when_the_repository_does_not_open() {
        let r = report(&Scripted::new(false, Some("666 root")));
        assert!(matches!(r.result, Preflight::Problem(_)));
        assert!(r.file_note.is_some_and(|n| n.contains("mode 666")));
    }

    #[test]
    fn a_file_the_ssh_user_cannot_see_is_reported_as_unchecked_but_only_when_the_repository_opened() {
        let r = report(&Scripted::new(true, None));
        assert_eq!(r.file_note.as_deref(), Some(restic::PASSWORD_FILE_UNCHECKED));
        let r = report(&Scripted::new(false, None));
        assert_eq!(r.file_note, None, "a failed check already says what is wrong");
        // A stat that answers with something else (another `stat`) is treated the same way.
        let r = report(&Scripted::new(true, Some("File: /root/pw Size: 6")));
        assert_eq!(r.file_note.as_deref(), Some(restic::PASSWORD_FILE_UNCHECKED));
    }
}
