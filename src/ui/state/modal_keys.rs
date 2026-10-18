//! Keys while a modal is open: the confirmations, and the forms with their submit paths.

use super::forms::{
    build_backup_job_action, build_bootenv_action, build_job_action, build_mirror_job_action, build_notify_action, build_pending_action, build_replicate_job_action, build_smb_action, build_snapshot_job_action, field_value, form_problem,
    irreversible_prompt,
};
use super::{App, Instant, Modal, MutationCall, PendingAction};
use crate::ui::modal::{FormField, FormKind};
use crate::ui::picker::build_picker_state;
use crossterm::event::KeyCode;

/// The pieces of an open `Modal::Form`, taken apart while one key is handled and put back with `into_modal`.
struct FormParts {
    title: String,
    fields: Vec<FormField>,
    focused: usize,
    description: Option<String>,
    kind: FormKind,
}

impl FormParts {
    fn into_modal(self) -> Modal {
        Modal::Form { title: self.title, fields: self.fields, focused: self.focused, description: self.description, kind: self.kind }
    }
}

impl App {
    /// Routes a key press to the currently open modal. Returns
    /// `Some(action)` when the modal was submitted/confirmed — the
    /// caller (event loop) is responsible for looking up the target
    /// host's runner and calling `dispatch_mutation`. Returns `None`
    /// for a cancel, an in-progress edit, or an unrecognized key (which
    /// leaves the modal open rather than silently dismissing it).
    /// No-ops (returns `None`, does nothing) if no modal is open.
    pub fn handle_modal_key(&mut self, key: KeyCode) -> Option<PendingAction> {
        let modal = self.modal.take()?;
        match modal {
            Modal::Confirm { prompt, action, choice } => self.confirm_key(key, prompt, action, choice),
            Modal::ConfirmTyped { prompt, expected, input, action } => self.confirm_typed_key(key, prompt, expected, input, action),
            Modal::Form { title, fields, focused, description, kind } => self.form_key(key, FormParts { title, fields, focused, description, kind }),
            Modal::Info { .. } => None,
        }
    }

    // Two steps for both outcomes: `y` / `n` / Esc only select, Enter applies the selection.
    // Enter with nothing selected, and every other key, change nothing.
    fn confirm_key(&mut self, key: KeyCode, prompt: String, action: PendingAction, choice: Option<bool>) -> Option<PendingAction> {
        match (key, choice) {
            // An add whose layout the host has not shown yet waits: the question is about what the pool will look like.
            (KeyCode::Enter, Some(true)) if matches!(action.call, MutationCall::AddVdev { .. }) && prompt.contains(super::forms::CHECKING_LAYOUT) => {
                self.status_message = Some("Waiting for the host to show the resulting layout; press Enter again in a moment".to_string());
                self.modal = Some(Modal::Confirm { prompt, action, choice: Some(true) });
                None
            }
            (KeyCode::Enter, Some(true)) => {
                self.confirm_restore = None;
                Some(action)
            }
            (KeyCode::Enter, Some(false)) => {
                // A confirmation that came from a form hands the form back, values intact.
                if let Some((tag, form)) = self.confirm_restore.take() {
                    if tag == prompt {
                        self.modal = Some(form);
                    }
                }
                None
            }
            (KeyCode::Char('y' | 'Y'), _) => {
                self.modal = Some(Modal::Confirm { prompt, action, choice: Some(true) });
                None
            }
            (KeyCode::Char('n' | 'N') | KeyCode::Esc, _) => {
                self.modal = Some(Modal::Confirm { prompt, action, choice: Some(false) });
                None
            }
            _ => {
                self.modal = Some(Modal::Confirm { prompt, action, choice });
                None
            }
        }
    }

    fn confirm_typed_key(&mut self, key: KeyCode, prompt: String, expected: String, mut input: String, action: PendingAction) -> Option<PendingAction> {
        match key {
            KeyCode::Esc => return None,
            KeyCode::Enter if input == expected => return Some(action),
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => input.push(c),
            _ => {}
        }
        self.modal = Some(Modal::ConfirmTyped { prompt, expected, input, action });
        None
    }

    /// Puts the form back on screen, unchanged apart from what the caller already did to it.
    fn reopen(&mut self, form: FormParts) -> Option<PendingAction> {
        self.modal = Some(form.into_modal());
        None
    }

