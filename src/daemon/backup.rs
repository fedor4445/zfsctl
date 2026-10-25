//! A scheduled restic backup: clone the newest `<prefix>-<timestamp>` snapshot to a fixed read-only
//! mountpoint (restic matches files against its parent backup by path, so the path must not change
//! between runs), run `restic backup` on it, destroy the clone on every exit path, then optionally
//! `restic forget --prune` this job's own backups. The source's snapshots are never touched.

use super::history::{EventKind, RunResult, UP_TO_DATE};
use super::progress::{self, ProgressWriter};
use super::replicate::Env;
use super::scrub::{event, finish, Outcome, Slot};
use super::snapshot::{is_own, snapshot_name};
use super::transfer::run_lines;
use crate::exec::CommandRunner;
use crate::error::AppError;
use crate::jobs::{backup_source, fresh_prefix, BackupSource, Job, DEFAULT_FRESH_KEEP};
use crate::restic::{self, Line, Summary};
use crate::zfs::replication::list_snapshot_infos;
use crate::zfs::snapshot::{create_snapshot_with, destroy_snapshot_with};
use chrono::{DateTime, Utc};
use std::fmt::Write;

/// Where a job's clone is mounted: the same path every run, so restic finds its parent backup.
pub const MOUNT_ROOT: &str = "/var/run/zfsctl";

/// `<pool>/<prefix>-<job id>`: the clone a job of one kind reads from (`zit-restic` for a backup, `zit-mirror`
/// for a cloud mirror, so the two kinds can never touch each other's clones).
pub(super) fn clone_name_for(prefix: &str, dataset: &str, job_id: &str) -> String {
    let pool = dataset.split('/').next().unwrap_or(dataset);
    format!("{pool}/{prefix}-{job_id}")
}

/// `<pool>/zit-restic-<job id>`.
pub fn clone_name(dataset: &str, job_id: &str) -> String {
    clone_name_for("zit-restic", dataset, job_id)
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

/// restic quotes the repository in many of its error messages; a run message never carries it.
fn redact(text: &str, repo: &str) -> String {
    restic::redact(text, repo)
}

fn restic_query(runner: &dyn CommandRunner, repo: &str, args: &[String]) -> std::result::Result<String, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match runner.run("restic", &refs) {
        Ok(out) if out.success() => Ok(out.stdout),
        Ok(out) => Err(redact(&restic::error_text(&out.stdout, &out.stderr), repo)),
        Err(e) => Err(redact(&e.to_string(), repo)),
    }
}

/// How `restic forget --prune` ended.
enum Forgot {
    Done,
    Cancelled,
    Failed(String),
}

/// Runs a retention command through the same cancellable line runner as the backup, so `c` can stop a long
/// prune. A killed prune is safe: restic leaves the repository consistent and a stale lock, which the next
/// run's unlock-and-retry removes.
fn run_retention(env: &Env, runner: &dyn CommandRunner, repo: &str, args: &[String]) -> Forgot {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cmd = match runner.command("restic", &refs) {
        Ok(c) => c,
        Err(e) => return Forgot::Failed(redact(&e.to_string(), repo)),
    };
    match run_lines(cmd, env.cancel, |_| {}) {
        Err(e) => Forgot::Failed(redact(&format!("could not run restic: {e}"), repo)),
        Ok(lines) if lines.cancelled => Forgot::Cancelled,
        Ok(lines) if lines.code == Some(0) => Forgot::Done,
        Ok(lines) => Forgot::Failed(redact(&restic::failure_text(None, &lines.stderr), repo)),
    }
}

/// restic's `--host` is the machine's real name, not zfsctl's label for it (usually `local`): two hubs
/// that share a repository must not mistake each other's backups for their own. The label is the
/// fallback when the host cannot say.
fn machine_name(runner: &dyn CommandRunner, label: &str) -> String {
    runner
        .run_unprivileged("hostname", &[])
        .ok()
        .filter(crate::exec::CommandOutput::success)
        .map(|o| o.stdout.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| label.to_string())
}

/// `zfs destroy -r` (a pool-wide recursive snapshot may have given the clone snapshots of its own; the
/// name is zfsctl's), retried a few times: after a cancel over SSH the remote restic can take a moment to
/// let go of the files. A clone that is already gone is fine.
pub(super) fn destroy_clone(runner: &dyn CommandRunner, clock: &dyn super::scrub::Clock, clone: &str) -> std::result::Result<(), String> {
    let mut last = String::new();
    for attempt in 0..5 {
        match zfs(runner, &["destroy", "-r", "--", clone]) {
            Ok(_) => return Ok(()),
            Err(e) if e.contains("does not exist") => return Ok(()),
            Err(e) => last = e,
        }
        if attempt < 4 {
            clock.sleep(std::time::Duration::from_secs(2));
        }
    }
    Err(last)
}

