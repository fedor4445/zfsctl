//! Jobs that have stopped protecting: an enabled snapshot, replication, backup or cloud job whose last success is much older
//! than its schedule says it should be. It reads only the job history the daemon keeps, so it needs no call to any host.
//! A job whose last run failed already has its own Dashboard line, and a stopped daemon its own red one: neither is repeated here.

use crate::daemon::cron::{schedule_gap, CURRENT_WITHIN_INTERVALS};
use crate::daemon::history::RunResult;
use crate::jobs::{self, Job};
use crate::ui::jobs::{age_text, schedule_label, job_target, JobsView};
use chrono::{DateTime, Utc};

/// A job is late once its last success is `YELLOW_AFTER` schedule intervals old, and in trouble at `RED_AFTER`: two is a whole
/// cycle gone, four is lasting.
const YELLOW_AFTER: i32 = CURRENT_WITHIN_INTERVALS;
const RED_AFTER: i32 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overdue {
    pub job_id: String,
    pub red: bool,
    pub text: String,
}

fn label(job: &Job) -> &'static str {
    match job.kind.as_str() {
        jobs::KIND_SNAPSHOT => "snapshot",
        jobs::KIND_REPLICATE => "replicate",
        jobs::KIND_BACKUP => "backup",
        _ => "cloud",
    }
}

