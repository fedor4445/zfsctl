//! The backup form's preflight check: runs `restic cat config` on a host's runner and says what it found.
//! The command and the reading of its output are in `restic`; this adds the worker and the deadline.

use crate::exec::{CommandOutput, CommandRunner};
use crate::rclone;
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

/// The cloud mirror form's check: lists the remote's top level (read-only) on the host's runner, then reads where
/// the rclone configuration lives and its permissions. Gives up after `deadline`, like `run`.
pub fn run_remote(runner: Arc<dyn CommandRunner>, host: &str, remote: &str, deadline: Duration) -> PreflightReport {
    let (tx, rx) = mpsc::channel();
    let r = remote.to_string();
    std::thread::spawn(move || {
        let args = rclone::lsf_args(&r);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let listed = runner.run("rclone", &refs);
        let conf_args = rclone::config_file_args();
        let conf_refs: Vec<&str> = conf_args.iter().map(String::as_str).collect();
        let path = runner.run("rclone", &conf_refs).ok().filter(CommandOutput::success).and_then(|o| rclone::parse_config_file(&o.stdout));
        let stat = path.as_ref().map(|p| {
            let stat_args = restic::password_file_stat_args(p);
            let stat_refs: Vec<&str> = stat_args.iter().map(String::as_str).collect();
            runner.run_unprivileged("stat", &stat_refs)
        });
        let _ = tx.send((listed, path, stat));
    });
    match rx.recv_timeout(deadline) {
        Ok((Ok(out), path, stat)) => {
            let result = match rclone::classify_check(out.status, &out.stdout, &out.stderr, remote, host) {
                rclone::Check::Ok => Preflight::Ok,
                rclone::Check::Problem(m) => Preflight::Problem(m),
            };
            let conf_note = match (path, stat) {
                (Some(path), Some(stat)) => {
                    let read = stat.ok().filter(CommandOutput::success).and_then(|o| restic::parse_password_file_stat(&o.stdout));
                    match read {
                        Some((mode, owner)) => rclone::config_permission_note(&path, mode, &owner),
                        None if result == Preflight::Ok => Some(rclone::CONFIG_UNCHECKED.to_string()),
                        None => None,
                    }
                }
                _ => None,
            };
            let notes: Vec<String> = [rclone::own_folder_warning(remote), conf_note].into_iter().flatten().collect();
            PreflightReport { result, file_note: (!notes.is_empty()).then(|| notes.join("; ")) }
        }
        Ok((Err(e), _, _)) => PreflightReport::problem(format!("could not run rclone on {host}: {}", rclone::redact(&e.to_string(), remote))),
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

    /// Answers `rclone lsf`, `rclone config file` and `stat` from fixed values and records every call.
    struct Rclone {
        lsf: (i32, &'static str),
        stat: Option<&'static str>,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl CommandRunner for Rclone {
        fn run(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
            self.calls.lock().unwrap().push(format!("{cmd} {}", args.join(" ")));
            Ok(match args.first().copied() {
                Some("lsf") => CommandOutput { stdout: String::new(), stderr: self.lsf.1.to_string(), status: self.lsf.0 },
                Some("config") => CommandOutput { stdout: "Configuration file is stored at:\n/root/.config/rclone/rclone.conf\n".to_string(), stderr: String::new(), status: 0 },
                other => panic!("unexpected {other:?}"),
            })
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _stdin: &str) -> crate::error::Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
            self.calls.lock().unwrap().push(format!("{cmd} {}", args.join(" ")));
            assert_eq!(cmd, "stat");
            Ok(match self.stat {
                Some(out) => CommandOutput { stdout: out.to_string(), stderr: String::new(), status: 0 },
                None => CommandOutput { stdout: String::new(), stderr: "Permission denied".to_string(), status: 1 },
            })
        }
    }

    fn rclone_host(lsf: (i32, &'static str), stat: Option<&'static str>) -> Arc<Rclone> {
        Arc::new(Rclone { lsf, stat, calls: std::sync::Mutex::new(vec![]) })
    }

    #[test]
    fn run_remote_reports_ok_and_only_ever_runs_read_only_commands() {
        let host = rclone_host((0, ""), Some("600 root\n"));
        let r = run_remote(host.clone(), "fb", "aws:bkt/p", Duration::from_secs(5));
        assert_eq!((r.result, r.file_note), (Preflight::Ok, None));
        let calls = host.calls.lock().unwrap().clone();
        assert_eq!(calls[0], format!("rclone {}", rclone::lsf_args("aws:bkt/p").join(" ")));
        assert!(calls.iter().all(|c| c.starts_with("rclone lsf") || c.starts_with("rclone config file") || c.starts_with("stat")), "{calls:?}");
    }

    #[test]
    fn run_remote_says_in_plain_words_what_is_wrong() {
        for (lsf, want) in [
            ((1, "CRITICAL: Failed to create file system for \"aws:b\": didn't find section in config file (\"aws\")"), "remote 'aws' is not in root's rclone config on fb"),
            ((3, "ERROR : error listing: directory not found"), "bucket or folder was not found"),
            ((1, "403 Forbidden AccessDenied"), "refused the keys"),
        ] {
            let r = run_remote(rclone_host(lsf, Some("600 root\n")), "fb", "aws:b", Duration::from_secs(5));
            assert!(matches!(&r.result, Preflight::Problem(m) if m.contains(want)), "{:?}", r.result);
        }
    }

    #[test]
    fn run_remote_notes_a_readable_config_an_unreadable_one_and_a_mirror_folder_path() {
        let r = run_remote(rclone_host((0, ""), Some("644 fb\n")), "fb", "aws:bkt/p", Duration::from_secs(5));
        assert!(r.file_note.unwrap().contains("chmod 600 /root/.config/rclone/rclone.conf"));
        let r = run_remote(rclone_host((0, ""), None), "fb", "aws:bkt/p", Duration::from_secs(5));
        assert_eq!(r.file_note.as_deref(), Some(rclone::CONFIG_UNCHECKED));
        let r = run_remote(rclone_host((0, ""), Some("600 root\n")), "fb", "aws:bkt/p/old", Duration::from_secs(5));
        assert!(r.file_note.unwrap().contains("'old'"));
    }

    #[test]
    fn a_slow_remote_gives_up_after_the_deadline() {
        let r = run_remote(Arc::new(SlowRunner { delay: Duration::from_millis(300) }), "fb", "aws:b", Duration::from_millis(20));
        assert!(matches!(&r.result, Preflight::Problem(m) if m.contains("no answer from fb")), "{:?}", r.result);
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