/// The snapshot's creation time as restic's `--time` wants it: in the host's own local time zone.
/// `None` when the host cannot say, and the backup then carries the run time.
fn snapshot_time(runner: &dyn CommandRunner, dataset: &str, snapshot: &str) -> Option<String> {
    let epoch = zfs(runner, &["get", "-H", "-p", "-o", "value", "creation", &format!("{dataset}@{snapshot}")]).ok()?;
    let epoch = epoch.trim();
    if epoch.is_empty() || !epoch.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let out = runner.run_unprivileged("date", &["-r", epoch, "+%Y-%m-%d %H:%M:%S"]).ok().filter(crate::exec::CommandOutput::success)?;
    let text = out.stdout.trim().to_string();
    (text.len() == 19).then_some(text)
}

/// Destroys a `new`-source job's own older snapshots so that, with the one about to be taken, `keep` remain.
/// Only exact `<prefix>-<timestamp>` names of this job are candidates, and a failure never stops the run: it
/// is reported in the run's message and tried again next time.
fn prune_fresh(runner: &dyn CommandRunner, dataset: &str, prefix: &str, keep: usize, note: &mut String) {
    let Ok(all) = list_snapshot_infos(runner, dataset) else { return };
    let mut own: Vec<String> = all.into_iter().map(|s| s.name).filter(|n| is_own(prefix, n)).collect();
    own.sort_unstable_by(|a, b| b.cmp(a));
    own.dedup();
    // Oldest first, so a failure part-way leaves the newest of the excess in place.
    for old in own.iter().skip(keep.saturating_sub(1)).rev() {
        match destroy_snapshot_with(runner, dataset, old, false) {
            Ok(()) => {}
            // A confirmed hold leaves it alone; the rest are still pruned (a check that could not run is reported below).
            Err(AppError::Held(why)) => {
                let _ = write!(note, "; kept the old snapshot {old} (held); {why}");
            }
            Err(e) => {
                let _ = write!(note, "; could not remove the old snapshot {old} ({e})");
            }
        }
    }
}

enum End {
    Done(Option<Summary>),
    Cancelled(u64),
    Failed(String),
}

#[allow(clippy::too_many_arguments)]
fn backup(env: &Env, runner: &dyn CommandRunner, job: &Job, machine: &str, repo: &str, password_file: &str, snapshot: &str, time: Option<&str>, mount: &str, run_id: &str) -> End {
    let args = restic::backup_args(repo, password_file, machine, &job.id, snapshot, time, mount);
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
            let text = restic::failure_text(fatal.as_deref(), &lines.stderr);
            let what = if code == Some(3) { "restic saved the backup but could not read some files (exit 3)".to_string() } else { format!("restic failed (exit {})", code.map_or_else(|| "signal".to_string(), |c| c.to_string())) };
            End::Failed(redact(&format!("{what}: {text}"), repo))
        }
    }
}

/// Why a run could not pick or prepare its source: a skip is quiet, a failure is an alert.
pub(super) enum SourceEnd {
    Skip(String),
    Fail(String),
}

/// Removes a clone an earlier, crashed run left behind: it would pin its snapshot and block the new clone.
pub(super) fn remove_stale_clone(runner: &dyn CommandRunner, clock: &dyn super::scrub::Clock, clone: &str) -> std::result::Result<(), String> {
    if zfs(runner, &["list", "-H", "-o", "name", "--", clone]).is_ok() {
        destroy_clone(runner, clock, clone).map_err(|e| format!("cannot remove the clone {clone} left by an earlier run: {e}"))?;
    }
    Ok(())
}

