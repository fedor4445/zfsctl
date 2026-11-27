//! Dates and times: the one place that decides how a date is saved, and a few helpers.
//!
//! Every date zfsctl saves (jobs.toml, the history, the daemon's state files) goes through `rfc3339`, which writes the
//! exact text chrono wrote before the move to jiff: UTC, `Z`, and a fraction of 0, 3, 6 or 9 digits. jiff's own text
//! trims trailing zeros (`.5Z` where chrono wrote `.500Z`); keeping chrono's shape means a file written by this build
//! reads the same in an older one, so a rollback is always safe.

use jiff::{SignedDuration, Timestamp};

/// `t` as chrono wrote it: `2026-11-27T01:00:00Z`, `….500Z`, `….000123Z`, `….485121070Z`.
pub fn format_rfc3339(t: Timestamp) -> String {
    let nanos = t.subsec_nanosecond();
    let whole = t.strftime("%Y-%m-%dT%H:%M:%S");
    if nanos == 0 {
        format!("{whole}Z")
    } else if nanos % 1_000_000 == 0 {
        format!("{whole}.{:03}Z", nanos / 1_000_000)
    } else if nanos % 1_000 == 0 {
        format!("{whole}.{:06}Z", nanos / 1_000)
    } else {
        format!("{whole}.{nanos:09}Z")
    }
}

/// `#[serde(with = "crate::time::rfc3339")]` for a `Timestamp` field; `::option` and `::map` for the other shapes.
pub mod rfc3339 {
    use jiff::Timestamp;

    pub fn serialize<S: serde::Serializer>(t: &Timestamp, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::format_rfc3339(*t))
    }

    /// Reads any RFC 3339 time with an offset (`Z`, `+00:00`, `+01:00`) and any fraction, as chrono did.
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Timestamp, D::Error> {
        let text = <String as serde::Deserialize>::deserialize(d)?;
        text.parse::<Timestamp>().map_err(serde::de::Error::custom)
    }

    /// An optional date.
    pub mod option {
        use jiff::Timestamp;

        #[allow(clippy::ref_option)]
        pub fn serialize<S: serde::Serializer>(t: &Option<Timestamp>, s: S) -> Result<S::Ok, S::Error> {
            match t {
                Some(t) => s.serialize_some(&super::super::format_rfc3339(*t)),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Timestamp>, D::Error> {
            let text = <Option<String> as serde::Deserialize>::deserialize(d)?;
            text.map(|t| t.parse::<Timestamp>().map_err(serde::de::Error::custom)).transpose()
        }
    }

    /// A map of names to dates.
    pub mod map {
        use jiff::Timestamp;
        use std::collections::BTreeMap;

        pub fn serialize<S: serde::Serializer>(m: &BTreeMap<String, Timestamp>, s: S) -> Result<S::Ok, S::Error> {
            let texts: BTreeMap<&String, String> = m.iter().map(|(k, t)| (k, super::super::format_rfc3339(*t))).collect();
            serde::Serialize::serialize(&texts, s)
        }

        pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, Timestamp>, D::Error> {
            let texts = <BTreeMap<String, String> as serde::Deserialize>::deserialize(d)?;
            texts.into_iter().map(|(k, t)| t.parse::<Timestamp>().map(|t| (k, t)).map_err(serde::de::Error::custom)).collect()
        }
    }
}

pub fn seconds(n: i64) -> SignedDuration {
    SignedDuration::from_secs(n)
}

pub fn minutes(n: i64) -> SignedDuration {
    SignedDuration::from_mins(n)
}

pub fn hours(n: i64) -> SignedDuration {
    SignedDuration::from_hours(n)
}

pub fn days(n: i64) -> SignedDuration {
    SignedDuration::from_hours(n * 24)
}

#[cfg(test)]
mod tests;
