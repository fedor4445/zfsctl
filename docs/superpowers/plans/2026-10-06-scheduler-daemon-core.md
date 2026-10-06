# Scheduler Daemon Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `zfsctl daemon` runs scheduled scrubs on the hub, unattended: cron schedules, catch-up of a missed run, restart-safe completion tracking, a run history, a notify hook, a heartbeat, and an `rc.d` script. Usable with a hand-edited `jobs.toml`.

**Architecture:** A pure `Scheduler` and a thread-free `Engine` turn the clock and the files (`jobs.toml`, `history.jsonl`) into `Work`; the runtime in `daemon/mod.rs` executes that work on threads using the existing `CommandRunner` hosts, so remote scrubs reuse the SSH + sudo path. State lives in `/var/db/zfsctl/`: the daemon is the only writer of history and heartbeat, and the TUI (a later plan) is the only writer of `jobs.toml`.

**Tech Stack:** Rust 2021; new dependencies `chrono` (with `serde`) and `serde_json`; `chrono-tz` as a dev-dependency (DST tests); `nix` gains the `signal` feature; existing `signal-hook`, `toml`, `serde`.

**Spec:** `docs/superpowers/specs/2026-10-06-scheduler-daemon-and-scrubs-design.md` (this is plan 1 of 2; the TUI Jobs section is plan 2).

**Verified:** Every code block below was written and run in a throwaway copy of the repo before this plan was written (805 tests passing, clippy clean on the new files). The daemon was also run against the real disposable `scratch` pool on the FreeBSD hub, and these behaviors were seen working: it fires on the minute; records `started` with the pool's pre-start scan summary and `finished` (`ok`) at the first 30-second poll; refuses a second instance; stops cleanly on SIGTERM (heartbeat file removed) leaving an unfinished run; on restart re-attaches to that run and finishes it; skips a catch-up slot as `already running` when the resumed scrub holds the pool; restarts immediately after `kill -9` even though the dead daemon's heartbeat is still fresh, re-attaching to the in-flight run; and keeps the previous schedule (logging the TOML error) when `jobs.toml` is corrupted, still firing on schedule. **Not seen live:** the notify hook (unit-tested only) and a catch-up run that actually executes rather than being skipped (unit-tested only). Executing the plan is transcription plus re-running those checks.

## Global Constraints

- The crate has `unsafe_code = "forbid"`: no `unsafe` anywhere. Clippy pedantic is on (`indexing_slicing`, `unwrap_used` warn): new non-test code must be clean under `cargo clippy --all-targets`.
- New dependencies are exactly `chrono` (features `serde`), `serde_json`, and dev-only `chrono-tz`; `nix` gains the `signal` feature. Nothing else.
- Schedules are five-field cron, evaluated in the hub's local time zone; day of week `0`-`6` with `0` Sunday; both day fields restricted means either matches; a nonexistent local time is skipped and a repeated one fires once.
- `jobs.toml` can name only job kinds the daemon implements and never contains a command. The only configured command is `[daemon] notify_command`, and job data reaches it only through `ZFSCTL_*` environment variables.
- State directory `/var/db/zfsctl/` is mode `0700`; `jobs.toml`, `history.jsonl` and `daemon.json` are `0600`; `jobs.toml` and `daemon.json` are written by temp file plus rename.
- The pool name is passed after `--` (`zpool scrub -- <pool>`, already how `scrub_pool` works). No new sudoers entry on any managed host.
- Baseline before starting: `cargo test` reports 730 passed.

## Spec clarification made by this plan

The spec says a catch-up runs a job "whose most recent scheduled time is later than its last recorded run". A job that has never run has no last recorded run, so a newly created job would immediately "catch up" a slot from before it existed. This plan adds an optional `created_at` field to `Job` and defines the baseline as the latest of `created_at` and the last recorded `scheduled_for`, falling back to the time the daemon first loaded the job. Task 2 amends the spec accordingly.

## Review Focus

- A write torn by a power cut leaves a history line with no newline; the next append must not be glued onto it and lost. Pinned in Task 3.
- A scrub that finishes before the first poll must be told apart from the previous scrub's "completed" line; a pool that never starts scanning must not hang forever. Pinned in Task 5.
- A brand-new job must not fire for a slot that passed before it existed, and a missed slot must run exactly once however many were missed. Pinned in Task 4.
- DST: a time inside the spring-forward gap is skipped and a repeated autumn time fires once. Pinned in Task 1.
- A bad edit of `jobs.toml` must keep the previous schedule, and a clock stepped backwards must not fire a slot twice. Pinned in Tasks 4 and 7.
- A crashed daemon's still-fresh heartbeat must not stop a supervisor restarting it, but a live second daemon must be refused. Pinned in Task 8.
- Job data must never be executed by the notify command. Pinned in Task 6.

---

### Task 1: Dependencies and the cron schedule parser

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/main.rs` (add `mod daemon;`)
- Create: `src/daemon/mod.rs`
- Create: `src/daemon/cron.rs`

**Interfaces:**
- Consumes: nothing.
- Produces, in `crate::daemon::cron`:
  - `pub struct Schedule` (`Debug, Clone, PartialEq, Eq`)
  - `Schedule::parse(expr: &str) -> Result<Schedule, String>`
  - `Schedule::next_after<Tz: TimeZone>(&self, after: &DateTime<Tz>) -> Option<DateTime<Tz>>` (strictly after)
  - `Schedule::latest_at_or_before<Tz: TimeZone>(&self, at: &DateTime<Tz>) -> Option<DateTime<Tz>>`

- [ ] **Step 1: Add the dependencies**

In `Cargo.toml`, under `[dependencies]` add after `signal-hook = "0.3"`:

```toml
chrono = { version = "0.4", features = ["serde"] }
serde_json = "1"
```

change the `nix` line to `nix = { version = "0.29", features = ["user", "signal"] }` and delete the `nix` line under `[dev-dependencies]`, then add under `[dev-dependencies]`:

```toml
chrono-tz = "0.10"
```

In `src/main.rs`, add `mod daemon;` after `mod config;`.

Run: `cargo build 2>&1 | tail -3`
Expected: builds (it downloads the new crates).

- [ ] **Step 2: Write the failing tests**

Create `src/daemon/mod.rs` containing `pub mod cron;` and create `src/daemon/cron.rs` containing ONLY this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Utc};
    use chrono_tz::America::New_York;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn next(expr: &str, after: &str) -> String {
        Schedule::parse(expr).unwrap().next_after(&utc(after)).unwrap().to_rfc3339()
    }

    #[test]
    fn parses_every_field_form() {
        for ok in ["* * * * *", "0 3 1 * *", "*/15 * * * *", "0,30 8-17 * * 1-5", "5-50/15 0 * * *", "0 0 29 2 *"] {
            assert!(Schedule::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_malformed_expressions_with_a_reason() {
        for (bad, why) in [
            ("", "found 0"),
            ("* * * *", "found 4"),
            ("* * * * * *", "found 6"),
            ("60 * * * *", "outside 0-59"),
            ("* 24 * * *", "outside 0-23"),
            ("* * 0 * *", "outside 1-31"),
            ("* * * 13 *", "outside 1-12"),
            ("* * * * 7", "outside 0-6"),
            ("*/0 * * * *", "at least 1"),
            ("5/10 * * * *", "needs '*' or a range"),
            ("30-10 * * * *", "backwards"),
            ("a * * * *", "not a number"),
            (",5 * * * *", "not a number"),
        ] {
            let err = Schedule::parse(bad).unwrap_err();
            assert!(err.contains(why), "{bad:?}: {err}");
        }
    }

    #[test]
    fn next_after_is_strictly_after_and_minute_aligned() {
        assert_eq!(next("*/15 * * * *", "2026-10-06T10:00:00Z"), "2026-10-06T10:15:00+00:00");
        assert_eq!(next("*/15 * * * *", "2026-10-06T10:07:30Z"), "2026-10-06T10:15:00+00:00");
        assert_eq!(next("0 3 * * *", "2026-10-06T03:00:00Z"), "2026-10-07T03:00:00+00:00");
    }

    #[test]
    fn monthly_weekly_and_year_rollover() {
        assert_eq!(next("0 3 1 * *", "2026-10-06T10:00:00Z"), "2026-11-01T03:00:00+00:00");
        assert_eq!(next("0 3 1 * *", "2026-12-15T00:00:00Z"), "2027-01-01T03:00:00+00:00");
        // 2026-10-06 is a Tuesday; the next Sunday is the 11th.
        assert_eq!(next("0 3 * * 0", "2026-10-06T10:00:00Z"), "2026-10-11T03:00:00+00:00");
    }

    #[test]
    fn day_of_month_and_day_of_week_both_restricted_means_either() {
        // 2026-10-13 is a Tuesday: the 13th matches the day of month, and the 9th is a Friday.
        assert_eq!(next("0 0 13 * 5", "2026-10-06T00:00:00Z"), "2026-10-09T00:00:00+00:00");
        assert_eq!(next("0 0 13 * 5", "2026-10-09T00:00:00Z"), "2026-10-13T00:00:00+00:00");
    }

    #[test]
    fn leap_day_waits_for_a_leap_year() {
        assert_eq!(next("0 0 29 2 *", "2026-03-01T00:00:00Z"), "2028-02-29T00:00:00+00:00");
    }

    #[test]
    fn an_impossible_date_has_no_next_time() {
        assert!(Schedule::parse("0 0 31 2 *").unwrap().next_after(&utc("2026-01-01T00:00:00Z")).is_none());
    }

    #[test]
    fn latest_at_or_before_includes_the_instant_itself() {
        let s = Schedule::parse("0 3 * * *").unwrap();
        assert_eq!(s.latest_at_or_before(&utc("2026-10-06T03:00:00Z")).unwrap().to_rfc3339(), "2026-10-06T03:00:00+00:00");
        assert_eq!(s.latest_at_or_before(&utc("2026-10-06T02:59:59Z")).unwrap().to_rfc3339(), "2026-10-05T03:00:00+00:00");
        assert_eq!(s.latest_at_or_before(&utc("2026-10-06T10:30:12Z")).unwrap().to_rfc3339(), "2026-10-06T03:00:00+00:00");
    }

    #[test]
    fn latest_at_or_before_crosses_month_and_year_boundaries() {
        let s = Schedule::parse("0 3 1 * *").unwrap();
        assert_eq!(s.latest_at_or_before(&utc("2027-01-01T02:00:00Z")).unwrap().to_rfc3339(), "2026-12-01T03:00:00+00:00");
    }

    #[test]
    fn evaluates_in_the_given_zone() {
        let est = FixedOffset::west_opt(5 * 3600).unwrap();
        let after = utc("2026-10-06T10:00:00Z").with_timezone(&est); // 05:00 local
        assert_eq!(Schedule::parse("0 3 * * *").unwrap().next_after(&after).unwrap().to_rfc3339(), "2026-10-07T03:00:00-05:00");
    }

    #[test]
    fn a_time_inside_the_spring_forward_gap_is_skipped() {
        // New York, 2026-03-08: 02:00 jumps to 03:00, so 02:30 does not exist that day.
        let s = Schedule::parse("30 2 * * *").unwrap();
        let after = New_York.with_ymd_and_hms(2026, 3, 7, 12, 0, 0).unwrap();
        let n = s.next_after(&after).unwrap();
        assert_eq!(n.naive_local().to_string(), "2026-03-09 02:30:00", "{n}");
    }

    #[test]
    fn a_repeated_autumn_time_fires_once_at_its_first_occurrence() {
        // New York, 2026-11-01: 01:00-02:00 happens twice (EDT then EST).
        let s = Schedule::parse("30 1 * * *").unwrap();
        let after = New_York.with_ymd_and_hms(2026, 10, 31, 12, 0, 0).unwrap();
        let first = s.next_after(&after).unwrap();
        assert_eq!(first.to_rfc3339(), "2026-11-01T01:30:00-04:00");
        let again = s.next_after(&first).unwrap();
        assert_eq!(again.naive_local().to_string(), "2026-11-02 01:30:00", "the second 01:30 must not fire: {again}");
        // Asked for the latest time at the second 01:30 (EST), the later occurrence is the answer.
        let second_instant = New_York.from_local_datetime(&NaiveDate::from_ymd_opt(2026, 11, 1).unwrap().and_hms_opt(1, 45, 0).unwrap()).latest().unwrap();
        assert_eq!(s.latest_at_or_before(&second_instant).unwrap().to_rfc3339(), "2026-11-01T01:30:00-05:00");
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test daemon::cron 2>&1 | grep -E "^error" -A4 | head -20`
Expected: FAIL to compile (`cannot find type Schedule`).

- [ ] **Step 4: Write the implementation**

Put this at the TOP of `src/daemon/cron.rs`, above the test module:

