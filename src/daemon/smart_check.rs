//! The daily disk health check: asks every host's `smartctl` once a day and keeps the answers in `smart.json` for the
//! Dashboard and the daily report. Reads only; a host that cannot answer keeps its previous result.

use crate::exec::CommandRunner;
use crate::smart::Disk;
use chrono::{DateTime, NaiveDateTime, NaiveTime, Utc};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// How long one host may take: a scan and a read of each disk.
pub const HOST_DEADLINE: Duration = Duration::from_mins(2);

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HostResult {
    pub checked_at: DateTime<Utc>,
    pub disks: Vec<Disk>,
    /// Why the latest check did not happen; `disks` and `checked_at` are then from the last one that did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SmartFile {
    #[serde(default)]
    pub hosts: BTreeMap<String, HostResult>,
    /// When the last run started, even if no host had anything to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_at: Option<DateTime<Utc>>,
}

impl SmartFile {
    /// The file at `path`, or an empty one when it is missing, torn or of another shape.
    pub fn load(path: &Path) -> SmartFile {
        std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    /// Writes through a temporary file and a rename, so a crash leaves the old file or the new one.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    }

    /// When the daily check last ran: the recorded run, else the newest host check.
    pub fn last_run(&self) -> Option<DateTime<Utc>> {
        self.ran_at.or_else(|| self.hosts.values().map(|h| h.checked_at).max())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The host has no usable `smartctl` (not installed, sudo says no, no disk reports): nothing to show.
    Silent,
    Checked(HostResult),
    /// The host could not be asked this time (ssh down, a run that errored).
    Failed(String),
}

/// Asks one host: the device list, then each device. Only the read-only flags are ever sent. A scan that comes back
/// empty (no `smartctl`, sudo says no, nothing listed) is silent, because that is how a host that was never set up
/// answers; only a host that could not be reached at all is a failure.
pub fn check_host(runner: &dyn CommandRunner, now: DateTime<Utc>) -> Outcome {
    let unreachable = |out: &crate::exec::CommandOutput| Outcome::Failed(out.stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("could not reach the host").trim().to_string());
    let scan = match runner.run("smartctl", &["--scan", "-j"]) {
        Ok(out) if out.status == 255 => return unreachable(&out),
        Ok(out) => out,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let mut disks = Vec::new();
    for dev in crate::smart::parse_scan(&scan.stdout) {
        let path = format!("/dev/{dev}");
        // smartctl's exit status is a bit mask of what it found, so only 255 (ssh) says the run itself failed.
        match runner.run("smartctl", &["-j", "-H", "-A", "-i", &path]) {
            Ok(out) if out.status == 255 => return unreachable(&out),
            Ok(out) => disks.extend(crate::smart::judge(&dev, &out.stdout)),
            Err(e) => return Outcome::Failed(e.to_string()),
        }
    }
    if disks.is_empty() {
        Outcome::Silent
    } else {
        Outcome::Checked(HostResult { checked_at: now, disks, error: None })
    }
}

/// Checks every host at once and waits until `deadline`; a host that has not answered then is `Failed`. A silent or
/// failed host keeps the entry it had.
pub fn run_all(hosts: &[(String, Arc<dyn CommandRunner>)], now: DateTime<Utc>, previous: &SmartFile, deadline: Duration) -> SmartFile {
    let (tx, rx) = std::sync::mpsc::channel();
    for (name, runner) in hosts {
        let (tx, name, runner) = (tx.clone(), name.clone(), Arc::clone(runner));
        // A thread that outlives the deadline is left to finish by itself, as the health probe's are.
        std::thread::spawn(move || {
            let _ = tx.send((name, check_host(runner.as_ref(), now)));
        });
    }
    drop(tx);
    let started = std::time::Instant::now();
    let mut answers: BTreeMap<String, Outcome> = BTreeMap::new();
    while answers.len() < hosts.len() {
        let Some(left) = deadline.checked_sub(started.elapsed()) else { break };
        match rx.recv_timeout(left) {
            Ok((name, outcome)) => {
                answers.insert(name, outcome);
            }
            Err(_) => break,
        }
    }
    let mut file = SmartFile { ran_at: Some(now), ..SmartFile::default() };
    for (name, _) in hosts {
        let kept = |why: String| previous.hosts.get(name).map(|old| HostResult { error: Some(why), ..old.clone() });
        let entry = match answers.remove(name) {
            Some(Outcome::Checked(result)) => Some(result),
            Some(Outcome::Silent) => kept("smartctl is no longer available on this host".to_string()),
            Some(Outcome::Failed(why)) => kept(why),
            None => kept(format!("no answer within {} s", deadline.as_secs())),
        };
        if let Some(entry) = entry {
            file.hosts.insert(name.clone(), entry);
        }
    }
    file
}

/// Whether the daily check is due: it is switched on (`at`), and either it never ran or is more than a day old, or
/// the local time has passed `at` on a day the last check did not happen.
pub fn due(now_local: NaiveDateTime, at: Option<NaiveTime>, last_local: Option<NaiveDateTime>) -> bool {
    let Some(at) = at else { return false };
    let Some(last) = last_local else { return true };
    now_local - last >= chrono::Duration::hours(24) || (now_local.time() >= at && last.date() < now_local.date())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AppError, Result};
    use crate::exec::CommandOutput;
    use crate::smart::Severity;

    const SCAN: &str = include_str!("../smart_fixtures/scan.json");
    const HEALTHY: &str = include_str!("../smart_fixtures/hdd_ok.json");
    const FAILING: &str = include_str!("../smart_fixtures/hdd_failed.json");
    const NO_SUPPORT: &str = include_str!("../smart_fixtures/no_support.json");

    struct Script(Box<dyn Fn(&str, &[&str]) -> Result<CommandOutput> + Send + Sync>);

    impl CommandRunner for Script {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            (self.0)(cmd, args)
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _stdin: &str) -> Result<CommandOutput> {
            (self.0)(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            (self.0)(cmd, args)
        }
    }

    fn out(stdout: &str, status: i32) -> Result<CommandOutput> {
        Ok(CommandOutput { stdout: stdout.to_string(), stderr: String::new(), status })
    }

    /// A host whose scan lists `ada0` and `nvme0`, whose first disk answers `first` and whose second answers `second`.
    fn host(first: &'static str, second: &'static str) -> Script {
        Script(Box::new(move |cmd, args| {
            assert_eq!(cmd, "smartctl");
            if args[0] == "--scan" {
                return out(SCAN, 0);
            }
            assert_eq!(&args[..4], ["-j", "-H", "-A", "-i"], "only the read-only flags");
            match args[4] {
                "/dev/ada0" => out(first, 0),
                "/dev/nvme0" => out(second, 4),
                other => panic!("unexpected device {other}"),
            }
        }))
    }

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn checked(outcome: Outcome) -> HostResult {
        match outcome {
            Outcome::Checked(r) => r,
            other => panic!("expected a result, got {other:?}"),
        }
    }

    #[test]
    fn a_host_with_a_failing_disk_gets_an_entry_with_its_problem() {
        let result = checked(check_host(&host(FAILING, HEALTHY), at("2026-10-17T03:30:00Z")));
        assert_eq!(result.checked_at, at("2026-10-17T03:30:00Z"));
        assert_eq!(result.error, None);
        assert_eq!(result.disks.len(), 2, "the healthy disk is kept too");
        assert_eq!((result.disks[0].dev.as_str(), result.disks[0].problems[0].severity), ("ada0", Severity::Issue));
        assert!(result.disks[1].problems.is_empty());
    }

    #[test]
    fn a_host_without_smartctl_or_with_sudo_denied_or_no_reporting_disk_is_silent() {
        let denied = Script(Box::new(|_, _| Ok(CommandOutput { stdout: String::new(), stderr: "sudo: a password is required".into(), status: 1 })));
        assert_eq!(check_host(&denied, at("2026-10-17T03:30:00Z")), Outcome::Silent);
        let missing = Script(Box::new(|_, _| Ok(CommandOutput { stdout: String::new(), stderr: "sudo: smartctl: command not found".into(), status: 1 })));
        assert_eq!(check_host(&missing, at("2026-10-17T03:30:00Z")), Outcome::Silent);
        assert_eq!(check_host(&host(NO_SUPPORT, NO_SUPPORT), at("2026-10-17T03:30:00Z")), Outcome::Silent, "virtual disks report nothing");
        let empty = Script(Box::new(|_, _| out("{\"devices\":[]}", 0)));
        assert_eq!(check_host(&empty, at("2026-10-17T03:30:00Z")), Outcome::Silent);
    }

    #[test]
    fn a_host_that_cannot_be_asked_is_failed_with_the_reason() {
        let down = Script(Box::new(|_, _| Err(AppError::Config("ssh: connect to host h port 22: No route to host".into()))));
        let Outcome::Failed(why) = check_host(&down, at("2026-10-17T03:30:00Z")) else { panic!("expected a failure") };
        assert!(why.contains("No route"), "{why}");
        let ssh_255 = Script(Box::new(|_, _| Ok(CommandOutput { stdout: String::new(), stderr: "ssh: Connection timed out".into(), status: 255 })));
        assert!(matches!(check_host(&ssh_255, at("2026-10-17T03:30:00Z")), Outcome::Failed(_)));
    }

    fn runners(list: Vec<(&str, Script)>) -> Vec<(String, Arc<dyn CommandRunner>)> {
        list.into_iter().map(|(n, s)| (n.to_string(), Arc::new(s) as Arc<dyn CommandRunner>)).collect()
    }

    #[test]
    fn a_failed_check_keeps_the_previous_result_and_records_the_error() {
        let mut previous = SmartFile::default();
        let old = HostResult { checked_at: at("2026-10-16T03:30:00Z"), disks: vec![Disk { dev: "ada0".into(), problems: vec![] }], error: None };
        previous.hosts.insert("h".into(), old.clone());
        let down = Script(Box::new(|_, _| Err(AppError::Config("no route".into()))));
        let file = run_all(&runners(vec![("h", down)]), at("2026-10-17T03:30:00Z"), &previous, Duration::from_secs(5));
        let kept = &file.hosts["h"];
        assert_eq!((kept.checked_at, &kept.disks), (old.checked_at, &old.disks));
        assert!(kept.error.as_deref().unwrap_or("").contains("no route"), "{kept:?}");
        // and a host that has never answered gets no entry at all
        let file = run_all(&runners(vec![("new", Script(Box::new(|_, _| Err(AppError::Config("no route".into())))))]), at("2026-10-17T03:30:00Z"), &SmartFile::default(), Duration::from_secs(5));
        assert!(file.hosts.is_empty());
    }

    #[test]
    fn a_silent_host_that_used_to_report_keeps_its_entry_with_a_note() {
        let mut previous = SmartFile::default();
        previous.hosts.insert("h".into(), HostResult { checked_at: at("2026-10-16T03:30:00Z"), disks: vec![], error: None });
        let gone = Script(Box::new(|_, _| Ok(CommandOutput { stdout: String::new(), stderr: "sudo: smartctl: command not found".into(), status: 1 })));
        let file = run_all(&runners(vec![("h", gone)]), at("2026-10-17T03:30:00Z"), &previous, Duration::from_secs(5));
        assert!(file.hosts["h"].error.as_deref().unwrap_or("").contains("no longer"), "{:?}", file.hosts["h"]);
    }

    #[test]
    fn a_run_is_dated_even_when_every_host_is_silent_so_it_is_not_repeated_every_few_minutes() {
        let silent = Script(Box::new(|_, _| Ok(CommandOutput { stdout: String::new(), stderr: "sudo: a password is required".into(), status: 1 })));
        let file = run_all(&runners(vec![("h", silent)]), at("2026-10-17T03:30:00Z"), &SmartFile::default(), Duration::from_secs(5));
        assert!(file.hosts.is_empty());
        assert_eq!(file.ran_at, Some(at("2026-10-17T03:30:00Z")));
        assert_eq!(file.last_run(), Some(at("2026-10-17T03:30:00Z")));
    }

    #[test]
    fn a_hung_host_does_not_stop_the_others() {
        let hung = Script(Box::new(|_, _| {
            std::thread::sleep(Duration::from_secs(3));
            out("", 0)
        }));
        let started = std::time::Instant::now();
        let file = run_all(&runners(vec![("hung", hung), ("good", host(FAILING, HEALTHY))]), at("2026-10-17T03:30:00Z"), &SmartFile::default(), Duration::from_millis(500));
        assert!(started.elapsed() < Duration::from_secs(2), "waited for the hung host");
        assert!(file.hosts.contains_key("good") && !file.hosts.contains_key("hung"), "{file:?}");
    }

    #[test]
    fn save_is_atomic_and_load_survives_a_missing_torn_or_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smart.json");
        assert_eq!(SmartFile::load(&path), SmartFile::default());
        let mut file = SmartFile::default();
        file.hosts.insert("h".into(), HostResult { checked_at: at("2026-10-17T03:30:00Z"), disks: vec![Disk { dev: "ada0".into(), problems: vec![] }], error: None });
        file.save(&path).unwrap();
        assert_eq!(SmartFile::load(&path), file);
        assert!(!dir.path().join("smart.json.tmp").exists());
        for torn in ["", "{\"hosts\":{\"h\":{\"checked", "[1,2]", "null"] {
            std::fs::write(&path, torn).unwrap();
            assert_eq!(SmartFile::load(&path), SmartFile::default(), "{torn:?}");
        }
    }

    fn local(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn due_once_per_local_day_from_the_time_on_and_at_start_when_the_file_is_missing_or_old() {
        let at_0330 = NaiveTime::from_hms_opt(3, 30, 0);
        assert!(!due(local("2026-10-17 12:00"), None, None), "off is off");
        assert!(due(local("2026-10-17 01:00"), at_0330, None), "never ran: check now");
        assert!(!due(local("2026-10-17 03:00"), at_0330, Some(local("2026-10-16 03:31"))), "before the time, and the last one is under a day old");
        assert!(due(local("2026-10-17 03:30"), at_0330, Some(local("2026-10-16 03:31"))), "the time has come on a new day");
        assert!(!due(local("2026-10-17 15:00"), at_0330, Some(local("2026-10-17 03:31"))), "already done today");
        assert!(due(local("2026-10-17 01:00"), at_0330, Some(local("2026-10-15 20:00"))), "more than a day old: check at start");
    }
}
