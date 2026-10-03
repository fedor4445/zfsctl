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

/// Parses a `sharenfs` value using FreeBSD's **native `exports(5)`**
/// syntax into `(networks, hosts, read_only, root)`.
///
/// **This is not OpenZFS's own cross-platform `sharenfs` mini-language**
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
/// OpenZFS docs — `crossmnt`/`no_subtree_check` aren't FreeBSD concepts
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
/// (see its doc comment for why this isn't OpenZFS's cross-platform
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
            if raw != "off" && raw != "-" {
                let (networks, hosts, read_only, root) = parse_sharenfs_fields(raw);
                shares.push(Share::Nfs { dataset: dataset.clone(), raw: raw.to_string(), networks, hosts, read_only, root });
            }
        }
        if let Some(name) = sharesmb.as_deref() {
            if name != "off" && name != "-" {
                let name = if name == "on" { None } else { Some(name.to_string()) };
                shares.push(Share::Smb { dataset: dataset.clone(), name });
            }
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
    let output = super::run_checked(runner, "zfs", &["get", "-H", "-p", "-t", "filesystem", "sharenfs,sharesmb"], "zfs get sharenfs,sharesmb")?;
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
tank/both\tsharesmb\ton\tlocal
tank/data\tsharenfs\toff\tdefault
tank/data\tsharesmb\toff\tdefault
tank/nfsonly\tsharenfs\t-maproot=nobody -network 172.16.10.0/24\tlocal
tank/nfsonly\tsharesmb\toff\tdefault
tank/smbonly\tsharenfs\toff\tdefault
tank/smbonly\tsharesmb\tmyshare\tlocal
tank/weird\tsharenfs\t-alldirs -ro -network 172.16.10.0/24\tlocal
tank/weird\tsharesmb\toff\tdefault
tank/data@snap1\tsharenfs\t-\t-
tank/data@snap1\tsharesmb\t-\t-
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
                    raw: "-maproot=nobody -network 172.16.10.0/24".to_string(),
                    networks: vec!["172.16.10.0/24".to_string()],
                    hosts: vec![],
                    read_only: false,
                    root: Some("nobody".to_string()),
                },
                Share::Smb { dataset: "tank/smbonly".to_string(), name: Some("myshare".to_string()) },
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
        let runner = FakeRunner::new("tank/data\tsharenfs\ton\tlocal\ntank/data\tsharesmb\toff\tdefault\n");
        let shares = list_shares(&runner).unwrap();
        assert_eq!(shares, vec![Share::Nfs { dataset: "tank/data".to_string(), raw: "on".to_string(), networks: vec![], hosts: vec![], read_only: false, root: None }]);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["get".to_string(), "-H".to_string(), "-p".to_string(), "-t".to_string(), "filesystem".to_string(), "sharenfs,sharesmb".to_string()]));
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
