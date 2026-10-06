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

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    /// A host name from `config.toml`; `local` is the hub itself.
    pub host: String,
    pub kind: String,
    /// The pool a scrub job runs on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
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

/// Whether the daemon can run `job`: its schedule if so, or the reason it cannot.
pub fn resolve(job: &Job, known_hosts: &[String]) -> std::result::Result<Schedule, String> {
    if job.kind != KIND_SCRUB {
        return Err(format!("unsupported job kind '{}'", job.kind));
    }
    if job.pool.as_deref().is_none_or(str::is_empty) {
        return Err("a scrub job needs a pool".to_string());
    }
    if !known_hosts.iter().any(|h| h == &job.host) {
        return Err(format!("host '{}' is not in config.toml", job.host));
    }
    Schedule::parse(&job.schedule).map_err(|e| format!("bad schedule: {e}"))
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
        std::fs::write(&path, "[[jobs]]\nid = \"x\"\nhost = \"local\"\nkind = \"replicate\"\nschedule = \"0 3 * * *\"\n").unwrap();
        let jobs = load(&path).unwrap();
        assert_eq!(resolve(&jobs[0], &hosts()).unwrap_err(), "unsupported job kind 'replicate'");
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
}
