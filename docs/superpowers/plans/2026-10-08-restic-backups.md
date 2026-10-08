# Restic Backups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `backup` job kind: on a schedule, clone the newest `<prefix>-<timestamp>` snapshot of one dataset to a fixed read-only mountpoint, `restic backup` it to a repository, destroy the clone, then optionally `restic forget --prune` per a retention policy; managed from the Jobs section with progress and cancel.

**Architecture:** Pure restic helpers (retention parser, argument builders, JSON-line parser) live in a new `src/restic.rs`. The run lives in a new `src/daemon/backup.rs` and reuses the daemon's `Env`, `finish`, locks, history and progress files; a new `transfer::run_lines` reads a child's stdout line by line with cancel. The daemon gets a shared `run_cancellable` helper (extracted from `replicate`). The TUI gets a `B` key and a backup form following the replication form. The snapshot job's prune learns to leave a snapshot that a clone pins.

**Tech Stack:** Rust, `serde_json`, `chrono`, restic 0.19.1 (verified locally; the FreeBSD package version is checked in the live task), `FakeRunner`-style scripted doubles.

**Spec:** `docs/superpowers/specs/2026-10-08-restic-backups-design.md`

## Global Constraints

- restic runs through the job's host runner (`runner.run` for queries, `runner.command` for the streaming backup); no `sh -c` anywhere. `local` needs no sudoers change; a remote host needs `/usr/local/bin/restic` in sudoers (README, Task 6).
- zfsctl stores and sends no secret: the job holds only `repository` (restic `-r`, no credentials inside) and `password_file` (a path on the host). The repository string never appears in a run message or history entry.
- Every restic option is passed in `--flag=value` form so a value that starts with `-` can never be read as an option.
- The run never creates, destroys or modifies a snapshot of the source. It creates exactly one temporary clone `<pool>/zit-restic-<job id>` mounted read-only at `/var/run/zfsctl/<job id>` and destroys it on every exit path.
- Tags: `zfsctl:<job id>` and `zfsctl-snapshot:<snapshot name>`; `restic snapshots --tag a,b` is AND (verified), `--tag a --tag b` is OR. Forget is always filtered by `--tag=zfsctl:<job id>` and `--host=<job host>`.
- Job ids used by a backup job must use only letters, digits, `_ - .` (they become a dataset name, a path and tags).
- `restic backup --json` exit code 3 means "snapshot saved but some files unreadable": reported as `Failed` with that explanation; forget never runs after any non-zero exit.
- Lints: `pedantic` plus `unwrap_used`/`indexing_slicing` warnings: no `unwrap()`/indexing in non-test code. Baseline: `cargo test` = 1045 passed; `cargo clippy` = exactly 1 pre-existing warning (`method get is never used`).
- Commit messages end with: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. Work on branch `restic-backups`; merge into `master` with `--no-ff` only after the user chooses to; no remote.

## Rulings made while planning

1. **Job field names:** the spec's `keep` collides with the snapshot job's numeric `keep`; the backup job's retention is the TOML field `retention` (form label "Keep"). The other two fields are `repository` and `password_file`. The spec's field table is updated in Task 6.
2. **`--time`** is produced on the job's host with `date -r <creation epoch> "+%Y-%m-%d %H:%M:%S"` (an unprivileged command already on the documented list) because restic parses `--time` in the host's local time zone. If either lookup fails, the backup runs without `--time` (retention then follows the run time).
3. **`restic --json` errors** arrive as a `{"message_type":"exit_error","message":…}` line on stdout; plain-text errors arrive on stderr. `restic::error_text` accepts both.

## Review Focus

Spec-implied inputs that a happy-path test would not cover; each has a test in the named task.

1. A clone left behind (crash, failed destroy) and the run after it; the clone is destroyed on success, failure, exit 3 and cancel — Task 2.
2. restic exiting non-zero after printing a summary, or killed by cancel: never `Ok`, never forgets — Task 2.
3. A repository, password file or retention text that looks like an option or contains odd characters; the repository never in a message — Tasks 1 and 2.
4. Retention text edge cases (`0d`, `7d 7d`, `7x`, `99999d`, empty) and forget touching only this job's tag and host — Tasks 1 and 2.
5. A snapshot job pruning the snapshot a clone pins (must not fail the run), and two backup jobs on one dataset with different repositories running at once (distinct clone names and mountpoints) — Tasks 2 and 3.

## File Structure

- Create `src/restic.rs` — retention, validation, argument builders, JSON-line parsing.
- Create `src/daemon/backup.rs` — `run_backup`.
- Modify `src/main.rs` — `mod restic;`.
- Modify `src/jobs.rs` — `KIND_BACKUP`, three fields, `check_backup_fields`, `resolve`, `is_cancellable`, test constructor, every `Job` literal in the crate.
- Modify `src/daemon/transfer.rs` — `Lines`, `run_lines`.
- Modify `src/daemon/progress.rs` — `ProgressWriter::set_total`.
- Modify `src/daemon/snapshot.rs` — prune treats a snapshot with dependent clones as in use.
- Modify `src/daemon/mod.rs` — `mod backup;`, `run_cancellable`, `backup()`, dispatch.
- Modify `src/ui/jobs.rs`, `src/ui/state.rs`, `src/ui/modal.rs`, `src/ui/picker.rs`, `src/ui/layout.rs`, `src/ui/hints.rs` — the `B` key, form, details, cancel.
- Modify `README.md`, the spec.

---

### Task 1: The job model and the pure restic helpers

**Files:**
- Create: `src/restic.rs`
- Modify: `src/main.rs` (add `mod restic;` next to the other `mod` lines)
- Modify: `src/jobs.rs` (constants ~line 17; `Job` struct ~line 50; new `check_backup_fields` next to `check_snapshot_fields` ~line 168; `resolve` ~line 183; `#[cfg(test)] impl Job` ~line 270; its tests)
- Modify: every `Job { … }` literal in the crate (see Step 4)

**Interfaces:**
- Produces:
  - `jobs::KIND_BACKUP: &str = "backup"`, `Job.repository/password_file/retention: Option<String>`, `Job::test_backup(id, dataset, prefix, repository, password_file) -> Job` (cfg(test)), `jobs::is_cancellable(&Job) -> bool` (replicate or backup).
  - `restic::Retention { last, daily, weekly, monthly, yearly: Option<u32> }` with `fn is_empty(&self) -> bool`, `fn args(&self) -> Vec<String>`; `restic::parse_retention(&str) -> Result<Retention, String>`; `valid_repository(&str) -> bool`; `valid_password_file(&str) -> bool`.
  - `restic::snapshots_args(repo, pf, job_id, snapshot) -> Vec<String>`; `backup_args(repo, pf, host, job_id, snapshot, time: Option<&str>, path) -> Vec<String>`; `forget_args(repo, pf, host, job_id, &Retention) -> Vec<String>`.
  - `restic::Summary { new, changed, unmodified, added: u64 }`; `restic::Line::{Status { bytes_done, total_bytes }, Summary(Summary), Error(String), Other}`; `parse_line(&str) -> Line`; `snapshot_count(&str) -> Result<usize, String>`; `error_text(stdout: &str, stderr: &str) -> String`.

- [ ] **Step 1: Create the branch and write the failing tests**

```bash
git switch -c restic-backups
```

