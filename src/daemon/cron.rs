//! A five-field cron schedule: parsing, and the nearest scheduled time
//! after or before an instant, evaluated in a caller-supplied time zone.
//!
//! Fields are minute, hour, day of month, month, day of week (`0`-`6`,
//! `0` = Sunday). Each accepts `*`, a number, a comma list, a range
//! `a-b` and a step `*/n` or `a-b/n`. When both day fields are
//! restricted a day matches if EITHER does (standard cron behavior).

use chrono::{DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};

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

/// A job is current while its last success is at most this many schedule intervals old. One missed run is normal (a run can
/// start late, a long replication can overlap the next slot); two is a whole cycle gone.
pub const CURRENT_WITHIN_INTERVALS: i32 = 2;

/// How many scheduled times ahead the gap is measured over: the longest gap among them counts, so a weekday-only job
/// is not late over a weekend.
const LOOKAHEAD: usize = 5;

/// The longest gap between the next few scheduled times after `now`; `None` for a schedule that does not parse or never fires.
pub fn schedule_gap(schedule: &str, now: DateTime<Utc>) -> Option<Duration> {
    let schedule = Schedule::parse(schedule).ok()?;
    let mut at = now.with_timezone(&Local);
    let mut gaps = Vec::new();
    for _ in 0..LOOKAHEAD {
        let next = schedule.next_after(&at)?;
        gaps.push(next - at);
        at = next;
    }
    // the first "gap" is from `now`, not between two fires: leave it out
    gaps.into_iter().skip(1).max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;
    use chrono_tz::America::New_York;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn next(expr: &str, after: &str) -> String {
        Schedule::parse(expr).unwrap().next_after(&utc(after)).unwrap().to_rfc3339()
    }

    #[test]
    fn schedule_gap_is_the_longest_gap_among_the_next_fires() {
        let now = utc("2026-10-22T12:00:00Z");
        assert_eq!(schedule_gap("0 * * * *", now), Some(Duration::hours(1)));
        assert!(schedule_gap("0 3 * * 1-5", now).unwrap() >= Duration::hours(24));
        assert_eq!(schedule_gap("not a schedule", now), None);
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
