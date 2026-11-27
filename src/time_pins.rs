//! What zfsctl does with dates today, pinned as text so that replacing the date library (chrono -> jiff) cannot change
//! it unseen. Every expected value is a literal string: a saved file's text, a snapshot name, a screen word, an RFC 3339
//! time with its offset. Only the helpers below name the date library; the swap may change their bodies and return
//! types and nothing else in this file.
#![allow(clippy::unwrap_used)]

// ----- Helpers: the only lines that know the date library. -----

/// An instant, from RFC 3339 text.
fn at(rfc3339: &str) -> jiff::Timestamp {
    rfc3339.parse().unwrap()
}

/// A wall-clock time (`YYYY-MM-DDTHH:MM`) in a named zone; the earlier one when it happens twice; panics in a gap.
fn local(zone: &str, civil: &str) -> jiff::ZonedDateTime {
    let tz = jiff::tz::TimeZone::get(zone).unwrap();
    let ambiguous = tz.to_ambiguous_zoned_datetime(jiff::civil::DateTime::strptime("%Y-%m-%dT%H:%M", civil).unwrap());
    assert!(!matches!(ambiguous.offset(), jiff::tz::AmbiguousOffset::Gap { .. }), "{civil} does not exist in {zone}");
    ambiguous.earlier().unwrap()
}

/// The same wall-clock time as `local`, the later one when it happens twice.
fn later(zone: &str, civil: &str) -> jiff::ZonedDateTime {
    let tz = jiff::tz::TimeZone::get(zone).unwrap();
    tz.to_ambiguous_zoned_datetime(jiff::civil::DateTime::strptime("%Y-%m-%dT%H:%M", civil).unwrap()).later().unwrap()
}

/// A length of time in whole seconds.
fn secs(d: &jiff::SignedDuration) -> i64 {
    d.as_secs()
}

/// A zoned time as RFC 3339 with its offset (`2027-03-15T02:30:00-04:00`).
fn text(t: &jiff::ZonedDateTime) -> String {
    t.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
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

// ----- Schedules across daylight saving -----
// The rules (daemon/cron.rs): a time in the spring gap is skipped; a time that happens twice in autumn fires once, at its
// first occurrence; "latest at or before" during the second pass gives the later occurrence. Two zones, because the US
// and Europe change on different dates.

const NY: &str = "America/New_York";
const BERLIN: &str = "Europe/Berlin";

fn next(expr: &str, zone: &str, civil: &str) -> String {
    text(&crate::daemon::cron::Schedule::parse(expr).unwrap().next_after(&local(zone, civil)).unwrap())
}

/// The next `count` fire times of `expr` after `civil` in `zone`, each after the one before.
fn next_n(expr: &str, zone: &str, civil: &str, count: usize) -> Vec<String> {
    let schedule = crate::daemon::cron::Schedule::parse(expr).unwrap();
    let mut t = local(zone, civil);
    (0..count)
        .map(|_| {
            t = schedule.next_after(&t).unwrap();
            text(&t)
        })
        .collect()
}

#[test]
fn a_time_in_the_spring_gap_is_skipped() {
    assert_eq!(next("30 2 * * *", NY, "2027-03-13T03:00"), "2027-03-15T02:30:00-04:00");
    assert_eq!(next("30 2 * * *", BERLIN, "2027-03-27T03:00"), "2027-03-29T02:30:00+02:00");
}

#[test]
fn a_time_that_happens_twice_fires_once_at_its_first_occurrence() {
    assert_eq!(next_n("30 1 * * *", NY, "2027-11-06T02:00", 2), ["2027-11-07T01:30:00-04:00", "2027-11-08T01:30:00-05:00"]);
    assert_eq!(next_n("30 2 * * *", BERLIN, "2027-10-30T03:00", 2), ["2027-10-31T02:30:00+02:00", "2027-11-01T02:30:00+01:00"]);
}

#[test]
fn hourly_jobs_across_both_changes() {
    assert_eq!(next_n("0 * * * *", NY, "2027-03-14T00:30", 4), ["2027-03-14T01:00:00-05:00", "2027-03-14T03:00:00-04:00", "2027-03-14T04:00:00-04:00", "2027-03-14T05:00:00-04:00"]);
    assert_eq!(next_n("0 * * * *", NY, "2027-11-07T00:30", 4), ["2027-11-07T01:00:00-04:00", "2027-11-07T02:00:00-05:00", "2027-11-07T03:00:00-05:00", "2027-11-07T04:00:00-05:00"]);
    assert_eq!(next_n("0 * * * *", BERLIN, "2027-03-28T00:30", 4), ["2027-03-28T01:00:00+01:00", "2027-03-28T03:00:00+02:00", "2027-03-28T04:00:00+02:00", "2027-03-28T05:00:00+02:00"]);
    assert_eq!(next_n("0 * * * *", BERLIN, "2027-10-31T00:30", 4), ["2027-10-31T01:00:00+02:00", "2027-10-31T02:00:00+02:00", "2027-10-31T03:00:00+01:00", "2027-10-31T04:00:00+01:00"]);
}

#[test]
fn the_latest_time_during_the_second_pass_is_the_later_occurrence() {
    // 01:45 during the second pass (EST): its earlier reading would be 01:45 EDT, an hour before.
    let schedule = crate::daemon::cron::Schedule::parse("30 1 * * *").unwrap();
    let second_pass = later(NY, "2027-11-07T01:45");
    assert_eq!(text(&schedule.latest_at_or_before(&second_pass).unwrap()), "2027-11-07T01:30:00-05:00");
}

#[test]
fn month_and_leap_year_ends() {
    assert_eq!(next("0 3 1 * *", "UTC", "2027-12-15T00:00"), "2028-01-01T03:00:00+00:00");
    assert_eq!(next("0 3 29 2 *", "UTC", "2027-03-01T00:00"), "2028-02-29T03:00:00+00:00");
}

/// The usual gap between two runs, at a time with no change of offset near (so the machine's own zone does not matter).
#[test]
fn schedule_gaps() {
    for (expr, seconds) in [("0 * * * *", 3600), ("30 * * * *", 3600), ("0 3 * * 0", 604_800)] {
        assert_eq!(crate::daemon::cron::schedule_gap(expr, at("2026-11-27T12:00:00Z")).map(|d| secs(&d)), Some(seconds), "{expr}");
    }
}


/// The mail `Date:` header (RFC 2822, UTC, day not padded), as chrono wrote it.
#[test]
fn mail_date_headers_read_the_same() {
    for (when, header) in [
        ("2026-10-11T07:00:00Z", "Date: Sun, 11 Oct 2026 07:00:00 +0000"),
        ("2026-11-01T07:05:09Z", "Date: Sun, 1 Nov 2026 07:05:09 +0000"),
        ("2026-11-27T23:59:59.9Z", "Date: Fri, 27 Nov 2026 23:59:59 +0000"),
    ] {
        let mail = crate::daemon::deliver::compose_mail("a@x", &["b@x".to_string()], "s", "b", at(when));
        assert!(mail.lines().any(|l| l == header), "{when}:\n{mail}");
    }
}
