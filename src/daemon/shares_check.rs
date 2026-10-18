//! The shares check: whether the NFS server and Samba run on a host that has shares for them. Part of the daily check
//! (and of a check on demand); the answer is kept in `shares.json` for the Dashboard and the daily report. Reads only.

use crate::exec::CommandRunner;
use crate::smart::Severity;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SharesResult {
    pub checked_at: DateTime<Utc>,
    pub nfs_running: bool,
    pub samba_running: bool,
    pub nfs_shares: usize,
    pub smb_shares: usize,
    /// The managed block of `smb4.conf` could not be read while Samba runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smb_error: Option<String>,
    /// Why the latest check did not happen; the rest is then from the last one that did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SharesFile {
    #[serde(default)]
    pub hosts: BTreeMap<String, SharesResult>,
    /// When the last daily pass ran, even if no host had anything to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_at: Option<DateTime<Utc>>,
}

impl SharesFile {
    /// The file at `path`, or an empty one when it is missing, torn or of another shape.
    pub fn load(path: &Path) -> SharesFile {
        std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    /// Writes through a temporary file and a rename, so a crash leaves the old file or the new one.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    }

    /// When any host last gave an answer.
    pub fn newest_check(&self) -> Option<DateTime<Utc>> {
        self.hosts.values().map(|h| h.checked_at).max()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The host is not set up for this check (sudo says no, no `service`): nothing to show.
    Silent,
    Checked(SharesResult),
    /// The host could not be asked this time.
    Failed(String),
}

/// What one `service <name> onestatus` said.
enum Service {
    Running,
    Stopped,
    /// sudo refused, or there is no `service` command: this host is not set up for the check.
    NotSetUp,
    Unreachable(String),
}

/// sudo's own refusals and a missing command, as opposed to the service saying it is not running.
fn is_refusal(stderr: &str) -> bool {
    stderr.trim_start().starts_with("sudo:") || stderr.contains("not found")
}

fn first_line(text: &str, fallback: &str) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(fallback).to_string()
}

/// Asks the service and tells a stopped service from a host that refuses or cannot be reached: the Shares section's own
/// availability calls cannot (they read every non-zero exit as "not running"), and a check that did would turn every host
/// without the sudoers entry red.
fn service(runner: &dyn CommandRunner, name: &str, running: impl Fn(&crate::exec::CommandOutput) -> bool) -> Result<Service, String> {
    let out = runner.run("service", &[name, "onestatus"]).map_err(|e| e.to_string())?;
    Ok(if out.status == 255 {
        Service::Unreachable(first_line(&out.stderr, "could not reach the host"))
    } else if running(&out) {
        Service::Running
    } else if is_refusal(&out.stderr) {
        Service::NotSetUp
    } else {
        Service::Stopped
    })
}

/// Whether a failed listing is sudo saying no (silent) or something else (failed).
fn listing_failure(error: crate::error::AppError) -> Outcome {
    match &error {
        crate::error::AppError::CommandFailed { stderr, .. } if is_refusal(stderr) => Outcome::Silent,
        other => Outcome::Failed(other.to_string()),
    }
}

/// Asks one host whether its NFS server and Samba run and how many shares need them. Read-only.
pub fn check_host(runner: &dyn CommandRunner, now: DateTime<Utc>) -> Outcome {
    let nfs = match service(runner, "nfsd", crate::exec::CommandOutput::success) {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e),
    };
    // Samba's rc script reports the combined state of smbd, nmbd and winbindd, so only the text about smbd counts.
    let samba = match service(runner, "samba_server", |out| out.stdout.contains("smbd is running")) {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e),
    };
    for state in [&nfs, &samba] {
        match state {
            Service::Unreachable(why) => return Outcome::Failed(why.clone()),
            Service::NotSetUp => return Outcome::Silent,
            Service::Running | Service::Stopped => {}
        }
    }
    let nfs_shares = match crate::zfs::share::list_shares(runner) {
        Ok(shares) => shares.len(),
        Err(e) => return listing_failure(e),
    };
    let samba_running = matches!(samba, Service::Running);
    // smb4.conf is read even when Samba is stopped, to know whether shares are waiting for it. A host without Samba has
    // no block and no file, which reads as no shares; a block that cannot be read is only news while Samba runs.
    let (smb_shares, smb_error) = match crate::zfs::smb::list_managed(runner) {
        Ok(shares) => (shares.len(), None),
        Err(e) if samba_running => (0, Some(e.to_string())),
        Err(_) => (0, None),
    };
    Outcome::Checked(SharesResult { checked_at: now, nfs_running: matches!(nfs, Service::Running), samba_running, nfs_shares, smb_shares, smb_error, error: None })
}

