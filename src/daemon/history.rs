//! The run history: an append-only JSON-lines file, one line per run event.
//!
//! One line is one record, so a write torn by a power cut damages at most the
//! last line; a reader skips any line that does not parse.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::PathBuf;
#[cfg(test)]
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    Started,
    Finished,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunResult {
    Ok,
    Failed,
    Skipped,
}

// The JSON key is `event` (see the spec), so the field keeps that name.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub job_id: String,
    pub run_id: String,
    pub event: EventKind,
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_for: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<RunResult>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub catch_up: bool,
    /// On `started`: the pool's scan summary before the scrub began, so a
    /// restarted daemon can tell the new scrub's completion from the old one's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// A run reconstructed from its events.
#[allow(clippy::struct_field_names)] // `run_id` mirrors the history's JSON key
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub job_id: String,
    pub run_id: String,
    pub started_at: DateTime<Utc>,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// `None` while the run is still going.
    pub result: Option<RunResult>,
    pub message: String,
    pub catch_up: bool,
    pub summary: Option<String>,
}

/// Groups events by `run_id`, in order of first appearance.
pub fn runs(events: &[Event]) -> Vec<Run> {
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, Run> = HashMap::new();
    for e in events {
        let run = by_id.entry(e.run_id.clone()).or_insert_with(|| {
            order.push(e.run_id.clone());
            Run {
                job_id: e.job_id.clone(),
                run_id: e.run_id.clone(),
                started_at: e.at,
                scheduled_for: None,
                finished_at: None,
                result: None,
                message: String::new(),
                catch_up: false,
                summary: None,
            }
        });
        run.scheduled_for = run.scheduled_for.or(e.scheduled_for);
        run.catch_up |= e.catch_up;
        if e.summary.is_some() {
            run.summary.clone_from(&e.summary);
        }
        if e.event != EventKind::Started {
            run.finished_at = Some(e.at);
            run.result = e.result.or(Some(RunResult::Ok));
            run.message.clone_from(&e.message);
        }
    }
    order.into_iter().filter_map(|id| by_id.remove(&id)).collect()
}

/// The latest `scheduled_for` recorded for each job, over every run including
/// skipped ones: a skipped run still means that slot was dealt with.
pub fn last_scheduled(runs: &[Run]) -> HashMap<String, DateTime<Utc>> {
    let mut out: HashMap<String, DateTime<Utc>> = HashMap::new();
    for run in runs {
        if let Some(t) = run.scheduled_for {
            let slot = out.entry(run.job_id.clone()).or_insert(t);
            *slot = (*slot).max(t);
        }
    }
    out
}

pub struct History {
    path: PathBuf,
}

impl History {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        History { path: path.into() }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one event as a single line in a single write.
    pub fn append(&self, event: &Event) -> std::io::Result<()> {
        let mut line = serde_json::to_string(event).map_err(std::io::Error::other)?;
        line.push('\n');
        let mut file = std::fs::OpenOptions::new().read(true).append(true).create(true).mode(0o600).open(&self.path)?;
        // A write torn by a power cut leaves a final line with no newline; start a fresh
        // line so this record is not glued onto the fragment and lost with it.
        let len = file.metadata()?.len();
        if len > 0 {
            let mut last = [0u8; 1];
            file.read_exact_at(&mut last, len - 1)?;
            if last[0] != b'\n' {
                line.insert(0, '\n');
            }
        }
        file.write_all(line.as_bytes())
    }