```rust
//! A five-field cron schedule: parsing, and the nearest scheduled time
//! after or before an instant, evaluated in a caller-supplied time zone.
//!
//! Fields are minute, hour, day of month, month, day of week (`0`-`6`,
//! `0` = Sunday). Each accepts `*`, a number, a comma list, a range
//! `a-b` and a step `*/n` or `a-b/n`. When both day fields are
//! restricted a day matches if EITHER does (standard cron behavior).

use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    days_star: bool,
    weekdays_star: bool,
}

fn parse_number(text: &str, min: u32, max: u32, what: &str) -> Result<u32, String> {
    let n: u32 = text.parse().map_err(|_| format!("{what}: '{text}' is not a number"))?;
    if n < min || n > max {
        return Err(format!("{what}: {n} is outside {min}-{max}"));
    }
    Ok(n)
}

/// Parses one field into a bitmask with bit `n` set for each allowed value `n`.
fn parse_field(text: &str, min: u32, max: u32, what: &str) -> Result<u64, String> {
    if text.is_empty() {
        return Err(format!("{what}: empty field"));
    }
    let mut mask = 0u64;
    for part in text.split(',') {
        let (base, step) = match part.split_once('/') {
            Some((b, s)) => {
                let step: u32 = s.parse().map_err(|_| format!("{what}: bad step '{s}'"))?;
                if step == 0 {
                    return Err(format!("{what}: step must be at least 1"));
                }
                (b, Some(step))
            }
            None => (part, None),
        };
        let (lo, hi) = if base == "*" {
            (min, max)
        } else if let Some((a, b)) = base.split_once('-') {
            let (a, b) = (parse_number(a, min, max, what)?, parse_number(b, min, max, what)?);
            if a > b {
                return Err(format!("{what}: range {a}-{b} runs backwards"));
            }
            (a, b)
        } else {
            if step.is_some() {
                return Err(format!("{what}: a step needs '*' or a range, not '{base}'"));
            }
            let n = parse_number(base, min, max, what)?;
            (n, n)
        };
        let mut v = lo;
        while v <= hi {
            mask |= 1u64 << v;
            v += step.unwrap_or(1);
        }
    }
    Ok(mask)
}

impl Schedule {
    pub fn parse(expr: &str) -> Result<Schedule, String> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields.as_slice() else {
            return Err(format!("expected 5 fields (minute hour day month weekday), found {}", fields.len()));
        };
        let narrow = |m: u64| u32::try_from(m).map_err(|_| "internal: mask too wide".to_string());
        Ok(Schedule {
            minutes: parse_field(minute, 0, 59, "minute")?,
            hours: narrow(parse_field(hour, 0, 23, "hour")?)?,
            days: narrow(parse_field(day, 1, 31, "day of month")?)?,
            months: u16::try_from(parse_field(month, 1, 12, "month")?).map_err(|_| "internal: mask too wide".to_string())?,
            weekdays: u8::try_from(parse_field(weekday, 0, 6, "day of week")?).map_err(|_| "internal: mask too wide".to_string())?,
            days_star: day.starts_with('*'),
            weekdays_star: weekday.starts_with('*'),
        })
    }

    fn month_ok(&self, t: &NaiveDateTime) -> bool {
        self.months & (1 << t.month()) != 0
    }

    fn day_ok(&self, t: &NaiveDateTime) -> bool {
        let dom = self.days & (1 << t.day()) != 0;
        let dow = self.weekdays & (1 << t.weekday().num_days_from_sunday()) != 0;
        match (self.days_star, self.weekdays_star) {
            (true, true) => true,
            (true, false) => dow,
            (false, true) => dom,
            (false, false) => dom || dow,
        }
    }

    fn hour_ok(&self, t: &NaiveDateTime) -> bool {
        self.hours & (1 << t.hour()) != 0
    }

    fn minute_ok(&self, t: &NaiveDateTime) -> bool {
        self.minutes & (1 << t.minute()) != 0
    }

    /// The first matching wall-clock minute strictly after `from`.
    fn next_naive(&self, from: NaiveDateTime) -> Option<NaiveDateTime> {
        let limit_year = from.year() + 8;
        let mut t = floor_minute(from) + Duration::minutes(1);
        for _ in 0..4_000_000 {
            if t.year() > limit_year {
                return None;
            }
            if !self.month_ok(&t) {
                t = first_of_next_month(t)?;
            } else if !self.day_ok(&t) {
                t = start_of_day(t) + Duration::days(1);
            } else if !self.hour_ok(&t) {
                t = floor_hour(t) + Duration::hours(1);
            } else if !self.minute_ok(&t) {
                t += Duration::minutes(1);
            } else {
                return Some(t);
            }
        }
        None
    }

    /// The last matching wall-clock minute at or before `from`.
    fn prev_naive(&self, from: NaiveDateTime) -> Option<NaiveDateTime> {
        let limit_year = from.year() - 8;
        let mut t = floor_minute(from);
        for _ in 0..4_000_000 {
            if t.year() < limit_year {
                return None;
            }
            if !self.month_ok(&t) {
                t = start_of_month(t) - Duration::minutes(1);
            } else if !self.day_ok(&t) {
                t = start_of_day(t) - Duration::minutes(1);
            } else if !self.hour_ok(&t) {
                t = floor_hour(t) - Duration::minutes(1);
            } else if !self.minute_ok(&t) {
                t -= Duration::minutes(1);
            } else {
                return Some(t);
            }
        }
        None
    }

    /// The first scheduled time strictly after `after`, in `after`'s zone.
    /// A local time that does not exist (a spring-forward gap) is skipped;
    /// one that occurs twice (autumn) fires at its first occurrence only.
    pub fn next_after<Tz: TimeZone>(&self, after: &DateTime<Tz>) -> Option<DateTime<Tz>> {
        let tz = after.timezone();
        let mut cursor = after.naive_local();
        loop {
            let naive = self.next_naive(cursor)?;
            match tz.from_local_datetime(&naive) {
                LocalResult::Single(dt) if dt > *after => return Some(dt),
                LocalResult::Ambiguous(first, _) if first > *after => return Some(first),
                _ => cursor = naive,
            }
        }
    }

    /// The most recent scheduled time at or before `at`, in `at`'s zone.
    pub fn latest_at_or_before<Tz: TimeZone>(&self, at: &DateTime<Tz>) -> Option<DateTime<Tz>> {
        let tz = at.timezone();
        let mut cursor = at.naive_local();
        loop {
            let naive = self.prev_naive(cursor)?;
            match tz.from_local_datetime(&naive) {
                LocalResult::Single(dt) if dt <= *at => return Some(dt),
                LocalResult::Ambiguous(first, second) => {
                    if second <= *at {
                        return Some(second);
                    }
                    if first <= *at {
                        return Some(first);
                    }
                    cursor = naive - Duration::minutes(1);
                }
                _ => cursor = naive - Duration::minutes(1),
            }
        }
    }
}

fn floor_minute(t: NaiveDateTime) -> NaiveDateTime {
    t.with_second(0).and_then(|t| t.with_nanosecond(0)).unwrap_or(t)
}

fn floor_hour(t: NaiveDateTime) -> NaiveDateTime {
    floor_minute(t).with_minute(0).unwrap_or(t)
}

fn start_of_day(t: NaiveDateTime) -> NaiveDateTime {
    floor_hour(t).with_hour(0).unwrap_or(t)
}

fn start_of_month(t: NaiveDateTime) -> NaiveDateTime {
    start_of_day(t).with_day(1).unwrap_or(t)
}

fn first_of_next_month(t: NaiveDateTime) -> Option<NaiveDateTime> {
    let (year, month) = if t.month() == 12 { (t.year() + 1, 1) } else { (t.year(), t.month() + 1) };
    NaiveDate::from_ymd_opt(year, month, 1).and_then(|d| d.and_hms_opt(0, 0, 0))
}
```

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test daemon::cron 2>&1 | grep -E "^error|^test result|FAILED|panicked" -A8`
Expected: `test result: ok. 12 passed`. This includes the two DST tests (a spring-forward gap time is skipped; an autumn repeated time fires once).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs src/daemon/mod.rs src/daemon/cron.rs
git commit -m "feat: parse five-field cron schedules with DST-aware next and previous times

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 2: The job store (`jobs.toml`)

**Files:**
- Create: `src/jobs.rs`
- Modify: `src/daemon/mod.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Schedule::parse` (Task 1).
- Produces, in `crate::jobs`:
  - `pub const KIND_SCRUB: &str = "scrub"`
  - `pub struct Job { pub id: String, pub host: String, pub kind: String, pub pool: Option<String>, pub schedule: String, pub catch_up: bool, pub enabled: bool, pub created_at: Option<DateTime<Utc>> }` (`Debug, Clone, PartialEq, Serialize, Deserialize`)
  - `pub fn load(path: &Path) -> Result<Vec<Job>>`, `pub fn save(path: &Path, jobs: &[Job]) -> Result<()>`
  - `pub fn resolve(job: &Job, known_hosts: &[String]) -> Result<Schedule, String>` (the Err is the reason the job cannot run)

- [ ] **Step 1: Write the failing tests**

Create `src/jobs.rs` containing ONLY the test module below. Update `src/daemon/mod.rs` to:

```rust
pub mod cron;
```

In `src/main.rs`, add `mod jobs;` after `mod hosts;`.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn scrub(id: &str) -> Job {
        Job {
            id: id.to_string(),
            host: "local".to_string(),
            kind: KIND_SCRUB.to_string(),
            pool: Some("tank".to_string()),
            schedule: "0 3 1 * *".to_string(),
            catch_up: true,
            enabled: true,
            created_at: None,
        }
    }

    fn hosts() -> Vec<String> {
        vec!["local".to_string(), "fbsd16".to_string()]
    }

    #[test]
    fn a_missing_file_is_an_empty_job_list() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("jobs.toml")).unwrap().is_empty());
    }

    #[test]
    fn save_then_load_round_trips_and_the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("jobs.toml");
        let mut job = scrub("scrub-tank");
        job.created_at = Some(DateTime::parse_from_rfc3339("2026-10-06T12:00:00Z").unwrap().with_timezone(&Utc));
        save(&path, &[job.clone(), scrub("scrub-two")]).unwrap();
        assert_eq!(load(&path).unwrap(), vec![job, scrub("scrub-two")]);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!path.with_extension("toml.tmp").exists(), "the temp file is renamed away");
    }

    #[test]
    fn omitted_flags_default_to_on_and_a_hand_written_file_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.toml");
        std::fs::write(&path, "[[jobs]]\nid = \"a\"\nhost = \"local\"\nkind = \"scrub\"\npool = \"tank\"\nschedule = \"0 3 * * 0\"\n").unwrap();
        let jobs = load(&path).unwrap();
        assert!(jobs[0].catch_up && jobs[0].enabled && jobs[0].created_at.is_none());
    }

    #[test]
    fn a_duplicate_id_or_malformed_toml_is_an_error_not_a_partial_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.toml");
        save(&path, &[scrub("same"), scrub("same")]).unwrap();
        assert!(load(&path).unwrap_err().to_string().contains("duplicate job id 'same'"));
        std::fs::write(&path, "not toml [[[").unwrap();
        assert!(load(&path).is_err());
    }

    #[test]
    fn an_unknown_kind_still_parses_but_is_not_runnable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.toml");
        std::fs::write(&path, "[[jobs]]\nid = \"x\"\nhost = \"local\"\nkind = \"replicate\"\nschedule = \"0 3 * * *\"\n").unwrap();
        let jobs = load(&path).unwrap();
        assert_eq!(resolve(&jobs[0], &hosts()).unwrap_err(), "unsupported job kind 'replicate'");
    }

    #[test]
    fn resolve_explains_why_a_job_cannot_run() {
        assert!(resolve(&scrub("ok"), &hosts()).is_ok());
        let mut no_pool = scrub("a");
        no_pool.pool = None;
        assert!(resolve(&no_pool, &hosts()).unwrap_err().contains("needs a pool"));
        let mut no_host = scrub("b");
        no_host.host = "gone".to_string();
        assert!(resolve(&no_host, &hosts()).unwrap_err().contains("'gone' is not in config.toml"));
        let mut bad = scrub("c");
        bad.schedule = "61 * * * *".to_string();
        assert!(resolve(&bad, &hosts()).unwrap_err().starts_with("bad schedule:"));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test jobs:: 2>&1 | grep -E "^error" -A4 | head -20`
Expected: FAIL to compile with `cannot find ...` errors for the items defined in Step 3.

- [ ] **Step 3: Write the implementation**

Put this at the TOP of `src/jobs.rs`, above the test module:

```rust
//! The job store: `jobs.toml`, written by the TUI and read by the daemon.
//!
//! `kind` is a plain string, not an enum, so a `jobs.toml` written by a
//! newer zfsctl (with job kinds this build does not know) still parses; such
//! a job is reported as unsupported and never run.

use crate::daemon::cron::Schedule;
use crate::error::{AppError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub const KIND_SCRUB: &str = "scrub";

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    /// A host name from `config.toml`; `local` is the hub itself.
    pub host: String,
    pub kind: String,
    /// The pool a scrub job runs on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// A five-field cron expression, in the hub's local time zone.
    pub schedule: String,
    #[serde(default = "yes")]
    pub catch_up: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// When the job was created. A catch-up never reaches back before this,
    /// so a new job does not fire for a slot that passed before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct JobFile {
    #[serde(default)]
    jobs: Vec<Job>,
}

/// Reads `jobs.toml`. A missing file is an empty job list. A parse error or a
/// duplicate id is an error, so the daemon can keep the job set it already has.
pub fn load(path: &Path) -> Result<Vec<Job>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).map_err(|e| AppError::Config(format!("reading {}: {e}", path.display())))?;
    let file: JobFile = toml::from_str(&text).map_err(|e| AppError::Config(format!("parsing {}: {e}", path.display())))?;
    let mut seen = std::collections::HashSet::new();
    for job in &file.jobs {
        if !seen.insert(job.id.as_str()) {
            return Err(AppError::Config(format!("{}: duplicate job id '{}'", path.display(), job.id)));
        }
    }
    Ok(file.jobs)
}

// The daemon only reads jobs; the TUI Jobs section (a later plan) is the writer.
#[cfg_attr(not(test), allow(dead_code))]
/// Writes `jobs.toml` atomically with mode 0600: a temp file in the same
/// directory is written and synced, then renamed over the real one, so a
/// reader (or a power cut) never sees a half-written file.
pub fn save(path: &Path, jobs: &[Job]) -> Result<()> {
    let text = toml::to_string_pretty(&JobFile { jobs: jobs.to_vec() }).map_err(|e| AppError::Config(format!("serializing jobs: {e}")))?;
    let io = |what: &str, e: std::io::Error| AppError::Config(format!("{what} {}: {e}", path.display()));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io("creating the directory of", e))?;
    }
    let tmp = path.with_extension("toml.tmp");
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).map_err(|e| io("writing", e))?;
    file.write_all(text.as_bytes()).map_err(|e| io("writing", e))?;
    file.sync_all().map_err(|e| io("syncing", e))?;
    std::fs::rename(&tmp, path).map_err(|e| io("replacing", e))
}

/// Whether the daemon can run `job`: its schedule if so, or the reason it cannot.
pub fn resolve(job: &Job, known_hosts: &[String]) -> std::result::Result<Schedule, String> {
    if job.kind != KIND_SCRUB {
        return Err(format!("unsupported job kind '{}'", job.kind));
    }
    if job.pool.as_deref().is_none_or(str::is_empty) {
        return Err("a scrub job needs a pool".to_string());
    }
    if !known_hosts.iter().any(|h| h == &job.host) {
        return Err(format!("host '{}' is not in config.toml", job.host));
    }
    Schedule::parse(&job.schedule).map_err(|e| format!("bad schedule: {e}"))
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test jobs:: 2>&1 | grep -E "^error|^test result|FAILED|panicked" -A8`
Expected: all tests PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "src/jobs.rs"`
Expected: only `never used` dead-code warnings for items a later task wires up; nothing else.

```bash
git add src/jobs.rs src/main.rs
git commit -m "feat: add the jobs.toml store with atomic saves and a runnable check

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: Amend the spec for `created_at`**

In `docs/superpowers/specs/2026-10-06-scheduler-daemon-and-scrubs-design.md`, in the `jobs.toml` example under "### 2. Jobs", add the line `created_at = "2026-10-06T12:00:00Z"` after `enabled = true`, and add this bullet after the one about unknown kinds:

```
- `created_at` is optional. A catch-up never reaches back before it, so a new job does not fire for a slot that passed before it existed. A job with neither `created_at` nor history uses the time the daemon first loaded it as its baseline.
```

```bash
git add docs/superpowers/specs/2026-10-06-scheduler-daemon-and-scrubs-design.md
git commit -m "docs: define the catch-up baseline for a job that has never run

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 3: The run history (`history.jsonl`)

**Files:**
- Create: `src/daemon/history.rs`
- Modify: `src/daemon/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces, in `crate::daemon::history`:
  - `pub enum EventKind { Started, Finished, Skipped }`, `pub enum RunResult { Ok, Failed, Skipped }` (both `Copy`)
  - `pub struct Event { pub job_id, pub run_id: String, pub event: EventKind, pub at: DateTime<Utc>, pub scheduled_for: Option<DateTime<Utc>>, pub result: Option<RunResult>, pub message: String, pub catch_up: bool, pub summary: Option<String> }`
  - `pub struct Run { job_id, run_id, started_at, scheduled_for: Option<..>, finished_at: Option<..>, result: Option<RunResult> /* None = still running */, message, catch_up, summary }`
  - `pub fn runs(events: &[Event]) -> Vec<Run>`, `pub fn last_scheduled(runs: &[Run]) -> HashMap<String, DateTime<Utc>>`
  - `pub struct History` with `new(path)`, `append(&Event) -> io::Result<()>`, `read() -> Vec<Event>`, `prune(keep: usize) -> io::Result<()>`

- [ ] **Step 1: Write the failing tests**

Create `src/daemon/history.rs` containing ONLY the test module below. Update `src/daemon/mod.rs` to:

