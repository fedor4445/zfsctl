//! Quotas and reservations: the size text an admin types, the refusals before anything is sent to ZFS, which changes need the
//! two-key confirm, and how close a dataset is to its limit. Pure; the form and the Dashboard call it.

/// The four properties the guided form edits.
pub const KEYS: [&str; 4] = ["quota", "refquota", "reservation", "refreservation"];

/// `10G`, `1.5T`, `512m`, `10GB`, `10GiB` (powers of 1024) as bytes; `none` is `Ok(None)` (no limit).
pub fn parse_size(text: &str) -> Result<Option<u64>, String> {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return Err("enter a size such as 10G, or none".to_string());
    }
    if lower == "none" {
        return Ok(None);
    }
    let bad = || format!("'{}' is not a size: use a number with K, M, G, T or P, such as 10G or 1.5T (or none)", text.trim());
    let digits = lower.strip_suffix("ib").or_else(|| lower.strip_suffix('b')).unwrap_or(&lower);
    let (number, shift) = match digits.chars().last() {
        Some('k') => (&digits[..digits.len() - 1], 10),
        Some('m') => (&digits[..digits.len() - 1], 20),
        Some('g') => (&digits[..digits.len() - 1], 30),
        Some('t') => (&digits[..digits.len() - 1], 40),
        Some('p') => (&digits[..digits.len() - 1], 50),
        _ => (digits, 0),
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty() && fraction.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) || !fraction.bytes().all(|b| b.is_ascii_digit()) || fraction.len() > 12 {
        return Err(bad());
    }
    let unit: u128 = 1 << shift;
    let whole: u128 = if whole.is_empty() { 0 } else { whole.parse().map_err(|_| bad())? };
    let fraction_bytes = if fraction.is_empty() { 0 } else { fraction.parse::<u128>().map_err(|_| bad())? * unit / 10u128.pow(u32::try_from(fraction.len()).unwrap_or(0)) };
    let total = whole.checked_mul(unit).and_then(|w| w.checked_add(fraction_bytes)).ok_or_else(bad)?;
    u64::try_from(total).map(Some).map_err(|_| format!("'{}' is larger than a dataset can have", text.trim()))
}

/// Bytes as the shortest exact size text (`10G`), or the plain number when it is not a whole unit.
pub fn format_size(bytes: u64) -> String {
    for (shift, unit) in [(50, 'P'), (40, 'T'), (30, 'G'), (20, 'M'), (10, 'K')] {
        if bytes > 0 && bytes % (1u64 << shift) == 0 {
            return format!("{}{unit}", bytes >> shift);
        }
    }
    bytes.to_string()
}

/// What the checks need to know about the dataset (0 = not set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Facts {
    pub used: u64,
    pub referenced: u64,
    pub available: u64,
    pub quota: u64,
    pub refquota: u64,
    pub reservation: u64,
    pub refreservation: u64,
    pub is_volume: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Tier {
    Direct,
    TwoKey,
}

