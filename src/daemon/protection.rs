//! What one job gives one dataset: whether it is current, whether it gives snapshots, what kind of copy it makes and which
//! datasets it covers, and the judging that combines them into the datasets that are not protected.

use crate::config::ProtectionSettings;
use crate::daemon::cron::schedule_gap;
use crate::daemon::storage_check::StorageFile;
use crate::daemon::history::{Run, RunResult};
use crate::jobs::{self, Job};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// How a job stands: the same overdue rule the Dashboard uses, with the tolerance (in schedule gaps) chosen by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JobState {
    Current,
    Stopped { last_ok: DateTime<Utc> },
    Failing,
    NeverRan,
}

/// When the job last succeeded (a skipped run is not a success).
pub(crate) fn last_ok(job: &Job, runs: &[Run]) -> Option<DateTime<Utc>> {
    runs.iter().filter(|r| r.job_id == job.id && r.result == Some(RunResult::Ok)).map(|r| r.finished_at.unwrap_or(r.started_at)).max()
}

pub(crate) fn job_state(job: &Job, runs: &[Run], tolerance: i32, now: DateTime<Utc>) -> JobState {
    let mine = || runs.iter().filter(|r| r.job_id == job.id);
    let latest_finished = mine().filter(|r| r.result.is_some()).max_by_key(|r| r.finished_at.unwrap_or(r.started_at));
    if latest_finished.is_some_and(|r| r.result == Some(RunResult::Failed)) {
        return JobState::Failing;
    }
    let last_ok = last_ok(job, runs);
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
pub(crate) fn gives_snapshots(job: &Job) -> bool {
    job.enabled && job.kind == jobs::KIND_SNAPSHOT
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyKind {
    OnPremises,
    Online,
}

/// The copy an enabled job makes on another machine, with a string saying where it writes; `None` for anything else.
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
        // A backup or mirror with a `path` copies one folder of the dataset, not the dataset.
        jobs::KIND_BACKUP | jobs::KIND_MIRROR if job.path.as_deref().is_some_and(|p| !p.is_empty()) => None,
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
pub(crate) fn covers(job: &Job, host: &str, dataset: &str) -> bool {
    if job.host != host {
        return false;
    }
    let Some(own) = job.dataset.as_deref() else { return false };
    own == dataset || (job.kind == jobs::KIND_SNAPSHOT && job.recursive && dataset.strip_prefix(own).is_some_and(|rest| rest.starts_with('/')))
}

/// A dataset created less than this long ago is not judged yet.
pub const GRACE: chrono::Duration = chrono::Duration::hours(24);

/// A host whose dataset list is older than this says nothing about its datasets. The Dashboard has no staleness rule
/// for the dataset list itself (its quota-check line is about the loop, not one host), so this is our own.
pub const STALE_AFTER: chrono::Duration = chrono::Duration::days(2);

/// Whether a dataset is the operator's data or the operating system's (ranked lower, never left out).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    Data,
    Os,
}

/// OS data: the boot environments (`<pool>/ROOT` and below, and Ubuntu's `<pool>/BOOT` and below), `<pool>/tmp`, `<pool>/var` and below, `<pool>/usr/ports`,
/// `<pool>/usr/src`, `<pool>/usr/obj` (and below), and volumes named `swap` or `dump`. Everything else is data
/// (`<pool>/usr/home` too).
pub fn tier_of(dataset: &str, volume: bool) -> Tier {
    let below = dataset.split_once('/').map_or("", |(_, rest)| rest);
    let at_or_under = |base: &str| below == base || below.strip_prefix(base).is_some_and(|rest| rest.starts_with('/'));
    let last = dataset.rsplit('/').next().unwrap_or(dataset);
    let os = at_or_under("ROOT") || at_or_under("BOOT") || below == "tmp" || ["var", "usr/ports", "usr/src", "usr/obj"].into_iter().any(at_or_under) || (volume && (last == "swap" || last == "dump"));
    if os { Tier::Os } else { Tier::Data }
}

/// What is wrong with one half (snapshots or copies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    /// No enabled job gives it.
    Missing,
    /// A job gives it but has not succeeded yet (never ran, or nothing dates it).
    NotYet,
    /// A recursive snapshot job covers it only through its parent and has not run since it became recursive: nothing shows
    /// the dataset was snapshotted.
    NotRecursiveYet,
    /// The job stopped succeeding in time; its last success.
    Stopped { last_ok: DateTime<Utc> },
    /// The job's latest run failed.
    Failing,
}

/// The copy half: how many current copies on other machines there are, how many the level asks, and with none, why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyGap {
    pub have: u8,
    pub need: u8,
    pub online_have: u8,
    pub online_need: u8,
    /// With copies but too few separate failure domains: `(have, need)`, the original's domain counted.
    pub domains: Option<(u8, u8)>,
    /// With no current copy: `Missing` when no copy job covers the dataset, else the state of the worst-off covering job
    /// (failing first, then the most recently stopped). With some current copies: `Missing`.
    pub worst: Gap,
}

/// One dataset that is not protected, and what it lacks (at least one of the two halves is `Some`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unprotected {
    pub host: String,
    pub dataset: String,
    pub tier: Tier,
    pub snapshots: Option<Gap>,
    pub copies: Option<CopyGap>,
}

/// What to do about an unprotected dataset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fix {
    /// No snapshot job covers it: make one.
    NewSnapshotJob,
    /// It needs another copy: ask which kind.
    ChooseCopy,
    /// A job that should be helping exists but is not (never ran, stopped, failing): look at it in Jobs.
    GoToJob(String),
}

/// The fix for one dataset: the snapshot half first (nothing can be copied without snapshots), then the copies. A job
/// that already covers the dataset is gone to, never made again; a copy that is merely too few asks for another.
pub fn fix_for(gap: &Unprotected, jobs: &[Job]) -> Fix {
    let first = |wanted: &dyn Fn(&Job) -> bool| jobs.iter().find(|j| covers(j, &gap.host, &gap.dataset) && wanted(j)).map(|j| j.id.clone());
    if gap.snapshots.is_some() {
        return first(&gives_snapshots).map_or(Fix::NewSnapshotJob, Fix::GoToJob);
    }
    match (&gap.copies, first(&|j| copy_of(j).is_some())) {
        (Some(c), Some(id)) if c.have == 0 => Fix::GoToJob(id),
        _ => Fix::ChooseCopy,
    }
}

