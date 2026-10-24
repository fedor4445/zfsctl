//! The checks a host gets once a day (and on demand from the Hosts section): its disks with `smartctl` and its file
//! sharing services. This module holds what both ways of starting them share: the marker that says a check is running,
//! one host's pair of checks, how an answer is merged into `smart.json` and `shares.json`, and the daily pass over all hosts.

use super::shares_check::{self, SharesFile};
use super::smart_check::{self, SmartFile};
use crate::exec::CommandRunner;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const MARKER_FILE: &str = "checks-running.json";
/// A marker whose heartbeat is older than this was left by a run that died: it is ignored.
pub const STALE_AFTER_SECS: i64 = 180;
/// How often a running check refreshes its marker.
pub const BEAT_EVERY: Duration = Duration::from_secs(20);

/// The note kept next to a result that today's check could not replace.
const GONE_SMART: &str = "smartctl is no longer available on this host";
const GONE_SHARES: &str = "the shares check no longer works on this host";

/// Says a check is running: who started it (`daemon` or `tui`), since when, the last heartbeat and, for a check of one
/// host, which.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Marker {
    pub by: String,
    pub since: DateTime<Utc>,
    pub beat: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

fn marker_path(dir: &Path) -> PathBuf {
    dir.join(MARKER_FILE)
}

fn write_marker(dir: &Path, marker: &Marker) -> std::io::Result<()> {
    smart_check::write_atomic(&marker_path(dir), &serde_json::to_string(marker).map_err(std::io::Error::other)?)
}

/// Takes the marker file for `marker`: creating it is the claim, so of two that start at the same moment only one gets
/// it. A fresh marker of another run refuses; one left by a run that died (no heartbeat for three minutes) is replaced.
/// (Two claimants that both find the same dead marker can still both replace it; that needs a crash and a collision.)
fn claim_marker(dir: &Path, marker: &Marker) -> bool {
    use std::io::Write as _;
    let path = marker_path(dir);
    for _ in 0..2 {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let _ = file.write_all(serde_json::to_string(marker).unwrap_or_default().as_bytes());
                return true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let taken = match read_marker(dir) {
                    Some(other) => (marker.since - other.beat).num_seconds() <= STALE_AFTER_SECS,
                    // Unreadable: a claimant may be between creating the file and writing it.
                    None => std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_none_or(|age| age < Duration::from_secs(5)),
                };
                if taken {
                    return false;
                }
                let _ = std::fs::remove_file(&path);
            }
            // The state directory cannot be written: no notice, but the check itself still runs.
            Err(_) => return true,
        }
    }
    false
}

/// The marker on disk, if there is a readable one (fresh or not).
pub fn read_marker(dir: &Path) -> Option<Marker> {
    serde_json::from_str(&std::fs::read_to_string(marker_path(dir)).ok()?).ok()
}

/// The marker of a check that is running now: present, readable and with a heartbeat younger than `STALE_AFTER_SECS`.
pub fn fresh_marker(dir: &Path, now: DateTime<Utc>) -> Option<Marker> {
    read_marker(dir).filter(|m| (now - m.beat).num_seconds() <= STALE_AFTER_SECS)
}

/// Holds the marker for as long as it lives: writes it, keeps its heartbeat going, and removes it on drop (also on a
/// panic) unless another run has taken the file over since.
pub struct RunGuard {
    dir: PathBuf,
    mine: Marker,
    stop: Arc<std::sync::atomic::AtomicBool>,
    beat: Option<std::thread::JoinHandle<()>>,
}

impl RunGuard {
    /// `None` when another run holds a fresh marker: the caller must not run.
    pub fn start(dir: &Path, by: &str, host: Option<&str>) -> Option<RunGuard> {
        RunGuard::start_with(dir, by, host, BEAT_EVERY)
    }

    pub fn start_with(dir: &Path, by: &str, host: Option<&str>, beat_every: Duration) -> Option<RunGuard> {
        let now = Utc::now();
        let mine = Marker { by: by.to_string(), since: now, beat: now, host: host.map(str::to_string) };
        if !claim_marker(dir, &mine) {
            return None;
        }
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let beat = {
            let (dir, mine, stop) = (dir.to_path_buf(), mine.clone(), Arc::clone(&stop));
            std::thread::Builder::new().name("checks-beat".to_string()).spawn(move || beat_loop(&dir, &mine, &stop, beat_every)).ok()
        };
        Some(RunGuard { dir: dir.to_path_buf(), mine, stop, beat })
    }
}