/// Whether setting `key` to `new` (`None` = remove) may go ahead, and how carefully.
pub fn check(key: &str, new: Option<u64>, facts: &Facts) -> Result<Tier, String> {
    let size = |n: u64| format_size(n);
    if facts.is_volume && key != "refreservation" {
        return Err(format!("{key} is not offered for volumes: a volume has its volsize and refreservation"));
    }
    let Some(new) = new else { return Ok(Tier::Direct) };
    let set = |n: u64| (n > 0).then_some(n);
    let (quota, refquota) = (set(facts.quota), set(facts.refquota));
    match key {
        "quota" => {
            if new < facts.used {
                return Err(format!("the dataset and its snapshots already use {}: a quota of {} is below that", size(facts.used), size(new)));
            }
            if new < facts.reservation || new < facts.refreservation {
                return Err(format!("a quota of {} is below the reservation ({})", size(new), size(facts.reservation.max(facts.refreservation))));
            }
        }
        "refquota" => {
            if new < facts.referenced {
                return Err(format!("the dataset itself already refers to {}: a refquota of {} is below that", size(facts.referenced), size(new)));
            }
            if new < facts.refreservation {
                return Err(format!("a refquota of {} is below the refreservation ({})", size(new), size(facts.refreservation)));
            }
        }
        "reservation" | "refreservation" => {
            if let Some(q) = quota.filter(|q| new > *q) {
                return Err(format!("a {key} of {} is larger than the quota ({})", size(new), size(q)));
            }
            if key == "refreservation" {
                if let Some(r) = refquota.filter(|r| new > *r) {
                    return Err(format!("a refreservation of {} is larger than the refquota ({})", size(new), size(r)));
                }
            }
            let old = if key == "reservation" { facts.reservation } else { facts.refreservation };
            let raise = new.saturating_sub(old);
            if raise > facts.available {
                return Err(format!("the pool cannot back {} more: only {} is available", size(raise), size(facts.available)));
            }
            return Ok(if u128::from(raise) * 10 > u128::from(facts.available) { Tier::TwoKey } else { Tier::Direct });
        }
        other => return Err(format!("{other} is not a quota or reservation property")),
    }
    // Less than 10% headroom over what is used now: writes would soon fail.
    let in_use = if key == "quota" { facts.used } else { facts.referenced };
    Ok(if u128::from(new) * 100 < u128::from(in_use) * 110 { Tier::TwoKey } else { Tier::Direct })
}

/// How close a dataset is to the nearer of its two limits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Use {
    pub percent: f64,
    pub limit: u64,
    /// The limit is `refquota` (measured against `referenced`) and not `quota` (measured against `used`).
    pub is_ref: bool,
}