/// The snapshot a run reads: a fresh one (after pruning the job's own old ones), the pinned one, or the newest
/// `<prefix>-<timestamp>`. Returns its short name and the note about pruned fresh snapshots.
pub(super) fn choose_snapshot(runner: &dyn CommandRunner, clock: &dyn super::scrub::Clock, job: &Job, dataset: &str) -> std::result::Result<(String, String), SourceEnd> {
    let mut prune_note = String::new();
    let snapshot = match backup_source(job) {
        BackupSource::Fresh => {
            let own = fresh_prefix(&job.id);
            // Before taking the new one, so a run that fails later never leaves more than `keep` behind.
            let keep = job.keep.unwrap_or(DEFAULT_FRESH_KEEP).max(1) as usize;
            prune_fresh(runner, dataset, &own, keep, &mut prune_note);
            let name = snapshot_name(&own, clock.now());
            match create_snapshot_with(runner, dataset, &name, false) {
                Ok(()) => {}
                Err(AppError::CommandFailed { stderr, .. }) if stderr.contains("already exists") => return Err(SourceEnd::Skip(format!("skipped: {name} already exists"))),
                Err(e) => return Err(SourceEnd::Fail(format!("cannot snapshot {dataset}: {e}"))),
            }
            name
        }
        BackupSource::Pinned(name) => match list_snapshot_infos(runner, dataset) {
            Ok(all) if all.iter().any(|s| s.name == name) => name.to_string(),
            Ok(_) => return Err(SourceEnd::Fail(format!("the snapshot {dataset}@{name} no longer exists"))),
            Err(e) => return Err(SourceEnd::Fail(e.to_string())),
        },
        BackupSource::Prefix(prefix) => {
            let all = list_snapshot_infos(runner, dataset).map_err(|e| SourceEnd::Fail(e.to_string()))?;
            let Some(newest) = all.iter().filter(|s| is_own(prefix, &s.name)).max_by(|a, b| a.name.cmp(&b.name)) else {
                return Err(SourceEnd::Skip(format!("skipped: no snapshots with prefix '{prefix}' on {dataset}")));
            };
            newest.name.clone()
        }
    };
    Ok((snapshot, prune_note))
}

/// Clones `origin` (`dataset@snapshot`) read-only at `mount`.
pub(super) fn make_clone(runner: &dyn CommandRunner, origin: &str, clone: &str, mount: &str) -> std::result::Result<(), String> {
    zfs(runner, &["clone", "-o", &format!("mountpoint={mount}"), "-o", "readonly=on", "-o", "com.sun:auto-snapshot=false", "--", origin, clone])
        .map(|_| ())
        .map_err(|e| format!("cannot clone {origin}: {e}"))
}

/// What a run reads: the clone's root, or a directory inside it.
pub(super) fn target_dir(job: &Job, mount: &str) -> String {
    job.path.as_deref().map_or_else(|| mount.to_string(), |p| format!("{mount}/{}", p.trim_end_matches('/')))
}

/// Every part of the job's `path` must be a real folder inside the clone: restic runs as root, the kernel follows
/// a symlink in the middle of a path, and `docs -> /` with `path = docs/etc` would back up the host's own files.
/// `readlink` prints the target of a symlink and exits 1, silently, for anything else.
fn path_has_no_symlink(runner: &dyn CommandRunner, mount: &str, path: &str) -> std::result::Result<(), String> {
    let mut so_far = mount.to_string();
    for part in path.split('/').filter(|p| !p.is_empty() && *p != ".") {
        so_far = format!("{so_far}/{part}");
        match runner.run("readlink", &[&so_far]) {
            Ok(out) if out.success() => return Err(format!("the path '{path}' goes through a symlink ('{part}'); nothing was backed up")),
            Ok(out) if out.status == 1 && out.stderr.trim().is_empty() => {}
            Ok(out) => return Err(format!("cannot check the path '{path}' for symlinks: {} (a host set up before this check needs `zfsctl print-sudoers HOST --with restic` installed again, for readlink)", out.stderr.trim())),
            Err(e) => return Err(format!("cannot check the path '{path}' for symlinks: {e}")),
        }
    }
    Ok(())
}

