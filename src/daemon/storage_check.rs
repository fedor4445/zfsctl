//! The storage check: which datasets are close to their quota. One `zfs list` per host, kept in `storage.json` for the
//! Dashboard and the daily report; it runs every few minutes (a quota line must clear soon after the fix) and on demand.
//! Reads only. Later slices (hybrid pools, checksum and dedup, encryption) add their own readings to the same file.

use crate::exec::CommandRunner;
use crate::smart::Severity;
use crate::zfs::pool_classes::{parse_classes, PoolClasses};
use crate::zfs::quota::{usage, Facts};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::path::Path;

/// Datasets whose `checksum` is off and pools with deduplication in use, as the host said (set here or received, never inherited).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Integrity {
    #[serde(default)]
    pub checksum_off: Vec<String>,
    /// Pools that have a dataset with `dedup` on (any algorithm).
    #[serde(default)]
    pub dedup_pools: Vec<String>,
    /// Every pool's allocated bytes and dedup ratio in hundredths (`1.02x` is 102), for the dedup line.
    #[serde(default)]
    pub pools: Vec<PoolSpace>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PoolSpace {
    pub name: String,
    pub allocated: u64,
    pub ratio_x100: u32,
}

impl Integrity {
    fn is_empty(&self) -> bool {
        *self == Integrity::default()
    }
}

/// A dataset that has a `quota` or a `refquota` (the others are not stored).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LimitedDataset {
    pub name: String,
    pub used: u64,
    pub referenced: u64,
    pub quota: u64,
    pub refquota: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StorageResult {
    pub checked_at: DateTime<Utc>,
    #[serde(default)]
    pub datasets: Vec<LimitedDataset>,
    /// Pools with something to say about their special, dedup, log or cache devices (quiet pools are not stored).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pools: Vec<PoolClasses>,
    /// What the integrity settings look like on this host (`zfs get -s local,received checksum,dedup`, `zpool list`).
    #[serde(default, skip_serializing_if = "Integrity::is_empty")]
    pub integrity: Integrity,
    /// The host's OpenZFS version (`2.4.2-1-FreeBSD_g...`), when `zfs version` says it (OmniOS has no such command).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zfs_version: Option<String>,
    /// Why the latest check did not happen; the rest is then from the last one that did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StorageFile {
    #[serde(default)]
    pub hosts: BTreeMap<String, StorageResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_at: Option<DateTime<Utc>>,
}

impl StorageFile {
    /// The file at `path`, or an empty one when it is missing, torn or of another shape.
    pub fn load(path: &Path) -> StorageFile {
        std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    /// Writes through a temporary file and a rename, so a crash leaves the old file or the new one.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        super::smart_check::write_atomic(path, &serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)
    }

    /// An answer replaces the host's entry; a host that is not set up for the check loses its entry; one that could not be asked
    /// keeps the last good reading and says why it is old.
    pub fn apply(&mut self, host: &str, outcome: Outcome, now: DateTime<Utc>) {
        match outcome {
            Outcome::Checked(result) => {
                self.hosts.insert(host.to_string(), result);
            }
            Outcome::Silent => {
                self.hosts.remove(host);
            }
            Outcome::Failed(why) => {
                if let Some(old) = self.hosts.get_mut(host) {
                    old.error = Some(why);
                }
            }
        }
        self.ran_at = Some(now);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The host is not set up for this check (sudo says no): nothing to show.
    Silent,
    Checked(StorageResult),
    /// The host could not be asked this time.
    Failed(String),
}

/// `zfs list -H -p -t filesystem,volume -o name,used,referenced,quota,refquota,type`: options first, as OmniOS's `zfs` wants.
const LIST_ARGS: &[&str] = &["list", "-H", "-p", "-t", "filesystem,volume", "-o", "name,used,referenced,quota,refquota,type"];

/// Unset limits print as `0`, and as `-` on a volume (which has no quota).
fn parse_limited(output: &str) -> Vec<LimitedDataset> {
    let number = |s: &str| s.trim().parse::<u64>().unwrap_or(0);
    output
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let &[name, used, referenced, quota, refquota, _kind] = f.as_slice() else { return None };
            let (quota, refquota) = (number(quota), number(refquota));
            (quota > 0 || refquota > 0).then(|| LimitedDataset { name: name.to_string(), used: number(used), referenced: number(referenced), quota, refquota })
        })
        .collect()
}

