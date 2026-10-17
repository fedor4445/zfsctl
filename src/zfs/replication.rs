//! The pure parts of replication: reading the listings a transfer is planned from, choosing what
//! to send, and building the `zfs send`/`zfs receive` argument lists.

use crate::daemon::snapshot::is_own;
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapInfo {
    /// The part after `@`.
    pub name: String,
    pub guid: u64,
    pub createtxg: u64,
}

const LIST_ARGS: &[&str] = &["list", "-t", "snapshot", "-H", "-p", "-o", "name,guid,createtxg", "-d", "1"];

/// Parses `zfs list -t snapshot -H -p -o name,guid,createtxg`, keeping only `dataset`'s own snapshots.
pub fn parse_snapshot_infos(output: &str, dataset: &str) -> Result<Vec<SnapInfo>> {
    let mut out = Vec::new();
    for line in output.lines().filter(|l| !l.trim().is_empty()) {
        let bad = || AppError::Parse(format!("bad snapshot line: {line:?}"));
        let fields: Vec<&str> = line.split('\t').collect();
        let [full, guid, txg] = fields.as_slice() else { return Err(bad()) };
        let (ds, name) = full.split_once('@').ok_or_else(bad)?;
        if ds != dataset {
            continue;
        }
        out.push(SnapInfo { name: name.to_string(), guid: guid.parse().map_err(|_| bad())?, createtxg: txg.parse().map_err(|_| bad())? });
    }
    Ok(out)
}

pub fn list_snapshot_infos(runner: &dyn CommandRunner, dataset: &str) -> Result<Vec<SnapInfo>> {
    let mut args: Vec<&str> = LIST_ARGS.to_vec();
    args.push(dataset);
    let output = super::run_checked(runner, "zfs", &args, &format!("zfs list -t snapshot {dataset}"))?;
    parse_snapshot_infos(&output.stdout, dataset)
}

/// Like `list_snapshot_infos`, but a dataset that does not exist (yet) has no snapshots.
pub fn list_snapshot_infos_or_empty(runner: &dyn CommandRunner, dataset: &str) -> Result<Vec<SnapInfo>> {
    match list_snapshot_infos(runner, dataset) {
        Err(AppError::CommandFailed { stderr, .. }) if stderr.contains("does not exist") => Ok(Vec::new()),
        other => other,
    }
}

/// The `receive_resume_token` of `dataset`: what an interrupted `zfs receive -s` left behind.
pub fn resume_token(runner: &dyn CommandRunner, dataset: &str) -> Result<Option<String>> {
    let args = ["get", "-H", "-p", "-o", "value", "receive_resume_token", dataset];
    match super::run_checked(runner, "zfs", &args, &format!("zfs get receive_resume_token {dataset}")) {
        Ok(output) => {
            let value = output.stdout.trim();
            Ok((!value.is_empty() && value != "-").then(|| value.to_string()))
        }
        Err(AppError::CommandFailed { stderr, .. }) if stderr.contains("does not exist") => Ok(None),
        Err(e) => Err(e),
    }
}

/// The stream size from `zfs send -nvP` output (the `size<TAB>N` line).
pub fn parse_send_size(output: &str) -> Option<u64> {
    output.lines().find_map(|l| l.strip_prefix("size\t")).and_then(|v| v.trim().parse().ok())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// No source snapshot matches the prefix.
    NoSnapshots,
    /// The destination already has the newest matching snapshot.
    UpToDate(String),
    /// Send `new`, incrementally from `base` when the two sides have one in common.
    Send { new: String, base: Option<String> },
}

/// Chooses what to send: the newest source snapshot named `<prefix>-<timestamp>`, incrementally
/// from the newest *older* snapshot both sides have, matched by guid (names can differ, and a
/// snapshot pruned from the source costs a longer increment, never a wrong one).
pub fn plan(source: &[SnapInfo], dest: &[SnapInfo], prefix: &str) -> Plan {
    let Some(newest) = source.iter().filter(|s| is_own(prefix, &s.name)).max_by(|a, b| a.name.cmp(&b.name)) else {
        return Plan::NoSnapshots;
    };
    if dest.iter().any(|d| d.guid == newest.guid) {
        return Plan::UpToDate(newest.name.clone());
    }
    let base = source.iter().filter(|s| s.createtxg < newest.createtxg && dest.iter().any(|d| d.guid == s.guid)).max_by_key(|s| s.createtxg).map(|s| s.name.clone());
    Plan::Send { new: newest.name.clone(), base }
}

