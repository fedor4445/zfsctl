//! What one job gives one dataset: whether it is current, whether it gives snapshots, what kind of copy it makes and which
//! datasets it covers. Pure helpers; the judging that combines them lives elsewhere.

use crate::daemon::cron::schedule_gap;
use crate::daemon::history::{Run, RunResult};
use crate::jobs::{self, Job};
use chrono::{DateTime, Utc};

/// How a job stands: the same overdue rule the Dashboard uses, with the tolerance (in schedule gaps) chosen by the caller.
#[allow(dead_code)] // the judging in the next task uses this
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JobState {
    Current,
    Stopped { last_ok: DateTime<Utc> },
    Failing,
    NeverRan,
}

#[allow(dead_code)] // the judging in the next task uses this
pub(crate) fn job_state(job: &Job, runs: &[Run], tolerance: i32, now: DateTime<Utc>) -> JobState {
    let mine = || runs.iter().filter(|r| r.job_id == job.id);
    let latest_finished = mine().filter(|r| r.result.is_some()).max_by_key(|r| r.finished_at.unwrap_or(r.started_at));
    if latest_finished.is_some_and(|r| r.result == Some(RunResult::Failed)) {
        return JobState::Failing;
    }
    let last_ok = mine().filter(|r| r.result == Some(RunResult::Ok)).map(|r| r.finished_at.unwrap_or(r.started_at)).max();
    let Some(gap) = schedule_gap(&job.schedule, now) else {
        return JobState::Stopped { last_ok: last_ok.or(job.created_at).unwrap_or(now) };
    };
    match last_ok {
        Some(at) if now - at > gap * tolerance => JobState::Stopped { last_ok: at },
        Some(_) => JobState::Current,
        None => JobState::NeverRan,
    }
}

/// An enabled snapshot job.
#[allow(dead_code)] // the judging in the next task uses this
pub(crate) fn gives_snapshots(job: &Job) -> bool {
    job.enabled && job.kind == jobs::KIND_SNAPSHOT
}

#[allow(dead_code)] // the judging in the next task uses this
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyKind {
    OnPremises,
    Online,
}

/// The copy an enabled job makes on another machine, with a string saying where it writes; `None` for anything else.
#[allow(dead_code)] // the judging in the next task uses this
pub(crate) fn copy_of(job: &Job) -> Option<(CopyKind, String)> {
    if !job.enabled {
        return None;
    }
    match job.kind.as_str() {
        jobs::KIND_REPLICATE => {
            let dest = job.dest_host.as_deref()?;
            if dest == jobs::DEST_FILE {
                if job.host == "local" {
                    return None;
                }
                Some((CopyKind::OnPremises, format!("{}/{}", jobs::DEST_FILE, job.dest_dir.as_deref()?)))
            } else if dest != job.host {
                Some((CopyKind::OnPremises, format!("{dest}/{}", job.dest_dataset.as_deref()?)))
            } else {
                None
            }
        }
        jobs::KIND_BACKUP => {
            let repo = job.repository.as_deref()?;
            let online = ["s3:", "b2:", "azure:", "gs:", "swift:", "rclone:"].iter().any(|p| repo.starts_with(p));
            let on_prem = ["sftp:", "rest:"].iter().any(|p| repo.starts_with(p));
            if online {
                Some((CopyKind::Online, repo.to_string()))
            } else if on_prem {
                Some((CopyKind::OnPremises, repo.to_string()))
            } else {
                None
            }
        }
        jobs::KIND_MIRROR => {
            let remote = job.remote.as_deref()?;
            (remote.contains(':') && !remote.starts_with('/') && !remote.starts_with(":local:")).then(|| (CopyKind::Online, remote.to_string()))
        }
        _ => None,
    }
}

