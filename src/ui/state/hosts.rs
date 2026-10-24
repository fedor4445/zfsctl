//! Hosts: the list and its filter, probes, the add-host check and the per-host checks.
#![allow(clippy::wildcard_imports)]
use super::*;

impl App {
    /// The object the bottom metrics panel describes right now: the drilled
    /// pool, else the highlighted pool or dataset, else nothing.
    pub fn metrics_target(&self) -> Option<crate::zfs::metrics::MetricsTarget> {
        use crate::zfs::metrics::MetricsTarget;
        if let Some(pool) = &self.drilled_pool {
            return Some(MetricsTarget::Pool(pool.clone()));
        }
        match self.current_section() {
            Section::Hosts => (!self.hosts.is_empty()).then_some(MetricsTarget::Host),
            Section::Pools => self.selected_pool_name().map(MetricsTarget::Pool),
            Section::Datasets => self.selected_dataset_name().map(MetricsTarget::Dataset),
            Section::Disks => self.disks_table.selected().and_then(|i| self.disks.get(i)).map(|d| MetricsTarget::Disk {
                name: d.name.clone(),
                pool: match &d.membership {
                    crate::zfs::disk::DiskMembership::InPool { pool, .. } => Some(pool.clone()),
                    crate::zfs::disk::DiskMembership::Available => None,
                },
            }),
            Section::Shares => self.shares_table.selected().and_then(|i| self.shares.get(i)).map(|share| match share {
                crate::zfs::share::Share::Nfs { dataset, .. } => MetricsTarget::Share { dataset: dataset.clone(), smb_name: None },
                crate::zfs::share::Share::Smb { dataset, name, .. } => MetricsTarget::Share { dataset: dataset.clone(), smb_name: Some(name.clone()) },
            }),
            Section::Snapshots => self
                .snapshots_table
                .selected()
                .and_then(|i| self.snapshots.get(i))
                .map(|s| MetricsTarget::Snapshot(format!("{}@{}", s.dataset, s.name))),
            _ => None,
        }
    }

    /// Keeps the metrics panel pointed at the selection and starts a sample
    /// on a worker thread when one is due. Called once per event-loop tick.
    pub fn poll_metrics(&mut self, runner: Arc<dyn CommandRunner>, now: Instant) {
        self.metrics.retarget(self.metrics_target());
        if !self.metrics.due(now) {
            return;
        }
        let Some(target) = self.metrics.target().cloned() else { return };
        let with_contents = self.metrics.want_contents(now);
        self.metrics.begin();
        let host = self.selected_host;
        let (tx, os) = (self.tx.clone(), self.host_os(host));
        std::thread::spawn(move || {
            let result = crate::zfs::metrics::sample_target(runner.as_ref(), &target, with_contents, os);
            let _ = tx.send(WorkerMsg::Metrics(host, target, Box::new(result)));
        });
    }

    /// Reloads the Jobs data from the state directory. These are small local files, so
    /// there is no runner and no thread. The selection stays on the same job.
    pub fn set_host_addresses(&mut self, addresses: Vec<String>) {
        self.host_addresses = addresses;
    }

    /// Whether host `index` matches `needle` (case-insensitive substring of its name, address, tag or type word).
    pub fn host_matches(&self, index: usize, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        let Some(name) = self.hosts.get(index) else { return false };
        let kind = self.host_probes.get(name).and_then(|s| s.kind.as_ref()).map_or("", |k| if matches!(k, crate::zfs::host_health::HostKind::Unknown) { "" } else { k.word() });
        [name.as_str(), self.host_address(index), self.host_tag(index), kind].iter().any(|f| f.to_lowercase().contains(&needle))
    }

    /// Global indices of the hosts the filter lets through, in order (all of them when there is no filter).
    pub fn visible_hosts(&self) -> Vec<usize> {
        (0..self.hosts.len()).filter(|i| self.host_filter.is_empty() || self.host_matches(*i, &self.host_filter)).collect()
    }

    /// Keeps the selection on a visible host when the filter hides it (so no section can act on a host the user
    /// cannot see). With no match at all the selection stays put: such a filter cannot be committed.
    pub(super) fn ensure_visible_selection(&mut self) {
        if let Some(first) = self.visible_hosts().first().copied().filter(|_| !self.visible_hosts().contains(&self.selected_host)) {
            self.selected_host = first;
            self.clear_host_data();
        }
    }