/// `zfs send [-nvP] [-i @base] -- dataset@new`; `dry` only reports the size.
/// The names (after `@`) of the job's own snapshots on the destination to destroy, oldest first: all
/// but the newest `keep` by `createtxg`. Anything not exactly `<prefix>-<timestamp>` is never listed,
/// and `keep` of 0 selects nothing rather than everything.
pub fn prune_selection(listing: &[SnapInfo], prefix: &str, keep: u32) -> Vec<String> {
    if keep == 0 {
        return Vec::new();
    }
    let mut own: Vec<&SnapInfo> = listing.iter().filter(|s| is_own(prefix, &s.name)).collect();
    own.sort_by(|a, b| (a.createtxg, &a.name).cmp(&(b.createtxg, &b.name)));
    let excess = own.len().saturating_sub(keep as usize);
    own[..excess].iter().map(|s| s.name.clone()).collect()
}

pub fn send_args(dataset: &str, new: &str, base: Option<&str>, dry: bool) -> Vec<String> {
    let mut args = vec!["send".to_string()];
    if dry {
        args.push("-nvP".to_string());
    }
    if let Some(base) = base {
        args.push("-i".to_string());
        args.push(format!("@{base}"));
    }
    args.push("--".to_string());
    args.push(format!("{dataset}@{new}"));
    args
}

/// `zfs send [-nvP] -t <token>`: continue an interrupted receive.
pub fn resume_send_args(token: &str, dry: bool) -> Vec<String> {
    let mut args = vec!["send".to_string()];
    if dry {
        args.push("-nvP".to_string());
    }
    args.push("-t".to_string());
    args.push(token.to_string());
    args
}

