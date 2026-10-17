//! The Cloud section's data and the pure helpers its table and panel use: which rclone remotes there are, which
//! jobs write to them, what the last run said, and how big the bucket's `current/` and `old/` folders are.
//!
//! Everything here reads or computes; nothing here draws, handles keys or runs a command. Nothing here can
//! change a remote: the numbers come from `rclone listremotes --long` and `rclone size`, which only read.

use crate::daemon::history::{Run, RunResult};
use crate::daemon::progress::human_bytes;
use crate::exec::CommandRunner;
use crate::jobs::{self, Job};
use crate::rclone;
use crate::ui::jobs::{job_target, last_result_text, schedule_label, JobsView};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// `(remote name, rclone type)` per host name, as `rclone listremotes --long` reported them.
pub type Known = HashMap<String, Vec<(String, String)>>;

/// The rclone remote a job writes to: a cloud mirror's remote, or a restic backup's `rclone:` repository.
pub fn remote_name(job: &Job) -> Option<String> {
    let text = match job.kind.as_str() {
        jobs::KIND_MIRROR => job.remote.as_deref()?,
        jobs::KIND_BACKUP => job.repository.as_deref()?.strip_prefix("rclone:")?,
        _ => return None,
    };
    let name = text.split(':').next()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// One line of the Cloud table: a remote on a host and the jobs that use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub host: String,
    pub name: String,
    /// rclone's type (`b2`, `s3`, `sftp`...), or `None` while the host's list is unknown or does not have it.
    pub kind: Option<String>,
    pub job_ids: Vec<String>,
}

/// Every configured remote of the hosts in `known`, plus every remote a job names (even one the host no longer has
/// or whose list is not known yet), each with its jobs. The hub's (`local`) first, then by host and name.
pub fn rows(jobs: &[Job], known: &Known) -> Vec<Row> {
    let mut rows: Vec<Row> = known.iter().flat_map(|(host, list)| list.iter().map(|(name, kind)| Row { host: host.clone(), name: name.clone(), kind: Some(kind.clone()), job_ids: vec![] })).collect();
    for job in jobs {
        let Some(name) = remote_name(job) else { continue };
        match rows.iter_mut().find(|r| r.host == job.host && r.name == name) {
            Some(row) => row.job_ids.push(job.id.clone()),
            None => rows.push(Row { host: job.host.clone(), name, kind: None, job_ids: vec![job.id.clone()] }),
        }
    }
    rows.sort_by(|a, b| (a.host != "local", &a.host, &a.name).cmp(&(b.host != "local", &b.host, &b.name)));
    rows
}

/// What `s` measured for one cloud mirror job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Size {
    /// `(files, bytes)` of `<remote>/current`.
    pub current: Option<(u64, u64)>,
    /// `(files, bytes)` of `<remote>/old`; `None` when there is no `old/` yet.
    pub old: Option<(u64, u64)>,
    pub at: DateTime<Utc>,
    /// Why `current` could not be measured.
    pub problem: Option<String>,
}

/// `rclone listremotes --long` on the host behind `runner`: the configured remotes and their types, or why not.
pub fn list_remotes(runner: &dyn CommandRunner, host: &str) -> Result<Vec<(String, String)>, String> {
    let args = rclone::listremotes_args();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match runner.run("rclone", &refs) {
        Ok(out) if out.success() => Ok(rclone::parse_listremotes(&out.stdout)),
        Ok(out) => match rclone::classify_check(out.status, &out.stdout, &out.stderr, "remote:", host) {
            rclone::Check::Problem(p) => Err(p),
            rclone::Check::Ok => Err("rclone failed".to_string()),
        },
        Err(e) => Err(rclone::redact(&e.to_string(), "remote:")),
    }
}

/// `(files, bytes)` of one folder of a remote. `Ok(None)` when the folder does not exist (rclone exit 3).
fn size_of(runner: &dyn CommandRunner, host: &str, remote: &str, folder: &str) -> Result<Option<(u64, u64)>, String> {
    let args = rclone::size_args(&rclone::join(remote, folder));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match runner.run("rclone", &refs) {
        Ok(out) if out.success() => rclone::parse_size(&out.stdout).map(Some),
        Ok(out) if out.status == 3 => Ok(None),
        Ok(out) => match rclone::classify_check(out.status, &out.stdout, &out.stderr, remote, host) {
            rclone::Check::Problem(p) => Err(p),
            rclone::Check::Ok => Err("rclone failed".to_string()),
        },
        Err(e) => Err(rclone::redact(&e.to_string(), remote)),
    }
}

