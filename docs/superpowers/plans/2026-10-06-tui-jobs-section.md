# TUI Jobs Section Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Jobs section in the TUI to list, create, edit, enable, disable and delete the scrub jobs the daemon runs, with a live schedule preview, a per-job run panel, a daemon-status title and a failed-jobs indicator.

**Architecture:** `src/ui/jobs.rs` holds the section's data (`JobsView`, read from the daemon's state directory) and pure helpers. Writes reuse the existing `MutationCall` pipeline with three new local-file variants backed by read-modify-write functions in `src/jobs.rs`. The job form is one new `FormKind::Job`; the table, panel and status indicator are drawn directly from the loaded data.

**Tech Stack:** Rust 2021, ratatui 0.29, crossterm 0.28, existing `chrono`, `serde_json`, `toml`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-06-scheduler-daemon-and-scrubs-design.md`, section 8 (amended; this is plan 2 of 2, plan 1 is merged).

**How this plan is built (and verified):** The change touches very large existing files (`state.rs` is over 7,000 lines), so each task gives the exact verified patch instead of prose edits. Every task is a test patch (apply, watch the build fail), then an implementation patch (apply, watch the suite pass). The 14 patches were produced test-first in a scratch clone and then replayed in order onto a fresh clone of the real repo: the resulting tree was identical and `cargo test` passed 871/871. Apply them with `git apply` exactly as written; a patch that does not apply means the tree has drifted from the base this plan was written against (commit `d0f9058`), which should be reported, not worked around.

## Global Constraints

- The crate has `unsafe_code = "forbid"`; clippy pedantic is on and `indexing_slicing`/`unwrap_used` warn: new non-test code is clean under `cargo clippy --all-targets`.
- No new dependencies.
- Baseline before starting: `cargo test` reports 817 passed. Expected totals after each task: 828, 835, 846, 859, 865, 871.
- Times shown to the operator are the hub's local time. The TUI only reads the daemon's state directory (`/var/db/zfsctl`), so it must run on the hub as root to see the daemon's jobs.
- `jobs.toml` is written only by the TUI's `SaveJob`/`DeleteJob`/`SetJobEnabled`, always by re-reading the file first, and never when the file cannot be read.
- Every key the Jobs section handles is advertised in the hint bar, one key per action (the existing hint-drift tests enforce this and now cover Jobs), which is why edit is `Enter` only, with no `e` alias.

## Review Focus

- The TUI must never overwrite a `jobs.toml` it cannot read (a half-typed hand edit), and must keep a hand edit made since it last looked. Pinned in Task 2.
- The selection must stay on the same job across a refresh that adds, removes or reorders jobs. Pinned in Task 3.
- Picking a preset with `F2` must insert only the cron text, never the label. Pinned in Task 4.
- A bad schedule, an unknown host, an empty pool or a bad catch-up answer must keep the form open and say why. Pinned in Task 4.
- Only enabled jobs whose latest run failed count toward `N jobs failed`; a recovered job clears it. Pinned in Tasks 1 and 6.
- The Jobs panel must not crowd the table on a short terminal. Pinned in Task 6.
- Existing tests that assumed Shares is the last section are updated, not weakened. Task 3.

---

### Task 1: The Jobs view data layer and form helpers

**Files (created or modified):**
- `src/ui/jobs.rs`
- `src/ui/mod.rs`

**Interfaces:**
- Consumes: `jobs::{load, resolve, Job}`, `daemon::{heartbeat, history}` (plan 1).
- Produces, in `crate::ui::jobs`: `JobsView` (`jobs`, `runs`, `heartbeat`, `unsupported`, `error`, `loaded`) with `last_run`, `runs_of`, `failed_count`, `daemon(now) -> DaemonState`; `load(state_dir, hosts, previous) -> JobsView`; `preset_values()`, `insert_text`, `describe`, `preview_lines`, `new_job_id`, `age_text`, `last_result_text`, `DEFAULT_SCHEDULE`.

- [ ] **Step 1: Apply the failing tests**

````bash
git apply <<'PATCH'
diff --git a/src/ui/jobs.rs b/src/ui/jobs.rs
new file mode 100644
index 0000000..8a4086b
--- /dev/null
+++ b/src/ui/jobs.rs
@@ -0,0 +1,156 @@
+#[cfg(test)]
+mod tests {
+    use super::*;
+    use crate::daemon::heartbeat::Heartbeat;
+    use crate::daemon::history::{Event, EventKind, History, RunResult};
+    use crate::jobs::{self, Job, KIND_SCRUB};
+    use chrono::FixedOffset;
+
+    fn at(s: &str) -> DateTime<Utc> {
+        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
+    }
+
+    fn job(id: &str) -> Job {
+        Job { id: id.into(), host: "local".into(), kind: KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * 0".into(), catch_up: true, enabled: true, created_at: None }
+    }
+
+    fn event(job_id: &str, run_id: &str, kind: EventKind, when: &str, result: Option<RunResult>) -> Event {
+        Event { job_id: job_id.into(), run_id: run_id.into(), event: kind, at: at(when), scheduled_for: Some(at(when)), result, message: "msg".into(), catch_up: false, summary: None }
+    }
+
+    fn hosts() -> Vec<String> {
+        vec!["local".to_string(), "fbsd16".to_string()]
+    }
+
+    #[test]
+    fn presets_carry_a_label_and_the_cron_part_is_what_gets_inserted() {
+        let values = preset_values();
+        assert_eq!(values[2], "0 3 * * 0  (Weekly, Sunday 03:00)");
+        assert_eq!(insert_text(&values[2]), "0 3 * * 0");
+        assert_eq!(insert_text("0 5 * * *"), "0 5 * * *", "a plain value is inserted as typed");
+        assert!(values.iter().all(|v| crate::daemon::cron::Schedule::parse(insert_text(v)).is_ok()), "every preset is a valid schedule");
+        assert!(crate::daemon::cron::Schedule::parse(DEFAULT_SCHEDULE).is_ok());
+    }
+
+    #[test]
+    fn describe_names_a_preset_and_calls_anything_else_custom() {
+        assert_eq!(describe("0 3 * * 0"), "Weekly, Sunday 03:00");
+        assert_eq!(describe("  0 3 * * 0  "), "Weekly, Sunday 03:00", "surrounding space does not matter");
+        assert_eq!(describe("*/10 * * * *"), "custom schedule");
+    }
+
+    #[test]
+    fn the_preview_lists_the_next_three_run_times_in_the_given_zone() {
+        let est = FixedOffset::west_opt(5 * 3600).unwrap();
+        let now = at("2026-10-06T10:00:00Z").with_timezone(&est); // Tue 05:00 local
+        let lines = preview_lines("0 3 * * 0", &now);
+        assert_eq!(lines[0], "Weekly, Sunday 03:00");
+        assert_eq!(lines[1], "Next: Sun 2026-10-11 03:00, Sun 2026-10-18 03:00, Sun 2026-10-25 03:00");
+    }
+
+    #[test]
+    fn the_preview_reports_a_bad_schedule_and_an_impossible_one() {
+        let now = at("2026-10-06T10:00:00Z");
+        let bad = preview_lines("61 * * * *", &now);
+        assert_eq!(bad.len(), 1);
+        assert!(bad[0].starts_with("Schedule error:") && bad[0].contains("outside 0-59"), "{bad:?}");
+        let never = preview_lines("0 0 31 2 *", &now);
+        assert_eq!(never[1], "Next: never");
+        assert!(preview_lines("", &now)[0].starts_with("Schedule error:"));
+    }
+
+    #[test]
+    fn new_job_ids_are_readable_and_never_collide() {
+        assert_eq!(new_job_id("local", "tank", &[]), "scrub-local-tank");
+        let taken = vec!["scrub-local-tank".to_string(), "scrub-local-tank-2".to_string()];
+        assert_eq!(new_job_id("local", "tank", &taken), "scrub-local-tank-3");
+        assert_eq!(new_job_id("fbsd16", "my pool", &[]), "scrub-fbsd16-my-pool", "anything outside [A-Za-z0-9_.-] becomes a dash");
+    }
+
+    #[test]
+    fn ages_read_naturally() {
+        let now = at("2026-10-06T12:00:00Z");
+        assert_eq!(age_text(now, at("2026-10-06T11:59:30Z")), "just now");
+        assert_eq!(age_text(now, at("2026-10-06T11:15:00Z")), "45m ago");
+        assert_eq!(age_text(now, at("2026-10-06T09:00:00Z")), "3h ago");
+        assert_eq!(age_text(now, at("2026-10-03T12:00:00Z")), "3d ago");
+        assert_eq!(age_text(now, at("2026-10-06T13:00:00Z")), "just now", "a time in the future is not negative");
+    }
+
+    #[test]
+    fn load_reads_jobs_runs_and_the_heartbeat_and_explains_jobs_that_cannot_run() {
+        let dir = tempfile::tempdir().unwrap();
+        let mut bad = job("bad");
+        bad.host = "gone".into();
+        jobs::save(&dir.path().join("jobs.toml"), &[job("ok"), bad]).unwrap();
+        let h = History::new(dir.path().join("history.jsonl"));
+        h.append(&event("ok", "r1", EventKind::Finished, "2026-10-05T03:00:00Z", Some(RunResult::Ok))).unwrap();
+        crate::daemon::heartbeat::write(&dir.path().join("daemon.json"), &Heartbeat { pid: 42, started_at: at("2026-10-01T00:00:00Z"), last_tick: at("2026-10-06T12:00:00Z") }).unwrap();
+        let view = load(dir.path(), &hosts(), None);
+        assert_eq!(view.jobs.len(), 2);
+        assert!(view.error.is_none() && view.loaded);
+        assert_eq!(view.runs.len(), 1);
+        assert!(view.unsupported["bad"].contains("'gone' is not in config.toml"));
+        assert!(!view.unsupported.contains_key("ok"));
+        assert_eq!(view.daemon(at("2026-10-06T12:00:05Z")), DaemonState::Running { pid: 42 });
+        assert_eq!(view.daemon(at("2026-10-06T12:01:00Z")), DaemonState::NotRunning, "a heartbeat older than ten seconds is stale");
+    }
+
+    #[test]
+    fn a_missing_heartbeat_means_the_daemon_is_not_running() {
+        let dir = tempfile::tempdir().unwrap();
+        let view = load(dir.path(), &hosts(), None);
+        assert_eq!(view.daemon(at("2026-10-06T12:00:00Z")), DaemonState::NotRunning);
+        assert!(view.jobs.is_empty() && view.error.is_none());
+    }
+
+    #[test]
+    fn an_unreadable_jobs_file_keeps_the_previous_data_and_reports_the_error() {
+        let dir = tempfile::tempdir().unwrap();
+        jobs::save(&dir.path().join("jobs.toml"), &[job("a")]).unwrap();
+        let first = load(dir.path(), &hosts(), None);
+        std::fs::write(dir.path().join("jobs.toml"), "this is [[[ not toml").unwrap();
+        let second = load(dir.path(), &hosts(), Some(&first));
+        assert_eq!(second.jobs, first.jobs, "the list does not vanish because of a typo");
+        assert!(second.error.as_deref().is_some_and(|e| e.contains("parsing")), "{:?}", second.error);
+    }
+
+    #[test]
+    fn last_run_runs_of_and_the_failed_count_use_the_newest_run_of_each_enabled_job() {
+        let dir = tempfile::tempdir().unwrap();
+        let mut off = job("off");
+        off.enabled = false;
+        jobs::save(&dir.path().join("jobs.toml"), &[job("a"), job("b"), job("c"), off]).unwrap();
+        let h = History::new(dir.path().join("history.jsonl"));
+        h.append(&event("a", "a1", EventKind::Finished, "2026-10-01T03:00:00Z", Some(RunResult::Failed))).unwrap();
+        h.append(&event("a", "a2", EventKind::Finished, "2026-10-02T03:00:00Z", Some(RunResult::Ok))).unwrap();
+        h.append(&event("b", "b1", EventKind::Finished, "2026-10-01T03:00:00Z", Some(RunResult::Ok))).unwrap();
+        h.append(&event("b", "b2", EventKind::Finished, "2026-10-02T03:00:00Z", Some(RunResult::Failed))).unwrap();
+        h.append(&event("off", "o1", EventKind::Finished, "2026-10-02T03:00:00Z", Some(RunResult::Failed))).unwrap();
+        let view = load(dir.path(), &hosts(), None);
+        assert_eq!(view.last_run("a").unwrap().run_id, "a2");
+        assert!(view.last_run("c").is_none());
+        assert_eq!(view.failed_count(), 1, "only b: a recovered, c never ran, off is disabled");
+        assert_eq!(view.runs_of("a", 8).iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(), vec!["a2", "a1"], "newest first");
+        assert_eq!(view.runs_of("a", 1).len(), 1);
+    }
+
+    #[test]
+    fn last_result_text_covers_never_running_ok_failed_and_skipped() {
+        let now = at("2026-10-06T12:00:00Z");
+        let dir = tempfile::tempdir().unwrap();
+        jobs::save(&dir.path().join("jobs.toml"), &[job("a")]).unwrap();
+        let h = History::new(dir.path().join("history.jsonl"));
+        assert_eq!(last_result_text(None, now), "never");
+        h.append(&event("a", "r1", EventKind::Started, "2026-10-06T09:00:00Z", None)).unwrap();
+        let running = load(dir.path(), &hosts(), None);
+        assert_eq!(last_result_text(running.last_run("a"), now), "running");
+        h.append(&event("a", "r1", EventKind::Finished, "2026-10-06T09:30:00Z", Some(RunResult::Ok))).unwrap();
+        let ok = load(dir.path(), &hosts(), None);
+        assert_eq!(last_result_text(ok.last_run("a"), now), "ok 2h ago");
+        h.append(&event("a", "r2", EventKind::Skipped, "2026-10-06T11:00:00Z", Some(RunResult::Skipped))).unwrap();
+        assert_eq!(last_result_text(load(dir.path(), &hosts(), None).last_run("a"), now), "skipped 1h ago");
+        h.append(&event("a", "r3", EventKind::Finished, "2026-10-04T11:00:00Z", Some(RunResult::Failed))).unwrap();
+        assert_eq!(last_result_text(load(dir.path(), &hosts(), None).last_run("a"), now), "skipped 1h ago", "the newest by start time wins, not by file order");
+    }
+}
diff --git a/src/ui/mod.rs b/src/ui/mod.rs
index 4f5fca4..97397fc 100644
--- a/src/ui/mod.rs
+++ b/src/ui/mod.rs
@@ -1,4 +1,5 @@
 pub mod hints;
+pub mod jobs;
 pub mod layout;
 pub mod metrics;
 pub mod modal;
PATCH
````

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test 2>&1 | grep -E "^error" -A3 | head -12`
Expected: the build FAILS because the code the tests use does not exist yet: `cannot find type DateTime`/`Utc` and the missing helpers in `src/ui/jobs.rs`.

- [ ] **Step 3: Apply the implementation**