/// The late jobs, worst first. Disabled, unsupported and running jobs, scrubs, jobs whose last run failed and jobs that never
/// ran with no creation time to measure from are left out.
pub fn overdue(view: &JobsView, now: DateTime<Utc>) -> Vec<Overdue> {
    let mut found: Vec<Overdue> = Vec::new();
    for job in &view.jobs {
        let protects = matches!(job.kind.as_str(), jobs::KIND_SNAPSHOT | jobs::KIND_REPLICATE | jobs::KIND_BACKUP | jobs::KIND_MIRROR);
        if !protects || !job.enabled || view.unsupported.contains_key(&job.id) || view.is_running(&job.id) {
            continue;
        }
        if view.last_run(&job.id).is_some_and(|r| r.result == Some(RunResult::Failed)) {
            continue;
        }
        let last_ok = view.runs.iter().filter(|r| r.job_id == job.id && r.result == Some(RunResult::Ok)).filter_map(|r| r.finished_at.or(Some(r.started_at))).max();
        let Some(since) = last_ok.or(job.created_at) else { continue };
        let Some(gap) = schedule_gap(&job.schedule, now) else { continue };
        let age = now - since;
        let red = age > gap * RED_AFTER;
        if !red && age <= gap * YELLOW_AFTER {
            continue;
        }
        let when = if last_ok.is_some() { format!("last ok {}", age_text(now, since)) } else { format!("never succeeded (created {})", age_text(now, since)) };
        found.push(Overdue { job_id: job.id.clone(), red, text: format!("{} {}: {when} ({})", label(job), job_target(job), schedule_label(&job.schedule)) });
    }
    found.sort_by(|a, b| b.red.cmp(&a.red).then_with(|| a.job_id.cmp(&b.job_id)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::history::Run;
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-22T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn run(job: &str, ago: Duration, result: Option<RunResult>) -> Run {
        let at = now() - ago;
        Run { job_id: job.into(), run_id: format!("{job}-{}", ago.num_minutes()), started_at: at, scheduled_for: None, finished_at: result.map(|_| at), result, message: String::new(), catch_up: false, summary: None }
    }

    fn view(jobs: Vec<Job>, runs: Vec<Run>) -> JobsView {
        JobsView { jobs, runs, loaded: true, ..JobsView::default() }
    }

    fn hourly() -> Job {
        Job::test_snapshot("snap", "tank/data", "hourly", 24)
    }

    #[test]
    fn a_job_is_late_after_two_intervals_and_in_trouble_after_four() {
        let at = |ago: Duration| overdue(&view(vec![hourly()], vec![run("snap", ago, Some(RunResult::Ok))]), now());
        assert!(at(Duration::minutes(90)).is_empty(), "one missed run is normal");
        assert!(at(Duration::minutes(119)).is_empty());
        let late = at(Duration::hours(3));
        assert_eq!(late.len(), 1);
        assert!(!late[0].red);
        assert_eq!(late[0].text, "snapshot tank/data: last ok 3h ago (Hourly)");
        let trouble = at(Duration::hours(5));
        assert!(trouble[0].red && trouble[0].text.contains("last ok 5h ago"), "{trouble:?}");
    }

    #[test]
    fn a_daily_job_gets_a_day_and_a_half_of_slack_and_a_weekday_job_is_not_late_over_the_weekend() {
        let daily = Job::test_backup("bk", "tank/data", "daily", "/repo", "/pw");
        let at = |ago: Duration| overdue(&view(vec![daily.clone()], vec![run("bk", ago, Some(RunResult::Ok))]), now());
        assert!(at(Duration::hours(40)).is_empty());
        assert!(!at(Duration::hours(60))[0].red);
        assert!(at(Duration::hours(110))[0].red);
        let mut weekdays = daily.clone();
        weekdays.schedule = "0 3 * * 1-5".into();
        let over_a_weekend = overdue(&view(vec![weekdays], vec![run("bk", Duration::hours(80), Some(RunResult::Ok))]), now());
        assert!(over_a_weekend.is_empty(), "{over_a_weekend:?}");
    }

    #[test]
    fn a_job_that_never_succeeded_is_measured_from_its_creation_and_without_one_it_is_left_alone() {
        let mut job = hourly();
        job.created_at = Some(now() - Duration::minutes(30));
        assert!(overdue(&view(vec![job.clone()], vec![]), now()).is_empty(), "just created");
        job.created_at = Some(now() - Duration::hours(10));
        let found = overdue(&view(vec![job.clone()], vec![]), now());
        assert!(found[0].red && found[0].text == "snapshot tank/data: never succeeded (created 10h ago) (Hourly)", "{found:?}");
        job.created_at = None;
        assert!(overdue(&view(vec![job], vec![]), now()).is_empty(), "nothing to measure from");
    }

    #[test]
    fn disabled_running_failed_unsupported_and_scrub_jobs_are_not_reported() {
        let old = Duration::days(3);
        let ok = |id: &str| run(id, old, Some(RunResult::Ok));
        let mut disabled = hourly();
        disabled.id = "off".into();
        disabled.enabled = false;
        let mut running = hourly();
        running.id = "busy".into();
        let mut failed = hourly();
        failed.id = "bad".into();
        let mut unsupported = hourly();
        unsupported.id = "nope".into();
        let mut scrub = hourly();
        scrub.id = "scrub".into();
        scrub.kind = jobs::KIND_SCRUB.into();
        let mut v = view(vec![disabled, running, failed, unsupported, scrub], vec![ok("off"), ok("busy"), run("busy", Duration::minutes(5), None), ok("bad"), run("bad", Duration::hours(1), Some(RunResult::Failed)), ok("nope"), ok("scrub")]);
        v.unsupported.insert("nope".into(), "no such host".into());
        assert!(overdue(&v, now()).is_empty(), "{:?}", overdue(&v, now()));
    }

    #[test]
    fn the_worst_come_first_and_each_kind_has_its_word() {
        let mirror = Job::test_mirror("cloud-a", "tank/data", "p", "b2:bkt/p");
        let repl = Job::test_replicate("rep", "tank/data", "p", "fbsd16", "tank/copy");
        let runs = vec![run("snap", Duration::hours(3), Some(RunResult::Ok)), run("cloud-a", Duration::days(6), Some(RunResult::Ok)), run("rep", Duration::hours(12), Some(RunResult::Ok))];
        let found = overdue(&view(vec![hourly(), mirror, repl], runs), now());
        assert_eq!(found.iter().map(|o| (o.job_id.as_str(), o.red)).collect::<Vec<_>>(), [("cloud-a", true), ("rep", true), ("snap", false)]);
        assert!(found[0].text.starts_with("cloud ") && found[1].text.starts_with("replicate "), "{found:?}");
    }
}
