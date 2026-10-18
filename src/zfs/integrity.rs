//! The values of the `checksum` and `dedup` properties, which OpenZFS versions know them, and which change needs which
//! confirmation. Pure; the form and the Dashboard call it.

pub const CHECKSUMS: [&str; 9] = ["on", "off", "fletcher2", "fletcher4", "sha256", "sha512", "skein", "edonr", "blake3"];

pub const DEDUPS: [&str; 12] = [
    "off",
    "on",
    "verify",
    "sha256",
    "sha256,verify",
    "sha512",
    "sha512,verify",
    "skein",
    "skein,verify",
    "edonr,verify",
    "blake3",
    "blake3,verify",
];

/// `(major, minor)` of an OpenZFS version string: `2.2.2-0ubuntu9.5`, `2.4.2-1-FreeBSD_g6330a45b0`, `zfs-2.1.14-1`.
pub fn openzfs_version(text: &str) -> Option<(u32, u32)> {
    let text = text.trim();
    let text = text.strip_prefix("zfs-").unwrap_or(text);
    let mut parts = text.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
    // the version must start the string: "garbage 2.2" is not a version
    if !text.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Whether a host running `version` knows `word`. An unknown version gates nothing: the host's own error stands.
fn known_to(word: &str, version: Option<&str>) -> Result<(), String> {
    let needs_2_2 = word == "blake3" || word.starts_with("blake3,");
    let Some(seen) = version.and_then(openzfs_version) else { return Ok(()) };
    if needs_2_2 && seen < (2, 2) {
        return Err(format!("blake3 needs OpenZFS 2.2 or newer; this host runs {}.{}", seen.0, seen.1));
    }
    Ok(())
}

fn normalized(value: &str) -> String {
    value.trim().to_lowercase().split(',').map(str::trim).collect::<Vec<_>>().join(",")
}

/// The `checksum` value as ZFS wants it, or why it is refused.
pub fn check_checksum(value: &str, version: Option<&str>) -> Result<String, String> {
    let value = normalized(value);
    if !CHECKSUMS.contains(&value.as_str()) {
        return Err(format!("'{}' is not a checksum: {}", value.trim(), CHECKSUMS.join(", ")));
    }
    known_to(&value, version)?;
    Ok(value)
}

/// The `dedup` value as ZFS wants it, or why it is refused.
pub fn check_dedup(value: &str, version: Option<&str>) -> Result<String, String> {
    let value = normalized(value);
    if !DEDUPS.contains(&value.as_str()) {
        let hint = if value == "edonr" { " (edonr is only allowed with verify: edonr,verify)" } else { "" };
        return Err(format!("'{}' is not a dedup setting{hint}: {}", value.trim(), DEDUPS.join(", ")));
    }
    known_to(&value, version)?;
    Ok(value)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Confirm {
    /// Type the dataset's name.
    Typed,
    /// `y` then Enter.
    Plain,
}

/// Switching the checksum off removes the integrity check for new writes: the typed confirm. Every other change is plain.
pub fn checksum_confirm(new: &str) -> Confirm {
    if new == "off" { Confirm::Typed } else { Confirm::Plain }
}

/// Turning dedup on where it was off costs RAM and does not undo cleanly: the typed confirm. Everything else is plain.
pub fn dedup_confirm(old: &str, new: &str) -> Confirm {
    if old == "off" && new != "off" { Confirm::Typed } else { Confirm::Plain }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_from_the_real_strings() {
        assert_eq!(openzfs_version("2.2.2-0ubuntu9.5"), Some((2, 2)));
        assert_eq!(openzfs_version("2.4.2-1-FreeBSD_g6330a45b0"), Some((2, 4)));
        assert_eq!(openzfs_version("zfs-2.1.14-1"), Some((2, 1)));
        assert_eq!(openzfs_version(" 0.8.6 "), Some((0, 8)));
        assert_eq!(openzfs_version(""), None);
        assert_eq!(openzfs_version("garbage 2.2"), None);
        assert_eq!(openzfs_version("2"), None);
        assert!(openzfs_version("2.10.0") > openzfs_version("2.2.0"), "10 is above 2, numerically");
    }

    #[test]
    fn checksum_values_are_normalized_and_blake3_needs_2_2() {
        assert_eq!(check_checksum(" SHA256 ", None).unwrap(), "sha256");
        assert!(check_checksum("blake3", Some("2.1.14-1")).unwrap_err().contains("2.2"));
        assert_eq!(check_checksum("blake3", Some("2.2.2")).unwrap(), "blake3");
        assert_eq!(check_checksum("blake3", None).unwrap(), "blake3", "an unknown version gates nothing");
        assert_eq!(check_checksum("blake3", Some("not a version")).unwrap(), "blake3");
        for bad in ["fletcher3", "", "none", "sha3", "noparity"] {
            assert!(check_checksum(bad, None).is_err(), "{bad:?}");
        }
        assert!(check_checksum("sha3", None).unwrap_err().contains("fletcher4"), "the message lists the choices");
    }

    #[test]
    fn dedup_values_normalize_the_comma_and_edonr_needs_verify() {
        assert_eq!(check_dedup("SHA256, Verify", None).unwrap(), "sha256,verify");
        assert_eq!(check_dedup("on", None).unwrap(), "on");
        assert!(check_dedup("edonr", None).unwrap_err().contains("edonr,verify"));
        assert_eq!(check_dedup("edonr,verify", None).unwrap(), "edonr,verify");
        assert!(check_dedup("verify,sha256", None).is_err());
        assert!(check_dedup("fletcher4,verify", None).is_err());
        assert!(check_dedup("blake3,verify", Some("2.0.7")).unwrap_err().contains("2.2"));
    }

    #[test]
    fn which_changes_need_the_typed_confirm() {
        assert_eq!(checksum_confirm("off"), Confirm::Typed);
        assert_eq!(checksum_confirm("sha256"), Confirm::Plain);
        assert_eq!(dedup_confirm("off", "on"), Confirm::Typed);
        assert_eq!(dedup_confirm("off", "sha256,verify"), Confirm::Typed);
        assert_eq!(dedup_confirm("on", "verify"), Confirm::Plain);
        assert_eq!(dedup_confirm("on", "off"), Confirm::Plain);
        assert_eq!(dedup_confirm("off", "off"), Confirm::Plain);
    }
}
