use crate::error::Result;
use crate::exec::CommandRunner;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Share {
    Nfs {
        dataset: String,
        raw: String,
        networks: Vec<String>,
        hosts: Vec<String>,
        read_only: bool,
        root: Option<String>,
    },
    Smb {
        dataset: String,
        name: Option<String>,
    },
}

/// The result of one Shares refresh: every active share on the host,
/// plus whether NFS/Samba are actually available to create new ones.
pub struct SharesSnapshot {
    pub shares: Vec<Share>,
    pub nfs_available: bool,
    pub samba_available: bool,
}

/// Splits a `rw=`/`ro=` clause's list into networks (`@`-prefixed) and
/// hosts (everything else), colon-separated.
fn parse_host_list(list: &str, networks: &mut Vec<String>, hosts: &mut Vec<String>) {
    for item in list.split(':') {
        if let Some(net) = item.strip_prefix('@') {
            networks.push(net.to_string());
        } else if !item.is_empty() {
            hosts.push(item.to_string());
        }
    }
}

/// Parses a `sharenfs` value's comma-separated clauses into
/// `(networks, hosts, read_only, root)`. Unrecognized clauses (e.g.
/// `sec=sys`, `crossmnt`) are silently ignored for these structured
/// fields — the caller always keeps the original string too (`raw`),
/// so nothing is lost for display, and the existing generic
/// property-edit form remains available as a raw-text escape hatch for
/// values using clauses this structured form doesn't support.
fn parse_sharenfs_fields(raw: &str) -> (Vec<String>, Vec<String>, bool, Option<String>) {
    let mut networks = Vec::new();
    let mut hosts = Vec::new();
    let mut read_only = false;
    let mut root = None;
    for clause in raw.split(',') {
        let clause = clause.trim();
        if clause == "rw" {
            read_only = false;
        } else if clause == "ro" {
            read_only = true;
        } else if let Some(list) = clause.strip_prefix("rw=") {
            read_only = false;
            parse_host_list(list, &mut networks, &mut hosts);
        } else if let Some(list) = clause.strip_prefix("ro=") {
            read_only = true;
            parse_host_list(list, &mut networks, &mut hosts);
        } else if let Some(user) = clause.strip_prefix("root=") {
            root = Some(user.to_string());
        }
        // else: unrecognized (e.g. "on", "sec=sys", "crossmnt") — ignored for structured fields.
    }
    (networks, hosts, read_only, root)
}

/// Assembles a `sharenfs` value from structured fields — the inverse
/// of `parse_sharenfs_fields`. Every argument empty/false/`None`
/// assembles to plain `"on"` (share to everyone, read-write, no
/// root-mapping restriction).
pub fn assemble_sharenfs(networks: &[String], hosts: &[String], read_only: bool, root: Option<&str>) -> String {
    if networks.is_empty() && hosts.is_empty() && root.is_none() && !read_only {
        return "on".to_string();
    }
    let verb = if read_only { "ro" } else { "rw" };
    let mut clauses = Vec::new();
    if networks.is_empty() && hosts.is_empty() {
        clauses.push(verb.to_string());
    } else {
        let mut list: Vec<String> = networks.iter().map(|n| format!("@{n}")).collect();
        list.extend(hosts.iter().cloned());
        clauses.push(format!("{verb}={}", list.join(":")));
    }
    if let Some(user) = root {
        clauses.push(format!("root={user}"));
    }
    clauses.join(",")
}

/// Groups `zfs get -H -p sharenfs,sharesmb` output (every dataset on
/// the host, no dataset argument) by dataset name and builds one
/// `Share` per protocol actually in use — a dataset shared via both
/// NFS and SMB produces two independent entries. Datasets where both
/// properties are `off` are dropped entirely. Lenient: a malformed
/// line is skipped rather than failing the whole list, since one odd
/// line for one dataset shouldn't take down the host's entire share
/// list — diverges deliberately from the stricter single-entity
/// parsers elsewhere in this crate (`parse_property_list` etc.), which
/// only ever describe one already-known-good pool/dataset at a time.
fn parse_shares(output: &str) -> Vec<Share> {
    let mut by_dataset: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 4 {
            continue;
        }
        let (dataset, property, value) = (fields[0], fields[1], fields[2]);
        let entry = by_dataset.entry(dataset.to_string()).or_insert((None, None));
        match property {
            "sharenfs" => entry.0 = Some(value.to_string()),
            "sharesmb" => entry.1 = Some(value.to_string()),
            _ => {}
        }
    }
    let mut shares = Vec::new();
    for (dataset, (sharenfs, sharesmb)) in by_dataset {
        if let Some(raw) = sharenfs.as_deref() {
            if raw != "off" {
                let (networks, hosts, read_only, root) = parse_sharenfs_fields(raw);
                shares.push(Share::Nfs { dataset: dataset.clone(), raw: raw.to_string(), networks, hosts, read_only, root });
            }
        }
        if let Some(name) = sharesmb.as_deref() {
            if name != "off" {
                let name = if name == "on" { None } else { Some(name.to_string()) };
                shares.push(Share::Smb { dataset: dataset.clone(), name });
            }
        }
    }
    shares
}

