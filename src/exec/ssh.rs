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
    /// `ssh -o BatchMode=yes -o ConnectTimeout=10 -- <host> "sudo -n <remote command>"`,
    /// where the remote command is `cmd` and `args` shell-quoted and
    /// joined into one string. `-o BatchMode=yes -o ConnectTimeout=10`
    /// prevent hanging on a password prompt or an unreachable host; the
    /// leading `--` stops `ssh` from treating a `host` value that starts
    /// with `-` as an option flag (argument injection / flag smuggling).
    ///
    /// `sudo -n` (non-interactive) is the same idea applied one layer
    /// further in: zfsctl connects as an unprivileged user and expects
    /// a narrow, command-scoped `NOPASSWD` sudoers entry on the remote
    /// host (see README) for exactly the commands listed in the
    /// README's sudoers section
    /// -- `-n` means a missing or misconfigured entry fails immediately
    /// with a clear error instead of `sudo` hanging on a password
    /// prompt that can never arrive over a non-interactive SSH command
    /// execution. Bare command names (not full paths) are intentional:
    /// `sudo`'s own `secure_path` (FreeBSD's default) resolves them
    /// from a fixed, root-controlled `PATH` regardless of the
    /// connecting user's own `$PATH`, so the usual bare-name/`PATH`
    /// substitution risk doesn't apply here the way it would running
    /// these commands directly, unprivileged.
    fn build_args(&self, cmd: &str, args: &[&str]) -> Vec<String> {
        self.build_args_with(cmd, args, true)
    }

    /// Same as `build_args` but without the `sudo -n` prefix; see
    /// `CommandRunner::run_unprivileged`.
    fn build_args_unprivileged(&self, cmd: &str, args: &[&str]) -> Vec<String> {
        self.build_args_with(cmd, args, false)
    }

    fn build_args_with(&self, cmd: &str, args: &[&str], sudo: bool) -> Vec<String> {
        let mut remote = if sudo { "sudo -n ".to_string() } else { String::new() };
        remote.push_str(&shell_quote(cmd));
        for arg in args {
            remote.push(' ');
            remote.push_str(&shell_quote(arg));
        }
        vec![
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
            "--".to_string(),
            self.host.clone(),
            remote,
        ]
    }

    fn run_ssh(ssh_args: &[String]) -> Result<CommandOutput> {
        let output = Command::new("ssh")
            .args(ssh_args)
            .output()
            .map_err(|source| AppError::Spawn {
                cmd: "ssh".to_string(),
                source,
            })?;
        Ok(CommandOutput::from_output(&output))
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
        super::privilege::require_registered(cmd)?;
        Self::run_ssh(&self.build_args(cmd, args))
    }

    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        super::privilege::require_registered(cmd)?;
        Self::run_ssh(&self.build_args_unprivileged(cmd, args))
    }

    fn run_with_stdin(&self, cmd: &str, args: &[&str], stdin: &str) -> Result<CommandOutput> {
        super::privilege::require_registered(cmd)?;
        let mut command = Command::new("ssh");
        command.args(self.build_args(cmd, args));
        super::run_command_with_stdin(command, "ssh", stdin)
    }

    fn command(&self, cmd: &str, args: &[&str]) -> Result<Command> {
        super::privilege::require_registered(cmd)?;
        let mut command = Command::new("ssh");
        command.args(self.build_args(cmd, args));
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unprivileged_args_have_the_same_options_and_quoting_but_no_sudo() {
        let runner = SshRunner::new("backup-host");
        let privileged = runner.build_args("getent", &["passwd"]);
        let unprivileged = runner.build_args_unprivileged("getent", &["passwd"]);
        assert_eq!(privileged.last().unwrap(), "sudo -n getent passwd");
        assert_eq!(unprivileged.last().unwrap(), "getent passwd");
        assert_eq!(privileged[..privileged.len() - 1], unprivileged[..unprivileged.len() - 1], "same ssh options, `--`, and host");
    }

    #[test]
    fn unprivileged_args_still_quote_unsafe_arguments() {
        let runner = SshRunner::new("h");
        let args = runner.build_args_unprivileged("echo", &["a b; rm -rf /"]);
        assert_eq!(args.last().unwrap(), "echo 'a b; rm -rf /'");
    }

    #[test]
    fn builds_simple_command_unquoted() {
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("zpool", &["list", "-H", "-p"]);
        assert_eq!(
            args,
            vec![
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "-o".to_string(),
                "ConnectTimeout=10".to_string(),
                "--".to_string(),
                "backup-host".to_string(),
                "sudo -n zpool list -H -p".to_string()
            ]
        );
    }

    #[test]
    fn quotes_args_containing_spaces() {
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("echo", &["hello world"]);
        assert_eq!(
            args,
            vec![
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "-o".to_string(),
                "ConnectTimeout=10".to_string(),
                "--".to_string(),
                "backup-host".to_string(),
                "sudo -n echo 'hello world'".to_string()
            ]
        );
    }

    #[test]
    fn quotes_and_escapes_args_containing_single_quotes() {
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("echo", &["it's here"]);
        assert_eq!(
            args,
            vec![
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "-o".to_string(),
                "ConnectTimeout=10".to_string(),
                "--".to_string(),
                "backup-host".to_string(),
                r"sudo -n echo 'it'\''s here'".to_string()
            ]
        );
    }

    #[test]
    fn zfs_dataset_names_pass_through_unquoted() {
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("zfs", &["snapshot", "tank/data@2026-09-04"]);
        assert_eq!(
            args,
            vec![
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "-o".to_string(),
                "ConnectTimeout=10".to_string(),
                "--".to_string(),
                "backup-host".to_string(),
                "sudo -n zfs snapshot tank/data@2026-09-04".to_string()
            ]
        );
    }

    #[test]
    fn always_prefixes_the_remote_command_with_non_interactive_sudo() {
        // -n (non-interactive) is to `sudo` what `BatchMode=yes` is to
        // `ssh` itself: if the remote sudoers NOPASSWD entry is missing
        // or wrong, `sudo` fails immediately with a clear error instead
        // of hanging on a password prompt that can never arrive over a
        // non-interactive SSH command execution.
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("getfacl", &["-v", "/tank/data"]);
        assert_eq!(args[6], "sudo -n getfacl -v /tank/data");
    }

    #[test]
    fn guards_against_host_values_that_look_like_flags() {
        let runner = SshRunner::new("-oProxyCommand=touch /tmp/pwned");
        let args = runner.build_args("zpool", &["list"]);
        assert_eq!(args[4], "--", "a leading '--' must precede the host so ssh cannot parse it as an option");
    }

    #[test]
    fn command_is_ssh_with_the_same_arguments_and_quoting_as_run() {
        let runner = SshRunner::new("backup-host");
        let cmd = runner.command("zfs", &["send", "--", "tank/a b@s"]).unwrap();
        assert_eq!(cmd.get_program(), "ssh");
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, runner.build_args("zfs", &["send", "--", "tank/a b@s"]));
        assert!(args.last().unwrap().starts_with("sudo -n zfs send"));
    }

    #[test]
    fn an_unregistered_command_is_refused_before_anything_is_sent() {
        let runner = SshRunner::new("host");
        for result in [
            runner.run("rm", &["-rf", "/"]),
            runner.run_unprivileged("rm", &[]),
            runner.run_with_stdin("rm", &[], "x"),
        ] {
            let err = result.unwrap_err().to_string();
            assert!(err.contains("rm is not a registered remote command"), "{err}");
        }
        assert!(runner.command("rm", &[]).is_err());
    }

}
