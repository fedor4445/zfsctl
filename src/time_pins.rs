//! What zfsctl does with dates today, pinned as text so that replacing the date library (chrono -> jiff) cannot change
//! it unseen. Every expected value is a literal string: a saved file's text, a snapshot name, a screen word, an RFC 3339
//! time with its offset. Only the three helpers below name the date library; the swap may change their bodies and
//! return types and nothing else in this file.
#![allow(clippy::unwrap_used)]

// ----- Helpers: the only lines that know the date library. -----

/// An instant, from RFC 3339 text.
fn at(rfc3339: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(rfc3339).unwrap().with_timezone(&chrono::Utc)
}

/// A wall-clock time (`YYYY-MM-DDTHH:MM`) in a named zone; the earlier one when it happens twice; panics in a gap.
fn local(zone: &str, civil: &str) -> chrono::DateTime<chrono_tz::Tz> {
    use chrono::TimeZone;
    let tz: chrono_tz::Tz = zone.parse().unwrap();
    let naive = chrono::NaiveDateTime::parse_from_str(civil, "%Y-%m-%dT%H:%M").unwrap();
    tz.from_local_datetime(&naive).earliest().unwrap()
}

/// A zoned time as RFC 3339 with its offset (`2027-03-15T02:30:00-04:00`).
fn text(t: &chrono::DateTime<chrono_tz::Tz>) -> String {
    t.to_rfc3339()
}

// ----- Saved files -----

/// How a date is given to a reader (left) and what is written back (right). Writing is always UTC with `Z`, no fraction
/// for a whole second, else 3, 6 or 9 digits (chrono's "auto"), trailing zeros kept inside the chosen width.
const SHAPES: [(&str, &str); 7] = [
    ("2026-11-27T01:00:00Z", "2026-11-27T01:00:00Z"),
    ("2026-11-27T01:00:00+00:00", "2026-11-27T01:00:00Z"),
    ("2026-11-27T02:00:00+01:00", "2026-11-27T01:00:00Z"),
    ("2026-11-27T01:00:00.5Z", "2026-11-27T01:00:00.500Z"),
    ("2026-11-27T01:00:00.000123Z", "2026-11-27T01:00:00.000123Z"),
    ("2026-11-27T01:00:00.000000001Z", "2026-11-27T01:00:00.000000001Z"),
    ("2026-11-27T01:23:36.485121070Z", "2026-11-27T01:23:36.485121070Z"),
];

/// Reads `template` (with every `@` replaced by the date given) as `T`, writes it back as compact JSON, and checks that
/// every date in it came back as the pinned shape.
fn round_trips<T: serde::de::DeserializeOwned + serde::Serialize>(what: &str, template: &str) {
    for (given, written) in SHAPES {
        let json = template.replace('@', given);
        let value: T = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{what}: cannot read {json}: {e}"));
        // Compared as JSON values: field order is the struct's and not what is pinned here; the date texts are.
        let back: serde_json::Value = serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
        let want: serde_json::Value = serde_json::from_str(&template.replace('@', written)).unwrap();
        assert_eq!(back, want, "{what}: {given} must be written as {written}");
    }
}

#[test]
fn history_events_keep_their_dates() {
    round_trips::<crate::daemon::history::Event>(
        "history event",
        r#"{"job_id":"j","run_id":"j-1","event":"started","at":"@","scheduled_for":"@","message":"","catch_up":false}"#,
    );
}

