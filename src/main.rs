mod acl;
mod config;
mod daemon;
mod error;
mod exec;
mod hosts;
mod jobs;
mod ui;
mod zfs;

use std::path::PathBuf;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Tui,
    Daemon { state_dir: PathBuf },
}

fn parse_args(args: &[String]) -> error::Result<Command> {
    let usage = || error::AppError::Config("usage: zfsctl [daemon [--state-dir DIR]]".to_string());
    match args {
        [] => Ok(Command::Tui),
        [cmd] if cmd == "daemon" => Ok(Command::Daemon { state_dir: PathBuf::from(daemon::engine::DEFAULT_STATE_DIR) }),
        [cmd, flag, dir] if cmd == "daemon" && flag == "--state-dir" => Ok(Command::Daemon { state_dir: PathBuf::from(dir) }),
        _ => Err(usage()),
    }
}

fn run() -> error::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = parse_args(&args)?;
    check_root()?;
    let config_path = PathBuf::from(config::DEFAULT_CONFIG_PATH);
    if let Command::Daemon { state_dir } = command {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
            signal_hook::flag::register(signal, std::sync::Arc::clone(&stop)).map_err(|e| error::AppError::Config(format!("installing signal handlers: {e}")))?;
        }
        return daemon::run(&config_path, daemon::engine::Paths::new(state_dir), &stop);
    }
    let config = config::Config::load(&config_path)?;
    let inventory = hosts::HostInventory::from_config(&config);
    // Resolved before the terminal enters raw mode, so a bad `theme`
    // value prints a plain error and exits instead of garbling the screen.
    let resolved = ui::theme::resolve(config.ui.theme.as_deref(), &ui::theme::ThemeEnv::from_process())?;
    ui::theme::apply_color_policy();
    ui::run_app(inventory, &config_path, resolved, config.ui.auto_refresh_interval())
}

fn check_root() -> error::Result<()> {
    if !nix::unistd::Uid::effective().is_root() {
        return Err(error::AppError::Config(
            "zfsctl must be run as root".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_arguments_start_the_tui_and_daemon_takes_an_optional_state_dir() {
        assert_eq!(parse_args(&[]).unwrap(), Command::Tui);
        assert_eq!(parse_args(&args(&["daemon"])).unwrap(), Command::Daemon { state_dir: PathBuf::from("/var/db/zfsctl") });
        assert_eq!(parse_args(&args(&["daemon", "--state-dir", "/tmp/x"])).unwrap(), Command::Daemon { state_dir: PathBuf::from("/tmp/x") });
    }

    #[test]
    fn anything_else_is_a_usage_error() {
        for bad in [&["bogus"][..], &["daemon", "--state-dir"], &["daemon", "--wat", "x"], &["daemon", "x", "y", "z"]] {
            let err = parse_args(&args(bad)).unwrap_err().to_string();
            assert!(err.contains("usage: zfsctl"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn check_root_matches_effective_uid() {
        let result = check_root();
        if nix::unistd::Uid::effective().is_root() {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(error::AppError::Config(_))));
        }
    }
}
