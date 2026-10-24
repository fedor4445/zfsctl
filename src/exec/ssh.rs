use super::{CommandOutput, CommandRunner};
use crate::error::{AppError, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};

/// How many commands may share one multiplexed connection at the same time. An ssh server accepts 10 sessions per
/// connection by default (`MaxSessions`); a request past that is refused, so the runner waits instead.
const MUX_SLOTS: usize = 8;

/// Seconds a shared connection stays open after its last command.
const MUX_PERSIST_SECS: u32 = 60;

pub struct SshRunner {
    pub host: String,
    mux: Option<Mux>,
}

/// Connection sharing (`[ssh] multiplex`): the directory for the control sockets and the limit on commands in flight.
struct Mux {
    dir: PathBuf,
    slots: Arc<Slots>,
}

/// A counting semaphore: `acquire` blocks while `MUX_SLOTS` are taken, and the returned guard gives its slot back.
struct Slots {
    taken: Mutex<usize>,
    freed: Condvar,
}

struct SlotGuard(Arc<Slots>);

impl Slots {
    fn acquire(self: &Arc<Self>) -> SlotGuard {
        let mut taken = self.taken.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while *taken >= MUX_SLOTS {
            taken = self.freed.wait(taken).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *taken += 1;
        SlotGuard(Arc::clone(self))
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        *self.0.taken.lock().unwrap_or_else(std::sync::PoisonError::into_inner) -= 1;
        self.0.freed.notify_one();
    }
}

impl SshRunner {
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into(), mux: None }
    }

    /// A runner whose short commands share one ssh connection per host. `dir` holds the control sockets and is
    /// created (owner only) on first use; if that fails the runner just opens a connection per command, as before.
    pub fn with_mux(host: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        Self { host: host.into(), mux: Some(Mux { dir: dir.into(), slots: Arc::new(Slots { taken: Mutex::new(0), freed: Condvar::new() }) }) }
    }

    /// The ssh options that make a command share the connection, or none when sharing is off or the socket directory
    /// cannot be made. `%C` is a hash of the local host, remote host, port and user, so the socket name stays short.
    /// `ServerAlive*` make a master whose host stopped answering (a reboot) give up within about 15 seconds.
    fn mux_options(&self) -> Vec<String> {
        let Some(mux) = &self.mux else { return Vec::new() };
        if !ensure_private_dir(&mux.dir) {
            return Vec::new();
        }
        let path = format!("ControlPath={}/%C", mux.dir.display());
        ["ControlMaster=auto", path.as_str(), &format!("ControlPersist={MUX_PERSIST_SECS}"), "ServerAliveInterval=5", "ServerAliveCountMax=3"]
            .into_iter()
            .flat_map(|o| ["-o".to_string(), o.to_string()])
            .collect()
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
        self.build_args_with(cmd, args, true, true)
    }

    /// For `command()`: a long transfer (`zfs send`, `restic`, `rclone`) gets a connection of its own, so a shared
    /// connection that drops cannot take a running backup or replication with it.
    fn build_args_own_connection(&self, cmd: &str, args: &[&str]) -> Vec<String> {
        self.build_args_with(cmd, args, true, false)
    }

    /// Same as `build_args` but without the `sudo -n` prefix; see
    /// `CommandRunner::run_unprivileged`.
    fn build_args_unprivileged(&self, cmd: &str, args: &[&str]) -> Vec<String> {
        self.build_args_with(cmd, args, false, true)
    }

    fn build_args_with(&self, cmd: &str, args: &[&str], sudo: bool, shared: bool) -> Vec<String> {
        let mut remote = if sudo { "sudo -n ".to_string() } else { String::new() };
        remote.push_str(&shell_quote(cmd));
        for arg in args {
            remote.push(' ');
            remote.push_str(&shell_quote(arg));
        }
        let mut out = vec!["-o".to_string(), "BatchMode=yes".to_string(), "-o".to_string(), "ConnectTimeout=10".to_string()];
        if shared {
            out.extend(self.mux_options());
        }
        out.extend(["--".to_string(), self.host.clone(), remote]);
        out
    }

    /// Runs `ssh`, holding one of the connection's slots while it does when sharing is on.
    fn run_ssh(&self, ssh_args: &[String]) -> Result<CommandOutput> {
        let _slot = self.mux.as_ref().map(|m| m.slots.acquire());
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

/// Makes `dir` (and its parents) if needed and keeps it readable by its owner only: a control socket is a login.
fn ensure_private_dir(dir: &Path) -> bool {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).is_ok() && std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).is_ok()
}

