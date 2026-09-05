mod config;
mod error;
mod exec;
mod hosts;
mod ui;
mod zfs;

use std::path::PathBuf;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> error::Result<()> {
    check_root()?;
    let config_path = PathBuf::from(config::DEFAULT_CONFIG_PATH);
    let config = config::Config::load(&config_path)?;
    let inventory = hosts::HostInventory::from_config(&config);
    ui::run_app(inventory)
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