    /// Shows `msg` and puts the form back with the first field focused, so nothing typed is lost.
    fn reject(&mut self, mut form: FormParts, msg: String) -> Option<PendingAction> {
        self.set_error(msg);
        form.focused = 0;
        self.reopen(form)
    }

    fn form_key(&mut self, key: KeyCode, mut form: FormParts) -> Option<PendingAction> {
        match key {
            KeyCode::Esc => None,
            KeyCode::Tab | KeyCode::Down => {
                form.focused = (form.focused + 1) % form.fields.len();
                self.reopen(form)
            }
            KeyCode::BackTab | KeyCode::Up => {
                form.focused = if form.focused == 0 { form.fields.len() - 1 } else { form.focused - 1 };
                self.reopen(form)
            }
            KeyCode::Enter => self.submit_form(form),
            KeyCode::Backspace => {
                if let Some(field) = form.fields.get_mut(form.focused) {
                    field.value.pop();
                }
                self.reopen(form)
            }
            KeyCode::Char(c) => {
                if let Some(field) = form.fields.get_mut(form.focused) {
                    field.value.push(c);
                }
                self.reopen(form)
            }
            KeyCode::F(5) if matches!(form.kind, FormKind::MirrorJob { .. }) => {
                self.request_mirror_check(field_value(&form.fields, 0).trim(), field_value(&form.fields, 5).trim());
                self.reopen(form)
            }
            KeyCode::F(5) if matches!(form.kind, FormKind::BackupJob { .. }) => {
                self.request_backup_preflight(field_value(&form.fields, 0).trim(), field_value(&form.fields, 5).trim(), field_value(&form.fields, 6).trim());
                self.reopen(form)
            }
            KeyCode::F(2) => {
                self.open_form_picker(&form);
                self.reopen(form)
            }
            _ => self.reopen(form),
        }
    }

    /// F2: opens the picker for the focused field, if it has one.
    fn open_form_picker(&mut self, form: &FormParts) {
        let FormParts { fields, focused, kind, .. } = form;
        let Some(mut picker) = build_picker_state(kind, *focused, field_value(fields, *focused)) else { return };
        if let Some((host, _)) = picker.account {
            self.open_account_picker(&mut picker, host, field_value(fields, *focused), Instant::now());
        }
        if picker.pool_host.is_some() {
            // The destination dataset lists the *destination* host's datasets.
            let host_field = if matches!(kind, FormKind::ReplicateJob { .. }) && *focused == 4 { 3 } else { 0 };
            // The backup form's Source field lists the snapshots of the form's dataset.
            let snapshots_of = (matches!(kind, FormKind::BackupJob { .. } | FormKind::MirrorJob { .. }) && *focused == 3).then(|| field_value(fields, 1).trim().to_string());
            self.open_pool_picker(&mut picker, kind, field_value(fields, host_field), snapshots_of);
        }
        self.picker = Some(picker);
    }

    /// Enter: moves to the next field, and on the last one submits the form.
    fn submit_form(&mut self, mut form: FormParts) -> Option<PendingAction> {
        if form.focused + 1 < form.fields.len() {
            form.focused += 1;
            return self.reopen(form);
        }
        match form.kind {
            FormKind::NotifySettings => self.submit_notify_form(form),
            FormKind::Setting { .. } => self.submit_setting_form(form),
            FormKind::AddHost => self.submit_add_host_form(form),
            FormKind::Job { .. } | FormKind::SnapshotJob { .. } | FormKind::BackupJob { .. } | FormKind::MirrorJob { .. } | FormKind::ReplicateJob { .. } => self.submit_job_form(form),
            FormKind::CreateBootEnv { .. } | FormKind::RenameBootEnv { .. } => match build_bootenv_action(&form.kind, &form.fields) {
                Ok(action) => {
                    if let MutationCall::RenameBootEnv { to, .. } = &action.call {
                        self.bootenv_pin = Some(to.clone());
                    }
                    Some(action)
                }
                Err(msg) => self.reject(form, msg),
            },
            FormKind::EditDatasetProperty { ref key, .. } if crate::zfs::quota::KEYS.contains(&key.as_str()) => self.submit_quota_form(form),
            FormKind::EditDatasetProperty { ref key, .. } if key == "special_small_blocks" => self.submit_small_blocks_form(form),
            FormKind::ShareSmb { .. } => match build_smb_action(&form.kind, &form.fields, &self.shares) {
                Ok(action) => Some(action),
                Err(msg) => self.reject(form, msg),
            },
            _ => self.submit_plain_form(form),
        }
    }

