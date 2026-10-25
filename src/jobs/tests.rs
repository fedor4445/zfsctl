//! Tests of `jobs.rs`.
use super::*;

fn scrub(id: &str) -> Job {
    Job {
        id: id.to_string(),
        host: "local".to_string(),
        kind: KIND_SCRUB.to_string(),
        pool: Some("tank".to_string()),
        schedule: "0 3 1 * *".to_string(),
        catch_up: true,
        enabled: true,
        created_at: None,
        dataset: None,
        recursive: false,
        prefix: None,
        keep: None,
        dest_host: None,
        dest_dataset: None,
        dest_dir: None,
        dest_keep: None, check_days: None,
        remote: None, bwlimit: None, force: false, raw: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None,
    }
}

fn hosts() -> Vec<String> {
    vec!["local".to_string(), "fbsd16".to_string()]
}

#[test]
fn a_missing_file_is_an_empty_job_list() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load(&dir.path().join("jobs.toml")).unwrap().is_empty());
}

#[test]
fn save_then_load_round_trips_and_the_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state").join("jobs.toml");
    let mut job = scrub("scrub-tank");
    job.created_at = Some(DateTime::parse_from_rfc3339("2026-10-06T12:00:00Z").unwrap().with_timezone(&Utc));
    save(&path, &[job.clone(), scrub("scrub-two")]).unwrap();
    assert_eq!(load(&path).unwrap(), vec![job, scrub("scrub-two")]);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(!path.with_extension("toml.tmp").exists(), "the temp file is renamed away");
}

#[test]
fn omitted_flags_default_to_on_and_a_hand_written_file_parses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.toml");
    std::fs::write(&path, "[[jobs]]\nid = \"a\"\nhost = \"local\"\nkind = \"scrub\"\npool = \"tank\"\nschedule = \"0 3 * * 0\"\n").unwrap();
    let jobs = load(&path).unwrap();
    assert!(jobs[0].catch_up && jobs[0].enabled && jobs[0].created_at.is_none());
}

#[test]
fn a_duplicate_id_or_malformed_toml_is_an_error_not_a_partial_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.toml");
    save(&path, &[scrub("same"), scrub("same")]).unwrap();
    assert!(load(&path).unwrap_err().to_string().contains("duplicate job id 'same'"));
    std::fs::write(&path, "not toml [[[").unwrap();
    assert!(load(&path).is_err());
}

#[test]
fn an_unknown_kind_still_parses_but_is_not_runnable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.toml");
    std::fs::write(&path, "[[jobs]]\nid = \"x\"\nhost = \"local\"\nkind = \"restic\"\nschedule = \"0 3 * * *\"\n").unwrap();
    let jobs = load(&path).unwrap();
    assert_eq!(resolve(&jobs[0], &hosts()).unwrap_err(), "unsupported job kind 'restic'");
}

fn file_with(jobs_list: &[Job]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.toml");
    save(&path, jobs_list).unwrap();
    (dir, path)
}

#[test]
fn upsert_adds_a_new_job_and_replaces_one_with_the_same_id_in_place() {
    let (_dir, path) = file_with(&[scrub("a"), scrub("b")]);
    upsert(&path, scrub("c")).unwrap();
    let mut edited = scrub("a");
    edited.schedule = "0 4 * * *".into();
    upsert(&path, edited.clone()).unwrap();
    let ids: Vec<String> = load(&path).unwrap().into_iter().map(|j| j.id).collect();
    assert_eq!(ids, vec!["a", "b", "c"], "an edit keeps the job's place");
    assert_eq!(load(&path).unwrap()[0], edited);
}

#[test]
fn upsert_creates_the_file_when_there_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state").join("jobs.toml");
    upsert(&path, scrub("a")).unwrap();
    assert_eq!(load(&path).unwrap(), vec![scrub("a")]);
}

#[test]
fn remove_and_set_enabled_touch_only_their_job() {
    let (_dir, path) = file_with(&[scrub("a"), scrub("b"), scrub("c")]);
    set_enabled(&path, "b", false).unwrap();
    let after = load(&path).unwrap();
    assert_eq!(after.iter().map(|j| j.enabled).collect::<Vec<_>>(), vec![true, false, true]);
    remove(&path, "a").unwrap();
    assert_eq!(load(&path).unwrap().iter().map(|j| j.id.as_str()).collect::<Vec<_>>(), vec!["b", "c"]);
    assert!(remove(&path, "nope").unwrap_err().to_string().contains("no job 'nope'"));
    assert!(set_enabled(&path, "nope", true).unwrap_err().to_string().contains("no job 'nope'"));
}

