//! The storage check: which datasets are close to their quota. One `zfs list` per host, kept in `storage.json` for the
//! Dashboard and the daily report; it runs every few minutes (a quota line must clear soon after the fix) and on demand.
//! Reads only. Later slices (hybrid pools, checksum and dedup, encryption) add their own readings to the same file.

use crate::exec::CommandRunner;
use crate::smart::Severity;
use crate::zfs::quota::{usage, Facts};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::path::Path;

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

/// Asks one host which of its datasets have a quota and how full they are.
pub fn check_host(runner: &dyn CommandRunner, now: DateTime<Utc>) -> Outcome {
    match runner.run("zfs", LIST_ARGS) {
        Err(e) => Outcome::Failed(e.to_string()),
        Ok(out) if out.status == 255 => Outcome::Failed(super::shares_check::first_line(&out.stderr, "could not reach the host")),
        Ok(out) if !out.success() && super::shares_check::is_refusal(&out.stderr) => Outcome::Silent,
        Ok(out) if !out.success() => Outcome::Failed(super::shares_check::first_line(&out.stderr, "zfs list failed")),
        Ok(out) => Outcome::Checked(StorageResult { checked_at: now, datasets: parse_limited(&out.stdout), error: None }),
    }
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
        file.hosts.insert(host.into(), StorageResult { checked_at: at(NOW), datasets, error: None });
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
        file.apply("hub", Outcome::Checked(StorageResult { checked_at: at(NOW), datasets: vec![], error: None }), at(NOW));
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