pub fn run_backup(env: &Env, runner: &dyn CommandRunner, job: &Job, scheduled_for: DateTime<Utc>, catch_up: bool, run_id: &str) -> Outcome {
    let ctx = env.ctx;
    let dataset = job.dataset.clone().unwrap_or_default();
    let repo = job.repository.clone().unwrap_or_default();
    let password_file = job.password_file.clone().unwrap_or_default();
    let slot = Slot { scheduled_for: Some(scheduled_for), catch_up };
    let failed = |msg: String| finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, msg, slot);
    let skipped = |msg: String| finish(ctx, job, run_id, EventKind::Skipped, RunResult::Skipped, msg, slot);

    // `#` cannot appear in a pool name, so the key cannot collide with another kind's lock.
    let Some(_guard) = ctx.locks.try_acquire(&job.host, &format!("#bak@{}", job.id)) else { return skipped("skipped: already running".to_string()) };
    if let Err(msg) = super::guard::check_source(runner, &dataset, super::guard::Need::KeyLoaded) {
        return failed(msg);
    }
    let clone = clone_name(&dataset, &job.id);
    let mount = mountpoint(&job.id);

    if let Err(e) = remove_stale_clone(runner, ctx.clock, &clone) {
        return failed(e);
    }

    let (snapshot, prune_note) = match choose_snapshot(runner, ctx.clock, job, &dataset) {
        Ok(chosen) => chosen,
        Err(SourceEnd::Skip(msg)) => return skipped(msg),
        Err(SourceEnd::Fail(msg)) => return failed(msg),
    };
    let target = target_dir(job, &mount);

    let machine = machine_name(runner, &job.host);
    match restic_query(runner, &repo, &restic::snapshots_args(&repo, &password_file, &machine, &job.id, &snapshot)).and_then(|json| restic::snapshot_count(&json)) {
        Ok(0) => {}
        Ok(_) => return skipped(format!("{UP_TO_DATE} ({snapshot})")),
        Err(e) => return failed(e),
    }

    let time = snapshot_time(runner, &dataset, &snapshot);
    let origin = format!("{dataset}@{snapshot}");
    if let Err(e) = make_clone(runner, &origin, &clone, &mount) {
        return failed(e);
    }

    let mut started = event(job, run_id, EventKind::Started, ctx.clock.now());
    started.scheduled_for = Some(scheduled_for);
    started.catch_up = catch_up;
    let _ = ctx.history.append(&started);

    let checked = job.path.as_deref().map_or(Ok(()), |path| path_has_no_symlink(runner, &mount, path));
    let end = match checked {
        Ok(()) => backup(env, runner, job, &machine, &repo, &password_file, &snapshot, time.as_deref(), &target, run_id),
        Err(e) => End::Failed(e),
    };
    // Always, whatever happened: the clone pins the snapshot and holds a mountpoint.
    let leftover = match destroy_clone(runner, ctx.clock, &clone) {
        Ok(()) => String::new(),
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
            let message = format!("{message}{prune_note}");
            if !leftover.is_empty() {
                return failed(format!("{message}{leftover}"));
            }
            let policy = job.retention.as_deref().and_then(|t| restic::parse_retention(t).ok()).filter(|r| !r.is_empty());
            if let Some(policy) = policy {
                let forget_args = restic::forget_args(&repo, &password_file, &machine, &job.id, &policy);
                // A cancelled or killed run leaves a stale repository lock, and `forget --prune` would wait out its
                // whole `--retry-lock` for it. `restic unlock` removes only stale locks (never a live one), so clear
                // them first; a failure here is ignored and surfaces, if it matters, as the forget's own error.
                let _ = restic_query(runner, &repo, &restic::unlock_args(&repo, &password_file));
                let forgot = run_retention(env, runner, &repo, &forget_args);
                match forgot {
                    Forgot::Done => {}
                    Forgot::Cancelled => {
                        return finish(ctx, job, run_id, EventKind::Finished, RunResult::Cancelled, format!("{message}; retention cancelled, it is applied after the next successful backup"), slot);
                    }
                    Forgot::Failed(e) => return failed(format!("backed up {snapshot} but forget failed: {e}")),
                }
            }
            // The repository is checked now and then, after a backup that has just worked: a backup nobody can restore from is
            // the one failure that stays silent until the day it is needed.
            let days = crate::jobs::check_days_of(job);
            let state_path = env.check_state();
            if super::restic_check::due(super::restic_check::read_state(&state_path).get(&job.id), days, ctx.clock.now()) {
                let args = restic::check_args(&repo, &password_file);
                let result = run_retention(env, runner, &repo, &args);
                let record = |ok: bool| {
                    let _ = super::restic_check::record(&state_path, &job.id, super::restic_check::Checked { at: ctx.clock.now(), ok });
                };
                match result {
                    Forgot::Done => {
                        record(true);
                        return finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, format!("{message}; restic check: the repository is sound"), slot);
                    }
                    Forgot::Cancelled => return finish(ctx, job, run_id, EventKind::Finished, RunResult::Cancelled, format!("{message}; the repository check was cancelled, it runs after the next successful backup"), slot),
                    Forgot::Failed(e) => {
                        record(false);
                        return failed(format!("{message}; restic check found a problem: {e}. The backup itself ran; check the repository before you rely on it"));
                    }
                }
            }
            finish(ctx, job, run_id, EventKind::Finished, RunResult::Ok, message, slot)
        }
    }
}

#[cfg(test)]
mod tests;
