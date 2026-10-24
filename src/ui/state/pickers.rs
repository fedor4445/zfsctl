//! The F2 pickers: the live-fetched lists (pools, datasets, accounts, remotes) and the keys of an open picker.
#![allow(clippy::wildcard_imports)]
use super::*;

impl App {
    /// Fills `picker` from a fresh cache entry for `host`, or leaves it
    /// `Loading` and queues one fetch (unless a non-stuck one is already in
    /// flight). `now` is a parameter so tests can age cache/in-flight entries.
    pub(super) fn open_account_picker(&mut self, picker: &mut PickerState, host: usize, current_value: &str, now: Instant) {
        if let Some((fetched_at, accounts)) = self.account_cache.get(&host) {
            if now.duration_since(*fetched_at) < ACCOUNT_CACHE_TTL {
                crate::ui::picker::fill_account_picker(picker, accounts, current_value);
                return;
            }
        }
        let stuck = self.account_fetch_in_flight.get(&host).is_none_or(|t| now.duration_since(*t) >= ACCOUNT_FETCH_STUCK_AFTER);
        if stuck {
            self.account_fetch_in_flight.insert(host, now);
            self.pending_account_fetch.push(host);
        }
    }

    /// Points the job form's Pool picker at the host named in the form's Host field and queues
    /// one fetch of its pool names; an unknown host name fails the picker right away.
    pub(super) fn open_pool_picker(&mut self, picker: &mut PickerState, kind: &FormKind, host_name: &str, wanted: Wanted) {
        let (hosts, mut what) = match kind {
            FormKind::Job { hosts, .. } => (hosts, crate::ui::picker::LiveNames::Pools),
            FormKind::SnapshotJob { hosts, .. } | FormKind::ReplicateJob { hosts, .. } | FormKind::BackupJob { hosts, .. } | FormKind::MirrorJob { hosts, .. } => (hosts, crate::ui::picker::LiveNames::Datasets),
            _ => return,
        };
        if host_name.trim() == crate::jobs::DEST_FILE {
            picker.load = crate::ui::picker::PickerLoad::Failed("the destination is a directory on the hub: type its path".to_string());
            return;
        }
        if let Wanted::Remotes(prefix) = &wanted {
            what = crate::ui::picker::LiveNames::Remotes { prefix: (*prefix).to_string() };
        }
        if let Wanted::Snapshots(dataset) = wanted {
            if !crate::jobs::valid_dataset(&dataset) {
                picker.load = crate::ui::picker::PickerLoad::Failed("enter the dataset first: the snapshots listed are that dataset's".to_string());
                return;
            }
            what = crate::ui::picker::LiveNames::Snapshots(dataset);
        }
        match hosts.iter().position(|h| h == host_name.trim()) {
            Some(host) => {
                picker.pool_host = Some(host);
                self.pending_pool_fetch.push((host, what));
            }
            None => picker.load = crate::ui::picker::PickerLoad::Failed(format!("unknown host '{}'", host_name.trim())),
        }
    }

    /// Lists the host's pools or datasets on a worker thread and reports them as `WorkerMsg::PoolNames`.
    pub fn fetch_pool_names(&mut self, runner: Arc<dyn CommandRunner>, host: usize, what: crate::ui::picker::LiveNames) {
        let tx = self.tx.clone();
        let host_name = self.hosts.get(host).cloned().unwrap_or_default();
        std::thread::spawn(move || {
            let result = match what {
                crate::ui::picker::LiveNames::Pools => crate::zfs::pool::list_pools(runner.as_ref()).map(|v| v.into_iter().map(|p| p.name).collect()),
                crate::ui::picker::LiveNames::Datasets => crate::zfs::dataset::list_datasets(runner.as_ref()).map(|v| v.into_iter().map(|d| d.name).collect()),
                crate::ui::picker::LiveNames::Snapshots(dataset) => {
                    crate::zfs::replication::list_snapshot_infos_or_empty(runner.as_ref(), &dataset).map(|v| crate::ui::picker::source_choices(&v.into_iter().map(|s| s.name).collect::<Vec<_>>()))
                }
                crate::ui::picker::LiveNames::Remotes { prefix } => match crate::ui::cloud::list_remotes(runner.as_ref(), &host_name) {
                    Ok(remotes) if remotes.is_empty() => Err(crate::error::AppError::Message("no rclone remotes on this host yet: in Cloud, press n for how to add one".to_string())),
                    Ok(remotes) => Ok(crate::ui::picker::remote_choices(&remotes, &prefix)),
                    Err(problem) => Err(crate::error::AppError::Message(problem)),
                },
            };
            let _ = tx.send(WorkerMsg::PoolNames(host, result));
        });
    }

