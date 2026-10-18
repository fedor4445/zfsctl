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
    /// When each pool in `dedup_pools` was first seen with dedup on (kept across passes): the ratio of a pool that has only just
    /// started deduplicating is 1.00x however much data it holds, so the line waits `DEDUP_GRACE`.
    #[serde(default)]
    pub dedup_since: std::collections::BTreeMap<String, DateTime<Utc>>,
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

/// An encrypted dataset whose key is not loaded, and whether it is shared (`sharenfs` / `sharesmb` on).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LockedDataset {
    pub name: String,
    #[serde(default)]
    pub shared: bool,
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
    /// Encrypted datasets whose key is not loaded (`zfs get keystatus,sharenfs,sharesmb`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locked: Vec<LockedDataset>,
    /// An integrity or key command could not be asked (ssh dropped), so `integrity`, `zfs_version` and `locked` are not this
    /// pass's: `apply` keeps the last good ones. Not stored.
    #[serde(skip)]
    pub integrity_stale: bool,
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
            Outcome::Checked(mut result) => {
                let old = self.hosts.get(host);
                if result.integrity_stale {
                    // The host could not be asked about its integrity settings this time: what was known stays.
                    if let Some(old) = old {
                        result.integrity = old.integrity.clone();
                        result.zfs_version.clone_from(&old.zfs_version);
                        result.locked.clone_from(&old.locked);
                    }
                } else {
                    // Remember since when each pool has had dedup on; a pool seen for the first time starts now.
                    let since: std::collections::BTreeMap<String, DateTime<Utc>> = result
                        .integrity
                        .dedup_pools
                        .iter()
                        .map(|pool| (pool.clone(), old.and_then(|o| o.integrity.dedup_since.get(pool)).copied().unwrap_or(now)))
                        .collect();
                    result.integrity.dedup_since = since;
                }
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
            // Illumos's dump device is exactly `<pool>/dump`; a user's `tank/backups/dump` is not it.
            ("checksum", "off") if name.split('/').count() != 2 || !name.ends_with("/dump") => out.checksum_off.push(name.to_string()),
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

/// `zfs get -H -t filesystem -o name,property,value keystatus,sharenfs,sharesmb`: the datasets whose key is not loaded.
fn parse_locked(output: &str) -> Vec<LockedDataset> {
    let mut seen: BTreeMap<&str, (bool, bool)> = BTreeMap::new();
    for line in output.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let &[name, property, value] = f.as_slice() else { continue };
        let entry = seen.entry(name).or_default();
        match property {
            "keystatus" => entry.0 = value == "unavailable",
            "sharenfs" | "sharesmb" if !matches!(value, "off" | "-") => entry.1 = true,
            _ => {}
        }
    }
    seen.into_iter().filter(|(_, (locked, _))| *locked).map(|(name, (_, shared))| LockedDataset { name: name.to_string(), shared }).collect()
}