#[allow(clippy::cast_precision_loss)] // a percentage for display
pub fn usage(f: &Facts) -> Option<Use> {
    let by_quota = (f.quota > 0).then(|| Use { percent: f.used as f64 * 100.0 / f.quota as f64, limit: f.quota, is_ref: false });
    let by_ref = (f.refquota > 0).then(|| Use { percent: f.referenced as f64 * 100.0 / f.refquota as f64, limit: f.refquota, is_ref: true });
    match (by_quota, by_ref) {
        (Some(q), Some(r)) => Some(if r.percent > q.percent { r } else { q }),
        (q, r) => q.or(r),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;

    #[test]
    fn parse_size_accepts_units_decimals_none_and_refuses_junk() {
        assert_eq!(parse_size("10G"), Ok(Some(10 * G)));
        assert_eq!(parse_size("10g"), Ok(Some(10 * G)));
        assert_eq!(parse_size("1.5T"), Ok(Some(1_649_267_441_664)));
        assert_eq!(parse_size("512M"), Ok(Some(512 << 20)));
        assert_eq!(parse_size("10GB"), Ok(Some(10 * G)));
        assert_eq!(parse_size("10GiB"), Ok(Some(10 * G)));
        assert_eq!(parse_size(" 5K "), Ok(Some(5 << 10)));
        assert_eq!(parse_size("4096"), Ok(Some(4096)), "bare numbers are bytes");
        assert_eq!(parse_size("none"), Ok(None));
        assert_eq!(parse_size(" None "), Ok(None));
        for bad in ["", "  ", "-5", "1e3", "G", "1.2.3G", "99999999999P", "10Q", "1,5G", "0x10"] {
            assert!(parse_size(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn format_size_is_exact_or_plain_bytes() {
        assert_eq!(format_size(10 * G), "10G");
        assert_eq!(format_size(1536 << 20), "1536M");
        assert_eq!(format_size(10 * G + 1), "10737418241");
        assert_eq!(parse_size(&format_size(123_456_789)), Ok(Some(123_456_789)));
    }

    fn facts() -> Facts {
        Facts { used: 5 * G, referenced: 3 * G, available: 100 * G, ..Facts::default() }
    }

    #[test]
    fn check_refuses_a_quota_below_use_and_a_refquota_below_referenced() {
        let f = facts();
        assert!(check("quota", Some(4 * G), &f).is_err());
        assert!(check("refquota", Some(2 * G), &f).is_err());
        assert!(check("quota", Some(6 * G), &f).is_ok());
        assert!(check("refquota", Some(4 * G), &f).is_ok(), "snapshots count toward quota, not refquota: 4G > referenced 3G");
        assert!(check("quota", None, &f).is_ok() && check("refquota", None, &f).is_ok(), "removing is always fine");
    }

    #[test]
    fn check_refuses_reservations_above_their_limits_and_limits_below_reservations() {
        let f = Facts { quota: 10 * G, refquota: 8 * G, ..facts() };
        assert!(check("reservation", Some(11 * G), &f).is_err());
        assert!(check("reservation", Some(10 * G), &f).is_ok());
        assert!(check("refreservation", Some(9 * G), &f).is_err(), "above refquota");
        assert!(check("refreservation", Some(8 * G), &f).is_ok());
        let only_quota = Facts { quota: 10 * G, ..facts() };
        assert!(check("refreservation", Some(11 * G), &only_quota).is_err(), "refreservation counts against quota too");
        let reserved = Facts { quota: 20 * G, reservation: 15 * G, ..facts() };
        assert!(check("quota", Some(12 * G), &reserved).is_err(), "a quota below the reservation");
    }

    #[test]
    fn check_refuses_a_reservation_raise_beyond_available_by_the_increase_only() {
        let f = Facts { reservation: 8 * G, available: G, ..facts() };
        assert!(check("reservation", Some(10 * G), &f).is_err(), "raising by 2G with 1G available");
        let roomy = Facts { available: 3 * G, ..f };
        assert!(check("reservation", Some(10 * G), &roomy).is_ok());
        assert!(check("reservation", Some(6 * G), &f).is_ok(), "lowering needs no room");
    }

    #[test]
    fn volumes_take_only_refreservation() {
        let v = Facts { is_volume: true, ..facts() };
        for key in ["quota", "refquota", "reservation"] {
            assert!(check(key, Some(10 * G), &v).unwrap_err().contains("volume"), "{key}");
        }
        assert!(check("refreservation", Some(10 * G), &v).is_ok());
        assert!(check("refreservation", None, &v).is_ok(), "a sparse volume has none, and that is normal");
    }

    #[test]
    fn tier_is_two_key_under_ten_percent_headroom_or_a_big_reservation_raise() {
        let f = Facts { used: 100 * G, referenced: 100 * G, available: 1000 * G, ..Facts::default() };
        assert_eq!(check("quota", Some(110 * G), &f), Ok(Tier::Direct), "exactly 110% of use");
        assert_eq!(check("quota", Some(109 * G), &f), Ok(Tier::TwoKey));
        assert_eq!(check("quota", None, &f), Ok(Tier::Direct));
        assert_eq!(check("refquota", Some(109 * G), &f), Ok(Tier::TwoKey));
        assert_eq!(check("reservation", Some(100 * G), &f), Ok(Tier::Direct), "exactly 10% of available");
        assert_eq!(check("reservation", Some(101 * G), &f), Ok(Tier::TwoKey));
    }

    #[test]
    fn usage_is_the_closer_limit() {
        let near = Facts { used: 96 * G / 10, quota: 10 * G, ..Facts::default() };
        let u = usage(&near).unwrap();
        assert!((u.percent - 96.0).abs() < 0.01 && u.limit == 10 * G && !u.is_ref);
        let refonly = Facts { referenced: 3 * G, refquota: 4 * G, used: 9 * G, ..Facts::default() };
        let u = usage(&refonly).unwrap();
        assert!((u.percent - 75.0).abs() < 0.01 && u.is_ref, "only refquota: judged by referenced, not used");
        let both = Facts { used: 5 * G, quota: 10 * G, referenced: 3 * G, refquota: 4 * G, ..Facts::default() };
        assert!(usage(&both).unwrap().is_ref, "75% beats 50%");
        assert_eq!(usage(&Facts::default()), None);
    }
}