    /// The guided form for `quota`, `refquota`, `reservation` and `refreservation`: the size is parsed and checked against what
    /// the dataset uses and the pool has before anything is sent; a change that leaves little headroom needs y then Enter.
    fn submit_quota_form(&mut self, form: FormParts) -> Option<PendingAction> {
        use crate::zfs::quota::{check, parse_size, Tier};
        let FormKind::EditDatasetProperty { host, dataset, key } = form.kind.clone() else { return None };
        let new = match parse_size(field_value(&form.fields, 0)) {
            // ZFS reads 0 as "no limit" for all four properties.
            Ok(new) => new.filter(|n| *n > 0),
            Err(msg) => return self.reject(form, msg),
        };
        // The facts must be the form's own dataset's: a refresh may have moved the selection while the form was open.
        if self.dataset_properties.is_empty() || self.dataset_properties_for.as_deref().is_some_and(|d| d != dataset) {
            return self.reject(form, "the dataset's properties are not loaded yet: press r and try again".to_string());
        }
        let facts = super::forms::quota_facts(&self.dataset_properties);
        let tier = match check(&key, new, &facts) {
            Ok(tier) => tier,
            Err(msg) => return self.reject(form, msg),
        };
        let value = new.map_or_else(|| "none".to_string(), |n| n.to_string());
        let action = PendingAction { host, section: crate::ui::Section::DatasetProperties, call: MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: key.clone(), value: value.clone() } };
        // Every limit change is confirmed (y then Enter); the text says more when it is the risky kind.
        let why = match (tier, key.ends_with("reservation")) {
            (Tier::TwoKey, true) => "This takes a large share of the pool's free space away from everything else.",
            (Tier::TwoKey, false) => "That leaves little room: writes to this dataset may start to fail soon.",
            (Tier::Direct, _) if new.is_none() => "The limit is removed.",
            (Tier::Direct, _) => "The new limit applies at once.",
        };
        let shown = new.map_or_else(String::new, |n| format!(" ({})", crate::zfs::quota::format_size(n)));
        let prompt = format!("On {}:\n  zfs set {key}={value}{shown} {dataset}\n\n{why} Set it? (y/n)", self.hosts.get(host).map_or("?", String::as_str));
        self.confirm_restore = Some((prompt.clone(), form.into_modal()));
        self.modal = Some(Modal::Confirm { choice: None, prompt, action });
        None
    }

    /// `special_small_blocks`: blocks this size and smaller of the dataset's files go to the pool's special vdev. A value that
    /// reaches the dataset's recordsize puts all of its data there, which the question says.
    fn submit_small_blocks_form(&mut self, form: FormParts) -> Option<PendingAction> {
        let FormKind::EditDatasetProperty { host, dataset, key } = form.kind.clone() else { return None };
        let bytes = match crate::zfs::vdev_role::parse_small_blocks(field_value(&form.fields, 0)) {
            Ok(n) => n,
            Err(msg) => return self.reject(form, msg),
        };
        let recordsize = self.dataset_properties.iter().find(|p| p.name == "recordsize").and_then(|p| p.value.parse::<u64>().ok()).unwrap_or(0);
        let why = if bytes > 0 && recordsize > 0 && bytes >= recordsize {
            "This is at least the recordsize: ALL of this dataset's data goes to the special vdev, and fills it. Only new writes are affected."
        } else if bytes == 0 {
            "New writes put only metadata on the special vdev."
        } else {
            "New writes of blocks this size and smaller go to the special vdev; existing data stays where it is."
        };
        let action = PendingAction { host, section: crate::ui::Section::DatasetProperties, call: MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: key.clone(), value: bytes.to_string() } };
        let prompt = format!("On {}:\n  zfs set {key}={bytes} {dataset}\n\n{why} Set it? (y/n)", self.hosts.get(host).map_or("?", String::as_str));
        self.confirm_restore = Some((prompt.clone(), form.into_modal()));
        self.modal = Some(Modal::Confirm { choice: None, prompt, action });
        None
    }

    fn submit_notify_form(&mut self, form: FormParts) -> Option<PendingAction> {
        match build_notify_action(&form.fields, &self.config_path) {
            Ok((mut action, prompt)) => {
                action.host = self.selected_host;
                // Declining returns to the form, not to nothing.
                self.confirm_restore = Some((prompt.clone(), form.into_modal()));
                self.modal = Some(Modal::Confirm { choice: None, prompt, action });
                None
            }
            Err(msg) => self.reject(form, msg),
        }
    }

    fn submit_setting_form(&mut self, form: FormParts) -> Option<PendingAction> {
        let FormKind::Setting { setting } = form.kind.clone() else { return None };
        let values: Vec<String> = form.fields.iter().map(|f| f.value.clone()).collect();
        match crate::ui::settings::build_changes(&setting, &values) {
            Ok(_) => {
                let prompt = format!("Save to config.toml?\n  {}\n(y/n)", crate::ui::settings::describe(&setting, &values));
                let action = PendingAction { host: self.selected_host, section: crate::ui::Section::Settings, call: MutationCall::SaveSettings { config_path: self.config_path.clone(), setting, values } };
                self.confirm_restore = Some((prompt.clone(), form.into_modal()));
                self.modal = Some(Modal::Confirm { choice: None, prompt, action });
                None
            }
            Err(msg) => self.reject(form, msg),
        }
    }

    fn submit_add_host_form(&mut self, form: FormParts) -> Option<PendingAction> {
        let name = field_value(&form.fields, 0).trim().to_string();
        let target = field_value(&form.fields, 1).trim().to_string();
        let name_lower = name.to_lowercase();
        let is_duplicate = self.hosts.iter().any(|h| h.eq_ignore_ascii_case(&name)) || self.pending_probes.contains(&name_lower);
        if name.is_empty() || target.is_empty() {
            return self.reopen(form);
        }
        if is_duplicate {
            self.set_error(format!("A host named '{name}' already exists or is already being added"));
            return self.reopen(form);
        }
        self.pending_probes.insert(name_lower.clone());
        if let Some(tag) = crate::config::clean_tag(field_value(&form.fields, 2)) {
            self.new_host_tags.insert(name_lower, tag);
        }
        self.begin_add_host(name, target);
        None
    }

    /// The four job forms: a scrub job saves at once, the others go through the save confirmation.
    fn submit_job_form(&mut self, form: FormParts) -> Option<PendingAction> {
        let now = chrono::Utc::now();
        let (built, confirm) = match form.kind {
            FormKind::Job { .. } => (build_job_action(&form.kind, &form.fields, &self.state_dir, now), false),
            FormKind::SnapshotJob { .. } => (build_snapshot_job_action(&form.kind, &form.fields, &self.state_dir, now), true),
            FormKind::BackupJob { .. } => (build_backup_job_action(&form.kind, &form.fields, &self.state_dir, now), true),
            FormKind::MirrorJob { .. } => (build_mirror_job_action(&form.kind, &form.fields, &self.state_dir, now), true),
            _ => (build_replicate_job_action(&form.kind, &form.fields, &self.state_dir, now), true),
        };
        match built {
            Ok(mut action) => {
                action.host = self.selected_host;
                let to_prune = matches!(&action.call, MutationCall::SaveJob { job, .. } if job.dest_keep.is_some());
                if to_prune {
                    self.request_prune_check(form.into_modal(), action);
                    None
                } else if confirm {
                    self.confirm_job_save(form.into_modal(), action)
                } else {
                    Some(action)
                }
            }
            Err(msg) => self.reject(form, msg),
        }
    }

    /// Every other form: what cannot be taken back is shown as the exact command and needs y then Enter.
    fn submit_plain_form(&mut self, form: FormParts) -> Option<PendingAction> {
        let Some(action) = build_pending_action(&form.kind, &form.fields) else {
            let msg = form_problem(&form.kind, &form.fields);
            return self.reject(form, msg);
        };
        if let Some(prompt) = irreversible_prompt(&self.hosts, &action) {
            let adding = match (&action.call, super::forms::add_vdev_request(&form.kind, &form.fields)) {
                (MutationCall::AddVdev { pool, vdev_spec }, Ok((_, _, role))) => Some((pool.clone(), vdev_spec.clone(), role)),
                _ => None,
            };
            let modal = form.into_modal();
            self.confirm_restore = Some((prompt.clone(), modal.clone()));
            // The host is asked what the layout would be; the confirm shows it when it answers.
            if let Some((pool, spec, role)) = adding {
                self.add_check_ids += 1;
                self.pending_add_checks.push(super::AddCheck { id: self.add_check_ids, host: action.host, pool, spec, role, form: modal });
            }
            self.modal = Some(Modal::Confirm { choice: None, prompt, action });
            return None;
        }
        Some(action)
    }
}