```rust
pub mod cron;
pub mod history;
```

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn ev(job: &str, run: &str, kind: EventKind, when: &str, result: Option<RunResult>) -> Event {
        Event {
            job_id: job.to_string(),
            run_id: run.to_string(),
            event: kind,
            at: at(when),
            scheduled_for: Some(at(when)),
            result,
            message: String::new(),
            catch_up: false,
            summary: None,
        }
    }

    #[test]
    fn appended_events_read_back_in_order_and_the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        assert!(h.read().is_empty(), "a missing file is empty");
        let a = ev("j", "r1", EventKind::Started, "2026-10-06T03:00:00Z", None);
        let b = ev("j", "r1", EventKind::Finished, "2026-10-06T03:20:00Z", Some(RunResult::Ok));
        h.append(&a).unwrap();
        h.append(&b).unwrap();
        assert_eq!(h.read(), vec![a, b]);
        assert_eq!(std::fs::metadata(h.path()).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_torn_or_garbage_line_is_skipped_and_the_rest_survive() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        let good = ev("j", "r1", EventKind::Started, "2026-10-06T03:00:00Z", None);
        h.append(&good).unwrap();
        std::fs::OpenOptions::new().append(true).open(h.path()).unwrap().write_all(b"{\"job_id\":\"j\",\"run_i").unwrap();
        assert_eq!(h.read(), vec![good.clone()], "the torn final line is dropped");
        h.append(&good).unwrap();
        assert_eq!(h.read(), vec![good.clone(), good], "an append after a torn line starts a fresh line, so it is not lost with the fragment");
    }

    #[test]
    fn runs_reconstruct_started_finished_skipped_and_running() {
        let mut started = ev("j", "r1", EventKind::Started, "2026-10-06T03:00:00Z", None);
        started.summary = Some("scrub repaired 0B on Sun".to_string());
        let mut finished = ev("j", "r1", EventKind::Finished, "2026-10-06T03:20:00Z", Some(RunResult::Failed));
        finished.message = "2 errors".to_string();
        let skipped = ev("j", "r2", EventKind::Skipped, "2026-10-07T03:00:00Z", Some(RunResult::Skipped));
        let running = ev("k", "r3", EventKind::Started, "2026-10-07T04:00:00Z", None);
        let all = runs(&[started, finished, skipped, running]);
        assert_eq!(all.len(), 3);
        assert_eq!((all[0].result, all[0].message.as_str(), all[0].summary.as_deref()), (Some(RunResult::Failed), "2 errors", Some("scrub repaired 0B on Sun")));
        assert_eq!(all[0].finished_at, Some(at("2026-10-06T03:20:00Z")));
        assert_eq!(all[1].result, Some(RunResult::Skipped));
        assert_eq!(all[2].result, None, "started with no finish is still running");
    }

    #[test]
    fn a_finish_with_no_start_is_a_complete_run() {
        let only = ev("j", "r1", EventKind::Finished, "2026-10-06T03:00:00Z", Some(RunResult::Failed));
        let all = runs(&[only]);
        assert_eq!((all.len(), all[0].result), (1, Some(RunResult::Failed)));
    }

    #[test]
    fn last_scheduled_takes_the_latest_slot_per_job_skipped_included() {
        let all = runs(&[
            ev("j", "r1", EventKind::Finished, "2026-10-01T03:00:00Z", Some(RunResult::Ok)),
            ev("j", "r2", EventKind::Skipped, "2026-10-08T03:00:00Z", Some(RunResult::Skipped)),
            ev("k", "r3", EventKind::Finished, "2026-10-02T03:00:00Z", Some(RunResult::Ok)),
        ]);
        let last = last_scheduled(&all);
        assert_eq!(last["j"], at("2026-10-08T03:00:00Z"));
        assert_eq!(last["k"], at("2026-10-02T03:00:00Z"));
    }

    #[test]
    fn prune_keeps_the_newest_runs_of_each_job_whole() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        for day in 1..=5 {
            let when = format!("2026-10-0{day}T03:00:00Z");
            h.append(&ev("j", &format!("j{day}"), EventKind::Started, &when, None)).unwrap();
            h.append(&ev("j", &format!("j{day}"), EventKind::Finished, &when, Some(RunResult::Ok))).unwrap();
        }
        h.append(&ev("k", "k1", EventKind::Finished, "2026-10-01T03:00:00Z", Some(RunResult::Ok))).unwrap();
        h.prune(2).unwrap();
        let kept: Vec<String> = runs(&h.read()).into_iter().map(|r| r.run_id).collect();
        assert_eq!(kept, vec!["j4", "j5", "k1"], "each kept run keeps both its events, other jobs are untouched");
        assert!(!h.path().with_extension("jsonl.tmp").exists());
    }

    #[test]
    fn prune_with_nothing_to_drop_leaves_the_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        h.append(&ev("j", "r1", EventKind::Finished, "2026-10-01T03:00:00Z", Some(RunResult::Ok))).unwrap();
        let before = std::fs::read(h.path()).unwrap();
        h.prune(10).unwrap();
        assert_eq!(std::fs::read(h.path()).unwrap(), before);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test daemon::history 2>&1 | grep -E "^error" -A4 | head -20`
Expected: FAIL to compile with `cannot find ...` errors for the items defined in Step 3.

- [ ] **Step 3: Write the implementation**

Put this at the TOP of `src/daemon/history.rs`, above the test module:

```rust
//! The run history: an append-only JSON-lines file, one line per run event.
//!
//! One line is one record, so a write torn by a power cut damages at most the
//! last line; a reader skips any line that does not parse.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::PathBuf;
#[cfg(test)]
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    Started,
    Finished,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunResult {
    Ok,
    Failed,
    Skipped,
}

// The JSON key is `event` (see the spec), so the field keeps that name.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub job_id: String,
    pub run_id: String,
    pub event: EventKind,
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_for: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<RunResult>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub catch_up: bool,
    /// On `started`: the pool's scan summary before the scrub began, so a
    /// restarted daemon can tell the new scrub's completion from the old one's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// A run reconstructed from its events.
#[allow(clippy::struct_field_names)] // `run_id` mirrors the history's JSON key
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub job_id: String,
    pub run_id: String,
    pub started_at: DateTime<Utc>,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// `None` while the run is still going.
    pub result: Option<RunResult>,
    pub message: String,
    pub catch_up: bool,
    pub summary: Option<String>,
}

/// Groups events by `run_id`, in order of first appearance.
pub fn runs(events: &[Event]) -> Vec<Run> {
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, Run> = HashMap::new();
    for e in events {
        let run = by_id.entry(e.run_id.clone()).or_insert_with(|| {
            order.push(e.run_id.clone());
            Run {
                job_id: e.job_id.clone(),
                run_id: e.run_id.clone(),
                started_at: e.at,
                scheduled_for: None,
                finished_at: None,
                result: None,
                message: String::new(),
                catch_up: false,
                summary: None,
            }
        });
        run.scheduled_for = run.scheduled_for.or(e.scheduled_for);
        run.catch_up |= e.catch_up;
        if e.summary.is_some() {
            run.summary.clone_from(&e.summary);
        }
        if e.event != EventKind::Started {
            run.finished_at = Some(e.at);
            run.result = e.result.or(Some(RunResult::Ok));
            run.message.clone_from(&e.message);
        }
    }
    order.into_iter().filter_map(|id| by_id.remove(&id)).collect()
}

/// The latest `scheduled_for` recorded for each job, over every run including
/// skipped ones: a skipped run still means that slot was dealt with.
pub fn last_scheduled(runs: &[Run]) -> HashMap<String, DateTime<Utc>> {
    let mut out: HashMap<String, DateTime<Utc>> = HashMap::new();
    for run in runs {
        if let Some(t) = run.scheduled_for {
            let slot = out.entry(run.job_id.clone()).or_insert(t);
            *slot = (*slot).max(t);
        }
    }
    out
}

pub struct History {
    path: PathBuf,
}

impl History {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        History { path: path.into() }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one event as a single line in a single write.
    pub fn append(&self, event: &Event) -> std::io::Result<()> {
        let mut line = serde_json::to_string(event).map_err(std::io::Error::other)?;
        line.push('\n');
        let mut file = std::fs::OpenOptions::new().read(true).append(true).create(true).mode(0o600).open(&self.path)?;
        // A write torn by a power cut leaves a final line with no newline; start a fresh
        // line so this record is not glued onto the fragment and lost with it.
        let len = file.metadata()?.len();
        if len > 0 {
            let mut last = [0u8; 1];
            file.read_exact_at(&mut last, len - 1)?;
            if last[0] != b'\n' {
                line.insert(0, '\n');
            }
        }
        file.write_all(line.as_bytes())
    }

