use super::{CommandOutput, CommandRunner};
use crate::error::{AppError, Result};
use std::process::Command;

pub struct SshRunner {
    pub host: String,
}

impl SshRunner {
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into() }
    }

    /// Builds the argument list passed to the local `ssh` binary:
    /// `ssh -- <host> "<remote command>"`, where the remote command is
    /// `cmd` and `args` shell-quoted and joined into one string. The
    /// leading `--` stops `ssh` from treating a `host` value that starts
    /// with `-` as an option flag (argument injection / flag smuggling).
    fn build_args(&self, cmd: &str, args: &[&str]) -> Vec<String> {
        let mut remote = shell_quote(cmd);
        for arg in args {
            remote.push(' ');
            remote.push_str(&shell_quote(arg));
        }
        vec!["--".to_string(), self.host.clone(), remote]
    }
}

fn shell_quote(s: &str) -> String {
    let is_safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.' | '@' | ':' | '=' | ','));
    if is_safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

impl CommandRunner for SshRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        let ssh_args = self.build_args(cmd, args);
        let output = Command::new("ssh")
            .args(&ssh_args)
            .output()
            .map_err(|source| AppError::Spawn {
                cmd: "ssh".to_string(),
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
    fn builds_simple_command_unquoted() {
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("zpool", &["list", "-H", "-p"]);
        assert_eq!(args, vec!["--".to_string(), "backup-host".to_string(), "zpool list -H -p".to_string()]);
    }

    #[test]
    fn quotes_args_containing_spaces() {
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("echo", &["hello world"]);
        assert_eq!(args, vec!["--".to_string(), "backup-host".to_string(), "echo 'hello world'".to_string()]);
    }

    #[test]
    fn quotes_and_escapes_args_containing_single_quotes() {
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("echo", &["it's here"]);
        assert_eq!(
            args,
            vec!["--".to_string(), "backup-host".to_string(), r"echo 'it'\''s here'".to_string()]
        );
    }

    #[test]
    fn zfs_dataset_names_pass_through_unquoted() {
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("zfs", &["snapshot", "tank/data@2026-09-04"]);
        assert_eq!(
            args,
            vec!["--".to_string(), "backup-host".to_string(), "zfs snapshot tank/data@2026-09-04".to_string()]
        );
    }

    #[test]
    fn guards_against_host_values_that_look_like_flags() {
        let runner = SshRunner::new("-oProxyCommand=touch /tmp/pwned");
        let args = runner.build_args("zpool", &["list"]);
        assert_eq!(args[0], "--", "a leading '--' must precede the host so ssh cannot parse it as an option");
    }
}