/// `zfs get -H -p -s local,received -t filesystem,volume -o name,property,value checksum,dedup` and `zpool list -H -p -o
/// name,allocated,dedupratio` read together. Illumos's dump device (`<pool>/dump`) has its checksum off by design: not listed.
fn parse_integrity(settings: &str, pools: &str) -> Integrity {
    let mut out = Integrity::default();
    for line in settings.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let &[name, property, value] = f.as_slice() else { continue };
        let pool = name.split('/').next().unwrap_or(name).to_string();
        match (property, value) {
            ("checksum", "off") if !name.ends_with("/dump") => out.checksum_off.push(name.to_string()),
            ("dedup", v) if v != "off" && v != "-" => {
                if !out.dedup_pools.contains(&pool) {
                    out.dedup_pools.push(pool);
                }
            }
            _ => {}
        }
    }
    for line in pools.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let &[name, allocated, ratio] = f.as_slice() else { continue };
        let ratio = ratio.trim_end_matches('x').parse::<f64>().ok().filter(|r| *r >= 1.0);
        if let (Ok(allocated), Some(ratio)) = (allocated.trim().parse::<u64>(), ratio) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small positive ratio times 100
            out.pools.push(PoolSpace { name: name.to_string(), allocated, ratio_x100: (ratio * 100.0).round() as u32 });
        }
    }
    out
}

/// The first line of `zfs version` without its `zfs-` prefix (`2.4.2-1-FreeBSD_g6330a45b0`).
fn parse_version(output: &str) -> Option<String> {
    let first = output.lines().map(str::trim).find(|l| !l.is_empty())?;
    let version = first.strip_prefix("zfs-").unwrap_or(first);
    version.starts_with(|c: char| c.is_ascii_digit()).then(|| version.to_string())
}

/// Asks one host which of its datasets have a quota and how full they are.
pub fn check_host(runner: &dyn CommandRunner, now: DateTime<Utc>) -> Outcome {
    match runner.run("zfs", LIST_ARGS) {
        Err(e) => Outcome::Failed(e.to_string()),
        Ok(out) if out.status == 255 => Outcome::Failed(super::shares_check::first_line(&out.stderr, "could not reach the host")),
        Ok(out) if !out.success() && super::shares_check::is_refusal(&out.stderr) => Outcome::Silent,
        Ok(out) if !out.success() => Outcome::Failed(super::shares_check::first_line(&out.stderr, "zfs list failed")),
        Ok(out) => {
            // The device classes come from `zpool status`, best effort: a host that will not say still gets its quota lines.
            let pools = runner
                .run("zpool", &["status"])
                .ok()
                .filter(crate::exec::CommandOutput::success)
                .map(|o| parse_classes(&o.stdout).into_iter().filter(|p| !p.is_quiet()).collect())
                .unwrap_or_default();
            // Each of these may fail on its own (sudo, an old zfs): the rest is still worth having.
            let text = |cmd: &str, args: &[&str]| runner.run(cmd, args).ok().filter(crate::exec::CommandOutput::success).map(|o| o.stdout).unwrap_or_default();
            let settings = text("zfs", &["get", "-H", "-p", "-s", "local,received", "-t", "filesystem,volume", "-o", "name,property,value", "checksum,dedup"]);
            let pool_list = text("zpool", &["list", "-H", "-p", "-o", "name,allocated,dedupratio"]);
            let zfs_version = parse_version(&text("zfs", &["version"]));
            Outcome::Checked(StorageResult { checked_at: now, datasets: parse_limited(&out.stdout), pools, zfs_version, integrity: parse_integrity(&settings, &pool_list), error: None })
        }
    }
}