/// Whether the job covers `dataset` on `host`: the same dataset, or for a recursive snapshot job any descendant. The other kinds
/// ignore `recursive`, so they cover only their own dataset.
#[allow(dead_code)] // the judging in the next task uses this
pub(crate) fn covers(job: &Job, host: &str, dataset: &str) -> bool {
    if job.host != host {
        return false;
    }
    let Some(own) = job.dataset.as_deref() else { return false };
    own == dataset || (job.kind == jobs::KIND_SNAPSHOT && job.recursive && dataset.strip_prefix(own).is_some_and(|rest| rest.starts_with('/')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-22T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn run(job: &str, ago: Duration, result: Option<RunResult>) -> Run {
        let at = now() - ago;
        Run { job_id: job.into(), run_id: format!("{job}-{}", ago.num_minutes()), started_at: at, scheduled_for: None, finished_at: result.map(|_| at), result, message: String::new(), catch_up: false, summary: None }
    }

    fn hourly() -> Job {
        Job::test_snapshot("snap", "tank/data", "hourly", 24)
    }

    #[test]
    fn job_state_follows_the_overdue_rule_at_each_tolerance() {
        let ok = |min: i64| vec![run("snap", Duration::minutes(min), Some(RunResult::Ok))];
        assert_eq!(job_state(&hourly(), &ok(119), 2, now()), JobState::Current);
        assert_eq!(job_state(&hourly(), &ok(121), 2, now()), JobState::Stopped { last_ok: now() - Duration::minutes(121) });
        assert!(matches!(job_state(&hourly(), &ok(61), 1, now()), JobState::Stopped { .. }));
        assert_eq!(job_state(&hourly(), &ok(239), 4, now()), JobState::Current);
        let failed = vec![run("snap", Duration::hours(5), Some(RunResult::Ok)), run("snap", Duration::minutes(10), Some(RunResult::Failed))];
        assert_eq!(job_state(&hourly(), &failed, 2, now()), JobState::Failing);
        assert_eq!(job_state(&hourly(), &[], 2, now()), JobState::NeverRan);
        let mut bad = hourly();
        bad.schedule = "not a schedule".into();
        assert!(matches!(job_state(&bad, &ok(5), 2, now()), JobState::Stopped { .. }));
    }

    fn backup(repo: &str) -> Job {
        Job::test_backup("bk", "tank/data", "daily", repo, "/pw")
    }

    #[test]
    fn copy_of_classifies_destinations() {
        assert_eq!(copy_of(&backup("/mnt/repo")), None);
        assert_eq!(copy_of(&backup("sftp:h:/r")), Some((CopyKind::OnPremises, "sftp:h:/r".into())));
        assert_eq!(copy_of(&backup("rest:http://h/r")).map(|c| c.0), Some(CopyKind::OnPremises));
        assert_eq!(copy_of(&backup("rclone:b2:x")), Some((CopyKind::Online, "rclone:b2:x".into())));
        for p in ["s3:h/b", "b2:b", "azure:b", "gs:b", "swift:b"] {
            assert_eq!(copy_of(&backup(p)).map(|c| c.0), Some(CopyKind::Online), "{p}");
        }
        let repl = |dest: &str| Job::test_replicate("rep", "tank/data", "p", dest, "tank/copy");
        assert_eq!(copy_of(&repl("local")), None);
        assert_eq!(copy_of(&repl("fbsd16")), Some((CopyKind::OnPremises, "fbsd16/tank/copy".into())));
        let mut file = repl("file");
        file.dest_dir = Some("/backup".into());
        assert_eq!(copy_of(&file), None, "a file on the hub itself is not another machine");
        file.host = "fbsd16".into();
        assert_eq!(copy_of(&file), Some((CopyKind::OnPremises, "file//backup".into())));
        let mut other = repl("fbsd16");
        other.host = "fbsd16".into();
        assert_eq!(copy_of(&other), None, "same host");
        let mirror = |r: &str| Job::test_mirror("m", "tank/data", "p", r);
        assert_eq!(copy_of(&mirror("b2:bucket/x")), Some((CopyKind::Online, "b2:bucket/x".into())));
        assert_eq!(copy_of(&mirror("/mnt/x")), None);
        assert_eq!(copy_of(&mirror(":local:/x")), None);
        let mut off = backup("s3:h/b");
        off.enabled = false;
        assert_eq!(copy_of(&off), None);
        assert!(gives_snapshots(&hourly()));
        let mut off = hourly();
        off.enabled = false;
        assert!(!gives_snapshots(&off) && !gives_snapshots(&backup("/r")));
    }

    #[test]
    fn covers_respects_host_prefix_and_recursion() {
        let mut snap = hourly();
        assert!(covers(&snap, "local", "tank/data"));
        assert!(!covers(&snap, "local", "tank/data2"));
        assert!(!covers(&snap, "local", "tank/data/child"), "not recursive");
        assert!(!covers(&snap, "fbsd16", "tank/data"));
        snap.recursive = true;
        assert!(covers(&snap, "local", "tank/data/child/deep"));
        assert!(!covers(&snap, "local", "tank/data2"));
        assert!(!covers(&snap, "local", "tank"), "a child never covers its parent");
        assert!(!covers(&snap, "fbsd16", "tank/data/child"));
        let mut repl = Job::test_replicate("rep", "tank/data", "p", "fbsd16", "tank/copy");
        repl.recursive = true;
        assert!(covers(&repl, "local", "tank/data"));
        assert!(!covers(&repl, "local", "tank/data/child"), "the daemon never reads recursive for replication");
    }
}
