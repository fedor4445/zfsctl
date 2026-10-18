//! The daily disk health check: asks every host's `smartctl` once a day and keeps the answers in `smart.json` for the
//! Dashboard and the daily report. Reads only; a host that cannot answer keeps its previous result.

use crate::exec::CommandRunner;
use crate::smart::Disk;
use chrono::{DateTime, NaiveDateTime, NaiveTime, Utc};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// How long one host may take: a scan and a read of each disk.
pub const HOST_DEADLINE: Duration = Duration::from_mins(2);
/// How long `check_host` keeps asking disks before it marks the rest "not checked in time", so a slow host still
/// reports what it did learn inside `HOST_DEADLINE`.
pub const HOST_BUDGET: Duration = Duration::from_secs(100);

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

    /// When the daily check last ran: the recorded run, else the newest host check. Only for deciding when to run again.
    pub fn last_run(&self) -> Option<DateTime<Utc>> {
        self.ran_at.or_else(|| self.newest_check())
    }

    /// When any host last gave an answer. A run that learns nothing does not move this, so it shows a broken check.
    pub fn newest_check(&self) -> Option<DateTime<Utc>> {
        self.hosts.values().map(|h| h.checked_at).max()
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
pub fn check_host(runner: &dyn CommandRunner, now: DateTime<Utc>, previous: Option<&HostResult>, budget: Duration) -> Outcome {
    let unreachable = |out: &crate::exec::CommandOutput| Outcome::Failed(out.stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("could not reach the host").trim().to_string());
    let scan = match runner.run("smartctl", &["--scan", "-j"]) {
        Ok(out) if out.status == 255 => return unreachable(&out),
        Ok(out) => out,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let unreadable = |dev: &str, why: String| Disk { dev: dev.to_string(), problems: vec![crate::smart::Problem { severity: crate::smart::Severity::Watch, text: format!("SMART unreadable ({why})") }] };
    let mut disks = Vec::new();
    let started = std::time::Instant::now();
    for (asked, dev) in crate::smart::parse_scan(&scan.stdout).into_iter().enumerate() {
        if asked > 0 && started.elapsed() >= budget {
            disks.push(unreadable(&dev, "not checked in time".to_string()));
            continue;
        }
        let path = format!("/dev/{dev}");
        // smartctl's exit status is a bit mask of what it found, so only 255 (ssh) says the run itself failed.
        match runner.run("smartctl", &["-j", "-H", "-A", "-i", &path]) {
            Ok(out) if out.status == 255 => return unreachable(&out),
            Ok(out) => match crate::smart::judge(&dev, &out.stdout) {
                Some(disk) => disks.push(disk),
                // A disk that had a problem yesterday and says nothing today has not got better: do not let it vanish.
                None => {
                    if let Some(was) = previous.and_then(|p| p.disks.iter().find(|d| d.dev == dev)).and_then(|d| d.problems.first()) {
                        disks.push(unreadable(&dev, format!("was: {}", was.text)));
                    }
                }
            },
            Err(e) => return Outcome::Failed(e.to_string()),
        }
    }
    if disks.is_empty() {
        Outcome::Silent
    } else {
        Outcome::Checked(HostResult { checked_at: now, disks, error: None })
    }
}

/// One host's problems in a form both the Dashboard and the daily report can print.
#[derive(Debug, Clone, PartialEq)]
pub struct HostProblems {
    pub host: String,
    /// The worst of the host's problems.
    pub severity: crate::smart::Severity,
    /// `ada0 reallocated sectors 8, temperature 63 C; ada2 SMART health FAILED`
    pub disks: String,
    pub checked_at: DateTime<Utc>,
}

/// The hosts that have at least one disk with a problem, by host name; a host with only healthy disks is left out.
pub fn problems(file: &SmartFile) -> Vec<HostProblems> {
    file.hosts
        .iter()
        .filter_map(|(host, result)| {
            let bad: Vec<&Disk> = result.disks.iter().filter(|d| !d.problems.is_empty()).collect();
            let severity = bad.iter().flat_map(|d| &d.problems).map(|p| p.severity).max()?;
            let disks = bad.iter().map(|d| format!("{} {}", d.dev, d.problems.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(", "))).collect::<Vec<_>>().join("; ");
            Some(HostProblems { host: host.clone(), severity, disks, checked_at: result.checked_at })
        })
        .collect()
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
        let result = checked(check_host(&host(FAILING, HEALTHY), at("2026-10-17T03:30:00Z"), None, Duration::from_secs(60)));
        assert_eq!(result.checked_at, at("2026-10-17T03:30:00Z"));
        assert_eq!(result.error, None);
        assert_eq!(result.disks.len(), 2, "the healthy disk is kept too");
        assert_eq!((result.disks[0].dev.as_str(), result.disks[0].problems[0].severity), ("ada0", Severity::Issue));
        assert!(result.disks[1].problems.is_empty());
    }

    #[test]
    fn a_host_without_smartctl_or_with_sudo_denied_or_no_reporting_disk_is_silent() {
        let denied = Script(Box::new(|_, _| Ok(CommandOutput { stdout: String::new(), stderr: "sudo: a password is required".into(), status: 1 })));
        assert_eq!(check_host(&denied, at("2026-10-17T03:30:00Z"), None, Duration::from_secs(60)), Outcome::Silent);
        let missing = Script(Box::new(|_, _| Ok(CommandOutput { stdout: String::new(), stderr: "sudo: smartctl: command not found".into(), status: 1 })));
        assert_eq!(check_host(&missing, at("2026-10-17T03:30:00Z"), None, Duration::from_secs(60)), Outcome::Silent);
        assert_eq!(check_host(&host(NO_SUPPORT, NO_SUPPORT), at("2026-10-17T03:30:00Z"), None, Duration::from_secs(60)), Outcome::Silent, "virtual disks report nothing");
        let empty = Script(Box::new(|_, _| out("{\"devices\":[]}", 0)));
        assert_eq!(check_host(&empty, at("2026-10-17T03:30:00Z"), None, Duration::from_secs(60)), Outcome::Silent);
    }

    #[test]
    fn a_host_that_cannot_be_asked_is_failed_with_the_reason() {
        let down = Script(Box::new(|_, _| Err(AppError::Config("ssh: connect to host h port 22: No route to host".into()))));
        let Outcome::Failed(why) = check_host(&down, at("2026-10-17T03:30:00Z"), None, Duration::from_secs(60)) else { panic!("expected a failure") };
        assert!(why.contains("No route"), "{why}");
        let ssh_255 = Script(Box::new(|_, _| Ok(CommandOutput { stdout: String::new(), stderr: "ssh: Connection timed out".into(), status: 255 })));
        assert!(matches!(check_host(&ssh_255, at("2026-10-17T03:30:00Z"), None, Duration::from_secs(60)), Outcome::Failed(_)));
    }

    #[test]
    fn problems_groups_a_hosts_disks_in_one_entry_and_skips_healthy_hosts() {
        use crate::smart::Problem;
        let disk = |dev: &str, problems: &[(Severity, &str)]| Disk { dev: dev.into(), problems: problems.iter().map(|(s, t)| Problem { severity: *s, text: (*t).into() }).collect() };
        let mut file = SmartFile::default();
        let entry = |disks| HostResult { checked_at: at("2026-10-17T03:30:00Z"), disks, error: None };
        file.hosts.insert("a".into(), entry(vec![disk("ada0", &[(Severity::Watch, "reallocated sectors 8"), (Severity::Watch, "temperature 63 C")]), disk("ada1", &[]), disk("ada2", &[(Severity::Issue, "SMART health FAILED")])]));
        file.hosts.insert("ok".into(), entry(vec![disk("ada0", &[])]));
        let found = problems(&file);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].host.as_str(), found[0].severity), ("a", Severity::Issue));
        assert_eq!(found[0].disks, "ada0 reallocated sectors 8, temperature 63 C; ada2 SMART health FAILED");
        assert_eq!(found[0].checked_at, at("2026-10-17T03:30:00Z"));
    }

    #[test]
    fn a_disk_that_had_a_problem_and_now_answers_nothing_becomes_unreadable_instead_of_vanishing() {
        use crate::smart::Problem;
        let previous = HostResult { checked_at: at("2026-10-16T03:30:00Z"), disks: vec![Disk { dev: "ada0".into(), problems: vec![Problem { severity: Severity::Watch, text: "pending sectors 8".into() }] }, Disk { dev: "gone".into(), problems: vec![Problem { severity: Severity::Watch, text: "x".into() }] }], error: None };
        // today ada0 (scanned) answers with no SMART block at all, and nvme0 is healthy; `gone` is no longer scanned
        let result = checked(check_host(&host(r#"{"smartctl":{"exit_status":2}}"#, HEALTHY), at("2026-10-17T03:30:00Z"), Some(&previous), Duration::from_secs(60)));
        let ada0 = result.disks.iter().find(|d| d.dev == "ada0").expect("ada0 must not vanish");
        assert_eq!(ada0.problems[0].severity, Severity::Watch);
        assert!(ada0.problems[0].text.starts_with("SMART unreadable"), "{:?}", ada0.problems);
        assert!(result.disks.iter().all(|d| d.dev != "gone"), "a disk that is gone from the scan is dropped");
    }

    #[test]
    fn a_slow_host_keeps_the_disks_it_did_answer_and_marks_the_rest_unreadable() {
        let slow = Script(Box::new(|_cmd, args| {
            if args[0] == "--scan" {
                return out(SCAN, 0);
            }
            if args[4] == "/dev/nvme0" {
                std::thread::sleep(Duration::from_millis(300));
            }
            out(if args[4] == "/dev/ada0" { FAILING } else { HEALTHY }, 0)
        }));
        // The budget is spent after the first disk: the second is not asked.
        let result = checked(check_host(&slow, at("2026-10-17T03:30:00Z"), None, Duration::from_millis(0)));
        assert_eq!(result.disks.len(), 2, "{result:?}");
        assert_eq!(result.disks[0].problems[0].text, "SMART health FAILED", "the disk that was answered keeps its verdict");
        assert!(result.disks[1].problems[0].text.contains("not checked in time"), "{:?}", result.disks[1]);
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