/// One host's share problems in a form both the Dashboard and the daily report can print.
#[derive(Debug, Clone, PartialEq)]
pub struct ShareProblem {
    pub host: String,
    /// Red when a service that has shares is stopped, yellow for a managed block that cannot be read.
    pub severity: Severity,
    /// `NFS: 2 shares but nfsd is not running; Samba: 1 share but smbd is not running`
    pub text: String,
    pub checked_at: DateTime<Utc>,
}

pub fn problems(file: &SharesFile) -> Vec<ShareProblem> {
    let shares = |n: usize| if n == 1 { "1 share".to_string() } else { format!("{n} shares") };
    file.hosts
        .iter()
        .filter_map(|(host, r)| {
            let mut texts = Vec::new();
            let mut severity = Severity::Watch;
            if r.nfs_shares > 0 && !r.nfs_running {
                texts.push(format!("NFS: {} but nfsd is not running", shares(r.nfs_shares)));
                severity = Severity::Issue;
            }
            if r.smb_shares > 0 && !r.samba_running {
                texts.push(format!("Samba: {} but smbd is not running", shares(r.smb_shares)));
                severity = Severity::Issue;
            }
            if let Some(why) = &r.smb_error {
                texts.push(format!("Samba: {why}"));
            }
            (!texts.is_empty()).then(|| ShareProblem { host: host.clone(), severity, text: texts.join("; "), checked_at: r.checked_at })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AppError, Result};
    use crate::exec::CommandOutput;

    const MANAGED_CONF: &str = "[global]\n  x = y\n\n## zfsctl managed shares -- do not edit by hand -- start\n[media]\n    # zfsctl-dataset = tank/media\n    path = /tank/media\n    valid users = carol\n    browseable = yes\n    read only = no\n## zfsctl managed shares -- do not edit by hand -- end\n";
    const TWO_NFS: &str = "tank/data\tsharenfs\ton\tlocal\ntank/more\tsharenfs\t-network 10.0.0.0/8\tlocal\ntank/none\tsharenfs\toff\tdefault\n";

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

    fn out(stdout: &str, stderr: &str, status: i32) -> Result<CommandOutput> {
        Ok(CommandOutput { stdout: stdout.to_string(), stderr: stderr.to_string(), status })
    }

    /// A host: `nfsd` answers `nfs` (stdout, status), `samba_server` answers `samba`, `zfs get` lists `nfs_list`,
    /// and `cat smb4.conf` gives `conf`.
    fn host(nfs: (&'static str, i32), samba: (&'static str, i32), nfs_list: &'static str, conf: &'static str) -> Script {
        Script(Box::new(move |cmd, args| match (cmd, args[0]) {
            ("service", "nfsd") => out(nfs.0, "", nfs.1),
            ("service", "samba_server") => out(samba.0, "", samba.1),
            ("zfs", "get") => out(nfs_list, "", 0),
            ("cat", _) => out(conf, "", 0),
            other => panic!("unexpected {other:?}"),
        }))
    }

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn checked(outcome: Outcome) -> SharesResult {
        match outcome {
            Outcome::Checked(r) => r,
            other => panic!("expected a result, got {other:?}"),
        }
    }

    const NOW: &str = "2026-10-17T03:30:00Z";
    const UP: (&str, i32) = ("is running as pid 1.", 0);
    const SMBD_UP: (&str, i32) = ("smbd is running as pid 2.", 0);
    const DOWN: (&str, i32) = ("is not running.", 1);

    #[test]
    fn running_servers_with_shares_are_healthy_and_stored() {
        let result = checked(check_host(&host(UP, SMBD_UP, TWO_NFS, MANAGED_CONF), at(NOW)));
        assert_eq!((result.nfs_running, result.samba_running, result.nfs_shares, result.smb_shares), (true, true, 2, 1));
        assert_eq!((result.smb_error, result.error, result.checked_at), (None, None, at(NOW)));
    }

    #[test]
    fn a_stopped_nfsd_with_shares_is_red_and_the_text_counts_the_shares() {
        let result = checked(check_host(&host(DOWN, SMBD_UP, TWO_NFS, MANAGED_CONF), at(NOW)));
        let mut file = SharesFile::default();
        file.hosts.insert("h".into(), result);
        let found = problems(&file);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].severity, found[0].text.as_str()), (Severity::Issue, "NFS: 2 shares but nfsd is not running"));
    }

    #[test]
    fn a_stopped_samba_with_a_share_is_red_and_one_share_is_singular() {
        // Samba stopped: the managed block is still counted, so "1 share but smbd is not running".
        let result = checked(check_host(&host(UP, DOWN, "", MANAGED_CONF), at(NOW)));
        assert_eq!((result.samba_running, result.smb_shares), (false, 1));
        let mut file = SharesFile::default();
        file.hosts.insert("h".into(), result);
        assert_eq!(problems(&file)[0].text, "Samba: 1 share but smbd is not running");
    }

    #[test]
    fn both_stopped_gives_one_entry_with_both_texts_joined() {
        let mut file = SharesFile::default();
        file.hosts.insert("h".into(), checked(check_host(&host(DOWN, DOWN, TWO_NFS, MANAGED_CONF), at(NOW))));
        let found = problems(&file);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "NFS: 2 shares but nfsd is not running; Samba: 1 share but smbd is not running");
    }

    #[test]
    fn stopped_services_without_shares_are_healthy() {
        let result = checked(check_host(&host(DOWN, DOWN, "tank/none\tsharenfs\toff\tdefault\n", "[global]\n"), at(NOW)));
        let mut file = SharesFile::default();
        file.hosts.insert("h".into(), result);
        assert!(problems(&file).is_empty());
    }

    #[test]
    fn an_unreadable_managed_block_while_samba_runs_is_yellow() {
        let broken = "## zfsctl managed shares -- do not edit by hand -- start\n[media]\n";
        let result = checked(check_host(&host(UP, SMBD_UP, "", broken), at(NOW)));
        assert!(result.smb_error.is_some(), "{result:?}");
        let mut file = SharesFile::default();
        file.hosts.insert("h".into(), result);
        let found = problems(&file);
        assert_eq!(found[0].severity, Severity::Watch);
        assert!(found[0].text.starts_with("Samba: "), "{}", found[0].text);
    }

    #[test]
    fn a_refusal_by_sudo_or_a_missing_service_command_is_silent_not_stopped() {
        for stderr in ["sudo: a password is required", "sudo: a terminal is required to read the password", "sudo: service: command not found"] {
            let denied = Script(Box::new(move |_, _| out("", stderr, 1)));
            assert_eq!(check_host(&denied, at(NOW)), Outcome::Silent, "{stderr}");
        }
    }

    #[test]
    fn a_listing_that_sudo_refuses_is_silent_and_other_failures_are_failed() {
        let denied_zfs = Script(Box::new(|cmd, _| if cmd == "zfs" { out("", "sudo: a password is required", 1) } else { out(UP.0, "", 0) }));
        assert_eq!(check_host(&denied_zfs, at(NOW)), Outcome::Silent);
        let broken_zfs = Script(Box::new(|cmd, _| if cmd == "zfs" { out("", "cannot open 'x': dataset does not exist", 1) } else { out(UP.0, "", 0) }));
        assert!(matches!(check_host(&broken_zfs, at(NOW)), Outcome::Failed(_)));
    }

    #[test]
    fn exit_255_and_a_runner_error_are_failed() {
        let ssh = Script(Box::new(|_, _| out("", "ssh: connect to host h port 22: Connection timed out", 255)));
        let Outcome::Failed(why) = check_host(&ssh, at(NOW)) else { panic!("expected a failure") };
        assert!(why.contains("timed out"), "{why}");
        let broken = Script(Box::new(|_, _| Err(AppError::Config("no route".into()))));
        assert!(matches!(check_host(&broken, at(NOW)), Outcome::Failed(_)));
    }

    #[test]
    fn save_is_atomic_and_load_survives_a_missing_torn_or_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shares.json");
        assert_eq!(SharesFile::load(&path), SharesFile::default());
        let mut file = SharesFile::default();
        file.hosts.insert("h".into(), checked(check_host(&host(UP, SMBD_UP, TWO_NFS, MANAGED_CONF), at(NOW))));
        file.save(&path).unwrap();
        assert_eq!(SharesFile::load(&path), file);
        assert!(!dir.path().join("shares.json.tmp").exists());
        for torn in ["", "{\"hosts\":{\"h\":{\"checked", "[1,2]", "null"] {
            std::fs::write(&path, torn).unwrap();
            assert_eq!(SharesFile::load(&path), SharesFile::default(), "{torn:?}");
        }
    }
}
