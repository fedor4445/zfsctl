use crate::error::Result;
use crate::exec::CommandRunner;
use crate::zfs::host_health::HostOs;
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
        name: String,
        path: String,
        valid_user: Option<String>,
        read_only: bool,
        guest_ok: bool,
    },
}

/// The result of one Shares refresh: every active share on the host,
/// plus whether NFS/Samba are actually available to create new ones.
pub struct SharesSnapshot {
    pub shares: Vec<Share>,
    pub nfs_available: bool,
    pub samba_available: bool,
    /// Set when the managed SMB block could not be read; NFS shares are
    /// still returned so one broken half never hides the other.
    pub smb_error: Option<String>,
}

/// Parses a `sharenfs` value using FreeBSD's **native `exports(5)`**
/// syntax into `(networks, hosts, read_only, root)`.
///
/// **This is not `OpenZFS`'s own cross-platform `sharenfs` mini-language**
/// (`rw=@net,root=user`, comma-separated) — confirmed live against a
/// real FreeBSD 15.1 box that `mountd` does not understand that syntax
/// at all. `mountd`'s own log, verbatim: `"can't get address info for
/// host rw=@172.16.10.0/24"` / `"bad host rw=@172.16.10.0/24, skipping"`
/// / `"bad exports list line '... rw=@172.16.10.0/24': no valid
/// entries"` — it tries to resolve the whole clause as a hostname via
/// `getaddrinfo` and fails, discarding the entire line. FreeBSD's own
/// `exports(5)` man page confirms this: it documents only dash-prefixed
/// options (`-maproot=`, `-ro`, `-network`, etc.) and never mentions
/// `rw=`/`ro=`/`root=`. The `zfsprops(7)` text describing `sharenfs`'s
/// default options (`sec=sys,rw,crossmnt,no_subtree_check`) is Linux
/// `nfs-utils`/`exportfs(8)` terminology copied into the generic
/// `OpenZFS` docs — `crossmnt`/`no_subtree_check` aren't FreeBSD concepts
/// at all, and FreeBSD's `zfs` writes the property value into
/// `/etc/zfs/exports` essentially as-is, so whatever syntax you set
/// *is* the `exports(5)` line `mountd` has to parse.
///
/// Native syntax, confirmed live: `-ro` (read-only; absence means
/// read-write, the default), `-maproot=<user>` (root's remote
/// credential), `-network <cidr>` (one clause per network — combined
/// CIDR notation, e.g. `172.16.10.0/24`, confirmed working with no
/// separate `-mask` needed), and bare hostnames for an unrestricted
/// host list. Per `zfsprops(7)` (the semicolon mechanism itself *was*
/// independently verified live, unlike the Linux-ism options above):
/// semicolon-separated clauses each become their own `exports(5)` line,
/// letting the same options apply to multiple networks/host-sets.
///
/// Unrecognized flags (e.g. `-alldirs`, `-sec=krb5i`) are silently
/// ignored for these structured fields — the caller always keeps the
/// original string too (`raw`), so nothing is lost for display, and the
/// existing generic property-edit form remains available as a raw-text
/// escape hatch for values using flags this structured form doesn't
/// support.
fn parse_sharenfs_fields(raw: &str) -> (Vec<String>, Vec<String>, bool, Option<String>) {
    let mut networks = Vec::new();
    let mut hosts = Vec::new();
    let mut read_only = false;
    let mut root = None;
    for clause in raw.split(';') {
        let mut tokens = clause.split_whitespace();
        while let Some(token) = tokens.next() {
            if token == "-ro" {
                read_only = true;
            } else if token == "-rw" {
                read_only = false;
            } else if let Some(user) = token.strip_prefix("-maproot=") {
                root = Some(user.to_string());
            } else if token == "-network" {
                if let Some(network) = tokens.next() {
                    networks.push(network.to_string());
                }
            } else if let Some(network) = token.strip_prefix("-network=") {
                networks.push(network.to_string());
            } else if token.starts_with('-') {
                // unrecognized flag (e.g. "-alldirs", "-sec=krb5i", "-mask") — ignored for structured fields.
            } else if token == "on" {
                // bare "on" (the whole, unrestricted-share value) — not a hostname.
            } else {
                hosts.push(token.to_string());
            }
        }
    }
    (networks, hosts, read_only, root)
}