    /// Every event that parses, oldest first. A missing file is empty.
    pub fn read(&self) -> Vec<Event> {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return Vec::new() };
        text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// Keeps only the newest `keep` runs of each job, rewriting the file atomically.
    pub fn prune(&self, keep: usize) -> std::io::Result<()> {
        let events = self.read();
        let all = runs(&events);
        let mut kept_ids = std::collections::HashSet::new();
        let mut per_job: HashMap<&str, usize> = HashMap::new();
        for run in all.iter().rev() {
            let n = per_job.entry(run.job_id.as_str()).or_insert(0);
            if *n < keep {
                *n += 1;
                kept_ids.insert(run.run_id.clone());
            }
        }
        if kept_ids.len() == all.len() {
            return Ok(());
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut out = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
            for e in events.iter().filter(|e| kept_ids.contains(&e.run_id)) {
                let mut line = serde_json::to_string(e).map_err(std::io::Error::other)?;
                line.push('\n');
                out.write_all(line.as_bytes())?;
            }
            out.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn ev(job: &str, run: &str, kind: EventKind, when: &str, result: Option<RunResult>) -> Event {
        Event {
            job_id: job.to_string(),
            run_id: run.to_string(),
            event: kind,
            at: at(when),
            scheduled_for: Some(at(when)),
            result,
            message: String::new(),
            catch_up: false,
            summary: None,
        }
    }

    #[test]
    fn appended_events_read_back_in_order_and_the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        assert!(h.read().is_empty(), "a missing file is empty");
        let a = ev("j", "r1", EventKind::Started, "2026-10-06T03:00:00Z", None);
        let b = ev("j", "r1", EventKind::Finished, "2026-10-06T03:20:00Z", Some(RunResult::Ok));
        h.append(&a).unwrap();
        h.append(&b).unwrap();
        assert_eq!(h.read(), vec![a, b]);
        assert_eq!(std::fs::metadata(h.path()).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_torn_or_garbage_line_is_skipped_and_the_rest_survive() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        let good = ev("j", "r1", EventKind::Started, "2026-10-06T03:00:00Z", None);
        h.append(&good).unwrap();
        std::fs::OpenOptions::new().append(true).open(h.path()).unwrap().write_all(b"{\"job_id\":\"j\",\"run_i").unwrap();
        assert_eq!(h.read(), vec![good.clone()], "the torn final line is dropped");
        h.append(&good).unwrap();
        assert_eq!(h.read(), vec![good.clone(), good], "an append after a torn line starts a fresh line, so it is not lost with the fragment");
    }

    #[test]
    fn runs_reconstruct_started_finished_skipped_and_running() {
        let mut started = ev("j", "r1", EventKind::Started, "2026-10-06T03:00:00Z", None);
        started.summary = Some("scrub repaired 0B on Sun".to_string());
        let mut finished = ev("j", "r1", EventKind::Finished, "2026-10-06T03:20:00Z", Some(RunResult::Failed));
        finished.message = "2 errors".to_string();
        let skipped = ev("j", "r2", EventKind::Skipped, "2026-10-07T03:00:00Z", Some(RunResult::Skipped));
        let running = ev("k", "r3", EventKind::Started, "2026-10-07T04:00:00Z", None);
        let all = runs(&[started, finished, skipped, running]);
        assert_eq!(all.len(), 3);
        assert_eq!((all[0].result, all[0].message.as_str(), all[0].summary.as_deref()), (Some(RunResult::Failed), "2 errors", Some("scrub repaired 0B on Sun")));
        assert_eq!(all[0].finished_at, Some(at("2026-10-06T03:20:00Z")));
        assert_eq!(all[1].result, Some(RunResult::Skipped));
        assert_eq!(all[2].result, None, "started with no finish is still running");
    }

    #[test]
    fn a_finish_with_no_start_is_a_complete_run() {
        let only = ev("j", "r1", EventKind::Finished, "2026-10-06T03:00:00Z", Some(RunResult::Failed));
        let all = runs(&[only]);
        assert_eq!((all.len(), all[0].result), (1, Some(RunResult::Failed)));
    }

    #[test]
    fn last_scheduled_takes_the_latest_slot_per_job_skipped_included() {
        let all = runs(&[
            ev("j", "r1", EventKind::Finished, "2026-10-01T03:00:00Z", Some(RunResult::Ok)),
            ev("j", "r2", EventKind::Skipped, "2026-10-08T03:00:00Z", Some(RunResult::Skipped)),
            ev("k", "r3", EventKind::Finished, "2026-10-02T03:00:00Z", Some(RunResult::Ok)),
        ]);
        let last = last_scheduled(&all);
        assert_eq!(last["j"], at("2026-10-08T03:00:00Z"));
        assert_eq!(last["k"], at("2026-10-02T03:00:00Z"));
    }

    #[test]
    fn prune_keeps_the_newest_runs_of_each_job_whole() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        for day in 1..=5 {
            let when = format!("2026-10-0{day}T03:00:00Z");
            h.append(&ev("j", &format!("j{day}"), EventKind::Started, &when, None)).unwrap();
            h.append(&ev("j", &format!("j{day}"), EventKind::Finished, &when, Some(RunResult::Ok))).unwrap();
        }
        h.append(&ev("k", "k1", EventKind::Finished, "2026-10-01T03:00:00Z", Some(RunResult::Ok))).unwrap();
        h.prune(2).unwrap();
        let kept: Vec<String> = runs(&h.read()).into_iter().map(|r| r.run_id).collect();
        assert_eq!(kept, vec!["j4", "j5", "k1"], "each kept run keeps both its events, other jobs are untouched");
        assert!(!h.path().with_extension("jsonl.tmp").exists());
    }

    #[test]
    fn prune_with_nothing_to_drop_leaves_the_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path().join("history.jsonl"));
        h.append(&ev("j", "r1", EventKind::Finished, "2026-10-01T03:00:00Z", Some(RunResult::Ok))).unwrap();
        let before = std::fs::read(h.path()).unwrap();
        h.prune(10).unwrap();
        assert_eq!(std::fs::read(h.path()).unwrap(), before);
    }
}
