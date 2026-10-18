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
    /// The services' own names on a host that is not FreeBSD (`nfs-server`, `smb/server`); unset means `nfsd` and Samba's `smbd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nfs_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smb_name: Option<String>,
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
        super::smart_check::write_atomic(path, &serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)
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

/// The same check for a Linux or illumos host: shares counted from `sharenfs` (and, on illumos, the native `sharesmb`),
/// services from systemd or SMF. Samba shares on Linux live in `smb.conf` and are not counted yet. Read-only.
fn check_other_os(runner: &dyn CommandRunner, now: DateTime<Utc>, os: crate::zfs::host_health::HostOs) -> Outcome {
    use crate::zfs::host_health::HostOs;
    use crate::zfs::share::{nfs_name, nfs_unit, parse_sharesmb, service_state, smb_name, smb_unit, ServiceState};
    let nfs_shares = match crate::zfs::share::list_shares(runner) {
        Ok(shares) => shares.len(),
        Err(e) => return listing_failure(e),
    };
    let smb_shares = if os == HostOs::Illumos {
        match super::super::zfs::run_checked(runner, "zfs", &["get", "-H", "-p", "-t", "filesystem", "sharesmb"], "zfs get sharesmb") {
            Ok(out) => parse_sharesmb(&out.stdout).len(),
            Err(e) => return listing_failure(e),
        }
    } else {
        0
    };
    let (nfs, smb) = match (service_state(runner, os, nfs_unit(os)), service_state(runner, os, smb_unit(os))) {
        (Ok(nfs), Ok(smb)) => (nfs, smb),
        (Err(e), _) | (_, Err(e)) => return Outcome::Failed(e.to_string()),
    };
    Outcome::Checked(SharesResult {
        checked_at: now,
        // A state that could not be read makes no claim: it is not a server that is down.
        nfs_running: matches!(nfs, ServiceState::Running | ServiceState::Unknown),
        samba_running: matches!(smb, ServiceState::Running | ServiceState::Unknown),
        nfs_shares,
        smb_shares,
        smb_error: None,
        error: None,
        nfs_name: Some(nfs_name(os).to_string()),
        smb_name: Some(smb_name(os).to_string()),
    })
}

