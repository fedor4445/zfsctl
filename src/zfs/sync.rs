//! The `sync` property of a dataset: what it may be set to, and how carefully a change is asked for.

use super::integrity::Confirm;

pub const SYNCS: [&str; 3] = ["standard", "always", "disabled"];

/// The `sync` value as ZFS wants it, or why it is refused.
pub fn check_sync(value: &str) -> Result<String, String> {
    let value = value.trim().to_lowercase();
    if !SYNCS.contains(&value.as_str()) {
        return Err(format!("'{}' is not a sync setting: standard, always or disabled", value.trim()));
    }
    Ok(value)
}

/// `disabled` makes `fsync` return before the data is safe, so an acknowledged write can be lost: the typed confirm. The other two are plain.
pub fn sync_confirm(new: &str) -> Confirm {
    if new == "disabled" { Confirm::Typed } else { Confirm::Plain }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_value_is_trimmed_and_lowered_and_only_the_three_are_known() {
        assert_eq!(check_sync(" DISABLED "), Ok("disabled".to_string()));
        assert_eq!(check_sync("always"), Ok("always".to_string()));
        let err = check_sync("off").unwrap_err();
        assert!(err.contains("standard, always or disabled"), "{err}");
        assert!(check_sync("").is_err());
    }

    #[test]
    fn only_disabled_asks_for_the_name() {
        assert_eq!(sync_confirm("disabled"), Confirm::Typed);
        assert_eq!(sync_confirm("always"), Confirm::Plain);
        assert_eq!(sync_confirm("standard"), Confirm::Plain);
    }
}