````bash
git apply <<'PATCH'
diff --git a/src/ui/jobs.rs b/src/ui/jobs.rs
index 8a4086b..1b49b1b 100644
--- a/src/ui/jobs.rs
+++ b/src/ui/jobs.rs
@@ -1,3 +1,160 @@
+//! The Jobs section's data and the pure helpers its table and form use.
+//!
+//! Everything here reads or computes; nothing here draws or handles keys.
+
+use crate::daemon::cron::Schedule;
+use crate::daemon::heartbeat::{self, Heartbeat};
+use crate::daemon::history::{self, History, Run, RunResult};
+use crate::jobs::{self, Job};
+use chrono::{DateTime, TimeZone, Utc};
+use std::collections::HashMap;
+use std::path::Path;
+
+/// What a new job starts with: weekly, Sunday at 03:00.
+pub const DEFAULT_SCHEDULE: &str = "0 3 * * 0";
+
+const PRESETS: [(&str, &str); 4] = [
+    ("0 * * * *", "Hourly"),
+    ("0 2 * * *", "Daily, 02:00"),
+    (DEFAULT_SCHEDULE, "Weekly, Sunday 03:00"),
+    ("0 3 1 * *", "Monthly, 1st 03:00"),
+];
+
+/// The `F2` list for the Schedule field: the cron text, then its label.
+pub fn preset_values() -> Vec<String> {
+    PRESETS.iter().map(|(cron, label)| format!("{cron}  ({label})")).collect()
+}
+
+/// What a picked value puts into the field: the cron part, without the label.
+pub fn insert_text(picked: &str) -> &str {
+    picked.split_once("  (").map_or(picked, |(cron, _)| cron)
+}
+
+/// The preset's name for `expr`, or `custom schedule`.
+pub fn describe(expr: &str) -> String {
+    let expr = expr.trim();
+    PRESETS.iter().find(|(cron, _)| *cron == expr).map_or_else(|| "custom schedule".to_string(), |(_, label)| (*label).to_string())
+}
+
+/// The form's preview under the Schedule field: what the schedule is called
+/// and its next three run times in `now`'s zone, or why it does not parse.
+pub fn preview_lines<Tz: TimeZone>(expr: &str, now: &DateTime<Tz>) -> Vec<String>
+where
+    Tz::Offset: std::fmt::Display,
+{
+    let schedule = match Schedule::parse(expr) {
+        Ok(s) => s,
+        Err(e) => return vec![format!("Schedule error: {e}")],
+    };
+    let mut times = Vec::new();
+    let mut cursor = now.clone();
+    for _ in 0..3 {
+        let Some(next) = schedule.next_after(&cursor) else { break };
+        times.push(next.format("%a %Y-%m-%d %H:%M").to_string());
+        cursor = next;
+    }
+    let next = if times.is_empty() { "never".to_string() } else { times.join(", ") };
+    vec![describe(expr), format!("Next: {next}")]
+}
+
+/// `scrub-<host>-<pool>`, made safe for a file key, with `-2`, `-3`, ... added if taken.
+pub fn new_job_id(host: &str, pool: &str, existing: &[String]) -> String {
+    let safe = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '-' }).collect::<String>();
+    let base = format!("scrub-{}-{}", safe(host), safe(pool));
+    if !existing.contains(&base) {
+        return base;
+    }
+    (2..).map(|n| format!("{base}-{n}")).find(|candidate| !existing.contains(candidate)).unwrap_or(base)
+}
+
+/// `just now`, `45m ago`, `3h ago`, `2d ago`. A time in the future reads as `just now`.
+pub fn age_text(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
+    let secs = (now - then).num_seconds();
+    if secs < 60 {
+        "just now".to_string()
+    } else if secs < 3_600 {
+        format!("{}m ago", secs / 60)
+    } else if secs < 86_400 {
+        format!("{}h ago", secs / 3_600)
+    } else {
+        format!("{}d ago", secs / 86_400)
+    }
+}
+
+/// The last-result column: `never`, `running`, or the result and its age.
+pub fn last_result_text(run: Option<&Run>, now: DateTime<Utc>) -> String {
+    let Some(run) = run else { return "never".to_string() };
+    let Some(result) = run.result else { return "running".to_string() };
+    let word = match result {
+        RunResult::Ok => "ok",
+        RunResult::Failed => "failed",
+        RunResult::Skipped => "skipped",
+    };
+    format!("{word} {}", age_text(now, run.finished_at.unwrap_or(run.started_at)))
+}
+
+#[derive(Debug, Clone, Copy, PartialEq, Eq)]
+pub enum DaemonState {
+    Running { pid: u32 },
+    NotRunning,
+}
+
+/// Everything the Jobs section shows, read from the daemon's state directory.
+#[derive(Debug, Clone, PartialEq, Default)]
+pub struct JobsView {
+    pub jobs: Vec<Job>,
+    pub runs: Vec<Run>,
+    pub heartbeat: Option<Heartbeat>,
+    /// Why a job cannot run, by job id.
+    pub unsupported: HashMap<String, String>,
+    /// Set when `jobs.toml` could not be read; `jobs` then holds the last good list.
+    pub error: Option<String>,
+    /// False until the first load, so the section does not claim "no jobs" before it has looked.
+    pub loaded: bool,
+}
+
+/// Reads `jobs.toml`, `history.jsonl` and `daemon.json` from `state_dir`. If
+/// `jobs.toml` cannot be read the list from `previous` is kept and the error recorded.
+pub fn load(state_dir: &Path, hosts: &[String], previous: Option<&JobsView>) -> JobsView {
+    let (jobs, error) = match jobs::load(&state_dir.join("jobs.toml")) {
+        Ok(list) => (list, None),
+        Err(e) => (previous.map(|p| p.jobs.clone()).unwrap_or_default(), Some(e.to_string())),
+    };
+    let unsupported = jobs.iter().filter_map(|j| jobs::resolve(j, hosts).err().map(|reason| (j.id.clone(), reason))).collect();
+    JobsView {
+        runs: history::runs(&History::new(state_dir.join("history.jsonl")).read()),
+        heartbeat: heartbeat::read(&state_dir.join("daemon.json")),
+        jobs,
+        unsupported,
+        error,
+        loaded: true,
+    }
+}
+
+impl JobsView {
+    /// The most recently started run of `id`.
+    pub fn last_run(&self, id: &str) -> Option<&Run> {
+        self.runs.iter().filter(|r| r.job_id == id).max_by_key(|r| r.started_at)
+    }
+
+    /// Up to `n` runs of `id`, newest first.
+    pub fn runs_of(&self, id: &str, n: usize) -> Vec<&Run> {
+        let mut runs: Vec<&Run> = self.runs.iter().filter(|r| r.job_id == id).collect();
+        runs.sort_by_key(|r| std::cmp::Reverse(r.started_at));
+        runs.truncate(n);
+        runs
+    }
+
+    /// Enabled jobs whose latest run failed.
+    pub fn failed_count(&self) -> usize {
+        self.jobs.iter().filter(|j| j.enabled && self.last_run(&j.id).is_some_and(|r| r.result == Some(RunResult::Failed))).count()
+    }
+
+    pub fn daemon(&self, now: DateTime<Utc>) -> DaemonState {
+        self.heartbeat.as_ref().filter(|h| h.is_fresh(now)).map_or(DaemonState::NotRunning, |h| DaemonState::Running { pid: h.pid })
+    }
+}
+
 #[cfg(test)]
 mod tests {
     use super::*;
PATCH
````

- [ ] **Step 4: Run the whole suite**

Run: `cargo test 2>&1 | grep -E "^error|^test .* FAILED|^test result" -A6`
Expected: `test result: ok. 828 passed; 0 failed`.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" -A4 | grep -E "^warning|-->" | paste - - | grep -E "src/(ui|jobs)"`
Expected: no output for these files (a `never used`/`never constructed` warning for code a later task wires up is fine).

```bash
git add -A
git commit -m "test: jobs view data layer; feat: the Jobs view data layer and form helpers

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 2: Job store edits and the three job mutations

**Files (created or modified):**
- `src/jobs.rs`
- `src/ui/modal.rs`

**Interfaces:**
- Consumes: `jobs::{load, save}` (plan 1), `MutationCall` (existing).
- Produces: `jobs::{upsert, remove, set_enabled}` (read-modify-write, refuse an unreadable file); `MutationCall::{SaveJob { state_dir, job: Box<Job> }, DeleteJob { state_dir, id }, SetJobEnabled { state_dir, id, enabled }}` (`DeleteJob` is warn-toned; the runner is never used).

- [ ] **Step 1: Apply the failing tests**

````bash
git apply <<'PATCH'
diff --git a/src/jobs.rs b/src/jobs.rs
index f4f65a8..001a3a8 100644
--- a/src/jobs.rs
+++ b/src/jobs.rs
@@ -162,6 +162,67 @@ mod tests {
         assert_eq!(resolve(&jobs[0], &hosts()).unwrap_err(), "unsupported job kind 'replicate'");
     }
 
+    fn file_with(jobs_list: &[Job]) -> (tempfile::TempDir, std::path::PathBuf) {
+        let dir = tempfile::tempdir().unwrap();
+        let path = dir.path().join("jobs.toml");
+        save(&path, jobs_list).unwrap();
+        (dir, path)
+    }
+
+    #[test]
+    fn upsert_adds_a_new_job_and_replaces_one_with_the_same_id_in_place() {
+        let (_dir, path) = file_with(&[scrub("a"), scrub("b")]);
+        upsert(&path, scrub("c")).unwrap();
+        let mut edited = scrub("a");
+        edited.schedule = "0 4 * * *".into();
+        upsert(&path, edited.clone()).unwrap();
+        let ids: Vec<String> = load(&path).unwrap().into_iter().map(|j| j.id).collect();
+        assert_eq!(ids, vec!["a", "b", "c"], "an edit keeps the job's place");
+        assert_eq!(load(&path).unwrap()[0], edited);
+    }
+
+    #[test]
+    fn upsert_creates_the_file_when_there_is_none() {
+        let dir = tempfile::tempdir().unwrap();
+        let path = dir.path().join("state").join("jobs.toml");
+        upsert(&path, scrub("a")).unwrap();
+        assert_eq!(load(&path).unwrap(), vec![scrub("a")]);
+    }
+
+    #[test]
+    fn remove_and_set_enabled_touch_only_their_job() {
+        let (_dir, path) = file_with(&[scrub("a"), scrub("b"), scrub("c")]);
+        set_enabled(&path, "b", false).unwrap();
+        let after = load(&path).unwrap();
+        assert_eq!(after.iter().map(|j| j.enabled).collect::<Vec<_>>(), vec![true, false, true]);
+        remove(&path, "a").unwrap();
+        assert_eq!(load(&path).unwrap().iter().map(|j| j.id.as_str()).collect::<Vec<_>>(), vec!["b", "c"]);
+        assert!(remove(&path, "nope").unwrap_err().to_string().contains("no job 'nope'"));
+        assert!(set_enabled(&path, "nope", true).unwrap_err().to_string().contains("no job 'nope'"));
+    }
+
+    #[test]
+    fn a_change_never_clobbers_a_hand_edit_made_since_the_tui_last_looked() {
+        let (_dir, path) = file_with(&[scrub("a")]);
+        let mut by_hand = load(&path).unwrap();
+        by_hand.push(scrub("typed-by-hand"));
+        save(&path, &by_hand).unwrap();
+        set_enabled(&path, "a", false).unwrap();
+        assert_eq!(load(&path).unwrap().len(), 2, "the hand-added job survived: every change re-reads the file first");
+    }
+
+    #[test]
+    fn a_file_that_cannot_be_read_is_refused_and_left_exactly_as_it_was() {
+        let dir = tempfile::tempdir().unwrap();
+        let path = dir.path().join("jobs.toml");
+        std::fs::write(&path, "[[jobs]]\nid = \"half-typed\n").unwrap();
+        let before = std::fs::read(&path).unwrap();
+        assert!(upsert(&path, scrub("a")).is_err());
+        assert!(remove(&path, "a").is_err());
+        assert!(set_enabled(&path, "a", false).is_err());
+        assert_eq!(std::fs::read(&path).unwrap(), before, "the operator's half-finished edit is not overwritten");
+    }
+
     #[test]
     fn resolve_explains_why_a_job_cannot_run() {
         assert!(resolve(&scrub("ok"), &hosts()).is_ok());
diff --git a/src/ui/modal.rs b/src/ui/modal.rs
index e297ac5..b60d9f1 100644
--- a/src/ui/modal.rs
+++ b/src/ui/modal.rs
@@ -283,6 +283,33 @@ mod tests {
     use super::*;
     use crate::exec::test_support::FakeRunner;
 
+    fn job(id: &str) -> crate::jobs::Job {
+        crate::jobs::Job { id: id.into(), host: "local".into(), kind: crate::jobs::KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * 0".into(), catch_up: true, enabled: true, created_at: None }
+    }
+
+    #[test]
+    fn job_mutations_edit_jobs_toml_and_never_touch_the_runner() {
+        let dir = tempfile::tempdir().unwrap();
+        let path = dir.path().join("jobs.toml");
+        let runner = FakeRunner::new("");
+        let state_dir = dir.path().to_path_buf();
+        MutationCall::SaveJob { state_dir: state_dir.clone(), job: Box::new(job("a")) }.run(&runner).unwrap();
+        MutationCall::SaveJob { state_dir: state_dir.clone(), job: Box::new(job("b")) }.run(&runner).unwrap();
+        MutationCall::SetJobEnabled { state_dir: state_dir.clone(), id: "a".into(), enabled: false }.run(&runner).unwrap();
+        assert_eq!(crate::jobs::load(&path).unwrap().iter().map(|j| j.enabled).collect::<Vec<_>>(), vec![false, true]);
+        MutationCall::DeleteJob { state_dir, id: "a".into() }.run(&runner).unwrap();
+        assert_eq!(crate::jobs::load(&path).unwrap().iter().map(|j| j.id.as_str()).collect::<Vec<_>>(), vec!["b"]);
+        assert!(runner.calls.lock().unwrap().is_empty(), "no command was run: these are local file edits");
+    }
+
+    #[test]
+    fn only_deleting_a_job_asks_for_a_warning_confirmation() {
+        let state_dir = std::path::PathBuf::from("/x");
+        assert_eq!(MutationCall::DeleteJob { state_dir: state_dir.clone(), id: "a".into() }.tone(), Tone::Warn);
+        assert_eq!(MutationCall::SaveJob { state_dir: state_dir.clone(), job: Box::new(job("a")) }.tone(), Tone::Neutral);
+        assert_eq!(MutationCall::SetJobEnabled { state_dir, id: "a".into(), enabled: true }.tone(), Tone::Neutral);
+    }
+
     #[test]
     fn secret_never_appears_in_debug_output() {
         let call = MutationCall::SetSmbShare {
PATCH
````

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test 2>&1 | grep -E "^error" -A3 | head -12`
Expected: the build FAILS because the code the tests use does not exist yet: `cannot find function upsert`, `no variant SaveJob`.

- [ ] **Step 3: Apply the implementation**

````bash
git apply <<'PATCH'
diff --git a/src/jobs.rs b/src/jobs.rs
index 001a3a8..4d1b993 100644
--- a/src/jobs.rs
+++ b/src/jobs.rs
@@ -94,6 +94,44 @@ pub fn resolve(job: &Job, known_hosts: &[String]) -> std::result::Result<Schedul
     Schedule::parse(&job.schedule).map_err(|e| format!("bad schedule: {e}"))
 }
 
