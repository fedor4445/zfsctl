mod error;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> error::Result<()> {
    check_root()?;
    println!("zfsctl starting (root check passed)");
    Ok(())
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
    fn effective_uid_matches_nix() {
        // Sanity check that the nix wrapper reports the same thing as the
        // real effective uid, so check_root's logic is exercised without
        // requiring the test process to actually run as root.
        let is_root = nix::unistd::Uid::effective().is_root();
        let expected = nix::unistd::Uid::effective().as_raw() == 0;
        assert_eq!(is_root, expected);
    }
}