Create `src/restic.rs` containing only this test module (plus `use super::*;` is satisfied by the items added in Step 3; until then the file fails to compile, which is the RED state):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_parses_each_unit_and_builds_the_keep_flags_in_a_fixed_order() {
        let r = parse_retention("7d 4w 12m 3y 5l").unwrap();
        assert_eq!(r, Retention { last: Some(5), daily: Some(7), weekly: Some(4), monthly: Some(12), yearly: Some(3) });
        assert_eq!(r.args(), vec!["--keep-last=5", "--keep-daily=7", "--keep-weekly=4", "--keep-monthly=12", "--keep-yearly=3"]);
        assert_eq!(parse_retention("  30d  ").unwrap().args(), vec!["--keep-daily=30"]);
    }

    #[test]
    fn blank_retention_is_empty_and_keeps_everything() {
        assert!(parse_retention("").unwrap().is_empty());
        assert!(parse_retention("   ").unwrap().is_empty());
        assert!(Retention::default().args().is_empty());
    }

    #[test]
    fn bad_retention_is_refused_with_the_token_named() {
        for bad in ["0d", "7d 7d", "7x", "d", "7", "-3d", "99999d", "1.5d", "7 d", "7dd", "all"] {
            let err = parse_retention(bad).unwrap_err();
            assert!(!err.is_empty(), "{bad}");
        }
        assert!(parse_retention("7x").unwrap_err().contains("7x"));
        assert!(parse_retention("7d 7d").unwrap_err().contains("twice"));
    }

    #[test]
    fn repository_and_password_file_validation() {
        for good in ["/mnt/usb/restic", "sftp:backup@host:/srv/restic", "rclone:b2:bucket/path", "/mnt/my disk/repo"] {
            assert!(valid_repository(good), "{good}");
        }
        for bad in ["", " ", "-r", "--help", "/a\nb", "/a\0b", " /lead", "trail ", "a\rb"] {
            assert!(!valid_repository(bad), "{bad:?}");
        }
        for good in ["/root/.restic-pw", "/etc/zfsctl/pw"] {
            assert!(valid_password_file(good), "{good}");
        }
        for bad in ["", "/", "rel/pw", "-x", "/a/../b", "/a//b", "/a/./b", "/a/", "/a\nb", "/a\0b"] {
            assert!(!valid_password_file(bad), "{bad:?}");
        }
    }

    #[test]
    fn snapshots_args_match_both_tags_with_one_comma_separated_flag() {
        let a = snapshots_args("/repo", "/pw", "backup-local-tank-data", "hourly-20261008T100000Z");
        assert_eq!(a, vec!["snapshots", "--repo=/repo", "--password-file=/pw", "--json", "--tag=zfsctl:backup-local-tank-data,zfsctl-snapshot:hourly-20261008T100000Z"]);
    }

    #[test]
    fn backup_args_tag_the_job_and_snapshot_and_put_the_path_after_a_double_dash() {
        let a = backup_args("/repo", "/pw", "local", "job1", "snapA", Some("2026-10-08 20:00:00"), "/var/run/zfsctl/job1");
        assert_eq!(a, vec!["backup", "--repo=/repo", "--password-file=/pw", "--host=local", "--tag=zfsctl:job1", "--tag=zfsctl-snapshot:snapA", "--time=2026-10-08 20:00:00", "--json", "--", "/var/run/zfsctl/job1"]);
        let no_time = backup_args("-weird", "/pw", "local", "job1", "snapA", None, "/m");
        assert!(!no_time.iter().any(|x| x.starts_with("--time")));
        assert_eq!(no_time.get(1).map(String::as_str), Some("--repo=-weird"), "a leading dash stays inside the flag value");
    }

    #[test]
    fn forget_args_are_limited_to_this_jobs_tag_and_host() {
        let r = parse_retention("7d 4w").unwrap();
        let a = forget_args("/repo", "/pw", "local", "job1", &r);
        assert_eq!(a, vec!["forget", "--repo=/repo", "--password-file=/pw", "--prune", "--tag=zfsctl:job1", "--host=local", "--keep-daily=7", "--keep-weekly=4"]);
    }

    #[test]
    fn json_lines_are_classified() {
        assert_eq!(parse_line(r#"{"message_type":"status","percent_done":0.5,"total_bytes":1000,"bytes_done":500}"#), Line::Status { bytes_done: 500, total_bytes: 1000 });
        assert_eq!(
            parse_line(r#"{"message_type":"summary","files_new":3,"files_changed":2,"files_unmodified":40,"data_added":4096,"snapshot_id":"abc"}"#),
            Line::Summary(Summary { new: 3, changed: 2, unmodified: 40, added: 4096 })
        );
        assert_eq!(parse_line(r#"{"message_type":"exit_error","code":10,"message":"Fatal: wrong password or no key found"}"#), Line::Error("Fatal: wrong password or no key found".to_string()));
        assert_eq!(parse_line(r#"{"message_type":"error","error":{"message":"x"}}"#), Line::Other);
        assert_eq!(parse_line("not json at all"), Line::Other);
        assert_eq!(parse_line(""), Line::Other);
    }

    #[test]
    fn snapshot_count_reads_the_json_array() {
        assert_eq!(snapshot_count("[]\n").unwrap(), 0);
        assert_eq!(snapshot_count("[{\"id\":\"a\"},{\"id\":\"b\"}]").unwrap(), 2);
        assert!(snapshot_count("Fatal: nope").is_err());
    }

    #[test]
    fn error_text_prefers_the_json_exit_error_then_stderr() {
        let out = "{\"message_type\":\"error\",\"error\":{}}\n{\"message_type\":\"exit_error\",\"code\":12,\"message\":\"Fatal: wrong password or no key found\"}\n";
        assert_eq!(error_text(out, "ignored"), "Fatal: wrong password or no key found");
        assert_eq!(error_text("", "  Fatal: unable to open repo\nIs there one?\n"), "Fatal: unable to open repo Is there one?");
        assert_eq!(error_text("", ""), "restic failed");
        let multi = "{\"message_type\":\"exit_error\",\"message\":\"Fatal: repository does not exist\\nIs there a repository?\"}";
        assert_eq!(error_text(multi, ""), "Fatal: repository does not exist Is there a repository?");
    }
}
```

In `src/jobs.rs` tests, add:

```rust
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
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test restic resolve_accepts_a_good_backup 2>&1 | grep -E "^error" | head -5` (run the two filters separately if cargo rejects two)
Expected: compile errors (`parse_retention`, `Job::test_backup`, `is_cancellable`, `repository` field not found).

- [ ] **Step 3: Implement `src/restic.rs`** — put this above the test module and add `mod restic;` to `src/main.rs`:

```rust
//! The pure parts of restic backups: the retention text, validation of what a job may hand to
//! restic, the argument lists, and reading restic's `--json` output.
//!
//! Every option is passed as `--flag=value`, so a value that starts with `-` can never be read as
//! an option. restic's tag filter is `--tag a,b` = both tags and `--tag a --tag b` = either.

use serde_json::Value;

/// How many backups of each kind to keep; every field `None` keeps all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Retention {
    pub last: Option<u32>,
    pub daily: Option<u32>,
    pub weekly: Option<u32>,
    pub monthly: Option<u32>,
    pub yearly: Option<u32>,
}

impl Retention {
    pub fn is_empty(&self) -> bool {
        *self == Retention::default()
    }

    /// The `--keep-*` flags, in a fixed order.
    pub fn args(&self) -> Vec<String> {
        [("last", self.last), ("daily", self.daily), ("weekly", self.weekly), ("monthly", self.monthly), ("yearly", self.yearly)]
            .into_iter()
            .filter_map(|(name, n)| n.map(|n| format!("--keep-{name}={n}")))
            .collect()
    }
}

/// Parses `7d 4w 12m 3y 5l` (daily, weekly, monthly, yearly, last). Blank means keep everything.
pub fn parse_retention(text: &str) -> Result<Retention, String> {
    let mut r = Retention::default();
    for token in text.split_whitespace() {
        let shape = || format!("'{token}' is not like 7d, 4w, 12m, 3y or 5l");
        let Some(unit) = token.chars().last() else { continue };
        let digits = token.strip_suffix(unit).unwrap_or("");
        let n: u32 = digits.parse().map_err(|_| shape())?;
        if !(1..=9999).contains(&n) {
            return Err(format!("'{token}': the count must be 1 to 9999"));
        }
        let slot = match unit {
            'l' => &mut r.last,
            'd' => &mut r.daily,
            'w' => &mut r.weekly,
            'm' => &mut r.monthly,
            'y' => &mut r.yearly,
            _ => return Err(shape()),
        };
        if slot.is_some() {
            return Err(format!("'{token}': that unit is given twice"));
        }
        *slot = Some(n);
    }
    Ok(r)
}

fn plain(s: &str) -> bool {
    !s.chars().any(|c| matches!(c, '\0' | '\n' | '\r'))
}

/// A restic repository string: non-empty, no surrounding whitespace, no control newlines, and not
/// starting with `-`. It may hold a host or path but must not hold credentials (documented).
pub fn valid_repository(s: &str) -> bool {
    !s.is_empty() && s.trim() == s && !s.starts_with('-') && plain(s)
}

/// An absolute path on the job's host with no empty, `.` or `..` component.
pub fn valid_password_file(s: &str) -> bool {
    s.len() > 1 && s.starts_with('/') && plain(s) && s.split('/').skip(1).all(|c| !c.is_empty() && c != "." && c != "..")
}

fn repo_flags(repo: &str, password_file: &str) -> [String; 2] {
    [format!("--repo={repo}"), format!("--password-file={password_file}")]
}

/// `restic snapshots` of the backups made from `snapshot` by this job (both tags must match).
pub fn snapshots_args(repo: &str, password_file: &str, job_id: &str, snapshot: &str) -> Vec<String> {
    let [r, p] = repo_flags(repo, password_file);
    vec!["snapshots".to_string(), r, p, "--json".to_string(), format!("--tag=zfsctl:{job_id},zfsctl-snapshot:{snapshot}")]
}

/// `restic backup` of `path`, tagged with the job and the ZFS snapshot it was made from.
pub fn backup_args(repo: &str, password_file: &str, host: &str, job_id: &str, snapshot: &str, time: Option<&str>, path: &str) -> Vec<String> {
    let [r, p] = repo_flags(repo, password_file);
    let mut args = vec!["backup".to_string(), r, p, format!("--host={host}"), format!("--tag=zfsctl:{job_id}"), format!("--tag=zfsctl-snapshot:{snapshot}")];
    if let Some(time) = time {
        args.push(format!("--time={time}"));
    }
    args.extend(["--json".to_string(), "--".to_string(), path.to_string()]);
    args
}

/// `restic forget --prune` limited to this job's backups from this host.
pub fn forget_args(repo: &str, password_file: &str, host: &str, job_id: &str, retention: &Retention) -> Vec<String> {
    let [r, p] = repo_flags(repo, password_file);
    let mut args = vec!["forget".to_string(), r, p, "--prune".to_string(), format!("--tag=zfsctl:{job_id}"), format!("--host={host}")];
    args.extend(retention.args());
    args
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub new: u64,
    pub changed: u64,
    pub unmodified: u64,
    pub added: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Status { bytes_done: u64, total_bytes: u64 },
    Summary(Summary),
    /// restic's fatal error (`exit_error`).
    Error(String),
    Other,
}

/// Classifies one line of `restic backup --json` output.
pub fn parse_line(line: &str) -> Line {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Line::Other };
    let num = |key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
    match v.get("message_type").and_then(Value::as_str) {
        Some("status") => Line::Status { bytes_done: num("bytes_done"), total_bytes: num("total_bytes") },
        Some("summary") => Line::Summary(Summary { new: num("files_new"), changed: num("files_changed"), unmodified: num("files_unmodified"), added: num("data_added") }),
        Some("exit_error") => Line::Error(v.get("message").and_then(Value::as_str).unwrap_or("restic failed").to_string()),
        _ => Line::Other,
    }
}

/// How many snapshots `restic snapshots --json` listed.
pub fn snapshot_count(json: &str) -> Result<usize, String> {
    serde_json::from_str::<Vec<Value>>(json.trim()).map(|v| v.len()).map_err(|e| format!("unexpected restic snapshots output ({e})"))
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A failed restic command's message: the `exit_error` of `--json` output if there is one, else
/// stderr, on one line.
pub fn error_text(stdout: &str, stderr: &str) -> String {
    let fatal = stdout.lines().find_map(|l| match parse_line(l) {
        Line::Error(m) => Some(m),
        _ => None,
    });
    let text = one_line(&fatal.unwrap_or_else(|| stderr.to_string()));
    if text.is_empty() {
        "restic failed".to_string()
    } else {
        text
    }
}
```

- [ ] **Step 4: Implement the job model**

In `src/jobs.rs`:
1. After `pub const KIND_REPLICATE …;` add `pub const KIND_BACKUP: &str = "backup";`.
2. In `struct Job`, after the `force` field add:

```rust
    /// A backup job's restic repository (`restic -r`). It must not contain credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// A backup job's restic password file: an absolute path on the job's host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_file: Option<String>,
    /// A backup job's retention such as `7d 4w 12m 3y`; absent or blank keeps every backup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<String>,
```

3. Next to `check_snapshot_fields` add:

```rust
fn check_backup_fields(job: &Job) -> std::result::Result<(), String> {
    if !job.dataset.as_deref().is_some_and(valid_dataset) {
        return Err("a backup job needs a dataset like tank/data".to_string());
    }
    if !valid_prefix(effective_prefix(job)) {
        return Err("the snapshot prefix may only use letters, digits, _ - and .".to_string());
    }
    // The id becomes a dataset name, a mountpoint and restic tags.
    if !valid_prefix(&job.id) {
        return Err("a backup job's id may only use letters, digits, _ - and .".to_string());
    }
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
    job.kind == KIND_REPLICATE || job.kind == KIND_BACKUP
}
```

4. In `resolve`, add `KIND_BACKUP => check_backup_fields(job)?,` after the `KIND_REPLICATE` arm.
5. In `#[cfg(test)] impl Job` add:

```rust
    pub fn test_backup(id: &str, dataset: &str, prefix: &str, repository: &str, password_file: &str) -> Job {
        Job { id: id.into(), host: "local".into(), kind: KIND_BACKUP.into(), pool: None, dataset: Some(dataset.into()), recursive: false, prefix: Some(prefix.into()), keep: None, dest_host: None, dest_dataset: None, dest_dir: None, force: false, repository: Some(repository.into()), password_file: Some(password_file.into()), retention: None, schedule: "0 3 * * *".into(), catch_up: true, enabled: true, created_at: None }
    }
```

6. Update every other `Job { … }` literal in the crate. Run, from the repo root:

```bash
python3 - <<'EOF'
import re,glob
for p in glob.glob('src/**/*.rs', recursive=True):
    s=open(p).read()
    if p=='src/jobs.rs':
        # the test_backup constructor above already carries the fields; patch the rest
        parts=s.split('pub fn test_backup')
        head=re.sub(r'\bforce: false\b(?!, repository)', 'force: false, repository: None, password_file: None, retention: None', parts[0])
        tail=parts[1] if len(parts)>1 else ''
        tail_head,_,tail_rest=tail.partition('\n    }\n')
        tail_rest=re.sub(r'\bforce: false\b(?!, repository)', 'force: false, repository: None, password_file: None, retention: None', tail_rest)
        s=head+'pub fn test_backup'+tail_head+'\n    }\n'+tail_rest
    else:
        s=re.sub(r'\bforce: false\b', 'force: false, repository: None, password_file: None, retention: None', s)
    open(p,'w').write(s)
EOF
cargo build 2>&1 | grep -E "^error" -A6 | head -30
```

The struct literal `Job { …, force, … }` built in `build_replicate_job_action` (`src/ui/state.rs`, the line `force,` inside `let base = crate::jobs::Job {`) uses shorthand: add `repository: None, password_file: None, retention: None,` after it by hand. Fix any other compile error the same way (a missing-field error names the literal).

- [ ] **Step 5: Run the tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"`
Expected: all pass (1045 + 14 new). `cargo clippy 2>&1 | tail -2`: only dead-code warnings for the not-yet-used helpers; no new category.

- [ ] **Step 6: Commit**

```bash
git add -A src
git commit -m "feat: backup job model and the pure restic helpers

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: The backup run

**Files:**
- Modify: `src/daemon/transfer.rs` (add `Lines` and `run_lines` before `#[cfg(test)]`; tests in its `mod tests`, which has the `sh(script)` helper)
- Modify: `src/daemon/progress.rs` (`ProgressWriter::set_total`)
- Create: `src/daemon/backup.rs`
- Modify: `src/daemon/mod.rs` (add `pub mod backup;` after `pub mod replicate_file;`)

**Interfaces:**
- Consumes: Task 1's `restic::*`, `Job` fields; existing `Env { ctx, progress_dir, cancel, replication_state }` (`daemon/replicate.rs`), `scrub::{event, finish, Outcome, Slot}`, `snapshot::is_own`, `zfs::replication::list_snapshot_infos`.
- Produces: `transfer::Lines { cancelled: bool, code: Option<i32>, stderr: String }`, `transfer::run_lines(cmd: Command, cancel: &AtomicBool, on_line: impl FnMut(&str)) -> std::io::Result<Lines>`; `ProgressWriter::set_total(&mut self, total: u64)`; `backup::clone_name(dataset, job_id) -> String`, `backup::mountpoint(job_id) -> String`, `backup::run_backup(env: &Env, runner: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome`.

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` of `src/daemon/transfer.rs`:

```rust
    #[test]
    fn run_lines_hands_over_each_stdout_line_and_the_exit_code() {
        let mut seen = Vec::new();
        let r = run_lines(sh("echo one; echo two; echo oops >&2; exit 2"), &AtomicBool::new(false), |l| seen.push(l.to_string())).unwrap();
        assert_eq!(seen, vec!["one", "two"]);
        assert_eq!((r.cancelled, r.code), (false, Some(2)));
        assert!(r.stderr.contains("oops"), "{}", r.stderr);
    }

    #[test]
    fn run_lines_kills_the_child_when_cancelled_and_still_returns() {
        let started = Instant::now();
        let r = run_lines(sh("echo started; sleep 60"), &AtomicBool::new(true), |_| {}).unwrap();
        assert!(r.cancelled);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_ne!(r.code, Some(0));
    }

    #[test]
    fn run_lines_reports_a_command_that_cannot_start() {
        let missing = Command::new("/nonexistent/zfsctl-test-binary");
        assert!(run_lines(missing, &AtomicBool::new(false), |_| {}).is_err());
    }
```

Append inside `mod tests` of `src/daemon/progress.rs` (it already imports what the other writer tests use; if `ProgressWriter` tests build a writer with `ProgressWriter::new(dir, "r".into(), "j".into(), None, t0)`, mirror that):

```rust
    #[test]
    fn a_total_learned_after_the_start_shows_up_in_later_progress_files() {
        let dir = tempfile::tempdir().unwrap();
        let t0 = chrono::DateTime::parse_from_rfc3339("2026-10-08T10:00:00Z").unwrap().with_timezone(&Utc);
        let mut w = ProgressWriter::new(dir.path().to_path_buf(), "run".into(), "job".into(), None, t0);
        w.update(10, t0);
        assert_eq!(read_all(dir.path())[0].total, None);
        w.set_total(1000);
        w.update(500, t0 + chrono::Duration::seconds(2));
        let p = &read_all(dir.path())[0];
        assert_eq!((p.bytes, p.total), (500, Some(1000)));
    }
```

Create `src/daemon/backup.rs` with the imports from Step 3 and this test module (RED = compile errors until the items exist):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::history::{runs, History};
    use crate::daemon::scrub::{Clock, Context, Locks};
    use crate::error::Result;
    use crate::exec::CommandOutput;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    struct FixedClock(DateTime<Utc>);
    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
        fn sleep(&self, _: Duration) {}
    }

    const A: &str = "hourly-20261008T100000Z";
    const B: &str = "hourly-20261008T110000Z";
    const OK_SCRIPT: &str = r#"echo '{"message_type":"status","percent_done":0.5,"total_bytes":1000,"bytes_done":500}'; echo '{"message_type":"summary","files_new":3,"files_changed":2,"files_unmodified":40,"data_added":2048}'"#;

    /// One host: answers zfs and restic queries from fixed strings, remembers whether the clone
    /// exists, hands out a scripted shell for `restic backup`, and records every call.
    struct Host {
        listing: String,
        restic_snapshots: Result<String, String>,
        backup_script: String,
        date_works: bool,
        clone_fails: bool,
        destroy_fails: bool,
        forget_fails: bool,
        clone_exists: Mutex<bool>,
        calls: Mutex<Vec<String>>,
        backup_args: Mutex<Vec<Vec<String>>>,
    }

    impl Host {
        fn new() -> Self {
            Host {
                listing: format!("tank/data@{A}\t1\t10\ntank/data@{B}\t2\t20\n"),
                restic_snapshots: Ok("[]\n".to_string()),
                backup_script: OK_SCRIPT.to_string(),
                date_works: true,
                clone_fails: false,
                destroy_fails: false,
                forget_fails: false,
                clone_exists: Mutex::new(false),
                calls: Mutex::new(vec![]),
                backup_args: Mutex::new(vec![]),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn called(&self, prefix: &str) -> bool {
            self.calls().iter().any(|c| c.starts_with(prefix))
        }
        fn clone_exists(&self) -> bool {
            *self.clone_exists.lock().unwrap()
        }
    }

    impl CommandRunner for Host {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(format!("{cmd} {}", args.join(" ")));
            let ok = |s: &str| Ok(CommandOutput { stdout: s.to_string(), stderr: String::new(), status: 0 });
            let fail = |s: &str| Ok(CommandOutput { stdout: String::new(), stderr: s.to_string(), status: 1 });
            match (cmd, args.first().copied()) {
                ("zfs", Some("list")) if args.contains(&"snapshot") => ok(&self.listing),
                ("zfs", Some("list")) => {
                    if self.clone_exists() { ok("tank/zit-restic-job\n") } else { fail("dataset does not exist") }
                }
                ("zfs", Some("get")) => ok("1791500000\n"),
                ("zfs", Some("clone")) => {
                    if self.clone_fails {
                        return fail("cannot mount '/var/run/zfsctl/job': failed to create mountpoint");
                    }
                    *self.clone_exists.lock().unwrap() = true;
                    ok("")
                }
                ("zfs", Some("destroy")) => {
                    if self.destroy_fails {
                        return fail("dataset is busy");
                    }
                    *self.clone_exists.lock().unwrap() = false;
                    ok("")
                }
                ("restic", Some("snapshots")) => match &self.restic_snapshots {
                    Ok(json) => ok(json),
                    Err(e) => Ok(CommandOutput { stdout: String::new(), stderr: e.clone(), status: 12 }),
                },
                ("restic", Some("forget")) => {
                    if self.forget_fails { fail("Fatal: unable to lock repository") } else { ok("") }
                }
                other => panic!("unexpected call {other:?}"),
            }
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _: &str) -> Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push(format!("{cmd} {}", args.join(" ")));
            assert_eq!(cmd, "date");
            let status = i32::from(!self.date_works);
            Ok(CommandOutput { stdout: if self.date_works { "2026-10-08 19:33:20\n".to_string() } else { String::new() }, stderr: String::new(), status })
        }
        fn command(&self, cmd: &str, args: &[&str]) -> Result<Command> {
            assert_eq!(cmd, "restic");
            self.calls.lock().unwrap().push(format!("restic {}", args.join(" ")));
            self.backup_args.lock().unwrap().push(args.iter().map(|a| (*a).to_string()).collect());
            let mut c = Command::new("sh");
            c.args(["-c", &self.backup_script]);
            Ok(c)
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-08T23:30:00Z").unwrap().with_timezone(&Utc)
    }

    struct Rig {
        state: tempfile::TempDir,
        locks: Locks,
        cancel: AtomicBool,
    }

    impl Rig {
        fn new() -> Self {
            Rig { state: tempfile::tempdir().unwrap(), locks: Locks::default(), cancel: AtomicBool::new(false) }
        }
        fn run(&self, host: &Host, job: &Job) -> (Outcome, Vec<crate::daemon::history::Run>) {
            let history = History::new(self.state.path().join("history.jsonl"));
            let (clock, stop) = (FixedClock(now()), AtomicBool::new(false));
            let ctx = Context { history: &history, clock: &clock, locks: &self.locks, poll: Duration::from_secs(1), stop: &stop };
            let progress = self.state.path().join("progress");
            let state = self.state.path().join("replication.json");
            let env = Env { ctx: &ctx, progress_dir: &progress, cancel: &self.cancel, replication_state: &state };
            let outcome = run_backup(&env, host, job, now(), false, "run-1");
            (outcome, runs(&history.read()))
        }
    }

    fn job() -> Job {
        Job::test_backup("job", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw")
    }

    #[test]
    fn names_are_derived_from_the_pool_and_the_job_id() {
        assert_eq!(clone_name("tank/data/sub", "backup-1"), "tank/zit-restic-backup-1");
        assert_eq!(clone_name("tank", "b"), "tank/zit-restic-b");
        assert_eq!(mountpoint("backup-1"), "/var/run/zfsctl/backup-1");
    }

    #[test]
    fn a_run_clones_the_newest_snapshot_backs_up_the_clone_destroys_it_and_reports_the_counts() {
        let rig = Rig::new();
        let host = Host::new();
        let (outcome, history) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert_eq!(outcome.message, format!("backed up {B} (3 new, 2 changed, 40 unmodified), 2.0 KiB added"));
        let calls = host.calls();
        let at = |prefix: &str| calls.iter().position(|c| c.starts_with(prefix)).unwrap_or_else(|| panic!("no call starting {prefix:?} in {calls:?}"));
        assert!(at("restic snapshots") < at("zfs clone") && at("zfs clone") < at("restic backup") && at("restic backup") < at("zfs destroy"));
        assert!(calls.contains(&format!("zfs clone -o mountpoint=/var/run/zfsctl/job -o readonly=on -- tank/data@{B} tank/zit-restic-job")), "{calls:?}");
        assert!(calls.contains(&"zfs destroy -- tank/zit-restic-job".to_string()));
        let backup = host.backup_args.lock().unwrap()[0].clone();
        assert!(backup.contains(&format!("--tag=zfsctl-snapshot:{B}")) && backup.contains(&"--tag=zfsctl:job".to_string()));
        assert!(backup.contains(&"--time=2026-10-08 19:33:20".to_string()), "{backup:?}");
        assert_eq!(backup.last().map(String::as_str), Some("/var/run/zfsctl/job"));
        assert!(!host.clone_exists());
        assert!(!host.called("restic forget"), "no retention, no forget");
        assert_eq!((history.len(), history[0].result), (1, Some(RunResult::Ok)));
        assert!(crate::daemon::progress::read_all(&rig.state.path().join("progress")).is_empty());
    }

    #[test]
    fn without_a_working_date_the_backup_runs_without_dash_dash_time() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.date_works = false;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(!host.backup_args.lock().unwrap()[0].iter().any(|a| a.starts_with("--time")));
    }

    #[test]
    fn a_source_without_matching_snapshots_is_skipped_and_nothing_is_created() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.listing = "tank/data@manual\t9\t1\n".to_string();
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(outcome.message.contains("no snapshots"), "{}", outcome.message);
        assert!(!host.called("zfs clone") && !host.called("restic"));
    }

    #[test]
    fn a_snapshot_already_in_the_repository_is_skipped_before_any_clone() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.restic_snapshots = Ok("[{\"id\":\"abc\"}]".to_string());
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(outcome.message.contains("up to date") && outcome.message.contains(B), "{}", outcome.message);
        assert!(!host.called("zfs clone"));
    }

    #[test]
    fn an_unreachable_or_wrongly_locked_repository_fails_the_run_before_cloning() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.restic_snapshots = Err("Fatal: wrong password or no key found".to_string());
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("wrong password"), "{}", outcome.message);
        assert!(!host.called("zfs clone"));
    }

    #[test]
    fn a_leftover_clone_from_a_crashed_run_is_destroyed_first() {
        let rig = Rig::new();
        let host = Host::new();
        *host.clone_exists.lock().unwrap() = true;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        let calls = host.calls();
        let first_destroy = calls.iter().position(|c| c.starts_with("zfs destroy")).unwrap();
        let clone = calls.iter().position(|c| c.starts_with("zfs clone")).unwrap();
        assert!(first_destroy < clone, "{calls:?}");
    }

    #[test]
    fn a_leftover_clone_that_cannot_be_removed_fails_the_run_without_backing_up() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.destroy_fails = true;
        *host.clone_exists.lock().unwrap() = true;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("left by an earlier run") && outcome.message.contains("busy"), "{}", outcome.message);
        assert!(!host.called("zfs clone") && !host.called("restic backup"));
    }

    #[test]
    fn a_failed_clone_is_a_failed_run_with_zfss_message_and_no_backup() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.clone_fails = true;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("failed to create mountpoint"), "{}", outcome.message);
        assert!(!host.called("restic backup"));
    }

    #[test]
    fn a_failing_backup_is_failed_with_resticss_message_destroys_the_clone_and_never_forgets() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.backup_script = r#"echo '{"message_type":"summary","files_new":1,"files_changed":0,"files_unmodified":0,"data_added":1}'; echo '{"message_type":"exit_error","code":1,"message":"Fatal: unable to save snapshot: disk full"}'; exit 1"#.to_string();
        let mut with_policy = job();
        with_policy.retention = Some("7d".to_string());
        let (outcome, history) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Failed, "{}", outcome.message);
        assert!(outcome.message.contains("disk full"), "{}", outcome.message);
        assert!(!host.clone_exists());
        assert!(!host.called("restic forget"));
        assert_eq!(history[0].result, Some(RunResult::Failed));
    }

    #[test]
    fn exit_code_three_means_the_snapshot_exists_but_files_were_unreadable_and_is_a_failure() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.backup_script = format!("{OK_SCRIPT}; echo 'error: permission denied' >&2; exit 3");
        let mut with_policy = job();
        with_policy.retention = Some("7d".to_string());
        let (outcome, _) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("could not read some files"), "{}", outcome.message);
        assert!(!host.clone_exists() && !host.called("restic forget"));
    }

    #[test]
    fn a_cancelled_run_is_recorded_cancelled_destroys_the_clone_and_does_not_forget() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.backup_script = "sleep 60".to_string();
        rig.cancel.store(true, Ordering::SeqCst);
        let mut with_policy = job();
        with_policy.retention = Some("7d".to_string());
        let (outcome, history) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Cancelled, "{}", outcome.message);
        assert_eq!(history[0].result, Some(RunResult::Cancelled));
        assert!(!host.clone_exists() && !host.called("restic forget"));
        assert!(crate::daemon::progress::read_all(&rig.state.path().join("progress")).is_empty());
    }

    #[test]
    fn a_successful_backup_with_a_policy_forgets_only_this_jobs_backups_afterwards() {
        let rig = Rig::new();
        let host = Host::new();
        let mut with_policy = job();
        with_policy.retention = Some("7d 4w".to_string());
        let (outcome, _) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        let calls = host.calls();
        let forget = calls.iter().find(|c| c.starts_with("restic forget")).expect("forget ran");
        assert_eq!(forget, "restic forget --repo=/mnt/usb/restic --password-file=/root/pw --prune --tag=zfsctl:job --host=local --keep-daily=7 --keep-weekly=4");
        assert!(calls.iter().position(|c| c.starts_with("zfs destroy")) < calls.iter().position(|c| c.starts_with("restic forget")), "the clone is gone before the slow prune");
    }

    #[test]
    fn a_blank_policy_forgets_nothing() {
        let rig = Rig::new();
        let host = Host::new();
        let mut blank = job();
        blank.retention = Some("   ".to_string());
        rig.run(&host, &blank);
        assert!(!host.called("restic forget"));
    }

    #[test]
    fn a_failed_forget_is_a_failed_run_that_says_the_backup_exists() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.forget_fails = true;
        let mut with_policy = job();
        with_policy.retention = Some("7d".to_string());
        let (outcome, _) = rig.run(&host, &with_policy);
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains(&format!("backed up {B}")) && outcome.message.contains("forget failed"), "{}", outcome.message);
    }

    #[test]
    fn a_clone_that_cannot_be_destroyed_is_reported_and_the_next_run_retries() {
        let rig = Rig::new();
        let mut host = Host::new();
        host.destroy_fails = true;
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("could not remove the clone") && outcome.message.contains("next run"), "{}", outcome.message);
    }

    #[test]
    fn the_repository_never_appears_in_a_run_message() {
        let rig = Rig::new();
        let host = Host::new();
        let mut secretive = job();
        secretive.repository = Some("sftp:me:hunter2@host:/srv/restic".to_string());
        let (ok, history) = rig.run(&host, &secretive);
        assert!(!ok.message.contains("hunter2") && !format!("{history:?}").contains("hunter2"));
        let mut failing = Host::new();
        failing.clone_fails = true;
        let (bad, _) = rig.run(&failing, &secretive);
        assert!(!bad.message.contains("hunter2"), "{}", bad.message);
    }

    #[test]
    fn a_busy_job_is_skipped_and_touches_nothing() {
        let rig = Rig::new();
        let host = Host::new();
        let _held = rig.locks.try_acquire("local", "#bak@job").unwrap();
        let (outcome, _) = rig.run(&host, &job());
        assert_eq!(outcome.result, RunResult::Skipped);
        assert!(outcome.message.contains("already running"), "{}", outcome.message);
        assert!(host.calls().is_empty());
    }

    #[test]
    fn two_jobs_for_one_dataset_use_separate_clones_and_mountpoints() {
        let mut other = job();
        other.id = "job-two".to_string();
        assert_ne!(clone_name("tank/data", &job().id), clone_name("tank/data", &other.id));
        assert_ne!(mountpoint(&job().id), mountpoint(&other.id));
        let rig = Rig::new();
        let _held = rig.locks.try_acquire("local", "#bak@job");
        let host = Host::new();
        let (outcome, _) = rig.run(&host, &other);
        assert_ne!(outcome.result, RunResult::Skipped, "the other job's lock does not block this one: {}", outcome.message);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test run_lines clone_name 2>&1 | grep -E "^error" | sort | uniq -c | head -5` (run the filters separately if needed)
Expected: compile errors (`run_lines`, `set_total`, `run_backup`, `clone_name` not found).

- [ ] **Step 3: Implement**

`src/daemon/progress.rs`, inside `impl ProgressWriter`:

```rust
    /// The total can become known after the start (restic reports it in its first status lines).
    pub fn set_total(&mut self, total: u64) {
        self.total = Some(total);
    }
```

`src/daemon/transfer.rs`, before `#[cfg(test)]`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lines {
    pub cancelled: bool,
    /// The exit code; `None` when the child was killed by a signal.
    pub code: Option<i32>,
    pub stderr: String,
}

/// Runs `cmd` and hands each line of its stdout to `on_line` as it arrives. Setting `cancel` kills the
/// child; the call still returns once its output ends.
pub fn run_lines(mut cmd: Command, cancel: &AtomicBool, mut on_line: impl FnMut(&str)) -> std::io::Result<Lines> {
    use std::io::BufRead;
    use std::sync::mpsc::{channel, RecvTimeoutError};
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::other("could not open the pipe"));
    };
    let log = drain_stderr(&mut child);
    let (tx, rx) = channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut cancelled = false;
    loop {
        match rx.recv_timeout(TICK) {
            Ok(line) => on_line(&line),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if !cancelled && cancel.load(Ordering::SeqCst) {
            cancelled = true;
            let _ = child.kill();
        }
    }
    let _ = reader.join();
    let status = child.wait()?;
    Ok(Lines { cancelled, code: status.code(), stderr: log.join().unwrap_or_default() })
}
```

Add `pub mod backup;` after `pub mod replicate_file;` in `src/daemon/mod.rs`, and put this above the test module in `src/daemon/backup.rs`:

```rust
//! A scheduled restic backup: clone the newest `<prefix>-<timestamp>` snapshot to a fixed read-only
//! mountpoint (restic matches files against its parent backup by path, so the path must not change
//! between runs), run `restic backup` on it, destroy the clone on every exit path, then optionally
//! `restic forget --prune` this job's own backups. The source's snapshots are never touched.

use super::history::{EventKind, RunResult};
use super::progress::{self, ProgressWriter};
use super::replicate::Env;
use super::scrub::{event, finish, Outcome, Slot};
use super::snapshot::is_own;
use super::transfer::run_lines;
use crate::exec::CommandRunner;
use crate::jobs::{effective_prefix, Job};
use crate::restic::{self, Line, Summary};
use crate::zfs::replication::list_snapshot_infos;
use chrono::{DateTime, Utc};

/// Where a job's clone is mounted: the same path every run, so restic finds its parent backup.
pub const MOUNT_ROOT: &str = "/var/run/zfsctl";

/// `<pool>/zit-restic-<job id>`.
pub fn clone_name(dataset: &str, job_id: &str) -> String {
    let pool = dataset.split('/').next().unwrap_or(dataset);
    format!("{pool}/zit-restic-{job_id}")
}

pub fn mountpoint(job_id: &str) -> String {
    format!("{MOUNT_ROOT}/{job_id}")
}

fn zfs(runner: &dyn CommandRunner, args: &[&str]) -> std::result::Result<String, String> {
    match runner.run("zfs", args) {
        Ok(out) if out.success() => Ok(out.stdout),
        Ok(out) => Err(out.stderr.trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}

fn restic_query(runner: &dyn CommandRunner, args: &[String]) -> std::result::Result<String, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match runner.run("restic", &refs) {
        Ok(out) if out.success() => Ok(out.stdout),
        Ok(out) => Err(restic::error_text(&out.stdout, &out.stderr)),
        Err(e) => Err(e.to_string()),
    }
}

/// The snapshot's creation time as restic's `--time` wants it: in the host's own local time zone.
/// `None` when the host cannot say, and the backup then carries the run time.
fn snapshot_time(runner: &dyn CommandRunner, dataset: &str, snapshot: &str) -> Option<String> {
    let epoch = zfs(runner, &["get", "-H", "-p", "-o", "value", "creation", "--", &format!("{dataset}@{snapshot}")]).ok()?;
    let epoch = epoch.trim();
    if epoch.is_empty() || !epoch.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let out = runner.run_unprivileged("date", &["-r", epoch, "+%Y-%m-%d %H:%M:%S"]).ok().filter(crate::exec::CommandOutput::success)?;
    let text = out.stdout.trim().to_string();
    (text.len() == 19).then_some(text)
}

enum End {
    Done(Option<Summary>),
    Cancelled(u64),
    Failed(String),
}

#[allow(clippy::too_many_arguments)]
fn backup(env: &Env, runner: &dyn CommandRunner, job: &Job, repo: &str, password_file: &str, snapshot: &str, time: Option<&str>, mount: &str, run_id: &str) -> End {
    let args = restic::backup_args(repo, password_file, &job.host, &job.id, snapshot, time, mount);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cmd = match runner.command("restic", &refs) {
        Ok(c) => c,
        Err(e) => return End::Failed(e.to_string()),
    };
    let clock = env.ctx.clock;
    let mut writer = ProgressWriter::new(env.progress_dir.to_path_buf(), run_id.to_string(), job.id.clone(), None, clock.now());
    let (mut summary, mut fatal, mut bytes) = (None, None, 0_u64);
    let result = run_lines(cmd, env.cancel, |line| match restic::parse_line(line) {
        Line::Status { bytes_done, total_bytes } => {
            if total_bytes > 0 {
                writer.set_total(total_bytes);
            }
            bytes = bytes_done;
            writer.update(bytes_done, clock.now());
        }
        Line::Summary(s) => summary = Some(s),
        Line::Error(m) => fatal = Some(m),
        Line::Other => {}
    });
    progress::remove(env.progress_dir, run_id);
    let lines = match result {
        Ok(l) => l,
        Err(e) => return End::Failed(format!("could not run restic: {e}")),
    };
    if lines.cancelled {
        return End::Cancelled(bytes);
    }
    match lines.code {
        Some(0) => End::Done(summary),
        code => {
            let text = restic::error_text(&fatal.map(|m| format!("{{\"message_type\":\"exit_error\",\"message\":{}}}", serde_json::Value::String(m))).unwrap_or_default(), &lines.stderr);
            let what = if code == Some(3) { "restic saved the backup but could not read some files (exit 3)".to_string() } else { format!("restic failed (exit {})", code.map_or_else(|| "signal".to_string(), |c| c.to_string())) };
            End::Failed(format!("{what}: {text}"))
        }
    }
}

pub fn run_backup(env: &Env, runner: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome {
    let ctx = env.ctx;
    let dataset = job.dataset.clone().unwrap_or_default();
    let repo = job.repository.clone().unwrap_or_default();
    let password_file = job.password_file.clone().unwrap_or_default();
    let prefix = effective_prefix(job).to_string();
    let slot = Slot { scheduled_for: Some(scheduled_for), catch_up };
    let failed = |msg: String| finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, msg, slot);
    let skipped = |msg: String| finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, msg, slot);

    // `#` cannot appear in a pool name, so the key cannot collide with another kind's lock.
    let Some(_guard) = ctx.locks.try_acquire(&job.host, &format!("#bak@{}", job.id)) else { return skipped("skipped: already running".to_string()) };
    let clone = clone_name(&dataset, &job.id);
    let mount = mountpoint(&job.id);

    // A clone left by a crashed run would pin its snapshot and block the new clone.
    if zfs(runner, &["list", "-H", "-o", "name", "--", &clone]).is_ok() {
        if let Err(e) = zfs(runner, &["destroy", "--", &clone]) {
            return failed(format!("cannot remove the clone {clone} left by an earlier run: {e}"));
        }
    }

    let source = match list_snapshot_infos(runner, &dataset) {
        Ok(s) => s,
        Err(e) => return failed(e.to_string()),
    };
    let Some(newest) = source.iter().filter(|s| is_own(&prefix, &s.name)).max_by(|a, b| a.name.cmp(&b.name)) else {
        return skipped(format!("skipped: no snapshots with prefix '{prefix}' on {dataset}"));
    };
    let snapshot = newest.name.clone();

    match restic_query(runner, &restic::snapshots_args(&repo, &password_file, &job.id, &snapshot)).and_then(|json| restic::snapshot_count(&json)) {
        Ok(0) => {}
        Ok(_) => return skipped(format!("skipped: up to date ({snapshot})")),
        Err(e) => return failed(e),
    }

    let time = snapshot_time(runner, &dataset, &snapshot);
    let origin = format!("{dataset}@{snapshot}");
    if let Err(e) = zfs(runner, &["clone", "-o", &format!("mountpoint={mount}"), "-o", "readonly=on", "--", &origin, &clone]) {
        return failed(format!("cannot clone {origin}: {e}"));
    }

    let mut started = event(job, run_id, EventKind::Started, ctx.clock.now());
    started.scheduled_for = Some(scheduled_for);
    started.catch_up = catch_up;
    let _ = ctx.history.append(&started);

    let end = backup(env, runner, job, &repo, &password_file, &snapshot, time.as_deref(), &mount, run_id);
    // Always, whatever happened: the clone pins the snapshot and holds a mountpoint.
    let leftover = match zfs(runner, &["destroy", "--", &clone]) {
        Ok(_) => String::new(),
        Err(e) => format!("; could not remove the clone {clone} ({e}), the next run retries"),
    };
    match end {
        End::Cancelled(bytes) => finish(ctx, job, run_id, EventKind::Finished, RunResult::Cancelled, format!("cancelled after {}; the next run starts again{leftover}", progress::human_bytes(bytes)), slot),
        End::Failed(msg) => failed(format!("{msg}{leftover}")),
        End::Done(summary) => {
            let message = match summary {
                Some(s) => format!("backed up {snapshot} ({} new, {} changed, {} unmodified), {} added", s.new, s.changed, s.unmodified, progress::human_bytes(s.added)),
                None => format!("backed up {snapshot}"),
            };
            if !leftover.is_empty() {
                return failed(format!("{message}{leftover}"));
            }
            let policy = job.retention.as_deref().and_then(|t| restic::parse_retention(t).ok()).filter(|r| !r.is_empty());
            if let Some(policy) = policy {
                if let Err(e) = restic_query(runner, &restic::forget_args(&repo, &password_file, &job.host, &job.id, &policy)) {
                    return failed(format!("backed up {snapshot} but forget failed: {e}"));
                }
            }
            finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, message, slot)
        }
    }
}
```

(`finish` appends the Finished history event itself as in `run_replicate`; `progress::human_bytes(2048)` is `2.0 KiB`.)

- [ ] **Step 4: Run the tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"`
Expected: all pass. Fix compile detail only if a name differs (for example if `crate::exec::CommandOutput::success` needs `&` handling in the `.filter`); behavior must match the tests. `cargo clippy 2>&1 | tail -3`: only "never used" warnings for `run_backup` (wired in Task 4).

- [ ] **Step 5: Commit**

```bash
git add -A src
git commit -m "feat: restic backup run: clone, backup, destroy, forget

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Snapshot prune leaves a snapshot that a clone pins

**Files:**
- Modify: `src/daemon/snapshot.rs` — the prune loop in `run_snapshot` (~lines 79-92); tests in its `mod tests` (the `Zfs` double has `destroy_fails: Vec<String>` and answers `fail("snapshot has holds")`)

**Interfaces:**
- Consumes: the existing `Zfs` test double.
- Produces: no new public names; the run message gains `kept N in use by a clone`.

- [ ] **Step 1: Write the failing tests** — in `mod tests` of `src/daemon/snapshot.rs`, first generalise the double so a test can choose the error text: change `destroy_fails: Vec<String>` to keep its type but add a field `destroy_error: String` defaulting to `"snapshot has holds"` in `Zfs::new`, and have the `"destroy"` arm use `fail(&self.destroy_error)` instead of the literal. Then add:

```rust
    #[test]
    fn a_snapshot_pinned_by_a_clone_is_left_alone_and_does_not_fail_the_run() {
        let rig_now = now();
        let mut zfs = Zfs::new(&["tank/data@hourly-20261008T040000Z", "tank/data@hourly-20261008T050000Z", "tank/data@hourly-20261008T060000Z"]);
        zfs.destroy_fails = vec!["tank/data@hourly-20261008T040000Z".to_string()];
        zfs.destroy_error = "cannot destroy 'tank/data@hourly-20261008T040000Z': snapshot has dependent clones\nuse '-R' to destroy the following datasets:\ntank/zit-restic-job".to_string();
        let (outcome, history) = run(&zfs, &job_keeping(2), rig_now);
        assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
        assert!(outcome.message.contains("kept 1 in use by a clone"), "{}", outcome.message);
        assert_eq!(history.last().and_then(|r| r.result), Some(RunResult::Ok));
    }

    #[test]
    fn any_other_destroy_failure_still_fails_the_run() {
        let mut zfs = Zfs::new(&["tank/data@hourly-20261008T040000Z", "tank/data@hourly-20261008T050000Z", "tank/data@hourly-20261008T060000Z"]);
        zfs.destroy_fails = vec!["tank/data@hourly-20261008T040000Z".to_string()];
        let (outcome, _) = run(&zfs, &job_keeping(2), now());
        assert_eq!(outcome.result, RunResult::Failed);
        assert!(outcome.message.contains("snapshot has holds"), "{}", outcome.message);
    }
```

(Use the module's existing helpers for the run and the job; if they are named differently — look at how the existing prune tests build the `Context`, the job and call `run_snapshot` — mirror them exactly: the test names, expected strings and the `destroy_error` knob are what matters. The listing rows are `name\tcreatetxg` style as in `Zfs::new`.)

- [ ] **Step 2: Run to verify the first test fails**

Run: `cargo test a_snapshot_pinned_by_a_clone 2>&1 | grep -E "panicked|FAILED|assert" | head -5`
Expected: FAIL (the run is `Failed: … could not destroy …`).

- [ ] **Step 3: Implement.** In `run_snapshot`'s prune loop replace the match and the final message with:

```rust
    let (mut pruned, mut failures, mut in_use) = (0_usize, Vec::new(), 0_usize);
    // Oldest first, so a failure part-way leaves the newest of the excess in place.
    for old in own.iter().skip(keep.saturating_sub(1)).rev() {
        match destroy_snapshot_with(runner, &dataset, old, job.recursive) {
            Ok(()) => pruned += 1,
            // A backup run's clone pins its snapshot for the length of the run: leave it for the next slot.
            Err(AppError::CommandFailed { stderr, .. }) if stderr.contains("dependent clones") => in_use += 1,
            Err(e) => failures.push(format!("{old}: {e}")),
        }
    }
    let kept = if in_use == 0 { String::new() } else { format!("; kept {in_use} in use by a clone") };
    if failures.is_empty() {
        finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, format!("created {name}; pruned {pruned}{kept}"), slot)
    } else {
        finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, format!("created {name}; pruned {pruned}{kept}; could not destroy {}", failures.join("; ")), slot)
    }
```

- [ ] **Step 4: Run all tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"; cargo clippy 2>&1 | tail -2`
Expected: all pass; clippy unchanged.

- [ ] **Step 5: Commit**

```bash
git add -A src
git commit -m "fix: snapshot prune leaves a snapshot pinned by a clone

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Daemon dispatch

**Files:**
- Modify: `src/daemon/mod.rs` — extract `run_cancellable` from `fn replicate` (~lines 129-161); add `fn backup`; the `match due.job.kind.as_str()` in `execute` (~line 183); tests.

**Interfaces:**
- Consumes: `backup::run_backup`, `Job::test_backup`, `replicate::Env`.
- Produces: `Shared::run_cancellable(&self, ctx: &Context, due: &scheduler::Due, run_id: &str, run: impl FnOnce(&replicate::Env) -> Outcome) -> Outcome`; backup jobs run and are cancellable.

- [ ] **Step 1: Write the failing tests** — append in `mod tests` of `src/daemon/mod.rs`:

```rust
    #[test]
    fn a_due_backup_job_is_run_by_the_backup_code() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let job = Job::test_backup("backup-local-tank-data", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
        shared.execute(Work::Run(Due { job, scheduled_for: shared.clock.now(), catch_up: false }));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Skipped)), "{:?}", runs[0].message);
        assert!(runs[0].message.contains("no snapshots"), "it reached the backup run: {}", runs[0].message);
    }

    #[test]
    fn a_due_backup_job_on_an_unconfigured_host_is_recorded_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let mut job = Job::test_backup("b", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
        job.host = "gone".into();
        shared.execute(Work::Run(Due { job, scheduled_for: shared.clock.now(), catch_up: false }));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Failed)));
        assert!(runs[0].message.contains("gone"), "{}", runs[0].message);
    }

    #[test]
    fn a_second_slot_of_a_running_backup_is_skipped_and_leaves_the_running_ones_cancel_flag_alone() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let job = Job::test_backup("b", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
        let running = Arc::new(AtomicBool::new(false));
        shared.cancels.lock().unwrap().insert(cancel::file_name("b"), Arc::clone(&running));
        shared.execute(Work::Run(Due { job, scheduled_for: shared.clock.now(), catch_up: false }));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Skipped)));
        assert!(runs[0].message.contains("already running"), "{}", runs[0].message);
        assert!(shared.cancels.lock().unwrap().get(&cancel::file_name("b")).is_some_and(|f| Arc::ptr_eq(f, &running)));
    }

    #[test]
    fn a_cancel_request_reaches_a_running_backup_through_the_shared_flag_map() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let running = Arc::new(AtomicBool::new(false));
        shared.cancels.lock().unwrap().insert(cancel::file_name("b"), Arc::clone(&running));
        let cancel_dir = dir.path().join("cancel");
        cancel::request(&cancel_dir, "b").unwrap();
        shared.apply_cancel_requests(&cancel_dir);
        assert!(running.load(Ordering::SeqCst));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test a_due_backup_job 2>&1 | grep -E "FAILED|panicked|unsupported" | head -3`
Expected: FAIL — an unknown kind falls into the scrub arm (the run is recorded for the wrong kind or panics on missing `pool`).

- [ ] **Step 3: Implement.** Replace the whole `fn replicate` (from its doc comment through its closing `}`) with:

```rust
    /// Runs `run` with a cancel flag registered under the job's cancel-file name, so a request file
    /// can reach it. A slot that falls due while this job's previous run is still going must not
    /// replace (and later remove) that run's flag: it is skipped instead.
    fn run_cancellable(&self, ctx: &Context, due: &scheduler::Due, run_id: &str, run: impl FnOnce(&replicate::Env) -> Outcome) -> Outcome {
        let job = &due.job;
        let flag = Arc::new(AtomicBool::new(false));
        let key = cancel::file_name(&job.id);
        {
            let mut running = self.cancels.lock().unwrap_or_else(PoisonError::into_inner);
            if running.contains_key(&key) {
                let slot = scrub::Slot { scheduled_for: Some(due.scheduled_for), catch_up: due.catch_up };
                return scrub::finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, "skipped: already running".to_string(), slot);
            }
            running.insert(key.clone(), Arc::clone(&flag));
        }
        let env = replicate::Env { ctx, progress_dir: &self.progress_dir, cancel: &flag, replication_state: &self.replication_state };
        let outcome = run(&env);
        self.cancels.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        outcome
    }

    /// A host that is no longer configured is a recorded failure.
    fn replicate(&self, ctx: &Context, due: &scheduler::Due, run_id: &str) -> Outcome {
        let job = &due.job;
        let dest_host = job.dest_host.clone().unwrap_or_default();
        let to_file = dest_host == crate::jobs::DEST_FILE;
        let src = self.runner(&job.host);
        let dst = if to_file { None } else { self.runner(&dest_host) };
        let Some(src) = src.filter(|_| to_file || dst.is_some()) else {
            let slot = scrub::Slot { scheduled_for: Some(due.scheduled_for), catch_up: due.catch_up };
            let which = if to_file { format!("host '{}' is", job.host) } else { format!("host '{}' or '{dest_host}' is", job.host) };
            return scrub::finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, format!("{which} not configured"), slot);
        };
        self.run_cancellable(ctx, due, run_id, |env| match &dst {
            Some(dst) => replicate::run_replicate(env, src.as_ref(), dst.as_ref(), job, due.scheduled_for, due.catch_up, run_id),
            None => replicate_file::run_replicate_file(env, src.as_ref(), job, due.scheduled_for, due.catch_up, run_id),
        })
    }

    fn backup(&self, ctx: &Context, due: &scheduler::Due, run_id: &str) -> Outcome {
        let job = &due.job;
        let Some(runner) = self.runner(&job.host) else {
            let slot = scrub::Slot { scheduled_for: Some(due.scheduled_for), catch_up: due.catch_up };
            return scrub::finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, format!("host '{}' is not configured", job.host), slot);
        };
        self.run_cancellable(ctx, due, run_id, |env| backup::run_backup(env, runner.as_ref(), job, due.scheduled_for, due.catch_up, run_id))
    }