+/// Re-reads `jobs.toml`, applies `change`, and saves. Reading first means a hand edit made
+/// since the caller last looked is kept, and a file that cannot be read stops the change
+/// before anything is written, so a half-typed file is never overwritten.
+fn edit(path: &Path, change: impl FnOnce(&mut Vec<Job>) -> Result<()>) -> Result<()> {
+    let mut list = load(path)?;
+    change(&mut list)?;
+    save(path, &list)
+}
+
+fn no_such_job(id: &str) -> AppError {
+    AppError::Config(format!("no job '{id}'"))
+}
+
+/// Adds `job`, or replaces the job with the same id in place.
+pub fn upsert(path: &Path, job: Job) -> Result<()> {
+    edit(path, |list| {
+        match list.iter_mut().find(|j| j.id == job.id) {
+            Some(slot) => *slot = job,
+            None => list.push(job),
+        }
+        Ok(())
+    })
+}
+
+pub fn remove(path: &Path, id: &str) -> Result<()> {
+    edit(path, |list| {
+        let before = list.len();
+        list.retain(|j| j.id != id);
+        if list.len() == before { Err(no_such_job(id)) } else { Ok(()) }
+    })
+}
+
+pub fn set_enabled(path: &Path, id: &str, enabled: bool) -> Result<()> {
+    edit(path, |list| {
+        list.iter_mut().find(|j| j.id == id).map(|j| j.enabled = enabled).ok_or_else(|| no_such_job(id))
+    })
+}
+
 #[cfg(test)]
 mod tests {
     use super::*;
diff --git a/src/ui/modal.rs b/src/ui/modal.rs
index b60d9f1..8ea184a 100644
--- a/src/ui/modal.rs
+++ b/src/ui/modal.rs
@@ -30,6 +30,10 @@ pub enum MutationCall {
     RemoveAclEntry { path: String, position: usize },
     SetSmbShare { share: crate::zfs::smb::SmbShare, password: Option<Secret> },
     RemoveSmbShare { dataset: String },
+    /// Creates or replaces a scheduled job in `<state_dir>/jobs.toml`. Local file edits: the runner is not used.
+    SaveJob { state_dir: std::path::PathBuf, job: Box<crate::jobs::Job> },
+    DeleteJob { state_dir: std::path::PathBuf, id: String },
+    SetJobEnabled { state_dir: std::path::PathBuf, id: String, enabled: bool },
 }
 
 /// A password held only until it is piped to `smbpasswd`. `Debug` never
@@ -65,7 +69,8 @@ impl MutationCall {
             | MutationCall::ReplaceDisk { .. }
             | MutationCall::UnallowPermission { .. }
             | MutationCall::RemoveAclEntry { .. }
-            | MutationCall::RemoveSmbShare { .. } => Tone::Warn,
+            | MutationCall::RemoveSmbShare { .. }
+            | MutationCall::DeleteJob { .. } => Tone::Warn,
             MutationCall::ScrubPool { .. }
             | MutationCall::ResilverPool { .. }
             | MutationCall::OnlineDisk { .. }
@@ -76,6 +81,8 @@ impl MutationCall {
             | MutationCall::AllowPermission { .. }
             | MutationCall::AddAclEntry { .. }
             | MutationCall::SetPoolProperty { .. }
+            | MutationCall::SaveJob { .. }
+            | MutationCall::SetJobEnabled { .. }
             | MutationCall::SetSmbShare { .. }
             | MutationCall::SetDatasetProperty { .. } => Tone::Neutral,
         }
@@ -105,6 +112,9 @@ impl MutationCall {
             MutationCall::RemoveAclEntry { .. } => "REMOVE ACL ENTRY",
             MutationCall::SetSmbShare { .. } => "SET SMB SHARE",
             MutationCall::RemoveSmbShare { .. } => "REMOVE SMB SHARE",
+            MutationCall::SaveJob { .. } => "SAVE JOB",
+            MutationCall::DeleteJob { .. } => "DELETE JOB",
+            MutationCall::SetJobEnabled { .. } => "ENABLE OR DISABLE JOB",
         }
     }
 }
@@ -149,6 +159,9 @@ impl MutationCall {
                 crate::zfs::smb::set_share(runner, share, password.as_ref().map(|p| p.0.as_str()))
             }
             MutationCall::RemoveSmbShare { dataset } => crate::zfs::smb::remove_share(runner, dataset),
+            MutationCall::SaveJob { state_dir, job } => crate::jobs::upsert(&state_dir.join("jobs.toml"), job.as_ref().clone()),
+            MutationCall::DeleteJob { state_dir, id } => crate::jobs::remove(&state_dir.join("jobs.toml"), id),
+            MutationCall::SetJobEnabled { state_dir, id, enabled } => crate::jobs::set_enabled(&state_dir.join("jobs.toml"), id, *enabled),
         }
     }
 }
PATCH
````

- [ ] **Step 4: Run the whole suite**

Run: `cargo test 2>&1 | grep -E "^error|^test .* FAILED|^test result" -A6`
Expected: `test result: ok. 835 passed; 0 failed`.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" -A4 | grep -E "^warning|-->" | paste - - | grep -E "src/(ui|jobs)"`
Expected: no output for these files (a `never used`/`never constructed` warning for code a later task wires up is fine).

```bash
git add -A
git commit -m "test: job store edits and job mutations; feat: job store edits and job mutations

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 3: The Jobs section: state, refresh, selection and the table

**Files (created or modified):**
- `src/ui/hints.rs`
- `src/ui/jobs.rs`
- `src/ui/layout.rs`
- `src/ui/mod.rs`
- `src/ui/state.rs`

**Interfaces:**
- Consumes: `JobsView`, `load`, `schedule_label`, `next_run_text` (Task 1).
- Produces: `Section::Jobs` (last in `App::sections`); `App::{jobs_view, jobs_table, state_dir}`; `App::refresh_jobs()`, `App::refresh_jobs_if_stale(now)` (every 2 s in the section, 10 s elsewhere); `ui::jobs::{schedule_label, next_run_text}`; `draw_jobs` (table, daemon-status title, empty-state and unreadable-file notices).

- [ ] **Step 1: Apply the failing tests**

````bash
git apply <<'PATCH'
diff --git a/src/ui/jobs.rs b/src/ui/jobs.rs
index 1b49b1b..9259073 100644
--- a/src/ui/jobs.rs
+++ b/src/ui/jobs.rs
@@ -310,4 +310,19 @@ mod tests {
         h.append(&event("a", "r3", EventKind::Finished, "2026-10-04T11:00:00Z", Some(RunResult::Failed))).unwrap();
         assert_eq!(last_result_text(load(dir.path(), &hosts(), None).last_run("a"), now), "skipped 1h ago", "the newest by start time wins, not by file order");
     }
+
+    #[test]
+    fn the_table_shows_a_presets_name_or_the_cron_text_of_a_custom_schedule() {
+        assert_eq!(schedule_label("0 3 * * 0"), "Weekly, Sunday 03:00");
+        assert_eq!(schedule_label(" */10 * * * * "), "*/10 * * * *");
+    }
+
+    #[test]
+    fn the_next_run_column_gives_a_day_and_time_or_never() {
+        let est = FixedOffset::west_opt(5 * 3600).unwrap();
+        let now = at("2026-10-06T10:00:00Z").with_timezone(&est);
+        assert_eq!(next_run_text("0 3 * * 0", &now), "Sun 10-11 03:00");
+        assert_eq!(next_run_text("0 0 31 2 *", &now), "never");
+        assert_eq!(next_run_text("bogus", &now), "never");
+    }
 }
diff --git a/src/ui/layout.rs b/src/ui/layout.rs
index 397fbf1..3a66f88 100644
--- a/src/ui/layout.rs
+++ b/src/ui/layout.rs
@@ -1188,6 +1188,67 @@ mod tests {
         }
     }
 
+    fn jobs_app(jobs_list: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
+        let dir = tempfile::tempdir().unwrap();
+        crate::jobs::save(&dir.path().join("jobs.toml"), jobs_list).unwrap();
+        let mut app = App::new(vec!["local".to_string()]);
+        app.state_dir = dir.path().to_path_buf();
+        app.selected_section = app.sections.iter().position(|s| *s == Section::Jobs).unwrap();
+        app.refresh_jobs();
+        (app, dir)
+    }
+
+    fn a_job(id: &str, schedule: &str) -> crate::jobs::Job {
+        crate::jobs::Job { id: id.into(), host: "local".into(), kind: crate::jobs::KIND_SCRUB.into(), pool: Some("tank".into()), schedule: schedule.into(), catch_up: true, enabled: true, created_at: None }
+    }
+
+    #[test]
+    fn the_jobs_table_shows_each_job_with_its_schedule_next_run_and_last_result() {
+        let (mut app, dir) = jobs_app(&[a_job("scrub-local-tank", "0 3 * * 0"), a_job("custom", "*/10 * * * *")]);
+        let h = crate::daemon::history::History::new(dir.path().join("history.jsonl"));
+        let when = chrono::Utc::now() - chrono::Duration::hours(2);
+        h.append(&crate::daemon::history::Event { job_id: "scrub-local-tank".into(), run_id: "r1".into(), event: crate::daemon::history::EventKind::Finished, at: when, scheduled_for: Some(when), result: Some(crate::daemon::history::RunResult::Ok), message: String::new(), catch_up: false, summary: None }).unwrap();
+        app.refresh_jobs();
+        let text = render(&mut app, 120, 30).join("\n");
+        for want in ["Job", "Host", "Pool", "Schedule", "Next run", "Last", "scrub-local-tank", "Weekly, Sunday 03:00", "*/10 * * * *", "ok 2h ago", "never"] {
+            assert!(text.contains(want), "missing {want:?} in\n{text}");
+        }
+    }
+
+    #[test]
+    fn a_disabled_job_says_so_and_an_unsupported_one_shows_why() {
+        let mut off = a_job("off", "0 3 * * 0");
+        off.enabled = false;
+        let mut bad = a_job("bad", "0 3 * * 0");
+        bad.host = "gone".into();
+        let (mut app, _dir) = jobs_app(&[off, bad]);
+        let text = render(&mut app, 140, 30).join("\n");
+        assert!(text.contains("disabled"), "{text}");
+        assert!(text.contains("'gone' is not in config.toml"), "{text}");
+    }
+
+    #[test]
+    fn the_jobs_title_says_whether_the_daemon_is_running() {
+        let (mut app, dir) = jobs_app(&[a_job("a", "0 3 * * 0")]);
+        let text = render(&mut app, 120, 30).join("\n");
+        assert!(text.contains("daemon NOT running"), "{text}");
+        crate::daemon::heartbeat::write(&dir.path().join("daemon.json"), &crate::daemon::heartbeat::Heartbeat { pid: 4242, started_at: chrono::Utc::now(), last_tick: chrono::Utc::now() }).unwrap();
+        app.refresh_jobs();
+        let text = render(&mut app, 120, 30).join("\n");
+        assert!(text.contains("daemon running (pid 4242)"), "{text}");
+    }
+
+    #[test]
+    fn an_empty_jobs_list_explains_how_to_add_one_and_an_unreadable_file_is_flagged() {
+        let (mut app, dir) = jobs_app(&[]);
+        let text = render(&mut app, 120, 30).join("\n");
+        assert!(text.contains("No scheduled jobs") && text.contains("'n'"), "{text}");
+        std::fs::write(dir.path().join("jobs.toml"), "not [[[ toml").unwrap();
+        app.refresh_jobs();
+        let text = render(&mut app, 140, 30).join("\n");
+        assert!(text.contains("jobs.toml unreadable"), "{text}");
+    }
+
     #[test]
     fn tiny_terminals_have_no_metrics_panel_and_an_empty_list_says_nothing_is_selected() {
         let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
diff --git a/src/ui/state.rs b/src/ui/state.rs
index b677b33..57a25b2 100644
--- a/src/ui/state.rs
+++ b/src/ui/state.rs
@@ -7136,6 +7136,90 @@ mod tests {
         assert!(app.pools.is_empty() && !app.is_loading());
     }
 
+    fn app_with_jobs(jobs_list: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
+        let dir = tempfile::tempdir().unwrap();
+        crate::jobs::save(&dir.path().join("jobs.toml"), jobs_list).unwrap();
+        let mut app = App::new(vec!["local".to_string()]);
+        app.state_dir = dir.path().to_path_buf();
+        app.selected_section = app.sections.iter().position(|s| *s == Section::Jobs).unwrap();
+        (app, dir)
+    }
+
+    fn scrub_job(id: &str) -> crate::jobs::Job {
+        crate::jobs::Job { id: id.into(), host: "local".into(), kind: crate::jobs::KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * 0".into(), catch_up: true, enabled: true, created_at: None }
+    }
+
+    #[test]
+    fn jobs_is_the_last_section_and_tab_cycles_into_and_out_of_it() {
+        let mut app = App::new(vec!["local".to_string()]);
+        assert_eq!(app.sections.last(), Some(&Section::Jobs));
+        app.selected_section = app.sections.iter().position(|s| *s == Section::Shares).unwrap();
+        app.handle_key(KeyCode::Tab);
+        assert_eq!(app.current_section(), Section::Jobs);
+        app.handle_key(KeyCode::Tab);
+        assert_eq!(app.current_section(), Section::Hosts, "wraps to the first section");
+    }
+
+    #[test]
+    fn refresh_jobs_reads_the_state_dir_and_keeps_the_selection_on_the_same_job() {
+        let (mut app, dir) = app_with_jobs(&[scrub_job("a"), scrub_job("b")]);
+        assert!(!app.jobs_view.loaded);
+        app.refresh_jobs();
+        assert_eq!(app.jobs_view.jobs.len(), 2);
+        app.jobs_table.select(Some(1));
+        // A job is added before "b": "b" moves to row 2 and must stay selected.
+        crate::jobs::save(&dir.path().join("jobs.toml"), &[scrub_job("a"), scrub_job("new"), scrub_job("b")]).unwrap();
+        app.refresh_jobs();
+        assert_eq!(app.jobs_table.selected(), Some(2));
+        // The selected job disappears: the selection clamps instead of dangling.
+        crate::jobs::save(&dir.path().join("jobs.toml"), &[scrub_job("a")]).unwrap();
+        app.refresh_jobs();
+        assert_eq!(app.jobs_table.selected(), Some(0));
+        crate::jobs::save(&dir.path().join("jobs.toml"), &[]).unwrap();
+        app.refresh_jobs();
+        assert_eq!(app.jobs_table.selected(), None);
+    }
+
+    #[test]
+    fn up_and_down_move_the_jobs_selection_and_wrap() {
+        let (mut app, _dir) = app_with_jobs(&[scrub_job("a"), scrub_job("b")]);
+        app.refresh_jobs();
+        app.handle_key(KeyCode::Down);
+        assert_eq!(app.jobs_table.selected(), Some(0));
+        app.handle_key(KeyCode::Down);
+        assert_eq!(app.jobs_table.selected(), Some(1));
+        app.handle_key(KeyCode::Down);
+        assert_eq!(app.jobs_table.selected(), Some(0));
+        app.handle_key(KeyCode::Up);
+        assert_eq!(app.jobs_table.selected(), Some(1));
+    }
+
+    #[test]
+    fn jobs_refresh_happens_at_once_then_every_two_seconds_in_the_section_and_ten_elsewhere() {
+        let (mut app, dir) = app_with_jobs(&[scrub_job("a")]);
+        let t0 = Instant::now();
+        app.refresh_jobs_if_stale(t0);
+        assert!(app.jobs_view.loaded, "the first call loads immediately");
+        crate::jobs::save(&dir.path().join("jobs.toml"), &[scrub_job("a"), scrub_job("b")]).unwrap();
+        app.refresh_jobs_if_stale(t0 + Duration::from_secs(1));
+        assert_eq!(app.jobs_view.jobs.len(), 1, "not yet: two seconds apply while the section is on screen");
+        app.refresh_jobs_if_stale(t0 + Duration::from_secs(2));
+        assert_eq!(app.jobs_view.jobs.len(), 2);
+        crate::jobs::save(&dir.path().join("jobs.toml"), &[scrub_job("a")]).unwrap();
+        app.selected_section = 0;
+        app.refresh_jobs_if_stale(t0 + Duration::from_secs(5));
+        assert_eq!(app.jobs_view.jobs.len(), 2, "ten seconds apply from another section");
+        app.refresh_jobs_if_stale(t0 + Duration::from_secs(12));
+        assert_eq!(app.jobs_view.jobs.len(), 1);
+    }
+
+    #[test]
+    fn refresh_visible_in_the_jobs_section_reloads_without_a_runner() {
+        let (mut app, _dir) = app_with_jobs(&[scrub_job("a")]);
+        app.refresh_visible(Arc::new(FakeRunner::new("")));
+        assert_eq!(app.jobs_view.jobs.len(), 1);
+    }
+
     #[test]
     fn help_text_documents_the_global_keys() {
         let text = help_text();
PATCH
````

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test 2>&1 | grep -E "^error" -A3 | head -12`
Expected: the build FAILS because the code the tests use does not exist yet: `cannot find function schedule_label`, `next_run_text`.

- [ ] **Step 3: Apply the implementation**

