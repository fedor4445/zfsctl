//! Tests of `time.rs`: the saved-date format must be chrono's, byte for byte.
use super::*;

const SHAPES: [(&str, &str); 7] = [
    ("2026-11-27T01:00:00Z", "2026-11-27T01:00:00Z"),
    ("2026-11-27T01:00:00+00:00", "2026-11-27T01:00:00Z"),
    ("2026-11-27T02:00:00+01:00", "2026-11-27T01:00:00Z"),
    ("2026-11-27T01:00:00.5Z", "2026-11-27T01:00:00.500Z"),
    ("2026-11-27T01:00:00.000123Z", "2026-11-27T01:00:00.000123Z"),
    ("2026-11-27T01:00:00.000000001Z", "2026-11-27T01:00:00.000000001Z"),
    ("2026-11-27T01:23:36.485121070Z", "2026-11-27T01:23:36.485121070Z"),
];

#[test]
fn format_rfc3339_writes_chronos_shape() {
    for (given, written) in SHAPES {
        assert_eq!(format_rfc3339(given.parse().unwrap()), written, "{given}");
    }
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
struct Saved {
    #[serde(with = "crate::time::rfc3339")]
    at: jiff::Timestamp,
    #[serde(default, with = "crate::time::rfc3339::option", skip_serializing_if = "Option::is_none")]
    maybe: Option<jiff::Timestamp>,
    #[serde(default, with = "crate::time::rfc3339::map")]
    since: std::collections::BTreeMap<String, jiff::Timestamp>,
}

#[test]
fn the_serde_module_reads_any_shape_and_writes_chronos() {
    for (given, written) in SHAPES {
        let json = format!(r#"{{"at":"{given}","maybe":"{given}","since":{{"tank":"{given}"}}}}"#);
        let saved: Saved = serde_json::from_str(&json).unwrap();
        assert_eq!(serde_json::to_string(&saved).unwrap(), format!(r#"{{"at":"{written}","maybe":"{written}","since":{{"tank":"{written}"}}}}"#), "{given}");
        let toml_text = toml::to_string(&saved).unwrap();
        assert!(toml_text.contains(&format!("at = \"{written}\"")), "{toml_text}");
        assert_eq!(toml::from_str::<Saved>(&toml_text).unwrap(), saved);
    }
}

#[test]
fn an_absent_option_stays_absent() {
    let saved: Saved = serde_json::from_str(r#"{"at":"2026-11-27T01:00:00Z"}"#).unwrap();
    assert_eq!(saved.maybe, None);
    assert_eq!(serde_json::to_string(&saved).unwrap(), r#"{"at":"2026-11-27T01:00:00Z","since":{}}"#);
}

#[test]
fn garbage_is_an_error_not_a_default() {
    assert!(serde_json::from_str::<Saved>(r#"{"at":"yesterday"}"#).is_err());
}

#[test]
fn the_duration_helpers_are_exact() {
    assert_eq!(minutes(2).as_secs(), 120);
    assert_eq!(hours(1).as_secs(), 3600);
    assert_eq!(days(1).as_secs(), 86_400);
    assert_eq!(seconds(-30).as_secs(), -30);
}
