use super::{CommandOutput, CommandRunner};
use crate::error::{AppError, Result};
use std::process::Command;

pub struct LocalRunner;

/// Where packages install their programs (`restic`, `smbpasswd`, ...). The daemon is started by rc
/// with a minimal `PATH` that lacks them, while an interactive shell has them.
const PACKAGE_DIRS: &[&str] = &["/usr/local/sbin", "/usr/local/bin"];

/// Appends each of `dirs` that the command's inherited `PATH` does not already hold, so a program
/// installed there is found however zfsctl was started. Existing entries and their order are kept.
fn with_extra_path(command: &mut Command, dirs: &[&str]) {
    let mut path = std::env::var("PATH").unwrap_or_default();
    let mut changed = false;
    for dir in dirs {
        if !path.split(':').any(|d| d == *dir) {
            if !path.is_empty() {
                path.push(':');
            }
            path.push_str(dir);
            changed = true;
        }
    }
    if changed {
        command.env("PATH", path);
    }
}

fn local(cmd: &str, args: &[&str]) -> Command {
    let mut command = Command::new(cmd);
    command.args(args);
    with_extra_path(&mut command, PACKAGE_DIRS);
    command
}

impl CommandRunner for LocalRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        let output = local(cmd, args)
            .output()
            .map_err(|source| AppError::Spawn {
                cmd: cmd.to_string(),
                source,
            })?;
        Ok(CommandOutput::from_output(&output))
    }

    fn run_with_stdin(&self, cmd: &str, args: &[&str], stdin: &str) -> Result<CommandOutput> {
        super::run_command_with_stdin(local(cmd, args), cmd, stdin)
    }

    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        self.run(cmd, args)
    }

    fn command(&self, cmd: &str, args: &[&str]) -> Result<Command> {
        Ok(local(cmd, args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_unprivileged_runs_the_command_directly() {
        let output = LocalRunner.run_unprivileged("echo", &["hi"]).unwrap();
        assert_eq!(output.stdout.trim(), "hi");
        assert!(output.success());
    }

    #[test]
    fn run_with_stdin_feeds_the_child_its_stdin() {
        let output = LocalRunner.run_with_stdin("cat", &[], "hello\nworld\n").unwrap();
        assert_eq!(output.stdout, "hello\nworld\n");
        assert!(output.success());
    }

    #[test]
    fn run_with_stdin_does_not_deadlock_on_input_larger_than_a_pipe_buffer() {
        let big = "x".repeat(1_000_000);
        let output = LocalRunner.run_with_stdin("cat", &[], &big).unwrap();
        assert_eq!(output.stdout.len(), big.len());
    }

    #[test]
    fn run_with_stdin_reports_exit_status_when_the_child_ignores_stdin() {
        let output = LocalRunner.run_with_stdin("false", &[], &"y".repeat(1_000_000)).unwrap();
        assert!(!output.success());
    }

    #[test]
    fn runs_a_successful_command_and_captures_stdout() {
        let runner = LocalRunner;
        let output = runner.run("echo", &["hello"]).unwrap();
        assert_eq!(output.stdout.trim(), "hello");
        assert!(output.success());
    }

    #[test]
    fn captures_nonzero_exit_status() {
        let runner = LocalRunner;
        let output = runner.run("false", &[]).unwrap();
        assert!(!output.success());
        assert_ne!(output.status, 0);
    }

    #[test]
    fn spawn_failure_for_missing_binary_is_an_error() {
        let runner = LocalRunner;
        let result = runner.run("definitely-not-a-real-binary-xyz", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn command_is_the_unspawned_command_itself() {
        let cmd = LocalRunner.command("echo", &["hi"]).unwrap();
        assert_eq!(cmd.get_program(), "echo");
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["hi"]);
    }

    #[test]
    fn a_program_in_an_added_directory_is_found_even_when_path_lacks_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("zit-probe-tool");
        std::fs::write(&tool, "#!/bin/sh\necho found\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let extra = dir.path().to_string_lossy().into_owned();
        let mut command = Command::new("zit-probe-tool");
        with_extra_path(&mut command, &[extra.as_str()]);
        let out = command.output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "found");
    }

    #[test]
    fn the_package_directories_are_appended_once_and_never_duplicated() {
        let mut command = Command::new("sh");
        with_extra_path(&mut command, &["/usr/local/bin", "/usr/local/sbin"]);
        command.args(["-c", "echo $PATH"]);
        let path = String::from_utf8_lossy(&command.output().unwrap().stdout).trim().to_string();
        for dir in ["/usr/local/bin", "/usr/local/sbin"] {
            assert_eq!(path.split(':').filter(|d| *d == dir).count(), 1, "{dir} in {path}");
        }
        assert!(std::env::var("PATH").unwrap_or_default().split(':').filter(|d| !d.is_empty()).all(|d| path.split(':').any(|p| p == d)), "the original entries stay: {path}");
    }

    #[test]
    fn local_commands_get_the_package_directories_too() {
        let out = LocalRunner.run("sh", &["-c", "echo $PATH"]).unwrap();
        assert!(out.stdout.split(':').any(|d| d.trim() == "/usr/local/bin"), "{}", out.stdout);
        let cmd = LocalRunner.command("sh", &[]).unwrap();
        assert!(cmd.get_envs().any(|(k, _)| k == "PATH") || std::env::var("PATH").is_ok_and(|p| p.split(':').any(|d| d == "/usr/local/bin")));
    }
}
