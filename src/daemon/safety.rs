//! The data-safety graph: where each dataset's data lives, how it got there and how safe that makes it.
//!
//! The protection check counts copies; this follows them. A body is an original dataset and every place its data was
//! copied to, through replications, restic repositories and cloud remotes, with the edges (the copy jobs) that carry it.
//! Everything here is pure: the jobs, their runs and the clock go in, the graph comes out.

use crate::config::ProtectionSettings;
use crate::daemon::history::Run;
use crate::daemon::protection::{copy_of, job_state, CopyKind, JobState};
use crate::jobs::{self, Job};
use chrono::{DateTime, Duration, Utc};

/// What a place is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceKind {
    /// The dataset the data lives in.
    Original,
    /// A dataset a replication writes into.
    Replica,
    /// A restic repository.
    Backup,
    /// A cloud remote written by a mirror job.
    Mirror,
    /// A directory on the hub that receives replication streams.
    File,
}

/// A place that holds the data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// `host:dataset` for a dataset, the repository or remote text for the rest.
    pub id: String,
    pub kind: PlaceKind,
    /// The failure that would take this place away: its host, a shared site, or a cloud remote.
    pub domain: String,
    /// Whether it is out of the operator's reach (cloud or a remote repository).
    pub online: bool,
}

/// A copy job, from one place to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub job: String,
    pub last_ok: Option<DateTime<Utc>>,
    pub current: bool,
}

/// An original and everything reachable from it. `places[0]` is the original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Body {
    pub places: Vec<Place>,
    pub edges: Vec<Edge>,
}

/// What is known of the data at one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// How old the data there is, at most; `None` when a hop on the way never succeeded.
    pub age: Option<Duration>,
    /// Edges between the original and this place.
    pub hops: usize,
    /// Every edge on the way is current.
    pub current: bool,
}

impl Body {
    /// The edge that feeds place `i` (every place but the original has exactly one).
    fn feeder(&self, i: usize) -> Option<&Edge> {
        self.edges.iter().find(|e| e.to == i)
    }

    /// The data at place `i`, from the oldest last success on the path from the original. The original is live.
    pub fn reading(&self, i: usize, now: DateTime<Utc>) -> Reading {
        let (mut oldest, mut hops, mut current, mut at, mut seen) = (Some(now), 0, true, i, vec![i]);
        while let Some(edge) = self.feeder(at) {
            hops += 1;
            current &= edge.current;
            oldest = match (oldest, edge.last_ok) {
                (Some(o), Some(t)) => Some(o.min(t)),
                _ => None,
            };
            at = edge.from;
            if seen.contains(&at) {
                break;
            }
            seen.push(at);
        }
        Reading { age: oldest.map(|t| now - t), hops, current }
    }

    /// How many failure domains can be lost at once with the data surviving: the distinct domains holding a current copy
    /// (the original's included), minus one.
    pub fn tolerates(&self, now: DateTime<Utc>) -> usize {
        let mut domains: Vec<&str> = self.places.first().map(|p| p.domain.as_str()).into_iter().collect();
        for (i, place) in self.places.iter().enumerate().skip(1) {
            if self.reading(i, now).current && !domains.contains(&place.domain.as_str()) {
                domains.push(place.domain.as_str());
            }
        }
        domains.len().saturating_sub(1)
    }

    /// The places that stop being fed when place `i` fails: everything below it in the feeding tree.
    pub fn dependents(&self, i: usize) -> Vec<usize> {
        let (mut out, mut queue): (Vec<usize>, Vec<usize>) = (Vec::new(), vec![i]);
        while let Some(cur) = queue.pop() {
            for e in self.edges.iter().filter(|e| e.from == cur && e.to != i) {
                if !out.contains(&e.to) {
                    out.push(e.to);
                    queue.push(e.to);
                }
            }
        }
        out.sort_unstable();
        out
    }
}