#[test]
fn a_change_never_clobbers_a_hand_edit_made_since_the_tui_last_looked() {
    let (_dir, path) = file_with(&[scrub("a")]);
    let mut by_hand = load(&path).unwrap();
    by_hand.push(scrub("typed-by-hand"));
    save(&path, &by_hand).unwrap();
    set_enabled(&path, "a", false).unwrap();
    assert_eq!(load(&path).unwrap().len(), 2, "the hand-added job survived: every change re-reads the file first");
}

#[test]
fn a_file_that_cannot_be_read_is_refused_and_left_exactly_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.toml");
    std::fs::write(&path, "[[jobs]]\nid = \"half-typed\n").unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(upsert(&path, scrub("a")).is_err());
    assert!(remove(&path, "a").is_err());
    assert!(set_enabled(&path, "a", false).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before, "the operator's half-finished edit is not overwritten");
}

#[test]
fn resolve_explains_why_a_job_cannot_run() {
    assert!(resolve(&scrub("ok"), &hosts()).is_ok());
    let mut no_pool = scrub("a");
    no_pool.pool = None;
    assert!(resolve(&no_pool, &hosts()).unwrap_err().contains("needs a pool"));
    let mut no_host = scrub("b");
    no_host.host = "gone".to_string();
    assert!(resolve(&no_host, &hosts()).unwrap_err().contains("'gone' is not in config.toml"));
    let mut bad = scrub("c");
    bad.schedule = "61 * * * *".to_string();
    assert!(resolve(&bad, &hosts()).unwrap_err().starts_with("bad schedule:"));
}

#[test]
fn a_snapshot_job_round_trips_and_a_scrub_job_serializes_exactly_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.toml");
    let mut snap = Job::test_snapshot("s", "tank/data", "hourly", 24);
    snap.recursive = true;
    let plain = scrub("x");
    save(&path, &[plain.clone(), snap.clone()]).unwrap();
    assert_eq!(load(&path).unwrap(), vec![plain.clone(), snap]);
    let text = toml::to_string_pretty(&JobFile { jobs: vec![plain] }).unwrap();
    for absent in ["dataset", "recursive", "prefix", "keep"] {
        assert!(!text.contains(absent), "{absent} leaked into a scrub job:\n{text}");
    }
}

#[test]
fn resolve_accepts_a_good_snapshot_job_and_rejects_each_bad_field() {
    let hosts = vec!["local".to_string()];
    assert!(resolve(&Job::test_snapshot("s", "tank/data", "hourly", 3), &hosts).is_ok());
    let bad = |f: &dyn Fn(&mut Job)| {
        let mut j = Job::test_snapshot("s", "tank/data", "hourly", 3);
        f(&mut j);
        resolve(&j, &hosts).unwrap_err()
    };
    assert!(bad(&|j| j.dataset = None).contains("dataset"));
    assert!(bad(&|j| j.dataset = Some(String::new())).contains("dataset"));
    for evil in ["-rf", "tank/a@b", "tank/a b", "tank/data;x", "/tank", "tank/"] {
        assert!(bad(&|j| j.dataset = Some(evil.to_string())).contains("dataset"), "{evil}");
    }
    assert!(bad(&|j| j.prefix = Some("a b".into())).contains("prefix"));
    assert!(bad(&|j| j.prefix = Some(String::new())).contains("prefix"));
    assert!(bad(&|j| j.keep = Some(0)).contains("keep"));
    assert!(bad(&|j| j.keep = None).contains("keep"));
    assert!(bad(&|j| j.host = "nope".into()).contains("host"));
    assert!(bad(&|j| j.schedule = "x".into()).contains("schedule"));
}