/// The state file's name in the state directory.
pub const FILE: &str = "storage.json";

/// How long one pass waits for a host before giving up on it.
pub const HOST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// Asks one host and writes the answer into `storage.json` (load, apply, save), so `c` and the daemon's loop share one path.
pub fn check_and_merge(dir: &Path, host: &str, runner: &dyn CommandRunner, now: DateTime<Utc>) {
    let outcome = check_host(runner, now);
    let path = dir.join(FILE);
    let mut file = StorageFile::load(&path);
    file.apply(host, outcome, now);
    let _ = file.save(&path);
}

/// Hosts whose last question has not been answered yet, so a host that hangs is asked once and not once per pass (each
/// question is a thread and an ssh process that stay until the host answers).
fn in_flight() -> &'static std::sync::Mutex<std::collections::BTreeSet<String>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> = std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}

/// Asks every host at once and waits at most `deadline` for each. Each answer is merged into the file as it arrives (load,
/// apply, save), so another writer's changes are never overwritten with an old snapshot; a host that has not answered by the
/// deadline keeps its last reading and gets the reason. A host still unanswered from an earlier pass is skipped.
/// Returns how many hosts answered in time.
pub fn run_pass(dir: &Path, hosts: &[(String, std::sync::Arc<dyn CommandRunner>)], now: DateTime<Utc>, deadline: std::time::Duration) -> usize {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut asked = Vec::new();
    for (name, runner) in hosts {
        // Keyed by state directory too: the guard is per daemon, and tests (one process, many directories) must not meet.
        let key = format!("{}\0{name}", dir.display());
        if !in_flight().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key.clone()) {
            continue;
        }
        asked.push(name.clone());
        let (tx, name, runner) = (tx.clone(), name.clone(), std::sync::Arc::clone(runner));
        std::thread::spawn(move || {
            let outcome = check_host(runner.as_ref(), now);
            in_flight().lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&key);
            let _ = tx.send((name, outcome));
        });
    }
    drop(tx);
    let end = std::time::Instant::now() + deadline;
    let path = dir.join(FILE);
    let merge = |name: &str, outcome: Outcome| {
        let mut file = StorageFile::load(&path);
        file.apply(name, outcome, now);
        let _ = file.save(&path);
    };
    let mut answered = std::collections::BTreeSet::new();
    while answered.len() < asked.len() {
        let Some(left) = end.checked_duration_since(std::time::Instant::now()) else { break };
        match rx.recv_timeout(left) {
            Ok((name, outcome)) => {
                merge(&name, outcome);
                answered.insert(name);
            }
            Err(_) => break,
        }
    }
    for name in asked.iter().filter(|n| !answered.contains(*n)) {
        merge(name, Outcome::Failed(format!("no answer within {} s", deadline.as_secs().max(1))));
    }
    answered.len()
}

/// Drops the hosts that are no longer configured, so a removed host's lines do not stay in the Dashboard's file or the
/// daily report.
pub fn retain_hosts(dir: &Path, configured: &[String]) {
    let path = dir.join(FILE);
    let mut file = StorageFile::load(&path);
    let before = file.hosts.len();
    file.hosts.retain(|host, _| configured.contains(host));
    if file.hosts.len() != before {
        let _ = file.save(&path);
    }
}

