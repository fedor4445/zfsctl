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

const BOOKMARK_LIST_ARGS: &[&str] = &["list", "-t", "bookmark", "-H", "-p", "-o", "name,guid,createtxg", "-d", "1"];

/// How many of a job's own bookmarks are kept on the source: the newest one is the base of the next run, the one before it is a
/// spare in case the newest was made for a snapshot that never reached the destination.
pub const BOOKMARKS_KEPT: usize = 2;

/// Parses `zfs list -t bookmark -H -p -o name,guid,createtxg`, keeping only `dataset`'s own bookmarks (the part after `#`).
pub fn parse_bookmark_infos(output: &str, dataset: &str) -> Result<Vec<SnapInfo>> {
    let mut out = Vec::new();
    for line in output.lines().filter(|l| !l.trim().is_empty()) {
        let bad = || AppError::Parse(format!("bad bookmark line: {line:?}"));
        let fields: Vec<&str> = line.split('\t').collect();
        let [full, guid, txg] = fields.as_slice() else { return Err(bad()) };
        let (ds, name) = full.split_once('#').ok_or_else(bad)?;
        if ds != dataset {
            continue;
        }
        out.push(SnapInfo { name: name.to_string(), guid: guid.parse().map_err(|_| bad())?, createtxg: txg.parse().map_err(|_| bad())? });
    }
    Ok(out)
}

/// The bookmarks of `dataset`. A host that cannot list them (an old `zfs`) simply has none: bookmarks only ever widen what a
/// run can do, so not seeing them is never a reason to fail.
pub fn list_bookmark_infos_or_empty(runner: &dyn CommandRunner, dataset: &str) -> Vec<SnapInfo> {
    let mut args: Vec<&str> = BOOKMARK_LIST_ARGS.to_vec();
    args.push(dataset);
    super::run_checked(runner, "zfs", &args, &format!("zfs list -t bookmark {dataset}")).ok().and_then(|o| parse_bookmark_infos(&o.stdout, dataset).ok()).unwrap_or_default()
}

/// The start of the names of the bookmarks one job keeps: `zfsctl-<job id>-`, the id cut to the characters a bookmark name may hold.
pub fn bookmark_prefix(job_id: &str) -> String {
    let id: String = job_id.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':') { c } else { '_' }).collect();
    format!("zfsctl-{id}-")
}

/// Only the job's own bookmarks: a user's bookmarks, and other jobs', are never touched.
pub fn own_bookmarks(all: &[SnapInfo], job_id: &str) -> Vec<SnapInfo> {
    let prefix = bookmark_prefix(job_id);
    all.iter().filter(|b| b.name.starts_with(&prefix) && b.name.len() > prefix.len()).cloned().collect()
}

/// The names of the job's own bookmarks to destroy: all but the newest [`BOOKMARKS_KEPT`] by `createtxg`.
pub fn bookmarks_to_prune(own: &[SnapInfo]) -> Vec<String> {
    let mut sorted: Vec<&SnapInfo> = own.iter().collect();
    sorted.sort_by(|a, b| (a.createtxg, &a.name).cmp(&(b.createtxg, &b.name)));
    let excess = sorted.len().saturating_sub(BOOKMARKS_KEPT);
    sorted[..excess].iter().map(|b| b.name.clone()).collect()
}

/// Keeps a bookmark of `snapshot` (already on the destination) so that the next incremental send still has a base when the
/// source's own snapshot has been deleted by then, and drops the job's older bookmarks. Returns a note for the run's message
/// (empty when all went well): a bookmark that cannot be made never fails a run that has already done its work.
pub fn keep_bookmark(runner: &dyn CommandRunner, dataset: &str, job_id: &str, snapshot: &SnapInfo) -> String {
    let own = own_bookmarks(&list_bookmark_infos_or_empty(runner, dataset), job_id);
    if !own.iter().any(|b| b.guid == snapshot.guid) {
        let name = format!("{}{}", bookmark_prefix(job_id), snapshot.name);
        let args = ["bookmark".to_string(), format!("{dataset}@{}", snapshot.name), format!("{dataset}#{name}")];
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        if let Err(e) = super::run_checked(runner, "zfs", &refs, &format!("zfs bookmark {dataset}@{}", snapshot.name)) {
            return format!("; the bookmark of {} was not kept: {e}", snapshot.name);
        }
    }
    let own = own_bookmarks(&list_bookmark_infos_or_empty(runner, dataset), job_id);
    for name in bookmarks_to_prune(&own) {
        let target = format!("{dataset}#{name}");
        if let Err(e) = super::run_checked(runner, "zfs", &["destroy", &target], &format!("zfs destroy {target}")) {
            return format!("; an old bookmark was not removed: {e}");
        }
    }
    String::new()
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
    Send { new: String, base: Option<Base> },
}

