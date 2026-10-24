//! The job store: `jobs.toml`, written by the TUI and read by the daemon.
//!
//! `kind` is a plain string, not an enum, so a `jobs.toml` written by a
//! newer zfsctl (with job kinds this build does not know) still parses; such
//! a job is reported as unsupported and never run.

use crate::daemon::cron::Schedule;
use crate::error::{AppError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub const KIND_SCRUB: &str = "scrub";
pub const KIND_SNAPSHOT: &str = "snapshot";
pub const KIND_REPLICATE: &str = "replicate";
/// A cloud mirror: the dataset copied to an rclone remote as plain files.
pub const KIND_MIRROR: &str = "rclone";
pub const KIND_BACKUP: &str = "backup";
/// `dest_host` value meaning "write stream files to `dest_dir` on the hub".
pub const DEST_FILE: &str = "file";
/// The snapshot name prefix a job without one uses.
pub const DEFAULT_PREFIX: &str = "auto";

fn yes() -> bool {
    true
}

// The bools (`recursive`, `force`, `raw`, `catch_up`, `enabled`) are independent job flags in `jobs.toml`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    /// A host name from `config.toml`; `local` is the hub itself.
    pub host: String,
    pub kind: String,
    /// The pool a scrub job runs on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// The dataset a snapshot job snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset: Option<String>,
    /// `zfs snapshot -r`, and pruning across the whole tree.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recursive: bool,
    /// The snapshot name prefix; only snapshots named `<prefix>-<timestamp>` are ever pruned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// How many of the job's own snapshots to keep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<u32>,
    /// A replication job's destination: a configured host name, or `file`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_host: Option<String>,
    /// The destination dataset on `dest_host`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_dataset: Option<String>,
    /// For `dest_host = "file"`: an absolute directory on the hub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_dir: Option<String>,
    /// A host-target replication job's retention on the destination: keep this many of the job's own
    /// snapshots there. Absent keeps everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_keep: Option<u32>,
    /// Receive with `-F`, rolling the destination back to the incremental base.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
    /// Replication only: send with `-w`, so an encrypted source stays encrypted (and locked) on the destination.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub raw: bool,
    /// A backup job's restic repository (`restic -r`). It must not contain credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// A backup job's restic password file: an absolute path on the job's host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_file: Option<String>,
    /// A cloud mirror job's rclone remote such as `aws:bucket/photos`. It must not contain credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// A cloud mirror job's speed limit (`5M`, bytes per second); absent is unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bwlimit: Option<String>,
    /// A backup job's retention such as `7d 4w 12m 3y`; absent or blank keeps every backup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<String>,
    /// Which snapshot a backup job backs up: `new` (take a fresh one each run), `snapshot` (the one named
    /// in `snapshot`), or absent / `prefix` (the newest `<prefix>-<timestamp>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// For `source = "snapshot"`: the snapshot's short name (the part after `@`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    /// A backup job's directory inside the dataset, relative to its root; absent backs up the whole dataset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// A five-field cron expression, in the hub's local time zone.
    pub schedule: String,
    #[serde(default = "yes")]
    pub catch_up: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// When the job was created. A catch-up never reaches back before this,
    /// so a new job does not fire for a slot that passed before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct JobFile {
    #[serde(default)]
    jobs: Vec<Job>,
}

/// Reads `jobs.toml`. A missing file is an empty job list. A parse error or a
/// duplicate id is an error, so the daemon can keep the job set it already has.
pub fn load(path: &Path) -> Result<Vec<Job>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).map_err(|e| AppError::Config(format!("reading {}: {e}", path.display())))?;
    let file: JobFile = toml::from_str(&text).map_err(|e| AppError::Config(format!("parsing {}: {e}", path.display())))?;
    let mut seen = std::collections::HashSet::new();
    for job in &file.jobs {
        if !seen.insert(job.id.as_str()) {
            return Err(AppError::Config(format!("{}: duplicate job id '{}'", path.display(), job.id)));
        }
    }
    Ok(file.jobs)
}

