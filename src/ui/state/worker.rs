//! What the UI does with the answers of its background workers: one handler per `WorkerMsg`, plus the
//! loop that drains the channel.

use super::{
    clamp_table_selection, flatten_permissions_snapshot, pin_selection, share_identity, App, Dataset, DiskInfo, DiskMembership, Instant, Modal, PickerState, Pool, Property, Result, Section, SharesSnapshot,
    Snapshot, VdevLine, WorkerMsg,
};

impl App {
    /// Drains all pending worker messages, applying each to `self`.
    /// Returns the list of sections that need re-listing, either because
    /// a mutation targeting the currently-selected host just succeeded,
    /// or because a refresh request for that section arrived while a
    /// same-section refresh was already in flight and got queued in
    /// `pending_refresh` (see `refresh_pools` et al.) instead of being
    /// dropped — the caller (event loop) looks up the right runner and
    /// triggers the actual refresh in both cases, since `App` has no way
    /// to resolve a host index to a runner on its own.
    pub fn drain_worker_messages(&mut self) -> Vec<Section> {
        let mut refreshes_needed = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Pools(host, result) => self.on_pools(host, result, &mut refreshes_needed),
                WorkerMsg::Datasets(host, result) => self.on_datasets(host, result, &mut refreshes_needed),
                WorkerMsg::Encryption(host, found) => {
                    if host == self.selected_host {
                        self.encryption = found;
                        self.encryption_for = host;
                    }
                }
                WorkerMsg::Snapshots(host, result) => self.on_snapshots(host, result, &mut refreshes_needed),
                WorkerMsg::Disks(host, result) => self.on_disks(host, result, &mut refreshes_needed),
                WorkerMsg::Shares(host, result) => self.on_shares(host, result, &mut refreshes_needed),
                WorkerMsg::BootEnvs(host, result) => self.on_boot_envs(host, result, &mut refreshes_needed),
                WorkerMsg::Vdevs(host, pool, result) => self.on_vdevs(host, &pool, result, &mut refreshes_needed),
                WorkerMsg::Metrics(host, target, result) => self.on_metrics(host, &target, result),
                WorkerMsg::PoolProperties(host, pool, result) => self.on_pool_properties(host, pool, result, &mut refreshes_needed),
                WorkerMsg::DatasetProperties(host, dataset, result) => self.on_dataset_properties(host, dataset, result, &mut refreshes_needed),
                WorkerMsg::DatasetPermissions(host, dataset, result) => self.on_dataset_permissions(host, dataset, result, &mut refreshes_needed),
                WorkerMsg::DatasetAcl(host, path, result) => self.on_dataset_acl(host, path, result, &mut refreshes_needed),
                WorkerMsg::Accounts(host, result) => self.on_accounts(host, result),
                WorkerMsg::HostResources(name, resources, offset) => self.on_host_resources(name, resources, offset),
                WorkerMsg::CloudRemotes(host, result) => self.on_cloud_remotes(host, result),
                WorkerMsg::CloudSize(job_id, size) => self.on_cloud_size(job_id, size),
                WorkerMsg::Preflight(host, repository, password_file, result, file_note) => self.on_preflight(host, &repository, &password_file, result, file_note),
                WorkerMsg::PrunePreview(host, dataset, result) => self.on_prune_preview(host, &dataset, result),
                WorkerMsg::PoolNames(host, result) => self.on_pool_names(host, result),
                WorkerMsg::HostHealth(name, probe) => self.on_host_health(name, probe),
                WorkerMsg::HostProbe(name, target, result) => self.on_host_probe(name, target, result),
                WorkerMsg::AlertsPreview(text) => self.on_alerts_preview(text),
                WorkerMsg::AddChecked(check, result) => self.on_add_checked(check, result),
                WorkerMsg::HostChecked(_, line) => {
                    self.host_check_running = false;
                    self.status_message = Some(line);
                    self.reload_check_files();
                }
                WorkerMsg::MutationDone(host, section, result) => self.on_mutation_done(host, section, result, &mut refreshes_needed),
            }
        }
        refreshes_needed
    }

    /// The dry run of a vdev add answered. If its confirm is still on screen it now shows the layout; if the host refused the
    /// add, the confirm gives way to the form again with the host's own message. A confirm that is gone (declined, or another
    /// one opened) ignores a late answer.
    pub fn on_add_checked(&mut self, check: super::AddCheck, result: std::result::Result<String, String>) {
        let still_asked = matches!(&self.modal, Some(Modal::Confirm { action, .. })
            if matches!(&action.call, crate::ui::modal::MutationCall::AddVdev { pool, vdev_spec } if *pool == check.pool && *vdev_spec == check.spec) && action.host == check.host)
            && self.add_check_ids == check.id;
        if !still_asked {
            return;
        }
        match result {
            Ok(layout) => {
                let host = self.hosts.get(check.host).map_or("?", String::as_str).to_string();
                let prompt = super::forms::add_vdev_prompt(&host, &check.pool, &check.spec, check.role, Some(&layout));
                if let Some(Modal::Confirm { prompt: shown, .. }) = &mut self.modal {
                    shown.clone_from(&prompt);
                }
                if let Some((restore_prompt, _)) = &mut self.confirm_restore {
                    *restore_prompt = prompt;
                }
            }
            Err(message) => {
                self.modal = Some(check.form);
                self.confirm_restore = None;
                // zpool's refusals run over several lines ("invalid vdev specification / use '-f' to override ..."): one line
                // keeps the hint on the status bar.
                self.set_error(message.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" | "));
            }
        }
    }

    fn on_pools(&mut self, host: usize, result: Result<Vec<Pool>>, refreshes_needed: &mut Vec<Section>) {
        let quiet = self.end_refresh(Section::Pools, host);
        // A refresh request that arrived while this one was in
        // flight (see `refresh_pools`) is fulfilled by queuing a
        // fresh one now, rather than staying lost forever.
        let had_pending = self.pending_refresh.remove(&(Section::Pools, host));
        if host != self.selected_host {
            // Late result for a host we've since navigated
            // away from — discard silently, it's not an error.
            return;
        }
        if had_pending {
            refreshes_needed.push(Section::Pools);
        }
        match result {
            Ok(pools) => {
                let previous = self.pools_table.selected().and_then(|i| self.pools.get(i)).map(|p| p.name.clone());
                self.pools = pools;
                pin_selection(&mut self.pools_table, previous, &self.pools, |p| p.name.clone());
                self.clear_status_after_refresh(quiet);
                // The list may have been reordered/resized in a
                // way that re-points the still-selected index at
                // a different pool than the one `pool_properties`
                // was fetched for — if so, the cached properties
                // are now stale and would render under the
                // wrong pool's title (see Finding 1). Only
                // invalidate on an actual mismatch; the common
                // case (never viewed, or still the same pool)
                // must be left untouched.
                if self.pool_properties_for.is_some() && self.pool_properties_for != self.selected_pool_name() {
                    self.pool_properties.clear();
                    self.pool_properties_table.select(None);
                    self.pool_properties_for = None;
                }
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn on_datasets(&mut self, host: usize, result: Result<Vec<Dataset>>, refreshes_needed: &mut Vec<Section>) {
        let quiet = self.end_refresh(Section::Datasets, host);
        let had_pending = self.pending_refresh.remove(&(Section::Datasets, host));
        if host != self.selected_host {
            return;
        }
        if had_pending {
            refreshes_needed.push(Section::Datasets);
        }
        match result {
            Ok(datasets) => {
                let previous = self.datasets_table.selected().and_then(|i| self.datasets.get(i)).map(|d| d.name.clone());
                self.datasets = datasets;
                pin_selection(&mut self.datasets_table, previous, &self.datasets, |d| d.name.clone());
                self.clear_status_after_refresh(quiet);
                // See the matching comment in the `Pools` arm.
                if self.dataset_properties_for.is_some() && self.dataset_properties_for != self.selected_dataset_name() {
                    self.dataset_properties.clear();
                    self.dataset_properties_table.select(None);
                    self.dataset_properties_for = None;
                }
                if self.dataset_permissions_for.is_some() && self.dataset_permissions_for != self.selected_dataset_name() {
                    self.dataset_permissions.clear();
                    self.dataset_permissions_table.select(None);
                    self.dataset_permissions_for = None;
                }
                if self.acl_entries_for.is_some() && self.acl_entries_for != self.selected_dataset_mountpoint() {
                    self.acl_entries.clear();
                    self.acl_entries_table.select(None);
                    self.acl_entries_for = None;
                }
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn on_snapshots(&mut self, host: usize, result: Result<Vec<Snapshot>>, refreshes_needed: &mut Vec<Section>) {
        let quiet = self.end_refresh(Section::Snapshots, host);
        let had_pending = self.pending_refresh.remove(&(Section::Snapshots, host));
        if host != self.selected_host {
            return;
        }
        if had_pending {
            refreshes_needed.push(Section::Snapshots);
        }
        match result {
            Ok(snapshots) => {
                let previous = self.snapshots_table.selected().and_then(|i| self.snapshots.get(i)).map(|s| (s.dataset.clone(), s.name.clone()));
                self.snapshots = snapshots;
                pin_selection(&mut self.snapshots_table, previous, &self.snapshots, |s| (s.dataset.clone(), s.name.clone()));
                self.snapshots_loaded = true;
                self.clear_status_after_refresh(quiet);
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn on_disks(&mut self, host: usize, result: Result<Vec<DiskInfo>>, refreshes_needed: &mut Vec<Section>) {
        let quiet = self.end_refresh(Section::Disks, host);
        let had_pending = self.pending_refresh.remove(&(Section::Disks, host));
        if host != self.selected_host {
            return;
        }
        if had_pending {
            refreshes_needed.push(Section::Disks);
        }
        match result {
            Ok(disks) => {
                // Drop any selected disk that no longer
                // exists, or has stopped being Available
                // (claimed by a concurrent operation, or a
                // slow refresh landing late) — a stale
                // selection must not be submittable against
                // a device that's no longer free.
                self.selected_disks.retain(|name| {
                    disks.iter().any(|d| &d.name == name && d.membership == DiskMembership::Available)
                });
                let previous = self.disks_table.selected().and_then(|i| self.disks.get(i)).map(|d| d.name.clone());
                self.disks = disks;
                pin_selection(&mut self.disks_table, previous, &self.disks, |d| d.name.clone());
                self.clear_status_after_refresh(quiet);
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn on_shares(&mut self, host: usize, result: Result<SharesSnapshot>, refreshes_needed: &mut Vec<Section>) {
        let quiet = self.end_refresh(Section::Shares, host);
        let had_pending = self.pending_refresh.remove(&(Section::Shares, host));
        if host != self.selected_host {
            return;
        }
        if had_pending {
            refreshes_needed.push(Section::Shares);
        }
        match result {
            Ok(snapshot) => {
                let previous = self.shares_table.selected().and_then(|i| self.shares.get(i)).map(share_identity);
                self.shares = snapshot.shares;
                pin_selection(&mut self.shares_table, previous, &self.shares, share_identity);
                self.nfs_available = Some(snapshot.nfs_available).filter(|_| !snapshot.nfs_unknown);
                self.samba_available = Some(snapshot.samba_available).filter(|_| !snapshot.samba_unknown);
                self.shares_loaded = true;
                self.clear_status_after_refresh(quiet);
                if let Some(e) = snapshot.smb_error {
                    self.set_error(format!("SMB shares unavailable: {e}"));
                }
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn on_boot_envs(&mut self, host: usize, result: Result<Vec<crate::zfs::bootenv::BootEnv>>, refreshes_needed: &mut Vec<Section>) {
        let quiet = self.end_refresh(Section::BootEnvs, host);
        let had_pending = self.pending_refresh.remove(&(Section::BootEnvs, host));
        if host != self.selected_host {
            return;
        }
        if had_pending {
            refreshes_needed.push(Section::BootEnvs);
        }
        match result {
            Ok(envs) => {
                // A just-renamed boot environment keeps the selection under its new name, once the
                // list shows it (an earlier refresh that still has the old name must not use up the pin).
                let wanted = self.bootenv_pin.clone().filter(|n| envs.iter().any(|b| &b.name == n));
                let previous = wanted.clone().or_else(|| self.selected_bootenv().map(|b| b.name.clone()));
                if wanted.is_some() {
                    self.bootenv_pin = None;
                }
                self.bootenvs = envs;
                pin_selection(&mut self.bootenvs_table, previous, &self.bootenvs, |b| b.name.clone());
                self.bootenvs_unavailable = None;
                self.bootenvs_loaded = true;
                self.clear_status_after_refresh(quiet);
            }
            Err(e) => {
                // Not `set_error`: a host without bectl would otherwise raise a fresh
                // error every auto-refresh. The title carries the reason instead.
                self.bootenvs.clear();
                self.bootenvs_table.select(None);
                self.bootenvs_unavailable = Some(e.to_string());
                self.bootenvs_loaded = false;
            }
        }
    }

    fn on_vdevs(&mut self, host: usize, pool: &str, result: Result<(Vec<VdevLine>, Option<crate::zfs::pool::ScanStatus>)>, refreshes_needed: &mut Vec<Section>) {
        self.end_refresh(Section::Vdevs, host);
        let had_pending = self.pending_refresh.remove(&(Section::Vdevs, host));
        if host != self.selected_host {
            return;
        }
        if had_pending {
            refreshes_needed.push(Section::Vdevs);
        }
        // Stale-pool check: a slow refresh for a pool the user
        // has since backed out of (or replaced with a different
        // drill-down) must not overwrite the vdev pane with the
        // wrong pool's disks under the current pool's title.
        if Some(pool) != self.drilled_pool.as_deref() {
            return;
        }
        match result {
            Ok((vdevs, scan_status)) => {
                self.vdevs = vdevs;
                clamp_table_selection(&mut self.vdevs_table, self.vdevs.len());
                self.pool_scan_status = scan_status;
                self.pool_scan_status_shown_at = Some(Instant::now());
                self.status_message = None;
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn on_metrics(&mut self, host: usize, target: &crate::zfs::metrics::MetricsTarget, result: Box<Result<crate::zfs::metrics::TargetSample>>) {
        if host == self.selected_host {
            self.metrics.apply(target, *result, Instant::now());
        }
    }

    fn on_pool_properties(&mut self, host: usize, pool: String, result: Result<Vec<Property>>, refreshes_needed: &mut Vec<Section>) {
        self.end_refresh(Section::PoolProperties, host);
        let had_pending = self.pending_refresh.remove(&(Section::PoolProperties, host));
        if host != self.selected_host {
            return;
        }
        if had_pending {
            refreshes_needed.push(Section::PoolProperties);
        }
        if Some(&pool) != self.selected_pool_name().as_ref() {
            return;
        }
        match result {
            Ok(props) => {
                self.pool_properties = props;
                clamp_table_selection(&mut self.pool_properties_table, self.pool_properties.len());
                self.pool_properties_for = Some(pool);
                self.status_message = None;
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn on_dataset_properties(&mut self, host: usize, dataset: String, result: Result<Vec<Property>>, refreshes_needed: &mut Vec<Section>) {
        self.end_refresh(Section::DatasetProperties, host);
        let had_pending = self.pending_refresh.remove(&(Section::DatasetProperties, host));
        if host != self.selected_host {
            return;
        }
        if had_pending {
            refreshes_needed.push(Section::DatasetProperties);
        }
        if Some(&dataset) != self.selected_dataset_name().as_ref() {
            return;
        }
        match result {
            Ok(props) => {
                self.dataset_properties = props;
                clamp_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len());
                self.dataset_properties_for = Some(dataset);
                self.status_message = None;
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn on_dataset_permissions(&mut self, host: usize, dataset: String, result: Result<crate::zfs::permission::PermissionsSnapshot>, refreshes_needed: &mut Vec<Section>) {
        self.end_refresh(Section::DatasetPermissions, host);
        let had_pending = self.pending_refresh.remove(&(Section::DatasetPermissions, host));
        if host != self.selected_host {
            return;
        }
        if had_pending {
            refreshes_needed.push(Section::DatasetPermissions);
        }
        if Some(&dataset) != self.selected_dataset_name().as_ref() {
            return;
        }
        match result {
            Ok(snapshot) => {
                self.dataset_permissions = flatten_permissions_snapshot(&snapshot);
                clamp_table_selection(&mut self.dataset_permissions_table, self.dataset_permissions.len());
                self.dataset_permissions_for = Some(dataset);
                self.status_message = None;
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn on_dataset_acl(&mut self, host: usize, path: String, result: Result<Vec<crate::acl::AclEntry>>, refreshes_needed: &mut Vec<Section>) {
        self.end_refresh(Section::DatasetAcl, host);
        let had_pending = self.pending_refresh.remove(&(Section::DatasetAcl, host));
        if host != self.selected_host {
            return;
        }
        if had_pending {
            refreshes_needed.push(Section::DatasetAcl);
        }
        if Some(&path) != self.selected_dataset_mountpoint().as_ref() {
            return;
        }
        match result {
            Ok(entries) => {
                self.acl_entries = entries;
                clamp_table_selection(&mut self.acl_entries_table, self.acl_entries.len());
                self.acl_entries_for = Some(path);
                self.status_message = None;
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }

    fn on_accounts(&mut self, host: usize, result: Result<crate::zfs::accounts::HostAccounts>) {
        self.account_fetch_in_flight.remove(&host);
        let current = self.focused_form_value();
        let waiting = |p: &PickerState| p.load == crate::ui::picker::PickerLoad::Loading && p.account.map(|(h, _)| h) == Some(host);
        match result {
            Ok(accounts) => {
                if let Some(p) = self.picker.as_mut().filter(|p| waiting(p)) {
                    crate::ui::picker::fill_account_picker(p, &accounts, &current);
                }
                self.account_cache.insert(host, (Instant::now(), accounts));
            }
            Err(e) => {
                if let Some(p) = self.picker.as_mut().filter(|p| waiting(p)) {
                    p.load = crate::ui::picker::PickerLoad::Failed(e.to_string());
                }
            }
        }
    }

    fn on_cloud_remotes(&mut self, host: String, result: std::result::Result<Vec<(String, String)>, String>) {
        self.cloud_busy.remove(&format!("remotes:{host}"));
        match result {
            Ok(list) => {
                self.cloud_errors.remove(&host);
                self.cloud_known.insert(host, list);
            }
            Err(problem) => {
                self.cloud_errors.insert(host, problem);
            }
        }
        let rows = self.cloud_rows().len();
        clamp_table_selection(&mut self.cloud_table, rows);
    }

    fn on_cloud_size(&mut self, job_id: String, size: crate::ui::cloud::Size) {
        self.cloud_busy.remove(&format!("size:{job_id}"));
        self.cloud_sizes.insert(job_id, size);
        if self.status_message.as_deref().is_some_and(|m| m.starts_with("Measuring the bucket")) && !self.cloud_busy.iter().any(|k| k.starts_with("size:")) {
            self.status_message = None;
        }
    }

    fn on_preflight(&mut self, host: usize, repository: &str, password_file: &str, result: crate::restic::Preflight, file_note: Option<String>) {
        // Only the answer to the question still asked is kept: a result for edited values is dropped.
        if let Some(check) = self.backup_preflight.as_mut().filter(|c| c.host == host && c.repository == repository && c.password_file == password_file && c.result.is_none()) {
            check.result = Some(result);
            check.file_note = file_note;
        }
    }

    /// The destination's snapshots for a held replication save: nothing to destroy goes on to the usual save
    /// confirmation, otherwise the confirmation names what would go; a failed listing hands the form back.
    fn on_prune_preview(&mut self, host: usize, dataset: &str, result: Result<Vec<crate::zfs::replication::SnapInfo>>) {
        if !self.prune_check.as_ref().is_some_and(|c| c.host == host && c.dest_dataset == dataset) {
            return;
        }
        let Some(check) = self.prune_check.take() else { return };
        self.status_message = None;
        if self.modal.is_some() {
            self.set_error("The replication job was not saved: another dialog was opened while its destination was checked".to_string());
            return;
        }
        let listing = match result {
            Ok(l) => l,
            Err(e) => {
                self.set_error(format!("Cannot save: the snapshots on {} could not be listed: {e}", check.host_name));
                self.modal = Some(check.form);
                return;
            }
        };
        let doomed = crate::zfs::replication::prune_preview(&listing, &check.prefix, check.keep);
        let crate::ui::modal::MutationCall::SaveJob { job, .. } = &check.action.call else { return };
        if doomed.is_empty() {
            if let Some(action) = self.confirm_job_save(check.form, check.action) {
                self.pending_mutations.push(action);
            }
            return;
        }
        let own = listing.iter().filter(|s| crate::daemon::snapshot::is_own(&check.prefix, &s.name)).count();
        let old = match &check.form {
            Modal::Form { kind: crate::ui::modal::FormKind::ReplicateJob { existing, .. }, .. } => existing.as_deref(),
            _ => None,
        };
        let tail = super::forms::job_save_prompt(old, job, None).unwrap_or_else(|| "Save this job? (y/n)".to_string());
        let prompt = format!("{}\n\n{tail}", super::forms::prune_prompt(&check.host_name, &check.dest_dataset, &doomed, own + 1 - doomed.len()));
        self.confirm_restore = Some((prompt.clone(), check.form));
        self.modal = Some(Modal::Confirm { choice: None, prompt, action: check.action });
    }

    fn on_pool_names(&mut self, host: usize, result: Result<Vec<String>>) {
        let current = self.focused_form_value();
        let waiting = |p: &PickerState| p.load == crate::ui::picker::PickerLoad::Loading && p.pool_host == Some(host);
        if let Some(p) = self.picker.as_mut().filter(|p| waiting(p)) {
            match result {
                Ok(names) => {
                    p.selected = names.iter().position(|n| *n == current.trim()).unwrap_or(0);
                    p.note = names.is_empty().then(|| "no pools on this host".to_string());
                    p.values = names;
                    p.load = crate::ui::picker::PickerLoad::Ready;
                }
                Err(e) => p.load = crate::ui::picker::PickerLoad::Failed(e.to_string()),
            }
        }
    }

    fn on_host_health(&mut self, name: String, probe: crate::zfs::host_health::HostProbe) {
        // A result for a host that was removed or renamed while the probe ran is dropped.
        if self.hosts.contains(&name) {
            let state = self.host_probes.entry(name).or_default();
            state.in_flight = false;
            if let crate::zfs::host_health::HostProbe::Reached { kind: Some(k), .. } = &probe {
                state.kind = Some(k.clone());
            }
            if let crate::zfs::host_health::HostProbe::Reached { os: Some(os), os_name, .. } = &probe {
                state.os = Some(*os);
                state.os_name.clone_from(os_name);
            }
            // Numbers from before the host stopped answering must not stay on screen.
            if !matches!(probe, crate::zfs::host_health::HostProbe::Reached { .. }) {
                state.resources.clear();
            }
            state.last = Some(probe);
            state.finished_at = Some(Instant::now());
        }
    }

    fn on_host_resources(&mut self, name: String, resources: Option<crate::zfs::host_resources::Resources>, offset: Option<i32>) {
        if !self.hosts.contains(&name) {
            return;
        }
        let state = self.host_probes.entry(name).or_default();
        if offset.is_some() {
            state.utc_offset_secs = offset;
        }
        match resources {
            Some(r) => {
                state.resources.push(r);
                if state.resources.len() > 2 {
                    state.resources.remove(0);
                }
            }
            None => state.resources.clear(),
        }
    }

    fn on_host_probe(&mut self, name: String, target: String, result: Result<()>) {
        self.pending_probes.remove(&name.to_lowercase());
        match result {
            Ok(()) => {
                self.pending_new_hosts.push((name, target));
                self.status_message = None;
            }
            Err(e) => {
                self.new_host_tags.remove(&name.to_lowercase());
                self.set_error(format!("Could not add host '{name}': {e}"));
            }
        }
    }

    fn on_alerts_preview(&mut self, text: String) {
        self.alerts_preview_busy = false;
        if self.modal.is_some() || self.picker.is_some() {
            // Never replace something the operator is typing: keep it for the next `p`.
            self.alerts_preview_ready = Some(text);
            self.status_message = Some("The report preview is ready: press p on the Alerts screen".to_string());
        } else {
            self.status_message = None;
            self.modal = Some(Modal::Info { title: "Report preview (nothing sent)".to_string(), body: text });
        }
    }

    fn on_mutation_done(&mut self, host: usize, section: Section, result: Result<()>, refreshes_needed: &mut Vec<Section>) {
        if section == Section::Alerts {
            self.alerts_test_busy = false;
        }
        // The Alerts calls are local to the hub: their result matters whichever host is selected now.
        if host != self.selected_host && !matches!(section, Section::Alerts | Section::Settings) {
            return;
        }
        if section == Section::BootEnvs {
            self.bootenv_busy = None;
            if result.is_err() {
                self.bootenv_pin = None;
            }
        }
        let note = if matches!(section, Section::Alerts | Section::Settings) { self.done_note.take() } else { None };
        match result {
            Ok(()) => {
                self.status_message = note;
                if section == Section::Settings {
                    self.apply_saved_settings();
                }
                refreshes_needed.push(section);
                if section == Section::Vdevs {
                    // A disk going offline/online/being replaced
                    // changes the parent pool's health too —
                    // the one deliberate two-section exception.
                    refreshes_needed.push(Section::Pools);
                }
                if section == Section::Vdevs || section == Section::Pools {
                    // Both changing a pool's vdev tree
                    // (add/replace vdev) and creating/destroying
                    // a pool outright change which disks are
                    // claimed — the Disks section must catch up,
                    // and any selection made against the
                    // pre-mutation disk list is stale now.
                    refreshes_needed.push(Section::Disks);
                    self.selected_disks.clear();
                }
            }
            Err(e) => self.set_error(e.to_string()),
        }
    }
}