pub(crate) fn shell_quote(s: &str) -> String {
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
        super::privilege::require_registered(cmd, true)?;
        self.run_ssh(&self.build_args(cmd, args))
    }

    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        super::privilege::require_registered(cmd, false)?;
        self.run_ssh(&self.build_args_unprivileged(cmd, args))
    }

    fn run_with_stdin(&self, cmd: &str, args: &[&str], stdin: &str) -> Result<CommandOutput> {
        super::privilege::require_registered(cmd, true)?;
        let _slot = self.mux.as_ref().map(|m| m.slots.acquire());
        let mut command = Command::new("ssh");
        command.args(self.build_args(cmd, args));
        super::run_command_with_stdin(command, "ssh", stdin)
    }

    fn command(&self, cmd: &str, args: &[&str]) -> Result<Command> {
        super::privilege::require_registered(cmd, true)?;
        let mut command = Command::new("ssh");
        command.args(self.build_args_own_connection(cmd, args));
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
        assert_eq!(args, runner.build_args_own_connection("zfs", &["send", "--", "tank/a b@s"]));
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

    #[test]
    fn a_command_registered_for_the_other_way_of_running_is_refused_too() {
        let runner = SshRunner::new("host");
        for result in [runner.run("uname", &["-sr"]), runner.run_with_stdin("uname", &[], "x")] {
            assert!(result.unwrap_err().to_string().contains("registered as unprivileged"));
        }
        assert!(runner.command("uname", &[]).is_err());
        assert!(runner.run_unprivileged("zfs", &["list"]).unwrap_err().to_string().contains("registered as privileged"));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zfsctl-mux-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_shared_connection_adds_the_control_options_before_the_double_dash() {
        let dir = temp_dir("opts");
        let runner = SshRunner::with_mux("backup-host", &dir);
        let args = runner.build_args("zpool", &["list"]);
        let want = [
            "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
            "-o", "ControlMaster=auto", "-o", &format!("ControlPath={}/%C", dir.display()), "-o", "ControlPersist=60",
            "-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=3",
            "--", "backup-host", "sudo -n zpool list",
        ];
        assert_eq!(args, want);
        assert_eq!(runner.build_args_unprivileged("uname", &["-sr"])[..14], want[..14], "the unprivileged way shares it too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_socket_directory_is_made_for_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("perm").join("nested").join("ssh");
        assert!(ensure_private_dir(&dir));
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ensure_private_dir(&dir), "a directory that already exists is tightened, not refused");
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn if_the_socket_directory_cannot_be_made_the_command_is_sent_the_plain_way() {
        let blocker = temp_dir("blocked");
        std::fs::write(&blocker, "a file where the directory should go").unwrap();
        let runner = SshRunner::with_mux("h", blocker.join("ssh"));
        assert_eq!(runner.build_args("zpool", &["list"]), SshRunner::new("h").build_args("zpool", &["list"]));
        let _ = std::fs::remove_file(&blocker);
    }

    #[test]
    fn a_long_transfer_never_rides_the_shared_connection() {
        let dir = temp_dir("own");
        let runner = SshRunner::with_mux("h", &dir);
        let cmd = runner.command("zfs", &["send", "--", "tank/a@s"]).unwrap();
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(!args.iter().any(|a| a.contains("Control")), "{args:?}");
        assert!(!dir.exists(), "and it does not even make the socket directory");
    }

    #[test]
    fn no_more_than_the_slot_limit_run_at_once_and_a_finished_one_lets_the_next_in() {
        let slots = Arc::new(Slots { taken: Mutex::new(0), freed: Condvar::new() });
        let held: Vec<SlotGuard> = (0..MUX_SLOTS).map(|_| slots.acquire()).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = {
            let slots = Arc::clone(&slots);
            std::thread::spawn(move || {
                let _g = slots.acquire();
                tx.send(()).unwrap();
            })
        };
        assert!(rx.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "the ninth waits");
        drop(held);
        assert!(rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok(), "and goes once slots are free");
        waiter.join().unwrap();
        assert_eq!(*slots.taken.lock().unwrap(), 0);
    }
}
