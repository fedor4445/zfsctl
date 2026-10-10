pub mod local;
pub mod privilege;
pub mod ssh;

use crate::error::{AppError, Result};

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
}

impl CommandOutput {
    /// A child killed by a signal has no exit code; report it as `-1`.
    pub(crate) fn from_output(output: &std::process::Output) -> Self {
        Self {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status.code().unwrap_or(-1),
        }
    }

    pub fn success(&self) -> bool {
        self.status == 0
    }
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput>;
    /// Like `run`, but writes `stdin` to the child's standard input and
    /// then closes it. Needed for `smbpasswd -s` (password on stdin, never
    /// an argument) and for writing a file via `tee`.
    fn run_with_stdin(&self, cmd: &str, args: &[&str], stdin: &str) -> Result<CommandOutput>;
    /// Runs `cmd` WITHOUT privilege escalation: over SSH this omits the
    /// `sudo -n` prefix every other call carries; locally it is the same as
    /// `run`. For read-only lookups that need no privilege (e.g.
    /// `getent passwd`). Never use it for anything that mutates state.
    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput>;
    /// The command this runner would run, unspawned, so a caller can wire its own pipes
    /// (replication pipes one host's `zfs send` into another's `zfs receive`). The default
    /// refuses, so a runner that cannot do this (a test fake) says so rather than guessing.
    fn command(&self, cmd: &str, args: &[&str]) -> Result<std::process::Command> {
        let _ = (cmd, args);
        Err(AppError::Config("this runner cannot hand out a command to spawn".to_string()))
    }
}

/// Shared by `LocalRunner` and `SshRunner`. Stdin is written from a
/// separate thread: `tee` echoes its input to stdout, so writing it all
/// before reading any output would deadlock once the input exceeds a
/// pipe buffer. A write error (child exited without reading everything)
/// is ignored on purpose: the exit status is the real signal.
pub(crate) fn run_command_with_stdin(
    mut command: std::process::Command,
    spawn_name: &str,
    stdin: &str,
) -> Result<CommandOutput> {
    use crate::error::AppError;
    use std::io::Write;
    use std::process::Stdio;

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| AppError::Spawn { cmd: spawn_name.to_string(), source })?;
    let mut pipe = child.stdin.take().expect("stdin was requested as piped");
    let data = stdin.to_string();
    let writer = std::thread::spawn(move || {
        let _ = pipe.write_all(data.as_bytes());
    });
    let output = child
        .wait_with_output()
        .map_err(|source| AppError::Spawn { cmd: spawn_name.to_string(), source })?;
    let _ = writer.join();
    Ok(CommandOutput::from_output(&output))
}

#[cfg(test)]
pub mod test_support {
    use super::{CommandOutput, CommandRunner};
    use crate::error::Result;
    use std::sync::Mutex;

    /// A `CommandRunner` that records every call it receives and, by
    /// default, always returns the same scripted `CommandOutput`. Used
    /// by unit tests throughout the crate so ZFS/host logic can be
    /// tested without a real system.
    ///
    /// Some library functions (e.g.
    /// `zfs::disk::list_disks_with_membership`) compose several
    /// different subcommands in one call — a single scripted response
    /// can't stand in for `geom disk list`, `zpool list`, and `zpool
    /// status` all at once. `route` scripts a distinct response for
    /// calls matching a given `cmd` and first argument (e.g. `("zpool",
    /// "status")` vs `("zpool", "list")`); calls that don't match any
    /// route fall back to `response`.
    pub struct FakeRunner {
        pub calls: Mutex<Vec<(String, Vec<String>)>>,
        pub stdin_calls: Mutex<Vec<(String, String)>>,
        pub unprivileged_calls: Mutex<Vec<(String, Vec<String>)>>,
        pub response: CommandOutput,
        routes: std::collections::HashMap<(String, String), CommandOutput>,
    }

    impl FakeRunner {
        pub fn new(stdout: &str) -> Self {
            FakeRunner {
                calls: Mutex::new(Vec::new()),
                stdin_calls: Mutex::new(Vec::new()),
                unprivileged_calls: Mutex::new(Vec::new()),
                response: CommandOutput {
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                    status: 0,
                },
                routes: std::collections::HashMap::new(),
            }
        }

        pub fn failing(stderr: &str, status: i32) -> Self {
            FakeRunner {
                calls: Mutex::new(Vec::new()),
                stdin_calls: Mutex::new(Vec::new()),
                unprivileged_calls: Mutex::new(Vec::new()),
                response: CommandOutput {
                    stdout: String::new(),
                    stderr: stderr.to_string(),
                    status,
                },
                routes: std::collections::HashMap::new(),
            }
        }

        /// Scripts a distinct successful response for calls matching
        /// `cmd` whose first argument is `first_arg`. Builder-style —
        /// call before wrapping the runner in `Arc`.
        pub fn route(mut self, cmd: &str, first_arg: &str, stdout: &str) -> Self {
            self.routes.insert(
                (cmd.to_string(), first_arg.to_string()),
                CommandOutput { stdout: stdout.to_string(), stderr: String::new(), status: 0 },
            );
            self
        }

        /// Like `route`, but scripts a failing response. Builder-style.
        pub fn route_failure(mut self, cmd: &str, first_arg: &str, stderr: &str, status: i32) -> Self {
            self.routes.insert(
                (cmd.to_string(), first_arg.to_string()),
                CommandOutput { stdout: String::new(), stderr: stderr.to_string(), status },
            );
            self
        }

