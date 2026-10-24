//! Key handling for Jobs, Alerts, Shares and Boot Envs.
#![allow(clippy::wildcard_imports)]
use super::*;

impl App {
    /// The Jobs section (and the `J` job shortcuts of Pools and Datasets).
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    pub(super) fn key_jobs(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Char('J') if self.current_section() == Section::Pools && self.drilled_pool.is_none() && self.pane_focus == PaneFocus::Middle => {
                match self.selected_pool_name() {
                    Some(pool) => self.modal = Some(self.job_form(None, &pool)),
                    None => self.status_message = Some("Select a pool first, then press J to schedule a scrub for it".to_string()),
                }
            }
            KeyCode::Char('n') if self.current_section() == Section::Jobs => {
                self.modal = Some(self.job_form(None, ""));
            }
            KeyCode::Char('N') if self.current_section() == Section::Jobs => {
                self.modal = Some(self.snapshot_job_form(None, ""));
            }
            KeyCode::Char('B') if self.current_section() == Section::Jobs => {
                self.backup_preflight = None;
                self.modal = Some(self.backup_job_form(None));
            }
            KeyCode::Char('C') if matches!(self.current_section(), Section::Jobs | Section::Cloud) => {
                self.backup_preflight = None;
                self.modal = Some(self.mirror_job_form(None));
            }
            KeyCode::Char('R') if self.current_section() == Section::Jobs => {
                self.modal = Some(self.replicate_job_form(None));
            }
            KeyCode::Char('c') if self.current_section() == Section::Jobs => {
                if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)) {
                    if crate::jobs::is_cancellable(job) && self.jobs_view.is_running(&job.id) {
                        match crate::daemon::cancel::request(&self.state_dir.join("cancel"), &job.id) {
                            Ok(()) => self.status_message = Some(format!("Cancel requested for '{}'; it stops within a second or two", job.id)),
                            Err(e) => self.set_error(format!("Could not request the cancel: {e}")),
                        }
                    }
                }
            }
            KeyCode::Char('J') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => match self.selected_dataset_name() {
                Some(dataset) => self.modal = Some(self.snapshot_job_form(None, &dataset)),
                None => self.status_message = Some("Select a dataset first, then press J to schedule snapshots of it".to_string()),
            },
            KeyCode::Enter if self.current_section() == Section::Jobs => {
                if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)).cloned() {
                    self.modal = Some(match job.kind.as_str() {
                        crate::jobs::KIND_SNAPSHOT => self.snapshot_job_form(Some(job), ""),
                        crate::jobs::KIND_REPLICATE => self.replicate_job_form(Some(job)),
                        crate::jobs::KIND_BACKUP => {
                            self.backup_preflight = None;
                            self.backup_job_form(Some(job))
                        }
                        crate::jobs::KIND_MIRROR => {
                            self.backup_preflight = None;
                            self.mirror_job_form(Some(job))
                        }
                        _ => self.job_form(Some(job.clone()), job.pool.as_deref().unwrap_or("")),
                    });
                }
            }
            KeyCode::Char(' ') if self.current_section() == Section::Jobs => {
                if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)) {
                    let enable = !job.enabled;
                    let as_enabled = crate::jobs::Job { enabled: true, ..job.clone() };
                    if let Some(other) = enable.then(|| crate::jobs::clashing_job(&self.jobs_view.jobs, &as_enabled)).flatten() {
                        let why = crate::jobs::clash_reason(other, &as_enabled).unwrap_or_default();
                        self.set_error(format!("Cannot enable '{}': it clashes with enabled job '{}': {why}", job.id, other.id));
                        return true;
                    }
                    self.status_message = Some(format!("{} job '{}'…", if enable { "Enabling" } else { "Disabling" }, job.id));
                    self.pending_mutations.push(PendingAction {
                        host: self.selected_host,
                        section: Section::Jobs,
                        call: MutationCall::SetJobEnabled { state_dir: self.state_dir.clone(), id: job.id.clone(), enabled: enable },
                    });
                }
            }
            KeyCode::Char('d') if self.current_section() == Section::Jobs => {
                if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)) {
                    let prompt = format!("Delete job '{}'? It stops running; its past runs stay in the history. (y/n)", job.id);
                    let call = MutationCall::DeleteJob { state_dir: self.state_dir.clone(), id: job.id.clone() };
                    self.modal = Some(Modal::Confirm { choice: None, prompt, action: PendingAction { host: self.selected_host, section: Section::Jobs, call } });
                }
            }
            _ => return false,
        }
        true
    }

    /// The Alerts section, Up/Down in the Jobs list, and the Shares section.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    pub(super) fn key_alerts_and_shares(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Char('e') if self.current_section() == Section::Alerts && self.modal.is_none() && self.picker.is_none() => {
                if let Some(form) = self.alerts_form() {
                    self.modal = Some(form);
                } else {
                    let problem = self.alerts_view.problem.clone().unwrap_or_default();
                    self.set_error(format!("{problem}: fix config.toml before editing here"));
                }
            }
            KeyCode::Char('t') if self.current_section() == Section::Alerts && self.modal.is_none() && self.picker.is_none() => self.ask_to_send_test(),
            KeyCode::Char('p') if self.current_section() == Section::Alerts && self.modal.is_none() && self.picker.is_none() => self.start_alerts_preview(),
            KeyCode::Down if self.current_section() == Section::Alerts => {
                let last = self.alerts_view.events.len().saturating_sub(1);
                self.alerts_view.scroll = (self.alerts_view.scroll + 1).min(last);
            }
            KeyCode::Up if self.current_section() == Section::Alerts => {
                self.alerts_view.scroll = self.alerts_view.scroll.saturating_sub(1);
            }
            KeyCode::Down if self.current_section() == Section::Settings => {
                move_table_selection(&mut self.settings_table, self.settings_view.rows.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Settings => {
                move_table_selection(&mut self.settings_table, self.settings_view.rows.len(), false);
            }
            KeyCode::Enter if self.current_section() == Section::Settings && self.modal.is_none() && self.picker.is_none() => {
                if let Some(problem) = self.settings_view.problem.clone() {
                    self.set_error(format!("{problem}: fix config.toml before editing here"));
                } else if let Some(form) = self.settings_form() {
                    self.modal = Some(form);
                }
            }
            KeyCode::Down if self.current_section() == Section::Dashboard => {
                let lines = self.dashboard().attention.len();
                move_table_selection(&mut self.dashboard_table, lines, true);
            }
            KeyCode::Up if self.current_section() == Section::Dashboard => {
                let lines = self.dashboard().attention.len();
                move_table_selection(&mut self.dashboard_table, lines, false);
            }
            // The Dashboard shows, the section that owns the problem fixes it; an unprotected-data line opens the form that makes the missing job.
            KeyCode::Enter if self.current_section() == Section::Dashboard => {
                let dashboard = self.dashboard();
                let item = self.dashboard_table.selected().and_then(|i| dashboard.attention.get(i).cloned());
                // A named protection line goes straight to its fix; the "and N more" line opens the list (Jobs shows none of them).
                if let Some(gap) = item.as_ref().and_then(|item| dashboard.protection_fix(item)).cloned() {
                    self.fix_unprotected(&gap);
                } else if let Some(popup) = item.as_ref().and_then(|item| dashboard.protection_popup(item)) {
                    self.modal = Some(Modal::Choose { title: crate::ui::dashboard::PROTECTION_POPUP_TITLE.to_string(), note: popup.note, rows: popup.rows, selected: popup.selected, then: crate::ui::modal::ChooseThen::Unprotected(popup.gaps), look: popup.look });
                } else if let Some(item) = item {
                    if let Some(position) = self.sections.iter().position(|s| *s == item.goto) {
                        self.selected_section = position;
                        self.pane_focus = PaneFocus::Middle;
                    }
                    match (item.goto, item.target) {
                        (Section::Hosts | Section::Pools | Section::Disks | Section::Shares, Some(name)) => {
                            if let Some(index) = self.hosts.iter().position(|h| *h == name) {
                                if index != self.selected_host {
                                    self.selected_host = index;
                                    // What the previous host's screens loaded must not look like this host's.
                                    self.clear_host_data();
                                }
                            }
                        }
                        (Section::Jobs, Some(id)) => {
                            if let Some(index) = self.jobs_view.jobs.iter().position(|j| j.id == id) {
                                self.jobs_table.select(Some(index));
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Down if self.current_section() == Section::Cloud => {
                let rows = self.cloud_rows().len();
                move_table_selection(&mut self.cloud_table, rows, true);
            }
            KeyCode::Up if self.current_section() == Section::Cloud => {
                let rows = self.cloud_rows().len();
                move_table_selection(&mut self.cloud_table, rows, false);
            }
            KeyCode::Char('s') if self.current_section() == Section::Cloud => self.request_cloud_sizes(),
            KeyCode::Char('n') if self.current_section() == Section::Cloud && self.modal.is_none() && self.picker.is_none() => {
                let rows = crate::ui::cloud_guide::PROVIDERS.iter().map(|p| p.name.to_string()).collect();
                let note = "Pick a provider: Enter shows the steps.\nzfsctl never creates the remote or sees its keys.".to_string();
                self.modal = Some(Modal::Choose { title: "Set up a cloud provider".to_string(), note, rows, selected: 0, then: crate::ui::modal::ChooseThen::Provider, look: crate::ui::modal::ChooseLook::default() });
            }
            KeyCode::F(5) if self.current_section() == Section::Cloud => self.request_cloud_check(),
            KeyCode::Down if self.current_section() == Section::Jobs => {
                move_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Jobs => {
                move_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len(), false);
            }
            KeyCode::Enter if self.current_section() == Section::Shares && self.shares_editable() => {
                if let Some(i) = self.shares_table.selected() {
                    if let Some(share) = self.shares.get(i) {
                        self.modal = Some(share_edit_modal(share, self.selected_host));
                    }
                }
            }
            KeyCode::Char('d') if self.current_section() == Section::Shares && self.shares_editable() => {
                if let Some(i) = self.shares_table.selected() {
                    if let Some(share) = self.shares.get(i) {
                        let (prompt, call) = match share {
                            crate::zfs::share::Share::Nfs { dataset, .. } => (
                                format!("Remove NFS share for '{dataset}'? (y/n)"),
                                MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: "sharenfs".to_string(), value: "off".to_string() },
                            ),
                            crate::zfs::share::Share::Smb { dataset, name, .. } => (
                                format!("Remove SMB share '{name}' for '{dataset}'? Connected clients lose access. (y/n)"),
                                MutationCall::RemoveSmbShare { dataset: dataset.clone() },
                            ),
                        };
                        self.modal = Some(Modal::Confirm { choice: None, prompt, action: PendingAction { host: self.selected_host, section: Section::Shares, call } });
                    }
                }
            }
            // A change to a boot environment is still running: do nothing else to the section until it has finished.
            _ => return false,
        }
        true
    }

    /// The Boot Envs section.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    pub(super) fn key_bootenvs(&mut self, key: KeyCode) -> bool {
        match key {
            // A key `beadm` cannot serve (see `ui::support::bootenv_key_offered`): nothing happens, and it is not advertised.
            KeyCode::Char(c) if self.current_section() == Section::BootEnvs && !self.bootenv_key_offered(c) => {}
            KeyCode::Char('n' | 'a' | 't' | 'T' | 'R' | 'm' | 'u' | 'd' | 'D') if self.current_section() == Section::BootEnvs && self.bootenv_busy_name().is_some() => {
                let name = self.bootenv_busy_name().unwrap_or_default().to_string();
                self.status_message = Some(format!("Still working on '{name}'… wait for it to finish"));
            }
            KeyCode::Char('n') if self.current_section() == Section::BootEnvs && self.bootenvs_unavailable.is_none() => {
                let existing: Vec<String> = self.bootenvs.iter().map(|b| b.name.clone()).collect();
                self.modal = Some(Modal::Form {
                    title: "New boot environment".to_string(),
                    fields: vec![
                        FormField { label: "Name (letters, digits, _ - . :)".to_string(), value: String::new() },
                        FormField { label: "Clone from (F2 to pick; blank = the running boot environment)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    description: None,
                    kind: FormKind::CreateBootEnv { host: self.selected_host, existing },
                });
            }
            KeyCode::Char('a') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_activate()).map(|b| b.name.clone()) else { return true };
                let prompt = format!("Make '{name}' the boot environment for every future boot? (y/n)");
                let call = MutationCall::ActivateBootEnv { name, temporary: false };
                self.modal = Some(Modal::Confirm { choice: None, prompt, action: PendingAction { host: self.selected_host, section: Section::BootEnvs, call } });
            }
            KeyCode::Char('t') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_activate_temporarily()).map(|b| b.name.clone()) else { return true };
                let prompt = format!("Boot '{name}' on the NEXT boot only? Later boots go back to the usual boot environment. (y/n)");
                let call = MutationCall::ActivateBootEnv { name, temporary: true };
                self.modal = Some(Modal::Confirm { choice: None, prompt, action: PendingAction { host: self.selected_host, section: Section::BootEnvs, call } });
            }
            KeyCode::Char('T') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_clear_temporary()).map(|b| b.name.clone()) else { return true };
                self.status_message = Some(format!("Clearing the next-boot activation of '{name}'…"));
                let call = MutationCall::ClearTemporaryBootEnv { name };
                self.queue_bootenv_change(call);
            }
            KeyCode::Char('R') if self.current_section() == Section::BootEnvs => {
                let Some(from) = self.selected_bootenv().map(|b| b.name.clone()) else { return true };
                let existing: Vec<String> = self.bootenvs.iter().map(|b| b.name.clone()).collect();
                self.modal = Some(Modal::Form {
                    title: format!("Rename boot environment '{from}'"),
                    fields: vec![FormField { label: "New name (letters, digits, _ - . :)".to_string(), value: String::new() }],
                    focused: 0,
                    description: None,
                    kind: FormKind::RenameBootEnv { host: self.selected_host, from, existing },
                });
            }
            KeyCode::Char('m') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_mount()).map(|b| b.name.clone()) else { return true };
                self.status_message = Some(format!("Mounting '{name}'…"));
                let call = MutationCall::MountBootEnv { name };
                self.queue_bootenv_change(call);
            }
            KeyCode::Char('u') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_unmount()).map(|b| b.name.clone()) else { return true };
                self.status_message = Some(format!("Unmounting '{name}'…"));
                let call = MutationCall::UnmountBootEnv { name };
                self.queue_bootenv_change(call);
            }
            KeyCode::Char(key @ ('d' | 'D')) if self.current_section() == Section::BootEnvs => {
                let Some(be) = self.selected_bootenv() else { return true };
                let name = be.name.clone();
                if !be.can_destroy() {
                    self.status_message = Some(format!("'{name}' cannot be destroyed: it is running, set for the next boot, or marked temporary (T clears that)"));
                    return true;
                }
                let origin = key == 'D';
                let also = if origin { " and its origin snapshot" } else { "" };
                self.modal = Some(Modal::ConfirmTyped {
                    prompt: format!("Type the boot environment name to destroy '{name}'{also}:"),
                    expected: name.clone(),
                    input: String::new(),
                    action: PendingAction { host: self.selected_host, section: Section::BootEnvs, call: MutationCall::DestroyBootEnv { name, origin } },
                });
            }
            _ => return false,
        }
        true
    }

    /// The boot environment a change is in flight for, unless that marker is stale.
    pub(super) fn bootenv_busy_name(&self) -> Option<&str> {
        const STALE: Duration = Duration::from_mins(2);
        self.bootenv_busy.as_ref().filter(|(_, since)| since.elapsed() < STALE).map(|(name, _)| name.as_str())
    }

    /// Queues a change to run on the next loop tick and marks the section busy right away, so a second key
    /// pressed before it is dispatched cannot queue a duplicate.
    pub(super) fn queue_bootenv_change(&mut self, call: MutationCall) {
        self.bootenv_busy = Some((bootenv_call_name(&call).to_string(), Instant::now()));
        self.pending_mutations.push(PendingAction { host: self.selected_host, section: Section::BootEnvs, call });
    }

    pub fn selected_bootenv(&self) -> Option<&crate::zfs::bootenv::BootEnv> {
        self.bootenvs_table.selected().and_then(|i| self.bootenvs.get(i))
    }
}
