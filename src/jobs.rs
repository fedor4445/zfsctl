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

// The four bools (`recursive`, `force`, `catch_up`, `enabled`) are independent job flags in `jobs.toml`.
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
mod tests {
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
            dest_keep: None,
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
}