        /// Overrides the default response's exit status while keeping
        /// its stdout — needed for tests simulating "a nonzero exit
        /// with meaningful stdout" (e.g. a status check whose text
        /// still reports useful state despite a nonzero overall exit),
        /// which neither `new` (always status 0) nor `failing` (always
        /// empty stdout) can express together. Builder-style, like
        /// `route`.
        pub fn with_status(mut self, status: i32) -> Self {
            self.response.status = status;
            self
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            assert!(
                crate::exec::privilege::is_registered(cmd),
                "{cmd} is not a registered remote command: add it to exec::privilege::ENTRIES, or use a registered command in this test"
            );
            self.calls
                .lock()
                .unwrap()
                .push((cmd.to_string(), args.iter().map(std::string::ToString::to_string).collect()));
            let first_arg = args.first().copied().unwrap_or("");
            if let Some(routed) = self.routes.get(&(cmd.to_string(), first_arg.to_string())) {
                return Ok(routed.clone());
            }
            Ok(self.response.clone())
        }

        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.unprivileged_calls.lock().unwrap().push((cmd.to_string(), args.iter().map(std::string::ToString::to_string).collect()));
            self.run(cmd, args)
        }

        fn run_with_stdin(&self, cmd: &str, args: &[&str], stdin: &str) -> Result<CommandOutput> {
            self.stdin_calls.lock().unwrap().push((cmd.to_string(), stdin.to_string()));
            let mut out = self.run(cmd, args)?;
            // Like `tee`, echo stdin as stdout unless a route scripted this call.
            let first_arg = args.first().copied().unwrap_or("");
            if !self.routes.contains_key(&(cmd.to_string(), first_arg.to_string())) {
                out.stdout = stdin.to_string();
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeRunner;
    use super::{CommandOutput, CommandRunner};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    #[test]
    fn from_output_reports_the_exit_code_and_both_streams() {
        let output = Output {
            status: ExitStatus::from_raw(3 << 8), // wait status for `exit(3)`
            stdout: b"out".to_vec(),
            stderr: b"err".to_vec(),
        };
        let converted = CommandOutput::from_output(&output);
        assert_eq!((converted.stdout.as_str(), converted.stderr.as_str(), converted.status), ("out", "err", 3));
    }

    #[test]
    fn from_output_reports_a_signal_killed_child_as_minus_one() {
        let output = Output { status: ExitStatus::from_raw(9), stdout: Vec::new(), stderr: Vec::new() };
        assert_eq!(CommandOutput::from_output(&output).status, -1);
    }

    #[test]
    fn fake_runner_records_unprivileged_calls_separately_and_honours_routes() {
        let runner = FakeRunner::new("").route("getent", "passwd", "root:*:0:0::/root:/bin/sh\n");
        runner.run("zfs", &["list"]).unwrap();
        let out = runner.run_unprivileged("getent", &["passwd"]).unwrap();
        assert_eq!(out.stdout, "root:*:0:0::/root:/bin/sh\n");
        assert_eq!(runner.unprivileged_calls.lock().unwrap().len(), 1);
        assert_eq!(runner.unprivileged_calls.lock().unwrap()[0].0, "getent");
        assert_eq!(runner.calls.lock().unwrap().len(), 2, "also recorded in the general call log");
    }

    #[test]
    fn fake_runner_records_stdin_and_honours_failure_routes() {
        let runner = FakeRunner::new("").route_failure("smbpasswd", "-a", "no such user", 1);
        let out = runner.run_with_stdin("smbpasswd", &["-a", "-s", "bob"], "pw\npw\n").unwrap();
        assert_eq!(out.status, 1);
        assert_eq!(out.stderr, "no such user");
        assert_eq!(runner.stdin_calls.lock().unwrap()[0], ("smbpasswd".to_string(), "pw\npw\n".to_string()));
        assert_eq!(runner.calls.lock().unwrap()[0].0, "smbpasswd");
    }

    #[test]
    fn fake_runner_route_overrides_default_response_for_matching_cmd_and_first_arg() {
        let runner = FakeRunner::new("default")
            .route("zpool", "status", "status-output")
            .route("zpool", "list", "list-output");
        assert_eq!(runner.run("zpool", &["status", "--", "tank"]).unwrap().stdout, "status-output");
        assert_eq!(runner.run("zpool", &["list", "-H"]).unwrap().stdout, "list-output");
        assert_eq!(
            runner.run("geom", &["disk", "list"]).unwrap().stdout,
            "default",
            "an unrouted call falls back to the default response"
        );
    }

    #[test]
    fn fake_runner_with_status_keeps_custom_stdout_with_a_nonzero_status() {
        let runner = FakeRunner::new("smbd is running as pid 1.\nwinbindd is not running.\n").with_status(1);
        let output = runner.run("service", &["samba_server", "onestatus"]).unwrap();
        assert_eq!(output.stdout, "smbd is running as pid 1.\nwinbindd is not running.\n");
        assert_eq!(output.status, 1);
    }

    #[test]
    fn a_runner_without_command_support_refuses_instead_of_guessing() {
        assert!(FakeRunner::new("").command("zfs", &[]).is_err());
    }
}

#[cfg(test)]
mod guard_tests {
    use super::test_support::FakeRunner;
    use super::CommandRunner;

    #[test]
    #[should_panic(expected = "not a registered remote command")]
    fn the_fake_runner_panics_on_a_command_nobody_registered() {
        let _ = FakeRunner::new("").run("not-a-registered-command", &[]);
    }

    #[test]
    fn the_fake_runner_accepts_registered_commands() {
        assert!(FakeRunner::new("x").run("zpool", &["list"]).is_ok());
    }
}