    /// Every event that parses, oldest first. A missing file is empty.
    pub fn read(&self) -> Vec<Event> {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return Vec::new() };
        text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// Keeps only the newest `keep` runs of each job, rewriting the file atomically.
    pub fn prune(&self, keep: usize) -> std::io::Result<()> {
        let events = self.read();
        let all = runs(&events);
        let mut kept_ids = std::collections::HashSet::new();
        let mut per_job: HashMap<&str, usize> = HashMap::new();
        for run in all.iter().rev() {
            let n = per_job.entry(run.job_id.as_str()).or_insert(0);
            if *n < keep {
                *n += 1;
                kept_ids.insert(run.run_id.clone());
            }
        }
        if kept_ids.len() == all.len() {
            return Ok(());
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut out = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
            for e in events.iter().filter(|e| kept_ids.contains(&e.run_id)) {
                let mut line = serde_json::to_string(e).map_err(std::io::Error::other)?;
                line.push('\n');
                out.write_all(line.as_bytes())?;
            }
            out.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)
    }
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test daemon::history 2>&1 | grep -E "^error|^test result|FAILED|panicked" -A8`
Expected: all tests PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "src/daemon/history.rs"`
Expected: only `never used` dead-code warnings for items a later task wires up; nothing else.

```bash
git add src/daemon/history.rs
git commit -m "feat: add the append-only run history, tolerant of torn writes

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 4: The scheduler (catch-up, firing, reload)

**Files:**
- Create: `src/daemon/scheduler.rs`
- Modify: `src/daemon/mod.rs`

**Interfaces:**
- Consumes: `Job`, `resolve` (Task 2); `Schedule` (Task 1).
- Produces, in `crate::daemon::scheduler`:
  - `pub struct Due { pub job: Job, pub scheduled_for: DateTime<Utc>, pub catch_up: bool }`
  - `pub struct Unsupported { pub id: String, pub reason: String }`
  - `pub struct Scheduler<Tz: TimeZone>` with `new(tz)`, `reload(jobs, hosts: &[String], last_scheduled: &HashMap<String, DateTime<Utc>>, now) -> Vec<Unsupported>`, `catch_up(now) -> Vec<Due>`, `tick(now) -> Vec<Due>`, and (test-only) `next_time(id)`

- [ ] **Step 1: Write the failing tests**

Create `src/daemon/scheduler.rs` containing ONLY the test module below. Update `src/daemon/mod.rs` to:

```rust
pub mod cron;
pub mod history;
pub mod scheduler;
```

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::KIND_SCRUB;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn job(id: &str, schedule: &str) -> Job {
        Job {
            id: id.to_string(),
            host: "local".to_string(),
            kind: KIND_SCRUB.to_string(),
            pool: Some("tank".to_string()),
            schedule: schedule.to_string(),
            catch_up: true,
            enabled: true,
            created_at: None,
        }
    }

    fn hosts() -> Vec<String> {
        vec!["local".to_string()]
    }

    fn loaded(jobs: Vec<Job>, last: &[(&str, &str)], now: &str) -> Scheduler<Utc> {
        let mut s = Scheduler::new(Utc);
        let last: HashMap<String, DateTime<Utc>> = last.iter().map(|(k, v)| ((*k).to_string(), at(v))).collect();
        assert!(s.reload(jobs, &hosts(), &last, at(now)).is_empty());
        s
    }

    #[test]
    fn reload_reports_what_it_cannot_run_and_silently_drops_disabled_jobs() {
        let mut bad_host = job("bad-host", "0 3 * * *");
        bad_host.host = "gone".to_string();
        let mut kind = job("kind", "0 3 * * *");
        kind.kind = "replicate".to_string();
        let mut off = job("off", "0 3 * * *");
        off.enabled = false;
        let mut s = Scheduler::new(Utc);
        let report = s.reload(vec![job("ok", "0 3 * * *"), bad_host, kind, off], &hosts(), &HashMap::new(), at("2026-10-06T00:00:00Z"));
        assert_eq!(report.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(), vec!["bad-host", "kind"]);
        assert_eq!(s.len(), 1, "only the enabled, runnable job is scheduled");
    }

    #[test]
    fn a_job_fires_once_at_its_slot_and_then_waits_for_the_next() {
        let mut s = loaded(vec![job("j", "0 3 * * *")], &[], "2026-10-06T02:00:00Z");
        assert!(s.tick(at("2026-10-06T02:59:59Z")).is_empty());
        let due = s.tick(at("2026-10-06T03:00:00Z"));
        assert_eq!((due.len(), due[0].scheduled_for, due[0].catch_up), (1, at("2026-10-06T03:00:00Z"), false));
        assert!(s.tick(at("2026-10-06T03:00:01Z")).is_empty(), "not twice for one slot");
        assert_eq!(s.next_time("j"), Some(at("2026-10-07T03:00:00Z")));
    }

    #[test]
    fn a_stalled_tick_fires_once_not_once_per_missed_slot() {
        let mut s = loaded(vec![job("j", "0 * * * *")], &[], "2026-10-06T00:30:00Z");
        let due = s.tick(at("2026-10-06T09:15:00Z"));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].scheduled_for, at("2026-10-06T01:00:00Z"));
        assert_eq!(s.next_time("j"), Some(at("2026-10-06T10:00:00Z")), "the next slot comes from now, not from the old slot");
    }

    #[test]
    fn a_clock_stepped_backwards_cannot_fire_a_slot_twice() {
        let mut s = loaded(vec![job("j", "0 3 * * *")], &[], "2026-10-06T02:00:00Z");
        assert_eq!(s.tick(at("2026-10-06T03:00:00Z")).len(), 1);
        assert!(s.tick(at("2026-10-06T02:30:00Z")).is_empty(), "the clock went back an hour");
        assert!(s.tick(at("2026-10-06T03:00:00Z")).is_empty(), "and forward again to the same slot");
    }

    #[test]
    fn an_unchanged_job_keeps_its_pending_time_across_a_reload_and_an_edited_one_restarts() {
        let mut s = loaded(vec![job("j", "0 3 * * *")], &[], "2026-10-06T02:00:00Z");
        let _ = s.reload(vec![job("j", "0 3 * * *")], &hosts(), &HashMap::new(), at("2026-10-06T02:59:30Z"));
        assert_eq!(s.tick(at("2026-10-06T03:00:00Z")).len(), 1, "the slot was not lost to a reload");
        let _ = s.reload(vec![job("j", "30 4 * * *")], &hosts(), &HashMap::new(), at("2026-10-06T03:10:00Z"));
        assert_eq!(s.next_time("j"), Some(at("2026-10-06T04:30:00Z")));
    }

    #[test]
    fn catch_up_runs_a_missed_slot_once_however_many_were_missed() {
        // Weekly on Sunday 03:00; last run was three weeks ago; the daemon starts on a Wednesday.
        let mut s = loaded(vec![job("j", "0 3 * * 0")], &[("j", "2026-09-13T03:00:00Z")], "2026-10-07T12:00:00Z");
        let due = s.catch_up(at("2026-10-07T12:00:00Z"));
        assert_eq!(due.len(), 1, "one catch-up run, not three");
        assert_eq!((due[0].scheduled_for, due[0].catch_up), (at("2026-10-04T03:00:00Z"), true));
    }

    #[test]
    fn catch_up_does_nothing_when_the_latest_slot_was_already_handled() {
        let mut s = loaded(vec![job("j", "0 3 * * 0")], &[("j", "2026-10-04T03:00:00Z")], "2026-10-07T12:00:00Z");
        assert!(s.catch_up(at("2026-10-07T12:00:00Z")).is_empty());
    }

    #[test]
    fn a_brand_new_job_never_catches_up_for_a_slot_before_it_existed() {
        let mut no_history = loaded(vec![job("j", "0 3 * * *")], &[], "2026-10-07T12:00:00Z");
        assert!(no_history.catch_up(at("2026-10-07T12:00:00Z")).is_empty(), "no history and no created_at: baseline is the load time");
        let mut created = job("c", "0 3 * * *");
        created.created_at = Some(at("2026-10-07T09:00:00Z"));
        let mut s = loaded(vec![created], &[], "2026-10-07T12:00:00Z");
        assert!(s.catch_up(at("2026-10-07T12:00:00Z")).is_empty(), "created after this morning's 03:00 slot");
        let mut older = job("o", "0 3 * * *");
        older.created_at = Some(at("2026-10-05T00:00:00Z"));
        let mut s = loaded(vec![older], &[], "2026-10-07T12:00:00Z");
        let due = s.catch_up(at("2026-10-07T12:00:00Z"));
        assert_eq!(due[0].scheduled_for, at("2026-10-07T03:00:00Z"), "created before it, never ran: catch up");
    }

    #[test]
    fn catch_up_respects_the_per_job_toggle_and_disabled_jobs() {
        let mut no_catch_up = job("n", "0 3 * * *");
        no_catch_up.catch_up = false;
        no_catch_up.created_at = Some(at("2026-10-01T00:00:00Z"));
        let mut disabled = job("d", "0 3 * * *");
        disabled.enabled = false;
        disabled.created_at = Some(at("2026-10-01T00:00:00Z"));
        let mut s = loaded(vec![no_catch_up, disabled], &[], "2026-10-07T12:00:00Z");
        assert!(s.catch_up(at("2026-10-07T12:00:00Z")).is_empty());
    }

    #[test]
    fn catch_up_does_not_stop_the_normal_schedule() {
        let mut s = loaded(vec![job("j", "0 3 * * 0")], &[("j", "2026-09-13T03:00:00Z")], "2026-10-07T12:00:00Z");
        let _ = s.catch_up(at("2026-10-07T12:00:00Z"));
        assert_eq!(s.next_time("j"), Some(at("2026-10-11T03:00:00Z")));
        assert_eq!(s.tick(at("2026-10-11T03:00:00Z")).len(), 1);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test daemon::scheduler 2>&1 | grep -E "^error" -A4 | head -20`
Expected: FAIL to compile with `cannot find ...` errors for the items defined in Step 3.

- [ ] **Step 3: Write the implementation**

Put this at the TOP of `src/daemon/scheduler.rs`, above the test module:

```rust
//! Decides which jobs are due. Pure: it never reads a clock, runs a job or
//! touches a file; the caller passes `now` and acts on what comes back.

use crate::daemon::cron::Schedule;
use crate::jobs::{self, Job};
use chrono::{DateTime, TimeZone, Utc};
use std::collections::HashMap;

/// A job that should start now.
#[derive(Debug, Clone, PartialEq)]
pub struct Due {
    pub job: Job,
    /// The scheduled slot this run is for.
    pub scheduled_for: DateTime<Utc>,
    /// True when this run makes up a slot missed while the daemon was down.
    pub catch_up: bool,
}

/// A job the daemon will not run, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    pub id: String,
    pub reason: String,
}

struct Entry {
    job: Job,
    schedule: Schedule,
    /// Slots at or before this have been dealt with (or predate the job).
    baseline: DateTime<Utc>,
    next: Option<DateTime<Utc>>,
}

pub struct Scheduler<Tz: TimeZone> {
    tz: Tz,
    entries: Vec<Entry>,
}

impl<Tz: TimeZone> Scheduler<Tz> {
    pub fn new(tz: Tz) -> Self {
        Scheduler { tz, entries: Vec::new() }
    }

    fn next_after(&self, schedule: &Schedule, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        schedule.next_after(&after.with_timezone(&self.tz)).map(|t| t.with_timezone(&Utc))
    }

    /// Replaces the job set. A job whose definition is unchanged keeps its
    /// pending time; a new or edited one starts from `now`. Disabled jobs are
    /// left out silently; jobs the daemon cannot run are returned with the reason.
    pub fn reload(&mut self, jobs: Vec<Job>, hosts: &[String], last_scheduled: &HashMap<String, DateTime<Utc>>, now: DateTime<Utc>) -> Vec<Unsupported> {
        let mut old = std::mem::take(&mut self.entries);
        let mut unsupported = Vec::new();
        for job in jobs {
            let schedule = match jobs::resolve(&job, hosts) {
                Ok(s) => s,
                Err(reason) => {
                    unsupported.push(Unsupported { id: job.id.clone(), reason });
                    continue;
                }
            };
            if !job.enabled {
                continue;
            }
            if let Some(i) = old.iter().position(|e| e.job == job) {
                self.entries.push(old.swap_remove(i));
                continue;
            }
            let baseline = [job.created_at, last_scheduled.get(&job.id).copied()].into_iter().flatten().max().unwrap_or(now);
            let next = self.next_after(&schedule, now);
            self.entries.push(Entry { job, schedule, baseline, next });
        }
        unsupported
    }

    /// Run once when the daemon starts: every catch-up job whose latest
    /// scheduled slot is newer than its baseline is due now, however many
    /// slots were missed.
    pub fn catch_up(&mut self, now: DateTime<Utc>) -> Vec<Due> {
        let mut due = Vec::new();
        for i in 0..self.entries.len() {
            let Some(entry) = self.entries.get(i) else { continue };
            if !entry.job.catch_up {
                continue;
            }
            let latest = entry.schedule.latest_at_or_before(&now.with_timezone(&self.tz)).map(|t| t.with_timezone(&Utc));
            if let Some(slot) = latest.filter(|slot| *slot > entry.baseline) {
                due.push(Due { job: entry.job.clone(), scheduled_for: slot, catch_up: true });
            }
        }
        due
    }

    /// The jobs whose scheduled time has arrived. Each fires at most once per
    /// slot: afterwards its next time is computed from the later of `now` and
    /// the slot, so a stalled tick fires once (not once per missed slot) and a
    /// clock stepped backwards cannot fire a slot twice.
    pub fn tick(&mut self, now: DateTime<Utc>) -> Vec<Due> {
        let mut due = Vec::new();
        for i in 0..self.entries.len() {
            let Some(entry) = self.entries.get(i) else { continue };
            let Some(slot) = entry.next.filter(|slot| *slot <= now) else { continue };
            due.push(Due { job: entry.job.clone(), scheduled_for: slot, catch_up: false });
            let next = self.next_after(&entry.schedule, now.max(slot));
            if let Some(entry) = self.entries.get_mut(i) {
                entry.next = next;
            }
        }
        due
    }

    /// When `id` next fires.
    #[cfg(test)]
    pub fn next_time(&self, id: &str) -> Option<DateTime<Utc>> {
        self.entries.iter().find(|e| e.job.id == id).and_then(|e| e.next)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test daemon::scheduler 2>&1 | grep -E "^error|^test result|FAILED|panicked" -A8`
Expected: all tests PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "src/daemon/scheduler.rs"`
Expected: only `never used` dead-code warnings for items a later task wires up; nothing else.

```bash
git add src/daemon/scheduler.rs
git commit -m "feat: add the scheduler with once-only catch-up and clock-safe firing

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 5: The scrub job: start, watch, resume

**Files:**
- Create: `src/daemon/scrub.rs`
- Modify: `src/daemon/mod.rs`

**Interfaces:**
- Consumes: `Job` (Task 2); `History`, `Event`, `EventKind`, `Run`, `RunResult` (Task 3); existing `zfs::pool::{get_pool_vdevs, scrub_pool, ScanStatus}`.
- Produces, in `crate::daemon::scrub`:
  - `pub trait Clock { fn now(&self) -> DateTime<Utc>; fn sleep(&self, d: Duration); }` and `pub struct SystemClock`
  - `pub struct Locks` (`Clone, Default`) with `try_acquire(&self, host, pool) -> Option<LockGuard>`
  - `pub struct Outcome { pub job_id: String, pub result: RunResult, pub message: String }`
  - `pub struct Context<'a> { pub history: &'a History, pub clock: &'a dyn Clock, pub locks: &'a Locks, pub poll: Duration, pub stop: &'a AtomicBool }`
  - `pub fn run_scrub(ctx: &Context, runner: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Option<Outcome>`
  - `pub fn resume_scrub(ctx: &Context, runner: &dyn CommandRunner, job: &Job, run: &Run) -> Option<Outcome>`

- [ ] **Step 1: Write the failing tests**

Create `src/daemon/scrub.rs` containing ONLY the test module below. Update `src/daemon/mod.rs` to:

```rust
pub mod cron;
pub mod history;
pub mod scheduler;
pub mod scrub;
```

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::history::runs;
    use crate::error::Result;
    use crate::exec::CommandOutput;
    use crate::jobs::KIND_SCRUB;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    const NEVER: &str = "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\ttank  ONLINE  0 0 0\n";
    fn status(scan: &str) -> String {
        format!("  pool: tank\n state: ONLINE\n  scan: {scan}\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\ttank  ONLINE  0 0 0\n")
    }
    fn running() -> String {
        status("scrub in progress since Tue Oct  6 03:00:01 2026\n\t10.0G / 20.0G scanned, 50.00% done, 00:10:00 to go")
    }
    fn done(errors: u32) -> String {
        status(&format!("scrub repaired 0B in 00:20:00 with {errors} errors on Tue Oct  6 03:20:01 2026"))
    }
    fn old_done() -> String {
        status("scrub repaired 0B in 00:20:00 with 0 errors on Sun Sep  6 03:20:01 2026")
    }

    /// Answers `zpool status` from a queue (the last answer repeats) and `zpool scrub` with a fixed result.
    struct Seq {
        statuses: Mutex<VecDeque<std::result::Result<String, String>>>,
        scrub: std::result::Result<(), String>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl Seq {
        fn new(statuses: Vec<std::result::Result<String, String>>, scrub: std::result::Result<(), String>) -> Self {
            Seq { statuses: Mutex::new(statuses.into()), scrub, calls: Mutex::new(Vec::new()) }
        }
    }

    impl CommandRunner for Seq {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(std::iter::once(cmd).chain(args.iter().copied()).map(str::to_string).collect());
            let fail = |stderr: &str| Ok(CommandOutput { stdout: String::new(), stderr: stderr.to_string(), status: 1 });
            let ok = |stdout: String| Ok(CommandOutput { stdout, stderr: String::new(), status: 0 });
            match args.first().copied() {
                Some("status") => {
                    let mut q = self.statuses.lock().unwrap();
                    let next = if q.len() > 1 { q.pop_front().unwrap() } else { q.front().cloned().unwrap() };
                    match next {
                        Ok(text) => ok(text),
                        Err(e) => fail(&e),
                    }
                }
                Some("scrub") => match &self.scrub {
                    Ok(()) => ok(String::new()),
                    Err(e) => fail(e),
                },
                _ => ok(String::new()),
            }
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _stdin: &str) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
    }

    /// A clock that only moves when something sleeps.
    struct FakeClock(Mutex<DateTime<Utc>>);

    impl FakeClock {
        fn new() -> Self {
            FakeClock(Mutex::new(DateTime::parse_from_rfc3339("2026-10-06T03:00:00Z").unwrap().with_timezone(&Utc)))
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
        fn sleep(&self, d: Duration) {
            *self.0.lock().unwrap() += chrono::Duration::from_std(d).unwrap();
        }
    }