This patch also updates the five existing tests that assumed Shares is the last section (two wrap-around expectations, and three that counted `BackTab` presses back to Snapshots; they now select Snapshots by position). That is a deliberate change to the tests, not a loosening of them.

````bash
git apply <<'PATCH'
diff --git a/src/ui/hints.rs b/src/ui/hints.rs
index 94b0b69..a460eb7 100644
--- a/src/ui/hints.rs
+++ b/src/ui/hints.rs
@@ -247,6 +247,12 @@ fn context_hints(app: &App) -> Vec<Hint> {
             }
             v.push(h("r", "Refresh", 4));
         }
+        (Section::Jobs, _) => {
+            if !app.jobs_view.jobs.is_empty() {
+                v.push(h("↑↓", "Move", 1));
+            }
+            v.push(h("r", "Refresh", 4));
+        }
         // Never in `App::sections`; listed so a future section forces a decision here.
         (Section::Vdevs | Section::PoolProperties | Section::DatasetProperties | Section::DatasetPermissions | Section::DatasetAcl, _) => {}
     }
diff --git a/src/ui/jobs.rs b/src/ui/jobs.rs
index 9259073..746e9b1 100644
--- a/src/ui/jobs.rs
+++ b/src/ui/jobs.rs
@@ -36,6 +36,20 @@ pub fn describe(expr: &str) -> String {
     PRESETS.iter().find(|(cron, _)| *cron == expr).map_or_else(|| "custom schedule".to_string(), |(_, label)| (*label).to_string())
 }
 
+/// The table's schedule column: a preset's name, or the cron text of a custom schedule.
+pub fn schedule_label(expr: &str) -> String {
+    let described = describe(expr);
+    if described == "custom schedule" { expr.trim().to_string() } else { described }
+}
+
+/// The table's next-run column: day and time in `now`'s zone, or `never`.
+pub fn next_run_text<Tz: TimeZone>(expr: &str, now: &DateTime<Tz>) -> String
+where
+    Tz::Offset: std::fmt::Display,
+{
+    Schedule::parse(expr).ok().and_then(|s| s.next_after(now)).map_or_else(|| "never".to_string(), |t| t.format("%a %m-%d %H:%M").to_string())
+}
+
 /// The form's preview under the Schedule field: what the schedule is called
 /// and its next three run times in `now`'s zone, or why it does not parse.
 pub fn preview_lines<Tz: TimeZone>(expr: &str, now: &DateTime<Tz>) -> Vec<String>
@@ -61,10 +75,13 @@ where
 pub fn new_job_id(host: &str, pool: &str, existing: &[String]) -> String {
     let safe = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '-' }).collect::<String>();
     let base = format!("scrub-{}-{}", safe(host), safe(pool));
-    if !existing.contains(&base) {
-        return base;
+    let mut candidate = base.clone();
+    let mut n = 1_u32;
+    while existing.contains(&candidate) {
+        n += 1;
+        candidate = format!("{base}-{n}");
     }
-    (2..).map(|n| format!("{base}-{n}")).find(|candidate| !existing.contains(candidate)).unwrap_or(base)
+    candidate
 }
 
 /// `just now`, `45m ago`, `3h ago`, `2d ago`. A time in the future reads as `just now`.
diff --git a/src/ui/layout.rs b/src/ui/layout.rs
index 3a66f88..a70b9a2 100644
--- a/src/ui/layout.rs
+++ b/src/ui/layout.rs
@@ -152,6 +152,7 @@ fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
                 Section::Snapshots => "Snapshots",
                 Section::Disks => "Disks",
                 Section::Shares => "Shares",
+                Section::Jobs => "Jobs",
                 Section::Vdevs => "Vdevs",
                 Section::PoolProperties => "Pool Properties",
                 Section::DatasetProperties => "Dataset Properties",
@@ -196,6 +197,7 @@ fn draw_main(f: &mut Frame, app: &mut App, area: Rect) {
         Section::Snapshots => draw_snapshots(f, app, area),
         Section::Disks => draw_disks(f, app, area),
         Section::Shares => draw_shares(f, app, area),
+        Section::Jobs => draw_jobs(f, app, area),
         Section::Pools | Section::Datasets => unreachable!("handled above"),
         Section::Vdevs | Section::PoolProperties | Section::DatasetProperties | Section::DatasetPermissions | Section::DatasetAcl => {
             unreachable!("never in App::sections")
@@ -579,6 +581,49 @@ pub(crate) fn smb_access_label(guest_ok: bool, valid_user: Option<&str>) -> Stri
     }
 }
 