/// Measures `<remote>/current` and `<remote>/old` with `rclone size` (read-only; it lists the bucket, so it costs
/// requests, which is why it only runs when asked).
pub fn measure(runner: &dyn CommandRunner, host: &str, remote: &str, now: DateTime<Utc>) -> Size {
    let current = size_of(runner, host, remote, "current");
    let old = if current.is_ok() { size_of(runner, host, remote, "old") } else { Ok(None) };
    match (current, old) {
        // A folder that is not there reads as empty on S3 and B2, and as an error on a local or sftp remote:
        // either way there is no `old/` yet.
        (Ok(current), Ok(old)) => Size { current, old: old.filter(|&(files, _)| files > 0), at: now, problem: None },
        (Err(p), _) | (_, Err(p)) => Size { current: None, old: None, at: now, problem: Some(p) },
    }
}

fn jobs_of<'a>(row: &Row, view: &'a JobsView) -> Vec<&'a Job> {
    row.job_ids.iter().filter_map(|id| view.jobs.iter().find(|j| &j.id == id)).collect()
}

/// The newest run among the row's jobs.
pub fn last_run<'a>(row: &Row, view: &'a JobsView) -> Option<&'a Run> {
    row.job_ids.iter().filter_map(|id| view.last_run(id)).max_by_key(|r| r.started_at)
}

/// Whether the latest run of any of the row's enabled jobs failed.
pub fn failed(row: &Row, view: &JobsView) -> bool {
    jobs_of(row, view).iter().any(|j| j.enabled && view.last_run(&j.id).is_some_and(|r| r.result == Some(RunResult::Failed)))
}

/// What the row's measured mirror jobs hold in the bucket: `current/` plus `old/`, which is what a plan counts. `None`
/// before `s` (or when nothing could be measured).
pub fn used_bytes(row: &Row, sizes: &HashMap<String, Size>) -> Option<u64> {
    let measured: Vec<u64> = row.job_ids.iter().filter_map(|id| sizes.get(id)).filter(|s| s.current.is_some()).map(|s| s.current.map_or(0, |c| c.1) + s.old.map_or(0, |o| o.1)).collect();
    (!measured.is_empty()).then(|| measured.iter().sum())
}

/// A remote's plan cap, as the Cloud screen has it: the size, or the text that is not a size.
pub type Caps = HashMap<String, Result<u64, String>>;

/// How full the plan is, as a percentage with enough decimals to tell a little from nothing: `0.02%`, `3.4%`, `87%`.
pub fn percent_text(used: u64, cap: u64) -> String {
    let pct = used as f64 * 100.0 / cap as f64;
    if pct < 1.0 { format!("{pct:.2}%") } else if pct < 10.0 { format!("{pct:.1}%") } else { format!("{pct:.0}%") }
}

/// A plan size as it reads on screen: `10 GB`, `2.5 GB`, `500 MB` (decimal, like the plan).
pub fn cap_text(bytes: u64) -> String {
    let (value, unit) = match bytes {
        b if b >= 1_000_000_000_000 => (b as f64 / 1e12, "TB"),
        b if b >= 1_000_000_000 => (b as f64 / 1e9, "GB"),
        b if b >= 1_000_000 => (b as f64 / 1e6, "MB"),
        b if b >= 1_000 => (b as f64 / 1e3, "KB"),
        b => return format!("{b} B"),
    };
    if (value - value.round()).abs() < 0.05 { format!("{value:.0} {unit}") } else { format!("{value:.1} {unit}") }
}

/// How full a plan is, as the colour to show it in: nothing special below 80%, a warning from 80%, a problem from 95%.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Fine,
    Warn,
    Full,
}

pub fn fill(used: u64, cap: u64) -> Fill {
    let pct = used as f64 * 100.0 / cap as f64;
    if pct >= 95.0 { Fill::Full } else if pct >= 80.0 { Fill::Warn } else { Fill::Fine }
}