pub fn list_shares(runner: &dyn CommandRunner) -> Result<Vec<Share>> {
    let output = super::run_checked(runner, "zfs", &["get", "-H", "-p", "sharenfs,sharesmb"], "zfs get sharenfs,sharesmb")?;
    Ok(parse_shares(&output.stdout))
}

/// Runs `service nfsd onestatus` directly (not through `run_checked` —
/// a nonzero exit here is an expected, meaningful "not running"
/// result, not a command failure to propagate as an error). Exit code
/// alone is reliable, confirmed live.
pub fn nfs_server_available(runner: &dyn CommandRunner) -> Result<bool> {
    let output = runner.run("service", &["nfsd", "onestatus"])?;
    Ok(output.success())
}

/// Runs `service samba_server onestatus` directly, same rationale as
/// `nfs_server_available`. Checks the **output text** for `"smbd is
/// running"` rather than the exit code — confirmed live that this
/// rc.d meta-service's exit code reflects the combined status of
/// `smbd`, `nmbd`, and `winbindd` (the AD/domain-integration daemon,
/// irrelevant to basic file sharing), so a host with `winbindd`
/// stopped but `smbd` serving shares fine reports a nonzero exit code
/// from `onestatus` despite SMB sharing being fully functional.
pub fn samba_available(runner: &dyn CommandRunner) -> Result<bool> {
    let output = runner.run("service", &["samba_server", "onestatus"])?;
    Ok(output.stdout.contains("smbd is running"))
}