/// A place a job writes to: `(id, kind, host it lives on or None, online)`.
fn place_of(job: &Job) -> Option<(String, PlaceKind, Option<String>, bool)> {
    let (kind, place) = copy_of(job)?;
    let online = kind == CopyKind::Online;
    match job.kind.as_str() {
        jobs::KIND_REPLICATE if job.dest_host.as_deref() == Some(jobs::DEST_FILE) => Some((format!("file:{}", job.dest_dir.as_deref()?), PlaceKind::File, Some("local".to_string()), false)),
        jobs::KIND_REPLICATE => {
            let host = job.dest_host.clone()?;
            Some((format!("{host}:{}", job.dest_dataset.as_deref()?), PlaceKind::Replica, Some(host), false))
        }
        jobs::KIND_BACKUP => Some((place, PlaceKind::Backup, None, online)),
        jobs::KIND_MIRROR => Some((place, PlaceKind::Mirror, None, online)),
        _ => None,
    }
}

/// The failure domain of a place that is not a dataset: a remote is its own, named after its first part (`b2:bucket/x` is
/// `b2`); an `sftp:` repository is the machine it names.
fn remote_domain(id: &str) -> String {
    let rest = id.strip_prefix("rclone:").unwrap_or(id);
    let rest = rest.strip_prefix("sftp:").or_else(|| rest.strip_prefix("rest:")).unwrap_or(rest);
    rest.split([':', '/']).next().unwrap_or(rest).to_string()
}