#[test]
fn a_backup_job_without_source_fields_is_a_prefix_job_and_the_new_fields_stay_out_of_the_file() {
    let j = Job::test_backup("b", "tank/data", "hourly", "/r", "/p");
    assert_eq!(backup_source(&j), BackupSource::Prefix("hourly"));
    let text = toml::to_string_pretty(&JobFile { jobs: vec![j] }).unwrap();
    for absent in ["source", "snapshot", "path"] {
        assert!(!text.contains(absent), "{absent} leaked:\n{text}");
    }
}

#[test]
fn backup_source_new_and_pinned_round_trip_and_resolve() {
    let hosts = vec!["local".to_string()];
    let mut fresh = Job::test_backup("b", "tank/data", "hourly", "/r", "/p");
    fresh.source = Some(SOURCE_NEW.into());
    fresh.path = Some("photos/2026".into());
    assert_eq!(backup_source(&fresh), BackupSource::Fresh);
    assert!(resolve(&fresh, &hosts).is_ok());
    let mut pinned = Job::test_backup("c", "tank/data", "hourly", "/r", "/p");
    pinned.source = Some(SOURCE_SNAPSHOT.into());
    pinned.snapshot = Some("snap4".into());
    assert_eq!(backup_source(&pinned), BackupSource::Pinned("snap4"));
    assert!(resolve(&pinned, &hosts).is_ok());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jobs.toml");
    save(&file, &[fresh.clone(), pinned.clone()]).unwrap();
    assert_eq!(load(&file).unwrap(), vec![fresh, pinned]);
}

#[test]
fn resolve_rejects_each_bad_backup_source_field() {
    let hosts = vec!["local".to_string()];
    let bad = |f: &dyn Fn(&mut Job)| {
        let mut j = Job::test_backup("b", "tank/data", "hourly", "/r", "/p");
        f(&mut j);
        resolve(&j, &hosts).unwrap_err()
    };
    assert!(bad(&|j| j.source = Some("weekly".into())).contains("unknown backup source"));
    assert!(bad(&|j| j.source = Some(SOURCE_SNAPSHOT.into())).contains("snapshot name"));
    for evil in ["", "-x", "a@b", "a b", "a/b", "a;b"] {
        assert!(bad(&|j| { j.source = Some(SOURCE_SNAPSHOT.into()); j.snapshot = Some(evil.into()); }).contains("snapshot name"), "{evil:?}");
    }
    assert!(bad(&|j| { j.source = Some(SOURCE_NEW.into()); j.keep = Some(0); }).contains("keep"));
    for evil in ["/etc", "../x", "a/../b", "a//b", "./a", "-x", "a b", "a;b", ""] {
        assert!(bad(&|j| j.path = Some(evil.into())).contains("path"), "{evil:?}");
    }
    // A pinned or new source does not care about the prefix, a prefix source does.
    let mut j = Job::test_backup("b", "tank/data", "bad prefix", "/r", "/p");
    assert!(resolve(&j, &hosts).unwrap_err().contains("prefix"));
    j.source = Some(SOURCE_NEW.into());
    assert!(resolve(&j, &hosts).is_ok());
}

#[test]
fn a_fresh_backup_prefix_is_a_valid_prefix_that_carries_the_job_id() {
    assert_eq!(fresh_prefix("backup-local-tank-data"), "zit-backup-local-tank-data");
    assert!(valid_prefix(&fresh_prefix("backup-local-tank-data")));
}

#[test]
fn a_missing_prefix_means_auto() {
    let mut j = Job::test_snapshot("s", "tank/data", "hourly", 3);
    j.prefix = None;
    assert_eq!(effective_prefix(&j), "auto");
    assert!(resolve(&j, &["local".to_string()]).is_ok());
}

#[test]
fn two_enabled_snapshot_jobs_on_one_host_dataset_and_prefix_clash_but_other_combinations_do_not() {
    let a = Job::test_snapshot("a", "tank/data", "hourly", 3);
    let b = Job::test_snapshot("b", "tank/data", "hourly", 5);
    let other_prefix = Job::test_snapshot("c", "tank/data", "daily", 5);
    let other_dataset = Job::test_snapshot("d", "tank/other", "hourly", 5);
    let mut disabled = Job::test_snapshot("e", "tank/data", "hourly", 5);
    disabled.enabled = false;
    let all = vec![a.clone(), b.clone(), other_prefix, other_dataset, disabled];
    let reasons = duplicate_prefix_reasons(&all);
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons["b"].contains("'a'"), "{reasons:?}");
    assert_eq!(clashing_job(&all, &b).map(|j| j.id.as_str()), Some("a"));
    let mut edited = a.clone();
    edited.keep = Some(9);
    assert!(clashing_job(&[a], &edited).is_none(), "a job never clashes with itself");
}

