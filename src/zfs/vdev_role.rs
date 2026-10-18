//! What a disk added to a pool is for: data, a log (SLOG), a cache (L2ARC), a special or dedup vdev, or a spare. Builds the
//! `zpool add` words for a selection of disks and refuses the layouts that lose a pool (a lone special or dedup disk) before
//! anything is sent. Pure; the forms call it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Data,
    Log,
    Cache,
    Special,
    Dedup,
    Spare,
}

impl Role {
    pub const ALL: [Role; 6] = [Role::Data, Role::Log, Role::Cache, Role::Special, Role::Dedup, Role::Spare];

    pub fn word(self) -> &'static str {
        match self {
            Role::Data => "data",
            Role::Log => "log",
            Role::Cache => "cache",
            Role::Special => "special",
            Role::Dedup => "dedup",
            Role::Spare => "spare",
        }
    }

    pub fn parse(text: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.word() == text.trim().to_lowercase())
    }

    /// Log, cache and spare devices can be taken out again with `zpool remove`.
    pub fn removable(self) -> bool {
        matches!(self, Role::Log | Role::Cache | Role::Spare)
    }

    /// Data, special and dedup vdevs usually cannot be removed again: the two-key confirm.
    pub fn irreversible(self) -> bool {
        !self.removable()
    }

    /// How much the role matters when a spec mixes several: the confirm follows the riskiest.
    fn risk(self) -> u8 {
        match self {
            Role::Special | Role::Dedup => 4,
            Role::Data => 3,
            Role::Log => 2,
            Role::Spare => 1,
            Role::Cache => 0,
        }
    }
}

const LOSES_THE_POOL: &str = "it holds data the pool cannot do without: losing it loses the whole pool, so it needs at least two disks (a mirror)";

/// The `zpool add` words for `disks` in `role`; `redundancy` is blank, `mirror`, `raidz1`, `raidz2` or `raidz3`.
pub fn build_spec(role: Role, redundancy: &str, disks: &[String]) -> Result<Vec<String>, String> {
    let redundancy = redundancy.trim().to_lowercase();
    if disks.is_empty() {
        return Err("select at least one disk".to_string());
    }
    let words = |head: Option<&str>, group: Option<&str>| -> Vec<String> { head.into_iter().chain(group).map(str::to_string).chain(disks.iter().cloned()).collect() };
    let need = |group: &str| -> Result<(), String> {
        let least = match group {
            "mirror" | "raidz1" => 2,
            "raidz2" => 3,
            "raidz3" => 4,
            other => return Err(format!("'{other}' is not mirror, raidz1, raidz2 or raidz3")),
        };
        if disks.len() < least { Err(format!("a {group} needs at least {least} disks, {} selected", disks.len())) } else { Ok(()) }
    };
    match role {
        Role::Data => Ok(if redundancy.is_empty() { disks.to_vec() } else { words(None, Some(&redundancy)) }),
        Role::Log => match redundancy.as_str() {
            "" => Ok(words(Some("log"), None)),
            "mirror" => need("mirror").map(|()| words(Some("log"), Some("mirror"))),
            other => Err(format!("a log device can be a single disk or a mirror, not {other}")),
        },
        Role::Cache | Role::Spare => {
            if redundancy.is_empty() {
                Ok(words(Some(role.word()), None))
            } else {
                Err(format!("{} devices are not mirrored: each selected disk becomes its own {} device", role.word(), role.word()))
            }
        }
        Role::Special | Role::Dedup => {
            if disks.len() < 2 {
                return Err(format!("a {} vdev with one disk is refused: {LOSES_THE_POOL}", role.word()));
            }
            let group = if redundancy.is_empty() { "mirror" } else { redundancy.as_str() };
            need(group)?;
            Ok(words(Some(role.word()), Some(group)))
        }
    }
}