#[test]
fn the_heartbeat_keeps_its_dates() {
    round_trips::<crate::daemon::heartbeat::Heartbeat>("heartbeat", r#"{"pid":1,"started_at":"@","last_tick":"@"}"#);
}

#[test]
fn progress_files_keep_their_dates() {
    round_trips::<crate::daemon::progress::Progress>("progress", r#"{"job_id":"j","bytes":1,"total":null,"started_at":"@","updated_at":"@","rate":null}"#);
}

#[test]
fn the_shares_check_keeps_its_date() {
    round_trips::<crate::daemon::shares_check::SharesResult>(
        "shares check",
        r#"{"checked_at":"@","nfs_running":true,"samba_running":false,"nfs_shares":0,"smb_shares":0}"#,
    );
}

#[test]
fn the_smart_check_keeps_its_date() {
    round_trips::<crate::daemon::smart_check::HostResult>("smart check", r#"{"checked_at":"@","disks":[]}"#);
}

#[test]
fn the_storage_check_keeps_its_dates() {
    round_trips::<crate::daemon::storage_check::StorageResult>(
        "storage check",
        r#"{"checked_at":"@","datasets":[],"integrity":{"checksum_off":[],"dedup_pools":["tank"],"pools":[],"dedup_since":{"tank":"@"}}}"#,
    );
}

#[test]
fn restic_checks_keep_their_date() {
    round_trips::<crate::daemon::restic_check::Checked>("restic check", r#"{"at":"@","ok":true}"#);
}

#[test]
fn the_running_check_marker_keeps_its_dates() {
    round_trips::<crate::daemon::checks::Marker>("check marker", r#"{"by":"daemon","since":"@","beat":"@"}"#);
}

#[test]
fn health_events_keep_their_date() {
    round_trips::<crate::daemon::health_text::HealthEvent>("health event", r#"{"at":"@","host":"fb","kind":"daemon-started","text":"started"}"#);
}

#[test]
fn cloud_sizes_keep_their_date() {
    round_trips::<crate::ui::cloud::Size>("cloud size", r#"{"current":[1,2],"old":null,"at":"@","problem":null}"#);
}

#[test]
fn jobs_toml_writes_created_at_in_the_same_shape() {
    for (given, written) in SHAPES {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.toml");
        let job = crate::jobs::Job { created_at: Some(at(given)), ..crate::jobs::Job::test_snapshot("j", "tank/a", "auto", 3) };
        crate::jobs::save(&path, &[job]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(&format!("created_at = \"{written}\"")), "{given}:\n{text}");
        let read_back = crate::jobs::load(&path).unwrap();
        assert_eq!(read_back[0].created_at, Some(at(written)), "{given}");
    }
}

#[test]
fn jobs_toml_reads_created_at_written_by_hand_with_an_offset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.toml");
    crate::jobs::save(&path, &[crate::jobs::Job::test_snapshot("j", "tank/a", "auto", 3)]).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let edited = if text.contains("created_at") { text } else { text.replace("enabled = true", "enabled = true\ncreated_at = \"2026-11-27T02:00:00+01:00\"") };
    std::fs::write(&path, edited).unwrap();
    let jobs = crate::jobs::load(&path).unwrap();
    assert_eq!(jobs[0].created_at, Some(at("2026-11-27T01:00:00Z")));
}

// ----- Text made from dates -----

/// Snapshot names: the prefix, then the UTC time to the second (the fraction is dropped, not rounded).
#[test]
fn snapshot_names_are_utc_to_the_second() {
    assert_eq!(crate::daemon::snapshot::snapshot_name("auto", at("2026-11-27T01:00:00.9Z")), "auto-20261127T010000Z");
    assert_eq!(crate::daemon::snapshot::snapshot_name("auto", at("2026-11-27T02:00:00+01:00")), "auto-20261127T010000Z");
}

/// The `old/` folder of an rclone run: `YYYYMMDDTHHMMSSZ` in UTC, fraction dropped.
#[test]
fn rclone_run_stamps_are_utc_to_the_second() {
    assert_eq!(crate::rclone::run_stamp(at("2026-11-27T01:02:03.9Z")), "20261127T010203Z");
}

/// "How long ago" words: whole minutes, hours and days, rounded down; a time in the future reads as now.
#[test]
fn ages_read_the_same() {
    let now = at("2026-11-27T12:00:00Z");
    let cases: [(&str, &str, &str); 11] = [
        ("2026-11-27T12:00:00Z", "0m ago", "just now"),
        ("2026-11-27T11:59:01Z", "0m ago", "just now"),
        ("2026-11-27T11:58:59Z", "1m ago", "1m ago"),
        ("2026-11-27T11:01:00Z", "59m ago", "59m ago"),
        ("2026-11-27T10:59:00Z", "1h 1m ago", "1h ago"),
        ("2026-11-26T13:00:00Z", "23h ago", "23h ago"),
        ("2026-11-26T11:00:00Z", "1d 1h ago", "1d ago"),
        ("2026-11-21T12:00:00Z", "6d ago", "6d ago"),
        ("2026-11-19T12:00:00Z", "8d ago", "8d ago"),
        ("2025-10-23T12:00:00Z", "400d ago", "400d ago"),
        ("2026-11-27T12:00:30Z", "0m ago", "just now"),
    ];
    for (then, health, jobs) in cases {
        assert_eq!(crate::daemon::health_text::ago(now, at(then)), health, "{then}");
        assert_eq!(crate::ui::jobs::age_text(now, at(then)), jobs, "{then}");
    }
}