/// The table's In bucket column: what the row holds (`-` before `s`), and against its cap, if it has one, `of 10 GB (0.02%)`.
pub fn in_bucket(row: &Row, sizes: &HashMap<String, Size>, caps: &Caps) -> String {
    let Some(used) = used_bytes(row, sizes) else { return "-".to_string() };
    match caps.get(&row.name) {
        Some(Ok(cap)) => format!("{} of {} ({})", human_bytes(used), cap_text(*cap), percent_text(used, *cap)),
        _ => human_bytes(used),
    }
}

/// The row's fill against its plan, once measured.
pub fn row_fill(row: &Row, sizes: &HashMap<String, Size>, caps: &Caps) -> Option<Fill> {
    match (used_bytes(row, sizes), caps.get(&row.name)) {
        (Some(used), Some(Ok(cap))) => Some(fill(used, *cap)),
        _ => None,
    }
}

/// The table's cells for a row: Remote, Host, Type, Jobs, Last, In bucket.
pub fn cells(row: &Row, view: &JobsView, sizes: &HashMap<String, Size>, caps: &Caps, now: DateTime<Utc>) -> Vec<String> {
    let last = if row.job_ids.is_empty() { "-".to_string() } else { last_result_text(last_run(row, view), now) };
    vec![format!("{}:", row.name), row.host.clone(), row.kind.clone().unwrap_or_else(|| "?".to_string()), row.job_ids.len().to_string(), last, in_bucket(row, sizes, caps)]
}

/// How a measured folder reads: `1.9 MiB (4 files)`.
fn folder_text(folder: (u64, u64)) -> String {
    let (files, bytes) = folder;
    format!("{} ({files} file{})", human_bytes(bytes), if files == 1 { "" } else { "s" })
}

/// The panel's plan line: the cap from `[cloud.caps]`, how much of it is used, and what is not counted.
fn plan_lines(row: &Row, sizes: &HashMap<String, Size>, caps: &Caps) -> Vec<Line> {
    match caps.get(&row.name) {
        None => vec![],
        Some(Err(text)) => vec![(format!("Plan cap '{text}' in [cloud.caps] is not a size like 10G, 500M or 2.5T"), true)],
        Some(Ok(cap)) => match used_bytes(row, sizes) {
            None => vec![(format!("Plan cap {}: press s to see how much of it is used", cap_text(*cap)), false)],
            Some(used) => {
                let left = cap.saturating_sub(used);
                let text = format!("Plan cap {}: using {} ({}), {} left. Counts only this remote's measured mirror jobs (current/ and old/).", cap_text(*cap), human_bytes(used), percent_text(used, *cap), cap_text(left));
                vec![(text, fill(used, *cap) != Fill::Fine)]
            }
        },
    }
}

/// A line of the detail panel and whether it is a problem.
pub type Line = (String, bool);

