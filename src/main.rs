mod restic;
mod acl;
mod config;
mod daemon;
mod error;
mod exec;
mod hosts;
mod jobs;
mod notify_cli;
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
    PrintSudoers { host: String, with: Vec<String>, without: Vec<String> },
    CheckHost { host: String, with: Vec<String>, without: Vec<String> },
    NotifyTest,
    Report,
}

const USAGE: &str = "usage: zfsctl [daemon [--state-dir DIR] | print-sudoers HOST [--with FEATURE] [--without FEATURE] | check-host HOST [--with FEATURE] [--without FEATURE] | notify-test | report]";

/// `HOST [--with F]... [--without F]...` for the two admin commands.
fn parse_host_flags(rest: &[String]) -> Option<(String, Vec<String>, Vec<String>)> {
    let (host, flags) = rest.split_first()?;
    if host.starts_with('-') {
        return None;
    }
    let (mut with, mut without) = (Vec::new(), Vec::new());
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        let value = it.next()?.clone();
        match flag.as_str() {
            "--with" => with.push(value),
            "--without" => without.push(value),
            _ => return None,
        }
    }
    Some((host.clone(), with, without))
}

fn parse_args(args: &[String]) -> error::Result<Command> {
    let usage = || error::AppError::Config(USAGE.to_string());
    match args {
        [] => Ok(Command::Tui),
        [cmd] if cmd == "daemon" => Ok(Command::Daemon { state_dir: PathBuf::from(daemon::engine::DEFAULT_STATE_DIR) }),
        [cmd, flag, dir] if cmd == "daemon" && flag == "--state-dir" => Ok(Command::Daemon { state_dir: PathBuf::from(dir) }),
        [cmd, rest @ ..] if cmd == "print-sudoers" => parse_host_flags(rest).map(|(host, with, without)| Command::PrintSudoers { host, with, without }).ok_or_else(usage),
        [cmd, rest @ ..] if cmd == "check-host" => parse_host_flags(rest).map(|(host, with, without)| Command::CheckHost { host, with, without }).ok_or_else(usage),
        [cmd] if cmd == "notify-test" => Ok(Command::NotifyTest),
        [cmd] if cmd == "report" => Ok(Command::Report),
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
    if matches!(command, Command::PrintSudoers { .. } | Command::CheckHost { .. }) {
        let config = config::Config::load(&config_path)?;
        let code = admin(&command, &config)?;
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
    if matches!(command, Command::NotifyTest | Command::Report) {
        let config = config::Config::load(&config_path)?;
        let code = if command == Command::NotifyTest { notify_cli::notify_test_command(&config)? } else { notify_cli::report_command(&config)? };
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
    let config = config::Config::load(&config_path)?;
    let inventory = hosts::HostInventory::from_config(&config);
    // Resolved before the terminal enters raw mode, so a bad `theme`
    // value prints a plain error and exits instead of garbling the screen.
    let resolved = ui::theme::resolve(config.ui.theme.as_deref(), &ui::theme::ThemeEnv::from_process())?;
    ui::theme::apply_color_policy();
    ui::run_app(inventory, &config_path, resolved, config.ui.auto_refresh_interval())
}

/// `print-sudoers` / `check-host`: read-only, over the host's normal unprivileged ssh path. Returns the exit code.
fn admin(command: &Command, config: &config::Config) -> error::Result<i32> {
    let (host, with, without, check) = match command {
        Command::PrintSudoers { host, with, without } => (host, with, without, false),
        Command::CheckHost { host, with, without } => (host, with, without, true),
        _ => unreachable!("admin() is only called for the two admin commands"),
    };
    let features = exec::privilege::select(with, without)?;
    let inventory = hosts::HostInventory::from_config(config);
    let Some(found) = inventory.get(host) else {
        let names: Vec<&str> = inventory.hosts.iter().map(|h| h.name.as_str()).collect();
        return Err(error::AppError::Config(format!("unknown host '{host}'; configured hosts: {}", names.join(", "))));
    };
    let Some(target) = found.ssh_target.as_deref() else {
        println!("{host} is the hub: zfsctl runs as root here, so there is nothing to configure.");
        return Ok(0);
    };
    let result = if check {
        exec::sudoers::check_host_target(found.runner.as_ref(), host, target, &features).map(|report| (report.render(), report.exit_code()))
    } else {
        let generated = chrono::Utc::now().format("%Y-%m-%d").to_string();
        exec::sudoers::print_sudoers(found.runner.as_ref(), host, target, &features, &generated).map(|text| (text, 0))
    };
    match result {
        Ok((text, code)) => {
            print!("{text}");
            Ok(code)
        }
        Err(failure) => {
            eprintln!("error: {}", failure.message());
            Ok(failure.exit_code())
        }
    }
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
    #[test]
    fn notify_test_and_report_take_no_arguments() {
        assert_eq!(parse_args(&["notify-test".into()]).unwrap(), Command::NotifyTest);
        assert_eq!(parse_args(&["report".into()]).unwrap(), Command::Report);
        assert!(parse_args(&["report".into(), "extra".into()]).is_err());
        assert!(parse_args(&["notify-test".into(), "x".into()]).is_err());
    }

    #[test]
    fn print_sudoers_and_check_host_take_a_host_and_repeatable_flags() {
        assert_eq!(
            parse_args(&args(&["print-sudoers", "fbsd16"])).unwrap(),
            Command::PrintSudoers { host: "fbsd16".to_string(), with: vec![], without: vec![] }
        );
        assert_eq!(
            parse_args(&args(&["check-host", "fbsd16", "--with", "restic", "--without", "samba", "--without", "acl"])).unwrap(),
            Command::CheckHost { host: "fbsd16".to_string(), with: vec!["restic".to_string()], without: vec!["samba".to_string(), "acl".to_string()] }
        );
    }

    #[test]
    fn the_admin_commands_reject_a_missing_host_a_dangling_flag_and_unknown_flags() {
        for bad in [&["print-sudoers"][..], &["check-host"], &["print-sudoers", "h", "--with"], &["print-sudoers", "h", "--wat", "x"], &["print-sudoers", "h", "extra"], &["check-host", "--with", "restic"]] {
            let err = parse_args(&args(bad)).unwrap_err().to_string();
            assert!(err.contains("usage"), "{bad:?}: {err}");
        }
    }

}