    /// Reports a failed pool lookup without running one (the host index no longer resolves).
    pub fn fail_pool_fetch(&self, host: usize, err: crate::error::AppError) {
        let _ = self.tx.send(WorkerMsg::PoolNames(host, Err(err)));
    }

    /// Runs the account lookup on a worker thread and reports it as
    /// `WorkerMsg::Accounts`. Called by the event loop for each queued host
    /// (`App` cannot resolve a host index to a runner on its own).
    pub fn fetch_accounts(&mut self, runner: Arc<dyn CommandRunner>, host: usize) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::accounts::fetch_accounts(runner.as_ref());
            let _ = tx.send(WorkerMsg::Accounts(host, result));
        });
    }

    /// Reports a failed account lookup without running one (e.g. the host
    /// index no longer resolves), through the same channel a worker uses so
    /// the picker shows Failed and the in-flight marker clears.
    pub fn fail_account_fetch(&self, host: usize, err: crate::error::AppError) {
        let _ = self.tx.send(WorkerMsg::Accounts(host, Err(err)));
    }

    /// Handles a key while `self.picker` is open. Never returns a
    /// `PendingAction` -- a picker only ever edits the still-open
    /// form's focused field, it never submits anything itself.
    pub fn handle_picker_key(&mut self, key: KeyCode) {
        let Some(mut picker) = self.picker.take() else { return };
        if picker.load != crate::ui::picker::PickerLoad::Ready {
            // Loading / Failed: nothing to filter or pick; only Esc leaves.
            if key != KeyCode::Esc {
                self.picker = Some(picker);
            }
            return;
        }
        let filter_lower = picker.filter.to_lowercase();
        let filtered: Vec<String> = picker.values.iter().filter(|v| v.to_lowercase().contains(&filter_lower)).cloned().collect();
        match key {
            KeyCode::Esc => {}
            KeyCode::Up => {
                picker.selected = picker.selected.saturating_sub(1);
                self.picker = Some(picker);
            }
            KeyCode::Down => {
                if !filtered.is_empty() && picker.selected + 1 < filtered.len() {
                    picker.selected += 1;
                }
                self.picker = Some(picker);
            }
            KeyCode::Char(' ') if picker.multi_select => {
                if let Some(value) = filtered.get(picker.selected) {
                    if !picker.checked.insert(value.clone()) {
                        picker.checked.remove(value);
                    }
                }
                self.picker = Some(picker);
            }
            KeyCode::Char(' ') => {
                self.picker = Some(picker);
            }
            KeyCode::Enter => {
                if picker.multi_select {
                    let mut vals: Vec<String> = picker.checked.iter().cloned().collect();
                    vals.sort();
                    let new_value = vals.join(",");
                    if let Some(Modal::Form { fields, focused, .. }) = &mut self.modal {
                        if let Some(field) = fields.get_mut(*focused) {
                            field.value = new_value;
                        }
                    }
                } else if let Some(value) = filtered.get(picker.selected).cloned() {
                    if let Some(Modal::Form { fields, focused, kind, .. }) = &mut self.modal {
                        // The remote field keeps the bucket and folder that were typed after the remote's name.
                        let remote_field = *focused == 5 && matches!(kind, FormKind::MirrorJob { .. } | FormKind::BackupJob { .. });
                        if let Some(field) = fields.get_mut(*focused) {
                            let chosen = crate::ui::jobs::insert_text(&value);
                            field.value = if remote_field { crate::ui::picker::with_remote(&field.value, chosen) } else { chosen.to_string() };
                        }
                    }
                } else {
                    self.picker = Some(picker);
                }
            }
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.selected = 0;
                self.picker = Some(picker);
            }
            KeyCode::Char(c) => {
                picker.filter.push(c);
                picker.selected = 0;
                self.picker = Some(picker);
            }
            _ => {
                self.picker = Some(picker);
            }
        }
    }
}