/// What an incremental send starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// A snapshot both sides have.
    Snapshot(String),
    /// A bookmark on the source whose snapshot is gone there; `on_destination` is the snapshot of the same guid on the
    /// destination, which must stay until the send has been received.
    Bookmark { name: String, on_destination: String },
}

impl Base {
    /// The argument of `zfs send -i`: `@snapshot` or `#bookmark`.
    pub fn reference(&self) -> String {
        match self {
            Base::Snapshot(name) => format!("@{name}"),
            Base::Bookmark { name, .. } => format!("#{name}"),
        }
    }

    /// The snapshot of this base as the destination knows it (what a prune there must leave alone).
    pub fn on_destination(&self) -> &str {
        match self {
            Base::Snapshot(name) => name,
            Base::Bookmark { on_destination, .. } => on_destination,
        }
    }
}

/// Chooses what to send: the newest source snapshot named `<prefix>-<timestamp>`, incrementally
/// from the newest *older* snapshot both sides have, matched by guid (names can differ, and a
/// snapshot pruned from the source costs a longer increment, never a wrong one).
pub fn plan(source: &[SnapInfo], dest: &[SnapInfo], prefix: &str) -> Plan {
    plan_with_bookmarks(source, dest, &[], prefix)
}

/// `plan`, with the job's own bookmarks on the source as further candidates for the base: a bookmark keeps the base alive
/// after the source's snapshot has been deleted. The newest base (by `createtxg`) wins, and a snapshot wins over a bookmark of
/// the same age.
pub fn plan_with_bookmarks(source: &[SnapInfo], dest: &[SnapInfo], bookmarks: &[SnapInfo], prefix: &str) -> Plan {
    let Some(newest) = source.iter().filter(|s| is_own(prefix, &s.name)).max_by(|a, b| a.name.cmp(&b.name)) else {
        return Plan::NoSnapshots;
    };
    if dest.iter().any(|d| d.guid == newest.guid) {
        return Plan::UpToDate(newest.name.clone());
    }
    let snapshot = source.iter().filter(|s| s.createtxg < newest.createtxg && dest.iter().any(|d| d.guid == s.guid)).max_by_key(|s| s.createtxg);
    let bookmark = bookmarks.iter().filter(|b| b.createtxg < newest.createtxg).filter_map(|b| dest.iter().find(|d| d.guid == b.guid).map(|d| (b, d))).max_by_key(|(b, _)| b.createtxg);
    let base = match (snapshot, bookmark) {
        (Some(s), Some((b, _))) if s.createtxg >= b.createtxg => Some(Base::Snapshot(s.name.clone())),
        (_, Some((b, d))) => Some(Base::Bookmark { name: b.name.clone(), on_destination: d.name.clone() }),
        (Some(s), None) => Some(Base::Snapshot(s.name.clone())),
        (None, None) => None,
    };
    Plan::Send { new: newest.name.clone(), base }
}

/// What the next run would destroy if it first adds one more own snapshot to the destination (a send
/// does): the preview a form shows, so a save that makes the next run destroy an existing snapshot is
/// never described as harmless. It is an upper bound: a run that finds nothing to send destroys less.
pub fn prune_preview(listing: &[SnapInfo], prefix: &str, keep: u32) -> Vec<String> {
    match keep {
        0 => Vec::new(),
        1 => {
            let mut own: Vec<&SnapInfo> = listing.iter().filter(|s| is_own(prefix, &s.name)).collect();
            own.sort_by(|a, b| (a.createtxg, &a.name).cmp(&(b.createtxg, &b.name)));
            own.iter().map(|s| s.name.clone()).collect()
        }
        n => prune_selection(listing, prefix, n - 1),
    }
}

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