#[test]
fn a_snapshot_job_with_the_prefix_a_backup_or_mirror_uses_for_its_own_snapshots_clashes_with_it() {
    let mut backup = Job::test_backup("b1", "tank/data", "x", "/repo", "/pw");
    backup.source = Some(SOURCE_NEW.into());
    let mut mirror = Job::test_mirror("m1", "tank/data", "x", "b2:bucket/photos");
    mirror.source = Some(SOURCE_NEW.into());
    for fresh in [&backup, &mirror] {
        let same = Job::test_snapshot("s", "tank/data", &fresh_prefix(&fresh.id), 5);
        assert_eq!(clashing_job(std::slice::from_ref(fresh), &same).map(|j| j.id.as_str()), Some(fresh.id.as_str()), "{}", fresh.id);
        assert_eq!(clashing_job(std::slice::from_ref(&same), fresh).map(|j| j.id.as_str()), Some("s"), "both ways: {}", fresh.id);
        let reason = clash_reason(fresh, &same).expect("a reason");
        assert!(reason.contains(&format!("'{}'", fresh.id)), "{reason}");
        // not the same prefix, not the same dataset, a disabled job and another host do not clash
        assert!(clashing_job(std::slice::from_ref(fresh), &Job::test_snapshot("s", "tank/data", "hourly", 5)).is_none());
        assert!(clashing_job(std::slice::from_ref(fresh), &Job::test_snapshot("s", "tank/other", &fresh_prefix(&fresh.id), 5)).is_none());
        let mut off = same.clone();
        off.enabled = false;
        assert!(clashing_job(std::slice::from_ref(fresh), &off).is_none());
        let mut elsewhere = same.clone();
        elsewhere.host = "fbsd16".into();
        assert!(clashing_job(std::slice::from_ref(fresh), &elsewhere).is_none());
        // a recursive snapshot job on the parent also prunes the descendant's `zit-` snapshots
        let mut parent = Job::test_snapshot("p", "tank", &fresh_prefix(&fresh.id), 5);
        parent.recursive = true;
        assert!(clashing_job(std::slice::from_ref(fresh), &parent).is_some());
    }
    // a backup that reads the newest `<prefix>-` snapshots takes none of its own
    let reading = Job::test_backup("b2", "tank/data", "hourly", "/repo", "/pw");
    assert!(clashing_job(std::slice::from_ref(&reading), &Job::test_snapshot("s", "tank/data", "zit-b2", 5)).is_none());
}

#[test]
fn a_recursive_job_clashes_with_a_job_on_a_descendant_that_shares_its_prefix() {
    let mut parent = Job::test_snapshot("p", "tank", "auto", 7);
    parent.recursive = true;
    let child = Job::test_snapshot("c", "tank/data", "auto", 24);
    assert_eq!(clashing_job(&[parent.clone()], &child).map(|j| j.id.as_str()), Some("p"));
    assert_eq!(clashing_job(std::slice::from_ref(&child), &parent).map(|j| j.id.as_str()), Some("c"));
    assert!(duplicate_prefix_reasons(&[parent.clone(), child.clone()]).contains_key("c"));
    // A different prefix, a non-recursive parent, a sibling, and a dataset that only shares a name prefix do not clash.
    let mut other_prefix = child.clone();
    other_prefix.prefix = Some("hourly".into());
    assert!(clashing_job(&[parent.clone()], &other_prefix).is_none());
    let flat_parent = Job::test_snapshot("fp", "tank", "auto", 7);
    assert!(clashing_job(&[flat_parent], &child).is_none());
    let sibling = Job::test_snapshot("s", "tank/other", "auto", 7);
    assert!(clashing_job(std::slice::from_ref(&child), &sibling).is_none());
    drop(parent);
    let mut parent2 = Job::test_snapshot("p2", "tank/data", "auto", 7);
    parent2.recursive = true;
    assert!(clashing_job(&[parent2], &Job::test_snapshot("l2", "tank/database", "auto", 7)).is_none(), "tank/database is not under tank/data");
}