    fn job() -> Job {
        Job { id: "scrub-tank".into(), host: "local".into(), kind: KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * *".into(), catch_up: true, enabled: true, created_at: None }
    }

    struct Rig {
        dir: tempfile::TempDir,
        clock: FakeClock,
        locks: Locks,
        stop: AtomicBool,
    }

    impl Rig {
        fn new() -> Self {
            Rig { dir: tempfile::tempdir().unwrap(), clock: FakeClock::new(), locks: Locks::default(), stop: AtomicBool::new(false) }
        }
        fn history(&self) -> History {
            History::new(self.dir.path().join("history.jsonl"))
        }
        fn run(&self, runner: &Seq) -> Option<Outcome> {
            let history = self.history();
            let ctx = Context { history: &history, clock: &self.clock, locks: &self.locks, poll: Duration::from_secs(30), stop: &self.stop };
            run_scrub(&ctx, runner, &job(), self.clock.now(), false, "run-1")
        }
    }

    fn t(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-06T03:00:00Z").unwrap().with_timezone(&Utc) + chrono::Duration::minutes(minutes)
    }

    #[test]
    fn watch_keeps_going_while_in_progress_and_finishes_on_a_new_completion() {
        let mut w = Watch::new(Some("old".into()));
        assert_eq!(w.observe(Observation::InProgress, t(0)), Verdict::Keep);
        let v = w.observe(Observation::Completed { summary: "new".into(), errors: false }, t(1));
        assert_eq!(v, Verdict::Done { ok: true, message: "new".into() });
        let mut w = Watch::new(None);
        assert_eq!(w.observe(Observation::Completed { summary: "x".into(), errors: true }, t(0)), Verdict::Done { ok: false, message: "x".into() });
    }

    #[test]
    fn watch_treats_the_old_completion_line_as_not_finished_until_three_polls_agree() {
        let mut w = Watch::new(Some("old".into()));
        let same = || Observation::Completed { summary: "old".into(), errors: false };
        assert_eq!(w.observe(same(), t(0)), Verdict::Keep);
        assert_eq!(w.observe(same(), t(1)), Verdict::Keep);
        assert_eq!(w.observe(same(), t(2)), Verdict::DidNotStart);
        let mut w = Watch::new(Some("old".into()));
        let _ = w.observe(same(), t(0));
        assert_eq!(w.observe(Observation::InProgress, t(1)), Verdict::Keep, "seeing it run resets the count");
        assert_eq!(w.observe(same(), t(2)), Verdict::Keep);
    }

    #[test]
    fn a_pool_that_never_gets_a_scan_line_is_a_scrub_that_did_not_start() {
        let mut w = Watch::new(None);
        for i in 0..2 {
            assert_eq!(w.observe(Observation::NeverScanned, t(i)), Verdict::Keep);
        }
        assert_eq!(w.observe(Observation::NeverScanned, t(2)), Verdict::DidNotStart);
    }

    #[test]
    fn an_unreachable_host_is_lost_only_after_ten_minutes_and_recovery_resets_the_clock() {
        let mut w = Watch::new(None);
        let down = || Observation::Unreachable("ssh: timeout".into());
        assert_eq!(w.observe(down(), t(0)), Verdict::Keep);
        assert_eq!(w.observe(down(), t(9)), Verdict::Keep);
        assert_eq!(w.observe(Observation::InProgress, t(9)), Verdict::Keep);
        assert_eq!(w.observe(down(), t(15)), Verdict::Keep, "the outage restarted at minute 15");
        assert_eq!(w.observe(down(), t(25)), Verdict::Lost("ssh: timeout".into()));
    }

    #[test]
    fn locks_allow_one_holder_per_pool_and_free_it_on_drop() {
        let locks = Locks::default();
        let a = locks.try_acquire("local", "tank").unwrap();
        assert!(locks.try_acquire("local", "tank").is_none());
        assert!(locks.try_acquire("local", "other").is_some(), "another pool is independent");
        assert!(locks.try_acquire("fbsd16", "tank").is_some(), "another host is independent");
        drop(a);
        assert!(locks.try_acquire("local", "tank").is_some());
    }

    #[test]
    fn a_scrub_that_runs_then_completes_cleanly_records_started_then_ok() {
        let rig = Rig::new();
        let runner = Seq::new(vec![Ok(old_done()), Ok(running()), Ok(running()), Ok(done(0))], Ok(()));
        let out = rig.run(&runner).unwrap();
        assert_eq!((out.result, out.job_id.as_str()), (RunResult::Ok, "scrub-tank"));
        let all = runs(&rig.history().read());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].result, Some(RunResult::Ok));
        assert!(all[0].message.contains("with 0 errors"));
        assert_eq!(all[0].summary.as_deref(), Some("scrub repaired 0B in 00:20:00 with 0 errors on Sun Sep  6 03:20:01 2026"));
        let calls = runner.calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == &["zpool", "scrub", "--", "tank"]), "{calls:?}");
        assert!(rig.locks.try_acquire("local", "tank").is_some(), "the lock is released afterwards");
    }

    #[test]
    fn a_scrub_that_finds_errors_is_a_failed_run_carrying_the_summary() {
        let rig = Rig::new();
        let runner = Seq::new(vec![Ok(NEVER.into()), Ok(running()), Ok(done(3))], Ok(()));
        let out = rig.run(&runner).unwrap();
        assert_eq!(out.result, RunResult::Failed);
        assert!(out.message.contains("with 3 errors"), "{}", out.message);
    }

    #[test]
    fn a_scrub_that_finished_before_the_first_poll_is_still_recognised() {
        let rig = Rig::new();
        let runner = Seq::new(vec![Ok(old_done()), Ok(done(0))], Ok(()));
        assert_eq!(rig.run(&runner).unwrap().result, RunResult::Ok);
    }

    #[test]
    fn a_pool_already_scanning_is_skipped_without_starting_anything() {
        let rig = Rig::new();
        let runner = Seq::new(vec![Ok(running())], Ok(()));
        let out = rig.run(&runner).unwrap();
        assert_eq!(out.result, RunResult::Skipped);
        assert!(out.message.contains("already scanning"));
        assert!(!runner.calls.lock().unwrap().iter().any(|c| c.get(1).is_some_and(|a| a == "scrub")), "no scrub command was sent");
    }

    #[test]
    fn a_scrub_that_races_another_is_skipped_not_failed() {
        let rig = Rig::new();
        let runner = Seq::new(vec![Ok(old_done())], Err("cannot scrub tank: currently scrubbing; use 'zpool scrub -s' to cancel".into()));
        assert_eq!(rig.run(&runner).unwrap().result, RunResult::Skipped);
    }

    #[test]
    fn a_failing_start_or_unreadable_pool_is_a_failed_run_with_the_error() {
        let rig = Rig::new();
        let out = rig.run(&Seq::new(vec![Err("cannot open 'tank': no such pool".into())], Ok(()))).unwrap();
        assert_eq!(out.result, RunResult::Failed);
        assert!(out.message.contains("no such pool"), "{}", out.message);
        let out = rig.run(&Seq::new(vec![Ok(old_done())], Err("permission denied".into()))).unwrap();
        assert_eq!(out.result, RunResult::Failed);
    }

    #[test]
    fn a_held_lock_skips_the_run() {
        let rig = Rig::new();
        let _held = rig.locks.try_acquire("local", "tank").unwrap();
        let out = rig.run(&Seq::new(vec![Ok(old_done())], Ok(()))).unwrap();
        assert_eq!(out.result, RunResult::Skipped);
        assert!(out.message.contains("already running"));
    }

    #[test]
    fn the_run_gives_up_as_lost_after_ten_minutes_of_an_unreachable_host() {
        let rig = Rig::new();
        let runner = Seq::new(vec![Ok(old_done()), Err("ssh: connect timed out".into())], Ok(()));
        let out = rig.run(&runner).unwrap();
        assert_eq!(out.result, RunResult::Failed);
        assert!(out.message.starts_with("lost:"), "{}", out.message);
    }

    #[test]
    fn shutdown_mid_watch_records_nothing_more_so_a_restart_can_resume() {
        let rig = Rig::new();
        rig.stop.store(true, Ordering::SeqCst);
        let runner = Seq::new(vec![Ok(old_done()), Ok(running())], Ok(()));
        assert!(rig.run(&runner).is_none());
        let all = runs(&rig.history().read());
        assert_eq!((all.len(), all[0].result), (1, None), "started, never finished: still running");
    }

    #[test]
    fn resume_finishes_a_run_using_the_summary_recorded_at_its_start() {
        let rig = Rig::new();
        let history = rig.history();
        let ctx = Context { history: &history, clock: &rig.clock, locks: &rig.locks, poll: Duration::from_secs(30), stop: &rig.stop };
        let run = Run {
            job_id: "scrub-tank".into(),
            run_id: "run-1".into(),
            started_at: t(0),
            scheduled_for: Some(t(0)),
            finished_at: None,
            result: None,
            message: String::new(),
            catch_up: false,
            summary: Some("scrub repaired 0B in 00:20:00 with 0 errors on Sun Sep  6 03:20:01 2026".into()),
        };
        // Still showing the old completion: not finished. Then a new one: finished.
        let runner = Seq::new(vec![Ok(old_done()), Ok(done(0))], Ok(()));
        let out = resume_scrub(&ctx, &runner, &job(), &run).unwrap();
        assert_eq!(out.result, RunResult::Ok);
        let all = runs(&history.read());
        assert_eq!((all.len(), all[0].run_id.as_str(), all[0].result), (1, "run-1", Some(RunResult::Ok)));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test daemon::scrub 2>&1 | grep -E "^error" -A4 | head -20`
Expected: FAIL to compile with `cannot find ...` errors for the items defined in Step 3.

- [ ] **Step 3: Write the implementation**

Put this at the TOP of `src/daemon/scrub.rs`, above the test module:

```rust
//! The scrub job: start `zpool scrub`, then watch `zpool status` until the
//! scrub finishes, recording each step in the history.

use crate::daemon::history::{Event, EventKind, History, Run, RunResult};
use crate::error::AppError;
use crate::exec::CommandRunner;
use crate::jobs::Job;
use crate::zfs::pool::{get_pool_vdevs, scrub_pool, ScanStatus};
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// How long a host may stay unreachable while a scrub is being watched before
/// the run is given up on.
const LOST_AFTER: chrono::Duration = chrono::Duration::minutes(10);
/// Polls that must show the old completion summary before we conclude the scrub never started.
const DID_NOT_START_POLLS: u32 = 3;

pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
    fn sleep(&self, d: Duration);
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// One look at the pool's scan state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    InProgress,
    Completed { summary: String, errors: bool },
    NeverScanned,
    Unreachable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    Done { ok: bool, message: String },
    DidNotStart,
    Lost(String),
}

/// Decides, poll by poll, whether a scrub we started has finished.
pub struct Watch {
    /// The scan summary before the scrub began, if the pool had one.
    before: Option<String>,
    unchanged: u32,
    unreachable_since: Option<DateTime<Utc>>,
}

impl Watch {
    pub fn new(before: Option<String>) -> Self {
        Watch { before, unchanged: 0, unreachable_since: None }
    }

    pub fn observe(&mut self, obs: Observation, now: DateTime<Utc>) -> Verdict {
        if let Observation::Unreachable(why) = &obs {
            let since = *self.unreachable_since.get_or_insert(now);
            return if now - since >= LOST_AFTER { Verdict::Lost(why.clone()) } else { Verdict::Keep };
        }
        self.unreachable_since = None;
        match obs {
            Observation::InProgress => {
                self.unchanged = 0;
                Verdict::Keep
            }
            // The completion line carries a timestamp, so a new scrub's differs from the old one's
            // even when it finished before our first poll.
            Observation::Completed { summary, errors } if self.before.as_ref() != Some(&summary) => Verdict::Done { ok: !errors, message: summary },
            Observation::Completed { .. } | Observation::NeverScanned => {
                self.unchanged += 1;
                if self.unchanged >= DID_NOT_START_POLLS { Verdict::DidNotStart } else { Verdict::Keep }
            }
            Observation::Unreachable(_) => Verdict::Keep,
        }
    }
}

/// At most one job per `(host, pool)` at a time.
#[derive(Clone, Default)]
pub struct Locks(Arc<Mutex<HashSet<(String, String)>>>);

pub struct LockGuard {
    locks: Locks,
    key: (String, String),
}

impl Locks {
    pub fn try_acquire(&self, host: &str, pool: &str) -> Option<LockGuard> {
        let key = (host.to_string(), pool.to_string());
        let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        held.insert(key.clone()).then(|| LockGuard { locks: self.clone(), key })
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        self.locks.0.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.key);
    }
}

/// What a finished run reports to the caller (which decides about notifying).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub job_id: String,
    pub result: RunResult,
    pub message: String,
}

pub struct Context<'a> {
    pub history: &'a History,
    pub clock: &'a dyn Clock,
    pub locks: &'a Locks,
    pub poll: Duration,
    /// Set at shutdown: a watcher returns without recording, so a restart re-attaches.
    pub stop: &'a AtomicBool,
}

fn observe(runner: &dyn CommandRunner, pool: &str) -> Observation {
    match get_pool_vdevs(runner, pool) {
        Ok((_, Some(ScanStatus::InProgress { .. }))) => Observation::InProgress,
        Ok((_, Some(ScanStatus::Completed { summary, errors }))) => Observation::Completed { summary, errors },
        Ok((_, None)) => Observation::NeverScanned,
        Err(e) => Observation::Unreachable(e.to_string()),
    }
}

fn event(job: &Job, run_id: &str, kind: EventKind, at: DateTime<Utc>) -> Event {
    Event { job_id: job.id.clone(), run_id: run_id.to_string(), event: kind, at, scheduled_for: None, result: None, message: String::new(), catch_up: false, summary: None }
}

const NO_SLOT: Slot = Slot { scheduled_for: None, catch_up: false };

/// The scheduled slot a run is for, recorded on its closing event.
#[derive(Clone, Copy)]
struct Slot {
    scheduled_for: Option<DateTime<Utc>>,
    catch_up: bool,
}

/// Records the end of a run and returns what to report.
fn finish(ctx: &Context, job: &Job, run_id: &str, kind: EventKind, result: RunResult, message: String, slot: Slot) -> Outcome {
    let mut e = event(job, run_id, kind, ctx.clock.now());
    e.result = Some(result);
    e.message.clone_from(&message);
    e.scheduled_for = slot.scheduled_for;
    e.catch_up = slot.catch_up;
    let _ = ctx.history.append(&e);
    Outcome { job_id: job.id.clone(), result, message }
}

fn watch(ctx: &Context, runner: &dyn CommandRunner, job: &Job, pool: &str, run_id: &str, mut watch: Watch) -> Option<Outcome> {
    loop {
        ctx.clock.sleep(ctx.poll);
        if ctx.stop.load(Ordering::SeqCst) {
            return None;
        }
        match watch.observe(observe(runner, pool), ctx.clock.now()) {
            Verdict::Keep => {}
            Verdict::Done { ok, message } => {
                return Some(finish(ctx, job, run_id, EventKind::Finished, if ok { RunResult::Ok } else { RunResult::Failed }, message, NO_SLOT));
            }
            Verdict::DidNotStart => return Some(finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, "scrub did not start".to_string(), NO_SLOT)),
            Verdict::Lost(why) => return Some(finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, format!("lost: host unreachable for 10 minutes ({why})"), NO_SLOT)),
        }
    }
}

/// Runs one scheduled scrub from start to finish. `None` means the daemon is
/// shutting down mid-watch and nothing further was recorded.
pub fn run_scrub(ctx: &Context, runner: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Option<Outcome> {
    let pool = job.pool.clone().unwrap_or_default();
    let slot = Slot { scheduled_for: Some(scheduled_for), catch_up };
    let Some(_guard) = ctx.locks.try_acquire(&job.host, &pool) else {
        return Some(finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, "skipped: already running".to_string(), slot));
    };
    let before = match get_pool_vdevs(runner, &pool) {
        Err(e) => return Some(finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, e.to_string(), slot)),
        Ok((_, Some(ScanStatus::InProgress { .. }))) => {
            return Some(finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, "skipped: already scanning".to_string(), slot));
        }
        Ok((_, Some(ScanStatus::Completed { summary, .. }))) => Some(summary),
        Ok((_, None)) => None,
    };
    match scrub_pool(runner, &pool) {
        Ok(()) => {}
        // A scrub or resilver that began between our check and our command.
        Err(AppError::CommandFailed { stderr, .. }) if stderr.contains("currently scrubbing") || stderr.contains("currently resilvering") => {
            return Some(finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, "skipped: already scanning".to_string(), slot));
        }
        Err(e) => return Some(finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, e.to_string(), slot)),
    }
    let mut started = event(job, run_id, EventKind::Started, ctx.clock.now());
    started.scheduled_for = Some(scheduled_for);
    started.catch_up = catch_up;
    started.summary.clone_from(&before);
    let _ = ctx.history.append(&started);
    watch(ctx, runner, job, &pool, run_id, Watch::new(before))
}

/// Picks up a scrub this daemon (or a previous one) started and never saw finish.
pub fn resume_scrub(ctx: &Context, runner: &dyn CommandRunner, job: &Job, run: &Run) -> Option<Outcome> {
    let pool = job.pool.clone().unwrap_or_default();
    let _guard = ctx.locks.try_acquire(&job.host, &pool);
    watch(ctx, runner, job, &pool, &run.run_id, Watch::new(run.summary.clone()))
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test daemon::scrub 2>&1 | grep -E "^error|^test result|FAILED|panicked" -A8`
Expected: all tests PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "src/daemon/scrub.rs"`
Expected: only `never used` dead-code warnings for items a later task wires up; nothing else.

```bash
git add src/daemon/scrub.rs
git commit -m "feat: add the scrub job with a restart-safe completion watcher

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 6: The notify hook

**Files:**
- Create: `src/daemon/notify.rs`
- Modify: `src/daemon/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces, in `crate::daemon::notify`:
  - `pub const TIMEOUT: Duration` (30 seconds)
  - `pub struct Notice { pub job, pub host, pub kind, pub result, pub message: String }`
  - `pub enum NotifyError { Spawn(String), TimedOut, Exit(i32) }` (`Display`)
  - `pub fn run(command: &str, notice: &Notice, timeout: Duration) -> Result<(), NotifyError>`

- [ ] **Step 1: Write the failing tests**

Create `src/daemon/notify.rs` containing ONLY the test module below. Update `src/daemon/mod.rs` to:

```rust
pub mod cron;
pub mod history;
pub mod notify;
pub mod scheduler;
pub mod scrub;
```

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn notice(message: &str) -> Notice {
        Notice { job: "scrub-tank".into(), host: "local".into(), kind: "scrub".into(), result: "failed".into(), message: message.into() }
    }

    #[test]
    fn the_notice_arrives_in_environment_variables() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let cmd = format!("printf '%s|%s|%s|%s|%s' \"$ZFSCTL_JOB\" \"$ZFSCTL_HOST\" \"$ZFSCTL_KIND\" \"$ZFSCTL_RESULT\" \"$ZFSCTL_MESSAGE\" > {}", out.display());
        run(&cmd, &notice("2 errors"), TIMEOUT).unwrap();
        assert_eq!(std::fs::read_to_string(out).unwrap(), "scrub-tank|local|scrub|failed|2 errors");
    }

    #[test]
    fn job_data_is_never_executed_as_part_of_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let canary = dir.path().join("pwned");
        let evil = format!("$(touch {}); `touch {}`", canary.display(), canary.display());
        run(&format!("printf '%s' \"$ZFSCTL_MESSAGE\" > {}", out.display()), &notice(&evil), TIMEOUT).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), evil, "the message is passed through verbatim");
        assert!(!canary.exists(), "and was not run");
    }

    #[test]
    fn a_command_that_outlives_the_timeout_is_killed() {
        let started = Instant::now();
        let err = run("exec sleep 30", &notice("x"), Duration::from_millis(300)).unwrap_err();
        assert_eq!(err, NotifyError::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5), "returned promptly, not after 30s");
    }

    #[test]
    fn a_failing_command_reports_its_status_and_a_missing_one_is_a_failure_too() {
        assert_eq!(run("exit 7", &notice("x"), TIMEOUT).unwrap_err(), NotifyError::Exit(7));
        assert_eq!(run("/nonexistent/notify-me", &notice("x"), TIMEOUT).unwrap_err(), NotifyError::Exit(127));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test daemon::notify 2>&1 | grep -E "^error" -A4 | head -20`
Expected: FAIL to compile with `cannot find ...` errors for the items defined in Step 3.

- [ ] **Step 3: Write the implementation**

Put this at the TOP of `src/daemon/notify.rs`, above the test module:

```rust
//! The operator's notify command: run when a job fails.
//!
//! The command is configured by the operator in `config.toml` and run through
//! `sh -c`. Job data reaches it only through environment variables, never
//! through the command line, so nothing in a job's message can be executed.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub job: String,
    pub host: String,
    pub kind: String,
    pub result: String,
    pub message: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum NotifyError {
    Spawn(String),
    TimedOut,
    Exit(i32),
}

impl std::fmt::Display for NotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotifyError::Spawn(e) => write!(f, "could not start: {e}"),
            NotifyError::TimedOut => write!(f, "timed out"),
            NotifyError::Exit(code) => write!(f, "exited with status {code}"),
        }
    }
}