+fn draw_jobs(f: &mut Frame, app: &mut App, area: Rect) {
+    use crate::ui::jobs::{last_result_text, next_run_text, schedule_label, DaemonState};
+    let theme = app.theme.clone();
+    let now = chrono::Utc::now();
+    let local_now = chrono::Local::now();
+    let daemon = match app.jobs_view.daemon(now) {
+        DaemonState::Running { pid } => Span::raw(format!("daemon running (pid {pid})")),
+        DaemonState::NotRunning => Span::styled("daemon NOT running: jobs will not fire".to_string(), theme.err),
+    };
+    let mut title = vec![Span::raw("Jobs — "), daemon];
+    if app.jobs_view.error.is_some() {
+        title.push(Span::styled(" — jobs.toml unreadable: showing the last good list".to_string(), theme.err));
+    }
+    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(Line::from(title));
+    if app.jobs_view.loaded && app.jobs_view.jobs.is_empty() {
+        let text = "No scheduled jobs. Press 'n' to add one, or 'J' on a pool in the Pools section.";
+        f.render_widget(Paragraph::new(text).style(theme.dim()).wrap(Wrap { trim: false }).block(block), area);
+        return;
+    }
+    let rows: Vec<Row> = app
+        .jobs_view
+        .jobs
+        .iter()
+        .map(|j| {
+            let last = app.jobs_view.last_run(&j.id);
+            let (next, last_text, style) = if let Some(reason) = app.jobs_view.unsupported.get(&j.id) {
+                ("-".to_string(), reason.clone(), theme.err)
+            } else if !j.enabled {
+                ("disabled".to_string(), last_result_text(last, now), theme.dim())
+            } else {
+                (next_run_text(&j.schedule, &local_now), last_result_text(last, now), Style::default())
+            };
+            Row::new(vec![j.id.clone(), j.host.clone(), j.pool.clone().unwrap_or_default(), schedule_label(&j.schedule), next, last_text]).style(style)
+        })
+        .collect();
+    let widths = [Constraint::Length(22), Constraint::Length(8), Constraint::Length(10), Constraint::Length(21), Constraint::Length(14), Constraint::Min(10)];
+    let table = Table::new(rows, widths)
+        .header(Row::new(vec!["Job", "Host", "Pool", "Schedule", "Next run", "Last"]))
+        .block(block)
+        .row_highlight_style(theme.selected_row());
+    f.render_stateful_widget(table, area, &mut app.jobs_table);
+}
+
 fn draw_shares(f: &mut Frame, app: &mut App, area: Rect) {
     let theme = app.theme.clone();
     let rows = app.shares.iter().map(|s| match s {
diff --git a/src/ui/mod.rs b/src/ui/mod.rs
index 97397fc..60ff5c8 100644
--- a/src/ui/mod.rs
+++ b/src/ui/mod.rs
@@ -152,6 +152,7 @@ fn event_loop(
     let signals = shell::ShellSignals::install().map_err(|e| e.to_string());
     loop {
         let refreshes = app.drain_worker_messages();
+        app.refresh_jobs_if_stale(Instant::now());
         apply_pending_new_hosts(app, inventory, config_path);
         apply_pending_account_fetches(app, inventory);
         app.clear_stale_scan_status();
@@ -176,6 +177,7 @@ fn event_loop(
                     Section::Snapshots => app.refresh_snapshots(host.runner.clone()),
                     Section::Disks => app.refresh_disks(host.runner.clone()),
                     Section::Shares => app.refresh_shares(host.runner.clone()),
+                    Section::Jobs => app.refresh_jobs(),
                     Section::Vdevs => {
                         if let Some(pool) = app.drilled_pool.clone() {
                             app.refresh_vdevs(host.runner.clone(), pool);
@@ -263,6 +265,7 @@ fn event_loop(
                                     Section::Snapshots => app.refresh_snapshots(host.runner.clone()),
                                     Section::Disks => app.refresh_disks(host.runner.clone()),
                                     Section::Shares => app.refresh_shares(host.runner.clone()),
+                                    Section::Jobs => app.refresh_jobs(),
                                     Section::Hosts
                                     | Section::Vdevs
                                     | Section::PoolProperties
diff --git a/src/ui/state.rs b/src/ui/state.rs
index 57a25b2..7f64f28 100644
--- a/src/ui/state.rs
+++ b/src/ui/state.rs
@@ -28,6 +28,7 @@ pub enum Section {
     Snapshots,
     Disks,
     Shares,
+    Jobs,
     Vdevs,
     PoolProperties,
     DatasetProperties,
@@ -165,6 +166,11 @@ pub struct App {
     pub selected_disks: HashSet<String>,
     pub shares: Vec<crate::zfs::share::Share>,
     pub shares_table: TableState,
+    pub jobs_view: crate::ui::jobs::JobsView,
+    pub jobs_table: TableState,
+    /// Where `jobs.toml`, `history.jsonl` and `daemon.json` live: the daemon's state directory.
+    pub state_dir: std::path::PathBuf,
+    last_jobs_refresh: Option<Instant>,
     /// `None` means "not yet checked" — kept distinct from `Some(false)`
     /// so the Shares title doesn't flash a false "unavailable" hint
     /// before the first refresh, consistent with every other section
@@ -270,7 +276,7 @@ impl App {
     pub fn new(hosts: Vec<String>) -> Self {
         let (tx, rx) = std::sync::mpsc::channel();
         App {
-            sections: vec![Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots, Section::Disks, Section::Shares],
+            sections: vec![Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots, Section::Disks, Section::Shares, Section::Jobs],
             selected_section: 0,
             hosts,
             selected_host: 0,
@@ -285,6 +291,10 @@ impl App {
             selected_disks: HashSet::new(),
             shares: Vec::new(),
             shares_table: TableState::default(),
+            jobs_view: crate::ui::jobs::JobsView::default(),
+            jobs_table: TableState::default(),
+            state_dir: std::path::PathBuf::from(crate::daemon::engine::DEFAULT_STATE_DIR),
+            last_jobs_refresh: None,
             nfs_available: None,
             samba_available: None,
             shares_loaded: false,
@@ -482,10 +492,33 @@ impl App {
             Section::Snapshots => self.refresh_snapshots(runner),
             Section::Disks => self.refresh_disks(runner),
             Section::Shares => self.refresh_shares(runner),
+            Section::Jobs => self.refresh_jobs(),
             _ => {}
         }
     }
 
+    /// Reloads the Jobs data from the state directory. These are small local files, so
+    /// there is no runner and no thread. The selection stays on the same job.
+    pub fn refresh_jobs(&mut self) {
+        let selected = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)).map(|j| j.id.clone());
+        self.jobs_view = crate::ui::jobs::load(&self.state_dir, &self.hosts, Some(&self.jobs_view));
+        match selected.and_then(|id| self.jobs_view.jobs.iter().position(|j| j.id == id)) {
+            Some(i) => self.jobs_table.select(Some(i)),
+            None => clamp_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len()),
+        }
+    }
+
+    /// Refreshes the Jobs data if it is stale: every 2 seconds while the section is on screen
+    /// (so a run's result appears live), every 10 otherwise (so the failed-jobs indicator is
+    /// right from any section). The first call always loads.
+    pub fn refresh_jobs_if_stale(&mut self, now: Instant) {
+        let every = if self.current_section() == Section::Jobs { Duration::from_secs(2) } else { Duration::from_secs(10) };
+        if self.last_jobs_refresh.is_none_or(|t| now.duration_since(t) >= every) {
+            self.last_jobs_refresh = Some(now);
+            self.refresh_jobs();
+        }
+    }
+
     /// Sets a failure message (rendered with the theme's `err` style).
     pub fn set_error(&mut self, msg: String) {
         self.status_error = Some(msg.clone());
@@ -1127,6 +1160,12 @@ impl App {
             KeyCode::Up if self.current_section() == Section::Shares => {
                 move_table_selection(&mut self.shares_table, self.shares.len(), false);
             }
+            KeyCode::Down if self.current_section() == Section::Jobs => {
+                move_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len(), true);
+            }
+            KeyCode::Up if self.current_section() == Section::Jobs => {
+                move_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len(), false);
+            }
             KeyCode::Enter if self.current_section() == Section::Shares => {
                 if let Some(i) = self.shares_table.selected() {
                     if let Some(share) = self.shares.get(i) {
@@ -3117,7 +3156,7 @@ mod tests {
         app.handle_key(KeyCode::BackTab);
         assert_eq!(app.current_section(), Section::Hosts);
         app.handle_key(KeyCode::BackTab);
-        assert_eq!(app.current_section(), Section::Shares);
+        assert_eq!(app.current_section(), Section::Jobs);
     }
 
     #[test]
@@ -3392,9 +3431,7 @@ mod tests {
             Snapshot { dataset: "tank/data".to_string(), name: "a".to_string(), used: 0 },
             Snapshot { dataset: "tank/data".to_string(), name: "b".to_string(), used: 0 },
         ];
-        app.handle_key(KeyCode::BackTab); // Hosts -> Shares (wraps backward)
-        app.handle_key(KeyCode::BackTab); // Shares -> Disks
-        app.handle_key(KeyCode::BackTab); // Disks -> Snapshots
+        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
         assert_eq!(app.current_section(), Section::Snapshots);
         app.handle_key(KeyCode::Down);
         assert_eq!(app.snapshots_table.selected(), Some(0));
@@ -3924,9 +3961,7 @@ mod tests {
     fn handle_key_d_on_snapshots_opens_destroy_confirm_typed_modal() {
         let mut app = App::new(vec!["local".to_string()]);
         app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "old-1".to_string(), used: 0 }];
-        app.handle_key(KeyCode::BackTab); // Hosts -> Shares (wraps backward)
-        app.handle_key(KeyCode::BackTab); // Shares -> Disks
-        app.handle_key(KeyCode::BackTab); // Disks -> Snapshots
+        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
         app.snapshots_table.select(Some(0));
         app.handle_key(KeyCode::Char('d'));
         match app.modal {
@@ -3945,9 +3980,7 @@ mod tests {
     fn handle_key_b_on_snapshots_opens_rollback_confirm_typed_modal() {
         let mut app = App::new(vec!["local".to_string()]);
         app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string(), used: 0 }];
-        app.handle_key(KeyCode::BackTab); // Hosts -> Shares (wraps backward)
-        app.handle_key(KeyCode::BackTab); // Shares -> Disks
-        app.handle_key(KeyCode::BackTab); // Disks -> Snapshots
+        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
         app.snapshots_table.select(Some(0));
         app.handle_key(KeyCode::Char('b'));
         match app.modal {
@@ -4384,7 +4417,7 @@ mod tests {
         let mut app = App::new(vec!["local".to_string()]);
         assert_eq!(app.current_section(), Section::Hosts);
         app.handle_key(KeyCode::BackTab);
-        assert_eq!(app.current_section(), Section::Shares, "must wrap backward from the first section to the last");
+        assert_eq!(app.current_section(), Section::Jobs, "must wrap backward from the first section to the last");
     }
 
     #[test]
PATCH
````

- [ ] **Step 4: Run the whole suite**

Run: `cargo test 2>&1 | grep -E "^error|^test .* FAILED|^test result" -A6`
Expected: `test result: ok. 846 passed; 0 failed`.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" -A4 | grep -E "^warning|-->" | paste - - | grep -E "src/(ui|jobs)"`
Expected: no output for these files (a `never used`/`never constructed` warning for code a later task wires up is fine).

```bash
git add -A
git commit -m "test: the Jobs section's state and table; feat: the Jobs section: state, refresh, selection and the table

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 4: The job form: open, validate, save, pickers and a live preview

**Files (created or modified):**
- `src/ui/hints.rs`
- `src/ui/layout.rs`
- `src/ui/modal.rs`
- `src/ui/picker.rs`
- `src/ui/state.rs`

**Interfaces:**
- Consumes: `SaveJob` (Task 2), `JobsView`, `new_job_id`, `preview_lines`, `preset_values`, `insert_text` (Task 1).
- Produces: `FormKind::Job { hosts, existing, taken_ids }` (fields `[host, pool, schedule, catch up]`); `J` in Pools, `n` and `Enter` in Jobs open it; `build_job_action(kind, fields, state_dir, now) -> Result<PendingAction, String>`; `F2` lists hosts (field 0) and presets (field 2); a live preview under the fields (`form_preview` in `layout.rs`).

- [ ] **Step 1: Apply the failing tests**

````bash
git apply <<'PATCH'
diff --git a/src/ui/layout.rs b/src/ui/layout.rs
index a70b9a2..1868dae 100644
--- a/src/ui/layout.rs
+++ b/src/ui/layout.rs
@@ -1294,6 +1294,35 @@ mod tests {
         assert!(text.contains("jobs.toml unreadable"), "{text}");
     }
 
+    fn job_form_app() -> App {
+        let mut app = App::new(vec!["local".to_string()]);
+        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
+        app.pools = vec![pool("tank", 10, 100, PoolHealth::Online)];
+        app.pools_table.select(Some(0));
+        app.handle_key(crossterm::event::KeyCode::Char('J'));
+        app
+    }
+
+    #[test]
+    fn the_job_form_previews_what_the_schedule_means_and_when_it_next_runs() {
+        let mut app = job_form_app();
+        let text = render(&mut app, 100, 30).join("\n");
+        for want in ["New scrub job", "Host: local", "Pool: tank", "Schedule: 0 3 * * 0", "Weekly, Sunday 03:00", "Next: Sun "] {
+            assert!(text.contains(want), "missing {want:?} in\n{text}");
+        }
+        assert_eq!(text.matches("03:00").count(), 4, "the label and the three listed run times:\n{text}");
+    }
+
+    #[test]
+    fn the_job_forms_preview_follows_what_is_typed_and_shows_a_parse_error() {
+        let mut app = job_form_app();
+        let Some(crate::ui::modal::Modal::Form { fields, .. }) = &mut app.modal else { panic!() };
+        fields[2].value = "61 * * * *".to_string();
+        let text = render(&mut app, 100, 30).join("\n");
+        assert!(text.contains("Schedule error:") && text.contains("outside 0-59"), "{text}");
+        assert!(!text.contains("Next:"));
+    }
+
     #[test]
     fn tiny_terminals_have_no_metrics_panel_and_an_empty_list_says_nothing_is_selected() {
         let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
diff --git a/src/ui/state.rs b/src/ui/state.rs
index 7f64f28..f5eb640 100644
--- a/src/ui/state.rs
+++ b/src/ui/state.rs
@@ -7253,6 +7253,181 @@ mod tests {
         assert_eq!(app.jobs_view.jobs.len(), 1);
     }
 
+    fn pools_app_with_one_pool() -> App {
+        let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
+        app.selected_host = 1;
+        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
+        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
+        app.pools_table.select(Some(0));
+        app
+    }
+
+    fn form_values(app: &App) -> Vec<String> {
+        match &app.modal {
+            Some(Modal::Form { fields, .. }) => fields.iter().map(|f| f.value.clone()).collect(),
+            other => panic!("expected a form, got {other:?}"),
+        }
+    }
+
+    fn set_form_values(app: &mut App, values: [&str; 4]) {
+        let Some(Modal::Form { fields, .. }) = &mut app.modal else { panic!("no form open") };
+        for (field, value) in fields.iter_mut().zip(values) {
+            field.value = value.to_string();
+        }
+    }
+
+    fn at_utc(s: &str) -> chrono::DateTime<chrono::Utc> {
+        chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc)
+    }
+
+    #[test]
+    fn j_on_a_pool_opens_the_job_form_with_the_host_and_pool_filled_in() {
+        let mut app = pools_app_with_one_pool();
+        app.handle_key(KeyCode::Char('J'));
+        assert_eq!(form_values(&app), vec!["fbsd16", "tank", "0 3 * * 0", "y"]);
+        let Some(Modal::Form { title, kind, .. }) = &app.modal else { panic!() };
+        assert!(title.contains("New scrub job"), "{title}");
+        assert!(matches!(kind, FormKind::Job { existing: None, .. }));
+    }
+
+    #[test]
+    fn j_with_no_pool_selected_opens_nothing_and_says_why() {
+        let mut app = pools_app_with_one_pool();
+        app.pools_table.select(None);
+        app.handle_key(KeyCode::Char('J'));
+        assert!(app.modal.is_none());
+        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Select a pool")), "{:?}", app.status_message);
+    }
+
+    #[test]
+    fn n_in_the_jobs_section_opens_a_blank_pool_form_for_the_selected_host() {
+        let (mut app, _dir) = app_with_jobs(&[]);
+        app.refresh_jobs();
+        app.handle_key(KeyCode::Char('n'));
+        assert_eq!(form_values(&app), vec!["local", "", "0 3 * * 0", "y"]);
+    }
+
+    #[test]
+    fn enter_on_a_job_opens_the_form_with_its_values_and_nothing_without_a_selection() {
+        let mut job = scrub_job("scrub-local-tank");
+        job.schedule = "*/10 * * * *".into();
+        job.catch_up = false;
+        let (mut app, _dir) = app_with_jobs(&[job]);
+        app.refresh_jobs();
+        app.handle_key(KeyCode::Enter);
+        assert!(app.modal.is_none(), "no row is selected yet");
+        app.handle_key(KeyCode::Down);
+        app.handle_key(KeyCode::Enter);
+        assert_eq!(form_values(&app), vec!["local", "tank", "*/10 * * * *", "n"]);
+        let Some(Modal::Form { title, kind, .. }) = &app.modal else { panic!() };
+        assert!(title.contains("scrub-local-tank"), "{title}");
+        assert!(matches!(kind, FormKind::Job { existing: Some(_), .. }));
+    }
+
+    fn job_kind(existing: Option<crate::jobs::Job>, taken: &[&str]) -> FormKind {
+        FormKind::Job { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), taken_ids: taken.iter().map(|s| (*s).to_string()).collect() }
+    }
+
+    fn fields(values: [&str; 4]) -> Vec<FormField> {
+        ["Host", "Pool", "Schedule", "Catch up (y/n)"].iter().zip(values).map(|(l, v)| FormField { label: (*l).to_string(), value: v.to_string() }).collect()
+    }
+
+    #[test]
+    fn a_new_job_gets_an_id_a_creation_time_and_is_enabled() {
+        let now = at_utc("2026-10-06T12:00:00Z");
+        let action = build_job_action(&job_kind(None, &[]), &fields(["fbsd16", " tank ", " 0 2 * * * ", "y"]), std::path::Path::new("/state"), now).unwrap();
+        assert_eq!(action.section, Section::Jobs);
+        let MutationCall::SaveJob { state_dir, job } = action.call else { panic!("{:?}", action.call) };
+        assert_eq!(state_dir, std::path::PathBuf::from("/state"));
+        assert_eq!((job.id.as_str(), job.host.as_str(), job.pool.as_deref(), job.schedule.as_str()), ("scrub-fbsd16-tank", "fbsd16", Some("tank"), "0 2 * * *"));
+        assert!(job.catch_up && job.enabled && job.kind == crate::jobs::KIND_SCRUB);
+        assert_eq!(job.created_at, Some(now));
+    }
+
+    #[test]
+    fn a_colliding_id_gets_a_numeric_suffix() {
+        let now = at_utc("2026-10-06T12:00:00Z");
+        let action = build_job_action(&job_kind(None, &["scrub-local-tank"]), &fields(["local", "tank", "0 3 * * 0", "y"]), std::path::Path::new("/s"), now).unwrap();
+        let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
+        assert_eq!(job.id, "scrub-local-tank-2");
+    }
+
+    #[test]
+    fn an_edit_changes_the_fields_but_keeps_id_creation_time_and_enabled() {
+        let mut existing = scrub_job("my-job");
+        existing.enabled = false;
+        existing.created_at = Some(at_utc("2026-09-01T00:00:00Z"));
+        let action = build_job_action(&job_kind(Some(existing), &["my-job"]), &fields(["fbsd16", "other", "0 4 * * *", "n"]), std::path::Path::new("/s"), at_utc("2026-10-06T12:00:00Z")).unwrap();
+        let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
+        assert_eq!((job.id.as_str(), job.host.as_str(), job.pool.as_deref(), job.schedule.as_str(), job.catch_up), ("my-job", "fbsd16", Some("other"), "0 4 * * *", false));
+        assert!(!job.enabled, "an edit does not re-enable a disabled job");
+        assert_eq!(job.created_at, Some(at_utc("2026-09-01T00:00:00Z")));
+    }
+
+    #[test]
+    fn a_bad_form_is_refused_with_the_reason() {
+        let now = at_utc("2026-10-06T12:00:00Z");
+        let try_it = |values: [&str; 4]| build_job_action(&job_kind(None, &[]), &fields(values), std::path::Path::new("/s"), now).unwrap_err();
+        assert!(try_it(["nowhere", "tank", "0 3 * * 0", "y"]).contains("Unknown host 'nowhere'"));
+        assert!(try_it(["local", "  ", "0 3 * * 0", "y"]).contains("Pool name is required"));
+        assert!(try_it(["local", "tank", "61 * * * *", "y"]).contains("outside 0-59"));
+        assert!(try_it(["local", "tank", "0 3 * * 0", "maybe"]).contains("y or n"));
+    }
+
+    #[test]
+    fn submitting_a_good_form_returns_the_save_action_and_a_bad_one_stays_open_on_the_first_field() {
+        let mut app = pools_app_with_one_pool();
+        app.handle_key(KeyCode::Char('J'));
+        set_form_values(&mut app, ["fbsd16", "tank", "61 * * * *", "y"]);
+        for _ in 0..3 {
+            assert!(app.handle_modal_key(KeyCode::Enter).is_none());
+        }
+        assert!(app.handle_modal_key(KeyCode::Enter).is_none(), "the schedule is bad");
+        assert!(app.modal.is_some(), "the form stays open");
+        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("outside 0-59")), "{:?}", app.status_message);
+        assert!(app.status_is_error());
+        let Some(Modal::Form { focused, .. }) = &app.modal else { panic!() };
+        assert_eq!(*focused, 0, "focus returns to the first field, as the SMB form does");
+        set_form_values(&mut app, ["fbsd16", "tank", "0 3 * * 0", "y"]);
+        for _ in 0..3 {
+            assert!(app.handle_modal_key(KeyCode::Enter).is_none());
+        }
+        let action = app.handle_modal_key(KeyCode::Enter).expect("the form submits");
+        assert!(matches!(action.call, MutationCall::SaveJob { .. }));
+        assert!(app.modal.is_none());
+    }
+
+    #[test]
+    fn f2_on_host_lists_the_configured_hosts_and_on_schedule_the_presets_inserting_only_the_cron_part() {
+        let mut app = pools_app_with_one_pool();
+        app.handle_key(KeyCode::Char('J'));
+        app.handle_modal_key(KeyCode::F(2));
+        let picker = app.picker.clone().expect("a host picker");
+        assert_eq!(picker.values, vec!["local", "fbsd16"]);
+        assert!(!picker.multi_select);
+        app.handle_picker_key(KeyCode::Down);
+        app.handle_picker_key(KeyCode::Enter);
+        assert_eq!(form_values(&app)[0], "fbsd16".to_string());
+        app.handle_modal_key(KeyCode::Tab);
+        app.handle_modal_key(KeyCode::Tab);
+        app.handle_modal_key(KeyCode::F(2));
+        let presets = app.picker.clone().expect("a schedule picker");
+        assert_eq!(presets.values, crate::ui::jobs::preset_values());
+        assert_eq!(presets.selected, 2, "the picker opens on the field's current preset");
+        app.handle_picker_key(KeyCode::Up);
+        app.handle_picker_key(KeyCode::Enter);
+        assert_eq!(form_values(&app)[2], "0 2 * * *", "no label text lands in the field");
+    }
+
+    #[test]
+    fn f2_on_pool_and_catch_up_does_nothing() {
+        let mut app = pools_app_with_one_pool();
+        app.handle_key(KeyCode::Char('J'));
+        app.handle_modal_key(KeyCode::Tab);
+        app.handle_modal_key(KeyCode::F(2));
+        assert!(app.picker.is_none());
+    }
+
     #[test]
     fn help_text_documents_the_global_keys() {
         let text = help_text();
PATCH
````

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test 2>&1 | grep -E "^error" -A3 | head -12`
Expected: the build FAILS because the code the tests use does not exist yet: `no variant named Job found for enum FormKind`.

- [ ] **Step 3: Apply the implementation**

````bash
git apply <<'PATCH'
diff --git a/src/ui/hints.rs b/src/ui/hints.rs
index a460eb7..8a4ddb1 100644
--- a/src/ui/hints.rs
+++ b/src/ui/hints.rs
@@ -140,6 +140,7 @@ fn context_hints(app: &App) -> Vec<Hint> {
             v.push(h("→", "Properties", 2));
             if selected {
                 v.push(h("s", "Scrub", 2));
+                v.push(h("J", "Schedule", 3));
                 v.push(h("R", "Resilver", 3));
                 v.push(h("d", "Destroy", 2).tone(Tone::Danger));
             }
@@ -251,6 +252,12 @@ fn context_hints(app: &App) -> Vec<Hint> {
             if !app.jobs_view.jobs.is_empty() {
                 v.push(h("↑↓", "Move", 1));
             }
+            if app.jobs_view.jobs.is_empty() || app.jobs_table.selected().is_none() {
+                v.push(h("n", "New job", 2));
+            } else {
+                v.push(h("Enter", "Edit", 1));
+                v.push(h("n", "New job", 2));
+            }
             v.push(h("r", "Refresh", 4));
         }
         // Never in `App::sections`; listed so a future section forces a decision here.
diff --git a/src/ui/layout.rs b/src/ui/layout.rs
index 1868dae..b383728 100644
--- a/src/ui/layout.rs
+++ b/src/ui/layout.rs
@@ -668,6 +668,15 @@ fn confirm_block(theme: &Theme, tone: Tone, label: &str, neutral_title: &str, ri
     Block::default().borders(Borders::ALL).border_style(border).title(Span::styled(title, border))
 }
 
+/// Extra lines drawn under a form's fields, computed from what is typed so far: for a job
+/// form, what the schedule means and its next three run times.
+fn form_preview(kind: &crate::ui::modal::FormKind, fields: &[crate::ui::modal::FormField]) -> Vec<String> {
+    match kind {
+        crate::ui::modal::FormKind::Job { .. } => crate::ui::jobs::preview_lines(fields.get(2).map_or("", |f| f.value.as_str()), &chrono::Local::now()),
+        _ => Vec::new(),
+    }
+}
+
 fn draw_modal(f: &mut Frame, modal: &Modal, theme: &Theme, area: Rect) {
     // Estimate the popup's eventual inner width (minus borders) so we
     // can reserve enough height up front — sizing the popup by a flat
@@ -682,11 +691,14 @@ fn draw_modal(f: &mut Frame, modal: &Modal, theme: &Theme, area: Rect) {
         Modal::ConfirmTyped { prompt, input, .. } => {
             wrapped_line_count(&format!("{prompt}\n\n> {input}"), inner_width)
         }
-        Modal::Form { fields, description, .. } => {
-            let fields_lines: u16 = fields
-                .iter()
-                .map(|field| wrapped_line_count(&format!("  {}: {}", field.label, field.value), inner_width))
-                .sum();
+        Modal::Form { fields, description, kind, .. } => {
+            let preview = form_preview(kind, fields);
+            let preview_lines: u16 = if preview.is_empty() { 0 } else { 1 + preview.iter().map(|l| wrapped_line_count(l, inner_width)).sum::<u16>() };
+            let fields_lines: u16 = preview_lines
+                + fields
+                    .iter()
+                    .map(|field| wrapped_line_count(&format!("  {}: {}", field.label, field.value), inner_width))
+                    .sum::<u16>();
             match description {
                 // +1 blank separator line between the description and the fields.
                 // Word wrapping can need more rows than the byte-based
@@ -738,6 +750,11 @@ fn draw_modal(f: &mut Frame, modal: &Modal, theme: &Theme, area: Rect) {
                 let shown = if kind.masks_field(i) { "*".repeat(field.value.chars().count()) } else { field.value.clone() };
                 lines.push(format!("{marker} {}: {shown}", field.label));
             }
+            let preview = form_preview(kind, fields);
+            if !preview.is_empty() {
+                lines.push(String::new());
+                lines.extend(preview);
+            }
             let p = Paragraph::new(lines.join("\n"))
                 .wrap(Wrap { trim: false })
                 .block(Block::default().borders(Borders::ALL).border_style(theme.accent).title(title.as_str()));
diff --git a/src/ui/modal.rs b/src/ui/modal.rs
index 8ea184a..8f05372 100644
--- a/src/ui/modal.rs
+++ b/src/ui/modal.rs
@@ -190,6 +190,10 @@ pub struct FormField {
 /// match the order the modal's `fields` were constructed in.
 #[derive(Debug, Clone, PartialEq)]
 pub enum FormKind {
+    /// Fields: `[host, pool, schedule, catch_up]`. `hosts` are the configured host names (for the
+    /// host picker and for validation); `existing` is the job being edited, if any; `taken_ids`
+    /// are the job ids already in use, so a new job can be named without a collision.
+    Job { hosts: Vec<String>, existing: Option<Box<crate::jobs::Job>>, taken_ids: Vec<String> },
     /// Fields: `[name, vdev_spec]`.
     CreatePool { host: usize },
     /// Fields: `[new_disk]`.
diff --git a/src/ui/picker.rs b/src/ui/picker.rs
index bc2464b..1c5df83 100644
--- a/src/ui/picker.rs
+++ b/src/ui/picker.rs
@@ -144,6 +144,18 @@ pub fn build_picker_state(kind: &FormKind, field_index: usize, current_value: &s
             account: Some((host, field)),
         });
     }
+    if let FormKind::Job { hosts, .. } = kind {
+        let (values, selected) = match field_index {
+            0 => (hosts.clone(), hosts.iter().position(|h| h == current_value.trim()).unwrap_or(0)),
+            2 => {
+                let presets = crate::ui::jobs::preset_values();
+                let selected = presets.iter().position(|p| crate::ui::jobs::insert_text(p) == current_value.trim()).unwrap_or(0);
+                (presets, selected)
+            }
+            _ => return None,
+        };
+        return Some(PickerState { values, filter: String::new(), selected, checked: std::collections::HashSet::new(), multi_select: false, load: PickerLoad::Ready, note: None, account: None });
+    }
     let (values, multi_select) = vocabulary_for(kind, field_index)?;
     let values: Vec<String> = values.iter().map(std::string::ToString::to_string).collect();
     let mut checked = std::collections::HashSet::new();
diff --git a/src/ui/state.rs b/src/ui/state.rs
index f5eb640..ce3cb68 100644
--- a/src/ui/state.rs
+++ b/src/ui/state.rs
@@ -1160,6 +1160,20 @@ impl App {
             KeyCode::Up if self.current_section() == Section::Shares => {
                 move_table_selection(&mut self.shares_table, self.shares.len(), false);
             }
+            KeyCode::Char('J') if self.current_section() == Section::Pools && self.drilled_pool.is_none() && self.pane_focus == PaneFocus::Middle => {
+                match self.selected_pool_name() {
+                    Some(pool) => self.modal = Some(self.job_form(None, &pool)),
+                    None => self.status_message = Some("Select a pool first, then press J to schedule a scrub for it".to_string()),
+                }
+            }
+            KeyCode::Char('n') if self.current_section() == Section::Jobs => {
+                self.modal = Some(self.job_form(None, ""));
+            }
+            KeyCode::Enter if self.current_section() == Section::Jobs => {
+                if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)).cloned() {
+                    self.modal = Some(self.job_form(Some(job.clone()), job.pool.as_deref().unwrap_or("")));
+                }
+            }
             KeyCode::Down if self.current_section() == Section::Jobs => {
                 move_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len(), true);
             }
@@ -1365,6 +1379,18 @@ impl App {
                             self.begin_add_host(name, target);
                             None
                         }
+                    } else if matches!(kind, FormKind::Job { .. }) {
+                        match build_job_action(&kind, &fields, &self.state_dir, chrono::Utc::now()) {
+                            Ok(mut action) => {
+                                action.host = self.selected_host;
+                                Some(action)
+                            }
+                            Err(msg) => {
+                                self.set_error(msg);
+                                self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
+                                None
+                            }
+                        }
                     } else if matches!(kind, FormKind::ShareSmb { .. }) {
                         match build_smb_action(&kind, &fields, &self.shares) {
                             Ok(action) => Some(action),
@@ -1505,7 +1531,7 @@ impl App {
                 } else if let Some(value) = filtered.get(picker.selected).cloned() {
                     if let Some(Modal::Form { fields, focused, .. }) = &mut self.modal {
                         if let Some(field) = fields.get_mut(*focused) {
-                            field.value = value;
+                            field.value = crate::ui::jobs::insert_text(&value).to_string();
                         }
                     }
                 } else {
@@ -2107,6 +2133,64 @@ fn build_vdev_spec_from_selection(redundancy: &str, disks: &[String]) -> Vec<Str
 /// (`self.selected_host` at every call site), threaded through into the
 /// resulting `FormKind` so the eventual mutation runs against the right
 /// host. SMB shares route to `smb_edit_modal` (managed smb4.conf block).
+/// The new/edit scrub-job form. `existing` is the job being edited; for a new job `pool`
+/// is what to pre-fill (a pool picked in the Pools section, or blank).
+impl App {
+    fn job_form(&self, existing: Option<crate::jobs::Job>, pool: &str) -> Modal {
+        let (host, schedule, catch_up, title) = match &existing {
+            Some(job) => (job.host.clone(), job.schedule.clone(), job.catch_up, format!("Edit scrub job '{}'", job.id)),
+            None => (self.active_host_name().to_string(), crate::ui::jobs::DEFAULT_SCHEDULE.to_string(), true, "New scrub job".to_string()),
+        };
+        Modal::Form {
+            title,
+            fields: vec![
+                FormField { label: "Host".to_string(), value: host },
+                FormField { label: "Pool".to_string(), value: pool.to_string() },
+                FormField { label: "Schedule".to_string(), value: schedule },
+                FormField { label: "Catch up (y/n)".to_string(), value: if catch_up { "y" } else { "n" }.to_string() },
+            ],
+            focused: 0,
+            description: None,
+            kind: FormKind::Job { hosts: self.hosts.clone(), existing: existing.map(Box::new), taken_ids: self.jobs_view.jobs.iter().map(|j| j.id.clone()).collect() },
+        }
+    }
+}
+
+/// Turns a submitted job form into a save, or says what is wrong with it. An edit keeps the
+/// job's id, creation time and enabled flag; a new job gets a fresh id and `created_at = now`.
+fn build_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
+    let FormKind::Job { hosts, existing, taken_ids } = kind else { return Err("not a job form".to_string()) };
+    let host = field_value(fields, 0).trim();
+    if !hosts.iter().any(|h| h == host) {
+        return Err(format!("Unknown host '{host}'. Configured hosts: {}", hosts.join(", ")));
+    }
+    let pool = field_value(fields, 1).trim();
+    if pool.is_empty() {
+        return Err("Pool name is required".to_string());
+    }
+    let schedule = field_value(fields, 2).trim();
+    crate::daemon::cron::Schedule::parse(schedule).map_err(|e| format!("Schedule: {e}"))?;
+    let catch_up = match field_value(fields, 3).trim().to_lowercase().as_str() {
+        "" | "y" | "yes" => true,
+        "n" | "no" => false,
+        other => return Err(format!("Catch up must be y or n, not '{other}'")),
+    };
+    let job = match existing {
+        Some(old) => crate::jobs::Job { host: host.to_string(), pool: Some(pool.to_string()), schedule: schedule.to_string(), catch_up, ..(**old).clone() },
+        None => crate::jobs::Job {
+            id: crate::ui::jobs::new_job_id(host, pool, taken_ids),
+            host: host.to_string(),
+            kind: crate::jobs::KIND_SCRUB.to_string(),
+            pool: Some(pool.to_string()),
+            schedule: schedule.to_string(),
+            catch_up,
+            enabled: true,
+            created_at: Some(now),
+        },
+    };
+    Ok(PendingAction { host: 0, section: Section::Jobs, call: MutationCall::SaveJob { state_dir: state_dir.to_path_buf(), job: Box::new(job) } })
+}
+
 fn share_edit_modal(share: &crate::zfs::share::Share, host: usize) -> Modal {
     match share {
         crate::zfs::share::Share::Nfs { dataset, raw, networks, hosts, read_only, root } => {
@@ -2332,7 +2416,7 @@ fn build_pending_action(kind: &FormKind, fields: &[FormField]) -> Option<Pending
         //
         // `handle_modal_key` routes `ShareSmb` through `build_smb_action`
         // instead: it needs the current share list for the duplicate-name check.
-        FormKind::AddHost | FormKind::ShareSmb { .. } => None,
+        FormKind::AddHost | FormKind::ShareSmb { .. } | FormKind::Job { .. } => None,
     }
 }
 
@@ -2622,6 +2706,7 @@ Pools
   Right / Left     focus/unfocus the properties pane
   n / d            create / destroy a pool
   s / R            scrub / resilver (y/n confirm)
+  J                schedule a recurring scrub of the selected pool (opens the job form)
 
 Vdevs (inside a drilled-into pool)
   o / O            offline / online the selected disk
PATCH
````

- [ ] **Step 4: Run the whole suite**

Run: `cargo test 2>&1 | grep -E "^error|^test .* FAILED|^test result" -A6`
Expected: `test result: ok. 859 passed; 0 failed`.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" -A4 | grep -E "^warning|-->" | paste - - | grep -E "src/(ui|jobs)"`
Expected: no output for these files (a `never used`/`never constructed` warning for code a later task wires up is fine).

```bash
git add -A
git commit -m "test: the job form; feat: the job form: open, validate, save, pickers and a live schedule preview

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 5: Enable, disable and delete jobs, with hints and help

**Files (created or modified):**
- `src/ui/hints.rs`
- `src/ui/mod.rs`
- `src/ui/state.rs`

**Interfaces:**
- Consumes: `SetJobEnabled`, `DeleteJob` (Task 2).
- Produces: `App::pending_mutations: Vec<PendingAction>` (drained by the event loop, for actions that need no confirmation); `Space` and `d` in Jobs; the Jobs hint-bar entries; the Jobs block in the Help text; the hint-drift scenarios and fingerprint now include the Jobs section.

- [ ] **Step 1: Apply the failing tests**

````bash
git apply <<'PATCH'
diff --git a/src/ui/hints.rs b/src/ui/hints.rs
index 8a4ddb1..ec286b3 100644
--- a/src/ui/hints.rs
+++ b/src/ui/hints.rs
@@ -477,6 +477,30 @@ mod tests {
         a
     }
 
+    fn jobs(with_selection: bool) -> App {
+        let mut a = base();
+        go(&mut a, Section::Jobs);
+        let job = |id: &str, enabled: bool| crate::jobs::Job { id: id.to_string(), host: "local".to_string(), kind: crate::jobs::KIND_SCRUB.to_string(), pool: Some("tank".to_string()), schedule: "0 3 * * 0".to_string(), catch_up: true, enabled, created_at: None };
+        a.jobs_view.jobs = vec![job("a", true), job("b", false)];
+        a.jobs_table.select(if with_selection { Some(0) } else { None });
+        a
+    }
+
+    #[test]
+    fn the_jobs_section_advertises_edit_enable_or_disable_delete_and_new_for_the_selected_job() {
+        let selected = jobs(true);
+        let labels: Vec<(&str, &str)> = hints(&selected).iter().map(|h| (h.key, h.label)).collect();
+        for want in [("Enter", "Edit"), ("Space", "Disable"), ("d", "Delete"), ("n", "New job")] {
+            assert!(labels.contains(&want), "{labels:?}");
+        }
+        let mut second = jobs(true);
+        second.jobs_table.select(Some(1));
+        assert!(hints(&second).iter().any(|h| (h.key, h.label) == ("Space", "Enable")), "a disabled job offers Enable");
+        let none = jobs(false);
+        let keys: Vec<&str> = hints(&none).iter().map(|h| h.key).collect();
+        assert!(keys.contains(&"n") && !keys.contains(&"Space") && !keys.contains(&"d") && !keys.contains(&"Enter"), "{keys:?}");
+    }
+
     fn hosts() -> App {
         let mut a = base();
         go(&mut a, Section::Hosts);
@@ -503,6 +527,8 @@ mod tests {
             ("disks, none selected", disks(false)),
             ("disks, one selected", disks(true)),
             ("shares", shares()),
+            ("jobs, one selected", jobs(true)),
+            ("jobs, none selected", jobs(false)),
         ]
     }
 
@@ -549,6 +575,7 @@ mod tests {
                 a.snapshots_table.selected(),
                 a.disks_table.selected(),
                 a.shares_table.selected(),
+                a.jobs_table.selected(),
                 a.vdevs_table.selected(),
                 a.pool_properties_table.selected(),
                 a.dataset_properties_table.selected(),
@@ -916,6 +943,7 @@ mod tests {
         a.snapshots.clear();
         a.disks.clear();
         a.shares.clear();
+        a.jobs_view.jobs.clear();
     }
 
     #[test]
@@ -970,6 +998,10 @@ mod tests {
             DiskInfo { name: "da1".to_string(), size: 1, membership: DiskMembership::Available, ..Default::default() },
         ];
         a.shares = vec![smb("tank/b"), smb("tank/c")];
+        a.jobs_view.jobs = vec![
+            crate::jobs::Job { id: "a".to_string(), host: "local".to_string(), kind: crate::jobs::KIND_SCRUB.to_string(), pool: Some("tank".to_string()), schedule: "0 3 * * 0".to_string(), catch_up: true, enabled: true, created_at: None },
+            crate::jobs::Job { id: "b".to_string(), host: "local".to_string(), kind: crate::jobs::KIND_SCRUB.to_string(), pool: Some("tank".to_string()), schedule: "0 3 * * 0".to_string(), catch_up: true, enabled: false, created_at: None },
+        ];
         a.vdevs = vec![
             VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Online },
             VdevLine { depth: 1, name: "da0".to_string(), health: PoolHealth::Online },
@@ -991,6 +1023,7 @@ mod tests {
             &mut a.snapshots_table,
             &mut a.disks_table,
             &mut a.shares_table,
+            &mut a.jobs_table,
             &mut a.vdevs_table,
             &mut a.pool_properties_table,
             &mut a.dataset_properties_table,
diff --git a/src/ui/state.rs b/src/ui/state.rs
index ce3cb68..c003254 100644
--- a/src/ui/state.rs
+++ b/src/ui/state.rs
@@ -7513,6 +7513,69 @@ mod tests {
         assert!(app.picker.is_none());
     }
 
+    fn selected_jobs_app(jobs_list: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
+        let (mut app, dir) = app_with_jobs(jobs_list);
+        app.refresh_jobs();
+        app.handle_key(KeyCode::Down);
+        (app, dir)
+    }
+
+    #[test]
+    fn space_on_a_job_queues_a_disable_then_an_enable_and_says_what_it_is_doing() {
+        let (mut app, _dir) = selected_jobs_app(&[scrub_job("a")]);
+        app.handle_key(KeyCode::Char(' '));
+        let [action] = app.pending_mutations.as_slice() else { panic!("{:?}", app.pending_mutations) };
+        assert_eq!(action.section, Section::Jobs);
+        assert!(matches!(&action.call, MutationCall::SetJobEnabled { id, enabled: false, .. } if id == "a"), "{:?}", action.call);
+        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Disabling job 'a'")), "{:?}", app.status_message);
+        app.pending_mutations.clear();
+        app.jobs_view.jobs[0].enabled = false;
+        app.handle_key(KeyCode::Char(' '));
+        assert!(matches!(&app.pending_mutations[0].call, MutationCall::SetJobEnabled { enabled: true, .. }));
+        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Enabling job 'a'")), "{:?}", app.status_message);
+    }
+
+    #[test]
+    fn space_with_no_job_selected_does_nothing() {
+        let (mut app, _dir) = app_with_jobs(&[scrub_job("a")]);
+        app.refresh_jobs();
+        app.handle_key(KeyCode::Char(' '));
+        assert!(app.pending_mutations.is_empty());
+    }
+
+    #[test]
+    fn d_on_a_job_asks_for_a_warning_confirmation_and_deletes_only_that_job() {
+        let (mut app, _dir) = selected_jobs_app(&[scrub_job("a"), scrub_job("b")]);
+        app.handle_key(KeyCode::Char('d'));
+        let Some(Modal::Confirm { prompt, action }) = &app.modal else { panic!("{:?}", app.modal) };
+        assert!(prompt.contains("Delete job 'a'") && prompt.contains("history"), "{prompt}");
+        assert_eq!(action.call.tone(), crate::ui::modal::Tone::Warn);
+        let action = app.handle_modal_key(KeyCode::Char('y')).expect("confirmed");
+        assert_eq!(action.section, Section::Jobs);
+        assert!(matches!(&action.call, MutationCall::DeleteJob { id, .. } if id == "a"), "{:?}", action.call);
+    }
+
+    #[test]
+    fn d_with_no_job_selected_opens_nothing_and_n_cancels_a_delete() {
+        let (mut app, _dir) = app_with_jobs(&[scrub_job("a")]);
+        app.refresh_jobs();
+        app.handle_key(KeyCode::Char('d'));
+        assert!(app.modal.is_none());
+        app.handle_key(KeyCode::Down);
+        app.handle_key(KeyCode::Char('d'));
+        assert!(app.handle_modal_key(KeyCode::Char('n')).is_none());
+        assert!(app.modal.is_none());
+    }
+
+    #[test]
+    fn help_text_documents_the_jobs_keys() {
+        let text = help_text();
+        let jobs = &text[text.find("\nJobs\n").expect("a Jobs block")..];
+        for want in ["Enter", "Space", "  d ", "  n ", "history"] {
+            assert!(jobs.contains(want), "missing {want:?} in the Jobs block:\n{jobs}");
+        }
+    }
+
     #[test]
     fn help_text_documents_the_global_keys() {
         let text = help_text();
PATCH
````

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test 2>&1 | grep -E "^error" -A3 | head -12`
Expected: the build FAILS because the code the tests use does not exist yet: `no field pending_mutations on type App`.

- [ ] **Step 3: Apply the implementation**

````bash
git apply <<'PATCH'
diff --git a/src/ui/hints.rs b/src/ui/hints.rs
index ec286b3..79cf10a 100644
--- a/src/ui/hints.rs
+++ b/src/ui/hints.rs
@@ -252,12 +252,12 @@ fn context_hints(app: &App) -> Vec<Hint> {
             if !app.jobs_view.jobs.is_empty() {
                 v.push(h("↑↓", "Move", 1));
             }
-            if app.jobs_view.jobs.is_empty() || app.jobs_table.selected().is_none() {
-                v.push(h("n", "New job", 2));
-            } else {
+            if let Some(job) = app.jobs_table.selected().and_then(|i| app.jobs_view.jobs.get(i)) {
                 v.push(h("Enter", "Edit", 1));
-                v.push(h("n", "New job", 2));
+                v.push(h("Space", if job.enabled { "Disable" } else { "Enable" }, 2));
+                v.push(h("d", "Delete", 2).tone(Tone::Warn));
             }
+            v.push(h("n", "New job", 2));
             v.push(h("r", "Refresh", 4));
         }
         // Never in `App::sections`; listed so a future section forces a decision here.
diff --git a/src/ui/mod.rs b/src/ui/mod.rs
index 60ff5c8..43590f9 100644
--- a/src/ui/mod.rs
+++ b/src/ui/mod.rs
@@ -320,6 +320,11 @@ fn event_loop(
                 }
             }
         }
+        for action in std::mem::take(&mut app.pending_mutations) {
+            if let Some(host) = inventory.hosts.get(action.host) {
+                app.dispatch_mutation(action, host.runner.clone());
+            }
+        }
         if app.pending_shell {
             app.pending_shell = false;
             if let Some(host) = inventory.hosts.get(app.selected_host) {
diff --git a/src/ui/state.rs b/src/ui/state.rs
index c003254..acf0b38 100644
--- a/src/ui/state.rs
+++ b/src/ui/state.rs
@@ -245,6 +245,9 @@ pub struct App {
     /// Set by `!`; the event loop takes it, because only it owns the terminal
     /// and can suspend the TUI to run a shell.
     pub pending_shell: bool,
+    /// Mutations to run on the next tick without a confirmation step (for example enabling or
+    /// disabling a job). The event loop drains this; `handle_key` cannot return an action itself.
+    pub pending_mutations: Vec<PendingAction>,
     pub account_cache: HashMap<usize, (Instant, crate::zfs::accounts::HostAccounts)>,
     pub account_fetch_in_flight: HashMap<usize, Instant>,
     pub pending_account_fetch: Vec<usize>,
@@ -326,6 +329,7 @@ impl App {
             should_quit: false,
             pending_new_hosts: Vec::new(),
             pending_shell: false,
+            pending_mutations: Vec::new(),
             account_cache: HashMap::new(),
             account_fetch_in_flight: HashMap::new(),
             pending_account_fetch: Vec::new(),
@@ -1174,6 +1178,24 @@ impl App {
                     self.modal = Some(self.job_form(Some(job.clone()), job.pool.as_deref().unwrap_or("")));
                 }
             }
+            KeyCode::Char(' ') if self.current_section() == Section::Jobs => {
+                if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)) {
+                    let enable = !job.enabled;
+                    self.status_message = Some(format!("{} job '{}'…", if enable { "Enabling" } else { "Disabling" }, job.id));
+                    self.pending_mutations.push(PendingAction {
+                        host: self.selected_host,
+                        section: Section::Jobs,
+                        call: MutationCall::SetJobEnabled { state_dir: self.state_dir.clone(), id: job.id.clone(), enabled: enable },
+                    });
+                }
+            }
+            KeyCode::Char('d') if self.current_section() == Section::Jobs => {
+                if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)) {
+                    let prompt = format!("Delete job '{}'? It stops running; its past runs stay in the history. (y/n)", job.id);
+                    let call = MutationCall::DeleteJob { state_dir: self.state_dir.clone(), id: job.id.clone() };
+                    self.modal = Some(Modal::Confirm { prompt, action: PendingAction { host: self.selected_host, section: Section::Jobs, call } });
+                }
+            }
             KeyCode::Down if self.current_section() == Section::Jobs => {
                 move_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len(), true);
             }
@@ -2746,6 +2768,13 @@ Shares
   Enter            edit the selected share
   d                remove the selected share
 
+Jobs
+  Up / Down        select a scheduled job
+  Enter            edit the job (host, pool, schedule, catch up)
+  n                new scrub job (or press J on a pool in Pools)
+  Space            enable / disable the job
+  d                delete the job (y/n; its past runs stay in the history)
+
 Press any key to close."
         .to_string()
 }
PATCH
````

- [ ] **Step 4: Run the whole suite**

Run: `cargo test 2>&1 | grep -E "^error|^test .* FAILED|^test result" -A6`
Expected: `test result: ok. 865 passed; 0 failed`.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" -A4 | grep -E "^warning|-->" | paste - - | grep -E "src/(ui|jobs)"`
Expected: no output for these files (a `never used`/`never constructed` warning for code a later task wires up is fine).

```bash
git add -A
git commit -m "test: job toggle, delete, hints and help; feat: enable, disable and delete jobs, with hints and help

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 6: The Jobs panel and the failed-jobs indicator

**Files (created or modified):**
- `src/ui/jobs.rs`
- `src/ui/layout.rs`

**Interfaces:**
- Consumes: `JobsView::{runs_of, failed_count, unsupported}`, `preview_lines`, `schedule_label` (Tasks 1 and 3).
- Produces: `ui::jobs::{duration_text, run_summary}`; `draw_jobs_panel` (the bottom half of the center column in the Jobs section: last 8 runs, schedule, next three times, flags); the `N jobs failed — see Jobs` line in the status bar from any section.

- [ ] **Step 1: Apply the failing tests**

````bash
git apply <<'PATCH'
diff --git a/src/ui/jobs.rs b/src/ui/jobs.rs
index 746e9b1..7d0e6e1 100644
--- a/src/ui/jobs.rs
+++ b/src/ui/jobs.rs
@@ -342,4 +342,31 @@ mod tests {
         assert_eq!(next_run_text("0 0 31 2 *", &now), "never");
         assert_eq!(next_run_text("bogus", &now), "never");
     }
+
+    #[test]
+    fn durations_read_in_the_two_largest_units() {
+        assert_eq!(duration_text(0), "0s");
+        assert_eq!(duration_text(45), "45s");
+        assert_eq!(duration_text(185), "3m 05s");
+        assert_eq!(duration_text(7_800), "2h 10m");
+        assert_eq!(duration_text(-5), "0s", "a clock that stepped back is not negative");
+    }
+
+    #[test]
+    fn a_run_summary_shows_when_how_it_ended_how_long_it_took_and_why() {
+        let est = FixedOffset::west_opt(5 * 3600).unwrap();
+        let dir = tempfile::tempdir().unwrap();
+        jobs::save(&dir.path().join("jobs.toml"), &[job("a")]).unwrap();
+        let h = History::new(dir.path().join("history.jsonl"));
+        let mut started = event("a", "r1", EventKind::Started, "2026-10-06T08:00:00Z", None);
+        started.message = String::new();
+        h.append(&started).unwrap();
+        let running = load(dir.path(), &hosts(), None);
+        assert_eq!(run_summary(running.last_run("a").unwrap(), &est), "10-06 03:00  running  -");
+        let mut done = event("a", "r1", EventKind::Finished, "2026-10-06T08:03:05Z", Some(RunResult::Failed));
+        done.message = "scrub repaired 12K with 3 errors\nsecond line".into();
+        h.append(&done).unwrap();
+        let finished = load(dir.path(), &hosts(), None);
+        assert_eq!(run_summary(finished.last_run("a").unwrap(), &est), "10-06 03:00  failed   3m 05s  scrub repaired 12K with 3 errors");
+    }
 }
diff --git a/src/ui/layout.rs b/src/ui/layout.rs
index b383728..1754900 100644
--- a/src/ui/layout.rs
+++ b/src/ui/layout.rs
@@ -1340,6 +1340,66 @@ mod tests {
         assert!(!text.contains("Next:"));
     }
 
+    fn history_event(job_id: &str, run_id: &str, kind: crate::daemon::history::EventKind, ago_mins: i64, result: Option<crate::daemon::history::RunResult>, message: &str) -> crate::daemon::history::Event {
+        let at = chrono::Utc::now() - chrono::Duration::minutes(ago_mins);
+        crate::daemon::history::Event { job_id: job_id.into(), run_id: run_id.into(), event: kind, at, scheduled_for: Some(at), result, message: message.into(), catch_up: false, summary: None }
+    }
+
+    #[test]
+    fn the_jobs_panel_shows_the_selected_jobs_runs_schedule_next_times_and_flags() {
+        use crate::daemon::history::{EventKind, History, RunResult};
+        let (mut app, dir) = jobs_app(&[a_job("scrub-local-tank", "0 3 * * 0")]);
+        let h = History::new(dir.path().join("history.jsonl"));
+        h.append(&history_event("scrub-local-tank", "r1", EventKind::Started, 300, None, "")).unwrap();
+        h.append(&history_event("scrub-local-tank", "r1", EventKind::Finished, 290, Some(RunResult::Failed), "scrub repaired 12K with 3 errors")).unwrap();
+        h.append(&history_event("scrub-local-tank", "r2", EventKind::Skipped, 10, Some(RunResult::Skipped), "skipped: already scanning")).unwrap();
+        app.refresh_jobs();
+        app.handle_key(crossterm::event::KeyCode::Down);
+        let text = render(&mut app, 130, 34).join("\n");
+        for want in ["Job — scrub-local-tank", "Last runs", "failed", "10m 00s", "scrub repaired 12K", "skipped", "skipped: already scanning", "Weekly, Sunday 03:00", "0 3 * * 0", "Next:", "enabled", "catch up"] {
+            assert!(text.contains(want), "missing {want:?} in\n{text}");
+        }
+    }
+
+    #[test]
+    fn the_jobs_panel_says_when_nothing_is_selected_and_shows_an_unsupported_reason_in_full() {
+        let mut bad = a_job("bad", "0 3 * * 0");
+        bad.host = "somewhere-else".into();
+        let (mut app, _dir) = jobs_app(&[bad]);
+        let text = render(&mut app, 120, 34).join("\n");
+        assert!(text.contains("nothing selected"), "{text}");
+        app.handle_key(crossterm::event::KeyCode::Down);
+        let text = render(&mut app, 150, 34).join("\n");
+        assert!(text.contains("host 'somewhere-else' is not in config.toml"), "{text}");
+        assert!(text.contains("No runs yet") || text.contains("never"), "{text}");
+    }
+
+    #[test]
+    fn the_jobs_panel_is_dropped_on_a_short_terminal_so_the_table_keeps_its_height() {
+        let (mut app, _dir) = jobs_app(&[a_job("a", "0 3 * * 0")]);
+        let text = render(&mut app, 120, 12).join("\n");
+        assert!(!text.contains("Last runs"), "{text}");
+    }
+
+    #[test]
+    fn failed_jobs_are_flagged_in_the_status_line_from_any_section() {
+        use crate::daemon::history::{EventKind, History, RunResult};
+        let (mut app, dir) = jobs_app(&[a_job("a", "0 3 * * 0"), a_job("b", "0 3 * * 0")]);
+        let h = History::new(dir.path().join("history.jsonl"));
+        h.append(&history_event("a", "r1", EventKind::Finished, 60, Some(RunResult::Failed), "boom")).unwrap();
+        app.refresh_jobs();
+        app.selected_section = 0;
+        let text = render(&mut app, 120, 30).join("\n");
+        assert!(text.contains("1 job failed"), "{text}");
+        h.append(&history_event("b", "r2", EventKind::Finished, 30, Some(RunResult::Failed), "boom")).unwrap();
+        app.refresh_jobs();
+        assert!(render(&mut app, 120, 30).join("\n").contains("2 jobs failed"));
+        h.append(&history_event("a", "r3", EventKind::Finished, 5, Some(RunResult::Ok), "")).unwrap();
+        h.append(&history_event("b", "r4", EventKind::Finished, 5, Some(RunResult::Ok), "")).unwrap();
+        app.refresh_jobs();
+        assert!(!render(&mut app, 120, 30).join("\n").contains("failed —"), "recovered jobs clear the indicator");
+    }
+
     #[test]
     fn tiny_terminals_have_no_metrics_panel_and_an_empty_list_says_nothing_is_selected() {
         let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
PATCH
````

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test 2>&1 | grep -E "^error" -A3 | head -12`
Expected: the build FAILS because the code the tests use does not exist yet: `cannot find function duration_text`.

- [ ] **Step 3: Apply the implementation**

````bash
git apply <<'PATCH'
diff --git a/src/ui/jobs.rs b/src/ui/jobs.rs
index 7d0e6e1..b30588a 100644
--- a/src/ui/jobs.rs
+++ b/src/ui/jobs.rs
@@ -36,6 +36,41 @@ pub fn describe(expr: &str) -> String {
     PRESETS.iter().find(|(cron, _)| *cron == expr).map_or_else(|| "custom schedule".to_string(), |(_, label)| (*label).to_string())
 }
 
+/// `45s`, `3m 05s`, `2h 10m`.
+pub fn duration_text(secs: i64) -> String {
+    let secs = secs.max(0);
+    if secs < 60 {
+        format!("{secs}s")
+    } else if secs < 3_600 {
+        format!("{}m {:02}s", secs / 60, secs % 60)
+    } else {
+        format!("{}h {:02}m", secs / 3_600, secs % 3_600 / 60)
+    }
+}
+
+/// One line of a job's run list: when it started (in `tz`), how it ended, how long it took, and
+/// the first line of its message.
+pub fn run_summary<Tz: TimeZone>(run: &Run, tz: &Tz) -> String
+where
+    Tz::Offset: std::fmt::Display,
+{
+    let when = run.started_at.with_timezone(tz).format("%m-%d %H:%M");
+    let word = match run.result {
+        None => "running",
+        Some(RunResult::Ok) => "ok",
+        Some(RunResult::Failed) => "failed",
+        Some(RunResult::Skipped) => "skipped",
+    };
+    let duration = run.finished_at.map_or_else(|| "-".to_string(), |end| duration_text((end - run.started_at).num_seconds()));
+    let mut line = format!("{when}  {word:<7}  {duration}");
+    let message = run.message.lines().next().unwrap_or("").trim();
+    if !message.is_empty() {
+        line.push_str("  ");
+        line.push_str(message);
+    }
+    line
+}
+
 /// The table's schedule column: a preset's name, or the cron text of a custom schedule.
 pub fn schedule_label(expr: &str) -> String {
     let described = describe(expr);
diff --git a/src/ui/layout.rs b/src/ui/layout.rs
index 1754900..eaf9dcb 100644
--- a/src/ui/layout.rs
+++ b/src/ui/layout.rs
@@ -39,7 +39,7 @@ pub fn draw(f: &mut Frame, app: &mut App) {
         .areas(body_area);
     // Every list section and a drilled pool give the bottom half of the center
     // column to a metrics panel; the sidebar keeps its full height.
-    let wants_metrics = app.drilled_pool.is_some() || matches!(app.current_section(), Section::Hosts | Section::Pools | Section::Datasets | Section::Snapshots | Section::Disks | Section::Shares);
+    let wants_metrics = app.drilled_pool.is_some() || matches!(app.current_section(), Section::Hosts | Section::Pools | Section::Datasets | Section::Snapshots | Section::Disks | Section::Shares | Section::Jobs);
     let (main_area, metrics_area) = if wants_metrics && body_area.height >= crate::ui::metrics::MIN_BODY_HEIGHT {
         let [top, bottom] = Layout::default()
             .direction(Direction::Vertical)
@@ -52,7 +52,9 @@ pub fn draw(f: &mut Frame, app: &mut App) {
 
     draw_sidebar(f, app, sidebar_area);
     draw_main(f, app, main_area);
-    if let Some(area) = metrics_area {
+    if let (Some(area), true) = (metrics_area, app.drilled_pool.is_none() && app.current_section() == Section::Jobs) {
+        draw_jobs_panel(f, app, area);
+    } else if let Some(area) = metrics_area {
         let health = app.metrics.target().and_then(|t| app.pools.iter().find(|p| Some(p.name.as_str()) == t.pool())).map(|p| p.health.clone());
         let disk = match app.metrics.target() {
             Some(MetricsTarget::Disk { name, .. }) => app.disks.iter().find(|d| &d.name == name),
@@ -98,6 +100,13 @@ fn status_bar_content(app: &App) -> (String, Style, Option<(f64, String)>) {
             style = app.theme.err;
         }
     }
+    let failed = app.jobs_view.failed_count();
+    if failed > 0 {
+        lines.push(format!("{failed} job{} failed — see Jobs", if failed == 1 { "" } else { "s" }));
+        if style == Style::default() {
+            style = app.theme.err;
+        }
+    }
     let mut gauge = None;
     if let Some(scan) = &app.pool_scan_status {
         let (summary, scan_style) = match scan {
@@ -581,6 +590,44 @@ pub(crate) fn smb_access_label(guest_ok: bool, valid_user: Option<&str>) -> Stri
     }
 }
 
+/// The bottom panel of the Jobs section: the selected job's recent runs on the left, its
+/// schedule, next run times and flags on the right. Drawn straight from the loaded data.
+fn draw_jobs_panel(f: &mut Frame, app: &App, area: Rect) {
+    use crate::ui::jobs::{preview_lines, run_summary, schedule_label};
+    let theme = &app.theme;
+    let selected = app.jobs_table.selected().and_then(|i| app.jobs_view.jobs.get(i));
+    let title = selected.map_or_else(|| "Job".to_string(), |j| format!("Job — {}", j.id));
+    let block = Block::default().borders(Borders::ALL).border_style(theme.pane_border(false)).title(title);
+    let inner = block.inner(area);
+    f.render_widget(block, area);
+    let Some(job) = selected else {
+        f.render_widget(Paragraph::new("nothing selected").style(theme.dim()), inner);
+        return;
+    };
+    let [left, right] = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(55), Constraint::Min(0)]).areas(inner);
+
+    let mut runs = vec![Line::styled("Last runs", theme.dim())];
+    let history = app.jobs_view.runs_of(&job.id, 8);
+    if history.is_empty() {
+        runs.push(Line::from("No runs yet"));
+    }
+    for run in history {
+        let style = if run.result == Some(crate::daemon::history::RunResult::Failed) { theme.err } else { Style::default() };
+        runs.push(Line::styled(run_summary(run, &chrono::Local), style));
+    }
+    f.render_widget(Paragraph::new(runs), left);
+
+    let label = schedule_label(&job.schedule);
+    let cron = job.schedule.trim();
+    let mut info = vec![Line::from(if label == cron { cron.to_string() } else { format!("{label}  ({cron})") })];
+    match app.jobs_view.unsupported.get(&job.id) {
+        Some(reason) => info.push(Line::styled(reason.clone(), theme.err)),
+        None => info.extend(preview_lines(&job.schedule, &chrono::Local::now()).into_iter().skip(1).map(Line::from)),
+    }
+    info.push(Line::from(format!("{}  catch up {}", if job.enabled { "enabled" } else { "disabled" }, if job.catch_up { "yes" } else { "no" })));
+    f.render_widget(Paragraph::new(info).wrap(Wrap { trim: false }), right);
+}
+
 fn draw_jobs(f: &mut Frame, app: &mut App, area: Rect) {
     use crate::ui::jobs::{last_result_text, next_run_text, schedule_label, DaemonState};
     let theme = app.theme.clone();
PATCH
````

- [ ] **Step 4: Run the whole suite**

Run: `cargo test 2>&1 | grep -E "^error|^test .* FAILED|^test result" -A6`
Expected: `test result: ok. 871 passed; 0 failed`.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" -A4 | grep -E "^warning|-->" | paste - - | grep -E "src/(ui|jobs)"`
Expected: no output for these files.

```bash
git add -A
git commit -m "test: the Jobs panel and the failed-jobs indicator; feat: the Jobs panel and the failed-jobs status indicator

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

### Task 7: Documentation, the spec alignment, and the live check

**Files:** `README.md`, `docs/superpowers/specs/2026-10-06-scheduler-daemon-and-scrubs-design.md`

- [ ] **Step 1: Apply the README patch**

````bash
git apply <<'PATCH'
diff --git a/README.md b/README.md
index 376398f..3f63ac1 100644
--- a/README.md
+++ b/README.md
@@ -118,6 +118,7 @@ On the four account fields (SMB valid user, NFS "Root user mapping" (users only)
 - `d`: destroy the selected pool (type the pool's name to confirm)
 - `s`: scrub the selected pool (y/n confirm)
 - `R`: resilver the selected pool (y/n confirm)
+- `J`: schedule a recurring scrub of the selected pool (opens the job form with host and pool filled in; see **Scheduled jobs** below)
 
 ### Pool/Dataset properties pane (right pane, `Right` to focus)
 
@@ -220,9 +221,19 @@ FreeBSD's `sharesmb` ZFS property doesn't work (`zfs set sharesmb=on` prints "No
 - **Removing a share** (`d` in Shares) deletes its stanza but leaves the user's Samba password in place, since the same user may also back a hand-written share.
 - SMB shares are only listed while `smbd` is running; a config or parse error shows in the Shares status line while NFS rows still render.
 
+### Jobs
+
+The Jobs section lists the scheduled jobs of every host (it is global, not per host). Its title says whether the daemon is running; the status line shows `N jobs failed` from any section while an enabled job's latest run failed.
+
+- `Up` / `Down`: select a job; the bottom panel shows its last 8 runs, its schedule and its next three run times
+- `n`: new scrub job (host, pool, schedule, catch up; `F2` lists the configured hosts and schedule presets, and the form previews the next three run times as you type)
+- `Enter`: edit the selected job (its id, creation time and enabled flag are kept)
+- `Space`: enable or disable the selected job
+- `d`: delete the selected job (y/n; its past runs stay in the history until pruned)
+
 ## Scheduled jobs (`zfsctl daemon`)
 
-`zfsctl daemon` runs scheduled jobs on the hub, unattended. It must run as root, like the TUI, and reaches other hosts over the same SSH + sudo path, so it needs no new sudoers entries. The only job kind so far is **scrub**: run `zpool scrub` on a pool of any configured host on a cron schedule, watch it to completion, and record the result. (A Jobs section in the TUI to manage these comes later; until then edit `jobs.toml` by hand.)
+`zfsctl daemon` runs scheduled jobs on the hub, unattended. It must run as root, like the TUI, and reaches other hosts over the same SSH + sudo path, so it needs no new sudoers entries. The only job kind so far is **scrub**: run `zpool scrub` on a pool of any configured host on a cron schedule, watch it to completion, and record the result. Manage jobs from the **Jobs** section of the TUI (see its keys above, or `J` on a pool in Pools), or edit `jobs.toml` by hand: the daemon notices either within a second. The TUI only reads the state directory, so it must run on the hub (as root) to see the daemon's jobs.
 
 **Install the service.** Copy the binary and the rc script, then enable it:
 
PATCH
````

- [ ] **Step 2: Apply the spec alignment**

````bash
git apply <<'PATCH'
diff --git a/docs/superpowers/specs/2026-10-06-scheduler-daemon-and-scrubs-design.md b/docs/superpowers/specs/2026-10-06-scheduler-daemon-and-scrubs-design.md
index cba2674..cb227d4 100644
--- a/docs/superpowers/specs/2026-10-06-scheduler-daemon-and-scrubs-design.md
+++ b/docs/superpowers/specs/2026-10-06-scheduler-daemon-and-scrubs-design.md
@@ -142,7 +142,7 @@ created_at = "2026-10-06T12:00:00Z"
 **Keys.**
 
 - In **Pools** (middle pane): `J` opens the job form for the selected pool, with host and pool filled in, schedule `0 3 * * 0`, catch up `y`.
-- In **Jobs**: `n` new (host defaults to the selected host), `e` or `Enter` edit, `d` delete (confirm), `Space` enable or disable.
+- In **Jobs**: `n` new (host defaults to the selected host), `Enter` edit, `d` delete (confirm), `Space` enable or disable. Edit has one key, not an `e` alias too: the hint-drift guard requires every working key to be advertised, and one advertised key per action.
 - The hint bar and Help text list these, and the existing hint-drift tests guard them.
 
 **Display.**
@@ -150,7 +150,7 @@ created_at = "2026-10-06T12:00:00Z"
 - Columns: job (`id`), host, pool, schedule (the preset's name or the cron text), next run, last result with its age (`ok 2h ago`, `failed 1d ago`, `skipped`, `running`, or `never`).
 - A disabled job is dimmed. A job that cannot run (unknown kind, host not in `config.toml`, bad schedule) is red with the reason in place of the next run.
 - The section title shows the daemon's state from the heartbeat: `daemon running (pid N)`, or, in the error style, `daemon NOT running: jobs will not fire`. Changes are saved either way.
-- The status line shows `N jobs failed` while the most recent run of any enabled job failed, from any section.
+- The status line shows `N jobs failed — see Jobs` (`1 job failed` for one) while the most recent run of any enabled job failed, from any section.
 - The bottom half of the center column holds a Jobs panel, drawn directly from the loaded data (it does not use the metrics panel's sampler): on the left the selected job's last 8 runs (time, result, duration, message); on the right its schedule in words and as cron, its next three run times, and its flags (`enabled`, `catch up`, and any reason it cannot run).
 - Times are the hub's local time. A schedule with no future time (`0 0 31 2 *`) shows `never`.
 
PATCH
````

- [ ] **Step 3: Run the whole suite and commit**

Run: `cargo test 2>&1 | grep -E "^error|^test result"`
Expected: `test result: ok. 871 passed`.

```bash
git add README.md docs/superpowers/specs/2026-10-06-scheduler-daemon-and-scrubs-design.md
git commit -m "docs: document the Jobs section and align spec section 8 with the shipped keys

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 4: Verify live on the hub**

Deploy the tree to `/home/fb/zfsctl` and build (`tar` the `src`, `contrib`, `Cargo.toml`, `Cargo.lock` over ssh, then `cargo build --release`). As root on the hub, with the daemon running on the default state directory (`./target/release/zfsctl daemon > /tmp/zfsctl-daemon.log 2>&1 &`), drive the TUI with the pexpect + pyte setup (see the project's `reference_driving_tui_with_pexpect_pyte` memory), using the disposable `scratch` pool:

1. `Tab` to Jobs: the list says `No scheduled jobs`, and the title says `daemon running (pid N)`.
2. `Tab` to Pools, `Down` to select `scratch`, press `J`: the form shows `Host: local`, `Pool: scratch`, `Schedule: 0 3 * * 0`, `Catch up: y`, and a preview with `Weekly, Sunday 03:00` and three `Next:` times. Focus the Schedule field, clear it and type `* * * * *`: the preview changes to `custom schedule` and lists three times a minute apart. Press `F2` there and confirm the preset list shows labels, and that choosing one puts only the cron text in the field. Type `* * * * *` again and press `Enter` through the last field to save.
3. `Tab` to Jobs: `scrub-local-scratch` is listed with `custom` schedule text, a next run within a minute, and `never` as the last result. The bottom panel names the job and shows `No runs yet`.
4. After the next minute boundary plus about 30 seconds the Last column reads `ok just now` and the panel lists the run with a duration. `cat /var/db/zfsctl/history.jsonl` agrees.
5. `Space`: the row dims and shows `disabled`; the hint changes to `Enable`. `Space` again re-enables it.
6. `Enter`, change the pool to `nosuchpool`, save: at the next run the Last column reads `failed`, the panel line is red with the `no such pool` message, and, from the Hosts section, the status line shows `1 job failed — see Jobs`.
7. Corrupt the file (`echo '[[[' > /var/db/zfsctl/jobs.toml`): the Jobs title adds `jobs.toml unreadable`, the list keeps its jobs, and pressing `Space` on a job reports an error and leaves the file exactly as you wrote it. Restore a valid file.
8. `d`, then `y`: the job is deleted and `jobs.toml` no longer lists it; its runs remain in `history.jsonl`.
9. Clean up: stop the daemon (`kill`), `rm -rf /var/db/zfsctl /tmp/zfsctl-daemon.log`, and confirm no `zfsctl` process remains and `zpool status scratch` shows no scrub in progress.