/// Composes all three calls into one snapshot — avoids extra
/// round-trips, mirrors `zfs::disk::list_disks_with_membership`.
pub fn list_shares_with_availability(runner: &dyn CommandRunner) -> Result<SharesSnapshot> {
    let shares = list_shares(runner)?;
    let nfs_available = nfs_server_available(runner)?;
    let samba_available = samba_available(runner)?;
    Ok(SharesSnapshot { shares, nfs_available, samba_available })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    #[test]
    fn parse_sharenfs_fields_bare_on_means_no_restriction() {
        assert_eq!(parse_sharenfs_fields("on"), (vec![], vec![], false, None));
    }

    #[test]
    fn parse_sharenfs_fields_rw_with_network_and_root() {
        assert_eq!(
            parse_sharenfs_fields("rw=@172.16.10.0/24,root=nobody"),
            (vec!["172.16.10.0/24".to_string()], vec![], false, Some("nobody".to_string()))
        );
    }

    #[test]
    fn parse_sharenfs_fields_ro_with_host() {
        assert_eq!(parse_sharenfs_fields("ro=somehost"), (vec![], vec!["somehost".to_string()], true, None));
    }

    #[test]
    fn parse_sharenfs_fields_ignores_unrecognized_clauses_but_keeps_recognized_ones() {
        assert_eq!(parse_sharenfs_fields("sec=sys,rw,crossmnt,no_subtree_check"), (vec![], vec![], false, None));
    }

    #[test]
    fn assemble_sharenfs_all_blank_produces_on() {
        assert_eq!(assemble_sharenfs(&[], &[], false, None), "on");
    }

    #[test]
    fn assemble_sharenfs_read_only_with_no_restriction_produces_bare_ro() {
        assert_eq!(assemble_sharenfs(&[], &[], true, None), "ro");
    }

    #[test]
    fn assemble_sharenfs_networks_hosts_and_root_combine_correctly() {
        assert_eq!(
            assemble_sharenfs(&["172.16.10.0/24".to_string()], &["somehost".to_string()], false, Some("nobody")),
            "rw=@172.16.10.0/24:somehost,root=nobody"
        );
    }

    #[test]
    fn parse_then_assemble_sharenfs_round_trips_a_canonical_value() {
        let raw = "rw=@172.16.10.0/24,root=nobody";
        let (networks, hosts, read_only, root) = parse_sharenfs_fields(raw);
        assert_eq!(assemble_sharenfs(&networks, &hosts, read_only, root.as_deref()), raw);
    }

    const SHARES_FIXTURE: &str = "\
tank/both\tsharenfs\ton\tlocal
tank/both\tsharesmb\ton\tlocal
tank/data\tsharenfs\toff\tdefault
tank/data\tsharesmb\toff\tdefault
tank/nfsonly\tsharenfs\trw=@172.16.10.0/24,root=nobody\tlocal
tank/nfsonly\tsharesmb\toff\tdefault
tank/smbonly\tsharenfs\toff\tdefault
tank/smbonly\tsharesmb\tmyshare\tlocal
tank/weird\tsharenfs\tsec=sys,rw,crossmnt,no_subtree_check\tlocal
tank/weird\tsharesmb\toff\tdefault
";

    #[test]
    fn parse_shares_groups_by_dataset_and_drops_fully_unshared_datasets() {
        let shares = parse_shares(SHARES_FIXTURE);
        assert_eq!(
            shares,
            vec![
                Share::Nfs { dataset: "tank/both".to_string(), raw: "on".to_string(), networks: vec![], hosts: vec![], read_only: false, root: None },
                Share::Smb { dataset: "tank/both".to_string(), name: None },
                Share::Nfs {
                    dataset: "tank/nfsonly".to_string(),
                    raw: "rw=@172.16.10.0/24,root=nobody".to_string(),
                    networks: vec!["172.16.10.0/24".to_string()],
                    hosts: vec![],
                    read_only: false,
                    root: Some("nobody".to_string()),
                },
                Share::Smb { dataset: "tank/smbonly".to_string(), name: Some("myshare".to_string()) },
                Share::Nfs {
                    dataset: "tank/weird".to_string(),
                    raw: "sec=sys,rw,crossmnt,no_subtree_check".to_string(),
                    networks: vec![],
                    hosts: vec![],
                    read_only: false,
                    root: None,
                },
            ],
            "tank/data (both off) must not appear; tank/weird's unrecognized clauses must not drop the share, raw must be preserved"
        );
    }

    #[test]
    fn list_shares_uses_correct_command_and_parses_result() {
        let runner = FakeRunner::new("tank/data\tsharenfs\ton\tlocal\ntank/data\tsharesmb\toff\tdefault\n");
        let shares = list_shares(&runner).unwrap();
        assert_eq!(shares, vec![Share::Nfs { dataset: "tank/data".to_string(), raw: "on".to_string(), networks: vec![], hosts: vec![], read_only: false, root: None }]);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["get".to_string(), "-H".to_string(), "-p".to_string(), "sharenfs,sharesmb".to_string()]));
    }

    #[test]
    fn list_shares_surfaces_command_failure() {
        let runner = FakeRunner::failing("permission denied", 1);
        assert!(list_shares(&runner).is_err());
    }

    #[test]
    fn nfs_server_available_true_when_exit_zero() {
        let runner = FakeRunner::new("nfsd is running as pid 1234.\n");
        assert_eq!(nfs_server_available(&runner).unwrap(), true);
    }

    #[test]
    fn nfs_server_available_false_when_exit_nonzero() {
        let runner = FakeRunner::new("nfsd is not running.\n").with_status(1);
        assert_eq!(nfs_server_available(&runner).unwrap(), false);
    }

    #[test]
    fn samba_available_true_when_smbd_running_even_if_exit_nonzero() {
        // The real, confirmed case: winbindd down drags onestatus's
        // exit code to nonzero even though smbd itself is fine.
        let runner = FakeRunner::new("nmbd is running as pid 1.\nsmbd is running as pid 2.\nwinbindd is not running.\n").with_status(1);
        assert_eq!(samba_available(&runner).unwrap(), true);
    }

    #[test]
    fn samba_available_false_when_smbd_itself_is_not_running() {
        let runner = FakeRunner::new("nmbd is not running.\nsmbd is not running.\nwinbindd is not running.\n").with_status(1);
        assert_eq!(samba_available(&runner).unwrap(), false);
    }

    #[test]
    fn list_shares_with_availability_composes_all_three_calls() {
        let runner = FakeRunner::new("")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\ntank/data\tsharesmb\toff\tdefault\n")
            .route("service", "nfsd", "nfsd is running as pid 1234.\n")
            .route("service", "samba_server", "smbd is running as pid 2.\n");
        let snapshot = list_shares_with_availability(&runner).unwrap();
        assert_eq!(snapshot.shares.len(), 1);
        assert_eq!(snapshot.nfs_available, true);
        assert_eq!(snapshot.samba_available, true);
    }

    #[test]
    fn list_shares_with_availability_surfaces_command_failure() {
        let runner = FakeRunner::failing("permission denied", 1);
        assert!(list_shares_with_availability(&runner).is_err());
    }
}