/// Assembles a `sharenfs` value from structured fields using FreeBSD's
/// native `exports(5)` syntax — the inverse of `parse_sharenfs_fields`
/// (see its doc comment for why this isn't `OpenZFS`'s cross-platform
/// mini-language). Every argument empty/false/`None` assembles to plain
/// `"on"` (share to everyone, read-write, no root-mapping restriction).
/// One clause per network, plus one clause for the host list if any
/// hosts are given, each carrying the same `-ro`/`-maproot=` options;
/// multiple clauses are joined with `;` (confirmed live: each becomes
/// its own `exports(5)` line). A single network with no hosts produces
/// one clause with no `;` at all.
pub fn assemble_sharenfs(networks: &[String], hosts: &[String], read_only: bool, root: Option<&str>) -> String {
    if networks.is_empty() && hosts.is_empty() && root.is_none() && !read_only {
        return "on".to_string();
    }
    let mut opts: Vec<String> = Vec::new();
    if read_only {
        opts.push("-ro".to_string());
    }
    if let Some(user) = root {
        opts.push(format!("-maproot={user}"));
    }
    let mut clauses: Vec<String> = Vec::new();
    if !hosts.is_empty() {
        let mut parts = opts.clone();
        parts.extend(hosts.iter().cloned());
        clauses.push(parts.join(" "));
    }
    for network in networks {
        let mut parts = opts.clone();
        parts.push("-network".to_string());
        parts.push(network.clone());
        clauses.push(parts.join(" "));
    }
    if clauses.is_empty() {
        opts.join(" ")
    } else {
        clauses.join(";")
    }
}

/// Groups `zfs get -H -p sharenfs` output (every dataset on the host,
/// no dataset argument) by dataset name and builds one `Share::Nfs` per
/// dataset actually shared. Datasets where `sharenfs` is `off` or `-`
/// are dropped. SMB shares are no longer derived from `zfs get` (they
/// come from the managed smb4.conf block). Lenient: a malformed line is
/// skipped rather than failing the whole list, since one odd line for
/// one dataset shouldn't take down the host's entire share list.
fn parse_shares(output: &str) -> Vec<Share> {
    let mut by_dataset: BTreeMap<String, String> = BTreeMap::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let &[dataset, property, value, _] = fields.as_slice() else {
            continue;
        };
        if property == "sharenfs" {
            by_dataset.insert(dataset.to_string(), value.to_string());
        }
    }
    let mut shares = Vec::new();
    for (dataset, raw) in by_dataset {
        if raw != "off" && raw != "-" {
            let (networks, hosts, read_only, root) = parse_sharenfs_fields(&raw);
            shares.push(Share::Nfs { dataset, raw, networks, hosts, read_only, root });
        }
    }
    shares
}

/// Whether `raw` can be round-tripped losslessly through the structured
/// Networks/Hosts/Read-only/Root form — i.e. editing and resubmitting it
/// through that form won't silently drop information. `"on"`/`"off"` are
/// always losslessly representable. Anything using a clause
/// `parse_sharenfs_fields` doesn't recognize (`sec=`, `crossmnt`, etc.)
/// fails the round-trip and must be flagged to the user before they edit it.
pub fn sharenfs_editable_losslessly(raw: &str) -> bool {
    if raw == "on" || raw == "off" {
        return true;
    }
    let (networks, hosts, read_only, root) = parse_sharenfs_fields(raw);
    assemble_sharenfs(&networks, &hosts, read_only, root.as_deref()) == raw
}

pub fn list_shares(runner: &dyn CommandRunner) -> Result<Vec<Share>> {
    let output = super::run_checked(runner, "zfs", &["get", "-H", "-p", "-t", "filesystem", "sharenfs"], "zfs get sharenfs")?;
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

/// Whether a file sharing service runs on a host that is not FreeBSD (where `service ... onestatus` says it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Running,
    Stopped,
    /// The unit or service does not exist on the host (a Samba or NFS server package that was never installed).
    NotInstalled,
}

/// The NFS server's unit on Linux (systemd) and its service instance on illumos (SMF).
pub fn nfs_unit(os: HostOs) -> &'static str {
    if os == HostOs::Illumos { "svc:/network/nfs/server:default" } else { "nfs-server" }
}