/// Every body of the jobs: one per original that has at least one copy job (enabled, to another machine, for the whole
/// dataset), the weakest first (the oldest data among its places, a place never reached before any). `domain_of` says which
/// failure domain a host belongs to.
pub fn bodies(jobs: &[Job], runs: &[Run], settings: &ProtectionSettings, domain_of: &dyn Fn(&str) -> String, now: DateTime<Utc>) -> Vec<Body> {
    struct Raw {
        from: (String, String),
        to: String,
        to_place: (PlaceKind, Option<String>, bool),
        job: String,
        last_ok: Option<DateTime<Utc>>,
        current: bool,
    }
    let raws: Vec<Raw> = jobs
        .iter()
        .filter_map(|job| {
            let (id, kind, host, online) = place_of(job)?;
            let from = (job.host.clone(), job.dataset.clone()?);
            let state = job_state(job, runs, settings.for_host(&job.host).tolerance, now);
            let last_ok = crate::daemon::protection::last_ok(job, runs);
            Some(Raw { from, to: id, to_place: (kind, host, online), job: job.id.clone(), last_ok, current: state == JobState::Current })
        })
        .collect();
    let dataset_id = |n: &(String, String)| format!("{}:{}", n.0, n.1);
    // An original has a copy job out of it and none into it. (A dataset in a circle has no original.)
    let into: Vec<&str> = raws.iter().filter(|r| r.to_place.0 == PlaceKind::Replica).map(|r| r.to.as_str()).collect();
    let mut roots: Vec<(String, String)> = Vec::new();
    for r in &raws {
        if !into.contains(&dataset_id(&r.from).as_str()) && !roots.contains(&r.from) {
            roots.push(r.from.clone());
        }
    }
    let mut out = Vec::new();
    for root in roots {
        let mut places = vec![Place { id: dataset_id(&root), kind: PlaceKind::Original, domain: domain_of(&root.0), online: false }];
        let mut edges: Vec<Edge> = Vec::new();
        let mut queue = vec![0usize];
        while let Some(at) = queue.pop() {
            // The datasets this place's data is read from: the place itself, as `host:dataset`.
            let Some(id) = places.get(at).map(|p| p.id.clone()) else { continue };
            for r in raws.iter().filter(|r| dataset_id(&r.from) == id) {
                let to = if let Some(existing) = places.iter().position(|p| p.id == r.to) {
                    existing
                } else {
                    let (kind, host, online) = &r.to_place;
                    let domain = host.as_deref().map_or_else(|| remote_domain(&r.to), domain_of);
                    places.push(Place { id: r.to.clone(), kind: *kind, domain, online: *online });
                    queue.push(places.len() - 1);
                    places.len() - 1
                };
                // A place is fed by one edge; a second job writing to the same place is the same copy, not a second feed.
                if !edges.iter().any(|e| e.to == to) {
                    edges.push(Edge { from: at, to, job: r.job.clone(), last_ok: r.last_ok, current: r.current });
                }
            }
        }
        out.push(Body { places, edges });
    }
    out.sort_by_key(|b| {
        // The oldest data among the places; unknown (a hop never succeeded) is the weakest of all.
        let oldest = (1..b.places.len()).map(|i| b.reading(i, now).age).try_fold(Duration::zero(), |acc, age| age.map(|a| acc.max(a)));
        (oldest.is_some(), std::cmp::Reverse(oldest.unwrap_or_else(Duration::zero)), b.places.first().map(|p| p.id.clone()).unwrap_or_default())
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::history::RunResult;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-24T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn ok_at(job: &str, ago: Duration) -> Run {
        let at = now() - ago;
        Run { job_id: job.into(), run_id: format!("{job}-{}", ago.num_minutes()), started_at: at, scheduled_for: None, finished_at: Some(at), result: Some(RunResult::Ok), message: String::new(), catch_up: false, summary: None }
    }

    fn on(host: &str, job: Job) -> Job {
        Job { host: host.into(), ..job }
    }

    fn host_domain(host: &str) -> String {
        host.to_string()
    }

    fn graph(jobs: &[Job], runs: &[Run]) -> Vec<Body> {
        bodies(jobs, runs, &ProtectionSettings::default(), &host_domain, now())
    }

    /// `a:tank/data` replicated to `b:tank/copy` ten minutes ago, which `b` mirrors to `b2:bkt/x` three hours ago.
    fn chain() -> (Vec<Job>, Vec<Run>) {
        let jobs = vec![on("a", Job::test_replicate("r", "tank/data", "rep", "b", "tank/copy")), on("b", Job::test_mirror("m", "tank/copy", "m", "b2:bkt/x"))];
        let runs = vec![ok_at("r", Duration::minutes(10)), ok_at("m", Duration::hours(3))];
        (jobs, runs)
    }

    #[test]
    fn a_chain_is_one_body_with_the_original_first_and_its_places_in_order() {
        let (jobs, runs) = chain();
        let all = graph(&jobs, &runs);
        assert_eq!(all.len(), 1);
        let body = &all[0];
        assert_eq!(body.places.iter().map(|p| (p.id.as_str(), p.kind)).collect::<Vec<_>>(), [("a:tank/data", PlaceKind::Original), ("b:tank/copy", PlaceKind::Replica), ("b2:bkt/x", PlaceKind::Mirror)]);
        assert_eq!(body.edges.iter().map(|e| (e.from, e.to, e.job.as_str())).collect::<Vec<_>>(), [(0, 1, "r"), (1, 2, "m")]);
        assert_eq!((body.places[2].domain.as_str(), body.places[2].online, body.places[1].online), ("b2", true, false));
    }

    #[test]
    fn the_age_of_the_data_is_the_oldest_hop_on_the_path_and_the_original_is_live() {
        let (jobs, runs) = chain();
        let body = &graph(&jobs, &runs)[0];
        assert_eq!(body.reading(0, now()), Reading { age: Some(Duration::zero()), hops: 0, current: true });
        assert_eq!(body.reading(1, now()), Reading { age: Some(Duration::minutes(10)), hops: 1, current: true });
        assert_eq!(body.reading(2, now()).age, Some(Duration::hours(3)), "fed through a replica that was done 10 minutes ago, but by a mirror run 3 hours ago");
        assert_eq!(body.reading(2, now()).hops, 2);
        // The other order: the cloud was fed after the replica last ran, from what the replica then held.
        let runs = vec![ok_at("r", Duration::hours(5)), ok_at("m", Duration::minutes(5))];
        assert_eq!(graph(&jobs, &runs)[0].reading(2, now()).age, Some(Duration::hours(5)), "the cloud holds what the replica held, which is 5 hours old");
    }

    #[test]
    fn a_hop_that_never_succeeded_makes_the_age_unknown_and_the_place_not_current() {
        let (jobs, _) = chain();
        let runs = vec![ok_at("r", Duration::minutes(10))];
        let body = &graph(&jobs, &runs)[0];
        let cloud = body.reading(2, now());
        assert_eq!((cloud.age, cloud.current, cloud.hops), (None, false, 2));
        assert_eq!(body.reading(1, now()).age, Some(Duration::minutes(10)), "the replica is still known");
    }

    #[test]
    fn a_stopped_job_is_an_edge_that_is_not_current_and_the_place_behind_it_is_shown_but_not_current() {
        let (jobs, _) = chain();
        let runs = vec![ok_at("r", Duration::days(3)), ok_at("m", Duration::minutes(5))];
        let body = &graph(&jobs, &runs)[0];
        assert!(!body.reading(1, now()).current && !body.reading(2, now()).current, "an hourly-ish job three days old is stopped, and what hangs on it with it");
        assert_eq!(body.edges.len(), 2, "the stopped edge is still drawn");
    }

    #[test]
    fn the_body_tolerates_as_many_lost_domains_as_it_has_current_domains_besides_one() {
        let (jobs, runs) = chain();
        assert_eq!(graph(&jobs, &runs)[0].tolerates(now()), 2, "a, b and the cloud: any two can be lost");
        let one_box = |host: &str| if host == "a" || host == "b" { "rack1".to_string() } else { host.to_string() };
        assert_eq!(bodies(&jobs, &runs, &ProtectionSettings::default(), &one_box, now())[0].tolerates(now()), 1, "a and b share a failure");
        let runs = vec![ok_at("r", Duration::minutes(10))];
        assert_eq!(graph(&jobs, &runs)[0].tolerates(now()), 1, "a cloud copy that has never been made protects nothing");
    }

    #[test]
    fn what_stops_being_fed_when_a_place_fails_is_everything_below_it() {
        let (jobs, runs) = chain();
        let body = &graph(&jobs, &runs)[0];
        assert_eq!(body.dependents(0), [1, 2]);
        assert_eq!(body.dependents(1), [2], "the cloud is fed only through the replica");
        assert!(body.dependents(2).is_empty());
    }

    #[test]
    fn two_copies_of_one_original_are_two_branches_and_two_jobs_to_one_place_are_one_place() {
        let jobs = vec![
            on("a", Job::test_replicate("r", "tank/data", "rep", "b", "tank/copy")),
            on("a", Job::test_mirror("m1", "tank/data", "m", "b2:bkt/x")),
            on("a", Job::test_backup("k", "tank/data", "bk", "rclone:b2:bkt/x", "/pw")),
            on("a", Job::test_backup("k2", "tank/data", "bk", "sftp:nas:/r", "/pw")),
        ];
        let runs: Vec<Run> = jobs.iter().map(|j| ok_at(&j.id, Duration::minutes(30))).collect();
        let body = &graph(&jobs, &runs)[0];
        assert_eq!(body.places.len(), 5, "the original, the replica, the mirror, the rclone repository and the sftp repository");
        assert_eq!(body.dependents(0).len(), 4);
        assert!(body.dependents(1).is_empty(), "the branches do not feed each other");
        assert_eq!(body.places.iter().find(|p| p.id == "sftp:nas:/r").map(|p| p.domain.as_str()), Some("nas"), "an sftp repository is the machine it names");
    }

    #[test]
    fn a_dataset_with_no_copy_job_has_no_body_and_replications_in_a_circle_have_no_original() {
        let jobs = vec![on("a", Job::test_snapshot("s", "tank/x", "auto", 24))];
        assert!(graph(&jobs, &[]).is_empty());
        let circle = vec![on("a", Job::test_replicate("r1", "tank/a", "rep", "b", "tank/b")), on("b", Job::test_replicate("r2", "tank/b", "rep", "a", "tank/a"))];
        let runs = vec![ok_at("r1", Duration::minutes(5)), ok_at("r2", Duration::minutes(5))];
        assert!(graph(&circle, &runs).is_empty(), "neither dataset is the original");
    }

    #[test]
    fn the_weakest_body_comes_first_an_unknown_age_before_the_oldest_known_one() {
        let jobs = vec![on("a", Job::test_replicate("fresh", "tank/a", "rep", "b", "tank/a")), on("a", Job::test_replicate("old", "tank/b", "rep", "b", "tank/b")), on("a", Job::test_replicate("never", "tank/c", "rep", "b", "tank/c"))];
        let runs = vec![ok_at("fresh", Duration::minutes(5)), ok_at("old", Duration::hours(30))];
        let all = graph(&jobs, &runs);
        assert_eq!(all.iter().map(|b| b.places[0].id.as_str()).collect::<Vec<_>>(), ["a:tank/c", "a:tank/b", "a:tank/a"]);
    }
}