/// Checks a raw `zpool add` spec the same way (`special mirror da0 da1`, `log da4`, `-f mirror da2 da3`) and returns the
/// riskiest role in it, for the confirm.
pub fn check_raw_spec(spec: &[String]) -> Result<Role, String> {
    let mut role = Role::Data;
    let mut group: Option<(&str, usize)> = None; // (kind, disks so far)
    let mut seen: Vec<Role> = Vec::new();
    let mut skip_value = false;
    // A finished group: a mirror or raidz of one disk is not what was meant, and cache/spare are never grouped.
    let flush = |role: Role, group: &mut Option<(&str, usize)>| -> Result<(), String> {
        let Some((kind, n)) = group.take() else { return Ok(()) };
        if matches!(role, Role::Cache | Role::Spare) {
            return Err(format!("{} devices are not mirrored: list the disks without {kind}", role.word()));
        }
        if role == Role::Log && kind != "mirror" {
            return Err(format!("a log device can be a single disk or a mirror, not {kind}"));
        }
        let least = match kind {
            "mirror" | "raidz1" => 2,
            "raidz2" => 3,
            "raidz3" => 4,
            _ => 1,
        };
        if n < least && role != Role::Data {
            return Err(format!("a {kind} needs at least {least} disks, {n} given"));
        }
        Ok(())
    };
    for token in spec {
        if skip_value {
            skip_value = false;
            continue;
        }
        if token == "-o" || token == "-O" {
            skip_value = true;
            continue;
        }
        if token.starts_with('-') {
            continue;
        }
        if let Some(next) = Role::parse(token).filter(|r| *r != Role::Data) {
            flush(role, &mut group)?;
            role = next;
            continue;
        }
        let kind = ["mirror", "raidz1", "raidz2", "raidz3", "raidz"].into_iter().find(|k| *k == token);
        if let Some(kind) = kind {
            flush(role, &mut group)?;
            group = Some((kind, 0));
            continue;
        }
        // a disk
        if !seen.contains(&role) {
            seen.push(role);
        }
        match group.as_mut() {
            Some((_, n)) => *n += 1,
            None if matches!(role, Role::Special | Role::Dedup) => return Err(format!("a {} vdev with one disk is refused: {LOSES_THE_POOL}", role.word())),
            None => {}
        }
    }
    flush(role, &mut group)?;
    seen.into_iter().max_by_key(|r| r.risk()).ok_or_else(|| "the vdev spec is empty".to_string())
}

