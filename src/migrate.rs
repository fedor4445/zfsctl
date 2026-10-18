//! One-time move from the project's earlier name (zfsctl) to zfsctl: the state directory is moved and the config copied the
//! first time zfsctl starts on a machine that has the old ones and not the new. Only the default locations are touched;
//! a custom `--state-dir` is the operator's own.

use std::path::Path;

pub const LEGACY_STATE_DIR: &str = "/var/db/zfsctl";
pub const LEGACY_CONFIG_PATH: &str = "/usr/local/etc/zfsctl/config.toml";

/// Moves `old` to `new` when only `old` exists (a rename: same filesystem, instant, nothing copied twice). The note says what
/// happened, or why nothing could be done.
pub fn move_state_dir(old: &Path, new: &Path) -> Option<String> {
    if new.exists() || !old.is_dir() {
        return None;
    }
    if let Some(parent) = new.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    Some(match std::fs::rename(old, new) {
        Ok(()) => format!("moved the state directory {} to {}", old.display(), new.display()),
        Err(e) => format!("could not move the state directory {} to {} ({e}); move it by hand, or jobs and history start empty", old.display(), new.display()),
    })
}

/// Copies the old `config.toml` to the new place when only the old one exists. The old file stays where it is.
pub fn copy_config(old: &Path, new: &Path) -> Option<String> {
    if new.exists() || !old.is_file() {
        return None;
    }
    let made = new.parent().is_none_or(|parent| std::fs::create_dir_all(parent).is_ok());
    Some(match made.then(|| std::fs::copy(old, new)) {
        Some(Ok(_)) => format!("copied the config {} to {} (the old file is left in place)", old.display(), new.display()),
        _ => format!("could not copy the config {} to {}; copy it by hand", old.display(), new.display()),
    })
}

/// Both moves at the default locations; every note is handed to `say`.
pub fn from_zfsctl(say: &mut dyn FnMut(&str)) {
    let notes = [
        move_state_dir(Path::new(LEGACY_STATE_DIR), Path::new(crate::daemon::engine::DEFAULT_STATE_DIR)),
        copy_config(Path::new(LEGACY_CONFIG_PATH), Path::new(crate::config::DEFAULT_CONFIG_PATH)),
    ];
    for note in notes.into_iter().flatten() {
        say(&note);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_state_directory_is_moved_once_and_never_over_a_new_one() {
        let root = tempfile::tempdir().unwrap();
        let (old, new) = (root.path().join("zfsctl"), root.path().join("db/zfsctl"));
        assert_eq!(move_state_dir(&old, &new), None, "nothing to move");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("jobs.toml"), "x").unwrap();
        let note = move_state_dir(&old, &new).unwrap();
        assert!(note.starts_with("moved the state directory"), "{note}");
        assert!(!old.exists() && std::fs::read_to_string(new.join("jobs.toml")).unwrap() == "x");
        // A second start, or both directories present: the new one wins and nothing is touched.
        std::fs::create_dir(&old).unwrap();
        assert_eq!(move_state_dir(&old, &new), None);
        assert!(old.is_dir());
    }

    #[test]
    fn the_old_config_is_copied_with_its_mode_and_left_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let (old, new) = (root.path().join("zfsctl/config.toml"), root.path().join("zfsctl/config.toml"));
        assert_eq!(copy_config(&old, &new), None);
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "[[hosts]]\n").unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o600)).unwrap();
        let note = copy_config(&old, &new).unwrap();
        assert!(note.starts_with("copied the config") && note.contains("left in place"), "{note}");
        assert_eq!(std::fs::read_to_string(&new).unwrap(), "[[hosts]]\n");
        assert_eq!(std::fs::metadata(&new).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(old.is_file());
        // The new one is never overwritten.
        std::fs::write(&new, "mine").unwrap();
        assert_eq!(copy_config(&old, &new), None);
        assert_eq!(std::fs::read_to_string(&new).unwrap(), "mine");
    }
}