/// `zfs send [-w] [-nvP] [-i @base|#bookmark] -- dataset@new`; `dry` only reports the size. `raw` (`-w`) sends an encrypted dataset
/// as it is, so it stays encrypted on the destination. On a plain dataset `-w` is the same as `-Lec` (compressed, large
/// blocks, embedded data): harmless for a new job, but a different stream type than a plain send.
pub fn send_args(dataset: &str, new: &str, base: Option<&Base>, dry: bool, raw: bool) -> Vec<String> {
    let mut args = vec!["send".to_string()];
    if raw {
        args.push("-w".to_string());
    }
    if dry {
        args.push("-nvP".to_string());
    }
    if let Some(base) = base {
        args.push("-i".to_string());
        args.push(base.reference());
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

/// Whether a resume token belongs to a raw send (`rawok = 1` in `zfs send -nv -t <token>`). A token keeps the flags of the
/// send that made it, so resuming a plain send's token from a raw job would send the rest in the clear.
pub fn token_is_raw(runner: &dyn CommandRunner, token: &str) -> Result<bool> {
    let out = super::run_checked(runner, "zfs", &["send", "-nv", "-t", token], "zfs send -nv -t")?;
    Ok(out.stdout.lines().chain(out.stderr.lines()).any(|l| l.trim().replace(' ', "") == "rawok=1"))
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
    fn the_preview_counts_the_snapshot_the_next_send_adds() {
        let three = vec![info(T1, 1, 10), info(T2, 2, 20), info(T3, 3, 30)];
        assert_eq!(prune_preview(&three, "hourly", 3), vec![T1], "three exist, keep 3: the run adds a fourth, so the oldest goes");
        assert_eq!(prune_preview(&three, "hourly", 1), vec![T1, T2, T3], "keep 1: the new one is the only survivor");
        assert!(prune_preview(&three, "hourly", 4).is_empty());
        assert!(prune_preview(&three, "hourly", 0).is_empty());
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
        assert_eq!(plan(&src, &dst, "hourly"), Plan::Send { new: "hourly-20261006T120000Z".to_string(), base: Some(Base::Snapshot("hourly-20261006T110000Z".to_string())) });
    }

    #[test]
    fn a_source_snapshot_pruned_since_the_last_run_costs_a_longer_incremental_not_a_wrong_one() {
        // 11:00 was pruned from the source; the destination still has 10:00 (guid 1) and 11:00 (guid 2).
        let src = vec![info("hourly-20261006T100000Z", 1, 10), info("hourly-20261006T120000Z", 3, 30)];
        let dst = vec![info("hourly-20261006T100000Z", 1, 5), info("hourly-20261006T110000Z", 2, 6)];
        assert_eq!(plan(&src, &dst, "hourly"), Plan::Send { new: "hourly-20261006T120000Z".to_string(), base: Some(Base::Snapshot("hourly-20261006T100000Z".to_string())) });
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
    fn a_bookmark_is_the_base_when_the_sources_snapshot_is_gone_and_a_snapshot_wins_a_tie() {
        // The source deleted 10:00 (and 11:00); the destination still has them, and the job's bookmark of 11:00 remains.
        let src = vec![info("hourly-20261006T130000Z", 5, 50)];
        let dst = vec![info("hourly-20261006T100000Z", 1, 10), info("hourly-20261006T110000Z", 2, 20)];
        let marks = vec![info("zfsctl-j-hourly-20261006T110000Z", 2, 20)];
        assert_eq!(plan(&src, &dst, "hourly"), Plan::Send { new: "hourly-20261006T130000Z".into(), base: None }, "without the bookmark there is no base");
        assert_eq!(
            plan_with_bookmarks(&src, &dst, &marks, "hourly"),
            Plan::Send { new: "hourly-20261006T130000Z".into(), base: Some(Base::Bookmark { name: "zfsctl-j-hourly-20261006T110000Z".into(), on_destination: "hourly-20261006T110000Z".into() }) }
        );
        // The snapshot itself is still there: it is used, not its bookmark.
        let src = vec![info("hourly-20261006T110000Z", 2, 20), info("hourly-20261006T130000Z", 5, 50)];
        assert_eq!(plan_with_bookmarks(&src, &dst, &marks, "hourly"), Plan::Send { new: "hourly-20261006T130000Z".into(), base: Some(Base::Snapshot("hourly-20261006T110000Z".into())) });
        // A bookmark whose snapshot the destination does not have is no base.
        let stray = vec![info("zfsctl-j-x", 99, 30)];
        assert_eq!(plan_with_bookmarks(&[info("hourly-20261006T130000Z", 5, 50)], &dst, &stray, "hourly"), Plan::Send { new: "hourly-20261006T130000Z".into(), base: None });
    }

    #[test]
    fn a_bookmark_base_is_sent_with_a_hash_and_its_destination_snapshot_is_protected() {
        let base = Base::Bookmark { name: "zfsctl-j-a".into(), on_destination: "a".into() };
        assert_eq!(send_args("tank/data", "b", Some(&base), false, false), ["send", "-i", "#zfsctl-j-a", "--", "tank/data@b"]);
        assert_eq!(base.on_destination(), "a");
    }

    #[test]
    fn only_the_jobs_own_bookmarks_are_listed_and_all_but_the_newest_two_are_pruned() {
        let all = vec![info("zfsctl-j-a", 1, 10), info("zfsctl-j-b", 2, 20), info("zfsctl-j-c", 3, 30), info("mine", 4, 5), info("zfsctl-other-a", 5, 1), info("zfsctl-j-", 6, 2)];
        let own = own_bookmarks(&all, "j");
        assert_eq!(own.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), ["zfsctl-j-a", "zfsctl-j-b", "zfsctl-j-c"]);
        assert_eq!(bookmarks_to_prune(&own), ["zfsctl-j-a"]);
        assert!(bookmarks_to_prune(&own[..2]).is_empty());
        assert_eq!(bookmark_prefix("repl a/b"), "zfsctl-repl_a_b-");
    }

    #[test]
    fn bookmark_listings_are_parsed_by_dataset() {
        let out = "tank/data#zfsctl-j-a\t11\t100\ntank/data/child#x\t12\t101\n";
        assert_eq!(parse_bookmark_infos(out, "tank/data").unwrap(), vec![info("zfsctl-j-a", 11, 100)]);
        assert!(parse_bookmark_infos("garbage\n", "tank/data").is_err());
    }

    #[test]
    fn a_bookmark_that_cannot_be_made_is_a_note_never_an_error_and_an_existing_one_is_not_made_again() {
        use crate::exec::test_support::FakeRunner;
        let snap = info("hourly-20261006T110000Z", 2, 20);
        let failing = FakeRunner::new("").route_failure("zfs", "bookmark", "bookmarks are not supported", 1);
        let note = keep_bookmark(&failing, "tank/data", "j", &snap);
        assert!(note.starts_with("; the bookmark of hourly-20261006T110000Z was not kept"), "{note}");
        let have = FakeRunner::new("tank/data#zfsctl-j-hourly-20261006T110000Z\t2\t20\n");
        assert_eq!(keep_bookmark(&have, "tank/data", "j", &snap), "");
        assert!(have.calls.lock().unwrap().iter().all(|(_, a)| a[0] != "bookmark"), "the bookmark of this snapshot already exists");
    }

    #[test]
    fn the_argument_lists_are_exactly_what_was_verified_live() {
        assert_eq!(send_args("tank/data", "h-2", None, false, false), ["send", "--", "tank/data@h-2"]);
        assert_eq!(send_args("tank/data", "h-2", Some(&Base::Snapshot("h-1".into())), false, false), ["send", "-i", "@h-1", "--", "tank/data@h-2"]);
        assert_eq!(send_args("tank/data", "h-2", Some(&Base::Snapshot("h-1".into())), true, false), ["send", "-nvP", "-i", "@h-1", "--", "tank/data@h-2"]);
        assert_eq!(send_args("tank/data", "h-2", Some(&Base::Snapshot("h-1".into())), false, true), ["send", "-w", "-i", "@h-1", "--", "tank/data@h-2"]);
        assert_eq!(send_args("tank/data", "h-2", None, true, true), ["send", "-w", "-nvP", "--", "tank/data@h-2"]);
        assert_eq!(resume_send_args("1-abc", false), ["send", "-t", "1-abc"]);
        assert_eq!(resume_send_args("1-abc", true), ["send", "-nvP", "-t", "1-abc"]);
        assert_eq!(receive_args("backup/data", false), ["receive", "-s", "-u", "--", "backup/data"]);
        assert_eq!(receive_args("backup/data", true), ["receive", "-s", "-u", "-F", "--", "backup/data"]);
    }

    #[test]
    fn a_resume_token_is_raw_only_when_it_decodes_with_rawok() {
        let raw = FakeRunner::new("resume token contents:\nnvlist version: 0\n\tcompressok = 1\n\trawok = 1\n");
        assert!(token_is_raw(&raw, "1-abc").unwrap());
        assert_eq!(raw.calls.lock().unwrap()[0].1, ["send", "-nv", "-t", "1-abc"]);
        assert!(!token_is_raw(&FakeRunner::new("resume token contents:\nnvlist version: 0\n\tobject = 0x2\n"), "1-abc").unwrap());
        assert!(token_is_raw(&FakeRunner::failing("bad token", 1), "1-abc").is_err());
    }
}