/// Asks one host whether its NFS server and Samba run and how many shares need them. Read-only.
pub fn check_host(runner: &dyn CommandRunner, now: DateTime<Utc>) -> Outcome {
    // `service ... onestatus` is FreeBSD's: Linux and illumos hosts are asked with their own tools; a host whose OS cannot be
    // read is asked as FreeBSD.
    match runner.run_unprivileged("uname", &["-s"]) {
        Ok(out) if out.status == 255 => return Outcome::Failed(first_line(&out.stderr, "could not reach the host")),
        Ok(out) if out.success() => {
            let os = crate::zfs::host_health::host_os_from_uname(&out.stdout);
            if matches!(os, crate::zfs::host_health::HostOs::Linux | crate::zfs::host_health::HostOs::Illumos) {
                return check_other_os(runner, now, os);
            }
        }
        Err(e) => return Outcome::Failed(e.to_string()),
        _ => {}
    }
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
        // This host may not be set up to read the file (the `samba` sudoers feature): no news, like any silent check. A
        // dropped connection is a failure, not a verdict.
        Err(crate::error::AppError::CommandFailed { status: 255, stderr, .. }) => return Outcome::Failed(first_line(&stderr, "could not reach the host")),
        Err(crate::error::AppError::CommandFailed { stderr, .. }) if is_refusal(&stderr) => (0, None),
        Err(e) if samba_running => (0, Some(e.to_string())),
        Err(_) => (0, None),
    };
    Outcome::Checked(SharesResult { checked_at: now, nfs_running: matches!(nfs, Service::Running), samba_running, nfs_shares, smb_shares, smb_error, error: None, nfs_name: None, smb_name: None })
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
                texts.push(format!("NFS: {} but {} is not running", shares(r.nfs_shares), r.nfs_name.as_deref().unwrap_or("nfsd")));
                severity = Severity::Issue;
            }
            if r.smb_shares > 0 && !r.samba_running {
                let name = r.smb_name.as_deref().unwrap_or("smbd");
                // Samba's own service says Samba; illumos's kernel service is plain SMB.
                texts.push(format!("{}: {} but {name} is not running", if name == "smbd" { "Samba" } else { "SMB" }, shares(r.smb_shares)));
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
            ("uname", _) => out("FreeBSD\n", "", 0),
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
    fn a_samba_block_that_sudo_refuses_is_silent_and_a_dropped_ssh_on_the_read_is_failed() {
        // A host with the `shares` feature (service, zfs) but not `samba` (cat smb4.conf) that runs its own Samba.
        let no_cat = Script(Box::new(|cmd, args| match (cmd, args[0]) {
            ("uname", _) => out("FreeBSD\n", "", 0),
            ("service", _) => out("smbd is running as pid 2.", "", 0),
            ("zfs", _) => out(TWO_NFS, "", 0),
            ("cat", _) => out("", "sudo: a password is required", 1),
            other => panic!("{other:?}"),
        }));
        let result = checked(check_host(&no_cat, at(NOW)));
        assert_eq!((result.smb_error, result.smb_shares), (None, 0), "not set up for reading smb4.conf: no news");
        let dropped = Script(Box::new(|cmd, args| match (cmd, args[0]) {
            ("uname", _) => out("FreeBSD\n", "", 0),
            ("service", _) => out("smbd is running as pid 2.", "", 0),
            ("zfs", _) => out(TWO_NFS, "", 0),
            ("cat", _) => out("", "ssh: Connection reset by peer", 255),
            other => panic!("{other:?}"),
        }));
        assert!(matches!(check_host(&dropped, at(NOW)), Outcome::Failed(_)));
    }

    /// A Linux or illumos host: `uname`, the share listings (sudo), the service states (no sudo).
    fn other_os(uname: &'static str, nfs_list: &'static str, sharesmb: &'static str, nfs_state: &'static str, smb_state: &'static str) -> Script {
        Script(Box::new(move |cmd, args| match (cmd, args[0]) {
            ("uname", _) => out(uname, "", 0),
            ("zfs", "get") => out(if args.contains(&"sharesmb") { sharesmb } else { nfs_list }, "", 0),
            ("systemctl", "show") => out(if args.contains(&"nfs-server") { nfs_state } else { smb_state }, "", 0),
            ("svcs", "-H") => out(if args.iter().any(|a| a.contains("nfs/server")) { nfs_state } else { smb_state }, "", 0),
            other => panic!("a {uname:?} host was asked {other:?}"),
        }))
    }

    const ONE_NFS: &str = "tank/data\tsharenfs\trw=@10.0.0.0/24\tlocal\n";
    const TWO_SMB: &str = "tank/alice\tsharesmb\tname=alice\tlocal\ntank/carol\tsharesmb\tname=carol\tlocal\n";
    const LOADED_ACTIVE: &str = "LoadState=loaded\nActiveState=active\n";
    const LOADED_INACTIVE: &str = "LoadState=loaded\nActiveState=inactive\n";

    #[test]
    fn a_linux_host_with_nfs_shares_and_a_stopped_server_is_red_with_its_own_service_name() {
        let result = checked(check_host(&other_os("Linux\n", ONE_NFS, TWO_SMB, LOADED_INACTIVE, LOADED_ACTIVE), at(NOW)));
        assert_eq!((result.nfs_running, result.nfs_shares, result.smb_shares), (false, 1, 0), "Samba shares are not counted on Linux yet");
        let mut file = SharesFile::default();
        file.hosts.insert("ub24".into(), result);
        assert_eq!(problems(&file)[0].text, "NFS: 1 share but nfs-server is not running");
        let healthy = checked(check_host(&other_os("Linux\n", ONE_NFS, "", LOADED_ACTIVE, LOADED_INACTIVE), at(NOW)));
        file.hosts.insert("ub24".into(), healthy);
        assert!(problems(&file).is_empty());
        let not_installed = checked(check_host(&other_os("Linux\n", ONE_NFS, "", "LoadState=not-found\nActiveState=inactive\n", LOADED_INACTIVE), at(NOW)));
        file.hosts.insert("ub24".into(), not_installed);
        assert_eq!(problems(&file).len(), 1, "shares set on a host without an NFS server do not work");
    }

    #[test]
    fn an_illumos_host_counts_sharesmb_and_names_the_smf_services() {
        let healthy = checked(check_host(&other_os("SunOS\n", ONE_NFS, TWO_SMB, "online\n", "online\n"), at(NOW)));
        assert_eq!((healthy.nfs_shares, healthy.smb_shares, healthy.nfs_running, healthy.samba_running), (1, 2, true, true));
        let down = checked(check_host(&other_os("SunOS\n", ONE_NFS, TWO_SMB, "online\n", "disabled\n"), at(NOW)));
        let mut file = SharesFile::default();
        file.hosts.insert("omnios".into(), down);
        assert_eq!(problems(&file)[0].text, "SMB: 2 shares but smb/server is not running");
    }

    #[test]
    fn a_service_that_cannot_be_read_is_no_claim_and_never_red() {
        // No systemd and no pgrep: shares exist, but nothing says the server is down.
        let blind = Script(Box::new(|cmd, args| match (cmd, args[0]) {
            ("uname", _) => out("Linux\n", "", 0),
            ("zfs", _) => out(ONE_NFS, "", 0),
            ("systemctl", _) => out("", "System has not been booted with systemd", 1),
            ("pgrep", _) => out("", "sh: pgrep: not found", 127),
            other => panic!("{other:?}"),
        }));
        let result = checked(check_host(&blind, at(NOW)));
        assert!(result.nfs_running && result.samba_running, "unknown is not 'not running'");
        let mut file = SharesFile::default();
        file.hosts.insert("alpine".into(), result);
        assert!(problems(&file).is_empty());
    }

    #[test]
    fn on_linux_and_illumos_sudo_refusing_zfs_is_silent_and_a_dropped_ssh_on_a_service_is_failed() {
        let denied = Script(Box::new(|cmd, _| if cmd == "uname" { out("Linux\n", "", 0) } else { out("", "sudo: a password is required", 1) }));
        assert_eq!(check_host(&denied, at(NOW)), Outcome::Silent);
        let dropped = Script(Box::new(|cmd, args| match (cmd, args[0]) {
            ("uname", _) => out("Linux\n", "", 0),
            ("zfs", _) => out(ONE_NFS, "", 0),
            _ => out("", "ssh: Connection reset by peer", 255),
        }));
        assert!(matches!(check_host(&dropped, at(NOW)), Outcome::Failed(_)));
    }

    #[test]
    fn a_freebsd_host_and_one_with_an_odd_uname_are_still_checked() {
        for uname in ["FreeBSD\n", "Darwin\n", ""] {
            let runner = Script(Box::new(move |cmd, args| match (cmd, args[0]) {
                ("uname", _) => out(uname, "", 0),
                ("service", "nfsd") => out(UP.0, "", UP.1),
                ("service", "samba_server") => out(SMBD_UP.0, "", SMBD_UP.1),
                ("zfs", "get") => out(TWO_NFS, "", 0),
                ("cat", _) => out(MANAGED_CONF, "", 0),
                other => panic!("{other:?}"),
            }));
            assert!(matches!(check_host(&runner, at(NOW)), Outcome::Checked(_)), "{uname:?}");
        }
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