/// The SMB server: Samba's `smbd` on Linux, the kernel SMB service on illumos.
pub fn smb_unit(os: HostOs) -> &'static str {
    if os == HostOs::Illumos { "svc:/network/smb/server:default" } else { "smbd" }
}

/// The names the Dashboard and the Shares title use for those services.
pub fn nfs_name(os: HostOs) -> &'static str {
    if os == HostOs::Illumos { "nfs/server" } else { "nfs-server" }
}

pub fn smb_name(os: HostOs) -> &'static str {
    if os == HostOs::Illumos { "smb/server" } else { "smbd" }
}

/// One service's state with the OS's own tool, without sudo: `svcs -H -o state <fmri>` on illumos, `systemctl show -p
/// LoadState -p ActiveState <unit>` on Linux. An ssh failure or an answer that cannot be read is an error; anything else
/// that is not "running" is stopped.
pub fn service_state(runner: &dyn CommandRunner, os: HostOs, unit: &str) -> Result<ServiceState> {
    let fail = |cmd: &str, out: &crate::exec::CommandOutput| crate::error::AppError::CommandFailed { cmd: cmd.to_string(), status: out.status, stderr: out.stderr.clone() };
    if os == HostOs::Illumos {
        let out = runner.run_unprivileged("svcs", &["-H", "-o", "state", unit])?;
        return match out.status {
            0 => Ok(if out.stdout.trim() == "online" { ServiceState::Running } else { ServiceState::Stopped }),
            1 if out.stderr.contains("doesn't match any instances") => Ok(ServiceState::NotInstalled),
            _ => Err(fail("svcs", &out)),
        };
    }
    let out = runner.run_unprivileged("systemctl", &["show", "-p", "LoadState", "-p", "ActiveState", unit])?;
    if out.status == 255 {
        return Err(fail("systemctl show", &out));
    }
    let value = |key: &str| out.stdout.lines().find_map(|l| l.strip_prefix(key)).map(str::trim).unwrap_or("");
    Ok(if value("LoadState=") == "not-found" {
        ServiceState::NotInstalled
    } else if value("ActiveState=") == "active" {
        ServiceState::Running
    } else {
        ServiceState::Stopped
    })
}

/// `zfs get -H -p -t filesystem sharesmb` (illumos's native SMB): one `Share::Smb` per dataset with the property set; the
/// share's name from `name=`, `guestok=true` as guest access. There is no path or user to show.
pub fn parse_sharesmb(output: &str) -> Vec<Share> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let &[dataset, "sharesmb", value, _] = fields.as_slice() else { return None };
            if value == "off" || value == "-" {
                return None;
            }
            let option = |key: &str| value.split(',').find_map(|o| o.strip_prefix(key));
            let name = option("name=").map_or_else(|| dataset.rsplit('/').next().unwrap_or(dataset).to_string(), str::to_string);
            Some(Share::Smb { dataset: dataset.to_string(), name, path: String::new(), valid_user: None, read_only: false, guest_ok: option("guestok=") == Some("true") })
        })
        .collect()
}

/// The Shares screen's data for a host running `os`: FreeBSD (and a host whose OS is not known) as before; on Linux and
/// illumos the datasets with `sharenfs` (raw values), on illumos also `sharesmb`, and each service's state. Read-only: no
/// FreeBSD tool (`service`, the Samba block of `smb4.conf`) is run.
pub fn list_shares_for(runner: &dyn CommandRunner, os: Option<HostOs>) -> Result<SharesSnapshot> {
    let Some(os @ (HostOs::Linux | HostOs::Illumos)) = os else { return list_shares_with_availability(runner) };
    let mut shares = list_shares(runner)?;
    if os == HostOs::Illumos {
        let out = super::run_checked(runner, "zfs", &["get", "-H", "-p", "-t", "filesystem", "sharesmb"], "zfs get sharesmb")?;
        shares.extend(parse_sharesmb(&out.stdout));
    }
    let nfs = service_state(runner, os, nfs_unit(os))?;
    let smb = service_state(runner, os, smb_unit(os))?;
    Ok(SharesSnapshot { shares, nfs_available: nfs == ServiceState::Running, samba_available: smb == ServiceState::Running, smb_error: None })
}