// The daemon only reads jobs; the TUI Jobs section (a later plan) is the writer.
#[cfg_attr(not(test), allow(dead_code))]
/// Writes `jobs.toml` atomically with mode 0600: a temp file in the same
/// directory is written and synced, then renamed over the real one, so a
/// reader (or a power cut) never sees a half-written file.
pub fn save(path: &Path, jobs: &[Job]) -> Result<()> {
    let text = toml::to_string_pretty(&JobFile { jobs: jobs.to_vec() }).map_err(|e| AppError::Config(format!("serializing jobs: {e}")))?;
    let io = |what: &str, e: std::io::Error| AppError::Config(format!("{what} {}: {e}", path.display()));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io("creating the directory of", e))?;
    }
    let tmp = path.with_extension("toml.tmp");
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).map_err(|e| io("writing", e))?;
    file.write_all(text.as_bytes()).map_err(|e| io("writing", e))?;
    file.sync_all().map_err(|e| io("syncing", e))?;
    std::fs::rename(&tmp, path).map_err(|e| io("replacing", e))
}

/// Whether `s` can be a dataset name for a job: ZFS name characters only, no `@`, no leading
/// `-` or `/`, no trailing `/`. Checked here so a hand-edited `jobs.toml` can never put an
/// option or a shell-looking string on a `zfs` command line.
pub fn valid_dataset(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(['-', '/'])
        && !s.ends_with('/')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.' | '/'))
        // No empty, `.` or `..` component: `tank//x`, `tank/./x` and `../x` are not dataset names.
        && s.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

pub fn valid_prefix(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

pub const SOURCE_NEW: &str = "new";
pub const SOURCE_SNAPSHOT: &str = "snapshot";
pub const SOURCE_PREFIX: &str = "prefix";
/// How many of its own snapshots a `new`-source backup job keeps when `keep` is not set.
pub const DEFAULT_FRESH_KEEP: u32 = 3;

/// Which snapshot a backup job backs up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupSource<'a> {
    /// Take a snapshot at the start of every run.
    Fresh,
    /// Always this snapshot (the short name).
    Pinned(&'a str),
    /// The newest `<prefix>-<timestamp>` snapshot.
    Prefix(&'a str),
}

/// A backup job's source. An unknown `source` reads as the prefix mode here; `resolve` refuses it.
pub fn backup_source(job: &Job) -> BackupSource<'_> {
    match job.source.as_deref() {
        Some(SOURCE_NEW) => BackupSource::Fresh,
        Some(SOURCE_SNAPSHOT) => BackupSource::Pinned(job.snapshot.as_deref().unwrap_or("")),
        _ => BackupSource::Prefix(effective_prefix(job)),
    }
}

/// The prefix of the snapshots a `new`-source backup job takes. It carries the job id, so no other job's
/// snapshot can ever be mistaken for one of its own.
pub fn fresh_prefix(job_id: &str) -> String {
    format!("zit-{job_id}")
}

/// Whether `s` can be the short name of an existing snapshot: the characters ZFS allows that are safe on a
/// command line, no `@`, no leading `-`.
pub fn valid_snapshot_name(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

/// Whether `s` is a directory inside a dataset: relative, no empty, `.` or `..` component, plain characters only.
pub fn valid_subpath(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(['/', '-'])
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
        && s.trim_end_matches('/').split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

pub fn effective_prefix(job: &Job) -> &str {
    job.prefix.as_deref().unwrap_or(DEFAULT_PREFIX)
}

/// An absolute hub directory in canonical form (no empty, `.` or `..` component, no trailing slash, so
/// two spellings of one directory cannot dodge the shared-destination rule) and only plain path characters.
pub fn valid_dest_dir(s: &str) -> bool {
    s.len() > 1 && s.starts_with('/') && s.split('/').skip(1).all(|c| !c.is_empty() && c != "." && c != "..") && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ' '))
}

fn check_replicate_fields(job: &Job) -> std::result::Result<(), String> {
    if !job.dataset.as_deref().is_some_and(valid_dataset) {
        return Err("a replication job needs a source dataset like tank/data".to_string());
    }
    if !valid_prefix(effective_prefix(job)) {
        return Err("the snapshot prefix may only use letters, digits, _ - and .".to_string());
    }
    match job.dest_host.as_deref() {
        None | Some("") => Err("a replication job needs a destination host (or 'file')".to_string()),
        Some(DEST_FILE) => {
            if job.dest_dataset.is_some() {
                return Err("a file destination takes a dest_dir, not a destination dataset".to_string());
            }
            if !job.dest_dir.as_deref().is_some_and(valid_dest_dir) {
                return Err("a file destination needs an absolute dest_dir on the hub".to_string());
            }
            Ok(())
        }
        Some(_) => {
            if !job.dest_dataset.as_deref().is_some_and(valid_dataset) {
                return Err("a replication job needs a destination dataset like backup/data".to_string());
            }
            if job.dest_dir.is_some() {
                return Err("dest_dir is only for a file destination".to_string());
            }
            if job.dest_host.as_deref() == Some(job.host.as_str()) && job.dest_dataset == job.dataset {
                return Err("the source and destination are the same dataset".to_string());
            }
            Ok(())
        }
    }
}

/// What a backup and a cloud mirror job share: the dataset, which snapshot is read, the folder inside it and the id
/// (it becomes a dataset name and a mountpoint). `noun` is how a message names the job kind.
fn check_source_fields(job: &Job, noun: &str) -> std::result::Result<(), String> {
    if !job.dataset.as_deref().is_some_and(valid_dataset) {
        return Err(format!("a {noun} job needs a dataset like tank/data"));
    }
    match job.source.as_deref() {
        None | Some(SOURCE_PREFIX) => {
            if !valid_prefix(effective_prefix(job)) {
                return Err("the snapshot prefix may only use letters, digits, _ - and .".to_string());
            }
        }
        Some(SOURCE_NEW) => {
            if job.keep == Some(0) {
                return Err(format!("a {noun} job that takes its own snapshots needs keep of at least 1"));
            }
        }
        Some(SOURCE_SNAPSHOT) => {
            if !job.snapshot.as_deref().is_some_and(valid_snapshot_name) {
                return Err("the snapshot name may only use letters, digits, _ - . and :".to_string());
            }
        }
        Some(other) => return Err(format!("unknown {noun} source '{other}'")),
    }
    if let Some(path) = job.path.as_deref() {
        if !valid_subpath(path) {
            return Err("the path must be a directory inside the dataset, like photos/2026 (letters, digits, _ - . /; no .. and no leading /)".to_string());
        }
    }
    // The id becomes a dataset name, a mountpoint and restic tags.
    if !valid_prefix(&job.id) {
        return Err(format!("a {noun} job's id may only use letters, digits, _ - and ."));
    }
    Ok(())
}

fn check_mirror_fields(job: &Job) -> std::result::Result<(), String> {
    check_source_fields(job, "cloud mirror")?;
    if !job.remote.as_deref().is_some_and(crate::rclone::valid_remote) {
        return Err("a cloud mirror job needs a remote like aws:mybucket/photos (a configured rclone remote name, a colon and a bucket or folder, with no keys in it and no . or .. parts)".to_string());
    }
    if job.bwlimit.as_deref().is_some_and(|l| !crate::rclone::valid_bwlimit(l)) {
        return Err("the speed limit is a number of bytes per second with an optional K, M or G, like 5M".to_string());
    }
    Ok(())
}

fn check_backup_fields(job: &Job) -> std::result::Result<(), String> {
    check_source_fields(job, "backup")?;
    if !job.repository.as_deref().is_some_and(crate::restic::valid_repository) {
        return Err("a backup job needs a repository (restic -r) that does not start with '-'".to_string());
    }
    if !job.password_file.as_deref().is_some_and(crate::restic::valid_password_file) {
        return Err("a backup job needs password_file: an absolute path on the host".to_string());
    }
    if let Some(text) = job.retention.as_deref() {
        crate::restic::parse_retention(text).map_err(|e| format!("retention: {e}"))?;
    }
    Ok(())
}

/// Whether a running run of `job` can be cancelled from the TUI (it has a long-lived child process).
pub fn is_cancellable(job: &Job) -> bool {
    job.kind == KIND_REPLICATE || job.kind == KIND_BACKUP || job.kind == KIND_MIRROR
}

fn check_snapshot_fields(job: &Job) -> std::result::Result<(), String> {
    if !job.dataset.as_deref().is_some_and(valid_dataset) {
        return Err("a snapshot job needs a dataset like tank/data".to_string());
    }
    if !valid_prefix(effective_prefix(job)) {
        return Err("the snapshot prefix may only use letters, digits, _ - and .".to_string());
    }
    if job.keep.is_none_or(|k| k == 0) {
        return Err("a snapshot job needs keep of at least 1".to_string());
    }
    Ok(())
}

/// Whether the daemon can run `job`: its schedule if so, or the reason it cannot.
pub fn resolve(job: &Job, known_hosts: &[String]) -> std::result::Result<Schedule, String> {
    match job.kind.as_str() {
        KIND_SCRUB => {
            if job.pool.as_deref().is_none_or(str::is_empty) {
                return Err("a scrub job needs a pool".to_string());
            }
        }
        KIND_SNAPSHOT => check_snapshot_fields(job)?,
        KIND_REPLICATE => check_replicate_fields(job)?,
        KIND_BACKUP => check_backup_fields(job)?,
        KIND_MIRROR => check_mirror_fields(job)?,
        other => return Err(format!("unsupported job kind '{other}'")),
    }
    if !known_hosts.iter().any(|h| h == &job.host) {
        return Err(format!("host '{}' is not in config.toml", job.host));
    }
    if job.dest_keep == Some(0) {
        return Err("dest_keep must be at least 1".to_string());
    }
    if job.dest_keep.is_some() && !(job.kind == KIND_REPLICATE && job.dest_host.as_deref() != Some(DEST_FILE)) {
        return Err("dest_keep is only for a replication job to a host, not to a file".to_string());
    }
    if job.raw && job.kind != KIND_REPLICATE {
        return Err("raw is only for a replication job".to_string());
    }
    if (job.remote.is_some() || job.bwlimit.is_some()) && job.kind != KIND_MIRROR {
        return Err("remote and bwlimit are only for a cloud mirror job".to_string());
    }
    if job.kind == KIND_REPLICATE {
        match job.dest_host.as_deref() {
            Some(DEST_FILE) => {}
            Some(dest) if !known_hosts.iter().any(|h| h == dest) => return Err(format!("destination host '{dest}' is not in config.toml")),
            _ => {}
        }
    }
    Schedule::parse(&job.schedule).map_err(|e| format!("bad schedule: {e}"))
}

/// Whether `ancestor` is a proper ancestor dataset of `descendant` (`tank` of `tank/data`, but not
/// `tank/data` of `tank/database`).
fn is_ancestor(ancestor: &str, descendant: &str) -> bool {
    descendant.strip_prefix(ancestor).is_some_and(|rest| rest.starts_with('/'))
}

/// What an enabled, well-formed job that takes snapshots of its own prunes: its dataset, the prefix of those snapshots
/// and whether it also covers descendants. A snapshot job takes `<prefix>-<stamp>`; a backup or cloud mirror whose source is
/// `new` takes `zit-<job id>-<stamp>` (on its dataset only) and keeps only the newest few of them.
fn snapshot_taker(j: &Job) -> Option<(&str, &str, bool)> {
    if !j.enabled {
        return None;
    }
    let dataset = j.dataset.as_deref().unwrap_or("");
    match j.kind.as_str() {
        KIND_SNAPSHOT if check_snapshot_fields(j).is_ok() => Some((dataset, effective_prefix(j), j.recursive)),
        KIND_BACKUP if check_backup_fields(j).is_ok() && matches!(backup_source(j), BackupSource::Fresh) => Some((dataset, "", false)),
        KIND_MIRROR if check_mirror_fields(j).is_ok() && matches!(backup_source(j), BackupSource::Fresh) => Some((dataset, "", false)),
        _ => None,
    }
}

/// Whether two enabled, well-formed jobs would prune each other's snapshots: same host and prefix, and datasets that are
/// equal or where a recursive job covers the other's dataset (a recursive job sees, and destroys with `-r`, the
/// same-named snapshots of its descendants). A backup or mirror that takes fresh snapshots counts, with its own prefix.
fn clashes(a: &Job, b: &Job) -> bool {
    let (Some((da, pa, ra)), Some((db, pb, rb))) = (snapshot_taker(a), snapshot_taker(b)) else { return false };
    let prefix = |j: &Job, own: &str| if own.is_empty() { fresh_prefix(&j.id) } else { own.to_string() };
    if a.host != b.host || prefix(a, pa) != prefix(b, pb) {
        return false;
    }
    da == db || (ra && is_ancestor(da, db)) || (rb && is_ancestor(db, da))
}

/// Whether two enabled, well-formed replicate jobs write to the same place: their incrementals
/// would corrupt each other.
fn dest_clashes(a: &Job, b: &Job) -> bool {
    let live = |j: &Job| j.enabled && j.kind == KIND_REPLICATE && check_replicate_fields(j).is_ok();
    if !live(a) || !live(b) || a.dest_host != b.dest_host {
        return false;
    }
    match a.dest_host.as_deref() {
        Some(DEST_FILE) => a.dest_dir == b.dest_dir,
        _ => a.dest_dataset == b.dest_dataset,
    }
}

/// Whether two enabled, well-formed jobs would write over each other in the cloud: two mirrors whose remote paths
/// overlap, or a mirror and a restic backup whose `rclone:` repository overlaps the mirror's path (a sync would move
/// the repository's files into `old/`). Remote names are compared across ALL hosts: the guide has people create the
/// same `aws` remote everywhere, so the same name and path on two hosts is taken to be one bucket. (A false alarm,
/// two accounts under one name, is cured by naming the remotes differently; the opposite mistake loses history.)
fn remote_clashes(a: &Job, b: &Job) -> bool {
    let mirror = |j: &Job| j.enabled && j.kind == KIND_MIRROR && check_mirror_fields(j).is_ok();
    let restic = |j: &Job| j.enabled && j.kind == KIND_BACKUP && check_backup_fields(j).is_ok();
    let repo = |j: &Job| j.repository.as_deref().and_then(|r| r.strip_prefix("rclone:")).unwrap_or("").to_string();
    let remote = |j: &Job| j.remote.as_deref().unwrap_or("").to_string();
    match (mirror(a), mirror(b), restic(a), restic(b)) {
        (true, true, _, _) => crate::rclone::overlaps(&remote(a), &remote(b)),
        (true, _, _, true) => !repo(b).is_empty() && crate::rclone::overlaps(&remote(a), &repo(b)),
        (_, true, true, _) => !repo(a).is_empty() && crate::rclone::overlaps(&remote(b), &repo(a)),
        _ => false,
    }
}

/// Why `job` cannot run alongside the enabled `earlier` job, if it cannot.
pub fn clash_reason(earlier: &Job, job: &Job) -> Option<String> {
    if clashes(earlier, job) {
        Some(format!("same host and prefix as job '{}' on an overlapping dataset: they would prune each other", earlier.id))
    } else if dest_clashes(earlier, job) {
        Some(format!("same destination as job '{}': their incrementals would corrupt each other", earlier.id))
    } else if remote_clashes(earlier, job) {
        Some(format!("its remote overlaps the one of job '{}' (remote names count as the same place on every host): a sync would move the other job's files", earlier.id))
    } else {
        None
    }
}

/// Another job in `jobs` that `candidate` would clash with, if any.
pub fn clashing_job<'a>(jobs: &'a [Job], candidate: &Job) -> Option<&'a Job> {
    jobs.iter().find(|j| j.id != candidate.id && clash_reason(j, candidate).is_some())
}