/// What one host adds to Needs attention for locked datasets that something depends on: a dataset a replication, backup or
/// mirror job reads (or writes) on that host, or one that is shared. A locked dataset nobody uses is not a problem.
pub fn locked_problems(file: &StorageFile, jobs: &[crate::jobs::Job]) -> Vec<StorageProblem> {
    use crate::jobs::{KIND_BACKUP, KIND_MIRROR, KIND_REPLICATE};
    file.hosts
        .iter()
        .filter_map(|(host, result)| {
            let uses = |locked: &str| {
                jobs.iter().filter(|j| j.enabled && matches!(j.kind.as_str(), KIND_REPLICATE | KIND_BACKUP | KIND_MIRROR)).any(|j| {
                    // A replication or mirror source is refused when encrypted, key or no key: loading the key would not help.
                    let reads = j.kind == KIND_BACKUP && j.host == *host && j.dataset.as_deref().is_some_and(|d| locked == d || (j.recursive && locked.strip_prefix(d).is_some_and(|rest| rest.starts_with('/'))));
                    // A raw replica is meant to stay locked, and a receive into it works without the key.
                    let writes = !j.raw && j.dest_host.as_deref() == Some(host.as_str()) && j.dest_dataset.as_deref() == Some(locked);
                    reads || writes
                })
            };
            let mut texts: Vec<String> = result
                .locked
                .iter()
                .filter_map(|l| {
                    let (job, shared) = (uses(&l.name), l.shared);
                    let why = match (job, shared) {
                        (false, false) => return None,
                        (true, false) => "a job uses it",
                        (false, true) => "shared",
                        (true, true) => "a job uses it, shared",
                    };
                    Some(format!("{} is locked ({why})", l.name))
                })
                .collect();
            let more = texts.len().saturating_sub(2);
            texts.truncate(2);
            let text = format!("{}{}", texts.join("; "), if more > 0 { format!(" (+{more} more)") } else { String::new() });
            (!texts.is_empty()).then(|| StorageProblem { host: host.clone(), severity: Severity::Watch, text, checked_at: result.checked_at })
        })
        .collect()
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
            // An answer that is an error (an old zfs, a refusal) is empty; a connection that dropped (Err or ssh's 255) is not an
            // answer at all, and must not wipe what was known.
            let mut lost = false;
            let mut text = |cmd: &str, args: &[&str]| match runner.run(cmd, args) {
                Ok(o) if o.status == 255 => {
                    lost = true;
                    String::new()
                }
                Ok(o) if o.success() => o.stdout,
                Ok(_) => String::new(),
                Err(_) => {
                    lost = true;
                    String::new()
                }
            };
            let settings = text("zfs", &["get", "-H", "-p", "-s", "local,received", "-t", "filesystem,volume", "-o", "name,property,value", "checksum,dedup"]);
            let pool_list = text("zpool", &["list", "-H", "-p", "-o", "name,allocated,dedupratio"]);
            let zfs_version = parse_version(&text("zfs", &["version"]));
            let keys = text("zfs", &["get", "-H", "-t", "filesystem", "-o", "name,property,value", "keystatus,sharenfs,sharesmb"]);
            Outcome::Checked(StorageResult { checked_at: now, datasets: parse_limited(&out.stdout), pools, zfs_version, locked: parse_locked(&keys), integrity: parse_integrity(&settings, &pool_list), integrity_stale: lost, error: None })
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

/// How long a pool must have had dedup on before a poor ratio is reported (the ratio is 1.00x until duplicates are written).
pub const DEDUP_GRACE: chrono::Duration = chrono::Duration::days(7);

/// What one host adds to Needs attention for its integrity settings: a checksum that is off (up to three datasets named), and a
/// pool paying for deduplication that saves less than `min_ratio_x100` (hundredths, 110 = 1.10x). A pool with under 10 GiB
/// allocated is left alone (dedup just switched on has nothing to dedupe yet).
pub fn integrity_problems(file: &StorageFile, min_ratio_x100: u16, now: DateTime<Utc>) -> Vec<StorageProblem> {
    const ENOUGH_DATA: u64 = 10 << 30;
    file.hosts
        .iter()
        .filter_map(|(host, result)| {
            let i = &result.integrity;
            let mut texts = Vec::new();
            for pool in &i.dedup_pools {
                // The ratio only counts data written since dedup was turned on: wait before judging a pool that has just started.
                let settled = i.dedup_since.get(pool).is_some_and(|since| now - *since >= DEDUP_GRACE);
                if let Some(space) = i.pools.iter().find(|p| &p.name == pool).filter(|p| settled && p.allocated > ENOUGH_DATA && p.ratio_x100 < u32::from(min_ratio_x100)) {
                    texts.push(format!("{pool} dedup is on but saves only {:.2}x", f64::from(space.ratio_x100) / 100.0));
                }
            }
            if !i.checksum_off.is_empty() {
                let shown = i.checksum_off.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
                let more = if i.checksum_off.len() > 3 { format!(" (+{} more)", i.checksum_off.len() - 3) } else { String::new() };
                texts.push(format!("{shown} checksum is off{more}"));
            }
            (!texts.is_empty()).then(|| StorageProblem { host: host.clone(), severity: Severity::Watch, text: texts.join("; "), checked_at: result.checked_at })
        })
        .collect()
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
        file.hosts.insert(host.into(), StorageResult { checked_at: at(NOW), datasets, pools: vec![], zfs_version: None, locked: vec![], integrity: crate::daemon::storage_check::Integrity::default(), integrity_stale: false, error: None });
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
        file.apply("hub", Outcome::Checked(StorageResult { checked_at: at(NOW), datasets: vec![], pools: vec![], zfs_version: None, locked: vec![], integrity: crate::daemon::storage_check::Integrity::default(), integrity_stale: false, error: None }), at(NOW));
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
        let nested = parse_integrity("tank/backups/dump\tchecksum\toff\nrpool/dump\tchecksum\toff\nzroot/dump\tchecksum\toff\n", "");
        assert_eq!(nested.checksum_off, ["tank/backups/dump"], "only `<pool>/dump` is the dump device");
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
    fn a_pool_with_dedup_on_that_saves_nothing_is_one_watch_line_and_checksum_off_names_three() {
        const GIB: u64 = 1 << 30;
        let mut file = file_with("hub", vec![]);
        let entry = file.hosts.get_mut("hub").unwrap();
        entry.integrity = Integrity { checksum_off: vec![], dedup_pools: vec!["tank".into()], pools: vec![PoolSpace { name: "tank".into(), allocated: 20 * GIB, ratio_x100: 102 }], dedup_since: [("tank".to_string(), at(NOW))].into() };
        let p = integrity_problems(&file, 110, at(NOW) + chrono::Duration::days(30));
        assert_eq!((p.len(), p[0].severity, p[0].text.as_str()), (1, Severity::Watch, "tank dedup is on but saves only 1.02x"));
        assert!(integrity_problems(&file, 100, at(NOW) + chrono::Duration::days(30)).is_empty(), "1.02 is not below 1.00");
        let mut edit = |f: &mut dyn FnMut(&mut Integrity), file: &mut StorageFile| f(&mut file.hosts.get_mut("hub").unwrap().integrity);
        edit(&mut |i| i.pools[0].ratio_x100 = 150, &mut file);
        assert!(integrity_problems(&file, 110, at(NOW) + chrono::Duration::days(30)).is_empty(), "it saves 1.5x");
        edit(&mut |i| i.pools[0] = PoolSpace { name: "tank".into(), allocated: 5 * GIB, ratio_x100: 100 }, &mut file);
        assert!(integrity_problems(&file, 110, at(NOW) + chrono::Duration::days(30)).is_empty(), "under 10 GiB: nothing to dedupe yet");
        edit(&mut |i| i.dedup_pools.clear(), &mut file);
        edit(&mut |i| i.pools[0].allocated = 50 * GIB, &mut file);
        assert!(integrity_problems(&file, 110, at(NOW) + chrono::Duration::days(30)).is_empty(), "no dataset has dedup on");
        edit(&mut |i| i.checksum_off = ["a", "b", "c", "d", "e"].map(String::from).to_vec(), &mut file);
        assert_eq!(integrity_problems(&file, 110, at(NOW) + chrono::Duration::days(30))[0].text, "a, b, c checksum is off (+2 more)");
        edit(&mut |i| i.checksum_off = vec!["tank/scratch".into()], &mut file);
        assert_eq!(integrity_problems(&file, 110, at(NOW) + chrono::Duration::days(30))[0].text, "tank/scratch checksum is off");
    }

    #[test]
    fn a_pool_that_only_just_started_deduplicating_is_not_judged_until_the_grace_has_passed() {
        const GIB: u64 = 1 << 30;
        let mut file = StorageFile::default();
        let answer = |dedup: bool| StorageResult { checked_at: at(NOW), datasets: vec![], pools: vec![], zfs_version: None, locked: vec![], integrity: Integrity { checksum_off: vec![], dedup_pools: if dedup { vec!["tank".into()] } else { vec![] }, pools: vec![PoolSpace { name: "tank".into(), allocated: 2000 * GIB, ratio_x100: 100 }], dedup_since: std::collections::BTreeMap::default() }, integrity_stale: false, error: None };
        file.apply("hub", Outcome::Checked(answer(true)), at(NOW));
        assert_eq!(file.hosts["hub"].integrity.dedup_since["tank"], at(NOW), "first seen now");
        assert!(integrity_problems(&file, 110, at(NOW) + chrono::Duration::days(1)).is_empty(), "2 TiB of old data, ratio 1.00x, one day after enabling: not yet");
        // a later pass keeps the first-seen time
        file.apply("hub", Outcome::Checked(answer(true)), at(NOW) + chrono::Duration::days(2));
        assert_eq!(file.hosts["hub"].integrity.dedup_since["tank"], at(NOW));
        assert_eq!(integrity_problems(&file, 110, at(NOW) + chrono::Duration::days(8)).len(), 1, "a week on, still 1.00x: now it is paying for nothing");
        // dedup switched off and on again starts a new clock
        file.apply("hub", Outcome::Checked(answer(false)), at(NOW) + chrono::Duration::days(9));
        assert!(file.hosts["hub"].integrity.dedup_since.is_empty());
        file.apply("hub", Outcome::Checked(answer(true)), at(NOW) + chrono::Duration::days(10));
        assert_eq!(file.hosts["hub"].integrity.dedup_since["tank"], at(NOW) + chrono::Duration::days(10));
    }

    #[test]
    fn a_dropped_connection_on_an_integrity_command_keeps_what_was_known_but_an_unsupported_one_does_not() {
        let dir_runner = |zfs_get: Result<crate::exec::CommandOutput, ()>| {
            struct R(Result<crate::exec::CommandOutput, ()>);
            impl CommandRunner for R {
                fn run(&self, cmd: &str, args: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
                    let ok = |s: &str| Ok(crate::exec::CommandOutput { stdout: s.to_string(), stderr: String::new(), status: 0 });
                    match (cmd, args.first().copied()) {
                        ("zfs", Some("list")) => ok("a\\t9\\t9\\t10\\t0\\tfilesystem\\n"),
                        ("zfs", Some("get")) => self.0.clone().map_err(|()| crate::error::AppError::Parse("ssh went away".into())),
                        _ => ok(""),
                    }
                }
                fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<crate::exec::CommandOutput> {
                    self.run(c, a)
                }
                fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
                    self.run(c, a)
                }
            }
            R(zfs_get)
        };
        let mut file = file_with("hub", vec![]);
        file.hosts.get_mut("hub").unwrap().integrity.checksum_off = vec!["tank/scratch".into()];
        file.hosts.get_mut("hub").unwrap().zfs_version = Some("2.4.2".into());
        let Outcome::Checked(lost) = check_host(&dir_runner(Err(())), at(NOW)) else { panic!() };
        assert!(lost.integrity_stale);
        file.apply("hub", Outcome::Checked(lost), at(NOW));
        assert_eq!(file.hosts["hub"].integrity.checksum_off, ["tank/scratch"], "the line must not vanish because ssh dropped");
        assert_eq!(file.hosts["hub"].zfs_version.as_deref(), Some("2.4.2"));
        // an old zfs that does not know the command answers with an error: that is an answer, and the list is empty
        let Outcome::Checked(unsupported) = check_host(&dir_runner(Ok(crate::exec::CommandOutput { stdout: String::new(), stderr: "unknown option".into(), status: 2 })), at(NOW)) else { panic!() };
        assert!(!unsupported.integrity_stale);
        file.apply("hub", Outcome::Checked(unsupported), at(NOW));
        assert!(file.hosts["hub"].integrity.checksum_off.is_empty());
    }

    const KEYS: &str = "tank/secure\tkeystatus\tunavailable\ntank/secure\tsharenfs\ton\ntank/secure\tsharesmb\toff\n\
tank/secure/kid\tkeystatus\tunavailable\ntank/secure/kid\tsharenfs\t-\ntank/secure/kid\tsharesmb\t-\n\
tank/open\tkeystatus\tavailable\ntank/open\tsharenfs\ton\ntank/plain\tkeystatus\t-\ntank/plain\tsharesmb\tname=x\n";

    #[test]
    fn parse_locked_lists_locked_datasets_and_whether_they_are_shared() {
        assert_eq!(parse_locked(KEYS), [LockedDataset { name: "tank/secure".into(), shared: true }, LockedDataset { name: "tank/secure/kid".into(), shared: false }]);
        assert!(parse_locked("").is_empty());
        assert!(parse_locked("tank\tkeystatus\n\nnonsense").is_empty());
    }

    fn locked_file(host: &str, locked: &[(&str, bool)]) -> StorageFile {
        let mut file = file_with(host, vec![]);
        file.hosts.get_mut(host).unwrap().locked = locked.iter().map(|(n, shared)| LockedDataset { name: (*n).into(), shared: *shared }).collect();
        file
    }

    #[test]
    fn a_locked_dataset_is_a_problem_only_when_a_job_or_a_share_uses_it() {
        use crate::jobs::Job;
        let nothing = locked_file("hub", &[("tank/secure", false)]);
        assert!(locked_problems(&nothing, &[]).is_empty(), "nobody uses it");
        let job = Job::test_backup("b", "tank/secure", "hourly", "/r", "/p");
        let got = locked_problems(&nothing, std::slice::from_ref(&job));
        assert!(got.is_empty(), "job host is `local`, the file's host is `hub`");
        let mut on_hub = job.clone();
        on_hub.host = "hub".into();
        let got = locked_problems(&nothing, std::slice::from_ref(&on_hub));
        assert_eq!((got.len(), got[0].text.as_str(), got[0].severity), (1, "tank/secure is locked (a job uses it)", Severity::Watch));
        let shared = locked_file("hub", &[("tank/secure", true)]);
        assert_eq!(locked_problems(&shared, &[])[0].text, "tank/secure is locked (shared)");
        assert_eq!(locked_problems(&shared, std::slice::from_ref(&on_hub))[0].text, "tank/secure is locked (a job uses it, shared)");
    }

    #[test]
    fn which_jobs_use_a_locked_dataset_is_exact_recursive_destination_and_enabled_only() {
        use crate::jobs::Job;
        let file = locked_file("hub", &[("tank/secure/kid", false)]);
        let on_hub = |mut j: Job| {
            j.host = "hub".into();
            j
        };
        let backup = |dataset: &str| on_hub(Job::test_backup("b", dataset, "p", "/r", "/pw"));
        assert!(locked_problems(&file, &[backup("tank/sec")]).is_empty(), "a name prefix is not an ancestor");
        assert!(locked_problems(&file, &[backup("tank/secure")]).is_empty(), "a parent without recursion reads only itself");
        let mut recursive = backup("tank/secure");
        recursive.recursive = true;
        assert_eq!(locked_problems(&file, std::slice::from_ref(&recursive)).len(), 1);
        let mut disabled = backup("tank/secure/kid");
        disabled.enabled = false;
        assert!(locked_problems(&file, &[disabled]).is_empty());
        let snapshot = on_hub(Job::test_snapshot("s", "tank/secure/kid", "p", 3));
        assert!(locked_problems(&file, &[snapshot]).is_empty(), "snapshots work on a locked dataset");
        let mut from = Job::test_replicate("r2", "tank/secure/kid", "p", "elsewhere", "x/y");
        from.host = "hub".into();
        assert!(locked_problems(&file, &[from]).is_empty(), "a replication source is refused whatever the key state");
        let into = Job::test_replicate("r", "tank/src", "p", "hub", "tank/secure/kid");
        assert_eq!(locked_problems(&file, std::slice::from_ref(&into)).len(), 1, "a receive into a locked destination fails");
        let mut raw = into.clone();
        raw.raw = true;
        assert!(locked_problems(&file, &[raw]).is_empty(), "a raw replica is meant to stay locked: no line");
    }

    #[test]
    fn several_locked_datasets_show_two_names_and_a_count() {
        let mut file = locked_file("hub", &[("a", true), ("b", true), ("c", true), ("d", true)]);
        let got = locked_problems(&file, &[]);
        assert_eq!(got[0].text, "a is locked (shared); b is locked (shared) (+2 more)");
        file.hosts.get_mut("hub").unwrap().locked.clear();
        assert!(locked_problems(&file, &[]).is_empty());
    }

    #[test]
    fn check_host_reads_locked_datasets_and_keeps_the_last_ones_when_the_connection_drops() {
        let Outcome::Checked(got) = check_host(&FakeRunner::new("").route("zfs", "get", KEYS), at(NOW)) else { panic!() };
        assert_eq!(got.locked.len(), 2);
        let mut file = locked_file("hub", &[("tank/old", false)]);
        file.hosts.get_mut("hub").unwrap().locked = got.locked.clone();
        let Outcome::Checked(lost) = check_host(&FakeRunner::new("").route_failure("zfs", "get", "Connection closed", 255), at(NOW)) else { panic!("the list answered") };
        assert!(lost.integrity_stale && lost.locked.is_empty());
        file.apply("hub", Outcome::Checked(lost), at(NOW));
        assert_eq!(file.hosts["hub"].locked.len(), 2, "kept");
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
