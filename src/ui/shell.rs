//! Opening an interactive shell on a host from the TUI.
//!
//! The pure parts (building the command, classifying the exit) are separate
//! from the part that touches the terminal, which lives in the event loop.

use signal_hook::consts::{SIGINT, SIGQUIT};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// How the shell session ended.
#[derive(Debug, PartialEq, Eq)]
pub enum ShellOutcome {
    /// The shell ran and exited. Its own exit status is deliberately not an error.
    Finished,
    /// `ssh` could not connect or authenticate (it exits 255 for that).
    ConnectFailed,
    /// The program could not be started at all.
    SpawnFailed(String),
}

/// The command for a shell on a host: `ssh -t -- <target>` for a remote one
/// (an interactive login as that unprivileged user; no `BatchMode`, so a
/// password or passphrase prompt works; `--` keeps a target that looks like
/// an option from being read as one), or the user's `$SHELL` locally.
/// `shell_env` is the value of `$SHELL`, passed in so this stays pure.
pub fn shell_command(ssh_target: Option<&str>, shell_env: Option<&str>) -> Command {
    match ssh_target {
        Some(target) => {
            let mut c = Command::new("ssh");
            c.args(["-t", "--", target]);
            c
        }
        None => Command::new(shell_env.filter(|s| !s.is_empty()).unwrap_or("/bin/sh")),
    }
}

fn classify(exit_code: Option<i32>, remote: bool) -> ShellOutcome {
    if remote && exit_code == Some(255) {
        ShellOutcome::ConnectFailed
    } else {
        ShellOutcome::Finished
    }
}

/// While a shell runs the terminal is out of raw mode, so Ctrl-C is a real
/// SIGINT sent to the whole foreground process group, zfsctl included. This
/// swallows SIGINT/SIGQUIT only while a [`SignalSuspend`] is alive and keeps
/// the default action (terminate) the rest of the time.
pub struct ShellSignals {
    default_active: Arc<AtomicBool>,
}

impl ShellSignals {
    pub fn install() -> std::io::Result<Self> {
        let default_active = Arc::new(AtomicBool::new(true));
        for signal in [SIGINT, SIGQUIT] {
            signal_hook::flag::register_conditional_default(signal, Arc::clone(&default_active))?;
        }
        Ok(ShellSignals { default_active })
    }

    /// Ignores SIGINT/SIGQUIT until the returned guard is dropped.
    pub fn suspend(&self) -> SignalSuspend<'_> {
        self.default_active.store(false, Ordering::SeqCst);
        SignalSuspend { signals: self }
    }
}

pub struct SignalSuspend<'a> {
    signals: &'a ShellSignals,
}

impl Drop for SignalSuspend<'_> {
    fn drop(&mut self) {
        self.signals.default_active.store(true, Ordering::SeqCst);
    }
}

fn run_command(mut command: Command, remote: bool, signals: &ShellSignals) -> ShellOutcome {
    let _guard = signals.suspend();
    match command.status() {
        Ok(status) => classify(status.code(), remote),
        Err(e) => ShellOutcome::SpawnFailed(e.to_string()),
    }
}

/// Prints a one-line banner and runs the shell with inherited stdio until it
/// exits. The caller must already have left raw mode and the alternate screen.
pub fn run_shell(host_name: &str, ssh_target: Option<&str>, signals: &ShellSignals) -> ShellOutcome {
    println!("zfsctl: shell on {host_name} — exit to return");
    let command = shell_command(ssh_target, std::env::var("SHELL").ok().as_deref());
    run_command(command, ssh_target.is_some(), signals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    fn args(c: &Command) -> Vec<String> {
        c.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_local_shell_is_the_users_shell_or_bin_sh() {
        let c = shell_command(None, Some("/bin/zsh"));
        assert_eq!(c.get_program(), "/bin/zsh");
        assert!(args(&c).is_empty());
        assert_eq!(shell_command(None, None).get_program(), "/bin/sh");
        assert_eq!(shell_command(None, Some("")).get_program(), "/bin/sh", "an empty $SHELL must not become an empty program name");
    }

    #[test]
    fn a_remote_shell_is_an_interactive_unprivileged_ssh_login() {
        let c = shell_command(Some("fbsd@fbsd16"), Some("/bin/zsh"));
        assert_eq!(c.get_program(), "ssh");
        assert_eq!(args(&c), vec!["-t", "--", "fbsd@fbsd16"]);
        assert!(!args(&c).iter().any(|a| a.contains("BatchMode")), "a password or passphrase prompt must be possible");
        assert!(!args(&c).iter().any(|a| a == "sudo"), "never a root shell");
    }

    #[test]
    fn a_target_that_looks_like_an_option_stays_after_the_double_dash() {
        let c = shell_command(Some("-oProxyCommand=evil"), None);
        let a = args(&c);
        let dashdash = a.iter().position(|x| x == "--").expect("-- present");
        let target = a.iter().position(|x| x == "-oProxyCommand=evil").expect("target present");
        assert!(dashdash < target);
    }

    #[test]
    fn only_ssh_uses_255_to_mean_the_connection_failed() {
        assert_eq!(classify(Some(255), true), ShellOutcome::ConnectFailed);
        assert_eq!(classify(Some(255), false), ShellOutcome::Finished, "a local shell may legitimately `exit 255`");
        assert_eq!(classify(Some(1), true), ShellOutcome::Finished, "the last command failing is not an error");
        assert_eq!(classify(None, true), ShellOutcome::Finished, "killed by a signal");
    }

    #[test]
    fn running_a_child_maps_exit_and_spawn_failure() {
        let signals = ShellSignals::install().unwrap();
        let mut ok = Command::new("sh");
        ok.args(["-c", "exit 3"]);
        assert_eq!(run_command(ok, false, &signals), ShellOutcome::Finished);
        let mut gone = Command::new("sh");
        gone.args(["-c", "exit 255"]);
        assert_eq!(run_command(gone, true, &signals), ShellOutcome::ConnectFailed);
        let missing = Command::new("/nonexistent/zfsctl-shell");
        assert!(matches!(run_command(missing, false, &signals), ShellOutcome::SpawnFailed(_)));
    }

    #[test]
    fn sigint_is_swallowed_only_while_suspended_and_the_guard_is_restored() {
        let signals = ShellSignals::install().unwrap();
        assert!(signals.default_active.load(Ordering::SeqCst), "default behavior is on until a shell runs");
        {
            let _guard = signals.suspend();
            assert!(!signals.default_active.load(Ordering::SeqCst));
            // With the default action active this would terminate the test process.
            nix::sys::signal::raise(nix::sys::signal::Signal::SIGINT).unwrap();
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(signals.default_active.load(Ordering::SeqCst), "the default is restored when the guard drops");
    }
}