/// `zfs receive -s -u [-F] -- dest`: resumable, never mounted, rolled back only when forced.
pub fn receive_args(dest: &str, force: bool) -> Vec<String> {
    let mut args = vec!["receive".to_string(), "-s".to_string(), "-u".to_string()];
    if force {
        args.push("-F".to_string());
    }
    args.push("--".to_string());
    args.push(dest.to_string());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    fn info(name: &str, guid: u64, txg: u64) -> SnapInfo {
        SnapInfo { name: name.to_string(), guid, createtxg: txg }
    }

    const T1: &str = "hourly-20261006T100000Z";
    const T2: &str = "hourly-20261006T110000Z";
    const T3: &str = "hourly-20261006T120000Z";

    #[test]
    fn only_exact_prefix_timestamp_names_are_selected() {
        let listing = vec![
            info("hourly-latest", 1, 1),
            info("hourly-20261006T1000Z", 2, 2),
            info("hourlyx-20261006T090000Z", 3, 3),
            info("manual", 4, 4),
            info("hourly-20261006T090000Z-keep", 5, 5),
            info(T1, 6, 10),
            info(T2, 7, 20),
            info(T3, 8, 30),
        ];
        assert_eq!(prune_selection(&listing, "hourly", 1), vec![T1, T2]);
    }

    #[test]
    fn order_is_by_createtxg_not_name() {
        let listing = vec![info(T1, 1, 30), info(T2, 2, 20), info(T3, 3, 10)];
        assert_eq!(prune_selection(&listing, "hourly", 1), vec![T3, T2]);
    }

    #[test]
    fn the_newest_is_kept_and_nothing_is_selected_when_there_are_n_or_fewer() {
        let three = vec![info(T1, 1, 10), info(T2, 2, 20), info(T3, 3, 30)];
        assert!(prune_selection(&three[..1], "hourly", 1).is_empty());
        assert!(prune_selection(&three, "hourly", 3).is_empty());
        assert_eq!(prune_selection(&three, "hourly", 2), vec![T1]);
    }

    #[test]
    fn keep_zero_selects_nothing() {
        assert!(prune_selection(&[info(T1, 1, 10), info(T2, 2, 20)], "hourly", 0).is_empty());
    }

    #[test]
    fn parses_name_guid_and_txg_and_ignores_other_datasets() {
        let out = "tank/data@a\t111\t10\ntank/data@b\t222\t20\ntank/data/child@a\t333\t11\n";
        assert_eq!(parse_snapshot_infos(out, "tank/data").unwrap(), vec![info("a", 111, 10), info("b", 222, 20)]);
        assert!(parse_snapshot_infos("tank/data@a\t111\n", "tank/data").is_err());
        assert!(parse_snapshot_infos("tank/data@a\tx\t1\n", "tank/data").is_err());
        assert!(parse_snapshot_infos("", "tank/data").unwrap().is_empty());
    }

    #[test]
    fn a_missing_destination_has_no_snapshots_and_no_token() {
        let gone = FakeRunner::failing("cannot open 'backup/x': dataset does not exist", 1);
        assert!(list_snapshot_infos_or_empty(&gone, "backup/x").unwrap().is_empty());
        assert_eq!(resume_token(&gone, "backup/x").unwrap(), None);
        let broken = FakeRunner::failing("permission denied", 1);
        assert!(list_snapshot_infos_or_empty(&broken, "backup/x").is_err());
        assert!(resume_token(&broken, "backup/x").is_err());
    }

    #[test]
    fn a_dash_is_no_token_and_anything_else_is_the_token() {
        assert_eq!(resume_token(&FakeRunner::new("-\n"), "d").unwrap(), None);
        assert_eq!(resume_token(&FakeRunner::new("1-abc-def\n"), "d").unwrap(), Some("1-abc-def".to_string()));
    }

    #[test]
    fn the_send_size_is_the_size_line() {
        // As printed live on 15.1 and 16.0-CURRENT, including the resume-token preamble.
        let full = "full\ttank/data@a\t1234567\nsize\t1234567\n";
        let incremental = "incremental\ttank/data@a\ttank/data@b\t99\nsize\t99\n";
        let resumed = "resume token contents:\nnvlist version: 0\n\tobject = 0x80\n\ttoname = tank/data@b\nincremental\ttank/data@a\ttank/data@b\t1735128\nsize\t1735128\n";
        assert_eq!(parse_send_size(full), Some(1_234_567));
        assert_eq!(parse_send_size(incremental), Some(99));
        assert_eq!(parse_send_size(resumed), Some(1_735_128));
        assert_eq!(parse_send_size("nothing useful"), None);
    }

    #[test]
    fn the_newest_prefixed_snapshot_is_sent_in_full_when_the_destination_has_nothing() {
        let src = vec![info("hourly-20261006T100000Z", 1, 10), info("hourly-20261006T110000Z", 2, 20), info("manual", 9, 30), info("daily-20261006T120000Z", 8, 40)];
        assert_eq!(plan(&src, &[], "hourly"), Plan::Send { new: "hourly-20261006T110000Z".to_string(), base: None });
    }

    #[test]
    fn the_base_is_the_newest_snapshot_both_sides_have_by_guid() {
        let src = vec![info("hourly-20261006T100000Z", 1, 10), info("hourly-20261006T110000Z", 2, 20), info("hourly-20261006T120000Z", 3, 30)];
        // The destination has 1 and 2 (names may differ; only the guid counts) but not 3.
        let dst = vec![info("renamed-one", 1, 5), info("hourly-20261006T110000Z", 2, 6)];
        assert_eq!(plan(&src, &dst, "hourly"), Plan::Send { new: "hourly-20261006T120000Z".to_string(), base: Some("hourly-20261006T110000Z".to_string()) });
    }

    #[test]
    fn a_source_snapshot_pruned_since_the_last_run_costs_a_longer_incremental_not_a_wrong_one() {
        // 11:00 was pruned from the source; the destination still has 10:00 (guid 1) and 11:00 (guid 2).
        let src = vec![info("hourly-20261006T100000Z", 1, 10), info("hourly-20261006T120000Z", 3, 30)];
        let dst = vec![info("hourly-20261006T100000Z", 1, 5), info("hourly-20261006T110000Z", 2, 6)];
        assert_eq!(plan(&src, &dst, "hourly"), Plan::Send { new: "hourly-20261006T120000Z".to_string(), base: Some("hourly-20261006T100000Z".to_string()) });
    }

    #[test]
    fn a_destination_with_snapshots_but_none_in_common_gets_no_base() {
        let src = vec![info("hourly-20261006T100000Z", 1, 10)];
        let dst = vec![info("unrelated", 77, 5)];
        assert_eq!(plan(&src, &dst, "hourly"), Plan::Send { new: "hourly-20261006T100000Z".to_string(), base: None });
    }

    #[test]
    fn nothing_to_send_when_up_to_date_or_when_no_snapshot_matches() {
        let src = vec![info("hourly-20261006T100000Z", 1, 10)];
        assert_eq!(plan(&src, &[info("whatever", 1, 3)], "hourly"), Plan::UpToDate("hourly-20261006T100000Z".to_string()));
        assert_eq!(plan(&src, &[], "daily"), Plan::NoSnapshots);
        assert_eq!(plan(&[], &[], "hourly"), Plan::NoSnapshots);
    }

    #[test]
    fn the_argument_lists_are_exactly_what_was_verified_live() {
        assert_eq!(send_args("tank/data", "h-2", None, false), ["send", "--", "tank/data@h-2"]);
        assert_eq!(send_args("tank/data", "h-2", Some("h-1"), false), ["send", "-i", "@h-1", "--", "tank/data@h-2"]);
        assert_eq!(send_args("tank/data", "h-2", Some("h-1"), true), ["send", "-nvP", "-i", "@h-1", "--", "tank/data@h-2"]);
        assert_eq!(resume_send_args("1-abc", false), ["send", "-t", "1-abc"]);
        assert_eq!(resume_send_args("1-abc", true), ["send", "-nvP", "-t", "1-abc"]);
        assert_eq!(receive_args("backup/data", false), ["receive", "-s", "-u", "--", "backup/data"]);
        assert_eq!(receive_args("backup/data", true), ["receive", "-s", "-u", "-F", "--", "backup/data"]);
    }
}