fn is_mine(on_disk: &Marker, mine: &Marker) -> bool {
    on_disk.by == mine.by && on_disk.since == mine.since
}

/// Rewrites the marker with a new heartbeat every `every`, for as long as `stop` is not set and the file is still ours.
fn beat_loop(dir: &Path, mine: &Marker, stop: &std::sync::atomic::AtomicBool, every: Duration) {
    use std::sync::atomic::Ordering;
    let step = every.min(Duration::from_millis(50));
    let mut last = std::time::Instant::now();
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(step);
        if last.elapsed() < every {
            continue;
        }
        last = std::time::Instant::now();
        match read_marker(dir) {
            Some(on_disk) if is_mine(&on_disk, mine) => {
                let _ = write_marker(dir, &Marker { beat: Utc::now(), ..on_disk });
            }
            _ => return,
        }
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.beat.take() {
            let _ = handle.join();
        }
        // Only the writer takes it away: another run may have started since.
        if read_marker(&self.dir).is_some_and(|on_disk| is_mine(&on_disk, &self.mine)) {
            let _ = std::fs::remove_file(marker_path(&self.dir));
        }
    }
}

/// What one host's two checks found.
pub struct HostChecks {
    pub disks: smart_check::Outcome,
    pub shares: shares_check::Outcome,
}

/// Disks first, then shares, one after the other on the one host. Only the tests use this: the real runs ask the two
/// halves at once (`collect`), so a slow disk cannot take the shares answer with it.
#[cfg(test)]
pub fn check_one_host(runner: &dyn CommandRunner, now: DateTime<Utc>, previous_disks: Option<&smart_check::HostResult>, _previous_shares: Option<&shares_check::SharesResult>) -> HostChecks {
    HostChecks { disks: smart_check::check_host(runner, now, previous_disks, smart_check::HOST_BUDGET), shares: shares_check::check_host(runner, now) }
}

/// Writes one host's disk answer into `smart.json`: an answer replaces the host's entry (so a problem that is gone
/// clears), a failed or silent check keeps the previous entry with a note, a host that never answered gets none. Only
/// this host's entry changes; the whole file is read, changed and renamed into place.
pub fn merge_disks(dir: &Path, host: &str, outcome: &smart_check::Outcome) {
    let path = dir.join("smart.json");
    let mut smart = SmartFile::load(&path);
    match outcome {
        smart_check::Outcome::Checked(result) => {
            smart.hosts.insert(host.to_string(), result.clone());
        }
        smart_check::Outcome::Silent => note(smart.hosts.get_mut(host).map(|e| &mut e.error), GONE_SMART),
        smart_check::Outcome::Failed(why) => note(smart.hosts.get_mut(host).map(|e| &mut e.error), why),
    }
    let _ = smart.save(&path);
}

/// The same for the shares answer and `shares.json`.
pub fn merge_shares(dir: &Path, host: &str, outcome: &shares_check::Outcome) {
    let path = dir.join("shares.json");
    let mut shares = SharesFile::load(&path);
    match outcome {
        shares_check::Outcome::Checked(result) => {
            shares.hosts.insert(host.to_string(), result.clone());
        }
        shares_check::Outcome::Silent => note(shares.hosts.get_mut(host).map(|e| &mut e.error), GONE_SHARES),
        shares_check::Outcome::Failed(why) => note(shares.hosts.get_mut(host).map(|e| &mut e.error), why),
    }
    let _ = shares.save(&path);
}

/// Both halves of one host's answer (see `merge_disks`, `merge_shares`).
#[cfg(test)]
pub fn merge_into(dir: &Path, host: &str, checks: &HostChecks) {
    merge_disks(dir, host, &checks.disks);
    merge_shares(dir, host, &checks.shares);
}

/// Records why a kept entry was not refreshed; nothing to note when the host has no entry.
fn note(slot: Option<&mut Option<String>>, why: &str) {
    if let Some(slot) = slot {
        *slot = Some(why.to_string());
    }
}

enum Half {
    Disks(smart_check::Outcome),
    Shares(shares_check::Outcome),
}