/// Every dataset of every host with a fresh list, past its grace, that lacks a current snapshot job or enough current
/// copies on other machines; sorted data first, then by host and dataset.
pub fn judge(file: &StorageFile, jobs: &[Job], runs: &[Run], settings: &ProtectionSettings, now: DateTime<Utc>) -> Vec<Unprotected> {
    // Worked out once per call: each enabled snapshot job and each enabled copy job, with where it stands.
    let standing = |job: &Job| gap_of(&job_state(job, runs, settings.for_host(&job.host).tolerance, now), now);
    let snap_jobs: Vec<(&Job, Option<Gap>, bool)> = jobs.iter().filter(|j| gives_snapshots(j)).map(|j| (j, standing(j), ran_recursively(j, runs))).collect();
    let made: Vec<(&Job, CopyKind, String)> = jobs.iter().filter_map(|j| copy_of(j).map(|(kind, place)| (j, kind, place))).collect();
    // Two jobs that write to the same place are one copy: each copy job carries the index of the first one writing there.
    let copy_jobs: Vec<CopyJob> = made
        .iter()
        .enumerate()
        .map(|(i, (job, kind, place))| CopyJob { job, kind: *kind, place: made.iter().take(i).position(|(_, _, p)| p == place).unwrap_or(i), standing: standing(job) })
        .collect();
    // The current replications to another host, each as source node -> destination node, with the copy job that makes it.
    // They chain datasets into one body of data: what is copied from the original, or from one of its replicas, is a copy of it.
    let links: Vec<(Node, Node, usize)> = copy_jobs
        .iter()
        .enumerate()
        .filter(|(_, c)| c.standing.is_none() && c.job.kind == jobs::KIND_REPLICATE && c.job.dest_host.as_deref() != Some(jobs::DEST_FILE))
        .filter_map(|(i, c)| Some(((c.job.host.clone(), c.job.dataset.clone()?), (c.job.dest_host.clone()?, c.job.dest_dataset.clone()?), i)))
        .collect();
    let parent_of = |n: &Node| links.iter().find(|l| l.1 == *n).map(|l| l.0.clone());
    // The original a replica comes from, following the chain up; `None` when the chain runs in a circle.
    let root_of = |n: &Node| {
        let (mut cur, mut seen) = (n.clone(), vec![n.clone()]);
        while let Some(p) = parent_of(&cur) {
            if seen.contains(&p) {
                return None;
            }
            seen.push(p.clone());
            cur = p;
        }
        Some(cur)
    };
    // Every replica below `root`, through however many hops.
    let members_of = |root: &Node| {
        let (mut out, mut queue): (Vec<Node>, Vec<Node>) = (Vec::new(), vec![root.clone()]);
        while let Some(cur) = queue.pop() {
            let next: Vec<Node> = links.iter().filter(|l| l.0 == cur && l.1 != *root).map(|l| l.1.clone()).filter(|n| !out.contains(n)).collect();
            for n in next {
                if !out.contains(&n) {
                    out.push(n.clone());
                    queue.push(n);
                }
            }
        }
        out
    };
    // The copies of one body of data: its replicas, plus every other current copy job of any of its datasets. A place that
    // two of them write to counts once. Cached by the original, since every replica asks the same question.
    let mut bodies: HashMap<Node, (u8, u8, Gap, u8)> = HashMap::new();
    let mut body_copies = |root: &Node, members: &[Node]| -> (u8, u8, Gap, u8) {
        *bodies.entry(root.clone()).or_insert_with(|| {
            let (mut counted, mut online_have, mut worst) = (Vec::<usize>::new(), 0usize, Gap::Missing);
            // The failure domains holding the data: the original's, every replica's and every counted copy job's place.
            let mut domains: Vec<String> = std::iter::once(root).chain(members).map(|n| settings.domain_of(&n.0)).collect();
            for copy in copy_jobs.iter().enumerate().filter(|(i, _)| !links.iter().any(|l| l.2 == *i)).map(|(_, c)| c) {
                if !std::iter::once(root).chain(members).any(|n| covers(copy.job, &n.0, &n.1)) {
                    continue;
                }
                match copy.standing {
                    None if !counted.contains(&copy.place) => {
                        counted.push(copy.place);
                        domains.extend(crate::daemon::safety::copy_domain(copy.job, &|h| settings.domain_of(h)));
                        online_have += usize::from(copy.kind == CopyKind::Online);
                    }
                    None => {}
                    Some(gap) => worst = worse(worst, gap),
                }
            }
            domains.sort();
            domains.dedup();
            (small(members.len() + counted.len()), small(online_have), worst, small(domains.len()))
        })
    };

    let mut out = Vec::new();
    for (host, result) in &file.hosts {
        if now - result.checked_at > STALE_AFTER {
            continue;
        }
        for seen in &result.seen {
            let dataset = seen.name.as_str();
            // A missing or future creation time counts as old.
            if seen.created.and_then(|c| DateTime::from_timestamp(c, 0)).is_some_and(|created| created <= now && now - created < GRACE) {
                continue;
            }
            let node: Node = (host.clone(), dataset.to_string());
            let tier = tier_of(dataset, seen.volume);
            // A replica of a current replication has the snapshots it was sent; a dataset in a circle of replications has no
            // original to lean on and is judged alone, its outgoing replications being its copies.
            let root = root_of(&node);
            let fed = root.is_some() && parent_of(&node).is_some();
            let (root, members) = if let Some(r) = root {
                let members = members_of(&r);
                (r, members)
            } else {
                let members = links.iter().filter(|l| l.0 == node && l.1 != node).map(|l| l.1.clone()).collect();
                (node, members)
            };

            let mut snapshots = if fed { None } else { Some(Gap::Missing) };
            for (job, gap, recursive_run) in snap_jobs.iter().filter(|(job, _, _)| !fed && covers(job, host, dataset)) {
                // A child is covered through its parent's recursion only if the job's newest success was a recursive one.
                let gap = match gap {
                    None if job.dataset.as_deref() != Some(dataset) && !recursive_run => Some(Gap::NotRecursiveYet),
                    other => *other,
                };
                let Some(gap) = gap else {
                    snapshots = None;
                    break;
                };
                snapshots = snapshots.map(|w| worse(w, gap));
            }

            let root_is_volume = file.hosts.get(&root.0).and_then(|h| h.seen.iter().find(|s| s.name == root.1)).is_some_and(|s| s.volume);
            // What the data must have is asked by the group of its original: where it lives.
            let (need, online_need) = match tier_of(&root.1, root_is_volume) {
                Tier::Data => (settings.for_host(&root.0).copies, settings.for_host(&root.0).online),
                Tier::Os => (1, 0),
            };
            let (have, online_have, worst, domains_have) = body_copies(&root, &members);
            let domains_need = if tier_of(&root.1, root_is_volume) == Tier::Data { settings.for_host(&root.0).domains } else { 1 };
            let domains = (have > 0 && domains_have < domains_need).then_some((domains_have, domains_need));
            let copies = (have < need || online_have < online_need || domains.is_some()).then_some(CopyGap { have, need, online_have, online_need, domains, worst: if have == 0 { worst } else { Gap::Missing } });

            if snapshots.is_some() || copies.is_some() {
                out.push(Unprotected { host: host.clone(), dataset: seen.name.clone(), tier, snapshots, copies });
            }
        }
    }
    out.sort_by(|a, b| (a.tier, &a.host, &a.dataset).cmp(&(b.tier, &b.host, &b.dataset)));
    out
}

/// Whether the newest success of the job was a recursive run (its message carries the mark): the only evidence that its
/// descendants were snapshotted.
fn ran_recursively(job: &Job, runs: &[Run]) -> bool {
    runs.iter()
        .filter(|r| r.job_id == job.id && r.result == Some(RunResult::Ok))
        .max_by_key(|r| r.finished_at.unwrap_or(r.started_at))
        .is_some_and(|r| r.message.contains(crate::daemon::snapshot::RECURSIVE_MARK))
}

/// A dataset on a host: `(host, dataset)`.
type Node = (String, String);

/// A copy job as the judging sees it: `place` is the index of the first copy job that writes to the same place.
struct CopyJob<'a> {
    job: &'a Job,
    kind: CopyKind,
    place: usize,
    standing: Option<Gap>,
}

/// `None` for a current job, else what it lacks. A job that never ran, or a bad schedule with nothing to date it by
/// (the state then says it stopped "now"), is `NotYet`, so nobody reads "stopped 0 minutes ago".
fn gap_of(state: &JobState, now: DateTime<Utc>) -> Option<Gap> {
    match state {
        JobState::Current => None,
        JobState::Failing => Some(Gap::Failing),
        JobState::Stopped { last_ok } if *last_ok < now => Some(Gap::Stopped { last_ok: *last_ok }),
        JobState::Stopped { .. } | JobState::NeverRan => Some(Gap::NotYet),
    }
}