/// What one host adds to Needs attention for its pools' device classes: a special or dedup vdev that is a lone disk is an issue
/// (losing it loses the pool), a faulted log or cache device is only a watch.
pub fn pool_problems(file: &StorageFile) -> Vec<StorageProblem> {
    file.hosts
        .iter()
        .filter_map(|(host, result)| {
            let mut texts = Vec::new();
            let mut severity = Severity::Watch;
            for pool in &result.pools {
                for class in &pool.unprotected {
                    texts.push(format!("{} {class} vdev has no redundancy", pool.name));
                    severity = Severity::Issue;
                }
                if pool.log_bad {
                    texts.push(format!("{} log device faulted", pool.name));
                }
                if pool.cache_bad {
                    texts.push(format!("{} cache device faulted", pool.name));
                }
            }
            (!texts.is_empty()).then(|| StorageProblem { host: host.clone(), severity, text: texts.join("; "), checked_at: result.checked_at })
        })
        .collect()
}

/// What one host adds to Needs attention: its dataset nearest to a limit, with how many others are over the warning line.
#[derive(Debug, Clone, PartialEq)]
pub struct StorageProblem {
    pub host: String,
    pub severity: Severity,
    pub text: String,
    pub checked_at: DateTime<Utc>,
}

/// One problem per host at most. `quota` is the (watch, issue) percentage pair.
pub fn problems(file: &StorageFile, quota: (u8, u8)) -> Vec<StorageProblem> {
    file.hosts
        .iter()
        .filter_map(|(host, result)| {
            let mut near: Vec<(f64, &LimitedDataset, crate::zfs::quota::Use)> = result
                .datasets
                .iter()
                .filter_map(|d| {
                    let u = usage(&Facts { used: d.used, referenced: d.referenced, quota: d.quota, refquota: d.refquota, ..Facts::default() })?;
                    (u.percent >= f64::from(quota.0)).then_some((u.percent, d, u))
                })
                .collect();
            near.sort_by(|a, b| b.0.total_cmp(&a.0));
            let (percent, dataset, u) = near.first()?;
            let severity = if *percent >= f64::from(quota.1) { Severity::Issue } else { Severity::Watch };
            let more = if near.len() > 1 { format!(" (+{} more)", near.len() - 1) } else { String::new() };
            let text = format!("{} {percent:.0}% of its {} {}{more}", dataset.name, crate::zfs::quota::format_size(u.limit), if u.is_ref { "refquota" } else { "quota" });
            Some(StorageProblem { host: host.clone(), severity, text, checked_at: result.checked_at })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    const G: u64 = 1 << 30;
    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }
    const NOW: &str = "2026-10-18T12:00:00Z";

    fn dataset(name: &str, used: u64, referenced: u64, quota: u64, refquota: u64) -> LimitedDataset {
        LimitedDataset { name: name.into(), used, referenced, quota, refquota }
    }

    fn file_with(host: &str, datasets: Vec<LimitedDataset>) -> StorageFile {
        let mut file = StorageFile::default();
        file.hosts.insert(host.into(), StorageResult { checked_at: at(NOW), datasets, pools: vec![], zfs_version: None, integrity: Default::default(), error: None });
        file
    }

    #[test]
    fn the_listing_keeps_only_datasets_with_a_limit_and_reads_unset_as_none() {
        // real shape: unset is 0 on a filesystem and - on a volume
        let out = "tank\t100\t90\t0\t0\tfilesystem\ntank/home\t9600\t500\t10000\t0\tfilesystem\ntank/vol\t5\t5\t-\t-\tvolume\ntank/r\t900\t300\t0\t400\tfilesystem\n";
        let kept = parse_limited(out);
        assert_eq!(kept.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["tank/home", "tank/r"]);
        assert_eq!((kept[1].quota, kept[1].refquota), (0, 400));
        assert!(parse_limited("garbage\n").is_empty());
    }

    #[test]
    fn a_dataset_at_94_percent_is_watch_and_at_98_is_issue() {
        let mut file = file_with("hub", vec![dataset("tank/home", 94 * G, 94 * G, 100 * G, 0)]);
        let p = &problems(&file, (90, 98))[0];
        assert_eq!((p.severity, p.text.as_str()), (Severity::Watch, "tank/home 94% of its 100G quota"));
        file.hosts.get_mut("hub").unwrap().datasets[0].used = 98 * G;
        assert_eq!(problems(&file, (90, 98))[0].severity, Severity::Issue);
        file.hosts.get_mut("hub").unwrap().datasets[0].used = 89 * G;
        assert!(problems(&file, (90, 98)).is_empty(), "under the warning line says nothing");
    }

    #[test]
    fn a_refquota_alone_is_judged_by_referenced_and_named() {
        let file = file_with("hub", vec![dataset("tank/r", 50 * G, 9 * G, 0, 10 * G)]);
        assert_eq!(problems(&file, (90, 98))[0].text, "tank/r 90% of its 10G refquota");
    }

    #[test]
    fn only_the_worst_dataset_per_host_is_named_with_a_count_of_the_rest() {
        let file = file_with("hub", vec![dataset("a", 91 * G, 0, 100 * G, 0), dataset("b", 97 * G, 0, 100 * G, 0), dataset("c", 50 * G, 0, 100 * G, 0), dataset("d", 95 * G, 0, 100 * G, 0)]);
        let all = problems(&file, (90, 98));
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, "b 97% of its 100G quota (+2 more)");
    }

    #[test]
    fn a_new_answer_replaces_the_hosts_lines_and_a_failure_keeps_the_old_ones_with_the_reason() {
        let mut file = file_with("hub", vec![dataset("a", 99 * G, 0, 100 * G, 0)]);
        file.apply("hub", Outcome::Failed("ssh: connection reset".into()), at(NOW));
        assert_eq!(file.hosts["hub"].error.as_deref(), Some("ssh: connection reset"));
        assert_eq!(problems(&file, (90, 98)).len(), 1, "the last good reading stays until the host answers");
        file.apply("hub", Outcome::Checked(StorageResult { checked_at: at(NOW), datasets: vec![], pools: vec![], zfs_version: None, integrity: Default::default(), error: None }), at(NOW));
        assert!(problems(&file, (90, 98)).is_empty() && file.hosts["hub"].error.is_none(), "the quota was removed: the line goes");
        file.apply("hub", Outcome::Silent, at(NOW));
        assert!(file.hosts.is_empty());
    }

    #[test]
    fn check_host_uses_options_first_and_tells_a_refusal_from_a_lost_connection() {
        let ok = FakeRunner::new("").route("zfs", "list", "tank/home\t9\t9\t10\t0\tfilesystem\n");
        let Outcome::Checked(result) = check_host(&ok, at(NOW)) else { panic!() };
        assert_eq!(result.datasets.len(), 1);
        let calls = ok.calls.lock().unwrap().clone();
        assert_eq!(calls[0].1, LIST_ARGS.iter().map(|s| (*s).to_string()).collect::<Vec<_>>());
        let denied = FakeRunner::new("").route_failure("zfs", "list", "sudo: a password is required", 1);
        assert_eq!(check_host(&denied, at(NOW)), Outcome::Silent);
        let lost = FakeRunner::new("").route_failure("zfs", "list", "ssh: connect to host x port 22: Connection reset", 255);
        assert!(matches!(check_host(&lost, at(NOW)), Outcome::Failed(_)));
        let broken = FakeRunner::new("").route_failure("zfs", "list", "cannot open pool", 1);
        assert!(matches!(check_host(&broken, at(NOW)), Outcome::Failed(_)));
    }

    #[test]
    fn check_and_merge_writes_the_hosts_answer_into_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let runner = FakeRunner::new("").route("zfs", "list", "tank/home\t9\t9\t10\t0\tfilesystem\n");
        check_and_merge(dir.path(), "hub", &runner, at(NOW));
        let file = StorageFile::load(&dir.path().join(FILE));
        assert_eq!(file.hosts["hub"].datasets.len(), 1);
        assert_eq!(file.ran_at, Some(at(NOW)));
        let quiet = FakeRunner::new("").route("zfs", "list", "");
        check_and_merge(dir.path(), "hub", &quiet, at(NOW));
        assert!(StorageFile::load(&dir.path().join(FILE)).hosts["hub"].datasets.is_empty(), "the quota is gone: the entry is replaced");
    }

    #[test]
    fn a_pass_asks_every_host_and_gives_up_on_one_that_never_answers() {
        use crate::exec::CommandOutput;
        struct Hang;
        impl CommandRunner for Hang {
            fn run(&self, _: &str, _: &[&str]) -> crate::error::Result<CommandOutput> {
                std::thread::sleep(std::time::Duration::from_secs(5));
                Ok(CommandOutput { stdout: String::new(), stderr: String::new(), status: 0 })
            }
            fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
            fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let good: std::sync::Arc<dyn CommandRunner> = std::sync::Arc::new(FakeRunner::new("").route("zfs", "list", "a\t9\t9\t10\t0\tfilesystem\n"));
        let hosts = vec![("good".to_string(), good), ("stuck".to_string(), std::sync::Arc::new(Hang) as std::sync::Arc<dyn CommandRunner>)];
        let started = std::time::Instant::now();
        let answered = run_pass(dir.path(), &hosts, at(NOW), std::time::Duration::from_millis(300));
        assert!(started.elapsed() < std::time::Duration::from_secs(3), "the hung host must not hold the pass");
        assert_eq!(answered, 1);
        let file = StorageFile::load(&dir.path().join(FILE));
        assert!(file.hosts.contains_key("good") && !file.hosts.contains_key("stuck"));
    }

    /// A runner whose `zfs list` blocks for `hold` and counts how often it was asked.
    struct Slow {
        hold: std::time::Duration,
        asked: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl CommandRunner for Slow {
        fn run(&self, _: &str, _: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(self.hold);
            Ok(crate::exec::CommandOutput { stdout: String::new(), stderr: String::new(), status: 0 })
        }
        fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<crate::exec::CommandOutput> {
            self.run(c, a)
        }
        fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
            self.run(c, a)
        }
    }

    #[test]
    fn a_pass_never_writes_back_a_stale_snapshot_over_what_another_writer_saved() {
        let dir = tempfile::tempdir().unwrap();
        let slow: std::sync::Arc<dyn CommandRunner> = std::sync::Arc::new(Slow { hold: std::time::Duration::from_millis(600), asked: Default::default() });
        let good: std::sync::Arc<dyn CommandRunner> = std::sync::Arc::new(FakeRunner::new("").route("zfs", "list", "a\t9\t9\t10\t0\tfilesystem\n"));
        let hosts = vec![("lu-good".to_string(), good), ("lost-update-slow".to_string(), slow)];
        let path = dir.path().to_path_buf();
        let pass = std::thread::spawn(move || run_pass(&path, &hosts, at(NOW), std::time::Duration::from_millis(300)));
        std::thread::sleep(std::time::Duration::from_millis(100));
        // another writer (the TUI's `c`, or the daemon) saves a host while the pass is still waiting
        check_and_merge(dir.path(), "other", &FakeRunner::new("").route("zfs", "list", "b\t9\t9\t10\t0\tfilesystem\n"), at(NOW));
        pass.join().unwrap();
        let file = StorageFile::load(&dir.path().join(FILE));
        assert!(file.hosts.contains_key("other"), "the other writer's host must survive the pass: {:?}", file.hosts.keys().collect::<Vec<_>>());
        assert!(file.hosts.contains_key("lu-good"));
    }

    #[test]
    fn a_host_that_misses_the_deadline_keeps_its_old_lines_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        check_and_merge(dir.path(), "late-host", &FakeRunner::new("").route("zfs", "list", "a\t99\t99\t100\t0\tfilesystem\n"), at(NOW));
        let slow: std::sync::Arc<dyn CommandRunner> = std::sync::Arc::new(Slow { hold: std::time::Duration::from_millis(800), asked: Default::default() });
        run_pass(dir.path(), &[("late-host".to_string(), slow)], at(NOW), std::time::Duration::from_millis(100));
        let entry = &StorageFile::load(&dir.path().join(FILE)).hosts["late-host"];
        assert_eq!(entry.datasets.len(), 1, "the last good reading stays");
        assert!(entry.error.as_deref().is_some_and(|e| e.contains("no answer")), "{:?}", entry.error);
    }

    #[test]
    fn a_host_whose_last_question_is_still_unanswered_is_not_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hung: std::sync::Arc<dyn CommandRunner> = std::sync::Arc::new(Slow { hold: std::time::Duration::from_millis(700), asked: asked.clone() });
        let hosts = vec![("hung-once".to_string(), hung)];
        for _ in 0..3 {
            run_pass(dir.path(), &hosts, at(NOW), std::time::Duration::from_millis(50));
        }
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1, "one stuck question, not one per pass");
        std::thread::sleep(std::time::Duration::from_millis(900));
        run_pass(dir.path(), &hosts, at(NOW), std::time::Duration::from_millis(50));
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 2, "asked again once the first one finished");
    }

    #[test]
    fn hosts_that_left_the_config_lose_their_lines() {
        let dir = tempfile::tempdir().unwrap();
        for host in ["keep", "gone"] {
            check_and_merge(dir.path(), host, &FakeRunner::new("").route("zfs", "list", "a\t99\t99\t100\t0\tfilesystem\n"), at(NOW));
        }
        retain_hosts(dir.path(), &["keep".to_string()]);
        let file = StorageFile::load(&dir.path().join(FILE));
        assert_eq!(file.hosts.keys().collect::<Vec<_>>(), ["keep"]);
    }

    fn status_with(special: &str, extra: &str) -> String {
        format!("  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\ttank        ONLINE       0     0     0\n\t  da0       ONLINE       0     0     0\n\tspecial\n{special}{extra}\nerrors: No known data errors\n")
    }

    #[test]
    fn check_host_keeps_the_device_class_facts_and_survives_a_failing_zpool_status() {
        let lone = status_with("\t  da1       ONLINE       0     0     0\n", "");
        let runner = FakeRunner::new("").route("zfs", "list", "").route("zpool", "status", &lone);
        let Outcome::Checked(r) = check_host(&runner, at(NOW)) else { panic!() };
        assert_eq!(r.pools.len(), 1);
        assert_eq!(r.pools[0].unprotected, ["special"]);
        let quiet = FakeRunner::new("").route("zfs", "list", "").route("zpool", "status", &status_with("\t  mirror-1    ONLINE       0     0     0\n\t    da1       ONLINE       0     0     0\n\t    da2       ONLINE       0     0     0\n", ""));
        let Outcome::Checked(r) = check_host(&quiet, at(NOW)) else { panic!() };
        assert!(r.pools.is_empty(), "quiet pools are not stored");
        let broken = FakeRunner::new("").route("zfs", "list", "a\t9\t9\t10\t0\tfilesystem\n").route_failure("zpool", "status", "cannot open", 1);
        let Outcome::Checked(r) = check_host(&broken, at(NOW)) else { panic!() };
        assert!(r.pools.is_empty() && r.datasets.len() == 1, "the quota lines survive a zpool status that fails");
    }

    #[test]
    fn a_lone_special_disk_is_an_issue_and_a_faulted_log_is_a_watch_one_line_per_host() {
        let mut file = file_with("hub", vec![]);
        file.hosts.get_mut("hub").unwrap().pools = vec![PoolClasses { name: "tank".into(), unprotected: vec!["special".into()], log_bad: true, cache_bad: false }, PoolClasses { name: "t2".into(), unprotected: vec![], log_bad: false, cache_bad: true }];
        let p = pool_problems(&file);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].severity, Severity::Issue);
        assert_eq!(p[0].text, "tank special vdev has no redundancy; tank log device faulted; t2 cache device faulted");
        file.hosts.get_mut("hub").unwrap().pools = vec![PoolClasses { name: "t2".into(), unprotected: vec![], log_bad: true, cache_bad: false }];
        assert_eq!(pool_problems(&file)[0].severity, Severity::Watch);
        file.hosts.get_mut("hub").unwrap().pools.clear();
        assert!(pool_problems(&file).is_empty());
    }

    #[test]
    fn integrity_facts_are_read_from_real_shaped_output() {
        let settings = "tpool/ck\tchecksum\toff\ntpool/ck\tdedup\ton\nrpool/dump\tchecksum\toff\nrpool/dump\tdedup\toff\ntank/a\tdedup\tsha256,verify\ntank/b\tdedup\tverify\nodd\tline\n";
        let pools = "rpool\t10434462720\t1.00\ntank\t26843545600\t1.02x\nweird\t5\t-\nbad\tx\t1.5\n";
        let i = parse_integrity(settings, pools);
        assert_eq!(i.checksum_off, ["tpool/ck"], "the dump device is skipped");
        assert_eq!(i.dedup_pools, ["tpool", "tank"], "one entry per pool, and `off` is not dedup");
        assert_eq!(i.pools, [PoolSpace { name: "rpool".into(), allocated: 10_434_462_720, ratio_x100: 100 }, PoolSpace { name: "tank".into(), allocated: 26_843_545_600, ratio_x100: 102 }]);
        assert_eq!(parse_integrity("", ""), Integrity::default());
    }

    #[test]
    fn the_version_is_the_first_line_without_the_prefix() {
        assert_eq!(parse_version("zfs-2.4.2-1-FreeBSD_g6330a45b0\nzfs-kmod-2.4.2-1-FreeBSD_g6330a45b0\n").as_deref(), Some("2.4.2-1-FreeBSD_g6330a45b0"));
        assert_eq!(parse_version("zfs-2.2.2-0ubuntu9.5\n").as_deref(), Some("2.2.2-0ubuntu9.5"));
        assert_eq!(parse_version("unrecognized command 'version'\nusage: zfs command args ...\n"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn each_integrity_command_may_fail_alone_and_asks_with_options_first() {
        let runner = FakeRunner::new("")
            .route("zfs", "list", "a\t9\t9\t10\t0\tfilesystem\n")
            .route("zfs", "get", "tank/x\tchecksum\toff\n")
            .route_failure("zpool", "list", "no pools available", 1)
            .route_failure("zfs", "version", "unrecognized command", 1);
        let Outcome::Checked(r) = check_host(&runner, at(NOW)) else { panic!() };
        assert_eq!((r.integrity.checksum_off.clone(), r.integrity.pools.len(), r.zfs_version.clone(), r.datasets.len()), (vec!["tank/x".to_string()], 0, None, 1));
        let calls = runner.calls.lock().unwrap().clone();
        let get = calls.iter().find(|c| c.0 == "zfs" && c.1.first().map(String::as_str) == Some("get")).unwrap();
        assert_eq!(get.1, ["get", "-H", "-p", "-s", "local,received", "-t", "filesystem,volume", "-o", "name,property,value", "checksum,dedup"].map(String::from));
        let list = calls.iter().find(|c| c.0 == "zpool" && c.1.first().map(String::as_str) == Some("list")).unwrap();
        assert_eq!(list.1, ["list", "-H", "-p", "-o", "name,allocated,dedupratio"].map(String::from));
    }

    #[test]
    fn the_file_round_trips_and_a_torn_one_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage.json");
        let file = file_with("hub", vec![dataset("a", 1, 1, 2, 0)]);
        file.save(&path).unwrap();
        assert_eq!(StorageFile::load(&path), file);
        std::fs::write(&path, "{ torn").unwrap();
        assert_eq!(StorageFile::load(&path), StorageFile::default());
    }
}
