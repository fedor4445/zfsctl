//! `zfsctl daemon`: runs scheduled jobs on the hub, unattended.

pub mod checks;
pub mod cron;
pub mod engine;
pub mod health;
pub mod health_text;
pub mod heartbeat;
pub mod history;
pub mod monitor;
pub mod notify;
pub mod scheduler;
pub mod cancel;
pub mod deliver;
pub mod progress;
pub mod replicate;
pub mod replicate_file;
pub mod backup;
pub mod mirror;
pub mod protection;
pub mod scrub;
pub mod guard;
pub mod shares_check;
pub mod storage_check;
pub mod smart_check;
pub mod snapshot;
pub mod transfer;

use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use crate::config::Config;
use crate::hosts::HostInventory;
use chrono::Utc;
use engine::{Engine, Paths, Work};
use history::{Event, EventKind, History, RunResult};
use scrub::{Clock, Context, Locks, Outcome, SystemClock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::path::Path;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

/// How often a running scrub is polled for completion.
const POLL: Duration = Duration::from_secs(30);

/// A log timestamp: second precision with the zone offset, e.g. `2026-10-06T11:43:16-04:00`.
fn stamp<Tz: chrono::TimeZone>(t: &chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Logs in the hub's local time, the zone its schedules are written in. (The
/// history file stays in UTC: it is machine data.)
fn log(message: &str) {
    eprintln!("{} {message}", stamp(&chrono::Local::now()));
}

/// What every job thread shares.
struct Shared {
    history: History,
    /// Replaced when `config.toml` changes, so jobs see hosts added after the daemon started.
    hosts: RwLock<Vec<(String, Arc<dyn CommandRunner>)>>,
    locks: Locks,
    clock: Arc<dyn Clock + Send + Sync>,
    poll: Duration,
    notify_command: RwLock<Option<String>>,
    /// The `[notify]` section: email, the monitor's interval, the daily report.
    settings: RwLock<crate::config::NotifySettings>,
    /// Name, address and tag of each host, for the monitor's messages.
    host_info: RwLock<Vec<(String, String, String)>>,
    stop: Arc<AtomicBool>,
    /// Where running transfers write their progress files.
    progress_dir: PathBuf,
    /// What each file-target replication last wrote.
    replication_state: PathBuf,
    /// The cancel flag of each running replication, by its cancel-file name.
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

/// The parts of `config.toml` the daemon uses.
struct Loaded {
    hosts: Vec<(String, Arc<dyn CommandRunner>)>,
    notify_command: Option<String>,
    /// `None` when `[notify]` is invalid: the hosts and the hook still apply, and the caller keeps its previous
    /// settings (or none, at startup) and logs `notify_problem`.
    notify: Option<crate::config::NotifySettings>,
    notify_problem: Option<String>,
    info: Vec<(String, String, String)>,
}

/// Reads the hosts (always including `local`) and the notify command from `config.toml`.
fn load_hosts(config_path: &Path) -> Result<Loaded> {
    let config = Config::load(config_path)?;
    let inventory = HostInventory::from_config(&config);
    let hosts = inventory.hosts.iter().map(|h| (h.name.clone(), Arc::clone(&h.runner))).collect();
    let info = inventory.hosts.iter().map(|h| (h.name.clone(), crate::ui::state::address_of(h.ssh_target.as_deref()), h.tag.clone().unwrap_or_default())).collect();
    let (notify, notify_problem) = match config.notify.resolve() {
        Ok(settings) => (Some(settings), None),
        Err(problem) => (None, Some(problem)),
    };
    Ok(Loaded { hosts, notify_command: config.daemon.notify_command, notify, notify_problem, info })
}

/// Progress and cancel files can only belong to a transfer of a previous daemon: remove them.
fn clear_runtime_files(paths: &Paths) {
    progress::clear_all(&paths.progress_dir());
    cancel::clear_all(&paths.cancel_dir());
}

/// A `.partial` stream file can only belong to a write of a previous daemon. The daemon knows the
/// directories from `jobs.toml`; nothing but `*.zfs.partial` is touched.
fn clear_partial_files(jobs: &[crate::jobs::Job]) {
    for job in jobs.iter().filter(|j| j.kind == crate::jobs::KIND_REPLICATE && j.dest_host.as_deref() == Some(crate::jobs::DEST_FILE)) {
        let Some(dir) = job.dest_dir.as_deref() else { continue };
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.filter_map(std::result::Result::ok).filter(|e| e.file_name().to_string_lossy().ends_with(".zfs.partial")) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl Shared {
    fn runner(&self, host: &str) -> Option<Arc<dyn CommandRunner>> {
        self.hosts.read().unwrap_or_else(PoisonError::into_inner).iter().find(|(name, _)| name == host).map(|(_, r)| Arc::clone(r))
    }

    /// Swaps in a freshly loaded config; returns the host names now known.
    fn apply(&self, loaded: Loaded) -> Vec<String> {
        let names = loaded.hosts.iter().map(|(n, _)| n.clone()).collect();
        *self.hosts.write().unwrap_or_else(PoisonError::into_inner) = loaded.hosts;
        *self.notify_command.write().unwrap_or_else(PoisonError::into_inner) = loaded.notify_command;
        if let Some(settings) = loaded.notify {
            *self.settings.write().unwrap_or_else(PoisonError::into_inner) = settings;
        }
        *self.host_info.write().unwrap_or_else(PoisonError::into_inner) = loaded.info;
        names
    }

    /// Tells the operator about a failed run: the notify hook, and email when it is configured. A failing
    /// channel is only logged and never disturbs the run or the other channel.
    fn notify(&self, job: &crate::jobs::Job, outcome: &Outcome) {
        if outcome.result != RunResult::Failed {
            return;
        }
        if let Some(command) = self.notify_command.read().unwrap_or_else(PoisonError::into_inner).clone() {
            let notice = notify::Notice { job: job.id.clone(), host: job.host.clone(), kind: job.kind.clone(), result: "failed".to_string(), message: outcome.message.clone() };
            match notify::run(&command, &notice, notify::TIMEOUT) {
                Ok(()) => log(&format!("notified about {}", job.id)),
                Err(e) => log(&format!("notify command for {} {e}", job.id)),
            }
        }
        let settings = self.settings.read().unwrap_or_else(PoisonError::into_inner).clone();
        if settings.has_mail() {
            let msg = deliver::Message {
                kind: job.kind.clone(),
                host: job.host.clone(),
                level: "failed".to_string(),
                subject: format!("[zfsctl] job {} failed on {}", deliver::ascii_line(&job.id), deliver::ascii_line(&job.host)),
                body: format!("Job {} ({}) on {} failed.\n\n{}\n", job.id, job.kind, job.host, outcome.message),
            };
            for d in deliver::send(&settings, None, &msg, Utc::now(), deliver::TIMEOUT) {
                match d.result {
                    Ok(()) => log(&format!("mailed about {}", job.id)),
                    Err(e) => log(&format!("mail for {} {e}", job.id)),
                }
            }
        }
    }

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

    fn mirror(&self, ctx: &Context, due: &scheduler::Due, run_id: &str) -> Outcome {
        let job = &due.job;
        let Some(runner) = self.runner(&job.host) else {
            let slot = scrub::Slot { scheduled_for: Some(due.scheduled_for), catch_up: due.catch_up };
            return scrub::finish(ctx, job, run_id, EventKind::Finished, RunResult::Failed, format!("host '{}' is not configured", job.host), slot);
        };
        self.run_cancellable(ctx, due, run_id, |env| mirror::run_mirror(env, runner.as_ref(), job, due.scheduled_for, due.catch_up, run_id))
    }

    /// Hands each pending cancel request to the run it names; a request for a job that is not
    /// running has nothing to cancel and is dropped.
    fn apply_cancel_requests(&self, dir: &Path) {
        let running = self.cancels.lock().unwrap_or_else(PoisonError::into_inner);
        for name in cancel::take_requests(dir) {
            if let Some(flag) = running.get(&name) {
                log(&format!("cancelling {name}"));
                flag.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Runs one piece of work to completion on the calling thread.
    fn execute(&self, work: Work) {
        let ctx = Context { history: &self.history, clock: self.clock.as_ref(), locks: &self.locks, poll: self.poll, stop: &self.stop };
        match work {
            Work::Run(due) => {
                let run_id = format!("{}-{}", due.job.id, self.clock.now().timestamp_millis());
                log(&format!("running {} (slot {}{})", due.job.id, due.scheduled_for.to_rfc3339(), if due.catch_up { ", catch-up" } else { "" }));
                let outcome = match due.job.kind.as_str() {
                    crate::jobs::KIND_REPLICATE => Some(self.replicate(&ctx, &due, &run_id)),
                    crate::jobs::KIND_BACKUP => Some(self.backup(&ctx, &due, &run_id)),
                    crate::jobs::KIND_MIRROR => Some(self.mirror(&ctx, &due, &run_id)),
                    kind => {
                        let Some(runner) = self.runner(&due.job.host) else { return };
                        if kind == crate::jobs::KIND_SNAPSHOT {
                            Some(snapshot::run_snapshot(&ctx, runner.as_ref(), &due.job, due.scheduled_for, due.catch_up, &run_id))
                        } else {
                            scrub::run_scrub(&ctx, runner.as_ref(), &due.job, due.scheduled_for, due.catch_up, &run_id)
                        }
                    }
                };
                if let Some(outcome) = outcome {
                    log(&format!("{} {:?}: {}", outcome.job_id, outcome.result, outcome.message));
                    self.notify(&due.job, &outcome);
                }
            }
            Work::Resume { job, run, then } => {
                let Some(runner) = self.runner(&job.host) else { return };
                log(&format!("re-attaching to {} (run {})", job.id, run.run_id));
                if let Some(outcome) = scrub::resume_scrub(&ctx, runner.as_ref(), &job, &run) {
                    log(&format!("{} {:?}: {}", outcome.job_id, outcome.result, outcome.message));
                    self.notify(&job, &outcome);
                    // The catch-up waits for the resumed run to end, so the two never race for the pool.
                    if let Some(due) = then {
                        self.execute(Work::Run(*due));
                    }
                }
            }
            Work::Orphan(run) => {
                log(&format!("run {} of {} was unfinished when the daemon stopped, or its job is gone", run.run_id, run.job_id));
                let _ = self.history.append(&Event {
                    job_id: run.job_id,
                    run_id: run.run_id,
                    event: EventKind::Finished,
                    at: self.clock.now(),
                    scheduled_for: None,
                    result: Some(RunResult::Failed),
                    message: "lost: the daemon stopped (or the job was removed or can no longer run) while it was running".to_string(),
                    catch_up: false,
                    summary: None,
                });
            }
        }
    }
}

/// The monitor runs only when something can receive its messages.
fn monitor_enabled(settings: &crate::config::NotifySettings, hook: Option<&str>) -> bool {
    settings.has_mail() || hook.is_some()
}

/// Delivers the monitor's messages through the configured channels, logging each failure (never the text).
struct DaemonSink<'a>(&'a Shared);

impl monitor::Sink for DaemonSink<'_> {
    fn send(&mut self, msg: &deliver::Message) -> bool {
        let settings = self.0.settings.read().unwrap_or_else(PoisonError::into_inner).clone();
        let hook = self.0.notify_command.read().unwrap_or_else(PoisonError::into_inner).clone();
        let results = deliver::send(&settings, hook.as_deref(), msg, Utc::now(), deliver::TIMEOUT);
        for d in &results {
            if let Err(e) = &d.result {
                log(&format!("{:?} delivery of '{}' {e}", d.channel, msg.subject));
            }
        }
        deliver::delivered(&results)
    }
}

/// Runs one unit of background work; a panic is logged and the caller carries on at its next turn instead of the
/// thread dying silently. Returns whether it completed.
fn guarded(label: &str, work: impl FnOnce()) -> bool {
    let completed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_ok();
    if !completed {
        log(&format!("{label} crashed; carrying on at the next turn"));
    }
    completed
}

/// Probes the hosts every `check_interval` and hands the results to the `Monitor`, until `stop`.
fn monitor_loop(shared: &Arc<Shared>, state_dir: &Path, stop: &Arc<AtomicBool>) {
    let mut monitor: Option<monitor::Monitor> = None;
    let mut next = std::time::Instant::now();
    let busy = monitor::Busy::default();
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(1));
        if std::time::Instant::now() < next {
            continue;
        }
        let settings = shared.settings.read().unwrap_or_else(PoisonError::into_inner).clone();
        let hook = shared.notify_command.read().unwrap_or_else(PoisonError::into_inner).clone();
        if !monitor_enabled(&settings, hook.as_deref()) {
            monitor = None;
            next = std::time::Instant::now() + Duration::from_secs(30);
            continue;
        }
        if let Some(m) = monitor.as_mut().filter(|m| *m.settings() != settings) {
            // New settings for the running monitor: its alert state and retry queue carry over.
            log("health monitor settings changed");
            m.set_settings(settings.clone());
        }
        if monitor.is_none() {
            let hub = deliver::default_from().trim_start_matches("zfsctl@").to_string();
            log(&format!("health monitor started (every {} min, report at {})", settings.check_interval.as_secs() / 60, settings.report_time.map_or("off".to_string(), |t| t.format("%H:%M").to_string())));
            monitor = Some(monitor::Monitor::new(settings.clone(), hub, state_dir.to_path_buf(), Utc::now()));
        }
        let Some(m) = monitor.as_mut() else { continue };
        guarded("the health monitor tick", || {
            let info = shared.host_info.read().unwrap_or_else(PoisonError::into_inner).clone();
            let runners = shared.hosts.read().unwrap_or_else(PoisonError::into_inner).clone();
            let hosts: Vec<monitor::HostInfo> = runners
                .into_iter()
                .map(|(name, runner)| {
                    let (address, tag) = info.iter().find(|(n, _, _)| *n == name).map_or((String::new(), String::new()), |(_, a, t)| (a.clone(), t.clone()));
                    monitor::HostInfo { name, address, tag, runner }
                })
                .collect();
            let probed = monitor::probe_all(&hosts, &|n| m.needs_kind(n), monitor::PROBE_DEADLINE, &busy);
            let jobs = crate::jobs::load(&state_dir.join("jobs.toml")).unwrap_or_default();
            let runs = history::runs(&shared.history.read());
            let local = chrono::Local::now().naive_local();
            m.observe(Utc::now(), local, &hosts, &probed, &jobs, &runs, &mut DaemonSink(shared));
        });
        // Wake for the daily report too, so it is not lost when the probe interval straddles the report time.
        let wake = match m.report_wait(chrono::Local::now().naive_local()) {
            Some(wait) if !wait.is_zero() => settings.check_interval.min(wait + Duration::from_secs(1)),
            _ => settings.check_interval,
        };
        next = std::time::Instant::now() + wake;
    }
}

/// Runs the daily checks (disks with `smartctl`, file sharing services) on every host once a day, at `at` (the hub's
/// local time), and also at start when the last run is missing or over a day old. Nothing here depends on the health
/// monitor being on. While it runs a marker file says so (the Dashboard shows a red notice); it does not start while a
/// check of one host is running from the TUI, and tries again at its next wake.
fn checks_loop(shared: &Arc<Shared>, state_dir: &Path, stop: &Arc<AtomicBool>, at: Option<chrono::NaiveTime>) {
    let Some(at) = at else {
        log("daily checks are off");
        return;
    };
    log(&format!("daily checks (disks, shares) at {}", at.format("%H:%M")));
    let path = state_dir.join("smart.json");
    while !stop.load(Ordering::SeqCst) {
        guarded("the daily checks", || {
            let last = smart_check::SmartFile::load(&path).last_run().map(|t| t.with_timezone(&chrono::Local).naive_local());
            if !smart_check::due(chrono::Local::now().naive_local(), Some(at), last) {
                return;
            }
            let Some(_running) = checks::RunGuard::start(state_dir, "daemon", None) else {
                log("daily checks wait: another check is running");
                return;
            };
            let hosts = shared.hosts.read().unwrap_or_else(PoisonError::into_inner).clone();
            let summary = checks::run_pass(state_dir, &hosts, Utc::now(), smart_check::HOST_DEADLINE);
            log(&format!(
                "daily checks done: disks on {} of {} hosts, shares on {} of {} hosts, {} problem(s)",
                summary.disks_hosts, summary.hosts, summary.shares_hosts, summary.hosts, summary.problems
            ));
        });
        for _ in 0..300 {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

/// How often the storage check runs: a quota line must clear soon after the fix, and one `zfs list` per host is cheap.
const STORAGE_EVERY: Duration = Duration::from_secs(600);

/// The storage check (datasets near their quota): once at start, then every ten minutes, into `storage.json`.
fn storage_loop(shared: &Arc<Shared>, state_dir: &Path, stop: &Arc<AtomicBool>) {
    log("storage check (quotas) every 10 minutes");
    while !stop.load(Ordering::SeqCst) {
        guarded("the storage check", || {
            let hosts = shared.hosts.read().unwrap_or_else(PoisonError::into_inner).clone();
            storage_check::run_pass(state_dir, &hosts, Utc::now(), storage_check::HOST_DEADLINE);
            storage_check::retain_hosts(state_dir, &hosts.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>());
        });
        let mut waited = Duration::ZERO;
        while waited < STORAGE_EVERY {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
            waited += Duration::from_secs(1);
        }
    }
}

fn dispatch(shared: &Arc<Shared>, work: Vec<Work>) {
    for item in work {
        let shared = Arc::clone(shared);
        if let Err(e) = std::thread::Builder::new().name("job".to_string()).spawn(move || shared.execute(item)) {
            log(&format!("could not start a job thread: {e}"));
        }
    }
}

/// Runs the scheduler until `stop` is set. Refuses to start if another daemon is alive.
pub fn run(config_path: &Path, paths: Paths, stop: &Arc<AtomicBool>) -> Result<()> {
    paths.ensure_dir().map_err(|e| AppError::Config(format!("creating {}: {e}", paths.dir.display())))?;
    let now = Utc::now();
    let heartbeat_path = paths.heartbeat();
    if let Some(pid) = heartbeat::live_other_instance(heartbeat::read(&heartbeat_path).as_ref(), now, std::process::id(), heartbeat::process_alive) {
        return Err(AppError::Config(format!("another zfsctl daemon is already running (pid {pid})")));
    }
    clear_runtime_files(&paths);
    if let Ok(jobs) = crate::jobs::load(&paths.jobs()) {
        clear_partial_files(&jobs);
    }
    let loaded = load_hosts(config_path)?;
    if let Some(problem) = &loaded.notify_problem {
        // Never a reason to stay down: the jobs matter more than the notifications.
        log(&format!("config.toml [notify] problem, health notifications are off until it is fixed: {problem}"));
    }
    let names: Vec<String> = loaded.hosts.iter().map(|(n, _)| n.clone()).collect();
    let shared = Arc::new(Shared {
        history: History::new(paths.history()),
        hosts: RwLock::new(loaded.hosts),
        locks: Locks::default(),
        clock: Arc::new(SystemClock),
        poll: POLL,
        notify_command: RwLock::new(loaded.notify_command),
        settings: RwLock::new(loaded.notify.clone().unwrap_or_default()),
        host_info: RwLock::new(loaded.info),
        stop: Arc::clone(stop),
        progress_dir: paths.progress_dir(),
        replication_state: paths.replication_state(),
        cancels: Mutex::default(),
    });
    let cancel_dir = paths.cancel_dir();
    let state_dir = paths.dir.clone();
    let mut config_mtime = modified(config_path);
    let mut engine = Engine::new(paths, chrono::Local, names, now);
    let started_at = now;

    let work = engine.start(Utc::now());
    log(&format!("zfsctl daemon started (pid {})", std::process::id()));
    report(&engine);
    dispatch(&shared, work);
    {
        let (shared, state_dir, stop) = (Arc::clone(&shared), state_dir.clone(), Arc::clone(stop));
        if let Err(e) = std::thread::Builder::new().name("health".to_string()).spawn(move || monitor_loop(&shared, &state_dir, &stop)) {
            log(&format!("could not start the health monitor: {e}"));
        }
    }

    {
        let (shared, state_dir, stop) = (Arc::clone(&shared), state_dir.clone(), Arc::clone(stop));
        if let Err(e) = std::thread::Builder::new().name("storage".to_string()).spawn(move || storage_loop(&shared, &state_dir, &stop)) {
            log(&format!("could not start the storage check: {e}"));
        }
    }

    match Config::load(config_path).map_err(|e| e.to_string()).and_then(|c| c.daemon.daily_time()) {
        Ok(at) => {
            let (shared, state_dir, stop) = (Arc::clone(&shared), state_dir.clone(), Arc::clone(stop));
            if let Err(e) = std::thread::Builder::new().name("checks".to_string()).spawn(move || checks_loop(&shared, &state_dir, &stop, at)) {
                log(&format!("could not start the daily checks: {e}"));
            }
        }
        Err(problem) => log(&format!("config.toml [daemon] problem, the daily checks are off: {problem}")),
    }

    let mut last_problem: Option<String> = None;
    while !stop.load(Ordering::SeqCst) {
        let now = Utc::now();
        if let Err(e) = heartbeat::write(&heartbeat_path, &heartbeat::Heartbeat { pid: std::process::id(), started_at, last_tick: now }) {
            let problem = format!("cannot write the heartbeat: {e}");
            if last_problem.as_deref() != Some(problem.as_str()) {
                log(&problem);
            }
            last_problem = Some(problem);
        }
        let current = modified(config_path);
        if current != config_mtime {
            config_mtime = current;
            match load_hosts(config_path) {
                Ok(loaded) => {
                    if let Some(problem) = &loaded.notify_problem {
                        log(&format!("config.toml [notify] problem, keeping the previous notify settings: {problem}"));
                    }
                    let names = shared.apply(loaded);
                    log(&format!("config.toml reloaded: hosts {}", names.join(", ")));
                    engine.set_hosts(names);
                }
                Err(e) => log(&format!("config.toml problem, keeping the previous hosts and notify settings: {e}")),
            }
        }
        shared.apply_cancel_requests(&cancel_dir);
        let before = engine.load_error.clone();
        let work = engine.step(now);
        if engine.load_error != before {
            report(&engine);
        }
        dispatch(&shared, work);
        std::thread::sleep(Duration::from_secs(1));
    }
    let _ = std::fs::remove_file(&heartbeat_path);
    log("zfsctl daemon stopping");
    Ok(())
}

fn report(engine: &Engine<chrono::Local>) {
    if let Some(e) = &engine.load_error {
        log(&format!("jobs file problem, keeping the previous schedule: {e}"));
    }
    for u in &engine.unsupported {
        log(&format!("job {} will not run: {}", u.id, u.reason));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;
    use crate::jobs::{Job, KIND_SCRUB};
    use scheduler::Due;
    use std::sync::Mutex;

    struct FixedClock(Mutex<chrono::DateTime<Utc>>);

    impl Clock for FixedClock {
        fn now(&self) -> chrono::DateTime<Utc> {
            *self.0.lock().unwrap()
        }
        fn sleep(&self, d: Duration) {
            *self.0.lock().unwrap() += chrono::Duration::from_std(d).unwrap();
        }
    }

    fn job() -> Job {
        Job { id: "scrub-tank".into(), host: "local".into(), kind: KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * *".into(), catch_up: true, enabled: true, created_at: None, dataset: None, recursive: false, prefix: None, keep: None, dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, remote: None, bwlimit: None, force: false, raw: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None }
    }

    fn shared(dir: &tempfile::TempDir, runner: FakeRunner, notify: Option<String>) -> Shared {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-07T03:00:00Z").unwrap().with_timezone(&Utc);
        Shared {
            history: History::new(dir.path().join("history.jsonl")),
            hosts: RwLock::new(vec![("local".to_string(), Arc::new(runner) as Arc<dyn CommandRunner>)]),
            locks: Locks::default(),
            clock: Arc::new(FixedClock(Mutex::new(now))),
            poll: Duration::from_secs(30),
            notify_command: RwLock::new(notify),
            settings: RwLock::new(crate::config::NotifySettings::default()),
            host_info: RwLock::new(Vec::new()),
            stop: Arc::new(AtomicBool::new(false)),
            progress_dir: dir.path().join("progress"),
            replication_state: dir.path().join("replication.json"),
            cancels: Mutex::default(),
        }
    }

    fn shared_with_mail(dir: &tempfile::TempDir, runner: FakeRunner, mail_command: &str) -> Shared {
        let shared = shared(dir, runner, None);
        *shared.settings.write().unwrap() = crate::config::NotifyConfig { mail_to: Some("me@x.org".into()), mail_command: Some(mail_command.into()), ..Default::default() }.resolve().unwrap();
        shared
    }

    #[test]
    fn a_failed_job_is_also_mailed_with_the_job_and_message() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("mail");
        let shared = shared_with_mail(&dir, FakeRunner::failing("cannot open 'tank': no such pool", 1), &format!("cat > {}", out.display()));
        shared.execute(Work::Run(due()));
        let got = std::fs::read_to_string(out).unwrap();
        assert!(got.contains("Subject: [zfsctl] job scrub-tank failed on local") && got.contains("no such pool"), "{got}");
    }

    #[test]
    fn a_failing_mail_command_does_not_disturb_the_recorded_run_or_the_hook() {
        let dir = tempfile::tempdir().unwrap();
        let hook_out = dir.path().join("hook");
        let shared = shared_with_mail(&dir, FakeRunner::failing("no such pool", 1), "exit 7");
        *shared.notify_command.write().unwrap() = Some(format!("printf '%s' \"$ZFSCTL_JOB\" > {}", hook_out.display()));
        shared.execute(Work::Run(due()));
        assert_eq!(history::runs(&shared.history.read())[0].result, Some(RunResult::Failed));
        assert_eq!(std::fs::read_to_string(hook_out).unwrap(), "scrub-tank");
    }

    #[test]
    fn load_hosts_reads_the_notify_section_and_the_host_addresses_and_tags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[[hosts]]\nname = \"fbsd16\"\nssh_target = \"fbsd@172.16.10.60\"\ntag = \"vm\"\n\n[notify]\nmail_to = \"me@x.org\"\n").unwrap();
        let l = load_hosts(&path).unwrap();
        assert_eq!(l.notify.unwrap().mail_to, ["me@x.org"]);
        assert_eq!(l.info, [("local".to_string(), "local".to_string(), String::new()), ("fbsd16".to_string(), "172.16.10.60".to_string(), "vm".to_string())]);
    }

    #[test]
    fn a_bad_notify_section_never_blocks_the_hosts_and_never_replaces_good_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[[hosts]]\nname = \"later\"\nssh_target = \"u@later\"\n\n[notify]\nmail_to = \"not an address\"\n").unwrap();
        let l = load_hosts(&path).expect("a bad [notify] is not a load failure");
        assert!(l.notify.is_none() && l.notify_problem.as_deref().unwrap().contains("mail_to"));
        assert_eq!(l.hosts.len(), 2, "the host in the same edit still loads");
        let shared = shared(&dir, FakeRunner::new(""), None);
        *shared.settings.write().unwrap() = crate::config::NotifyConfig { mail_to: Some("me@x.org".into()), ..Default::default() }.resolve().unwrap();
        shared.apply(l);
        assert_eq!(shared.settings.read().unwrap().mail_to, ["me@x.org"], "the previous settings are kept");
        assert!(shared.runner("later").is_some());
    }

    #[test]
    fn the_monitor_runs_only_when_a_channel_is_configured() {
        assert!(!monitor_enabled(&crate::config::NotifySettings::default(), None));
        assert!(monitor_enabled(&crate::config::NotifySettings::default(), Some("true")));
        let mail = crate::config::NotifyConfig { mail_to: Some("me@x.org".into()), ..Default::default() }.resolve().unwrap();
        assert!(monitor_enabled(&mail, None));
    }

    #[test]
    fn a_panic_in_background_work_is_caught_and_reported_not_fatal() {
        assert!(guarded("fine", || {}));
        assert!(!guarded("boom", || panic!("on purpose")));
        let mut counter = 0;
        assert!(guarded("again", || counter += 1) && counter == 1, "work after a caught panic runs normally");
    }

    #[test]
    fn the_monitor_loop_returns_at_once_when_stopped_and_does_nothing_without_a_channel() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(shared(&dir, FakeRunner::new(""), None));
        let stop = Arc::new(AtomicBool::new(true));
        let t = std::time::Instant::now();
        monitor_loop(&shared, dir.path(), &stop);
        assert!(t.elapsed() < Duration::from_secs(2));
        assert!(!dir.path().join("health-events.jsonl").exists(), "no channel: no monitor, no event log");
    }

    #[test]
    fn applying_a_new_config_replaces_the_notify_settings_and_host_info() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[notify]\nmail_to = \"me@x.org\"\ncheck_interval_minutes = 2\n").unwrap();
        shared.apply(load_hosts(&path).unwrap());
        assert_eq!(shared.settings.read().unwrap().check_interval, Duration::from_secs(120));
        assert_eq!(shared.host_info.read().unwrap().len(), 1);
    }

    fn due() -> Due {
        Due { job: job(), scheduled_for: chrono::DateTime::parse_from_rfc3339("2026-10-07T03:00:00Z").unwrap().with_timezone(&Utc), catch_up: false }
    }

    #[test]
    fn a_failed_run_is_recorded_and_runs_the_notify_command_with_the_job_in_its_environment() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("notified");
        let cmd = format!("printf '%s|%s' \"$ZFSCTL_JOB\" \"$ZFSCTL_RESULT\" > {}", out.display());
        let shared = shared(&dir, FakeRunner::failing("cannot open 'tank': no such pool", 1), Some(cmd));
        shared.execute(Work::Run(due()));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Failed)));
        assert!(runs[0].message.contains("no such pool"));
        assert_eq!(std::fs::read_to_string(out).unwrap(), "scrub-tank|failed");
    }

    #[test]
    fn a_failing_notify_command_does_not_disturb_the_recorded_run() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::failing("no such pool", 1), Some("exit 9".to_string()));
        shared.execute(Work::Run(due()));
        assert_eq!(history::runs(&shared.history.read())[0].result, Some(RunResult::Failed));
    }

    #[test]
    fn a_skipped_run_does_not_notify() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("notified");
        let shared = shared(&dir, FakeRunner::new(""), Some(format!("touch {}", out.display())));
        let _held = shared.locks.try_acquire("local", "tank").unwrap();
        shared.execute(Work::Run(due()));
        assert_eq!(history::runs(&shared.history.read())[0].result, Some(RunResult::Skipped));
        assert!(!out.exists());
    }

    #[test]
    fn an_orphaned_run_is_closed_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let started = Event { job_id: "gone".into(), run_id: "r1".into(), event: EventKind::Started, at: shared.clock.now(), scheduled_for: None, result: None, message: String::new(), catch_up: false, summary: None };
        shared.history.append(&started).unwrap();
        let run = history::runs(&shared.history.read()).remove(0);
        shared.execute(Work::Orphan(run));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Failed)));
        assert!(runs[0].message.starts_with("lost:"));
        assert!(runs[0].message.contains("daemon stopped"), "{}", runs[0].message);
    }

    #[test]
    fn a_resumed_run_finishes_before_its_queued_catch_up_starts() {
        let dir = tempfile::tempdir().unwrap();
        // Every `zpool` call fails, so the resumed run is closed as failed and the catch-up fails at its pre-check.
        let shared = shared(&dir, FakeRunner::failing("cannot open 'tank': no such pool", 1), None);
        let started = Event { job_id: "scrub-tank".into(), run_id: "old-run".into(), event: EventKind::Started, at: shared.clock.now(), scheduled_for: None, result: None, message: String::new(), catch_up: false, summary: Some("old".into()) };
        shared.history.append(&started).unwrap();
        let run = history::runs(&shared.history.read()).remove(0);
        let mut catch_up = due();
        catch_up.catch_up = true;
        shared.execute(Work::Resume { job: Box::new(job()), run: Box::new(run), then: Some(Box::new(catch_up)) });
        let order: Vec<String> = shared.history.read().into_iter().filter(|e| e.event != EventKind::Started).map(|e| e.run_id).collect();
        assert_eq!(order.len(), 2, "{order:?}");
        assert_eq!(order[0], "old-run", "the resumed run is closed first");
        assert_ne!(order[1], "old-run");
        let runs = history::runs(&shared.history.read());
        assert!(runs.iter().any(|r| r.catch_up && r.result == Some(RunResult::Failed)), "the catch-up then ran: {runs:?}");
    }

    #[test]
    fn load_hosts_reads_the_hosts_and_the_notify_command_from_config_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[[hosts]]\nname = \"fbsd16\"\nssh_target = \"fbsd@172.16.10.60\"\n\n[daemon]\nnotify_command = \"true\"\n").unwrap();
        let loaded = load_hosts(&path).unwrap();
        assert_eq!(loaded.hosts.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), vec!["local", "fbsd16"]);
        assert_eq!(loaded.notify_command.as_deref(), Some("true"));
        std::fs::write(&path, "not [[[ toml").unwrap();
        assert!(load_hosts(&path).is_err(), "a bad file is an error so the caller keeps the previous hosts");
        assert_eq!(load_hosts(&dir.path().join("missing.toml")).unwrap().hosts.len(), 1, "no file: just the local host");
    }

    #[test]
    fn applying_a_new_config_swaps_the_hosts_and_the_notify_command_for_running_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        assert!(shared.runner("later").is_none());
        let loaded = Loaded { hosts: vec![("local".to_string(), Arc::new(FakeRunner::new("")) as Arc<dyn CommandRunner>), ("later".to_string(), Arc::new(FakeRunner::new("")) as Arc<dyn CommandRunner>)], notify_command: Some("true".to_string()), notify: Some(crate::config::NotifySettings::default()), notify_problem: None, info: Vec::new() };
        let names = shared.apply(loaded);
        assert_eq!(names, vec!["local", "later"]);
        assert!(shared.runner("later").is_some());
        assert_eq!(shared.notify_command.read().unwrap().as_deref(), Some("true"));
    }

    #[test]
    fn log_lines_are_stamped_in_the_given_zone_to_the_second() {
        let when = chrono::DateTime::parse_from_rfc3339("2026-10-06T15:43:16.472Z").unwrap();
        let eastern = chrono::FixedOffset::west_opt(4 * 3600).unwrap();
        assert_eq!(stamp(&when.with_timezone(&eastern)), "2026-10-06T11:43:16-04:00");
        assert_eq!(stamp(&when.with_timezone(&Utc)), "2026-10-06T15:43:16+00:00");
    }

    #[test]
    fn work_for_an_unknown_host_is_ignored_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let mut d = due();
        d.job.host = "gone".into();
        shared.execute(Work::Run(d));
        assert!(shared.history.read().is_empty());
    }

    #[test]
    fn a_second_daemon_refuses_to_start_while_the_first_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        // A heartbeat from this very process, as if another daemon were running here.
        let other = heartbeat::Heartbeat { pid: 1, started_at: Utc::now(), last_tick: Utc::now() };
        heartbeat::write(&paths.heartbeat(), &other).unwrap();
        let err = run(&dir.path().join("config.toml"), paths, &Arc::new(AtomicBool::new(true))).unwrap_err();
        assert!(err.to_string().contains("another zfsctl daemon is already running (pid 1)"), "{err}");
    }

    #[test]
    fn the_daemon_writes_a_heartbeat_while_it_runs_and_removes_it_on_a_clean_stop() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let hb = paths.heartbeat();
        let stop = Arc::new(AtomicBool::new(false));
        let config_path = dir.path().join("config.toml");
        let stopper = Arc::clone(&stop);
        let watch_path = hb.clone();
        let watcher = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut seen = None;
            while std::time::Instant::now() < deadline && seen.is_none() {
                seen = heartbeat::read(&watch_path);
                std::thread::sleep(Duration::from_millis(50));
            }
            stopper.store(true, Ordering::SeqCst);
            seen
        });
        run(&config_path, paths, &stop).unwrap();
        let seen = watcher.join().unwrap().expect("a heartbeat appeared while the daemon ran");
        assert_eq!(seen.pid, std::process::id());
        assert!(!hb.exists(), "removed on a clean stop, so the TUI shows 'not running' at once");
    }

    #[test]
    fn a_due_snapshot_job_runs_through_the_snapshot_runner_and_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let due = Due { job: Job::test_snapshot("snap", "tank/data", "hourly", 3), scheduled_for: shared.clock.now(), catch_up: false };
        shared.execute(Work::Run(due));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Ok)));
        assert!(runs[0].message.starts_with("created hourly-20261007T030000Z"), "{}", runs[0].message);
    }

    #[test]
    fn a_cancelled_run_does_not_notify() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("notified");
        let shared = shared(&dir, FakeRunner::new(""), Some(format!("touch {}", out.display())));
        let outcome = Outcome { job_id: "j".into(), result: RunResult::Cancelled, message: "cancelled".into() };
        shared.notify(&job(), &outcome);
        assert!(!out.exists());
    }

    #[test]
    fn a_due_replicate_job_whose_destination_host_is_not_configured_is_recorded_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let job = Job::test_replicate("repl", "tank/data", "hourly", "gone", "backup/data");
        let due = Due { job, scheduled_for: shared.clock.now(), catch_up: false };
        shared.execute(Work::Run(due));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Failed)));
        assert!(runs[0].message.contains("gone"), "{}", runs[0].message);
    }

    #[test]
    fn a_cancel_request_sets_the_flag_of_the_running_job_only_and_is_always_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let running = Arc::new(AtomicBool::new(false));
        shared.cancels.lock().unwrap().insert(cancel::file_name("repl-a"), Arc::clone(&running));
        let cancel_dir = dir.path().join("cancel");
        cancel::request(&cancel_dir, "repl-a").unwrap();
        cancel::request(&cancel_dir, "not-running").unwrap();
        shared.apply_cancel_requests(&cancel_dir);
        assert!(running.load(Ordering::SeqCst));
        assert!(cancel::take_requests(&cancel_dir).is_empty(), "every request file is consumed");
    }

    #[test]
    fn startup_removes_progress_and_cancel_files_left_by_a_previous_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let p = progress::Progress { job_id: "j".into(), bytes: 1, total: None, started_at: Utc::now(), updated_at: Utc::now(), rate: None };
        progress::write(&paths.progress_dir(), "old", &p).unwrap();
        cancel::request(&paths.cancel_dir(), "j").unwrap();
        clear_runtime_files(&paths);
        assert!(progress::read_all(&paths.progress_dir()).is_empty());
        assert!(cancel::take_requests(&paths.cancel_dir()).is_empty());
    }

    #[test]
    fn a_second_slot_of_a_running_replication_is_skipped_and_leaves_the_running_ones_cancel_flag_alone() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let job = Job::test_replicate("repl", "tank/data", "hourly", "local", "backup/data");
        let running = Arc::new(AtomicBool::new(false));
        shared.cancels.lock().unwrap().insert(cancel::file_name("repl"), Arc::clone(&running));
        shared.execute(Work::Run(Due { job, scheduled_for: shared.clock.now(), catch_up: false }));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Skipped)));
        assert!(runs[0].message.contains("already running"), "{}", runs[0].message);
        let map = shared.cancels.lock().unwrap();
        assert!(map.get(&cancel::file_name("repl")).is_some_and(|f| Arc::ptr_eq(f, &running)), "the running transfer stays cancellable");
    }

    fn file_job(id: &str, dir: &Path) -> Job {
        let mut j = Job::test_replicate(id, "tank/data", "hourly", "file", "x");
        j.dest_dataset = None;
        j.dest_dir = Some(dir.display().to_string());
        j
    }

    #[test]
    fn a_due_file_replication_needs_no_destination_host() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let due = Due { job: file_job("repl-file", &dir.path().join("out")), scheduled_for: shared.clock.now(), catch_up: false };
        shared.execute(Work::Run(due));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Skipped)), "{:?}", runs[0].message);
        assert!(runs[0].message.contains("no snapshots"), "it reached the file run: {}", runs[0].message);
    }

    #[test]
    fn a_due_file_replication_from_an_unconfigured_source_host_is_recorded_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let shared = shared(&dir, FakeRunner::new(""), None);
        let mut job = file_job("repl-file", &dir.path().join("out"));
        job.host = "gone".into();
        shared.execute(Work::Run(Due { job, scheduled_for: shared.clock.now(), catch_up: false }));
        let runs = history::runs(&shared.history.read());
        assert_eq!((runs.len(), runs[0].result), (1, Some(RunResult::Failed)));
        assert!(runs[0].message.contains("gone"), "{}", runs[0].message);
    }

    #[test]
    fn startup_removes_partial_stream_files_of_file_jobs_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("tank-data@s2.zfs.partial"), "half").unwrap();
        std::fs::write(out.join("tank-data@s1.zfs"), "complete").unwrap();
        std::fs::write(out.join("notes.partial"), "not ours").unwrap();
        let host_to_host = Job::test_replicate("h", "tank/data", "hourly", "fbsd16", "backup/data");
        clear_partial_files(&[file_job("f", &out), host_to_host]);
        assert!(!out.join("tank-data@s2.zfs.partial").exists());
        assert!(out.join("tank-data@s1.zfs").exists() && out.join("notes.partial").exists());
    }

    #[test]
    fn a_missing_file_directory_is_not_an_error_at_startup() {
        clear_partial_files(&[file_job("f", Path::new("/nonexistent/zfsctl-test-dir"))]);
    }

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
}
