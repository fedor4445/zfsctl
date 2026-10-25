//! Reloading what a section shows: manual and automatic refreshes, and what to do with an answer that comes late.
#![allow(clippy::wildcard_imports)]
use super::*;

impl App {
    /// Clears a completed scrub/resilver status once it's been shown
    /// for `POOL_SCAN_STATUS_DISPLAY_DURATION` — called once per event
    /// loop tick so a finished resilver/scrub doesn't linger in the
    /// status bar forever. A no-op while `pool_scan_status` is
    /// `InProgress` (that one's supposed to stay until it finishes) or
    /// already `None`.
    pub fn clear_stale_scan_status(&mut self) {
        let Some(crate::zfs::pool::ScanStatus::Completed { .. }) = &self.pool_scan_status else {
            return;
        };
        if self.pool_scan_status_shown_at.is_some_and(|t| t.elapsed() >= POOL_SCAN_STATUS_DISPLAY_DURATION) {
            self.pool_scan_status = None;
            self.pool_scan_status_shown_at = None;
        }
    }

    pub(super) fn begin_refresh(&mut self, section: Section, host: usize) {
        self.in_flight.insert((section, host));
    }

    /// Returns whether the refresh that just finished was a quiet (auto) one.
    pub(super) fn end_refresh(&mut self, section: Section, host: usize) -> bool {
        self.in_flight.remove(&(section, host));
        self.quiet_refresh.remove(&(section, host))
    }

    /// A manual refresh result clears whatever status message was showing; a quiet
    /// (auto) one only clears a stale error, so "scrub started" is not wiped every
    /// interval but a "host unreachable" error does go away once the host answers.
    /// An error younger than `ERROR_STATUS_MIN_AGE` survives a quiet refresh, so a refusal is readable
    /// even when an overdue auto-refresh fires the moment its modal closes.
    pub(super) fn clear_status_after_refresh(&mut self, quiet: bool) {
        let young = self.status_error_at.is_some_and(|at| at.elapsed() < ERROR_STATUS_MIN_AGE);
        if !quiet || (self.status_is_error() && !young) {
            self.status_message = None;
        }
    }

    /// The list section the periodic refresh should keep fresh right now, if any:
    /// the one on screen, and only while nothing (a modal, a picker, a pool
    /// drill-down, which has its own faster poll) could be mid-interaction with it.
    /// Hosts has nothing to refresh.
    pub fn auto_refresh_section(&self) -> Option<Section> {
        if self.modal.is_some() || self.picker.is_some() || self.drilled_pool.is_some() {
            return None;
        }
        match self.current_section() {
            section @ (Section::Pools | Section::Datasets | Section::Snapshots | Section::Disks | Section::Shares | Section::BootEnvs) => Some(section),
            _ => None,
        }
    }

    /// Refreshes the visible list section if `interval` has passed since it was last
    /// auto-refreshed (immediately the first time it is shown on this host).
    pub fn auto_refresh(&mut self, runner: Arc<dyn CommandRunner>, now: Instant, interval: Duration) {
        let Some(section) = self.auto_refresh_section() else { return };
        let key = (section, self.selected_host);
        if self.last_auto_refresh.get(&key).is_some_and(|last| now.duration_since(*last) < interval) {
            return;
        }
        self.last_auto_refresh.insert(key, now);
        let already_running = self.is_refreshing(section, key.1);
        let for_snapshots = runner.clone();
        match section {
            Section::Pools => self.refresh_pools(runner),
            Section::Datasets => self.refresh_datasets(runner),
            Section::Snapshots => self.refresh_snapshots(runner),
            Section::Disks => self.refresh_disks(runner),
            Section::Shares => self.refresh_shares(runner),
            Section::BootEnvs => self.refresh_bootenvs(runner),
            _ => return,
        }
        if !already_running {
            self.quiet_refresh.insert(key);
        }
        // The destroy prompts count a dataset's snapshots, so the Datasets view refreshes the snapshot list
        // (quietly) with the datasets: the counts are as fresh as the list, never "unknown" or hours old.
        if section == Section::Datasets && !self.is_refreshing(Section::Snapshots, key.1) {
            self.refresh_snapshots(for_snapshots);
            self.quiet_refresh.insert((Section::Snapshots, key.1));
        }
    }

    /// Reloads whatever list is on screen, right now. Used after a shell
    /// session, which may have changed ZFS state behind the TUI's back.
    pub fn refresh_visible(&mut self, runner: Arc<dyn CommandRunner>) {
        if let Some(pool) = self.drilled_pool.clone() {
            self.refresh_vdevs(runner, pool);
            return;
        }
        match self.current_section() {
            Section::Pools => self.refresh_pools(runner),
            Section::Datasets => self.refresh_datasets(runner),
            Section::Snapshots => self.refresh_snapshots(runner),
            Section::Disks => self.refresh_disks(runner),
            Section::Shares => self.refresh_shares(runner),
            Section::BootEnvs => self.refresh_bootenvs(runner),
            Section::Jobs => self.refresh_jobs(),
            Section::Dashboard => self.refresh_dashboard(),
            Section::Cloud => self.refresh_cloud(),
            Section::Alerts => self.refresh_alerts(),
            Section::Settings => self.refresh_settings(),
            _ => {}
        }
    }