    pub(super) fn handle_filter_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char(c) => {
                self.host_filter.push(c);
                self.ensure_visible_selection();
            }
            KeyCode::Backspace => {
                self.host_filter.pop();
                self.ensure_visible_selection();
            }
            KeyCode::Esc => {
                self.host_filter.clear();
                self.host_filter_editing = false;
                if let Some(origin) = self.host_filter_origin.take().filter(|o| *o != self.selected_host && *o < self.hosts.len()) {
                    self.selected_host = origin;
                    self.clear_host_data();
                }
            }
            KeyCode::Enter if self.host_filter.is_empty() || !self.visible_hosts().is_empty() => {
                self.host_filter_editing = false;
                self.host_filter_origin = None;
            }
            _ => {}
        }
    }

    pub fn set_host_tags(&mut self, tags: Vec<String>) {
        self.host_tags = tags;
    }

    pub fn host_tag(&self, index: usize) -> &str {
        self.host_tags.get(index).map_or("", String::as_str)
    }

    pub fn host_address(&self, index: usize) -> &str {
        self.host_addresses.get(index).map_or("-", String::as_str)
    }

    /// Hosts whose probe is due: never probed (host `i` becomes due `i` seconds after start, so the hosts are
    /// not all probed in the same second) or last finished at least the interval ago; never while one is running.
    pub fn due_host_probes(&self, now: Instant) -> Vec<usize> {
        // A big fleet is asked a few hosts at a time: the places not taken by probes still running.
        let running = self.host_probes.values().filter(|s| s.in_flight).count();
        let room = crate::daemon::workers::PARALLEL.saturating_sub(running);
        self.hosts
            .iter()
            .enumerate()
            .filter(|(i, name)| {
                let state = self.host_probes.get(*name);
                if state.is_some_and(|s| s.in_flight) {
                    return false;
                }
                match state.and_then(|s| s.finished_at) {
                    None => now >= self.probe_epoch + HOST_PROBE_STAGGER * u32::try_from(*i).unwrap_or(u32::MAX),
                    Some(done) => now.duration_since(done) >= HOST_PROBE_INTERVAL,
                }
            })
            .map(|(i, _)| i)
            .take(room)
            .collect()
    }

    /// Starts one probe on a worker thread (the event loop resolves the runner; `App` cannot).
    pub fn start_host_probe(&mut self, index: usize, runner: Arc<dyn CommandRunner>) {
        let Some(name) = self.hosts.get(index).cloned() else { return };
        let state = self.host_probes.entry(name.clone()).or_default();
        state.in_flight = true;
        state.started_at = Some(Instant::now());
        let detect_kind = state.kind.is_none();
        let known_os = state.os;
        let want_offset = state.utc_offset_secs.is_none();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let probe = crate::zfs::host_health::probe_host_guarded(runner.as_ref(), detect_kind);
            let reached = matches!(probe, crate::zfs::host_health::HostProbe::Reached { .. });
            // A probe that did not ask for the OS again (it is read once) leaves it to what the app already knows.
            let os = match &probe {
                crate::zfs::host_health::HostProbe::Reached { os, .. } => os.or(known_os),
                _ => None,
            };
            let _ = tx.send(WorkerMsg::HostHealth(name.clone(), probe));
            // The Dashboard's readings are TUI-only (the daemon's monitor probes without them) and only asked of a host that answered.
            let (resources, offset) = if reached {
                (crate::zfs::host_resources::probe_resources(runner.as_ref(), os), want_offset.then(|| crate::zfs::host_resources::utc_offset_secs(runner.as_ref())).flatten())
            } else {
                (None, None)
            };
            let _ = tx.send(WorkerMsg::HostResources(name, resources, offset));
        });
    }

    /// The newest resource reading of a host and the one before it, if it has them.
    pub fn host_resources(&self, name: &str) -> Option<(&crate::zfs::host_resources::Resources, Option<&crate::zfs::host_resources::Resources>)> {
        let all = &self.host_probes.get(name)?.resources;
        let newest = all.last()?;
        Some((newest, all.len().checked_sub(2).and_then(|i| all.get(i))))
    }

    /// The health of host `index` from its latest probe and from the Jobs data (an enabled job of this host
    /// whose latest run failed).
    pub(super) fn host_probe_gave_up(&self, name: &str) -> bool {
        self.host_probes.get(name).filter(|s| s.in_flight).and_then(|s| s.started_at).is_some_and(|t| t.elapsed() > HOST_PROBE_GIVE_UP)
    }

    pub fn host_assessment(&self, index: usize) -> crate::zfs::host_health::Assessment {
        let Some(name) = self.hosts.get(index) else { return crate::zfs::host_health::assess(None, false) };
        // A probe that has been running far longer than any healthy one (a hung ssh, a wedged pool) must not
        // leave the last good answer on screen.
        if let Some(waited) = self.host_probes.get(name).filter(|s| s.in_flight).and_then(|s| s.started_at).map(|t| t.elapsed()).filter(|d| *d > HOST_PROBE_GIVE_UP) {
            return crate::zfs::host_health::Assessment { level: crate::zfs::host_health::Level::Warning, reason: format!("no answer for {}s", waited.as_secs()) };
        }
        let job_failed = self.jobs_view.jobs.iter().any(|j| j.enabled && &j.host == name && self.jobs_view.last_run(&j.id).is_some_and(|r| r.result == Some(crate::daemon::history::RunResult::Failed)));
        crate::zfs::host_health::assess(self.host_probes.get(name).and_then(|s| s.last.as_ref()), job_failed)
    }

    /// `(failed, total)` over the enabled jobs of host `name`; failed means the latest run failed.
    pub fn host_jobs_counts(&self, name: &str) -> (usize, usize) {
        let mine = || self.jobs_view.jobs.iter().filter(|j| j.enabled && j.host == name);
        let failed = mine().filter(|j| self.jobs_view.last_run(&j.id).is_some_and(|r| r.result == Some(crate::daemon::history::RunResult::Failed))).count();
        (failed, mine().count())
    }

    /// `c` in Hosts: queue a check of the selected host's disks and shares, unless one is already running (from here, or
    /// by the daemon or another zfsctl, which the marker file tells).
    pub(super) fn request_host_check(&mut self) {
        let Some(host) = self.hosts.get(self.selected_host).cloned() else { return };
        if self.host_check_running {
            self.set_error("A check is already running".to_string());
            return;
        }
        if let Some(other) = crate::daemon::checks::fresh_marker(&self.state_dir, chrono::Utc::now()) {
            let since = other.since.with_timezone(&chrono::Local).format("%H:%M");
            self.set_error(format!("A check is already running ({}, since {since})", other.by));
            return;
        }
        self.host_check_running = true;
        self.status_error = None;
        self.status_message = Some(format!("Checking {host}..."));
        self.pending_host_checks.push(host);
    }

    /// Runs `zpool add -n` for a queued vdev add on a worker thread; the answer reaches `on_add_checked`. A host that does not
    /// answer within `ADD_CHECK_DEADLINE` gives the form back with that said, so a confirm never waits for its layout forever.
    pub fn start_add_check(&mut self, check: AddCheck, runner: Arc<dyn CommandRunner>) {
        self.start_add_check_with(check, runner, ADD_CHECK_DEADLINE);
    }

    pub fn start_add_check_with(&mut self, check: AddCheck, runner: Arc<dyn CommandRunner>, deadline: Duration) {
        let answered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let (tx, check, answered) = (self.tx.clone(), check.clone(), Arc::clone(&answered));
            std::thread::spawn(move || {
                std::thread::sleep(deadline);
                if !answered.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    let _ = tx.send(WorkerMsg::AddChecked(check, Err(format!("the host did not answer the layout check within {} s", deadline.as_secs().max(1)))));
                }
            });
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let spec: Vec<&str> = check.spec.iter().map(String::as_str).collect();
            let result = crate::zfs::pool::add_vdev_dry_run(runner.as_ref(), &check.pool, &spec).map_err(|e| e.to_string());
            if !answered.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let _ = tx.send(WorkerMsg::AddChecked(check, result));
            }
        });
    }

    /// Runs both checks on `host` in the background, with the same code as the daemon's daily pass, and merges the answers
    /// into `smart.json` and `shares.json`. A running marker tells the Dashboard (and the daemon) meanwhile; a host that
    /// does not answer within the daemon's per-host deadline is given up on, so `c` and the daemon are never held up.
    pub fn start_host_check(&mut self, host: String, runner: Arc<dyn CommandRunner>) {
        self.start_host_check_with(host, runner, crate::daemon::smart_check::HOST_DEADLINE);
    }

    pub fn start_host_check_with(&mut self, host: String, runner: Arc<dyn CommandRunner>, deadline: Duration) {
        let (state_dir, tx) = (self.state_dir.clone(), self.tx.clone());
        std::thread::spawn(move || {
            // A crash in the worker must still answer, or `c` would stay busy for the rest of the session.
            let line = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Some(_running) = crate::daemon::checks::RunGuard::start(&state_dir, "tui", Some(&host)) else {
                    return format!("{host}: another check is running; try again in a moment");
                };
                // The quota reading rides along, at the same time and within the same deadline (one `zfs list`, merged into
                // storage.json like the other two), so a host that hangs holds neither.
                let storage = {
                    let (dir, one, runner) = (state_dir.clone(), host.clone(), Arc::clone(&runner));
                    std::thread::spawn(move || crate::daemon::storage_check::run_pass(&dir, &[(one, runner)], chrono::Utc::now(), deadline))
                };
                let done = crate::daemon::checks::collect(&state_dir, &[(host.clone(), runner)], chrono::Utc::now(), deadline);
                let _ = storage.join();
                // Read after the storage pass has joined, so the roles are this check's.
                let storage_file = crate::daemon::storage_check::StorageFile::load(&state_dir.join(crate::daemon::storage_check::FILE));
                let role_of = |h: &str, dev: &str| crate::daemon::storage_check::role_of(&storage_file, h, dev, None);
                done.first().map_or_else(|| format!("{host}: nothing to check"), |(_, checks)| crate::daemon::checks::describe_with(&host, checks, &role_of))
            }))
            .unwrap_or_else(|_| format!("{host}: the check crashed; what it had found may not be saved"));
            let _ = tx.send(WorkerMsg::HostChecked(line));
        });
    }

    /// Re-reads what the daily and manual checks found (`smart.json`, `shares.json`) and the running marker.
    pub(super) fn reload_check_files(&mut self) {
        self.smart = crate::daemon::smart_check::SmartFile::load(&self.state_dir.join("smart.json"));
        self.shares_check = crate::daemon::shares_check::SharesFile::load(&self.state_dir.join("shares.json"));
        self.storage_check = crate::daemon::storage_check::StorageFile::load(&self.state_dir.join("storage.json"));
        self.refresh_checks_marker();
    }

    /// Re-reads the marker of a running check. Cheap (one small file), so it is also done every pass of the event loop while the
    /// Dashboard is open: the red notice must appear and vanish with the check, not with the next 60-second refresh.
    pub fn refresh_checks_marker(&mut self) {
        self.checks_marker = crate::daemon::checks::fresh_marker(&self.state_dir, chrono::Utc::now());
    }

    /// Spawns the SSH reachability probe for a candidate new host and
    /// returns immediately -- the modal is already closed by the
    /// caller before this runs, matching every other slow mutation in
    /// this app (the outcome is reported later via `status_message`,
    /// not by keeping the form open across the async gap). Uses
    /// `probe_reachable`, which checks the exit status (a raw `SshRunner`
    /// call returns `Ok` even for an unreachable host or a rejected
    /// key, just with a non-zero `CommandOutput::status`) and runs
    /// without sudo.
    pub fn begin_add_host(&mut self, name: String, target: String) {
        self.status_message = Some(format!("Connecting to '{target}'..."));
        let tx = self.tx.clone();
        let name_for_thread = name;
        let target_for_thread = target;
        std::thread::spawn(move || {
            let runner = crate::exec::ssh::SshRunner::new(target_for_thread.clone());
            let result = probe_reachable(&runner, &target_for_thread);
            let _ = tx.send(WorkerMsg::HostProbe(name_for_thread, target_for_thread, result));
        });
    }
}