/// Composes all three calls into one snapshot — avoids extra
/// round-trips, mirrors `zfs::disk::list_disks_with_membership`.
pub fn list_shares_with_availability(runner: &dyn CommandRunner) -> Result<SharesSnapshot> {
    let mut shares = list_shares(runner)?;
    let nfs_available = nfs_server_available(runner)?;
    let samba_available = samba_available(runner)?;
    let mut smb_error = None;
    // Only read smb4.conf when Samba is up: a host without Samba (or with a
    // sudoers file that predates SMB support) must not report an error on
    // every refresh.
    if samba_available {
        match super::smb::list_managed(runner) {
            Ok(managed) => shares.extend(managed.into_iter().map(|s| Share::Smb {
                dataset: s.dataset,
                name: s.name,
                path: s.path,
                valid_user: s.valid_user,
                read_only: s.read_only,
                guest_ok: s.guest_ok,
            })),
            Err(e) => smb_error = Some(e.to_string()),
        }
    }
    Ok(SharesSnapshot { shares, nfs_available, samba_available, smb_error })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    #[test]
    fn linux_service_state_reads_systemctl_show() {
        use crate::zfs::host_health::HostOs::Linux;
        let r = |text: &str| FakeRunner::new("").route("systemctl", "show", text);
        assert_eq!(service_state(&r("LoadState=loaded\nActiveState=active\n"), Linux, nfs_unit(Linux)).unwrap(), ServiceState::Running);
        assert_eq!(service_state(&r("LoadState=loaded\nActiveState=inactive\n"), Linux, nfs_unit(Linux)).unwrap(), ServiceState::Stopped);
        assert_eq!(service_state(&r("LoadState=loaded\nActiveState=failed\n"), Linux, smb_unit(Linux)).unwrap(), ServiceState::Stopped);
        assert_eq!(service_state(&r("LoadState=not-found\nActiveState=inactive\n"), Linux, smb_unit(Linux)).unwrap(), ServiceState::NotInstalled, "a unit that does not exist is not installed, whatever ActiveState says");
        assert_eq!(service_state(&r(""), Linux, nfs_unit(Linux)).unwrap(), ServiceState::Stopped, "no answer lines: not running");
    }

    #[test]
    fn illumos_service_state_reads_svcs() {
        use crate::zfs::host_health::HostOs::Illumos;
        let ok = |text: &str| FakeRunner::new("").route("svcs", "-H", text);
        assert_eq!(service_state(&ok("online\n"), Illumos, nfs_unit(Illumos)).unwrap(), ServiceState::Running);
        for stopped in ["disabled\n", "offline\n", "maintenance\n"] {
            assert_eq!(service_state(&ok(stopped), Illumos, smb_unit(Illumos)).unwrap(), ServiceState::Stopped, "{stopped:?}");
        }
        let missing = FakeRunner::new("").route_failure("svcs", "-H", "svcs: Pattern 'svc:/network/nfs/server:default' doesn't match any instances\n", 1);
        assert_eq!(service_state(&missing, Illumos, nfs_unit(Illumos)).unwrap(), ServiceState::NotInstalled);
        let ssh = FakeRunner::new("").route_failure("svcs", "-H", "ssh: connection reset", 255);
        assert!(service_state(&ssh, Illumos, nfs_unit(Illumos)).is_err());
        let odd = FakeRunner::new("").route_failure("svcs", "-H", "svcs: something else went wrong", 2);
        assert!(service_state(&odd, Illumos, nfs_unit(Illumos)).is_err());
    }

    #[test]
    fn the_service_names_are_each_oss_own() {
        use crate::zfs::host_health::HostOs::{Illumos, Linux};
        assert_eq!((nfs_name(Linux), smb_name(Linux)), ("nfs-server", "smbd"));
        assert_eq!((nfs_name(Illumos), smb_name(Illumos)), ("nfs/server", "smb/server"));
    }

    const SHARESMB: &str = "rpool/export/nfs-test\tsharesmb\toff\tdefault\ntank/alice\tsharesmb\tname=alice\tlocal\ntank/carol\tsharesmb\tname=carol,guestok=true\tlocal\ntank\tsharesmb\t-\t-\n";

    #[test]
    fn illumos_native_smb_shares_come_from_sharesmb() {
        let shares = parse_sharesmb(SHARESMB);
        assert_eq!(shares.len(), 2, "{shares:?}");
        let Share::Smb { dataset, name, guest_ok, .. } = &shares[1] else { panic!("{shares:?}") };
        assert_eq!((dataset.as_str(), name.as_str(), *guest_ok), ("tank/carol", "carol", true));
        assert!(parse_sharesmb("").is_empty() && parse_sharesmb("garbage").is_empty());
    }

    #[test]
    fn the_snapshot_on_linux_and_illumos_lists_nfs_and_service_state_without_freebsd_tools() {
        use crate::zfs::host_health::HostOs::Illumos;
        let r = FakeRunner::new("")
            .route("zfs", "get", "tank/data\tsharenfs\trw=@10.0.0.0/24\tlocal\ntank/off\tsharenfs\toff\tdefault\n")
            .route("svcs", "-H", "online\n");
        let snapshot = list_shares_for(&r, Some(Illumos)).unwrap();
        assert_eq!(snapshot.shares.len(), 1, "{:?}", snapshot.shares);
        assert!(snapshot.nfs_available && snapshot.samba_available && snapshot.smb_error.is_none());
        let calls: Vec<String> = r.calls.lock().unwrap().iter().map(|(c, _)| c.clone()).chain(r.unprivileged_calls.lock().unwrap().iter().map(|(c, _)| c.clone())).collect();
        assert!(calls.iter().all(|c| matches!(c.as_str(), "zfs" | "svcs")), "no service/cat/smbpasswd: {calls:?}");
        let freebsd = FakeRunner::new("").route("zfs", "get", "").route("service", "nfsd", "nfsd is running\n").route("service", "samba_server", "smbd is running\n").route("cat", crate::zfs::smb::CONF_PATH, "[global]\n");
        assert!(list_shares_for(&freebsd, None).unwrap().nfs_available, "an unknown OS asks FreeBSD's tools");
    }

    #[test]
    fn parse_sharenfs_fields_bare_on_means_no_restriction() {
        assert_eq!(parse_sharenfs_fields("on"), (vec![], vec![], false, None));
    }

    #[test]
    fn parse_sharenfs_fields_network_and_maproot() {
        // FreeBSD-native exports(5) syntax -- confirmed live against the
        // real test box (see share.rs's module doc comment for the full
        // story): ZFS's own rw=@net,root=user mini-language is NOT what
        // FreeBSD's mountd actually understands.
        assert_eq!(
            parse_sharenfs_fields("-maproot=nobody -network 172.16.10.0/24"),
            (vec!["172.16.10.0/24".to_string()], vec![], false, Some("nobody".to_string()))
        );
    }

    #[test]
    fn parse_sharenfs_fields_ro_with_bare_host() {
        assert_eq!(parse_sharenfs_fields("-ro somehost"), (vec![], vec!["somehost".to_string()], true, None));
    }

    #[test]
    fn parse_sharenfs_fields_ignores_unrecognized_flags_but_keeps_recognized_ones() {
        assert_eq!(
            parse_sharenfs_fields("-alldirs -ro -network 172.16.10.0/24"),
            (vec!["172.16.10.0/24".to_string()], vec![], true, None)
        );
    }

    #[test]
    fn parse_sharenfs_fields_semicolon_separates_multiple_network_clauses() {
        // Confirmed live: each semicolon-separated set becomes its own
        // exports(5) line, letting the same root mapping apply to
        // multiple networks.
        assert_eq!(
            parse_sharenfs_fields("-maproot=nobody -network 172.16.10.0/24;-maproot=nobody -network 172.16.20.0/24"),
            (vec!["172.16.10.0/24".to_string(), "172.16.20.0/24".to_string()], vec![], false, Some("nobody".to_string()))
        );
    }

    #[test]
    fn assemble_sharenfs_all_blank_produces_on() {
        assert_eq!(assemble_sharenfs(&[], &[], false, None), "on");
    }

    #[test]
    fn assemble_sharenfs_read_only_with_no_restriction_produces_bare_ro() {
        assert_eq!(assemble_sharenfs(&[], &[], true, None), "-ro");
    }

    #[test]
    fn assemble_sharenfs_single_network_with_root_produces_native_syntax() {
        assert_eq!(assemble_sharenfs(&["172.16.10.0/24".to_string()], &[], false, Some("nobody")), "-maproot=nobody -network 172.16.10.0/24");
    }

    #[test]
    fn assemble_sharenfs_networks_and_hosts_produce_semicolon_separated_clauses() {
        assert_eq!(
            assemble_sharenfs(&["172.16.10.0/24".to_string()], &["somehost".to_string()], false, Some("nobody")),
            "-maproot=nobody somehost;-maproot=nobody -network 172.16.10.0/24"
        );
    }

    #[test]
    fn assemble_sharenfs_multiple_networks_produce_one_clause_each() {
        assert_eq!(
            assemble_sharenfs(&["172.16.10.0/24".to_string(), "172.16.20.0/24".to_string()], &[], false, Some("nobody")),
            "-maproot=nobody -network 172.16.10.0/24;-maproot=nobody -network 172.16.20.0/24"
        );
    }

    #[test]
    fn parse_then_assemble_sharenfs_round_trips_a_canonical_value() {
        let raw = "-maproot=nobody -network 172.16.10.0/24";
        let (networks, hosts, read_only, root) = parse_sharenfs_fields(raw);
        assert_eq!(assemble_sharenfs(&networks, &hosts, read_only, root.as_deref()), raw);
    }

    #[test]
    fn sharenfs_editable_losslessly_true_for_canonical_values() {
        assert!(sharenfs_editable_losslessly("on"));
        assert!(sharenfs_editable_losslessly("-maproot=nobody -network 172.16.10.0/24"));
    }

    #[test]
    fn sharenfs_editable_losslessly_false_for_unrecognized_flags() {
        assert!(!sharenfs_editable_losslessly("-alldirs -ro -network 172.16.10.0/24"));
    }

    const SHARES_FIXTURE: &str = "\
tank/both\tsharenfs\ton\tlocal
tank/data\tsharenfs\toff\tdefault
tank/nfsonly\tsharenfs\t-maproot=nobody -network 172.16.10.0/24\tlocal
tank/smbonly\tsharenfs\toff\tdefault
tank/weird\tsharenfs\t-alldirs -ro -network 172.16.10.0/24\tlocal
tank/data@snap1\tsharenfs\t-\t-
";

    #[test]
    fn parse_shares_groups_by_dataset_and_drops_fully_unshared_datasets() {
        let shares = parse_shares(SHARES_FIXTURE);
        assert_eq!(
            shares,
            vec![
                Share::Nfs { dataset: "tank/both".to_string(), raw: "on".to_string(), networks: vec![], hosts: vec![], read_only: false, root: None },
                Share::Nfs {
                    dataset: "tank/nfsonly".to_string(),
                    raw: "-maproot=nobody -network 172.16.10.0/24".to_string(),
                    networks: vec!["172.16.10.0/24".to_string()],
                    hosts: vec![],
                    read_only: false,
                    root: Some("nobody".to_string()),
                },
                Share::Nfs {
                    dataset: "tank/weird".to_string(),
                    raw: "-alldirs -ro -network 172.16.10.0/24".to_string(),
                    networks: vec!["172.16.10.0/24".to_string()],
                    hosts: vec![],
                    read_only: true,
                    root: None,
                },
            ],
            "tank/data (both off) must not appear; tank/data@snap1 (a snapshot, both '-') must not appear; tank/weird's unrecognized -alldirs flag must not drop the share, raw must be preserved"
        );
    }

    #[test]
    fn list_shares_uses_correct_command_and_parses_result() {
        let runner = FakeRunner::new("tank/data\tsharenfs\ton\tlocal\n");
        let shares = list_shares(&runner).unwrap();
        assert_eq!(shares, vec![Share::Nfs { dataset: "tank/data".to_string(), raw: "on".to_string(), networks: vec![], hosts: vec![], read_only: false, root: None }]);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["get".to_string(), "-H".to_string(), "-p".to_string(), "-t".to_string(), "filesystem".to_string(), "sharenfs".to_string()]));
    }

    #[test]
    fn list_shares_surfaces_command_failure() {
        let runner = FakeRunner::failing("permission denied", 1);
        assert!(list_shares(&runner).is_err());
    }

    #[test]
    fn nfs_server_available_true_when_exit_zero() {
        let runner = FakeRunner::new("nfsd is running as pid 1234.\n");
        assert!(nfs_server_available(&runner).unwrap());
    }

    #[test]
    fn nfs_server_available_false_when_exit_nonzero() {
        let runner = FakeRunner::new("nfsd is not running.\n").with_status(1);
        assert!(!nfs_server_available(&runner).unwrap());
    }

    #[test]
    fn samba_available_true_when_smbd_running_even_if_exit_nonzero() {
        // The real, confirmed case: winbindd down drags onestatus's
        // exit code to nonzero even though smbd itself is fine.
        let runner = FakeRunner::new("nmbd is running as pid 1.\nsmbd is running as pid 2.\nwinbindd is not running.\n").with_status(1);
        assert!(samba_available(&runner).unwrap());
    }

    #[test]
    fn samba_available_false_when_smbd_itself_is_not_running() {
        let runner = FakeRunner::new("nmbd is not running.\nsmbd is not running.\nwinbindd is not running.\n").with_status(1);
        assert!(!samba_available(&runner).unwrap());
    }

    const MANAGED_CONF: &str = "[global]\n  x = y\n\n## zfsctl managed shares -- do not edit by hand -- start\n[media]\n    # zfsctl-dataset = tank/media\n    path = /tank/media\n    valid users = carol\n    browseable = yes\n    read only = no\n## zfsctl managed shares -- do not edit by hand -- end\n";

    #[test]
    fn list_shares_with_availability_merges_nfs_and_managed_smb_shares() {
        let runner = FakeRunner::new("")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\n")
            .route("service", "nfsd", "nfsd is running as pid 1234.\n")
            .route("service", "samba_server", "smbd is running as pid 2.\n")
            .route("cat", crate::zfs::smb::CONF_PATH, MANAGED_CONF);
        let snapshot = list_shares_with_availability(&runner).unwrap();
        assert!(snapshot.nfs_available);
        assert!(snapshot.samba_available);
        assert_eq!(snapshot.smb_error, None);
        assert_eq!(snapshot.shares.len(), 2);
        assert_eq!(
            snapshot.shares[1],
            Share::Smb {
                dataset: "tank/media".to_string(),
                name: "media".to_string(),
                path: "/tank/media".to_string(),
                valid_user: Some("carol".to_string()),
                read_only: false,
                guest_ok: false,
            }
        );
    }

    #[test]
    fn smb_conf_is_not_read_when_samba_is_not_running() {
        let runner = FakeRunner::new("")
            .route("zfs", "get", "")
            .route("service", "nfsd", "nfsd is running as pid 1.\n")
            .route("service", "samba_server", "smbd is not running.\n")
            .route_failure("cat", crate::zfs::smb::CONF_PATH, "sudo: a password is required", 1);
        let snapshot = list_shares_with_availability(&runner).unwrap();
        assert_eq!(snapshot.smb_error, None);
        assert!(!runner.calls.lock().unwrap().iter().any(|(c, _)| c == "cat"));
    }

    #[test]
    fn an_unreadable_smb_conf_leaves_nfs_shares_intact_and_reports_the_problem() {
        let runner = FakeRunner::new("")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\n")
            .route("service", "nfsd", "nfsd is running as pid 1.\n")
            .route("service", "samba_server", "smbd is running as pid 2.\n")
            .route_failure("cat", crate::zfs::smb::CONF_PATH, "sudo: a password is required", 1);
        let snapshot = list_shares_with_availability(&runner).unwrap();
        assert_eq!(snapshot.shares.len(), 1);
        assert!(snapshot.smb_error.unwrap().contains("password is required"));
    }

    #[test]
    fn list_shares_with_availability_surfaces_command_failure() {
        let runner = FakeRunner::failing("permission denied", 1);
        assert!(list_shares_with_availability(&runner).is_err());
    }

    #[test]
    fn sharenfs_editable_losslessly_accepts_on_and_off_but_not_unrecognized_clauses() {
        assert!(sharenfs_editable_losslessly("off"));
        assert!(sharenfs_editable_losslessly("on"));
        assert!(!sharenfs_editable_losslessly("-sec=krb5 -network 10.0.0.0/8"));
    }
}