/// For each job that clashes with an *earlier* job that can itself run, why it cannot run. A
/// flagged job never blocks a later one, so only the first of a group runs.
pub fn duplicate_prefix_reasons(jobs: &[Job]) -> std::collections::HashMap<String, String> {
    let mut accepted: Vec<&Job> = Vec::new();
    let mut reasons = std::collections::HashMap::new();
    for job in jobs {
        match accepted.iter().find_map(|earlier| clash_reason(earlier, job)) {
            Some(reason) => {
                reasons.insert(job.id.clone(), reason);
            }
            None => accepted.push(job),
        }
    }
    reasons
}

#[cfg(test)]
impl Job {
    pub fn test_snapshot(id: &str, dataset: &str, prefix: &str, keep: u32) -> Job {
        Job { id: id.into(), host: "local".into(), kind: KIND_SNAPSHOT.into(), pool: None, dataset: Some(dataset.into()), recursive: false, prefix: Some(prefix.into()), keep: Some(keep), dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, remote: None, bwlimit: None, force: false, raw: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None, schedule: "0 * * * *".into(), catch_up: true, enabled: true, created_at: None }
    }

    pub fn test_backup(id: &str, dataset: &str, prefix: &str, repository: &str, password_file: &str) -> Job {
        Job { id: id.into(), host: "local".into(), kind: KIND_BACKUP.into(), pool: None, dataset: Some(dataset.into()), recursive: false, prefix: Some(prefix.into()), keep: None, dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, remote: None, bwlimit: None, force: false, raw: false, repository: Some(repository.into()), password_file: Some(password_file.into()), retention: None, source: None, snapshot: None, path: None, schedule: "0 3 * * *".into(), catch_up: true, enabled: true, created_at: None }
    }