    pub fn refresh_jobs(&mut self) {
        let selected = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)).map(|j| j.id.clone());
        self.jobs_view = crate::ui::jobs::load(&self.state_dir, &self.hosts, Some(&self.jobs_view));
        if !self.cloud_sizes_loaded {
            self.cloud_sizes_loaded = true;
            for (job, size) in crate::ui::cloud::load_sizes(&self.state_dir) {
                self.cloud_sizes.entry(job).or_insert(size);
            }
        }
        match selected.and_then(|id| self.jobs_view.jobs.iter().position(|j| j.id == id)) {
            Some(i) => self.jobs_table.select(Some(i)),
            None => clamp_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len()),
        }
    }

    /// Refreshes the Jobs data if it is stale: every 2 seconds while the section is on screen
    /// (so a run's result appears live), every 10 otherwise (so the failed-jobs indicator is
    /// right from any section). The first call always loads.
    pub fn refresh_jobs_if_stale(&mut self, now: Instant) {
        let every = if matches!(self.current_section(), Section::Jobs | Section::Cloud | Section::Dashboard) { Duration::from_secs(2) } else { Duration::from_secs(10) };
        if self.last_jobs_refresh.is_none_or(|t| now.duration_since(t) >= every) {
            self.last_jobs_refresh = Some(now);
            self.refresh_jobs();
        }
    }

    /// Does not spawn a second refresh if one for the current host is
    /// already in flight — otherwise holding 'r' against an unreachable
    /// host would spawn an unbounded number of blocking threads/subprocesses,
    /// one per key-repeat tick. Keyed by host too, so a hung refresh for
    /// one host never blocks refreshing a different host. The request is
    /// not simply dropped, though: it's recorded in `pending_refresh` and
    /// re-dispatched (see `drain_worker_messages`) once the in-flight one
    /// completes, so a manual 'r' press (or a post-mutation refresh) that
    /// arrives mid-flight is never silently lost.
    pub fn refresh_pools(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Pools, host) {
            self.pending_refresh.insert((Section::Pools, host));
            return;
        }
        self.begin_refresh(Section::Pools, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::pool::list_pools(runner.as_ref());
            let _ = tx.send(WorkerMsg::Pools(host, result));
        });
    }

    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight.
    pub fn refresh_datasets(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Datasets, host) {
            self.pending_refresh.insert((Section::Datasets, host));
            return;
        }
        self.begin_refresh(Section::Datasets, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            // Not sent when the host could not be asked: the Lock column and the save-time guard keep what they knew.
            if let Some(found) = crate::zfs::encryption::list(runner.as_ref()) {
                let _ = tx.send(WorkerMsg::Encryption(host, found));
            }
            let result = crate::zfs::dataset::list_datasets(runner.as_ref());
            let _ = tx.send(WorkerMsg::Datasets(host, result));
        });
    }

    pub fn refresh_snapshots(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Snapshots, host) {
            self.pending_refresh.insert((Section::Snapshots, host));
            return;
        }
        self.begin_refresh(Section::Snapshots, host);
        let generation = self.snapshot_gen.entry(host).and_modify(|g| *g += 1).or_insert(1);
        let generation = *generation;
        let (tx, running) = (self.tx.clone(), Arc::clone(&self.tags_running));
        std::thread::spawn(move || {
            let result = crate::zfs::snapshot::list_snapshots(runner.as_ref());
            let held: Vec<String> = result.as_ref().map(|l| l.iter().filter(|s| s.holds > 0).map(|s| format!("{}@{}", s.dataset, s.name)).collect()).unwrap_or_default();
            let _ = tx.send(WorkerMsg::Snapshots(host, result));
            if held.is_empty() {
                return;
            }
            // One tag fetch per host at a time; a skipped one leaves the tags unknown until the next refresh.
            if !running.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(host) {
                return;
            }
            // A failed chunk leaves its snapshots out: their tags stay unknown, and they cannot be released from here.
            // The guard answers with an empty map if this thread panics, so the host's mark is always cleared.
            let mut reply = TagsReply { tx, host, generation, sent: false };
            let tags = crate::zfs::snapshot::hold_tags_for(runner.as_ref(), &held);
            reply.send(tags);
        });
    }

    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight. Fetches every disk on the host
    /// and its pool/vdev membership in one call — see
    /// `zfs::disk::list_disks_with_membership`.
    pub fn refresh_disks(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Disks, host) {
            self.pending_refresh.insert((Section::Disks, host));
            return;
        }
        self.begin_refresh(Section::Disks, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::disk::list_disks_with_membership(runner.as_ref());
            let _ = tx.send(WorkerMsg::Disks(host, result));
        });
    }

    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight.
    pub fn refresh_shares(&mut self, runner: Arc<dyn CommandRunner>) {
        if !self.section_supported(Section::Shares) {
            self.shares.clear();
            (self.nfs_available, self.samba_available) = (None, None);
            self.nfsv4_note = None;
            self.shares_loaded = false;
            return;
        }
        let host = self.selected_host;
        if self.is_refreshing(Section::Shares, host) {
            self.pending_refresh.insert((Section::Shares, host));
            return;
        }
        self.begin_refresh(Section::Shares, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::share::list_shares_with_availability(runner.as_ref());
            let _ = tx.send(WorkerMsg::Shares(host, result));
        });
    }

    /// See `refresh_pools` for why this queues rather than drops a request while in flight.
    pub fn refresh_bootenvs(&mut self, runner: Arc<dyn CommandRunner>) {
        if !self.section_supported(Section::BootEnvs) {
            self.bootenvs.clear();
            self.bootenvs_unavailable = None;
            return;
        }
        let host = self.selected_host;
        if self.is_refreshing(Section::BootEnvs, host) {
            self.pending_refresh.insert((Section::BootEnvs, host));
            return;
        }
        self.begin_refresh(Section::BootEnvs, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::bootenv::list(runner.as_ref());
            let _ = tx.send(WorkerMsg::BootEnvs(host, result));
        });
    }

    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight. `pool` is the name of the
    /// currently drilled-into pool.
    ///
    /// The in-flight key is `(Section::Vdevs, host)` — it does not
    /// include the pool name. This means drilling into a *different*
    /// pool while a previous pool's vdev refresh for the same host is
    /// still in flight will queue the new request as pending rather than
    /// starting it immediately; the user can press 'r' again once the
    /// stale one clears. This is intentionally simple: correctness (never
    /// showing one pool's disks under another pool's title) is fully
    /// handled by the stale-pool check in `drain_worker_messages`, so the
    /// in-flight key doesn't need to be pool-aware too.
    pub fn refresh_vdevs(&mut self, runner: Arc<dyn CommandRunner>, pool: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Vdevs, host) {
            self.pending_refresh.insert((Section::Vdevs, host));
            return;
        }
        self.begin_refresh(Section::Vdevs, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::pool::get_pool_vdevs(runner.as_ref(), &pool);
            let _ = tx.send(WorkerMsg::Vdevs(host, pool, result));
        });
    }

    /// See `refresh_pools` for the in-flight/pending-refresh behavior.
    /// Not pool-aware in its in-flight key, same simplifying tradeoff
    /// `refresh_vdevs` makes for pools — correctness (never showing one
    /// pool's properties under another's) is fully handled by the
    /// stale-identity discard in `drain_worker_messages`.
    pub fn refresh_pool_properties(&mut self, runner: Arc<dyn CommandRunner>, pool: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::PoolProperties, host) {
            self.pending_refresh.insert((Section::PoolProperties, host));
            return;
        }
        self.begin_refresh(Section::PoolProperties, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::pool::get_pool_properties(runner.as_ref(), &pool);
            let _ = tx.send(WorkerMsg::PoolProperties(host, pool, result));
        });
    }

    /// See `refresh_pool_properties`.
    pub fn refresh_dataset_properties(&mut self, runner: Arc<dyn CommandRunner>, dataset: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::DatasetProperties, host) {
            self.pending_refresh.insert((Section::DatasetProperties, host));
            return;
        }
        self.begin_refresh(Section::DatasetProperties, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::dataset::get_dataset_properties(runner.as_ref(), &dataset);
            let _ = tx.send(WorkerMsg::DatasetProperties(host, dataset, result));
        });
    }

    /// See `refresh_dataset_properties`.
    pub fn refresh_dataset_permissions(&mut self, runner: Arc<dyn CommandRunner>, dataset: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::DatasetPermissions, host) {
            self.pending_refresh.insert((Section::DatasetPermissions, host));
            return;
        }
        self.begin_refresh(Section::DatasetPermissions, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::permission::list_permissions(runner.as_ref(), &dataset);
            let _ = tx.send(WorkerMsg::DatasetPermissions(host, dataset, result));
        });
    }

    /// See `refresh_dataset_permissions`. `path` is the dataset's
    /// mountpoint, not its name -- ACLs are filesystem-path-scoped.
    pub fn refresh_dataset_acl(&mut self, runner: Arc<dyn CommandRunner>, path: String) {
        if !self.section_supported(Section::DatasetAcl) {
            self.acl_entries.clear();
            return;
        }
        let host = self.selected_host;
        if self.is_refreshing(Section::DatasetAcl, host) {
            self.pending_refresh.insert((Section::DatasetAcl, host));
            return;
        }
        self.begin_refresh(Section::DatasetAcl, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::acl::list_acl(runner.as_ref(), &path);
            let _ = tx.send(WorkerMsg::DatasetAcl(host, path, result));
        });
    }
}