/// Asks every host for both checks, the two halves of a host at once on threads of their own, and merges each half into
/// the files as it arrives. A half that has not answered by `deadline` is recorded as failed (the previous entry stays,
/// with a note); the other half is kept. Returns what each host answered, in the order of `hosts`. Does not prune hosts
/// or date the run: that is `run_pass`.
pub fn collect(dir: &Path, hosts: &[(String, Arc<dyn CommandRunner>)], now: DateTime<Utc>, deadline: Duration) -> Vec<(String, HostChecks)> {
    let smart_before = SmartFile::load(&dir.join("smart.json"));
    let (tx, rx) = std::sync::mpsc::channel();
    for (name, runner) in hosts {
        let before = smart_before.hosts.get(name).cloned();
        let (tx_disks, tx_shares, name_disks, name_shares) = (tx.clone(), tx.clone(), name.clone(), name.clone());
        let (disks_runner, shares_runner) = (Arc::clone(runner), Arc::clone(runner));
        // A thread that outlives the deadline is left to finish by itself, as the health probe's are.
        std::thread::spawn(move || {
            let _ = tx_disks.send((name_disks, Half::Disks(smart_check::check_host(disks_runner.as_ref(), now, before.as_ref(), smart_check::HOST_BUDGET.min(deadline)))));
        });
        std::thread::spawn(move || {
            let _ = tx_shares.send((name_shares, Half::Shares(shares_check::check_host(shares_runner.as_ref(), now))));
        });
    }
    drop(tx);
    let started = std::time::Instant::now();
    let mut answers: Vec<(String, Option<smart_check::Outcome>, Option<shares_check::Outcome>)> = hosts.iter().map(|(n, _)| (n.clone(), None, None)).collect();
    let mut waiting = hosts.len() * 2;
    while waiting > 0 {
        let Some(left) = deadline.checked_sub(started.elapsed()) else { break };
        let Ok((name, half)) = rx.recv_timeout(left) else { break };
        let Some(slot) = answers.iter_mut().find(|a| a.0 == name) else { continue };
        match half {
            Half::Disks(outcome) => {
                merge_disks(dir, &name, &outcome);
                slot.1 = Some(outcome);
            }
            Half::Shares(outcome) => {
                merge_shares(dir, &name, &outcome);
                slot.2 = Some(outcome);
            }
        }
        waiting -= 1;
    }
    let why = format!("no answer within {} s", deadline.as_secs());
    answers
        .into_iter()
        .map(|(name, disks, shares)| {
            let disks = disks.unwrap_or_else(|| {
                let late = smart_check::Outcome::Failed(why.clone());
                merge_disks(dir, &name, &late);
                late
            });
            let shares = shares.unwrap_or_else(|| {
                let late = shares_check::Outcome::Failed(why.clone());
                merge_shares(dir, &name, &late);
                late
            });
            (name, HostChecks { disks, shares })
        })
        .collect()
}

/// The status line after a check of one host: what each half found, in words, with no disk roles. The screens use
/// `describe_with` and the roles; this form is what the tests compare it against.
#[cfg(test)]
pub fn describe(host: &str, checks: &HostChecks) -> String {
    describe_with(host, checks, &|_, _| None)
}

/// `describe`, with each failing disk's role from `role_of(host, dev)` (`cache of tank`) in brackets after its name.
pub fn describe_with(host: &str, checks: &HostChecks, role_of: &dyn Fn(&str, &str) -> Option<String>) -> String {
    use shares_check::Outcome as Shares;
    use smart_check::Outcome as Disks;
    let disks = match &checks.disks {
        Disks::Checked(result) => {
            let mut file = SmartFile::default();
            file.hosts.insert(host.to_string(), result.clone());
            smart_check::problems_with(&file, role_of).first().map_or_else(|| "disks fine".to_string(), |p| format!("disks: {}", p.disks))
        }
        Disks::Silent => String::new(),
        Disks::Failed(why) if why.starts_with("no answer") => "disks: no answer in time".to_string(),
        Disks::Failed(_) => "disks not reachable".to_string(),
    };
    let shares = match &checks.shares {
        Shares::Checked(result) => {
            let mut file = SharesFile::default();
            file.hosts.insert(host.to_string(), result.clone());
            shares_check::problems(&file).first().map_or_else(|| "shares fine".to_string(), |p| format!("shares: {}", p.text))
        }
        Shares::Silent => String::new(),
        Shares::Failed(why) if why.starts_with("no answer") => "shares: no answer in time".to_string(),
        Shares::Failed(_) => "shares not reachable".to_string(),
    };
    match (disks.is_empty(), shares.is_empty()) {
        (true, true) => format!("{host}: nothing to check (no smartctl, no shares access)"),
        (false, true) => format!("{host}: {disks}"),
        (true, false) => format!("{host}: {shares}"),
        (false, false) => format!("{host}: {disks}, {shares}"),
    }
}