    pub fn test_mirror(id: &str, dataset: &str, prefix: &str, remote: &str) -> Job {
        Job { remote: Some(remote.into()), kind: KIND_MIRROR.into(), password_file: None, repository: None, schedule: "30 3 * * *".into(), ..Job::test_backup(id, dataset, prefix, "x", "/p") }
    }

    pub fn test_replicate(id: &str, dataset: &str, prefix: &str, dest_host: &str, dest_dataset: &str) -> Job {
        Job { id: id.into(), host: "local".into(), kind: KIND_REPLICATE.into(), pool: None, dataset: Some(dataset.into()), recursive: false, prefix: Some(prefix.into()), keep: None, dest_host: Some(dest_host.into()), dest_dataset: Some(dest_dataset.into()), dest_dir: None, dest_keep: None, remote: None, bwlimit: None, force: false, raw: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None, schedule: "30 * * * *".into(), catch_up: true, enabled: true, created_at: None }
    }
}

/// Re-reads `jobs.toml`, applies `change`, and saves. Reading first means a hand edit made
/// since the caller last looked is kept, and a file that cannot be read stops the change
/// before anything is written, so a half-typed file is never overwritten.
fn edit(path: &Path, change: impl FnOnce(&mut Vec<Job>) -> Result<()>) -> Result<()> {
    let mut list = load(path)?;
    change(&mut list)?;
    save(path, &list)
}

fn no_such_job(id: &str) -> AppError {
    AppError::Config(format!("no job '{id}'"))
}

/// Adds `job`, or replaces the job with the same id in place.
pub fn upsert(path: &Path, job: Job) -> Result<()> {
    edit(path, |list| {
        match list.iter_mut().find(|j| j.id == job.id) {
            Some(slot) => *slot = job,
            None => list.push(job),
        }
        Ok(())
    })
}

pub fn remove(path: &Path, id: &str) -> Result<()> {
    edit(path, |list| {
        let before = list.len();
        list.retain(|j| j.id != id);
        if list.len() == before { Err(no_such_job(id)) } else { Ok(()) }
    })
}

pub fn set_enabled(path: &Path, id: &str, enabled: bool) -> Result<()> {
    edit(path, |list| {
        list.iter_mut().find(|j| j.id == id).map(|j| j.enabled = enabled).ok_or_else(|| no_such_job(id))
    })
}

#[cfg(test)]
mod tests;