/// Runs `command` with the notice in its environment and waits up to `timeout`.
/// Its output goes to the daemon's own stdout/stderr, which the rc script
/// sends to the log. A command that outlives the timeout is killed.
pub fn run(command: &str, notice: &Notice, timeout: Duration) -> Result<(), NotifyError> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("ZFSCTL_JOB", &notice.job)
        .env("ZFSCTL_HOST", &notice.host)
        .env("ZFSCTL_KIND", &notice.kind)
        .env("ZFSCTL_RESULT", &notice.result)
        .env("ZFSCTL_MESSAGE", &notice.message)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| NotifyError::Spawn(e.to_string()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(NotifyError::Exit(status.code().unwrap_or(-1))),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(NotifyError::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(NotifyError::Spawn(e.to_string())),
        }
    }
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test daemon::notify 2>&1 | grep -E "^error|^test result|FAILED|panicked" -A8`
Expected: all tests PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "src/daemon/notify.rs"`
Expected: only `never used` dead-code warnings for items a later task wires up; nothing else.

```bash
git add src/daemon/notify.rs
git commit -m "feat: add the notify command hook, with job data only in the environment

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 7: The engine: reload, catch-up, resume, prune

**Files:**
- Create: `src/daemon/engine.rs`
- Modify: `src/daemon/mod.rs`

**Interfaces:**
- Consumes: `jobs::{load, resolve, Job}` (Task 2); `History`, `runs`, `last_scheduled`, `Run` (Task 3); `Scheduler`, `Due`, `Unsupported` (Task 4).
- Produces, in `crate::daemon::engine`:
  - `pub const KEEP_RUNS: usize = 500`, `pub const DEFAULT_STATE_DIR: &str = "/var/db/zfsctl"`
  - `pub struct Paths { pub dir: PathBuf }` with `new`, `jobs()`, `history()`, `heartbeat()`, `ensure_dir()`
  - `pub enum Work { Run(Due), Resume { job: Job, run: Run }, Orphan(Run) }`
  - `pub struct Engine<Tz: TimeZone>` with `new(paths, tz, hosts: Vec<String>, now)`, `start(now) -> Vec<Work>`, `step(now) -> Vec<Work>`, and public fields `unsupported: Vec<Unsupported>`, `load_error: Option<String>`

- [ ] **Step 1: Write the failing tests**

Create `src/daemon/engine.rs` containing ONLY the test module below. Update `src/daemon/mod.rs` to:

```rust
pub mod cron;
pub mod engine;
pub mod history;
pub mod notify;
pub mod scheduler;
pub mod scrub;
```

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::history::{Event, EventKind, RunResult};
    use crate::jobs::KIND_SCRUB;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn job(id: &str, schedule: &str) -> Job {
        Job { id: id.into(), host: "local".into(), kind: KIND_SCRUB.into(), pool: Some("tank".into()), schedule: schedule.into(), catch_up: true, enabled: true, created_at: Some(at("2026-09-01T00:00:00Z")) }
    }

    fn engine(dir: &tempfile::TempDir, now: &str) -> Engine<Utc> {
        Engine::new(Paths::new(dir.path()), Utc, vec!["local".to_string()], at(now))
    }

    fn started(job_id: &str, run_id: &str, when: &str) -> Event {
        Event { job_id: job_id.into(), run_id: run_id.into(), event: EventKind::Started, at: at(when), scheduled_for: Some(at(when)), result: None, message: String::new(), catch_up: false, summary: Some("old".into()) }
    }

    #[test]
    fn paths_live_under_one_private_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path().join("zfsctl"));
        p.ensure_dir().unwrap();
        p.ensure_dir().unwrap();
        assert_eq!(std::fs::metadata(&p.dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!((p.jobs().file_name().unwrap(), p.history().file_name().unwrap(), p.heartbeat().file_name().unwrap()), ("jobs.toml".as_ref(), "history.jsonl".as_ref(), "daemon.json".as_ref()));
    }

    #[test]
    fn start_catches_up_a_missed_slot_once_and_a_second_start_does_not_repeat_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        jobs::save(&paths.jobs(), &[job("j", "0 3 * * 0")]).unwrap();
        let work = engine(&dir, "2026-10-07T12:00:00Z").start(at("2026-10-07T12:00:00Z"));
        let [Work::Run(due)] = work.as_slice() else { panic!("{work:?}") };
        assert_eq!((due.scheduled_for, due.catch_up), (at("2026-10-04T03:00:00Z"), true));
        // The runtime records the catch-up run; the next start sees that slot as handled.
        let h = History::new(paths.history());
        let mut done = started("j", "r1", "2026-10-04T03:00:00Z");
        done.event = EventKind::Finished;
        done.result = Some(RunResult::Ok);
        h.append(&done).unwrap();
        assert!(engine(&dir, "2026-10-07T12:05:00Z").start(at("2026-10-07T12:05:00Z")).is_empty());
    }

    #[test]
    fn start_resumes_an_unfinished_run_and_orphans_one_whose_job_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        jobs::save(&paths.jobs(), &[job("j", "0 3 * * *")]).unwrap();
        let h = History::new(paths.history());
        h.append(&started("j", "r1", "2026-10-07T03:00:00Z")).unwrap();
        h.append(&started("deleted", "r2", "2026-10-07T03:00:00Z")).unwrap();
        let work = engine(&dir, "2026-10-07T04:00:00Z").start(at("2026-10-07T04:00:00Z"));
        assert!(work.iter().any(|w| matches!(w, Work::Resume { job, run } if job.id == "j" && run.run_id == "r1")), "{work:?}");
        assert!(work.iter().any(|w| matches!(w, Work::Orphan(run) if run.run_id == "r2")), "{work:?}");
    }

    #[test]
    fn step_fires_due_jobs_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = job("j", "0 3 * * *");
        j.created_at = Some(at("2026-10-07T01:00:00Z"));
        jobs::save(&Paths::new(dir.path()).jobs(), &[j]).unwrap();
        let mut e = engine(&dir, "2026-10-07T02:00:00Z");
        assert!(e.start(at("2026-10-07T02:00:00Z")).is_empty());
        assert!(e.step(at("2026-10-07T02:59:59Z")).is_empty());
        assert_eq!(e.step(at("2026-10-07T03:00:00Z")).len(), 1);
        assert!(e.step(at("2026-10-07T03:00:01Z")).is_empty());
        assert_eq!(e.next_time("j"), Some(at("2026-10-08T03:00:00Z")));
    }

    #[test]
    fn step_picks_up_an_edited_jobs_file_and_a_bad_edit_keeps_the_old_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let mut e = engine(&dir, "2026-10-07T02:00:00Z");
        assert!(e.start(at("2026-10-07T02:00:00Z")).is_empty());
        jobs::save(&paths.jobs(), &[job("j", "0 3 * * *")]).unwrap();
        assert!(e.step(at("2026-10-07T02:10:00Z")).is_empty());
        assert_eq!(e.next_time("j"), Some(at("2026-10-07T03:00:00Z")), "the new job is scheduled");
        std::fs::write(paths.jobs(), "this is [[[ not toml").unwrap();
        // A different mtime is needed to notice the change; bump it explicitly.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(paths.jobs()).unwrap().set_modified(later).unwrap();
        assert_eq!(e.step(at("2026-10-07T03:00:00Z")).len(), 1, "the previous job set still fires");
        assert!(e.load_error.as_deref().is_some_and(|m| m.contains("parsing")), "{:?}", e.load_error);
    }

    #[test]
    fn unsupported_jobs_are_reported_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut bad = job("bad", "0 3 * * *");
        bad.host = "gone".into();
        jobs::save(&Paths::new(dir.path()).jobs(), &[bad]).unwrap();
        let mut e = engine(&dir, "2026-10-07T02:00:00Z");
        assert!(e.start(at("2026-10-07T04:00:00Z")).is_empty());
        assert_eq!(e.unsupported.len(), 1);
        assert!(e.unsupported[0].reason.contains("'gone'"));
    }

    #[test]
    fn step_prunes_the_history_once_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let h = History::new(paths.history());
        for i in 0..(KEEP_RUNS + 5) {
            let mut e = started("j", &format!("r{i:04}"), "2026-10-01T03:00:00Z");
            e.event = EventKind::Finished;
            e.result = Some(RunResult::Ok);
            h.append(&e).unwrap();
        }
        let mut e = engine(&dir, "2026-10-07T00:00:00Z");
        e.last_prune = at("2026-10-07T00:00:00Z");
        let _ = e.step(at("2026-10-07T12:00:00Z"));
        assert_eq!(history::runs(&h.read()).len(), KEEP_RUNS + 5, "not yet a day");
        let _ = e.step(at("2026-10-08T00:00:01Z"));
        assert_eq!(history::runs(&h.read()).len(), KEEP_RUNS);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test daemon::engine 2>&1 | grep -E "^error" -A4 | head -20`
Expected: FAIL to compile with `cannot find ...` errors for the items defined in Step 3.

- [ ] **Step 3: Write the implementation**

Put this at the TOP of `src/daemon/engine.rs`, above the test module:

```rust
//! The daemon's brain, without threads or sleeping: given the clock it says
//! what work to start. The runtime loop in `mod.rs` executes that work.

use crate::daemon::history::{self, History, Run};
use crate::daemon::scheduler::{Due, Scheduler, Unsupported};
use crate::jobs::{self, Job};
use chrono::{DateTime, Duration, TimeZone, Utc};
use std::path::PathBuf;
use std::time::SystemTime;

/// Runs kept per job when the history is pruned.
pub const KEEP_RUNS: usize = 500;
const PRUNE_EVERY: Duration = Duration::hours(24);

/// Where the daemon keeps its state.
#[derive(Debug, Clone)]
pub struct Paths {
    pub dir: PathBuf,
}

pub const DEFAULT_STATE_DIR: &str = "/var/db/zfsctl";

impl Paths {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Paths { dir: dir.into() }
    }
    pub fn jobs(&self) -> PathBuf {
        self.dir.join("jobs.toml")
    }
    pub fn history(&self) -> PathBuf {
        self.dir.join("history.jsonl")
    }
    pub fn heartbeat(&self) -> PathBuf {
        self.dir.join("daemon.json")
    }

    /// Creates the state directory, readable by root only.
    pub fn ensure_dir(&self) -> std::io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir)?;
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))
    }
}

/// One piece of work for the runtime to start on its own thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Work {
    /// A scheduled (or catch-up) run.
    Run(Due),
    /// A scrub started earlier and never seen to finish: keep watching it.
    Resume { job: Job, run: Run },
    /// A running run whose job no longer exists or can no longer run.
    Orphan(Run),
}

pub struct Engine<Tz: TimeZone> {
    paths: Paths,
    hosts: Vec<String>,
    scheduler: Scheduler<Tz>,
    jobs_mtime: Option<SystemTime>,
    last_prune: DateTime<Utc>,
    /// Jobs that cannot run, with the reason, for the log and the TUI.
    pub unsupported: Vec<Unsupported>,
    /// The last problem reading `jobs.toml`, if the file is currently bad.
    pub load_error: Option<String>,
}

fn mtime(paths: &Paths) -> Option<SystemTime> {
    std::fs::metadata(paths.jobs()).and_then(|m| m.modified()).ok()
}

impl<Tz: TimeZone> Engine<Tz> {
    pub fn new(paths: Paths, tz: Tz, hosts: Vec<String>, now: DateTime<Utc>) -> Self {
        Engine { paths, hosts, scheduler: Scheduler::new(tz), jobs_mtime: None, last_prune: now, unsupported: Vec::new(), load_error: None }
    }

    /// Reads `jobs.toml` and swaps the job set in. A bad file keeps the job
    /// set already in place (it never empties the schedule). Returns the jobs
    /// that were read so the caller can look runs up by id.
    fn reload(&mut self, now: DateTime<Utc>, last_scheduled: &std::collections::HashMap<String, DateTime<Utc>>) -> Vec<Job> {
        self.jobs_mtime = mtime(&self.paths);
        match jobs::load(&self.paths.jobs()) {
            Ok(list) => {
                self.load_error = None;
                self.unsupported = self.scheduler.reload(list.clone(), &self.hosts, last_scheduled, now);
                list
            }
            Err(e) => {
                self.load_error = Some(e.to_string());
                Vec::new()
            }
        }
    }

    fn last_scheduled(&self) -> std::collections::HashMap<String, DateTime<Utc>> {
        history::last_scheduled(&history::runs(&History::new(self.paths.history()).read()))
    }

    /// Everything to do when the daemon starts: re-attach to unfinished runs
    /// and catch up on missed slots.
    pub fn start(&mut self, now: DateTime<Utc>) -> Vec<Work> {
        let _ = History::new(self.paths.history()).prune(KEEP_RUNS);
        let last = self.last_scheduled();
        let loaded = self.reload(now, &last);
        let mut work = Vec::new();
        let unfinished: Vec<Run> = history::runs(&History::new(self.paths.history()).read()).into_iter().filter(|r| r.result.is_none()).collect();
        for run in unfinished {
            match loaded.iter().find(|j| j.id == run.job_id && jobs::resolve(j, &self.hosts).is_ok()) {
                Some(job) => work.push(Work::Resume { job: job.clone(), run }),
                None => work.push(Work::Orphan(run)),
            }
        }
        work.extend(self.scheduler.catch_up(now).into_iter().map(Work::Run));
        work
    }

    /// One tick: pick up an edited `jobs.toml`, fire what is due, prune daily.
    pub fn step(&mut self, now: DateTime<Utc>) -> Vec<Work> {
        let current = mtime(&self.paths);
        if current != self.jobs_mtime {
            let last = self.last_scheduled();
            self.reload(now, &last);
        }
        if now - self.last_prune >= PRUNE_EVERY {
            self.last_prune = now;
            let _ = History::new(self.paths.history()).prune(KEEP_RUNS);
        }
        self.scheduler.tick(now).into_iter().map(Work::Run).collect()
    }

    #[cfg(test)]
    pub fn next_time(&self, id: &str) -> Option<DateTime<Utc>> {
        self.scheduler.next_time(id)
    }
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test daemon::engine 2>&1 | grep -E "^error|^test result|FAILED|panicked" -A8`
Expected: all tests PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "src/daemon/engine.rs"`
Expected: only `never used` dead-code warnings for items a later task wires up; nothing else.

```bash
git add src/daemon/engine.rs
git commit -m "feat: add the engine that turns time and files into work

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 8: The heartbeat and single-instance check