```

and in `execute` add the arm next to `KIND_REPLICATE`:

```rust
                    crate::jobs::KIND_BACKUP => Some(self.backup(&ctx, &due, &run_id)),
```

- [ ] **Step 4: Run all tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"; cargo clippy 2>&1 | tail -2`
Expected: all pass (the existing replicate cancel/skip tests still pass through the helper); clippy back to the one pre-existing warning.

- [ ] **Step 5: Commit**

```bash
git add -A src
git commit -m "feat: the daemon runs backup jobs; shared cancel-flag helper

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: The TUI

**Files:**
- Modify: `src/ui/jobs.rs` — `new_backup_job_id`.
- Modify: `src/ui/modal.rs` — `FormKind::BackupJob`.
- Modify: `src/ui/state.rs` — `B` key, `backup_job_form`, `build_backup_job_action`, submit branch, `Enter` edit dispatch, `c` cancel condition, `open_pool_picker` arm, `build_pending_action` `None` arm, help text.
- Modify: `src/ui/picker.rs` — job pickers for the new form.
- Modify: `src/ui/layout.rs` — `form_preview` schedule field, details lines, empty-state text.
- Modify: `src/ui/hints.rs` — `B` hint, cancel hint, scenario/probe updates.

**Interfaces:**
- Consumes: `jobs::{KIND_BACKUP, is_cancellable}`, `restic::{valid_repository, valid_password_file, parse_retention}`, `crate::jobs::resolve`.
- Produces: `FormKind::BackupJob { hosts: Vec<String>, existing: Option<Box<Job>>, jobs: Vec<Job> }` with fields `[host, dataset, prefix, repository, password_file, keep, schedule, catch_up]` (indexes 0-7); `jobs::new_backup_job_id(host, dataset, existing) -> String` (`backup-<host>-<dataset>`, made unique) in `ui/jobs.rs`.

- [ ] **Step 1: Write the failing tests**

`src/ui/jobs.rs` `mod tests`:

```rust
    #[test]
    fn backup_job_ids_are_safe_and_unique() {
        assert_eq!(new_backup_job_id("local", "tank/data", &[]), "backup-local-tank-data");
        assert_eq!(new_backup_job_id("local", "tank/data", &["backup-local-tank-data".to_string()]), "backup-local-tank-data-2");
        assert!(new_backup_job_id("my host", "tank/we ird", &[]).chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')));
    }
```

`src/ui/state.rs` `mod tests` (reuse `in_section`, `form_values`, `repl_kind`-style helpers; define these two new helpers next to the replication ones):

```rust
    const BACKUP_GOOD: [&str; 8] = ["local", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw", "7d 4w", "30 3 * * *", "y"];

    fn backup_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
        FormKind::BackupJob { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), jobs }
    }

    fn backup_fields(values: [&str; 8]) -> Vec<FormField> {
        values.iter().map(|v| FormField { label: "x".to_string(), value: (*v).to_string() }).collect()
    }

    fn backup_err(values: [&str; 8]) -> String {
        build_backup_job_action(&backup_kind(None, vec![]), &backup_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap_err()
    }

    #[test]
    fn big_b_in_jobs_opens_the_backup_form_with_sensible_defaults() {
        let (mut app, _dir) = app_with_jobs(&[]);
        app.handle_key(KeyCode::Char('B'));
        assert_eq!(form_values(&app), ["local", "", "auto", "", "", "", "30 3 * * *", "y"]);
        assert!(matches!(&app.modal, Some(Modal::Form { kind: FormKind::BackupJob { .. }, .. })));
    }

    #[test]
    fn big_b_outside_jobs_does_nothing() {
        let mut app = in_section(Section::Pools);
        app.handle_key(KeyCode::Char('B'));
        assert!(app.modal.is_none());
    }

    #[test]
    fn a_good_backup_form_saves_a_backup_job_with_a_generated_id() {
        let action = build_backup_job_action(&backup_kind(None, vec![]), &backup_fields(BACKUP_GOOD), std::path::Path::new("/x"), chrono::Utc::now()).unwrap();
        let MutationCall::SaveJob { job, .. } = action.call else { panic!("{:?}", action.call) };
        assert_eq!((job.id.as_str(), job.kind.as_str(), job.host.as_str(), job.dataset.as_deref(), job.prefix.as_deref()), ("backup-local-tank-data", "backup", "local", Some("tank/data"), Some("hourly")));
        assert_eq!((job.repository.as_deref(), job.password_file.as_deref(), job.retention.as_deref()), (Some("/mnt/usb/restic"), Some("/root/pw"), Some("7d 4w")));
        assert_eq!((job.schedule.as_str(), job.catch_up, job.enabled), ("30 3 * * *", true, true));
        assert_eq!(action.section, Section::Jobs);
    }

    #[test]
    fn a_blank_keep_field_saves_no_retention_and_a_blank_prefix_means_the_default() {
        let mut values = BACKUP_GOOD;
        values[5] = "  ";
        values[2] = "";
        let action = build_backup_job_action(&backup_kind(None, vec![]), &backup_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap();
        let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
        assert_eq!((job.retention, job.prefix.as_deref()), (None, Some(crate::jobs::DEFAULT_PREFIX)));
    }

    #[test]
    fn the_backup_form_refuses_what_the_daemon_would_refuse() {
        let with = |i: usize, v: &'static str| {
            let mut vals = BACKUP_GOOD;
            vals[i] = v;
            vals
        };
        assert!(backup_err(with(0, "nope")).contains("host"));
        assert!(backup_err(with(1, "-rf")).contains("dataset"));
        assert!(backup_err(with(2, "a b")).contains("prefix"));
        assert!(backup_err(with(3, "")).contains("repository"));
        assert!(backup_err(with(3, "-r")).contains("repository"));
        assert!(backup_err(with(4, "relative")).contains("password_file"));
        assert!(backup_err(with(5, "7x")).contains("retention"));
        assert!(backup_err(with(6, "bogus")).contains("Schedule"));
        assert!(backup_err(with(7, "q")).contains("Catch up"));
    }

    #[test]
    fn enter_on_a_backup_job_edits_it_in_the_backup_form_and_keeps_its_id_and_state() {
        let mut job = crate::jobs::Job::test_backup("backup-x", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
        job.retention = Some("7d".to_string());
        job.enabled = false;
        let (mut app, _dir) = app_with_jobs(&[job.clone()]);
        app.jobs_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        assert_eq!(form_values(&app), ["local", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw", "7d", "0 3 * * *", "y"]);
        let kind = backup_kind(Some(job.clone()), vec![job]);
        let mut values = BACKUP_GOOD;
        values[3] = "/mnt/other";
        let action = build_backup_job_action(&kind, &backup_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap();
        let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
        assert_eq!((job.id.as_str(), job.enabled, job.repository.as_deref()), ("backup-x", false, Some("/mnt/other")));
    }

    #[test]
    fn c_cancels_a_running_backup_but_not_an_idle_one() {
        let job = crate::jobs::Job::test_backup("backup-x", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
        let (mut app, dir) = app_with_jobs(&[job]);
        app.jobs_table.select(Some(0));
        app.handle_key(KeyCode::Char('c'));
        assert!(crate::daemon::cancel::take_requests(&dir.path().join("cancel")).is_empty(), "not running: nothing to cancel");
    }
```

(The last test only asserts the idle case; the running case reuses the existing replication cancel test's setup — copy that test's way of making `jobs_view.is_running` true and assert `take_requests(...) == ["backup-x"]` for a backup job. Find it with `grep -n "Cancel requested" src/ui/state.rs`.)

`src/ui/picker.rs` `mod tests`:

```rust
    #[test]
    fn the_backup_form_offers_hosts_live_datasets_and_schedule_presets() {
        let kind = FormKind::BackupJob { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: None, jobs: vec![] };
        assert_eq!(build_picker_state(&kind, 0, "fbsd16").unwrap().values, vec!["local", "fbsd16"]);
        assert!(build_picker_state(&kind, 1, "").unwrap().pool_host.is_some(), "the dataset field fetches live names");
        assert!(!build_picker_state(&kind, 6, "").unwrap().values.is_empty(), "schedule presets");
        for text_field in [2, 3, 4, 5, 7] {
            assert!(build_picker_state(&kind, text_field, "").is_none(), "field {text_field} has no vocabulary");
        }
    }
```

`src/ui/layout.rs` `mod tests` (uses the module's `render` helper and `Section`):

```rust
    #[test]
    fn a_backup_job_shows_its_repository_retention_and_progress_in_the_details() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Jobs).unwrap();
        let mut job = crate::jobs::Job::test_backup("backup-x", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
        job.retention = Some("7d 4w".to_string());
        app.jobs_view.jobs = vec![job];
        app.jobs_table.select(Some(0));
        let text = render(&mut app, 140, 30).join("\n");
        for want in ["tank/data", "to /mnt/usb/restic", "keep 7d 4w", "prefix hourly"] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
        assert!(!text.contains("password"), "the password file path is not shown");
    }
```

`src/ui/hints.rs` `mod tests`: the Jobs scenarios already exist; extend:

```rust
    #[test]
    fn the_jobs_section_advertises_b_for_a_backup_job_and_c_only_for_a_running_cancellable_one() {
        let a = jobs(true);
        let labels: Vec<(&str, &str)> = hints(&a).iter().map(|h| (h.key, h.label)).collect();
        assert!(labels.contains(&("B", "New backup job")), "{labels:?}");
        assert!(!labels.iter().any(|(k, _)| *k == "c"), "nothing is running");
    }
```

and add `'B'` to the `probes` array in `keys_that_are_not_advertised_are_no_ops` (the Jobs scenarios advertise it, every other section must treat it as inert).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test 2>&1 | grep -E "^error" | sort | uniq -c | head -5`
Expected: compile errors (`new_backup_job_id`, `FormKind::BackupJob`, `build_backup_job_action`).

- [ ] **Step 3: Implement.**

`src/ui/jobs.rs`, after `new_snapshot_job_id`:

```rust
/// `backup-<host>-<dataset>`, made safe the same way.
pub fn new_backup_job_id(host: &str, dataset: &str, existing: &[String]) -> String {
    unique(&format!("backup-{}-{}", safe_key(host), safe_key(dataset)), existing)
}
```

`src/ui/modal.rs`, in `enum FormKind` after `ReplicateJob`:

```rust
    /// Fields: `[host, dataset, prefix, repository, password file, keep, schedule, catch_up (y/n)]`.
    /// `keep` is blank (keep everything) or text like `7d 4w 12m 3y`. `jobs` is every current job.
    BackupJob { hosts: Vec<String>, existing: Option<Box<crate::jobs::Job>>, jobs: Vec<crate::jobs::Job> },
```

`src/ui/picker.rs`, in the `job_form` match in `build_picker_state` add `FormKind::BackupJob { hosts, .. } => Some(JobPickers { hosts, host_fields: &[0], live_fields: &[1], schedule_field: 6 }),`.

`src/ui/layout.rs`: in `form_preview` add `crate::ui::modal::FormKind::BackupJob { .. } => crate::ui::jobs::preview_lines(fields.get(6).map_or("", |f| f.value.as_str()), &chrono::Local::now()),`; in the job details (`if job.kind == crate::jobs::KIND_REPLICATE { … }`) add after it:

```rust
    if job.kind == crate::jobs::KIND_BACKUP {
        let keep = job.retention.as_deref().map(str::trim).filter(|k| !k.is_empty()).map_or_else(|| "keep everything".to_string(), |k| format!("keep {k}"));
        info.push(Line::from(format!("to {}  {keep}  prefix {}", job.repository.as_deref().unwrap_or(""), crate::jobs::effective_prefix(job))));
        if let Some(p) = app.jobs_view.progress_of(&job.id) {
            info.push(Line::from(crate::daemon::progress::progress_line(p)));
        }
    }
```

and extend the empty-state sentence to `…'R' for a replication job, 'B' for a backup job, or 'J' on …`.

`src/ui/hints.rs`: after the `R` hint add `v.push(h("B", "New backup job", 3));`, and change the cancel condition `job.kind == crate::jobs::KIND_REPLICATE && app.jobs_view.is_running(&job.id)` to `crate::jobs::is_cancellable(job) && app.jobs_view.is_running(&job.id)`.

`src/ui/state.rs`:
1. Keys: next to `KeyCode::Char('R') if … Section::Jobs`:

```rust
            KeyCode::Char('B') if self.current_section() == Section::Jobs => {
                self.modal = Some(self.backup_job_form(None));
            }
```

2. In the `c` handler change `job.kind == crate::jobs::KIND_REPLICATE && self.jobs_view.is_running(&job.id)` to `crate::jobs::is_cancellable(job) && self.jobs_view.is_running(&job.id)`.
3. In the `Enter if Jobs` match add `crate::jobs::KIND_BACKUP => self.backup_job_form(Some(job)),`.
4. In `open_pool_picker` change the second arm to `FormKind::SnapshotJob { hosts, .. } | FormKind::ReplicateJob { hosts, .. } | FormKind::BackupJob { hosts, .. } => (hosts, crate::ui::picker::LiveNames::Datasets),`.
5. In the submit chain add a branch next to the `ReplicateJob` one:

```rust
                    } else if matches!(kind, FormKind::BackupJob { .. }) {
                        match build_backup_job_action(&kind, &fields, &self.state_dir, chrono::Utc::now()) {
                            Ok(mut action) => {
                                action.host = self.selected_host;
                                Some(action)
                            }
                            Err(msg) => {
                                self.set_error(msg);
                                self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
                                None
                            }
                        }
```

6. In `build_pending_action`'s exhaustive `None` arm add `| FormKind::BackupJob { .. }`.
7. The form and builder (next to `replicate_job_form` / `build_replicate_job_action`):

```rust
impl App {
    fn backup_job_form(&self, existing: Option<crate::jobs::Job>) -> Modal {
        let yn = |b: bool| if b { "y" } else { "n" }.to_string();
        let (title, host, dataset, prefix, repository, password_file, keep, schedule, catch_up) = match &existing {
            Some(j) => (
                format!("Edit backup job '{}'", j.id),
                j.host.clone(),
                j.dataset.clone().unwrap_or_default(),
                crate::jobs::effective_prefix(j).to_string(),
                j.repository.clone().unwrap_or_default(),
                j.password_file.clone().unwrap_or_default(),
                j.retention.clone().unwrap_or_default(),
                j.schedule.clone(),
                j.catch_up,
            ),
            None => ("New backup job".to_string(), self.active_host_name().to_string(), String::new(), crate::jobs::DEFAULT_PREFIX.to_string(), String::new(), String::new(), String::new(), "30 3 * * *".to_string(), true),
        };
        Modal::Form {
            title,
            fields: vec![
                FormField { label: "Host (where restic runs)".to_string(), value: host },
                FormField { label: "Dataset".to_string(), value: dataset },
                FormField { label: "Prefix (snapshots to back up)".to_string(), value: prefix },
                FormField { label: "Repository (restic -r; no credentials in it)".to_string(), value: repository },
                FormField { label: "Password file (absolute path on the host)".to_string(), value: password_file },
                FormField { label: "Keep (e.g. 7d 4w 12m 3y 5l; blank = keep everything)".to_string(), value: keep },
                FormField { label: "Schedule".to_string(), value: schedule },
                FormField { label: "Catch up (y/n)".to_string(), value: yn(catch_up) },
            ],
            focused: 0,
            description: Some("Backs up the newest <prefix>-<timestamp> snapshot (it never takes snapshots) through a temporary read-only clone. The repository must already exist (restic init).".to_string()),
            kind: FormKind::BackupJob { hosts: self.hosts.clone(), existing: existing.map(Box::new), jobs: self.jobs_view.jobs.clone() },
        }
    }
}

/// Turns a submitted backup form into a save, or says what is wrong. It refuses exactly what the
/// daemon would refuse (`jobs::resolve`).
fn build_backup_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
    let FormKind::BackupJob { hosts, existing, jobs } = kind else { return Err("not a backup job form".to_string()) };
    let yes_no = |label: &str, raw: &str, blank: bool| match raw.trim().to_lowercase().as_str() {
        "" => Ok(blank),
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        other => Err(format!("{label} must be y or n, not '{other}'")),
    };
    let host = field_value(fields, 0).trim();
    if !hosts.iter().any(|h| h == host) {
        return Err(format!("Unknown host '{host}'. Configured hosts: {}", hosts.join(", ")));
    }
    let dataset = field_value(fields, 1).trim();
    let prefix = match field_value(fields, 2).trim() {
        "" => crate::jobs::DEFAULT_PREFIX,
        p => p,
    };
    let repository = field_value(fields, 3).trim();
    let password_file = field_value(fields, 4).trim();
    let keep = field_value(fields, 5).trim();
    let schedule = field_value(fields, 6).trim();
    crate::daemon::cron::Schedule::parse(schedule).map_err(|e| format!("Schedule: {e}"))?;
    let catch_up = yes_no("Catch up", field_value(fields, 7), true)?;
    let taken: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
    let base = crate::jobs::Job {
        id: String::new(),
        host: host.to_string(),
        kind: crate::jobs::KIND_BACKUP.to_string(),
        pool: None,
        dataset: Some(dataset.to_string()),
        recursive: false,
        prefix: Some(prefix.to_string()),
        keep: None,
        dest_host: None,
        dest_dataset: None,
        dest_dir: None,
        force: false,
        repository: Some(repository.to_string()),
        password_file: Some(password_file.to_string()),
        retention: (!keep.is_empty()).then(|| keep.to_string()),
        schedule: schedule.to_string(),
        catch_up,
        enabled: true,
        created_at: Some(now),
    };
    let job = match existing {
        Some(old) => crate::jobs::Job { id: old.id.clone(), enabled: old.enabled, created_at: old.created_at, ..base },
        None => crate::jobs::Job { id: crate::ui::jobs::new_backup_job_id(host, dataset, &taken), ..base },
    };
    crate::jobs::resolve(&job, hosts)?;
    Ok(PendingAction { host: 0, section: Section::Jobs, call: MutationCall::SaveJob { state_dir: state_dir.to_path_buf(), job: Box::new(job) } })
}
```

8. Help text: in the `Jobs` block of `help_text()` add the line `  B                new backup job (restic: back up a snapshot through a temporary clone)` and mention backups in the `c` line (`cancel a running replication or backup`). Add a test that `help_text()` contains `B ` and `backup`.

- [ ] **Step 4: Run all tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"; cargo clippy 2>&1 | tail -2`
Expected: all pass, including `every_advertised_key_actually_does_something` and `keys_that_are_not_advertised_are_no_ops`; clippy back to the one pre-existing warning. If a drift guard fails, the hint and the handler disagree: fix the handler, not the test.

- [ ] **Step 5: Commit**

```bash
git add -A src
git commit -m "feat: backup job form, B key, details and cancel in the Jobs section

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Docs and live verification

**Files:**
- Modify: `README.md` (Scheduled jobs: a "Backups with restic" subsection after the replication/file-target text and before **Notifications**; the sudoers section; the Jobs keys line)
- Modify: `docs/superpowers/specs/2026-10-08-restic-backups-design.md` (field names; live results)

- [ ] **Step 1: README.** Add this subsection before `**Notifications.**`:

````markdown
**A backup job** runs `restic backup` on a dataset's newest `<prefix>-<timestamp>` snapshot on the job's host (the hub is `local`). It never takes snapshots: pair it with a snapshot job that uses the same prefix.

```toml
[[jobs]]
id = "backup-local-tank-data"
host = "local"
kind = "backup"
dataset = "tank/data"
prefix = "hourly"
repository = "/mnt/usb/restic-tank"      # restic's -r: a path, sftp:user@host:/path, rclone:remote:path ...
password_file = "/root/.restic-password"  # absolute path on the host, mode 600
retention = "7d 4w 12m"                   # optional: daily/weekly/monthly/yearly/last; blank keeps everything
schedule = "30 3 * * *"
```

- **One-time setup.** `pkg install restic` on the host; put the repository password in a root-only file (`install -m 600 /dev/null /root/.restic-password`, then write it); create the repository once with `restic init --repo <repository> --password-file <file>`. A backup job never creates a repository (a mistyped path would silently make a new one); an uninitialised repository or wrong password fails the run with restic's own message.
- **How a run works.** It picks the newest matching snapshot, skips (`up to date`) if the repository already holds a backup tagged `zfsctl-snapshot:<name>` from this job, then clones that snapshot read-only to `/var/run/zfsctl/<job id>`, backs up that path, and destroys the clone, whether the backup succeeded, failed or was cancelled. The path is the same every run on purpose: restic finds its parent backup by path, so unchanged files are skipped instead of read again. While the clone exists its snapshot cannot be destroyed: a snapshot job's prune leaves it for the next slot (`kept 1 in use by a clone`). A clone left by a crash is destroyed by the next run.
- **Retention.** With `retention` set, a successful backup is followed by `restic forget --prune` limited to this job's own backups from this host (matched by tag and host); without it nothing is ever forgotten. Backups are dated by their snapshot's creation time, so `7d` means seven days of data, not seven runs. `prune` takes the repository lock and can be slow or costly on a big remote repository.
- **Secrets.** The job holds no password or key, only the path of the password file on the host. The repository string is stored and shown in the Jobs details, so never put credentials in it (`rest:https://user:pass@host/` is the case to avoid; use an rclone remote, whose config holds the keys). Direct S3/Azure/B2 environment variables are not supported, because an environment cannot pass through `sudo -n` without allowing `env`, which would make the sudoers entry root-equivalent.
- **Cancel and locks.** `c` stops a running backup; restic can leave a stale repository lock, which `restic unlock` removes (zfsctl never removes locks). A cancelled or failed run does not forget. restic exit code 3 (snapshot saved but some files unreadable) is reported as a failure that says so.
- **Restoring.** Use restic: `restic restore latest --repo <repository> --password-file <file> --tag zfsctl:<job id> --target /restore`. The backed-up path is `/var/run/zfsctl/<job id>`.
- **Remote hosts.** restic runs on the job's host over SSH + `sudo -n`, so it must be installed there and `/usr/local/bin/restic` added to that host's sudoers entry. That entry is as broad as `zfs` or `bectl`: restic as root reads any file and writes anywhere, so whoever holds the SSH key effectively has root on that host. The hub (`local`) needs no change.
````

In the sudoers section add `restic` (only needed on hosts that run backup jobs) to the list of privileged binaries with a sentence, and `B` to the Jobs keys. Mention `kind = "backup"` fields `repository`, `password_file`, `retention` where the other job fields are documented if there is such a list.

- [ ] **Step 2: Spec.** In the spec's job table rename the `keep` row to `retention` with a note ("the snapshot job's numeric `keep` already uses that name; the form label stays 'Keep'"), and note in the forget step `--keep-*=N` flag form. Append a `## Live results (2026-10-08)` section in Step 4.

- [ ] **Step 3: Install restic on the hub — STOP and ask the user first.** The hub is the user's NAS; `pkg install restic` is a change outside this repository. Ask before running it, and ask before adding any sudoers entry on a VM (that needs root there). If the user declines, skip Steps 4-5, run only the local checks below, and say so in the final report.

Local checks that need no hub change (workstation, restic 0.19.1 installed): with the real binary in place of the double, point a throwaway job at a plain directory standing in for the clone, run `restic backup --json` twice and confirm the second prints `files_unmodified` equal to the file count and `files_new` 0 when the path is the same.

- [ ] **Step 4: Live checks on the hub** (after the user agreed to `pkg install restic`; deploy as in [[project-zfsctl-freebsd-test-box]]: `scp` the changed `src/` files and `README.md`, `cargo test && cargo build --release` on the hub, `sudo install -m 755 target/release/zfsctl /usr/local/bin/zfsctl`, `sudo service zfsctl restart`; the login shell of `fb` is not sh, so run multi-line commands via `sudo sh -s`). Use only the `scratch` pool (a dataset `scratch/bsrc` with ~3 MB of random files) and a local repository `/var/tmp/zit-restic` with password file `/var/tmp/zit-restic.pw` (mode 600); delete all of it afterwards:
  1. Record `restic version` and `zfs get -H -p -o value creation scratch/bsrc@…` and `date -r <epoch> "+%Y-%m-%d %H:%M:%S"` outputs; confirm they match what the code parses.
  2. `restic init`, snapshot `scratch/bsrc@hourly-20261009T010000Z`, define the job through the TUI (`B`) with schedule `* * * * *`. Within a minute: `Ok: backed up … (N new, 0 changed, 0 unmodified)`, no `zit-restic-*` dataset left (`zfs list -r scratch`), nothing mounted under `/var/run/zfsctl`, `restic snapshots` shows one snapshot with the two tags and the snapshot's creation time.
  3. Next minute: `skipped: up to date`. Add a file and a second snapshot: the next run reports `0 new … unmodified = (all earlier files)` for the unchanged files (**the claim the design rests on: clone inodes and ctimes are stable, so restic skips unchanged files**; if it reports them as new or changed, stop and report: the stable-path design needs a rethink).
  4. **Restore:** `restic restore latest --target /var/tmp/zit-restore …`; `sha256` of the restored files equals the source's.
  5. **Retention:** set `retention = "1l"`; after the next backup `restic snapshots` shows one snapshot and the job message is `Ok`.
  6. **Pinned snapshot vs snapshot job:** create a snapshot job with keep 1 and the same prefix on `scratch/bsrc` while a backup of the older snapshot is running (use a larger file to make it long enough, or hold the clone by hand with `zfs clone` of the old snapshot): the snapshot job's run is `Ok: … kept 1 in use by a clone`, then prunes it after the clone is gone. Record zfs's exact "dependent clones" text.
  7. **Errors:** a wrong password file content, an uninitialised repository path, and a clone failure (set `readonly`-incompatible mountpoint or point the job at a locked/absent dataset): each a `Failed` with restic's or zfs's message and no leftover clone.
  8. **Cancel:** with a larger dataset (the hub's `scratch` has ~15 MB free; if too small run this on `fbsd16t1` after the user adds the sudoers line), press `c` while the progress line moves: `Cancelled`, clone gone, repository usable (`restic check` clean after `restic unlock` if a lock remains).
  9. **kill -9 mid-run:** the next run destroys the leftover clone first and succeeds.
  10. Clean up: delete the jobs from the TUI (or set `jobs = []`), `zfs destroy -r scratch/bsrc`, remove the repository, password file and restore directory, confirm `zfs list -r scratch` shows only the pre-existing `scratch/data` tree.

- [ ] **Step 5: Record, commit, review, finish.**

Append the live results to the spec (what steps 1-9 showed, with the restic and zfs version strings and message texts), then:

```bash
cargo test 2>&1 | grep -E "^test result|FAILED"
git add README.md docs
git commit -m "docs: restic backups; live results

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

Then one fresh whole-branch review on the most capable model with the Review Focus section verbatim, a single fix pass for Critical/Important findings (each with a failing test first), update the memory notes, and finish with superpowers:finishing-a-development-branch (the merge into `master` is the user's choice).