/// The gap worth naming of two: a failing job first, then the most recently stopped, then one that has not run yet, then a
/// missing one.
fn worse(a: Gap, b: Gap) -> Gap {
    let rank = |g: Gap| match g {
        Gap::Missing => 0,
        Gap::NotYet | Gap::NotRecursiveYet => 1,
        Gap::Stopped { .. } => 2,
        Gap::Failing => 3,
    };
    match (a, b) {
        (Gap::Stopped { last_ok: x }, Gap::Stopped { last_ok: y }) => Gap::Stopped { last_ok: x.max(y) },
        _ if rank(a) >= rank(b) => a,
        _ => b,
    }
}

fn small(n: usize) -> u8 {
    u8::try_from(n).unwrap_or(u8::MAX)
}

impl Unprotected {
    /// What is missing, in a short phrase (`no snapshot job; no copy job`). `age` words a past time
    /// (`3 days ago`).
    pub fn describe(&self, age: &dyn Fn(DateTime<Utc>) -> String) -> String {
        let mut parts: Vec<String> = Vec::new();
        match self.snapshots {
            Some(Gap::Missing) => parts.push("no snapshot job".into()),
            Some(Gap::NotYet) => parts.push("snapshot job has not run yet".into()),
            Some(Gap::NotRecursiveYet) => parts.push("snapshot job has not run recursively yet".into()),
            Some(Gap::Stopped { last_ok }) => parts.push(format!("snapshots stopped {}", age(last_ok))),
            Some(Gap::Failing) => parts.push("snapshot job failing".into()),
            None => {}
        }
        if let Some(c) = &self.copies {
            if c.have == 0 && c.need >= 1 {
                parts.push(match c.worst {
                    Gap::Missing => "no copy job".into(),
                    Gap::NotYet | Gap::NotRecursiveYet => "copy job has not run yet".into(),
                    Gap::Stopped { last_ok } => format!("copy on another machine stopped {}", age(last_ok)),
                    Gap::Failing => "copy job failing".into(),
                });
            } else if c.have < c.need {
                parts.push(format!("{} of {} copies on other machines", c.have, c.need));
            }
            if let Some((have, need)) = c.domains {
                parts.push(if have == 1 { "all copies share one failure domain".into() } else { format!("copies in {have} of {need} failure domains") });
            }
            if c.online_have < c.online_need {
                parts.push(if c.online_need == 1 { "no online copy".into() } else { format!("{} of {} online copies", c.online_have, c.online_need) });
            }
        }
        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EmailTiming;
    use crate::daemon::storage_check::{Integrity, SeenDataset, StorageResult};
    use chrono::Duration;

    fn seen(name: &str, created: Option<i64>) -> SeenDataset {
        SeenDataset { name: name.into(), volume: false, created }
    }

    fn host_result(checked_ago: Duration, seen: Vec<SeenDataset>) -> StorageResult {
        StorageResult { checked_at: now() - checked_ago, datasets: vec![], seen, pools: vec![], integrity: Integrity::default(), locked: vec![], integrity_stale: false, zfs_version: None, error: None }
    }

    /// One host's datasets, all a week old, checked five minutes ago.
    fn storage(hosts: &[(&str, &[&str])]) -> StorageFile {
        let old = Some((now() - Duration::days(7)).timestamp());
        let mut file = StorageFile::default();
        for (host, names) in hosts {
            file.hosts.insert((*host).into(), host_result(Duration::minutes(5), names.iter().map(|n| seen(n, old)).collect()));
        }
        file
    }

    fn level(copies: u8, online: u8) -> ProtectionSettings {
        ProtectionSettings { copies, online, tolerance: 2, email: EmailTiming::Immediate, ..ProtectionSettings::default() }
    }

    /// A run that succeeded ten minutes ago for each job: all current.
    fn ok_runs(jobs: &[Job]) -> Vec<Run> {
        jobs.iter().map(|j| run(&j.id, Duration::minutes(10), Some(RunResult::Ok))).collect()
    }

    fn snap(id: &str, dataset: &str) -> Job {
        Job::test_snapshot(id, dataset, "auto", 24)
    }

    fn repl(id: &str, dataset: &str, dest_host: &str, dest_dataset: &str) -> Job {
        Job::test_replicate(id, dataset, "rep", dest_host, dest_dataset)
    }

    fn bk(id: &str, dataset: &str, repo: &str) -> Job {
        Job::test_backup(id, dataset, "bk", repo, "/pw")
    }

    fn words(found: &[Unprotected]) -> Vec<String> {
        found.iter().map(|u| format!("{}: {}: {}", u.host, u.dataset, u.describe(&|_| "3 days ago".to_string()))).collect()
    }

    #[test]
    fn tier_of_recognises_the_os_datasets() {
        for os in ["zroot/ROOT", "zroot/ROOT/default", "rpool/ROOT/ubuntu_x/var", "zroot/tmp", "zroot/var", "zroot/var/log", "zroot/usr/ports", "zroot/usr/ports/distfiles", "zroot/usr/src", "zroot/usr/obj"] {
            assert_eq!(tier_of(os, false), Tier::Os, "{os}");
        }
        assert_eq!(tier_of("rpool/swap", true), Tier::Os);
        for boot in ["bpool/BOOT", "bpool/BOOT/ubuntu_abc123"] {
            assert_eq!(tier_of(boot, false), Tier::Os, "{boot}: Ubuntu's boot pool is like ROOT");
        }
        assert_eq!(tier_of("tank/photos/BOOT", false), Tier::Data);
        assert_eq!(tier_of("rpool/dump", true), Tier::Os);
        for data in ["zroot/usr/home", "zroot/usr/home/me", "tank/photos", "tank/var2", "tank/tmpfiles", "zroot", "zroot/usr", "tank/photos/ROOT", "zroot/usr/portsx"] {
            assert_eq!(tier_of(data, false), Tier::Data, "{data}");
        }
        assert_eq!(tier_of("rpool/swap", false), Tier::Data, "a filesystem named swap is data");
        assert_eq!(tier_of("tank/vm/disk0", true), Tier::Data);
        assert!(Tier::Data < Tier::Os);
    }

    #[test]
    fn basic_is_one_current_snapshot_job_and_one_current_copy() {
        let file = storage(&[("local", &["tank/data"])]);
        let both = vec![snap("s", "tank/data"), repl("r", "tank/data", "fbsd16", "tank/copy")];
        assert!(judge(&file, &both, &ok_runs(&both), &level(1, 0), now()).is_empty());

        let only_snap = vec![snap("s", "tank/data")];
        let found = judge(&file, &only_snap, &ok_runs(&only_snap), &level(1, 0), now());
        assert_eq!(found, vec![Unprotected { host: "local".into(), dataset: "tank/data".into(), tier: Tier::Data, snapshots: None, copies: Some(CopyGap { have: 0, need: 1, online_have: 0, online_need: 0, domains: None, worst: Gap::Missing }) }]);
        assert_eq!(words(&found), ["local: tank/data: no copy job"]);

        let only_copy = vec![repl("r", "tank/data", "fbsd16", "tank/copy")];
        let found = judge(&file, &only_copy, &ok_runs(&only_copy), &level(1, 0), now());
        assert_eq!((found[0].snapshots, found[0].copies), (Some(Gap::Missing), None));
        assert_eq!(words(&found), ["local: tank/data: no snapshot job"]);

        let found = judge(&file, &[], &[], &level(1, 0), now());
        assert_eq!(words(&found), ["local: tank/data: no snapshot job; no copy job"]);

        // A local restic repository and a same-host replication are not copies on another machine.
        let local = vec![snap("s", "tank/data"), bk("b", "tank/data", "/mnt/repo"), repl("r", "tank/data", "local", "tank/copy")];
        assert_eq!(words(&judge(&file, &local, &ok_runs(&local), &level(1, 0), now())), ["local: tank/data: no copy job"]);

        // A disabled job gives nothing, whatever its history says.
        let mut off = both.clone();
        off[1].enabled = false;
        assert_eq!(words(&judge(&file, &off, &ok_runs(&off), &level(1, 0), now())), ["local: tank/data: no copy job"]);
    }

    #[test]
    fn careful_needs_three_copies_one_of_them_online() {
        let file = storage(&[("local", &["tank/data"])]);
        let mut jobs = vec![snap("s", "tank/data"), repl("r", "tank/data", "fbsd16", "tank/copy"), bk("b", "tank/data", "sftp:nas:/restic")];
        let found = judge(&file, &jobs, &ok_runs(&jobs), &level(3, 1), now());
        assert_eq!(found[0].copies, Some(CopyGap { have: 2, need: 3, online_have: 0, online_need: 1, domains: None, worst: Gap::Missing }));
        assert_eq!(words(&found), ["local: tank/data: 2 of 3 copies on other machines; no online copy"]);
        jobs.push(Job::test_mirror("m", "tank/data", "m", "b2:bucket/data"));
        assert!(judge(&file, &jobs, &ok_runs(&jobs), &level(3, 1), now()).is_empty());
    }

    #[test]
    fn custom_counts_what_it_is_told() {
        let file = storage(&[("local", &["tank/data"])]);
        let one = vec![snap("s", "tank/data"), bk("b", "tank/data", "sftp:nas:/restic")];
        assert_eq!(words(&judge(&file, &one, &ok_runs(&one), &level(2, 0), now())), ["local: tank/data: 1 of 2 copies on other machines"]);
        assert_eq!(words(&judge(&file, &one, &ok_runs(&one), &level(1, 1), now())), ["local: tank/data: no online copy"]);
        assert!(judge(&file, &one, &ok_runs(&one), &level(1, 0), now()).is_empty());
        let three = vec![snap("s", "tank/data"), bk("b", "tank/data", "sftp:nas:/restic"), bk("b2", "tank/data", "s3:host/bucket"), repl("r", "tank/data", "fbsd16", "tank/copy")];
        let found = judge(&file, &three, &ok_runs(&three), &level(3, 2), now());
        assert_eq!(found[0].copies, Some(CopyGap { have: 3, need: 3, online_have: 1, online_need: 2, domains: None, worst: Gap::Missing }));
        assert_eq!(words(&found), ["local: tank/data: 1 of 2 online copies"]);
        assert!(judge(&file, &three, &ok_runs(&three), &level(3, 1), now()).is_empty());
    }

    #[test]
    fn two_jobs_to_one_destination_are_one_copy() {
        let file = storage(&[("local", &["tank/data"])]);
        let jobs = vec![snap("s", "tank/data"), bk("b1", "tank/data", "sftp:nas:/restic"), bk("b2", "tank/data", "sftp:nas:/restic"), repl("r1", "tank/data", "fbsd16", "tank/copy"), repl("r2", "tank/data", "fbsd16", "tank/copy")];
        let found = judge(&file, &jobs, &ok_runs(&jobs), &level(3, 0), now());
        assert_eq!(found[0].copies.map(|c| c.have), Some(2));
        let mirrors = vec![snap("s", "tank/data"), Job::test_mirror("m1", "tank/data", "m", "b2:x"), Job::test_mirror("m2", "tank/data", "n", "b2:x")];
        let found = judge(&file, &mirrors, &ok_runs(&mirrors), &level(2, 2), now());
        assert_eq!(found[0].copies.map(|c| (c.have, c.online_have)), Some((1, 1)));
    }

    #[test]
    fn each_missing_half_is_named() {
        let at = now() - Duration::days(3);
        let age = |t: DateTime<Utc>| if t == at { "3 days ago".to_string() } else { format!("wrong time {t}") };
        let item = |snapshots: Option<Gap>, copies: Option<CopyGap>| Unprotected { host: "h".into(), dataset: "tank/d".into(), tier: Tier::Data, snapshots, copies };
        let none = |worst: Gap| Some(CopyGap { have: 0, need: 1, online_have: 0, online_need: 0, domains: None, worst });
        assert_eq!(item(Some(Gap::Missing), None).describe(&age), "no snapshot job");
        assert_eq!(item(Some(Gap::Stopped { last_ok: at }), None).describe(&age), "snapshots stopped 3 days ago");
        assert_eq!(item(Some(Gap::Failing), None).describe(&age), "snapshot job failing");
        assert_eq!(item(None, none(Gap::Missing)).describe(&age), "no copy job");
        assert_eq!(item(Some(Gap::NotYet), None).describe(&age), "snapshot job has not run yet");
        assert_eq!(item(None, none(Gap::NotYet)).describe(&age), "copy job has not run yet");
        assert_eq!(item(Some(Gap::Missing), none(Gap::NotYet)).describe(&age), "no snapshot job; copy job has not run yet");
        assert_eq!(item(None, none(Gap::Stopped { last_ok: at })).describe(&age), "copy on another machine stopped 3 days ago");
        assert_eq!(item(None, none(Gap::Failing)).describe(&age), "copy job failing");
        assert_eq!(item(None, Some(CopyGap { have: 1, need: 3, online_have: 0, online_need: 0, domains: None, worst: Gap::Missing })).describe(&age), "1 of 3 copies on other machines");
        assert_eq!(item(None, Some(CopyGap { have: 1, need: 1, online_have: 0, online_need: 1, domains: None, worst: Gap::Missing })).describe(&age), "no online copy");
        assert_eq!(item(None, Some(CopyGap { have: 3, need: 3, online_have: 0, online_need: 2, domains: None, worst: Gap::Missing })).describe(&age), "0 of 2 online copies");
        assert_eq!(item(Some(Gap::Failing), Some(CopyGap { have: 0, need: 3, online_have: 0, online_need: 1, domains: None, worst: Gap::Stopped { last_ok: at } })).describe(&age), "snapshot job failing; copy on another machine stopped 3 days ago; no online copy");

        // Through the judging: each job state becomes its gap.
        let file = storage(&[("local", &["tank/data"])]);
        let jobs = vec![snap("s", "tank/data"), bk("b", "tank/data", "sftp:nas:/r")];
        let judged = |runs: &[Run], jobs: &[Job]| judge(&file, jobs, runs, &level(1, 0), now());
        let failing = vec![run("s", Duration::minutes(10), Some(RunResult::Failed)), run("b", Duration::minutes(10), Some(RunResult::Failed))];
        let found = judged(&failing, &jobs);
        assert_eq!((found[0].snapshots, found[0].copies.map(|c| c.worst)), (Some(Gap::Failing), Some(Gap::Failing)));
        let stopped = vec![run("s", Duration::days(3), Some(RunResult::Ok)), run("b", Duration::days(3), Some(RunResult::Ok))];
        let found = judged(&stopped, &jobs);
        assert_eq!((found[0].snapshots, found[0].copies.map(|c| c.worst)), (Some(Gap::Stopped { last_ok: at }), Some(Gap::Stopped { last_ok: at })));
        assert_eq!(words(&found), ["local: tank/data: snapshots stopped 3 days ago; copy on another machine stopped 3 days ago"]);
        let found = judged(&[], &jobs);
        assert_eq!(words(&found), ["local: tank/data: snapshot job has not run yet; copy job has not run yet"], "a job that never ran gives nothing yet, and says so");
        // A bad schedule with no success and no creation time: not yet, never "stopped 0 minutes ago".
        let mut bad = jobs.clone();
        bad[0].schedule = "not a schedule".into();
        bad[1].schedule = "not a schedule".into();
        let found = judged(&[], &bad);
        assert_eq!((found[0].snapshots, found[0].copies.map(|c| c.worst)), (Some(Gap::NotYet), Some(Gap::NotYet)));
        // Of two covering copy jobs, a failing one is named before a stopped one; of two stopped, the latest.
        let two = vec![snap("s", "tank/data"), bk("b", "tank/data", "sftp:nas:/r"), bk("c", "tank/data", "sftp:nas2:/r")];
        let runs = vec![run("s", Duration::minutes(10), Some(RunResult::Ok)), run("b", Duration::days(3), Some(RunResult::Ok)), run("c", Duration::minutes(10), Some(RunResult::Failed))];
        assert_eq!(judged(&runs, &two)[0].copies.map(|c| c.worst), Some(Gap::Failing));
        let runs = vec![run("s", Duration::minutes(10), Some(RunResult::Ok)), run("b", Duration::days(5), Some(RunResult::Ok)), run("c", Duration::days(3), Some(RunResult::Ok))];
        assert_eq!(judged(&runs, &two)[0].copies.map(|c| c.worst), Some(Gap::Stopped { last_ok: at }));
    }

    fn places(found: &[Unprotected]) -> Vec<String> {
        found.iter().map(|u| format!("{}:{}", u.host, u.dataset)).collect()
    }

    fn mirror_on(host: &str, id: &str, dataset: &str, remote: &str) -> Job {
        Job { host: host.into(), ..Job::test_mirror(id, dataset, "m", remote) }
    }

    #[test]
    fn without_a_cloud_copy_the_source_and_its_replica_both_warn_when_the_level_asks_for_one() {
        let file = storage(&[("local", &["tank/data"]), ("fbsd16", &["tank/copy"])]);
        let jobs = vec![snap("s", "tank/data"), repl("r", "tank/data", "fbsd16", "tank/copy")];
        let found = judge(&file, &jobs, &ok_runs(&jobs), &level(2, 1), now());
        assert_eq!(places(&found), ["fbsd16:tank/copy", "local:tank/data"]);
        let gap = Some(CopyGap { have: 1, need: 2, online_have: 0, online_need: 1, domains: None, worst: Gap::Missing });
        assert!(found.iter().all(|u| u.copies == gap && u.snapshots.is_none()), "the same verdict for the data on both machines: {found:?}");
        assert_eq!(words(&found), ["fbsd16: tank/copy: 1 of 2 copies on other machines; no online copy", "local: tank/data: 1 of 2 copies on other machines; no online copy"]);
    }

    #[test]
    fn a_cloud_copy_made_from_the_replica_counts_for_the_source_and_one_made_from_the_source_for_the_replica() {
        let file = storage(&[("local", &["tank/data"]), ("fbsd16", &["tank/copy"])]);
        for cloud in [mirror_on("fbsd16", "m", "tank/copy", "b2:bkt/photos"), mirror_on("local", "m", "tank/data", "b2:bkt/photos")] {
            let jobs = vec![snap("s", "tank/data"), repl("r", "tank/data", "fbsd16", "tank/copy"), cloud];
            assert!(judge(&file, &jobs, &ok_runs(&jobs), &level(2, 1), now()).is_empty(), "the cloud has the data, whichever machine sent it");
        }
    }

    #[test]
    fn the_cloud_copy_has_to_be_current_to_count() {
        let file = storage(&[("local", &["tank/data"]), ("fbsd16", &["tank/copy"])]);
        let jobs = vec![snap("s", "tank/data"), repl("r", "tank/data", "fbsd16", "tank/copy"), mirror_on("fbsd16", "m", "tank/copy", "b2:bkt/photos")];
        let mut runs = ok_runs(&jobs);
        runs.retain(|r| r.job_id != "m");
        runs.push(run("m", Duration::days(3), Some(RunResult::Ok)));
        let found = judge(&file, &jobs, &runs, &level(2, 1), now());
        assert_eq!(places(&found), ["fbsd16:tank/copy", "local:tank/data"], "a stopped cloud job is no copy");
    }

    #[test]
    fn one_cloud_place_counts_once_however_many_datasets_of_the_chain_write_to_it() {
        let file = storage(&[("local", &["tank/data"]), ("fbsd16", &["tank/copy"])]);
        let jobs = vec![snap("s", "tank/data"), repl("r", "tank/data", "fbsd16", "tank/copy"), mirror_on("local", "m1", "tank/data", "b2:bkt/photos"), mirror_on("fbsd16", "m2", "tank/copy", "b2:bkt/photos")];
        let found = judge(&file, &jobs, &ok_runs(&jobs), &level(3, 1), now());
        assert_eq!(found.len(), 2, "the replica and one cloud place are two copies, not three");
        assert!(found.iter().all(|u| u.copies.is_some_and(|c| c.have == 2 && c.online_have == 1)), "{found:?}");
    }

    #[test]
    fn a_chain_of_replicas_is_one_body_of_data() {
        let file = storage(&[("local", &["tank/data"]), ("fbsd16", &["tank/copy"]), ("cold", &["tank/arch"])]);
        let jobs = vec![snap("s", "tank/data"), repl("r1", "tank/data", "fbsd16", "tank/copy"), Job { host: "fbsd16".into(), ..repl("r2", "tank/copy", "cold", "tank/arch") }];
        let runs = ok_runs(&jobs);
        assert!(judge(&file, &jobs, &runs, &level(2, 0), now()).is_empty(), "two replicas are two copies");
        assert_eq!(places(&judge(&file, &jobs, &runs, &level(3, 0), now())), ["cold:tank/arch", "fbsd16:tank/copy", "local:tank/data"]);
    }

    #[test]
    fn a_replication_that_is_not_current_breaks_the_chain_and_the_replica_stands_alone() {
        let file = storage(&[("local", &["tank/data"]), ("fbsd16", &["tank/copy"])]);
        let jobs = vec![snap("s", "tank/data"), repl("r", "tank/data", "fbsd16", "tank/copy")];
        let runs = vec![run("s", Duration::minutes(10), Some(RunResult::Ok)), run("r", Duration::days(3), Some(RunResult::Ok))];
        let found = judge(&file, &jobs, &runs, &level(1, 0), now());
        assert_eq!(places(&found), ["fbsd16:tank/copy", "local:tank/data"]);
        let by = |p: &str| found.iter().find(|u| format!("{}:{}", u.host, u.dataset) == p).unwrap();
        assert_eq!(by("fbsd16:tank/copy").snapshots, Some(Gap::Missing), "the replica is no longer fed, so nothing snapshots it");
        assert!(matches!(by("local:tank/data").copies, Some(CopyGap { have: 0, worst: Gap::Stopped { .. }, .. })));
    }

    #[test]
    fn replications_in_a_circle_have_no_original_so_each_is_judged_on_its_own() {
        let file = storage(&[("local", &["tank/a"]), ("fbsd16", &["tank/b"])]);
        let jobs = vec![repl("r1", "tank/a", "fbsd16", "tank/b"), Job { host: "fbsd16".into(), ..repl("r2", "tank/b", "local", "tank/a") }];
        let found = judge(&file, &jobs, &ok_runs(&jobs), &level(1, 0), now());
        assert_eq!(places(&found), ["fbsd16:tank/b", "local:tank/a"], "neither is exempt: each needs its own snapshots");
        assert!(found.iter().all(|u| u.snapshots == Some(Gap::Missing) && u.copies.is_none()), "its outgoing replication is its copy: {found:?}");
    }

    /// The settings of a `config.toml` whose hosts are `a` (tag `db`), `b` (tag `web`), `c` (no tag).
    fn grouped(toml: &str) -> ProtectionSettings {
        let text = format!("[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\ntag = \"db\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\ntag = \"web\"\n\n[[hosts]]\nname = \"c\"\nssh_target = \"u@c\"\n\n{toml}");
        let (settings, problems) = toml::from_str::<crate::config::Config>(&text).unwrap().protection_settings();
        assert!(problems.is_empty(), "{problems:?}");
        settings
    }

    fn on(host: &str, job: Job) -> Job {
        Job { host: host.into(), ..job }
    }

    #[test]
    fn copies_on_hosts_that_share_a_site_are_one_failure_domain_and_the_requirement_asks_for_more() {
        let file = storage(&[("a", &["tank/x"]), ("b", &["tank/x"])]);
        let jobs = vec![on("a", snap("s", "tank/x")), on("a", repl("r", "tank/x", "b", "tank/x"))];
        let runs = ok_runs(&jobs);
        let config = |sites: bool, domains: u8| {
            let site = if sites { "site = \"rack1\"\n" } else { "" };
            let text = format!("[protection]\ndomains = {domains}\n\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n{site}\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n{site}");
            toml::from_str::<crate::config::Config>(&text).unwrap().protection_settings().0
        };
        let found = |sites: bool, domains: u8| judge(&file, &jobs, &runs, &config(sites, domains), now());
        assert!(found(false, 1).is_empty() && found(true, 1).is_empty(), "1 asks nothing");
        assert!(found(false, 2).is_empty(), "two hosts are two domains");
        let shared = found(true, 2);
        assert_eq!(places(&shared), ["a:tank/x", "b:tank/x"], "the original and its replica both share the one domain");
        assert_eq!(shared[0].copies.map(|c| c.domains), Some(Some((1, 2))));
        assert_eq!(words(&shared), ["a: tank/x: all copies share one failure domain", "b: tank/x: all copies share one failure domain"]);
        assert_eq!(fix_for(&shared[0], &jobs), Fix::ChooseCopy, "another copy elsewhere is the fix");
    }

    #[test]
    fn a_cloud_copy_is_its_own_failure_domain_and_a_dataset_with_no_copy_is_not_asked_about_domains() {
        let file = storage(&[("a", &["tank/x"]), ("b", &["tank/x"])]);
        let settings = grouped("[protection]\ndomains = 3\n");
        let jobs = vec![on("a", snap("s", "tank/x")), on("a", repl("r", "tank/x", "b", "tank/x")), on("a", bk("k", "tank/x", "rclone:b2:bkt/x"))];
        let runs = ok_runs(&jobs);
        assert!(judge(&file, &jobs, &runs, &settings, now()).iter().all(|u| u.copies.is_none()), "a, b and b2 are three domains");
        let bare = vec![on("a", snap("s", "tank/x"))];
        let found = judge(&file, &bare, &ok_runs(&bare), &settings, now());
        let a = found.iter().find(|u| u.host == "a").unwrap();
        assert_eq!(a.copies.map(|c| (c.have, c.domains)), Some((0, None)), "only the missing copy is said, not the domains");
    }

    #[test]
    fn the_original_s_group_decides_how_many_domains_its_data_must_span() {
        let file = storage(&[("a", &["tank/x"]), ("c", &["tank/y"]), ("b", &["tank/x", "tank/y"])]);
        let jobs = vec![on("a", snap("s1", "tank/x")), on("a", repl("r1", "tank/x", "b", "tank/x")), on("c", snap("s2", "tank/y")), on("c", repl("r2", "tank/y", "b", "tank/y"))];
        let settings = grouped("[groups.db.protection]\ndomains = 3\n");
        let found = judge(&file, &jobs, &ok_runs(&jobs), &settings, now());
        assert!(found.iter().any(|u| u.dataset == "tank/x" && u.copies.is_some_and(|c| c.domains == Some((2, 3)))), "{found:?}");
        assert!(found.iter().all(|u| u.dataset != "tank/y" || u.copies.is_none()), "c is in no group: the global 1 asks nothing");
    }

    #[test]
    fn each_group_asks_its_own_level_of_the_data_it_holds() {
        let file = storage(&[("a", &["tank/x"]), ("c", &["tank/y"])]);
        let jobs = vec![on("a", snap("s1", "tank/x")), on("a", bk("b1", "tank/x", "sftp:nas:/r1")), on("c", snap("s2", "tank/y")), on("c", bk("b2", "tank/y", "sftp:nas:/r2"))];
        let settings = grouped("[groups.db.protection]\nlevel = \"careful\"\n");
        let found = judge(&file, &jobs, &ok_runs(&jobs), &settings, now());
        assert_eq!(places(&found), ["a:tank/x"], "the db group wants three copies and one online; the others one copy: {found:?}");
        assert_eq!(found[0].copies.map(|c| (c.have, c.need, c.online_need)), Some((1, 3, 1)));
    }

    #[test]
    fn a_chain_across_groups_is_judged_by_the_group_of_its_original() {
        let file = storage(&[("a", &["tank/x"]), ("c", &["tank/copy"])]);
        let jobs = vec![on("a", snap("s", "tank/x")), on("a", repl("r", "tank/x", "c", "tank/copy"))];
        let careful_original = grouped("[groups.db.protection]\nlevel = \"careful\"\n");
        assert_eq!(places(&judge(&file, &jobs, &ok_runs(&jobs), &careful_original, now())), ["a:tank/x", "c:tank/copy"], "the original's group asks three copies of both");
        // The same chain the other way: the original in the basic group, its replica on a careful host.
        let file = storage(&[("c", &["tank/x"]), ("a", &["tank/copy"])]);
        let jobs = vec![on("c", snap("s", "tank/x")), on("c", repl("r", "tank/x", "a", "tank/copy"))];
        assert!(judge(&file, &jobs, &ok_runs(&jobs), &careful_original, now()).is_empty(), "the original is on a basic host, so one copy is enough, wherever the replica sits");
    }

    #[test]
    fn how_recent_is_the_groups_of_the_host_that_runs_the_job() {
        let file = storage(&[("a", &["tank/x"]), ("c", &["tank/y"])]);
        let jobs = vec![on("a", snap("s1", "tank/x")), on("c", snap("s2", "tank/y"))];
        let runs = vec![run("s1", Duration::minutes(150), Some(RunResult::Ok)), run("s2", Duration::minutes(150), Some(RunResult::Ok))];
        let settings = grouped("[groups.db.protection]\nrecent = \"relaxed\"\n");
        let found = judge(&file, &jobs, &runs, &settings, now());
        let by = |p: &str| found.iter().find(|u| format!("{}:{}", u.host, u.dataset) == p);
        assert!(by("a:tank/x").is_none_or(|u| u.snapshots.is_none()), "an hourly job 2.5 hours old is current for a group that allows four intervals");
        assert!(matches!(by("c:tank/y").and_then(|u| u.snapshots), Some(Gap::Stopped { .. })), "and stopped for the global two");
    }

    #[test]
    fn a_replica_is_protected_by_its_current_source_job() {
        let file = storage(&[("local", &["tank/data"]), ("fbsd16", &["tank/copy", "tank/other"])]);
        let jobs = vec![snap("s", "tank/data"), repl("r", "tank/data", "fbsd16", "tank/copy")];
        let found = judge(&file, &jobs, &ok_runs(&jobs), &level(3, 1), now());
        assert_eq!(found.iter().map(|u| (u.host.as_str(), u.dataset.as_str())).collect::<Vec<_>>(), [("fbsd16", "tank/copy"), ("fbsd16", "tank/other"), ("local", "tank/data")], "one copy is short of three for the data, wherever it is held");
        let found = judge(&file, &jobs, &ok_runs(&jobs), &level(1, 0), now());
        assert_eq!(found.iter().map(|u| u.dataset.as_str()).collect::<Vec<_>>(), ["tank/other"], "at Basic the source and its replica are both protected");
        let stopped = vec![run("s", Duration::minutes(10), Some(RunResult::Ok)), run("r", Duration::days(3), Some(RunResult::Ok))];
        let found = judge(&file, &jobs, &stopped, &level(1, 0), now());
        assert!(found.iter().any(|u| u.host == "fbsd16" && u.dataset == "tank/copy"), "a stopped source job protects nothing");
        let mut off = jobs.clone();
        off[1].enabled = false;
        let found = judge(&file, &off, &ok_runs(&off), &level(1, 0), now());
        assert!(found.iter().any(|u| u.host == "fbsd16" && u.dataset == "tank/copy"), "nor does a disabled one");
    }

    #[test]
    fn grace_staleness_and_recency_boundaries() {
        let at = |ago: Duration| Some((now() - ago).timestamp());
        let mut file = StorageFile::default();
        let list = vec![seen("tank/h23", at(Duration::hours(23))), seen("tank/h24", at(Duration::hours(24))), seen("tank/h25", at(Duration::hours(25))), seen("tank/none", None), seen("tank/future", at(Duration::hours(-5)))];
        file.hosts.insert("local".into(), host_result(Duration::days(2), list));
        file.hosts.insert("stale".into(), host_result(Duration::days(2) + Duration::minutes(1), vec![seen("tank/x", None)]));
        let names: Vec<String> = judge(&file, &[], &[], &level(1, 0), now()).into_iter().map(|u| format!("{}:{}", u.host, u.dataset)).collect();
        assert_eq!(names, ["local:tank/future", "local:tank/h24", "local:tank/h25", "local:tank/none"]);

        // How recent: an hourly snapshot job last good 90 minutes ago is current at normal and relaxed, stopped at strict.
        let file = storage(&[("local", &["tank/data"])]);
        let jobs = vec![snap("s", "tank/data"), bk("b", "tank/data", "sftp:nas:/r")];
        let runs = vec![run("s", Duration::minutes(90), Some(RunResult::Ok)), run("b", Duration::minutes(10), Some(RunResult::Ok))];
        let with = |tolerance: i32| judge(&file, &jobs, &runs, &ProtectionSettings { tolerance, ..level(1, 0) }, now());
        assert_eq!(with(1)[0].snapshots, Some(Gap::Stopped { last_ok: now() - Duration::minutes(90) }));
        assert!(with(2).is_empty() && with(4).is_empty());
        let runs = vec![run("s", Duration::minutes(239), Some(RunResult::Ok)), run("b", Duration::minutes(10), Some(RunResult::Ok))];
        let with = |tolerance: i32| judge(&file, &jobs, &runs, &ProtectionSettings { tolerance, ..level(1, 0) }, now());
        assert!(matches!(with(2)[0].snapshots, Some(Gap::Stopped { .. })));
        assert!(with(4).is_empty());
    }

    #[test]
    fn a_stale_or_empty_host_says_nothing() {
        assert!(judge(&StorageFile::default(), &[], &[], &level(1, 0), now()).is_empty());
        let mut file = StorageFile::default();
        file.hosts.insert("empty".into(), host_result(Duration::minutes(5), vec![]));
        file.hosts.insert("old".into(), host_result(Duration::days(3), vec![seen("tank/x", None)]));
        assert!(judge(&file, &[], &[], &level(1, 0), now()).is_empty());
    }

    #[test]
    fn os_data_needs_only_one_copy_at_every_level() {
        let file = storage(&[("local", &["zroot/var/log"])]);
        let jobs = vec![snap("s", "zroot/var/log"), bk("b", "zroot/var/log", "sftp:nas:/r")];
        for (copies, online) in [(1, 0), (3, 1), (2, 2), (3, 2)] {
            assert!(judge(&file, &jobs, &ok_runs(&jobs), &level(copies, online), now()).is_empty(), "{copies}/{online}");
        }
        let found = judge(&file, &jobs[..1], &ok_runs(&jobs[..1]), &level(3, 1), now());
        assert_eq!((found[0].tier, found[0].copies), (Tier::Os, Some(CopyGap { have: 0, need: 1, online_have: 0, online_need: 0, domains: None, worst: Gap::Missing })));
        assert_eq!(words(&found), ["local: zroot/var/log: no copy job"]);
    }

    #[test]
    fn output_is_sorted_data_first() {
        let file = storage(&[("zeta", &["zroot/var", "tank/b", "tank/a"]), ("alpha", &["zroot/tmp", "tank/z"])]);
        let found: Vec<(Tier, String, String)> = judge(&file, &[], &[], &level(1, 0), now()).into_iter().map(|u| (u.tier, u.host, u.dataset)).collect();
        let want = [(Tier::Data, "alpha", "tank/z"), (Tier::Data, "zeta", "tank/a"), (Tier::Data, "zeta", "tank/b"), (Tier::Os, "alpha", "zroot/tmp"), (Tier::Os, "zeta", "zroot/var")];
        assert_eq!(found, want.map(|(t, h, d)| (t, h.to_string(), d.to_string())));
    }

    #[test]
    fn a_recursive_snapshot_job_covers_descendants_and_copies_do_not() {
        let file = storage(&[("local", &["tank/data", "tank/data/child"])]);
        let mut jobs = vec![snap("s", "tank/data"), bk("b", "tank/data", "sftp:nas:/r")];
        jobs[0].recursive = true;
        jobs[1].recursive = true;
        let mut runs = ok_runs(&jobs);
        runs[0].message = format!("created auto-20261006T193000Z{}; pruned 0", crate::daemon::snapshot::RECURSIVE_MARK);
        let found = judge(&file, &jobs, &runs, &level(1, 0), now());
        assert_eq!(words(&found), ["local: tank/data/child: no copy job"]);
    }

    #[test]
    fn judging_five_thousand_datasets_is_fast() {
        let mut file = StorageFile::default();
        let old = Some((now() - Duration::days(30)).timestamp());
        for h in 0..5 {
            let list = (0..1000).map(|i| seen(&format!("tank/d{}/s{i}", i % 20), old)).collect();
            file.hosts.insert(format!("host{h}"), host_result(Duration::minutes(5), list));
        }
        let mut jobs = Vec::new();
        for i in 0..10 {
            let mut s = snap(&format!("s{i}"), &format!("tank/d{i}"));
            s.host = format!("host{}", i % 5);
            s.recursive = true;
            jobs.push(s);
            let mut r = repl(&format!("r{i}"), &format!("tank/d{i}/s{i}"), "backup", &format!("tank/copy{i}"));
            r.host = format!("host{}", i % 5);
            jobs.push(r);
            let mut b = bk(&format!("b{i}"), &format!("tank/d{i}/s{}", i + 20), "sftp:nas:/r");
            b.host = format!("host{}", i % 5);
            jobs.push(b);
            let mut m = Job::test_mirror(&format!("m{i}"), &format!("tank/d{i}/s{}", i + 40), "m", "b2:x");
            m.host = format!("host{}", i % 5);
            jobs.push(m);
        }
        let mut runs = Vec::new();
        for day in 0..30 {
            for j in &jobs {
                runs.push(run(&j.id, Duration::minutes(10) + Duration::days(day), Some(RunResult::Ok)));
            }
        }
        let start = std::time::Instant::now();
        let found = judge(&file, &jobs, &runs, &level(3, 1), now());
        let took = start.elapsed();
        assert_eq!(found.len(), 5000);
        assert!(took < std::time::Duration::from_millis(200), "took {took:?}");
    }

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

    #[test]
    fn an_up_to_date_skip_does_not_keep_a_job_current() {
        let with_msg = |ago: i64, result: RunResult, msg: &str| Run { message: msg.into(), ..run("snap", Duration::minutes(ago), Some(result)) };
        let up_to_date = format!("{} (auto-2026)", crate::daemon::history::UP_TO_DATE);
        let runs = vec![with_msg(300, RunResult::Ok, ""), with_msg(10, RunResult::Skipped, &up_to_date)];
        assert_eq!(job_state(&hourly(), &runs, 2, now()), JobState::Stopped { last_ok: now() - Duration::minutes(300) }, "only a real copy counts");
        let runs = vec![with_msg(300, RunResult::Ok, ""), with_msg(10, RunResult::Skipped, "skipped: no snapshots with prefix 'auto' on tank/data")];
        assert_eq!(job_state(&hourly(), &runs, 2, now()), JobState::Stopped { last_ok: now() - Duration::minutes(300) });
        let runs = vec![with_msg(300, RunResult::Ok, ""), with_msg(20, RunResult::Skipped, &up_to_date), with_msg(10, RunResult::Failed, "boom")];
        assert_eq!(job_state(&hourly(), &runs, 2, now()), JobState::Failing);
    }

    #[test]
    fn a_frozen_copy_that_logs_up_to_date_every_run_goes_stopped() {
        // A pinned source snapshot (or a prefix no job advances) is "up to date" on every run; that must never read as protected.
        let up_to_date = format!("{} (pinned)", crate::daemon::history::UP_TO_DATE);
        let skip = |ago: i64| Run { message: up_to_date.clone(), ..run("snap", Duration::minutes(ago), Some(RunResult::Skipped)) };
        let runs = vec![run("snap", Duration::days(30), Some(RunResult::Ok)), skip(180), skip(120), skip(60), skip(5)];
        assert_eq!(job_state(&hourly(), &runs, 2, now()), JobState::Stopped { last_ok: now() - Duration::days(30) });
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
    fn a_backup_or_mirror_of_one_folder_does_not_protect_the_dataset() {
        let file = storage(&[("local", &["tank/home"])]);
        let whole = vec![snap("s", "tank/home"), bk("b", "tank/home", "sftp:nas:/r")];
        assert!(judge(&file, &whole, &ok_runs(&whole), &level(1, 0), now()).is_empty(), "the whole dataset is copied");
        let mut part = whole.clone();
        part[1].path = Some("me/docs".into());
        assert_eq!(words(&judge(&file, &part, &ok_runs(&part), &level(1, 0), now())), ["local: tank/home: no copy job"]);
        assert_eq!(copy_of(&part[1]), None);
        let mut mirror = Job::test_mirror("m", "tank/home", "m", "b2:bucket/home");
        assert!(copy_of(&mirror).is_some());
        mirror.path = Some("me".into());
        assert_eq!(copy_of(&mirror), None, "a mirror of one folder");
        mirror.path = Some(String::new());
        assert!(copy_of(&mirror).is_some(), "an empty path is the whole dataset");
    }

    fn gap(snapshots: Option<Gap>, copies: Option<CopyGap>) -> Unprotected {
        Unprotected { host: "local".into(), dataset: "tank/data".into(), tier: Tier::Data, snapshots, copies }
    }

    fn no_copy(worst: Gap) -> CopyGap {
        CopyGap { have: 0, need: 1, online_have: 0, online_need: 0, domains: None, worst }
    }

    #[test]
    fn the_fix_for_a_dataset_with_no_snapshot_job_is_a_new_snapshot_job_whatever_else_is_missing() {
        let both = gap(Some(Gap::Missing), Some(no_copy(Gap::Missing)));
        assert_eq!(fix_for(&both, &[]), Fix::NewSnapshotJob);
        let other = vec![Job::test_snapshot("s", "tank/other", "auto", 24)];
        assert_eq!(fix_for(&both, &other), Fix::NewSnapshotJob, "a job on another dataset does not cover this one");
    }

    #[test]
    fn a_job_that_exists_is_gone_to_not_made_again() {
        let jobs = vec![Job::test_snapshot("s", "tank/data", "auto", 24)];
        assert_eq!(fix_for(&gap(Some(Gap::Missing), None), &jobs), Fix::GoToJob("s".into()), "it has not run yet");
        assert_eq!(fix_for(&gap(Some(Gap::Failing), None), &jobs), Fix::GoToJob("s".into()));
        let mut off = jobs.clone();
        off[0].enabled = false;
        assert_eq!(fix_for(&gap(Some(Gap::Missing), None), &off), Fix::NewSnapshotJob, "a disabled job gives nothing");
        let copies = vec![Job::test_replicate("r", "tank/data", "auto", "nas", "tank/copy")];
        assert_eq!(fix_for(&gap(None, Some(no_copy(Gap::Failing))), &copies), Fix::GoToJob("r".into()));
        assert_eq!(fix_for(&gap(None, Some(no_copy(Gap::Missing))), &copies), Fix::GoToJob("r".into()), "never ran");
    }

    #[test]
    fn a_dataset_that_needs_a_copy_is_asked_which_kind_unless_a_copy_job_is_already_its_problem() {
        assert_eq!(fix_for(&gap(None, Some(no_copy(Gap::Missing))), &[]), Fix::ChooseCopy);
        let some = Some(CopyGap { have: 1, need: 3, online_have: 0, online_need: 1, domains: None, worst: Gap::Missing });
        let copies = vec![Job::test_replicate("r", "tank/data", "auto", "nas", "tank/copy")];
        assert_eq!(fix_for(&gap(None, some), &copies), Fix::ChooseCopy, "one copy is current: another is needed, not a repair");
    }

    #[test]
    fn a_recursive_job_covers_the_children_only_once_it_has_run_recursively() {
        let file = storage(&[("local", &["tank", "tank/kids", "tank/kids/deep"])]);
        let mut tree = snap("s", "tank");
        tree.recursive = true;
        let jobs = vec![tree];
        let ran = |message: &str| vec![Run { message: message.into(), ..run("s", Duration::minutes(10), Some(RunResult::Ok)) }];
        let judged = |runs: &[Run]| judge(&file, &jobs, runs, &level(1, 0), now()).into_iter().filter(|u| u.snapshots.is_some()).map(|u| (u.dataset, u.snapshots)).collect::<Vec<_>>();
        let before = judged(&ran("created auto-20261024T180000Z; pruned 0"));
        let rows: Vec<(&str, Option<Gap>)> = before.iter().map(|(d, g)| (d.as_str(), *g)).collect();
        assert_eq!(rows, [("tank/kids", Some(Gap::NotRecursiveYet)), ("tank/kids/deep", Some(Gap::NotRecursiveYet))], "tank itself is covered by the run; its children are not proven");
        let after = judged(&ran(&format!("created auto-20261024T190000Z{}; pruned 0", crate::daemon::snapshot::RECURSIVE_MARK)));
        assert!(after.is_empty(), "{after:?}");
        // The newest success decides: a plain run after a recursive one takes the proof away.
        let mut both = ran(&format!("created a{}; pruned 0", crate::daemon::snapshot::RECURSIVE_MARK));
        both.push(Run { message: "created b; pruned 0".into(), ..run("s", Duration::minutes(5), Some(RunResult::Ok)) });
        assert_eq!(judged(&both).len(), 2);
        // A job of the child's own never needs the proof.
        let mut own = jobs.clone();
        own.push(snap("kid", "tank/kids/deep"));
        let runs = vec![run("s", Duration::minutes(10), Some(RunResult::Ok)), run("kid", Duration::minutes(10), Some(RunResult::Ok))];
        let names: Vec<String> = judge(&file, &own, &runs, &level(1, 0), now()).into_iter().filter(|u| u.snapshots.is_some()).map(|u| u.dataset).collect();
        assert_eq!(names, ["tank/kids"], "tank/kids is unproven; tank/kids/deep has a job of its own");
    }

    #[test]
    fn the_recursive_wait_is_worded_and_sent_to_the_job() {
        let age = |_: DateTime<Utc>| String::new();
        let item = Unprotected { host: "h".into(), dataset: "tank/kids".into(), tier: Tier::Data, snapshots: Some(Gap::NotRecursiveYet), copies: None };
        assert_eq!(item.describe(&age), "snapshot job has not run recursively yet");
        let mut tree = snap("s", "tank");
        tree.recursive = true;
        let item = Unprotected { host: "local".into(), dataset: "tank/kids".into(), ..item };
        assert_eq!(fix_for(&item, &[tree]), Fix::GoToJob("s".into()));
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
