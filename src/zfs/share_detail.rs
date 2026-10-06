//! Who is using one highlighted share.
//!
//! Client lists are best-effort and `None` when the host won't say:
//! `smbstatus` only works as root (so over SSH it needs a sudoers entry the
//! managed hosts don't have by default) and `showmount -a` needs a running
//! `mountd`. Not knowing is shown as `unavailable`, never as zero clients.

use crate::error::Result;
use crate::exec::{CommandOutput, CommandRunner};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShareLive {
    pub mountpoint: Option<String>,
    /// Distinct connected client machines; `None` = could not be determined.
    pub clients: Option<Vec<String>>,
}

fn push_unique(list: &mut Vec<String>, item: &str) {
    if !list.iter().any(|c| c == item) {
        list.push(item.to_string());
    }
}

/// Machines connected to SMB share `share`, from `smbstatus -S`: rows after
/// the dashed rule are `service pid machine connected-at ...`.
pub fn parse_smb_clients(output: &str, share: &str) -> Vec<String> {
    let mut clients = Vec::new();
    let mut past_rule = false;
    for line in output.lines() {
        if line.starts_with("---") {
            past_rule = true;
            continue;
        }
        if !past_rule {
            continue;
        }
        if let [service, _pid, machine, ..] = line.split_whitespace().collect::<Vec<_>>().as_slice() {
            if service.eq_ignore_ascii_case(share) {
                push_unique(&mut clients, machine);
            }
        }
    }
    clients
}

/// Clients that mounted `mountpoint` (or something under it), from
/// `showmount -a`: a header line, then `client:/path` lines.
pub fn parse_nfs_clients(output: &str, mountpoint: &str) -> Vec<String> {
    let mut clients = Vec::new();
    for line in output.lines().skip(1) {
        let Some((client, rest)) = line.trim().split_once(":/") else { continue };
        let path = format!("/{rest}");
        if path == mountpoint || path.strip_prefix(mountpoint).is_some_and(|r| r.starts_with('/') || mountpoint.ends_with('/')) {
            push_unique(&mut clients, client);
        }
    }
    clients
}

fn ok_stdout(output: Result<CommandOutput>) -> Option<String> {
    output.ok().filter(CommandOutput::success).map(|o| o.stdout)
}

/// One refresh for the share on `dataset`; `smb_name` is `Some` for an SMB
/// share and `None` for NFS.
pub fn sample_share(runner: &dyn CommandRunner, dataset: &str, smb_name: Option<&str>) -> ShareLive {
    let mountpoint = ok_stdout(runner.run("zfs", &["get", "-H", "-o", "value", "mountpoint", "--", dataset]))
        .map(|s| s.trim().to_string())
        .filter(|m| m.starts_with('/'));
    let clients = match smb_name {
        Some(name) => ok_stdout(runner.run("smbstatus", &["-S"])).map(|out| parse_smb_clients(&out, name)),
        None => mountpoint
            .as_deref()
            .and_then(|mp| ok_stdout(runner.run_unprivileged("showmount", &["-a"])).map(|out| parse_nfs_clients(&out, mp))),
    };
    ShareLive { mountpoint, clients }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    // The header and the IPC$ row are real `smbstatus -S` output from the hub.
    const SMB: &str = "\nService      pid     Machine       Connected at                     Encryption   Signing     \n---------------------------------------------------------------------------------------------\nIPC$         3507    100.100.100.10 Tue Oct  6 10:16:22 2026 EDT     -            -           \ncarol        3511    10.0.0.5       Tue Oct  6 10:17:00 2026 EDT     -            -           \ncarol        3512    10.0.0.5       Tue Oct  6 10:17:09 2026 EDT     -            -           \nalice        3520    10.0.0.9       Tue Oct  6 10:18:00 2026 EDT     -            -           \n";
    const NFS: &str = "All mount points on localhost:\n10.0.0.7:/tpool\n10.0.0.8:/tpool/sub\n10.0.0.7:/tpool/sub\n10.0.0.9:/tpoolx\nfe80::1:/tpool\n";

    #[test]
    fn smb_clients_are_the_distinct_machines_on_that_share_only() {
        assert_eq!(parse_smb_clients(SMB, "carol"), vec!["10.0.0.5"]);
        assert_eq!(parse_smb_clients(SMB, "CAROL"), vec!["10.0.0.5"], "Samba share names are case-insensitive");
        assert!(parse_smb_clients(SMB, "none").is_empty());
        assert!(parse_smb_clients("carol 1 10.0.0.1\n", "carol").is_empty(), "rows only count after the rule");
    }

    #[test]
    fn nfs_clients_match_the_mountpoint_and_its_subdirectories_not_lookalikes() {
        assert_eq!(parse_nfs_clients(NFS, "/tpool"), vec!["10.0.0.7", "10.0.0.8", "fe80::1"]);
        assert!(parse_nfs_clients(NFS, "/other").is_empty());
        assert!(parse_nfs_clients("All mount points on localhost:\n", "/tpool").is_empty());
    }

    #[test]
    fn nfs_asks_showmount_unprivileged_and_smb_asks_smbstatus() {
        let runner = FakeRunner::new("").route("zfs", "get", "/tpool\n").route("showmount", "-a", NFS);
        let live = sample_share(&runner, "tpool", None);
        assert_eq!((live.mountpoint.as_deref(), live.clients.map(|c| c.len())), (Some("/tpool"), Some(3)));
        assert_eq!(runner.unprivileged_calls.lock().unwrap().len(), 1);

        let runner = FakeRunner::new("").route("zfs", "get", "/data/carol\n").route("smbstatus", "-S", SMB);
        let live = sample_share(&runner, "tank/carol", Some("carol"));
        assert_eq!(live.clients, Some(vec!["10.0.0.5".to_string()]));
        assert!(runner.unprivileged_calls.lock().unwrap().is_empty(), "smbstatus needs root, so it runs under sudo");
    }

    #[test]
    fn a_host_that_will_not_say_is_unavailable_not_zero_clients() {
        let runner = FakeRunner::new("").route("zfs", "get", "/tpool\n").route_failure("showmount", "-a", "RPC: Program not registered", 1);
        assert_eq!(sample_share(&runner, "tpool", None).clients, None);
        let runner = FakeRunner::new("").route_failure("smbstatus", "-S", "sudo: a password is required", 1);
        assert_eq!(sample_share(&runner, "tank/x", Some("x")).clients, None);
        let legacy = FakeRunner::new("").route("zfs", "get", "legacy\n");
        let live = sample_share(&legacy, "tank/y", None);
        assert_eq!((live.mountpoint, live.clients), (None, None), "no mountpoint, so NFS clients cannot be matched");
    }
}