**Files:**
- Create: `src/daemon/heartbeat.rs`
- Modify: `src/daemon/mod.rs`

**Interfaces:**
- Consumes: nothing (uses `nix::sys::signal::kill`, enabled by the `signal` feature added in Task 1).
- Produces, in `crate::daemon::heartbeat`:
  - `pub const STALE_AFTER: chrono::Duration` (10 seconds)
  - `pub struct Heartbeat { pub pid: u32, pub started_at: DateTime<Utc>, pub last_tick: DateTime<Utc> }` with `is_fresh(&self, now) -> bool`
  - `pub fn write(path: &Path, h: &Heartbeat) -> io::Result<()>` (atomic), `pub fn read(path: &Path) -> Option<Heartbeat>`
  - `pub fn live_other_instance(h: Option<&Heartbeat>, now, my_pid: u32, pid_alive: impl Fn(u32) -> bool) -> Option<u32>`
  - `pub fn process_alive(pid: u32) -> bool`

- [ ] **Step 1: Write the failing tests**

Create `src/daemon/heartbeat.rs` containing ONLY the test module below. Update `src/daemon/mod.rs` to:

```rust
pub mod cron;
pub mod engine;
pub mod heartbeat;
pub mod history;
pub mod notify;
pub mod scheduler;
pub mod scrub;
```

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn beat(pid: u32, last: &str) -> Heartbeat {
        Heartbeat { pid, started_at: at("2026-10-07T00:00:00Z"), last_tick: at(last) }
    }

    #[test]
    fn a_heartbeat_round_trips_through_its_file_and_a_missing_or_corrupt_file_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        assert!(read(&path).is_none());
        let b = beat(42, "2026-10-07T00:00:05Z");
        write(&path, &b).unwrap();
        assert_eq!(read(&path), Some(b));
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::write(&path, "{ not json").unwrap();
        assert!(read(&path).is_none());
    }

    #[test]
    fn a_heartbeat_goes_stale_after_ten_seconds() {
        let b = beat(1, "2026-10-07T00:00:00Z");
        assert!(b.is_fresh(at("2026-10-07T00:00:09Z")));
        assert!(!b.is_fresh(at("2026-10-07T00:00:10Z")));
    }

    #[test]
    fn another_instance_needs_a_fresh_heartbeat_from_a_live_process_that_is_not_us() {
        let now = at("2026-10-07T00:00:05Z");
        let fresh = beat(7, "2026-10-07T00:00:03Z");
        assert_eq!(live_other_instance(Some(&fresh), now, 99, |_| true), Some(7));
        assert_eq!(live_other_instance(Some(&fresh), now, 7, |_| true), None, "our own pid");
        assert_eq!(live_other_instance(Some(&fresh), now, 99, |_| false), None, "crashed: a supervisor may restart at once");
        assert_eq!(live_other_instance(Some(&beat(7, "2026-10-07T00:00:00Z")), at("2026-10-07T00:01:00Z"), 99, |_| true), None, "stale");
        assert_eq!(live_other_instance(None, now, 99, |_| true), None);
    }

    #[test]
    fn process_alive_is_true_for_this_process_and_false_for_one_that_has_exited() {
        assert!(process_alive(std::process::id()));
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!process_alive(pid));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test daemon::heartbeat 2>&1 | grep -E "^error" -A4 | head -20`
Expected: FAIL to compile with `cannot find ...` errors for the items defined in Step 3.

- [ ] **Step 3: Write the implementation**

Put this at the TOP of `src/daemon/heartbeat.rs`, above the test module:

```rust
//! `daemon.json`: proof the daemon is alive, rewritten every tick. The TUI
//! reads it to show whether jobs will fire, and a second daemon reads it to
//! refuse to start.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// A heartbeat older than this means the daemon is not running.
pub const STALE_AFTER: Duration = Duration::seconds(10);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub last_tick: DateTime<Utc>,
}

impl Heartbeat {
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        now - self.last_tick < STALE_AFTER
    }
}

/// Writes the heartbeat atomically, so a reader never sees half of it.
pub fn write(path: &Path, heartbeat: &Heartbeat) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string(heartbeat).map_err(std::io::Error::other)?;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    file.write_all(text.as_bytes())?;
    std::fs::rename(&tmp, path)
}

/// `None` when the file is missing or unreadable.
pub fn read(path: &Path) -> Option<Heartbeat> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The pid of another daemon that is alive: a fresh heartbeat from a process
/// that still exists and is not us. A crashed daemon's heartbeat can still be
/// fresh for a few seconds, so the process check is what lets a supervisor
/// restart it immediately.
pub fn live_other_instance(heartbeat: Option<&Heartbeat>, now: DateTime<Utc>, my_pid: u32, pid_alive: impl Fn(u32) -> bool) -> Option<u32> {
    heartbeat.filter(|h| h.pid != my_pid && h.is_fresh(now) && pid_alive(h.pid)).map(|h| h.pid)
}

/// Whether a process with this pid exists. Signal 0 only checks; `EPERM` means it exists but is not ours.
pub fn process_alive(pid: u32) -> bool {
    use nix::errno::Errno;
    let Ok(raw) = i32::try_from(pid) else { return false };
    matches!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None), Ok(()) | Err(Errno::EPERM))
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test daemon::heartbeat 2>&1 | grep -E "^error|^test result|FAILED|panicked" -A8`
Expected: all tests PASS.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "src/daemon/heartbeat.rs"`
Expected: only `never used` dead-code warnings for items a later task wires up; nothing else.

```bash
git add src/daemon/heartbeat.rs
git commit -m "feat: add the daemon heartbeat and single-instance check

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 9: The daemon runtime, `zfsctl daemon`, config, rc script, README, live check

**Files:**
- Modify: `src/daemon/mod.rs` (the runtime), `src/main.rs`, `src/config.rs`, `README.md`
- Create: `contrib/rc.d/zfsctl`

**Interfaces:**
- Consumes: everything above: `Engine`, `Paths`, `Work` (Task 7); `scrub::{Clock, Context, Locks, Outcome, SystemClock, run_scrub, resume_scrub}` (Task 5); `notify::{Notice, run, TIMEOUT}` (Task 6); `heartbeat` (Task 8); existing `HostInventory`.
- Produces: `crate::daemon::run(inventory: &HostInventory, notify_command: Option<String>, paths: Paths, stop: &Arc<AtomicBool>) -> Result<()>`; the `zfsctl daemon [--state-dir DIR]` command; `Config.daemon.notify_command`.

- [ ] **Step 1: Write the failing tests**

In `src/config.rs`, add this test at the end of its `mod tests` (and add `..Default::default()` after the `ui: UiConfig { theme: Some("mono".to_string()), auto_refresh_secs: None },` line in `ui_theme_survives_a_save_load_round_trip_with_hosts`, because `Config` gains a field in Step 3):

```rust
    #[test]
    fn daemon_notify_command_parses_and_is_only_written_back_when_set() {
        let config: Config = toml::from_str("[daemon]\nnotify_command = \"curl -s ntfy.sh/zfs\"\n").unwrap();
        assert_eq!(config.daemon.notify_command.as_deref(), Some("curl -s ntfy.sh/zfs"));
        assert!(toml::to_string_pretty(&config).unwrap().contains("notify_command"));
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!text.contains("daemon") && !text.contains("notify"), "{text}");
    }
```

In `src/main.rs`, add these tests inside its `mod tests` (the `args` helper first):

```rust
    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_arguments_start_the_tui_and_daemon_takes_an_optional_state_dir() {
        assert_eq!(parse_args(&[]).unwrap(), Command::Tui);
        assert_eq!(parse_args(&args(&["daemon"])).unwrap(), Command::Daemon { state_dir: PathBuf::from("/var/db/zfsctl") });
        assert_eq!(parse_args(&args(&["daemon", "--state-dir", "/tmp/x"])).unwrap(), Command::Daemon { state_dir: PathBuf::from("/tmp/x") });
    }

    #[test]
    fn anything_else_is_a_usage_error() {
        for bad in [&["bogus"][..], &["daemon", "--state-dir"], &["daemon", "--wat", "x"], &["daemon", "x", "y", "z"]] {
            let err = parse_args(&args(bad)).unwrap_err().to_string();
            assert!(err.contains("usage: zfsctl"), "{bad:?}: {err}");
        }
    }
```

In `src/daemon/mod.rs`, append this test module at the end of the file (keep the existing `pub mod` lines for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;
    use crate::jobs::{Job, KIND_SCRUB};
    use scheduler::Due;
    use std::sync::Mutex;

    struct FixedClock(Mutex<chrono::DateTime<Utc>>);

    impl Clock for FixedClock {
        fn now(&self) -> chrono::DateTime<Utc> {
            *self.0.lock().unwrap()
        }
        fn sleep(&self, d: Duration) {
            *self.0.lock().unwrap() += chrono::Duration::from_std(d).unwrap();
        }
    }

    fn job() -> Job {
        Job { id: "scrub-tank".into(), host: "local".into(), kind: KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * *".into(), catch_up: true, enabled: true, created_at: None }
    }

    fn shared(dir: &tempfile::TempDir, runner: FakeRunner, notify: Option<String>) -> Shared {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-07T03:00:00Z").unwrap().with_timezone(&Utc);
        Shared {
            history: History::new(dir.path().join("history.jsonl")),
            hosts: vec![("local".to_string(), Arc::new(runner) as Arc<dyn CommandRunner>)],
            locks: Locks::default(),
            clock: Arc::new(FixedClock(Mutex::new(now))),
            poll: Duration::from_secs(30),
            notify_command: notify,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    fn due() -> Due {
        Due { job: job(), scheduled_for: chrono::DateTime::parse_from_rfc3339("2026-10-07T03:00:00Z").unwrap().with_timezone(&Utc), catch_up: false }
    }

    #[test]
    fn a_failed_run_is_recorded_and_runs_the_notify_command_with_the_job_in_its_environment() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("notified");
        let cmd = format!("printf '%s|%s' \"$ZFSCTL_JOB\" \"$ZFSCTL_RESULT\" > {}", out.display());
        let shared = shared(&dir, FakeRunner::failing("cannot open 'tank': no such pool", 1), Some(cmd));
        shared.execute(Work::Run(due()));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Failed)));
        assert!(runs[0].message.contains("no such pool"));
        assert_eq!(std::fs::read_to_string(out).unwrap(), "scrub-tank|failed");
    }

    #[test]
    fn a_failing_notify_command_does_not_disturb_the_recorded_run() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::failing("no such pool", 1), Some("exit 9".to_string()));
        shared.execute(Work::Run(due()));
        assert_eq!(history::runs(&shared.history.read())[0].result, Some(RunResult::Failed));
    }

    #[test]
    fn a_skipped_run_does_not_notify() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("notified");
        let shared = shared(&dir, FakeRunner::new(""), Some(format!("touch {}", out.display())));
        let _held = shared.locks.try_acquire("local", "tank").unwrap();
        shared.execute(Work::Run(due()));
        assert_eq!(history::runs(&shared.history.read())[0].result, Some(RunResult::Skipped));
        assert!(!out.exists());
    }

    #[test]
    fn an_orphaned_run_is_closed_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let started = Event { job_id: "gone".into(), run_id: "r1".into(), event: EventKind::Started, at: shared.clock.now(), scheduled_for: None, result: None, message: String::new(), catch_up: false, summary: None };
        shared.history.append(&started).unwrap();
        let run = history::runs(&shared.history.read()).remove(0);
        shared.execute(Work::Orphan(run));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Failed)));
        assert!(runs[0].message.starts_with("lost:"));
    }

    #[test]
    fn work_for_an_unknown_host_is_ignored_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let mut d = due();
        d.job.host = "gone".into();
        shared.execute(Work::Run(d));
        assert!(shared.history.read().is_empty());
    }

    #[test]
    fn a_second_daemon_refuses_to_start_while_the_first_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        // A heartbeat from this very process, as if another daemon were running here.
        let other = heartbeat::Heartbeat { pid: 1, started_at: Utc::now(), last_tick: Utc::now() };
        heartbeat::write(&paths.heartbeat(), &other).unwrap();
        let inventory = HostInventory { hosts: Vec::new() };
        let err = run(&inventory, None, paths, &Arc::new(AtomicBool::new(true))).unwrap_err();
        assert!(err.to_string().contains("another zfsctl daemon is already running (pid 1)"), "{err}");
    }

    #[test]
    fn the_daemon_writes_a_heartbeat_while_it_runs_and_removes_it_on_a_clean_stop() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let hb = paths.heartbeat();
        let stop = Arc::new(AtomicBool::new(false));
        let inventory = HostInventory { hosts: Vec::new() };
        let stopper = Arc::clone(&stop);
        let watch_path = hb.clone();
        let watcher = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut seen = None;
            while std::time::Instant::now() < deadline && seen.is_none() {
                seen = heartbeat::read(&watch_path);
                std::thread::sleep(Duration::from_millis(50));
            }
            stopper.store(true, Ordering::SeqCst);
            seen
        });
        run(&inventory, None, paths, &stop).unwrap();
        let seen = watcher.join().unwrap().expect("a heartbeat appeared while the daemon ran");
        assert_eq!(seen.pid, std::process::id());
        assert!(!hb.exists(), "removed on a clean stop, so the TUI shows 'not running' at once");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test 2>&1 | grep -E "^error" -A4 | head -30`
Expected: FAIL to compile (`no field daemon on Config`, `cannot find function parse_args`, `cannot find function run`, `cannot find type Shared`).

- [ ] **Step 3: Implement the config table**

In `src/config.rs`, add this field to `struct Config`, after the `ui` field:

```rust
    #[serde(default, skip_serializing_if = "DaemonConfig::is_default")]
    pub daemon: DaemonConfig,
```

and add these definitions after `struct Config`:

```rust
/// Settings for `zfsctl daemon`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DaemonConfig {
    /// A shell command run when a scheduled job fails. The job's details
    /// arrive in `ZFSCTL_JOB`, `ZFSCTL_HOST`, `ZFSCTL_KIND`, `ZFSCTL_RESULT`
    /// and `ZFSCTL_MESSAGE`; never on the command line.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub notify_command: Option<String>,
}

impl DaemonConfig {
    fn is_default(&self) -> bool {
        self.notify_command.is_none()
    }
}
```

- [ ] **Step 4: Implement the runtime**