#[test]
fn a_job_flagged_as_clashing_does_not_make_later_jobs_clash_with_it() {
    let a = Job::test_snapshot("a", "tank/data", "hourly", 1);
    let b = Job::test_snapshot("b", "tank/data", "hourly", 1);
    let c = Job::test_snapshot("c", "tank/data", "hourly", 1);
    let reasons = duplicate_prefix_reasons(&[a, b, c]);
    assert!(reasons["b"].contains("'a'") && reasons["c"].contains("'a'"), "{reasons:?}");
}

#[test]
fn a_replicate_job_round_trips_and_other_kinds_serialize_without_its_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.toml");
    let mut repl = Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data");
    repl.force = true;
    let snap = Job::test_snapshot("s", "tank/data", "hourly", 3);
    save(&path, &[repl.clone(), snap.clone()]).unwrap();
    assert_eq!(load(&path).unwrap(), vec![repl, snap.clone()]);
    let text = toml::to_string_pretty(&JobFile { jobs: vec![snap, scrub("x")] }).unwrap();
    for absent in ["dest_host", "dest_dataset", "dest_dir", "dest_keep", "remote", "bwlimit", "force"] {
        assert!(!text.contains(absent), "{absent} leaked:\n{text}");
    }
}

#[test]
fn resolve_accepts_a_good_replicate_job_and_rejects_each_bad_field() {
    let hosts = vec!["local".to_string(), "fbsd16".to_string()];
    assert!(resolve(&Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data"), &hosts).is_ok());
    let bad = |f: &dyn Fn(&mut Job)| {
        let mut j = Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data");
        f(&mut j);
        resolve(&j, &hosts).unwrap_err()
    };
    assert!(bad(&|j| j.dataset = None).contains("source dataset"));
    for evil in ["-rf", "tank/a@b", "tank/a b", "tank;x", "/tank"] {
        assert!(bad(&|j| j.dataset = Some(evil.into())).contains("source dataset"), "{evil}");
        assert!(bad(&|j| j.dest_dataset = Some(evil.into())).contains("destination dataset"), "{evil}");
    }
    assert!(bad(&|j| j.prefix = Some("a b".into())).contains("prefix"));
    assert!(bad(&|j| j.dest_host = None).contains("destination host"));
    assert!(bad(&|j| j.dest_host = Some("nope".into())).contains("host"));
    assert!(bad(&|j| j.dest_dataset = None).contains("destination dataset"));
    assert!(bad(&|j| j.dest_dir = Some("/mnt/x".into())).contains("dest_dir"));
    assert!(bad(&|j| { j.host = "fbsd16".into(); j.dest_dataset = j.dataset.clone(); }).contains("same dataset"), "source and destination must differ");
    assert!(bad(&|j| j.schedule = "x".into()).contains("schedule"));
}

fn mirror() -> Job {
    Job::test_mirror("m", "tank/data", "hourly", "aws:bkt/photos")
}

#[test]
fn a_good_mirror_job_resolves() {
    assert!(resolve(&mirror(), &hosts()).is_ok());
    let mut limited = mirror();
    limited.bwlimit = Some("5M".into());
    assert!(resolve(&limited, &hosts()).is_ok());
}

#[test]
fn a_mirror_job_needs_a_dataset_and_a_valid_remote() {
    let bad = |f: &dyn Fn(&mut Job)| {
        let mut j = mirror();
        f(&mut j);
        resolve(&j, &hosts()).unwrap_err()
    };
    assert!(bad(&|j| j.dataset = None).contains("dataset"));
    assert!(bad(&|j| j.remote = None).contains("remote"));
    for evil in [":s3,access_key_id=X:b/p", "-x:b", "a,b:c", "nocolon", "aws:b\nx", "", "aws:", "aws:b/../c"] {
        assert!(bad(&|j| j.remote = Some(evil.into())).contains("remote"), "{evil:?}");
    }
}

#[test]
fn bwlimit_is_validated_and_optional() {
    let mut j = mirror();
    for evil in ["5MB", "-1", "1.5M", ""] {
        j.bwlimit = Some(evil.into());
        assert!(resolve(&j, &hosts()).unwrap_err().contains("speed limit"), "{evil:?}");
    }
}

#[test]
fn the_source_rules_match_the_backup_job() {
    let bad = |f: &dyn Fn(&mut Job)| {
        let mut j = mirror();
        f(&mut j);
        resolve(&j, &hosts()).unwrap_err()
    };
    assert!(bad(&|j| { j.source = Some(SOURCE_NEW.into()); j.keep = Some(0); }).contains("keep"));
    assert!(bad(&|j| j.source = Some(SOURCE_SNAPSHOT.into())).contains("snapshot name"));
    assert!(bad(&|j| j.source = Some("weird".into())).contains("unknown cloud mirror source"));
    assert!(bad(&|j| j.path = Some("../x".into())).contains("path"));
    assert!(bad(&|j| j.id = "a b".into()).contains("id"));
}

#[test]
fn remote_and_bwlimit_are_rejected_on_other_kinds() {
    for mut other in [Job::test_snapshot("s", "tank/data", "hourly", 3), scrub("x"), Job::test_backup("b", "tank/data", "hourly", "/r", "/p"), Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data")] {
        other.remote = Some("aws:b".into());
        assert!(resolve(&other, &hosts()).unwrap_err().contains("only for a cloud mirror"), "{}", other.kind);
        other.remote = None;
        other.bwlimit = Some("5M".into());
        assert!(resolve(&other, &hosts()).unwrap_err().contains("only for a cloud mirror"), "{}", other.kind);
    }
}

#[test]
fn mirror_jobs_with_overlapping_remotes_clash_on_any_host() {
    let a = Job::test_mirror("a", "tank/one", "hourly", "aws:bkt/p");
    for (other, clashes) in [("aws:bkt/p", true), ("aws:bkt/p/q", true), ("aws:bkt", true), ("aws:bkt/pq", false), ("b2:bkt/p", false)] {
        let b = Job::test_mirror("b", "tank/two", "hourly", other);
        assert_eq!(clashing_job(std::slice::from_ref(&a), &b).is_some(), clashes, "{other}");
    }
    let mut elsewhere = Job::test_mirror("b", "tank/two", "hourly", "aws:bkt/p");
    elsewhere.host = "fbsd16".into();
    assert!(clashing_job(std::slice::from_ref(&a), &elsewhere).is_some(), "the same remote name and path on another host is the same bucket");
    elsewhere.remote = Some("aws2:bkt/p".into());
    assert!(clashing_job(std::slice::from_ref(&a), &elsewhere).is_none(), "a different remote name is a different place");
    let mut off = a.clone();
    off.enabled = false;
    assert!(clashing_job(&[off], &Job::test_mirror("b", "tank/two", "hourly", "aws:bkt/p")).is_none(), "disabled jobs never clash");
}

#[test]
fn a_mirror_whose_remote_holds_a_restic_jobs_rclone_repository_clashes_both_ways() {
    let restic = Job::test_backup("b", "tank/data", "hourly", "rclone:aws:bkt/x/current", "/pw");
    let m = Job::test_mirror("m", "tank/data", "hourly", "aws:bkt/x");
    assert!(clashing_job(std::slice::from_ref(&restic), &m).is_some());
    assert!(clashing_job(std::slice::from_ref(&m), &restic).is_some());
    let apart = Job::test_mirror("m", "tank/data", "hourly", "aws:bkt/y");
    assert!(clashing_job(std::slice::from_ref(&restic), &apart).is_none());
    let mut elsewhere = m.clone();
    elsewhere.host = "fbsd16".into();
    assert!(clashing_job(&[restic], &elsewhere).is_some(), "a restic repository on another host counts too");
}

#[test]
fn mirror_is_cancellable_and_the_new_keys_round_trip() {
    assert!(is_cancellable(&mirror()));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.toml");
    let mut m = mirror();
    m.bwlimit = Some("5M".into());
    save(&path, &[m.clone()]).unwrap();
    assert_eq!(load(&path).unwrap(), vec![m]);
}

#[test]
fn dest_keep_of_zero_is_rejected() {
    let mut j = Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data");
    j.dest_keep = Some(0);
    assert!(resolve(&j, &hosts()).unwrap_err().contains("dest_keep"));
}

#[test]
fn dest_keep_is_accepted_on_a_host_target_replication_and_nowhere_else() {
    let mut j = Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data");
    j.dest_keep = Some(3);
    assert!(resolve(&j, &hosts()).is_ok());
    let mut file = Job::test_replicate("r", "tank/data", "hourly", "file", "x");
    file.dest_dataset = None;
    file.dest_dir = Some("/var/streams".into());
    file.dest_keep = Some(3);
    assert!(resolve(&file, &["local".to_string()]).unwrap_err().contains("dest_keep"));
    for mut other in [Job::test_snapshot("s", "tank/data", "hourly", 3), scrub("x"), Job::test_backup("b", "tank/data", "hourly", "/r", "/p")] {
        other.dest_keep = Some(3);
        assert!(resolve(&other, &hosts()).unwrap_err().contains("dest_keep"), "{}", other.kind);
    }
}

#[test]
fn dest_keep_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.toml");
    let mut repl = Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data");
    repl.dest_keep = Some(5);
    save(&path, &[repl.clone()]).unwrap();
    assert_eq!(load(&path).unwrap(), vec![repl]);
}

#[test]
fn a_file_destination_is_accepted_and_still_needs_a_plain_absolute_directory() {
    let mut j = Job::test_replicate("r", "tank/data", "hourly", "file", "x");
    j.dest_dataset = None;
    j.dest_dir = Some("/mnt/usb/zfsctl".into());
    assert!(resolve(&j, &["local".to_string()]).is_ok());
    j.dest_dir = Some("relative/dir".into());
    assert!(resolve(&j, &["local".to_string()]).unwrap_err().contains("dest_dir"));
    j.dest_dir = Some("/a/../etc".into());
    assert!(resolve(&j, &["local".to_string()]).is_err());
}

#[test]
fn dest_dir_must_be_a_plain_absolute_path() {
    for good in ["/mnt/usb", "/mnt/usb/zfsctl", "/var/backups/my disk"] {
        assert!(valid_dest_dir(good), "{good}");
    }
    for bad in ["", "/", "mnt/usb", "-x", "/mnt/../etc", "/mnt/..", "/mnt/a;b", "/mnt/a\nb", "/mnt/$x", "/mnt/a\\b", "//", "/.", "/mnt//usb", "/mnt/./usb", "/mnt/usb/", "/mnt/usb/."] {
        assert!(!valid_dest_dir(bad), "{bad:?}");
    }
}

#[test]
fn two_enabled_replicate_jobs_with_one_destination_clash_and_other_destinations_do_not() {
    let a = Job::test_replicate("a", "tank/one", "hourly", "fbsd16", "backup/data");
    let b = Job::test_replicate("b", "tank/two", "hourly", "fbsd16", "backup/data");
    let other_dataset = Job::test_replicate("c", "tank/two", "hourly", "fbsd16", "backup/other");
    let other_host = Job::test_replicate("d", "tank/two", "hourly", "local", "backup/data");
    let reasons = duplicate_prefix_reasons(&[a.clone(), b.clone(), other_dataset.clone(), other_host.clone()]);
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons["b"].contains("same destination") && reasons["b"].contains("'a'"), "{reasons:?}");
    assert_eq!(clashing_job(std::slice::from_ref(&a), &b).map(|j| j.id.as_str()), Some("a"));
    assert!(clashing_job(std::slice::from_ref(&a), &other_dataset).is_none());
    assert!(clash_reason(&a, &b).unwrap().contains("same destination"));
    let snap = Job::test_snapshot("s", "tank/one", "hourly", 3);
    assert!(clash_reason(&a, &snap).is_none(), "different kinds never clash");
}