/// What a pass found, for the log.
#[derive(Debug, Default, PartialEq)]
pub struct PassSummary {
    pub hosts: usize,
    pub disks_hosts: usize,
    pub shares_hosts: usize,
    pub problems: usize,
}

/// The daily pass: both checks on every host at once, each answer merged as it arrives, hosts that have not answered by
/// `deadline` recorded as failed, entries of hosts that are gone from the configuration dropped, and `ran_at` set.
pub fn run_pass(dir: &Path, hosts: &[(String, Arc<dyn CommandRunner>)], now: DateTime<Utc>, deadline: Duration) -> PassSummary {
    collect(dir, hosts, now, deadline);
    let mut smart = SmartFile::load(&dir.join("smart.json"));
    let mut shares = SharesFile::load(&dir.join("shares.json"));
    smart.hosts.retain(|name, _| hosts.iter().any(|(n, _)| n == name));
    shares.hosts.retain(|name, _| hosts.iter().any(|(n, _)| n == name));
    (smart.ran_at, shares.ran_at) = (Some(now), Some(now));
    let _ = smart.save(&dir.join("smart.json"));
    let _ = shares.save(&dir.join("shares.json"));
    PassSummary { hosts: hosts.len(), disks_hosts: smart.hosts.len(), shares_hosts: shares.hosts.len(), problems: smart_check::problems(&smart).len() + shares_check::problems(&shares).len() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::shares_check::SharesResult;
    use super::smart_check::HostResult;
    use crate::error::{AppError, Result};
    use crate::exec::test_support::FakeRunner;
    use crate::exec::CommandOutput;
    use crate::smart::{Disk, Problem, Severity};

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    const NOW: &str = "2026-10-17T03:30:00Z";
    const SCAN: &str = include_str!("../smart_fixtures/scan.json");
    const FAILING: &str = include_str!("../smart_fixtures/hdd_failed.json");

    /// A healthy host: smartctl sees `ada0` and `nvme0`, both fine; nfsd and smbd run; two NFS shares.
    fn healthy() -> FakeRunner {
        FakeRunner::new("")
            .route("smartctl", "--scan", SCAN)
            .route("smartctl", "-j", include_str!("../smart_fixtures/hdd_ok.json"))
            .route("service", "nfsd", "nfsd is running as pid 1.\n")
            .route("service", "samba_server", "smbd is running as pid 2.\n")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\ntank/more\tsharenfs\ton\tlocal\n")
            .route("cat", crate::zfs::smb::CONF_PATH, "[global]\n")
    }

    fn silent_host() -> FakeRunner {
        FakeRunner::failing("sudo: a password is required", 1)
    }

    fn down_host() -> FakeRunner {
        FakeRunner::failing("ssh: connect to host h port 22: Connection timed out", 255)
    }

    fn entry(disks: Vec<Disk>) -> HostResult {
        HostResult { checked_at: at("2026-10-16T03:30:00Z"), disks, error: None }
    }

    fn red_disk() -> Disk {
        Disk { dev: "ada0".into(), problems: vec![Problem { severity: Severity::Issue, text: "SMART health FAILED".into() }] }
    }

    #[test]
    fn a_fresh_marker_is_seen_and_a_stale_or_torn_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(fresh_marker(dir.path(), at(NOW)), None);
        let marker = Marker { by: "daemon".into(), since: at("2026-10-17T03:30:00Z"), beat: at("2026-10-17T03:30:00Z"), host: None };
        write_marker(dir.path(), &marker).unwrap();
        assert_eq!(fresh_marker(dir.path(), at("2026-10-17T03:32:59Z")), Some(marker.clone()));
        assert_eq!(fresh_marker(dir.path(), at("2026-10-17T03:33:01Z")), None, "no heartbeat for over 3 minutes: the run died");
        assert_eq!(read_marker(dir.path()), Some(marker), "a stale marker can still be read");
        for torn in ["", "{\"by\":\"dae", "[]"] {
            std::fs::write(dir.path().join(MARKER_FILE), torn).unwrap();
            assert_eq!(fresh_marker(dir.path(), at(NOW)), None, "{torn:?}");
        }
    }

    #[test]
    fn the_guard_writes_the_marker_keeps_the_heartbeat_going_and_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _guard = RunGuard::start_with(dir.path(), "tui", Some("fbsd16"), Duration::from_millis(40)).unwrap();
            let first = read_marker(dir.path()).expect("written at once");
            assert_eq!((first.by.as_str(), first.host.as_deref()), ("tui", Some("fbsd16")));
            std::thread::sleep(Duration::from_millis(250));
            let later = read_marker(dir.path()).expect("still there");
            assert_eq!(later.since, first.since);
            assert!(later.beat > first.beat, "the heartbeat moved: {first:?} {later:?}");
        }
        assert_eq!(read_marker(dir.path()), None, "removed when the guard is dropped");
    }

    #[test]
    fn a_guard_dropped_after_another_run_took_the_file_over_leaves_the_others_marker() {
        let dir = tempfile::tempdir().unwrap();
        let guard = RunGuard::start_with(dir.path(), "tui", None, Duration::from_secs(60)).unwrap();
        let other = Marker { by: "daemon".into(), since: Utc::now() + chrono::Duration::seconds(5), beat: Utc::now(), host: None };
        write_marker(dir.path(), &other).unwrap();
        drop(guard);
        assert_eq!(read_marker(dir.path()), Some(other));
    }

    #[test]
    fn one_host_is_asked_for_disks_then_shares_and_both_answers_come_back() {
        let checks = check_one_host(&healthy(), at(NOW), None, None);
        let smart_check::Outcome::Checked(disks) = checks.disks else { panic!("disks not checked") };
        assert_eq!(disks.disks.len(), 2);
        let shares_check::Outcome::Checked(shares) = checks.shares else { panic!("shares not checked") };
        assert_eq!((shares.nfs_shares, shares.nfs_running, shares.checked_at), (2, true, at(NOW)));
    }

    /// Disks hang (smartctl never answers in time), shares answer at once.
    struct DisksHang;

    impl CommandRunner for DisksHang {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            if cmd == "smartctl" {
                std::thread::sleep(Duration::from_secs(3));
                return Err(AppError::Config("too late".into()));
            }
            healthy().run(cmd, args)
        }
        fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> Result<CommandOutput> {
            self.run(c, a)
        }
        fn run_unprivileged(&self, c: &str, a: &[&str]) -> Result<CommandOutput> {
            healthy().run_unprivileged(c, a)
        }
    }

    #[test]
    fn the_shares_half_is_stored_even_when_the_disks_half_hangs_and_the_disks_keep_their_note() {
        let dir = tempfile::tempdir().unwrap();
        let mut old = SmartFile::default();
        old.hosts.insert("h".into(), entry(vec![red_disk()]));
        old.save(&dir.path().join("smart.json")).unwrap();
        let started = std::time::Instant::now();
        let done = collect(dir.path(), &runners(vec![("h", Arc::new(DisksHang))]), at(NOW), Duration::from_millis(600));
        assert!(started.elapsed() < Duration::from_secs(2), "waited for the hung half");
        assert!(matches!(done[0].1.shares, shares_check::Outcome::Checked(_)), "the shares answer is not lost");
        assert!(SharesFile::load(&dir.path().join("shares.json")).hosts.contains_key("h"));
        let smart = SmartFile::load(&dir.path().join("smart.json"));
        assert!(smart.hosts["h"].error.as_deref().unwrap_or("").contains("no answer"), "{:?}", smart.hosts["h"]);
        assert_eq!(smart_check::problems(&smart).len(), 1, "the old red disk stays");
    }

    #[test]
    fn many_writers_of_one_file_never_leave_it_torn_or_leave_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smart.json");
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..60 {
                        let mut file = SmartFile::default();
                        file.hosts.insert(format!("h{n}-{i}"), entry(vec![red_disk()]));
                        file.save(&path).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let loaded = SmartFile::load(&path);
        assert_eq!(loaded.hosts.len(), 1, "one writer's whole file won, not a mixture: {}", std::fs::read_to_string(&path).unwrap());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n != "smart.json").collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_claim_is_refused_while_a_fresh_marker_exists_replaces_a_stale_one_and_only_one_racer_wins() {
        let dir = tempfile::tempdir().unwrap();
        let first = RunGuard::start_with(dir.path(), "tui", None, Duration::from_secs(60)).expect("free");
        assert!(RunGuard::start_with(dir.path(), "daemon", None, Duration::from_secs(60)).is_none(), "a fresh marker is not taken over");
        drop(first);
        let old = Utc::now() - chrono::Duration::minutes(10);
        write_marker(dir.path(), &Marker { by: "daemon".into(), since: old, beat: old, host: None }).unwrap();
        assert!(RunGuard::start_with(dir.path(), "tui", None, Duration::from_secs(60)).is_some(), "a run that died does not block");
        let racing = tempfile::tempdir().unwrap();
        let wins: usize = (0..8)
            .map(|_| {
                let p = racing.path().to_path_buf();
                std::thread::spawn(move || {
                    let g = RunGuard::start_with(&p, "tui", None, Duration::from_secs(60));
                    std::thread::sleep(Duration::from_millis(300));
                    usize::from(g.is_some())
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .sum();
        assert_eq!(wins, 1, "exactly one of eight simultaneous claims");
    }

    #[test]
    fn the_status_line_names_a_failing_disks_role_when_it_is_known() {
        let runner = FakeRunner::new("")
            .route("smartctl", "--scan", SCAN)
            .route("smartctl", "-j", FAILING)
            .route("service", "nfsd", "nfsd is running as pid 1.\n")
            .route("service", "samba_server", "smbd is running as pid 2.\n")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\n")
            .route("cat", crate::zfs::smb::CONF_PATH, "[global]\n");
        let checks = check_one_host(&runner, at(NOW), None, None);
        let role = |host: &str, dev: &str| (host == "fbsd16" && dev == "ada0").then(|| "cache of tank".to_string());
        assert_eq!(describe_with("fbsd16", &checks, &role), "fbsd16: disks: ada0 (cache of tank) SMART health FAILED; nvme0 SMART health FAILED, shares fine");
        assert_eq!(describe("fbsd16", &checks), describe_with("fbsd16", &checks, &|_, _| None));
    }

    #[test]
    fn the_status_line_names_what_each_half_found() {
        let line = |runner: &FakeRunner| describe("fbsd16", &check_one_host(runner, at(NOW), None, None));
        assert_eq!(line(&healthy()), "fbsd16: disks fine, shares fine");
        let stopped = FakeRunner::new("")
            .route("smartctl", "--scan", SCAN)
            .route("smartctl", "-j", FAILING)
            .route("service", "samba_server", "smbd is running as pid 2.\n")
            .route_failure("service", "nfsd", "", 1)
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\n")
            .route("cat", crate::zfs::smb::CONF_PATH, "[global]\n");
        assert_eq!(line(&stopped), "fbsd16: disks: ada0 SMART health FAILED; nvme0 SMART health FAILED, shares: NFS: 1 share but nfsd is not running");
        assert_eq!(line(&down_host()), "fbsd16: disks not reachable, shares not reachable");
        assert_eq!(line(&silent_host()), "fbsd16: nothing to check (no smartctl, no shares access)");
    }

    #[test]
    fn merge_replaces_on_success_so_a_problem_that_is_gone_clears() {
        let dir = tempfile::tempdir().unwrap();
        let mut before = SmartFile::default();
        before.hosts.insert("h".into(), entry(vec![red_disk()]));
        before.save(&dir.path().join("smart.json")).unwrap();
        merge_into(dir.path(), "h", &check_one_host(&healthy(), at(NOW), None, None));
        let after = SmartFile::load(&dir.path().join("smart.json"));
        assert!(smart_check::problems(&after).is_empty(), "{after:?}");
        assert_eq!(after.hosts["h"].checked_at, at(NOW));
        assert!(after.hosts["h"].error.is_none());
        assert!(SharesFile::load(&dir.path().join("shares.json")).hosts.contains_key("h"));
    }

    #[test]
    fn merge_keeps_the_previous_entry_with_a_note_when_the_host_is_down_or_not_set_up() {
        let dir = tempfile::tempdir().unwrap();
        let mut smart = SmartFile::default();
        smart.hosts.insert("h".into(), entry(vec![red_disk()]));
        smart.save(&dir.path().join("smart.json")).unwrap();
        let mut shares = SharesFile::default();
        shares.hosts.insert("h".into(), SharesResult { checked_at: at("2026-10-16T03:30:00Z"), nfs_running: false, samba_running: true, nfs_shares: 2, smb_shares: 0, smb_error: None, error: None, nfs_name: None, smb_name: None });
        shares.save(&dir.path().join("shares.json")).unwrap();
        for runner in [down_host(), silent_host()] {
            merge_into(dir.path(), "h", &check_one_host(&runner, at(NOW), None, None));
            let s = SmartFile::load(&dir.path().join("smart.json"));
            assert_eq!(s.hosts["h"].checked_at, at("2026-10-16T03:30:00Z"), "the old answer stays");
            assert!(s.hosts["h"].error.is_some());
            assert_eq!(smart_check::problems(&s).len(), 1);
            let r = SharesFile::load(&dir.path().join("shares.json"));
            assert!(r.hosts["h"].error.is_some() && r.hosts["h"].nfs_shares == 2);
        }
    }

    #[test]
    fn merge_gives_a_host_that_never_answered_no_entry_and_leaves_the_other_hosts_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut smart = SmartFile::default();
        smart.hosts.insert("other".into(), entry(vec![red_disk()]));
        smart.save(&dir.path().join("smart.json")).unwrap();
        merge_into(dir.path(), "new", &check_one_host(&silent_host(), at(NOW), None, None));
        let after = SmartFile::load(&dir.path().join("smart.json"));
        assert_eq!(after.hosts.keys().collect::<Vec<_>>(), ["other"]);
        assert_eq!(after.hosts["other"], entry(vec![red_disk()]));
    }

    #[test]
    fn two_merges_for_different_hosts_both_survive() {
        let dir = tempfile::tempdir().unwrap();
        merge_into(dir.path(), "a", &check_one_host(&healthy(), at(NOW), None, None));
        merge_into(dir.path(), "b", &check_one_host(&healthy(), at(NOW), None, None));
        let after = SmartFile::load(&dir.path().join("smart.json"));
        assert_eq!(after.hosts.keys().collect::<Vec<_>>(), ["a", "b"]);
    }

    fn runners(list: Vec<(&str, Arc<dyn CommandRunner>)>) -> Vec<(String, Arc<dyn CommandRunner>)> {
        list.into_iter().map(|(n, r)| (n.to_string(), r)).collect()
    }

    #[test]
    fn a_pass_checks_every_host_prunes_removed_ones_and_dates_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut old = SmartFile::default();
        old.hosts.insert("gone".into(), entry(vec![red_disk()]));
        old.save(&dir.path().join("smart.json")).unwrap();
        let summary = run_pass(dir.path(), &runners(vec![("a", Arc::new(healthy())), ("b", Arc::new(silent_host()))]), at(NOW), Duration::from_secs(5));
        assert_eq!((summary.hosts, summary.disks_hosts, summary.shares_hosts, summary.problems), (2, 1, 1, 0), "{summary:?}");
        let smart = SmartFile::load(&dir.path().join("smart.json"));
        assert_eq!(smart.hosts.keys().collect::<Vec<_>>(), ["a"], "a host that is gone from the configuration is dropped");
        assert_eq!(smart.ran_at, Some(at(NOW)));
        assert_eq!(SharesFile::load(&dir.path().join("shares.json")).ran_at, Some(at(NOW)));
    }

    struct Hung;

    impl CommandRunner for Hung {
        fn run(&self, _: &str, _: &[&str]) -> Result<CommandOutput> {
            std::thread::sleep(Duration::from_secs(3));
            Err(AppError::Config("too late".into()))
        }
        fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> Result<CommandOutput> {
            self.run(c, a)
        }
        fn run_unprivileged(&self, c: &str, a: &[&str]) -> Result<CommandOutput> {
            self.run(c, a)
        }
    }

    #[test]
    fn a_hung_host_does_not_stop_the_others_and_keeps_its_old_entry_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let mut old = SmartFile::default();
        old.hosts.insert("hung".into(), entry(vec![red_disk()]));
        old.save(&dir.path().join("smart.json")).unwrap();
        let started = std::time::Instant::now();
        let summary = run_pass(dir.path(), &runners(vec![("hung", Arc::new(Hung)), ("good", Arc::new(healthy()))]), at(NOW), Duration::from_millis(500));
        assert!(started.elapsed() < Duration::from_secs(2), "waited for the hung host");
        let smart = SmartFile::load(&dir.path().join("smart.json"));
        assert!(smart.hosts.contains_key("good"));
        assert!(smart.hosts["hung"].error.as_deref().unwrap_or("").contains("no answer"), "{:?}", smart.hosts["hung"]);
        assert_eq!(summary.problems, 1, "the kept red disk still counts");
    }
}
