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
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status.code().unwrap_or(-1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