/// `special_small_blocks`: `0` (none) or a power of two from 512 to 16M, as `32K`, `1M` or plain bytes.
pub fn parse_small_blocks(text: &str) -> Result<u64, String> {
    let bad = || format!("'{}' is not a block size: use 0 or a power of two from 512 to 16M, such as 32K", text.trim());
    let bytes = match super::quota::parse_size(text) {
        Ok(Some(n)) => n,
        _ => return Err(bad()),
    };
    if bytes == 0 || ((512..=(16 << 20)).contains(&bytes) && bytes.is_power_of_two()) { Ok(bytes) } else { Err(bad()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    #[test]
    fn build_spec_for_each_role() {
        assert_eq!(build_spec(Role::Log, "mirror", &d(&["a", "b"])).unwrap(), d(&["log", "mirror", "a", "b"]));
        assert_eq!(build_spec(Role::Log, "", &d(&["a"])).unwrap(), d(&["log", "a"]));
        assert_eq!(build_spec(Role::Cache, "", &d(&["a", "b"])).unwrap(), d(&["cache", "a", "b"]));
        assert_eq!(build_spec(Role::Spare, "", &d(&["a"])).unwrap(), d(&["spare", "a"]));
        assert_eq!(build_spec(Role::Special, "", &d(&["a", "b"])).unwrap(), d(&["special", "mirror", "a", "b"]));
        assert_eq!(build_spec(Role::Dedup, "raidz1", &d(&["a", "b", "c"])).unwrap(), d(&["dedup", "raidz1", "a", "b", "c"]));
        assert_eq!(build_spec(Role::Data, "mirror", &d(&["a", "b"])).unwrap(), d(&["mirror", "a", "b"]));
        assert_eq!(build_spec(Role::Data, "", &d(&["a", "b"])).unwrap(), d(&["a", "b"]), "a stripe, as before");
    }

    #[test]
    fn special_and_dedup_refuse_a_single_disk_with_the_pool_loss_reason() {
        for role in [Role::Special, Role::Dedup] {
            let msg = build_spec(role, "", &d(&["a"])).unwrap_err();
            assert!(msg.contains("whole pool") && msg.contains(role.word()), "{msg}");
        }
        assert!(build_spec(Role::Special, "", &[]).is_err());
    }

    #[test]
    fn mirror_and_raidz_need_enough_disks() {
        assert!(build_spec(Role::Log, "mirror", &d(&["a"])).is_err());
        assert!(build_spec(Role::Special, "raidz2", &d(&["a", "b"])).is_err());
        assert!(build_spec(Role::Special, "raidz2", &d(&["a", "b", "c"])).is_ok());
        assert!(build_spec(Role::Dedup, "raidz3", &d(&["a", "b", "c"])).is_err());
        assert!(build_spec(Role::Special, "stripe", &d(&["a", "b"])).is_err());
    }

    #[test]
    fn cache_and_log_refuse_what_they_cannot_be() {
        assert!(build_spec(Role::Cache, "mirror", &d(&["a", "b"])).unwrap_err().contains("not mirrored"));
        assert!(build_spec(Role::Spare, "raidz1", &d(&["a", "b"])).is_err());
        assert!(build_spec(Role::Log, "raidz1", &d(&["a", "b", "c"])).is_err());
    }

    #[test]
    fn raw_spec_checks() {
        let s = |t: &[&str]| check_raw_spec(&d(t));
        assert!(s(&["special", "da0"]).unwrap_err().contains("whole pool"));
        assert_eq!(s(&["special", "mirror", "da0", "da1"]), Ok(Role::Special));
        assert_eq!(s(&["log", "da0"]), Ok(Role::Log));
        assert_eq!(s(&["cache", "da0", "da1"]), Ok(Role::Cache));
        assert!(s(&["special", "mirror", "da0"]).is_err(), "a mirror of one disk");
        assert_eq!(s(&["-f", "mirror", "da0", "da1"]), Ok(Role::Data));
        assert_eq!(s(&["-o", "ashift=12", "mirror", "da0", "da1"]), Ok(Role::Data), "an option's value is not a disk");
        assert_eq!(s(&["spare", "da9"]), Ok(Role::Spare));
        assert!(s(&[]).is_err());
        assert!(s(&["special", "mirror", "a", "b", "dedup", "c"]).is_err(), "the dedup disk is alone");
        assert!(s(&["cache", "mirror", "a", "b"]).is_err());
        assert!(s(&["log", "raidz1", "a", "b", "c"]).is_err());
        assert_eq!(s(&["/root/f1", "/root/f2"]), Ok(Role::Data), "file-backed vdevs are disks too");
        assert_eq!(s(&["mirror", "a", "b", "log", "c"]), Ok(Role::Data), "the riskiest role decides the confirm");
        assert_eq!(s(&["log", "a", "special", "mirror", "b", "c"]), Ok(Role::Special));
    }

    #[test]
    fn parse_small_blocks_takes_zero_or_a_power_of_two_in_range() {
        for (text, want) in [("0", 0), ("512", 512), ("4K", 4096), ("32k", 32768), ("1M", 1 << 20), ("16M", 16 << 20)] {
            assert_eq!(parse_small_blocks(text), Ok(want), "{text}");
        }
        for bad in ["3K", "100", "32M", "none", "", "256", "-4K", "1.5K"] {
            assert!(parse_small_blocks(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn role_words_round_trip_and_tiers() {
        for role in Role::ALL {
            assert_eq!(Role::parse(role.word()), Some(role));
        }
        assert_eq!(Role::parse(" LOG "), Some(Role::Log));
        assert_eq!(Role::parse("fast"), None);
        assert!(Role::Log.removable() && Role::Cache.removable() && Role::Spare.removable());
        assert!(Role::Data.irreversible() && Role::Special.irreversible() && Role::Dedup.irreversible());
    }
}