/// The detail panel for a row: each job that uses the remote, then what `s` measured for it.
pub fn detail(row: &Row, view: &JobsView, sizes: &HashMap<String, Size>, caps: &Caps, now: DateTime<Utc>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let kind = row.kind.as_deref().map_or_else(String::new, |k| format!(" ({k})"));
    out.push((format!("{}:{kind} on {}", row.name, row.host), false));
    out.extend(plan_lines(row, sizes, caps));
    let jobs = jobs_of(row, view);
    if jobs.is_empty() {
        out.push(("No job uses this remote. Press C to mirror a dataset to it.".to_string(), false));
    }
    for job in jobs {
        let state = if job.enabled { String::new() } else { "  (disabled)".to_string() };
        let last = last_result_text(view.last_run(&job.id), now);
        let bad = view.last_run(&job.id).is_some_and(|r| r.result == Some(RunResult::Failed));
        out.push((format!("{}  {}  {}  last {last}{state}", job.id, job_target(job), schedule_label(&job.schedule)), bad));
        if let Some(run) = view.last_run(&job.id).filter(|r| r.result == Some(RunResult::Failed)) {
            let why = run.message.lines().next().unwrap_or("").trim();
            if !why.is_empty() {
                out.push((format!("    {why}"), true));
            }
        }
        if job.kind != jobs::KIND_MIRROR {
            out.push(("    a restic repository: it is not browsable, so its size is not measured here".to_string(), false));
            continue;
        }
        match sizes.get(&job.id) {
            None => out.push(("    bucket size: not measured yet, press s".to_string(), false)),
            Some(size) => {
                let age = crate::ui::jobs::age_text(now, size.at);
                match (&size.problem, size.current) {
                    (Some(problem), _) => out.push((format!("    bucket size: {problem} ({age})"), true)),
                    (None, Some(current)) => {
                        let old = size.old.map_or_else(|| "no old/ yet".to_string(), |o| format!("old/ {}", folder_text(o)));
                        out.push((format!("    current/ {}   {old}   measured {age}", folder_text(current)), false));
                    }
                    (None, None) => out.push((format!("    bucket size: nothing measured ({age})"), false)),
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::history::{Event, EventKind, History};

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn known() -> Known {
        let mut k = Known::new();
        k.insert("local".into(), vec![("b2".into(), "b2".into()), ("t1".into(), "sftp".into()), ("zfsctl-aws".into(), "s3".into())]);
        k
    }

    fn mirror(id: &str, host: &str, remote: &str) -> Job {
        let mut j = Job::test_mirror(id, "tank/data", "hourly", remote);
        j.host = host.into();
        j
    }

    fn view_with(jobs: Vec<Job>, events: &[(&str, EventKind, &str, Option<RunResult>, &str)]) -> JobsView {
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(dir.path().join("history.jsonl"));
        for (i, (job_id, kind, when, result, message)) in events.iter().enumerate() {
            let mut e = Event {
                job_id: (*job_id).into(),
                run_id: format!("{job_id}-{i}"),
                event: *kind,
                at: at(when),
                scheduled_for: None,
                result: *result,
                message: (*message).into(),
                catch_up: false,
                summary: None,
            };
            if *kind == EventKind::Started {
                e.result = None;
            }
            history.append(&e).unwrap();
        }
        JobsView { jobs, runs: crate::daemon::history::runs(&history.read()), loaded: true, ..JobsView::default() }
    }

    #[test]
    fn a_job_names_its_remote_whether_it_mirrors_or_backs_up_through_rclone() {
        assert_eq!(remote_name(&mirror("m", "local", "b2:zfsctl/photos")), Some("b2".to_string()));
        let restic = Job::test_backup("b", "tank/data", "hourly", "rclone:zfsctl-aws:bkt/x", "/pw");
        assert_eq!(remote_name(&restic), Some("zfsctl-aws".to_string()));
        assert_eq!(remote_name(&Job::test_backup("b", "tank/data", "hourly", "/mnt/usb/repo", "/pw")), None, "a local repository is not a remote");
        assert_eq!(remote_name(&Job::test_backup("b", "tank/data", "hourly", "sftp:me@h:/r", "/pw")), None);
        assert_eq!(remote_name(&Job::test_snapshot("s", "tank/data", "hourly", 3)), None);
    }

    #[test]
    fn every_configured_remote_is_a_row_with_its_jobs_and_the_hubs_come_first() {
        let jobs = vec![mirror("m1", "local", "b2:zfsctl/photos"), mirror("m2", "local", "b2:zfsctl/docs"), mirror("m3", "fbsd16", "aws:bkt/p")];
        let r = rows(&jobs, &known());
        let names: Vec<(String, String, Option<String>, Vec<String>)> = r.into_iter().map(|r| (r.host, r.name, r.kind, r.job_ids)).collect();
        assert_eq!(
            names,
            [
                ("local".to_string(), "b2".to_string(), Some("b2".to_string()), vec!["m1".to_string(), "m2".to_string()]),
                ("local".to_string(), "t1".to_string(), Some("sftp".to_string()), vec![]),
                ("local".to_string(), "zfsctl-aws".to_string(), Some("s3".to_string()), vec![]),
                ("fbsd16".to_string(), "aws".to_string(), None, vec!["m3".to_string()]),
            ],
            "a remote only a job knows about (its host was not listed) still shows, with an unknown type"
        );
        assert!(rows(&[], &Known::new()).is_empty());
    }

    #[test]
    fn the_cells_show_the_jobs_the_last_result_and_the_measured_size() {
        let jobs = vec![mirror("m1", "local", "b2:zfsctl/photos"), mirror("m2", "local", "b2:zfsctl/docs")];
        let view = view_with(
            jobs.clone(),
            &[
                ("m1", EventKind::Started, "2026-10-17T03:30:00Z", None, ""),
                ("m1", EventKind::Finished, "2026-10-17T03:31:00Z", Some(RunResult::Ok), "mirrored x"),
                ("m2", EventKind::Started, "2026-10-17T03:32:00Z", None, ""),
                ("m2", EventKind::Finished, "2026-10-17T03:33:00Z", Some(RunResult::Ok), "mirrored y"),
            ],
        );
        let row = rows(&jobs, &known()).remove(0);
        let now = at("2026-10-17T05:33:00Z");
        assert_eq!(cells(&row, &view, &HashMap::new(), &Caps::new(), now), ["b2:", "local", "b2", "2", "ok 2h ago", "-"]);
        let mut sizes = HashMap::new();
        sizes.insert("m1".to_string(), Size { current: Some((3, 2_000_000)), old: None, at: now, problem: None });
        sizes.insert("m2".to_string(), Size { current: Some((1, 1_000_000)), old: Some((1, 5)), at: now, problem: None });
        assert_eq!(cells(&row, &view, &sizes, &Caps::new(), now)[5], "2.9 MiB");
        let idle = rows(&jobs, &known()).remove(1);
        assert_eq!(cells(&idle, &view, &sizes, &Caps::new(), now), ["t1:", "local", "sftp", "0", "-", "-"], "a remote nobody uses");
    }

    #[test]
    fn a_failed_latest_run_marks_the_row_and_its_reason_is_in_the_detail() {
        let jobs = vec![mirror("m1", "local", "b2:zfsctl/photos")];
        let view = view_with(
            jobs.clone(),
            &[("m1", EventKind::Started, "2026-10-17T03:30:00Z", None, ""), ("m1", EventKind::Finished, "2026-10-17T03:30:05Z", Some(RunResult::Failed), "the source holds no files to copy\nmore")],
        );
        let row = rows(&jobs, &known()).remove(0);
        assert!(failed(&row, &view));
        let lines = detail(&row, &view, &HashMap::new(), &Caps::new(), at("2026-10-17T04:00:00Z"));
        assert!(lines.iter().any(|(t, bad)| *bad && t.contains("last failed")), "{lines:?}");
        assert!(lines.iter().any(|(t, bad)| *bad && t.trim() == "the source holds no files to copy"), "only the first line of the reason: {lines:?}");
        let ok_view = view_with(jobs, &[]);
        assert!(!failed(&row, &ok_view));
    }

    #[test]
    fn the_detail_says_what_was_measured_or_why_not() {
        let jobs = vec![mirror("m1", "local", "b2:zfsctl/photos"), Job::test_backup("b1", "tank/data", "hourly", "rclone:b2:zfsctl/restic", "/pw")];
        let mut backup = jobs[1].clone();
        backup.host = "local".into();
        let jobs = vec![jobs[0].clone(), backup];
        let view = view_with(jobs.clone(), &[]);
        let row = rows(&jobs, &known()).remove(0);
        let now = at("2026-10-17T05:00:00Z");
        let text = |sizes: &HashMap<String, Size>| detail(&row, &view, sizes, &Caps::new(), now).into_iter().map(|(t, _)| t).collect::<Vec<_>>().join("\n");
        let before = text(&HashMap::new());
        assert!(before.contains("b2: (b2) on local") && before.contains("not measured yet, press s") && before.contains("a restic repository"), "{before}");
        let mut sizes = HashMap::new();
        sizes.insert("m1".to_string(), Size { current: Some((412, 1_992_294)), old: Some((3, 24_576)), at: at("2026-10-17T04:58:00Z"), problem: None });
        let after = text(&sizes);
        assert!(after.contains("current/ 1.9 MiB (412 files)") && after.contains("old/ 24.0 KiB (3 files)") && after.contains("measured 2m ago"), "{after}");
        sizes.insert("m1".to_string(), Size { current: Some((1, 10)), old: None, at: now, problem: None });
        assert!(text(&sizes).contains("no old/ yet") && text(&sizes).contains("(1 file)"));
        sizes.insert("m1".to_string(), Size { current: None, old: None, at: now, problem: Some("the keys were refused".into()) });
        let bad = detail(&row, &view, &sizes, &Caps::new(), now);
        assert!(bad.iter().any(|(t, is_bad)| *is_bad && t.contains("the keys were refused")), "{bad:?}");
    }

    /// Answers `rclone` calls from a script of `(argument fragment, status, stdout, stderr)`.
    struct Rclone(Vec<(&'static str, i32, &'static str, &'static str)>);

    impl CommandRunner for Rclone {
        fn run(&self, cmd: &str, args: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
            assert_eq!(cmd, "rclone", "only rclone is ever run");
            assert!(matches!(args.first().copied(), Some("size" | "listremotes")), "only read-only verbs: {args:?}");
            let line = args.join(" ");
            let (_, status, out, err) = self.0.iter().find(|(frag, ..)| line.contains(frag)).copied().unwrap_or(("", 1, "", "boom"));
            Ok(crate::exec::CommandOutput { stdout: out.to_string(), stderr: err.to_string(), status })
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _: &str) -> crate::error::Result<crate::exec::CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
            self.run(cmd, args)
        }
        fn command(&self, _: &str, _: &[&str]) -> crate::error::Result<std::process::Command> {
            panic!("nothing is streamed here")
        }
    }

    #[test]
    fn the_remotes_of_a_host_are_listed_or_the_reason_is_given_in_plain_words() {
        let ok = Rclone(vec![("listremotes", 0, "b2:  b2\nt1: sftp\n", "")]);
        assert_eq!(list_remotes(&ok, "local"), Ok(vec![("b2".to_string(), "b2".to_string()), ("t1".to_string(), "sftp".to_string())]));
        let no_sudo = Rclone(vec![("listremotes", 1, "", "sudo: a password is required")]);
        let err = list_remotes(&no_sudo, "fbsd16").unwrap_err();
        assert!(err.contains("print-sudoers fbsd16 --with rclone"), "{err}");
    }

    #[test]
    fn measuring_reads_current_and_old_and_treats_a_missing_old_as_none_yet() {
        let now = at("2026-10-17T05:00:00Z");
        let both = Rclone(vec![("b2:bkt/p/current", 0, "{\"count\":4,\"bytes\":2000,\"sizeless\":0}", ""), ("b2:bkt/p/old", 0, "{\"count\":2,\"bytes\":30,\"sizeless\":0}", "")]);
        assert_eq!(measure(&both, "local", "b2:bkt/p", now), Size { current: Some((4, 2000)), old: Some((2, 30)), at: now, problem: None });
        let no_old = Rclone(vec![("b2:bkt/p/current", 0, "{\"count\":4,\"bytes\":2000,\"sizeless\":0}", ""), ("b2:bkt/p/old", 3, "", "directory not found")]);
        assert_eq!(measure(&no_old, "local", "b2:bkt/p", now), Size { current: Some((4, 2000)), old: None, at: now, problem: None });
        let empty_old = Rclone(vec![("b2:bkt/p/current", 0, "{\"count\":4,\"bytes\":2000,\"sizeless\":0}", ""), ("b2:bkt/p/old", 0, "{\"count\":0,\"bytes\":0,\"sizeless\":0}", "")]);
        assert_eq!(measure(&empty_old, "local", "b2:bkt/p", now).old, None, "cloud storage answers an absent folder with zero files");
        let nothing = Rclone(vec![("b2:bkt/p/current", 3, "", "directory not found"), ("b2:bkt/p/old", 3, "", "directory not found")]);
        let size = measure(&nothing, "local", "b2:bkt/p", now);
        assert_eq!((size.current, size.problem), (None, None), "a first run has not created current/ yet");
    }

    #[test]
    fn a_refused_key_is_the_reason_not_a_crash_and_never_shows_the_remote() {
        let denied = Rclone(vec![("size", 1, "", "403 Forbidden: AccessDenied on b2:bkt/p/current")]);
        let size = measure(&denied, "local", "b2:bkt/p", at("2026-10-17T05:00:00Z"));
        let problem = size.problem.unwrap();
        assert!(problem.contains("refused the keys") && !problem.contains("bkt"), "{problem}");
        assert_eq!((size.current, size.old), (None, None));
    }

    fn caps(pairs: &[(&str, Result<u64, &str>)]) -> Caps {
        pairs.iter().map(|(n, c)| ((*n).to_string(), c.clone().map_err(str::to_string))).collect()
    }

    #[test]
    fn a_plan_is_counted_with_old_included_and_shown_as_a_percentage_of_its_cap() {
        let jobs = vec![mirror("m1", "local", "b2:zfsctl/photos"), mirror("m2", "local", "b2:zfsctl/docs")];
        let view = view_with(jobs.clone(), &[]);
        let row = rows(&jobs, &known()).remove(0);
        let now = at("2026-10-17T05:00:00Z");
        let mut sizes = HashMap::new();
        sizes.insert("m1".to_string(), Size { current: Some((3, 2_000_000)), old: Some((1, 1_000_000)), at: now, problem: None });
        sizes.insert("m2".to_string(), Size { current: Some((1, 1_000_000)), old: None, at: now, problem: None });
        assert_eq!(used_bytes(&row, &sizes), Some(4_000_000), "old/ counts: the plan charges for it");
        let ten_gb = caps(&[("b2", Ok(10_000_000_000))]);
        assert_eq!(in_bucket(&row, &sizes, &ten_gb), "3.8 MiB of 10 GB (0.04%)");
        assert_eq!(in_bucket(&row, &sizes, &Caps::new()), "3.8 MiB", "no cap, no percentage");
        assert_eq!(in_bucket(&row, &HashMap::new(), &ten_gb), "-", "nothing is shown before s");
        let lines = detail(&row, &view, &sizes, &ten_gb, now);
        assert!(lines.iter().any(|(t, bad)| !*bad && t.contains("Plan cap 10 GB: using 3.8 MiB (0.04%), 10 GB left") && t.contains("Counts only")), "{lines:?}");
        let before = detail(&row, &view, &HashMap::new(), &ten_gb, now);
        assert!(before.iter().any(|(t, _)| t.contains("Plan cap 10 GB: press s")), "{before:?}");
        assert_eq!(row_fill(&row, &sizes, &ten_gb), Some(Fill::Fine));
    }

    #[test]
    fn a_plan_that_is_nearly_full_is_a_warning_and_a_full_one_a_problem() {
        assert_eq!((fill(79, 100), fill(80, 100), fill(94, 100), fill(95, 100), fill(150, 100)), (Fill::Fine, Fill::Warn, Fill::Warn, Fill::Full, Fill::Full));
        let jobs = vec![mirror("m1", "local", "b2:zfsctl/photos")];
        let view = view_with(jobs.clone(), &[]);
        let row = rows(&jobs, &known()).remove(0);
        let now = at("2026-10-17T05:00:00Z");
        let mut sizes = HashMap::new();
        sizes.insert("m1".to_string(), Size { current: Some((1, 9_000)), old: None, at: now, problem: None });
        let c = caps(&[("b2", Ok(10_000))]);
        assert_eq!(row_fill(&row, &sizes, &c), Some(Fill::Warn));
        assert!(detail(&row, &view, &sizes, &c, now).iter().any(|(t, bad)| *bad && t.contains("90%") && t.contains("left")));
    }

    #[test]
    fn a_cap_that_is_not_a_size_is_said_so_and_never_guessed() {
        let jobs = vec![mirror("m1", "local", "b2:zfsctl/photos")];
        let view = view_with(jobs.clone(), &[]);
        let row = rows(&jobs, &known()).remove(0);
        let lines = detail(&row, &view, &HashMap::new(), &caps(&[("b2", Err("ten gigs"))]), at("2026-10-17T05:00:00Z"));
        assert!(lines.iter().any(|(t, bad)| *bad && t.contains("'ten gigs'") && t.contains("not a size")), "{lines:?}");
        assert_eq!(row_fill(&row, &HashMap::new(), &caps(&[("b2", Err("x"))])), None);
    }

    #[test]
    fn sizes_and_percentages_read_plainly() {
        assert_eq!((cap_text(10_000_000_000), cap_text(2_500_000_000), cap_text(500_000_000), cap_text(1_000_000_000_000), cap_text(900)), ("10 GB".to_string(), "2.5 GB".to_string(), "500 MB".to_string(), "1 TB".to_string(), "900 B".to_string()));
        assert_eq!((percent_text(2, 10_000), percent_text(340, 10_000), percent_text(8_700, 10_000), percent_text(0, 10_000)), ("0.02%".to_string(), "3.4%".to_string(), "87%".to_string(), "0.00%".to_string()));
    }

    #[test]
    fn a_remote_nobody_uses_invites_a_first_job() {
        let row = rows(&[], &known()).remove(1);
        let lines = detail(&row, &JobsView::default(), &HashMap::new(), &Caps::new(), at("2026-10-17T05:00:00Z"));
        assert!(lines.iter().any(|(t, _)| t.contains("No job uses this remote") && t.contains("press C") || t.contains("Press C")), "{lines:?}");
    }
}