#[test]
fn a_backup_job_round_trips_and_other_kinds_serialize_without_its_fields() {
    let mut job = Job::test_backup("b", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
    job.retention = Some("7d 4w".into());
    let text = toml::to_string_pretty(&JobFile { jobs: vec![job.clone()] }).unwrap();
    let back: JobFile = toml::from_str(&text).unwrap();
    assert_eq!(back.jobs, vec![job]);
    let scrub = toml::to_string_pretty(&JobFile { jobs: vec![Job::test_snapshot("s", "tank/a", "auto", 3)] }).unwrap();
    for absent in ["repository", "password_file", "retention"] {
        assert!(!scrub.contains(absent), "{absent} leaked into a snapshot job:\n{scrub}");
    }
}

#[test]
fn resolve_accepts_a_good_backup_job_and_rejects_each_bad_field() {
    let hosts = vec!["local".to_string()];
    let good = Job::test_backup("backup-local-tank-data", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
    assert!(resolve(&good, &hosts).is_ok());
    let bad = |change: &dyn Fn(&mut Job)| {
        let mut j = good.clone();
        change(&mut j);
        resolve(&j, &hosts).unwrap_err()
    };
    assert!(bad(&|j| j.dataset = Some("-rf".into())).contains("dataset"));
    assert!(bad(&|j| j.dataset = None).contains("dataset"));
    assert!(bad(&|j| j.prefix = Some("a b".into())).contains("prefix"));
    assert!(bad(&|j| j.id = "a b".into()).contains("id"));
    assert!(bad(&|j| j.id = "../x".into()).contains("id"));
    assert!(bad(&|j| j.repository = None).contains("repository"));
    assert!(bad(&|j| j.repository = Some("-r".into())).contains("repository"));
    assert!(bad(&|j| j.password_file = None).contains("password_file"));
    assert!(bad(&|j| j.password_file = Some("relative".into())).contains("password_file"));
    assert!(bad(&|j| j.retention = Some("7x".into())).contains("retention"));
    assert!(bad(&|j| j.host = "nope".into()).contains("host"));
    assert!(bad(&|j| j.schedule = "x".into()).contains("schedule"));
    let mut blank = good.clone();
    blank.retention = Some("  ".into());
    assert!(resolve(&blank, &hosts).is_ok(), "blank retention keeps everything");
}

#[test]
fn only_replication_and_backup_runs_can_be_cancelled() {
    assert!(is_cancellable(&Job::test_backup("b", "tank/a", "auto", "/r", "/pw")));
    assert!(is_cancellable(&Job::test_replicate("r", "tank/a", "auto", "fbsd16", "backup/a")));
    assert!(!is_cancellable(&Job::test_snapshot("s", "tank/a", "auto", 3)));
}

#[test]
fn two_backup_jobs_for_one_dataset_do_not_clash() {
    let a = Job::test_backup("a", "tank/data", "hourly", "/mnt/one", "/pw");
    let b = Job::test_backup("b", "tank/data", "hourly", "/mnt/two", "/pw");
    assert!(clashing_job(&[a], &b).is_none());
}


#[test]
fn dataset_names_with_empty_dot_or_dotdot_components_are_refused() {
    for good in ["tank", "tank/data", "tank/data-1/sub.dir", "zroot/ROOT/default", "a:b/c_d"] {
        assert!(valid_dataset(good), "{good}");
    }
    for bad in ["", "-x", "/tank", "tank/", "../x", "tank//x", "tank/./x", "tank/../x", "tank/data/..", ".", "..", "tank/a b", "tank/a@b"] {
        assert!(!valid_dataset(bad), "{bad:?}");
    }
}

#[test]
fn raw_is_omitted_when_false_round_trips_when_true_and_is_only_for_replication() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.toml");
    let mut raw = Job::test_replicate("r", "tank/s", "p", "hub", "backup/s");
    raw.raw = true;
    let plain = Job::test_replicate("q", "tank/t", "p", "hub", "backup/t");
    upsert(&path, raw).unwrap();
    upsert(&path, plain).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("raw = true").count(), 1, "{text}");
    assert!(!text.contains("raw = false"), "{text}");
    let loaded = load(&path).unwrap();
    assert!(loaded.iter().find(|j| j.id == "r").unwrap().raw && !loaded.iter().find(|j| j.id == "q").unwrap().raw);
    // A file written before the field existed loads with raw off.
    std::fs::write(&path, text.replace("raw = true\n", "")).unwrap();
    assert!(load(&path).unwrap().iter().all(|j| !j.raw));
    let hosts = vec!["local".to_string(), "hub".to_string()];
    let mut other = scrub("s");
    other.raw = true;
    assert!(resolve(&other, &hosts).unwrap_err().contains("raw is only for a replication"));
    let mut ok = Job::test_replicate("r", "tank/s", "p", "hub", "backup/s");
    ok.raw = true;
    assert!(resolve(&ok, &hosts).is_ok());
}
