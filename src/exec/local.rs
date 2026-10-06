use super::{CommandOutput, CommandRunner};
use crate::error::{AppError, Result};
use std::process::Command;

pub struct LocalRunner;

impl CommandRunner for LocalRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        let output = Command::new(cmd)
            .args(args)
            .output()
            .map_err(|source| AppError::Spawn {
                cmd: cmd.to_string(),
                source,
            })?;
        Ok(CommandOutput::from_output(&output))
    }

    fn run_with_stdin(&self, cmd: &str, args: &[&str], stdin: &str) -> Result<CommandOutput> {
        let mut command = Command::new(cmd);
        command.args(args);
        super::run_command_with_stdin(command, cmd, stdin)
    }

    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        self.run(cmd, args)
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
}