Replace the part of `src/daemon/mod.rs` ABOVE the test module you appended (the file's current `pub mod` lines) with:

```rust
//! `zfsctl daemon`: runs scheduled jobs on the hub, unattended.

pub mod cron;
pub mod engine;
pub mod heartbeat;
pub mod history;
pub mod notify;
pub mod scheduler;
pub mod scrub;

use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use crate::hosts::HostInventory;
use chrono::Utc;
use engine::{Engine, Paths, Work};
use history::{Event, EventKind, History, RunResult};
use scrub::{Clock, Context, Locks, Outcome, SystemClock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How often a running scrub is polled for completion.
const POLL: Duration = Duration::from_secs(30);

fn log(message: &str) {
    eprintln!("{} {message}", Utc::now().to_rfc3339());
}

/// What every job thread shares.
struct Shared {
    history: History,
    hosts: Vec<(String, Arc<dyn CommandRunner>)>,
    locks: Locks,
    clock: Arc<dyn Clock + Send + Sync>,
    poll: Duration,
    notify_command: Option<String>,
    stop: Arc<AtomicBool>,
}

impl Shared {
    fn runner(&self, host: &str) -> Option<Arc<dyn CommandRunner>> {
        self.hosts.iter().find(|(name, _)| name == host).map(|(_, r)| Arc::clone(r))
    }

    /// Runs the notify command for a failed run; a failing hook is only logged.
    fn notify(&self, job: &crate::jobs::Job, outcome: &Outcome) {
        if outcome.result != RunResult::Failed {
            return;
        }
        let Some(command) = &self.notify_command else { return };
        let notice = notify::Notice { job: job.id.clone(), host: job.host.clone(), kind: job.kind.clone(), result: "failed".to_string(), message: outcome.message.clone() };
        match notify::run(command, &notice, notify::TIMEOUT) {
            Ok(()) => log(&format!("notified about {}", job.id)),
            Err(e) => log(&format!("notify command for {} {e}", job.id)),
        }
    }

    /// Runs one piece of work to completion on the calling thread.
    fn execute(&self, work: Work) {
        let ctx = Context { history: &self.history, clock: self.clock.as_ref(), locks: &self.locks, poll: self.poll, stop: &self.stop };
        match work {
            Work::Run(due) => {
                let Some(runner) = self.runner(&due.job.host) else { return };
                let run_id = format!("{}-{}", due.job.id, self.clock.now().timestamp_millis());
                log(&format!("running {} (slot {}{})", due.job.id, due.scheduled_for.to_rfc3339(), if due.catch_up { ", catch-up" } else { "" }));
                if let Some(outcome) = scrub::run_scrub(&ctx, runner.as_ref(), &due.job, due.scheduled_for, due.catch_up, &run_id) {
                    log(&format!("{} {:?}: {}", outcome.job_id, outcome.result, outcome.message));
                    self.notify(&due.job, &outcome);
                }
            }
            Work::Resume { job, run } => {
                let Some(runner) = self.runner(&job.host) else { return };
                log(&format!("re-attaching to {} (run {})", job.id, run.run_id));
                if let Some(outcome) = scrub::resume_scrub(&ctx, runner.as_ref(), &job, &run) {
                    log(&format!("{} {:?}: {}", outcome.job_id, outcome.result, outcome.message));
                    self.notify(&job, &outcome);
                }
            }
            Work::Orphan(run) => {
                log(&format!("run {} of {} has no runnable job any more", run.run_id, run.job_id));
                let _ = self.history.append(&Event {
                    job_id: run.job_id,
                    run_id: run.run_id,
                    event: EventKind::Finished,
                    at: self.clock.now(),
                    scheduled_for: None,
                    result: Some(RunResult::Failed),
                    message: "lost: the job was removed or can no longer run".to_string(),
                    catch_up: false,
                    summary: None,
                });
            }
        }
    }
}

fn dispatch(shared: &Arc<Shared>, work: Vec<Work>) {
    for item in work {
        let shared = Arc::clone(shared);
        if let Err(e) = std::thread::Builder::new().name("job".to_string()).spawn(move || shared.execute(item)) {
            log(&format!("could not start a job thread: {e}"));
        }
    }
}

/// Runs the scheduler until `stop` is set. Refuses to start if another daemon is alive.
pub fn run(inventory: &HostInventory, notify_command: Option<String>, paths: Paths, stop: &Arc<AtomicBool>) -> Result<()> {
    paths.ensure_dir().map_err(|e| AppError::Config(format!("creating {}: {e}", paths.dir.display())))?;
    let now = Utc::now();
    let heartbeat_path = paths.heartbeat();
    if let Some(pid) = heartbeat::live_other_instance(heartbeat::read(&heartbeat_path).as_ref(), now, std::process::id(), heartbeat::process_alive) {
        return Err(AppError::Config(format!("another zfsctl daemon is already running (pid {pid})")));
    }
    let hosts: Vec<(String, Arc<dyn CommandRunner>)> = inventory.hosts.iter().map(|h| (h.name.clone(), Arc::clone(&h.runner))).collect();
    let names: Vec<String> = hosts.iter().map(|(n, _)| n.clone()).collect();
    let shared = Arc::new(Shared { history: History::new(paths.history()), hosts, locks: Locks::default(), clock: Arc::new(SystemClock), poll: POLL, notify_command, stop: Arc::clone(stop) });
    let mut engine = Engine::new(paths, chrono::Local, names, now);
    let started_at = now;

    let work = engine.start(Utc::now());
    log(&format!("zfsctl daemon started (pid {})", std::process::id()));
    report(&engine);
    dispatch(&shared, work);

    let mut last_problem: Option<String> = None;
    while !stop.load(Ordering::SeqCst) {
        let now = Utc::now();
        if let Err(e) = heartbeat::write(&heartbeat_path, &heartbeat::Heartbeat { pid: std::process::id(), started_at, last_tick: now }) {
            let problem = format!("cannot write the heartbeat: {e}");
            if last_problem.as_deref() != Some(problem.as_str()) {
                log(&problem);
            }
            last_problem = Some(problem);
        }
        let before = engine.load_error.clone();
        let work = engine.step(now);
        if engine.load_error != before {
            report(&engine);
        }
        dispatch(&shared, work);
        std::thread::sleep(Duration::from_secs(1));
    }
    let _ = std::fs::remove_file(&heartbeat_path);
    log("zfsctl daemon stopping");
    Ok(())
}

fn report(engine: &Engine<chrono::Local>) {
    if let Some(e) = &engine.load_error {
        log(&format!("jobs file problem, keeping the previous schedule: {e}"));
    }
    for u in &engine.unsupported {
        log(&format!("job {} will not run: {}", u.id, u.reason));
    }
}
```

- [ ] **Step 5: Implement the subcommand**

Replace `src/main.rs` above its `mod tests` with the following (keep the existing `mod tests` and its `check_root` test, plus the two tests from Step 1):

```rust
mod acl;
mod config;
mod daemon;
mod error;
mod exec;
mod hosts;
mod jobs;
mod ui;
mod zfs;

use std::path::PathBuf;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Tui,
    Daemon { state_dir: PathBuf },
}

fn parse_args(args: &[String]) -> error::Result<Command> {
    let usage = || error::AppError::Config("usage: zfsctl [daemon [--state-dir DIR]]".to_string());
    match args {
        [] => Ok(Command::Tui),
        [cmd] if cmd == "daemon" => Ok(Command::Daemon { state_dir: PathBuf::from(daemon::engine::DEFAULT_STATE_DIR) }),
        [cmd, flag, dir] if cmd == "daemon" && flag == "--state-dir" => Ok(Command::Daemon { state_dir: PathBuf::from(dir) }),
        _ => Err(usage()),
    }
}

fn run() -> error::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = parse_args(&args)?;
    check_root()?;
    let config_path = PathBuf::from(config::DEFAULT_CONFIG_PATH);
    let config = config::Config::load(&config_path)?;
    let inventory = hosts::HostInventory::from_config(&config);
    if let Command::Daemon { state_dir } = command {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
            signal_hook::flag::register(signal, std::sync::Arc::clone(&stop)).map_err(|e| error::AppError::Config(format!("installing signal handlers: {e}")))?;
        }
        return daemon::run(&inventory, config.daemon.notify_command.clone(), daemon::engine::Paths::new(state_dir), &stop);
    }
    // Resolved before the terminal enters raw mode, so a bad `theme`
    // value prints a plain error and exits instead of garbling the screen.
    let resolved = ui::theme::resolve(config.ui.theme.as_deref(), &ui::theme::ThemeEnv::from_process())?;
    ui::theme::apply_color_policy();
    ui::run_app(inventory, &config_path, resolved, config.ui.auto_refresh_interval())
}

fn check_root() -> error::Result<()> {
    if !nix::unistd::Uid::effective().is_root() {
        return Err(error::AppError::Config(
            "zfsctl must be run as root".to_string(),
        ));
    }
    Ok(())
}
```

- [ ] **Step 6: Add the rc script**

Create `contrib/rc.d/zfsctl` (mode 755):

```sh
#!/bin/sh
#
# PROVIDE: zfsctl
# REQUIRE: LOGIN NETWORKING zfs
# KEYWORD: shutdown
#
# Add to /etc/rc.conf:
#   zfsctl_enable="YES"
# Optional:
#   zfsctl_binary="/usr/local/bin/zfsctl"   # where the binary is
#   zfsctl_state_dir="/var/db/zfsctl"         # where jobs, history and heartbeat live
#   zfsctl_log="/var/log/zfsctl.log"          # the daemon's log
#
# zfsctl does not daemonize itself: daemon(8) detaches it, writes the pidfile,
# sends its output to the log, and restarts it ten seconds after a crash.

. /etc/rc.subr

name="zfsctl"
rcvar="zfsctl_enable"

load_rc_config $name

: ${zfsctl_enable:="NO"}
: ${zfsctl_binary:="/usr/local/bin/zfsctl"}
: ${zfsctl_state_dir:="/var/db/zfsctl"}
: ${zfsctl_log:="/var/log/zfsctl.log"}

pidfile="/var/run/${name}.pid"
command="/usr/sbin/daemon"
command_args="-r -R 10 -f -P ${pidfile} -o ${zfsctl_log} ${zfsctl_binary} daemon --state-dir ${zfsctl_state_dir}"

run_rc_command "$1"
```

Run: `chmod +x contrib/rc.d/zfsctl && sh -n contrib/rc.d/zfsctl && echo ok`
Expected: `ok`.

- [ ] **Step 7: Run the whole suite and lint**

Run: `cargo test 2>&1 | grep -E "^error|^test result|FAILED|panicked" -A8; cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" -A4 | grep -E "src/(daemon|jobs|main|config)"`
Expected: all tests PASS (the previous 730 plus the new ones); no clippy output for the touched files (remove any leftover test-only `#[cfg(test)]` helper that is now used, if clippy says so).

- [ ] **Step 8: Document it**

In `README.md`, add this section directly before `## Status`:

````markdown
## Scheduled jobs (`zfsctl daemon`)

`zfsctl daemon` runs scheduled jobs on the hub, unattended. It must run as root, like the TUI, and reaches other hosts over the same SSH + sudo path, so it needs no new sudoers entries. The only job kind so far is **scrub**: run `zpool scrub` on a pool of any configured host on a cron schedule, watch it to completion, and record the result. (A Jobs section in the TUI to manage these comes later; until then edit `jobs.toml` by hand.)

**Install the service.** Copy the binary and the rc script, then enable it:

```sh
install -m 755 target/release/zfsctl /usr/local/bin/zfsctl
install -m 755 contrib/rc.d/zfsctl /usr/local/etc/rc.d/zfsctl
sysrc zfsctl_enable=YES
service zfsctl start
```

`daemon(8)` detaches it, writes `/var/run/zfsctl.pid`, sends output to `/var/log/zfsctl.log`, and restarts it ten seconds after a crash. `zfsctl_binary`, `zfsctl_state_dir` and `zfsctl_log` in `rc.conf` override the binary path, the state directory and the log.

**State** lives in `/var/db/zfsctl/` (root only): `jobs.toml` (the jobs), `history.jsonl` (one JSON line per run event) and `daemon.json` (a heartbeat rewritten every second; older than 10 seconds means the daemon is not running). The daemon notices an edited `jobs.toml` within a second and keeps the previous schedule if the new file does not parse.

**A job** is one `[[jobs]]` table:

```toml
[[jobs]]
id = "scrub-tank-monthly"
host = "local"          # a host name from config.toml; "local" is the hub
kind = "scrub"
pool = "tank"
schedule = "0 3 1 * *"  # five-field cron, in the hub's local time zone
catch_up = true         # run once on startup if a scheduled time was missed (default true)
enabled = true          # default true
created_at = "2026-10-06T12:00:00Z"  # optional; a catch-up never reaches back before this
```

Schedules are `minute hour day-of-month month day-of-week` with `*`, numbers, lists, ranges and steps (`*/15`). Day of week is `0`-`6`, `0` being Sunday. If both day fields are restricted a day matches when either does. A local time that does not exist (the spring DST jump) is skipped; one that happens twice (autumn) runs once.

**Notifications.** To be told when a job fails or a scrub finds errors, set a command in `config.toml`:

```toml
[daemon]
notify_command = "curl -s -d \"$ZFSCTL_JOB on $ZFSCTL_HOST: $ZFSCTL_MESSAGE\" ntfy.sh/my-zfs-alerts"
```

It runs through `sh -c` with `ZFSCTL_JOB`, `ZFSCTL_HOST`, `ZFSCTL_KIND`, `ZFSCTL_RESULT` and `ZFSCTL_MESSAGE` set, and is killed after 30 seconds. Job data only ever reaches it through those variables, never the command line, so a pool or message can never be executed as code. A failing hook is logged and never affects the job.
````

- [ ] **Step 9: Verify live on the hub**

Deploy the changed tree (`src/`, `contrib/`, `Cargo.toml`, `Cargo.lock`) to `/home/fb/zfsctl` and build: `ssh fb@172.16.10.90 'cd /home/fb/zfsctl && cargo build --release 2>&1 | tail -2'`. Then, as root on the hub (`ssh fb@172.16.10.90 sudo sh`), using the disposable `scratch` pool and a temporary state directory:

1. `mkdir -p /tmp/zfsctl-live`, write `/tmp/zfsctl-live/jobs.toml` with one job: `id="live-scrub"`, `host="local"`, `kind="scrub"`, `pool="scratch"`, `schedule="* * * * *"`, and `created_at` set to the current UTC time.
2. Start `./target/release/zfsctl daemon --state-dir /tmp/zfsctl-live > /tmp/zfsctl-live/daemon.log 2>&1 &`. Within 3 seconds `daemon.json` exists. A second `zfsctl daemon --state-dir /tmp/zfsctl-live` prints `another zfsctl daemon is already running` and exits non-zero.
3. At the next minute boundary the log shows `running live-scrub`, and `history.jsonl` gets a `started` event whose `summary` is the pool's previous scan line. About 30 seconds later it gets a `finished` event with `result` `ok` and the new `scrub repaired ... on <today>` summary. `zpool status scratch` shows the new scan line.
4. Stop the daemon with `kill <pid>` (SIGTERM) while a run is being watched: the log shows `zfsctl daemon stopping` and `daemon.json` is gone. Wait past a minute boundary, start it again: the log shows `re-attaching to` the unfinished run and ONE catch-up for the missed slot (`catch-up` in the log, `"catch_up":true` in history), not several. If the resumed scrub still holds the pool, the catch-up is recorded as `skipped: already running`, which is correct and counts as handled.
5. Edit `jobs.toml` to garbage (`echo '[[[' > /tmp/zfsctl-live/jobs.toml`): within a second the log says `jobs file problem, keeping the previous schedule`, and the job still fires. Restore a valid file.
6. Right after a run starts, `kill -9` the daemon and restart it within 10 seconds (its heartbeat is still fresh, but its process is dead): it must start, not refuse, and the log shows `re-attaching to` the in-flight run; a `finished` event is then recorded for the same `run_id`.
7. Optional, the notify hook: in `/usr/local/etc/zfsctl/config.toml` add `[daemon]` with `notify_command = "touch /tmp/zfsctl-notified"`, point a job at a pool that does not exist, and confirm the file appears after the run fails and the history shows `failed`. Remove the setting and the file afterwards.
8. Clean up: `kill` the daemon, `rm -rf /tmp/zfsctl-live`, confirm `pgrep -l zfsctl` shows nothing and `zpool status scratch` has no scrub in progress.

- [ ] **Step 10: Commit**

```bash
git add src/daemon/mod.rs src/main.rs src/config.rs contrib/rc.d/zfsctl README.md
git commit -m "feat: add zfsctl daemon, the scheduler runtime for scheduled scrubs

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```
