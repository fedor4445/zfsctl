//! Keys while a modal is open: the confirmations, and the forms with their submit paths.

use super::forms::{
    build_backup_job_action, build_bootenv_action, build_job_action, build_mirror_job_action, build_notify_action, build_pending_action, build_replicate_job_action, build_smb_action, build_snapshot_job_action, field_value, form_problem,
    build_create_encrypted, build_key_action, irreversible_prompt,
};
use super::{App, Instant, Modal, MutationCall, PaneFocus, PendingAction, Section};
use crate::ui::modal::{ChooseLook, ChooseThen, FormField, FormKind};
use crate::ui::picker::build_picker_state;
use crossterm::event::KeyCode;

/// The pieces of an open `Modal::Choose`, taken apart while one key is handled.
struct ChooseParts {
    title: String,
    note: String,
    rows: Vec<String>,
    selected: usize,
    then: ChooseThen,
    look: ChooseLook,
}

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
            Modal::Info { title, body } => self.info_key(key, title, body),
            Modal::Choose { title, note, rows, selected, then, look } => self.choose_key(key, ChooseParts { title, note, rows, selected, then, look }),
        }
    }

    /// A read-only pop-up scrolls with the navigation keys; any other key closes it.
    fn info_key(&mut self, key: KeyCode, title: String, body: String) -> Option<PendingAction> {
        const PAGE: u16 = 10;
        let last = u16::try_from(body.lines().count().saturating_sub(1)).unwrap_or(u16::MAX);
        let scroll = match key {
            KeyCode::Down => self.info_scroll.saturating_add(1),
            KeyCode::Up => self.info_scroll.saturating_sub(1),
            KeyCode::PageDown => self.info_scroll.saturating_add(PAGE),
            KeyCode::PageUp => self.info_scroll.saturating_sub(PAGE),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => {
                self.info_scroll = 0;
                return None;
            }
        };
        self.info_scroll = scroll.min(last);
        self.modal = Some(Modal::Info { title, body });
        None
    }

    /// A list to choose from: the arrows move, Esc closes, Enter acts on the row, and every other key does nothing.
    fn choose_key(&mut self, key: KeyCode, parts: ChooseParts) -> Option<PendingAction> {
        const PAGE: usize = 10;
        let ChooseParts { title, note, rows, selected, then, look } = parts;
        let last = rows.len().saturating_sub(1);
        let selected = match key {
            KeyCode::Esc => return None,
            KeyCode::Enter => {
                self.choose_row(selected, then);
                return None;
            }
            KeyCode::Down => (selected + 1).min(last),
            KeyCode::Up => selected.saturating_sub(1),
            KeyCode::PageDown => (selected + PAGE).min(last),
            KeyCode::PageUp => selected.saturating_sub(PAGE),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => selected,
        };
        self.modal = Some(Modal::Choose { title, note, rows, selected, then, look });
        None
    }

    /// The fix for one unprotected dataset: open the form that makes the missing job, ask which kind of copy, or go to the
    /// job that should be helping.
    pub(super) fn fix_unprotected(&mut self, gap: &crate::daemon::protection::Unprotected) {
        use crate::daemon::protection::{fix_for, Fix};
        match fix_for(gap, &self.jobs_view.jobs) {
            Fix::NewSnapshotJob => self.modal = Some(prefilled(self.snapshot_job_form(None, &gap.dataset), &gap.host, &gap.dataset)),
            Fix::ChooseCopy => {
                let rows = vec!["Replicate to another host".to_string(), "Back up with restic".to_string(), "Mirror to the cloud (rclone)".to_string()];
                let note = format!("{}:{} needs a copy on another machine.\nEnter opens the form for the kind you pick.", gap.host, gap.dataset);
                self.modal = Some(Modal::Choose { title: "Which copy?".to_string(), note, rows, selected: 0, then: ChooseThen::Copy { host: gap.host.clone(), dataset: gap.dataset.clone() }, look: ChooseLook::default() });
            }
            Fix::GoToJob(id) => {
                if let Some(position) = self.sections.iter().position(|s| *s == Section::Jobs) {
                    self.selected_section = position;
                    self.pane_focus = PaneFocus::Middle;
                }
                if let Some(index) = self.jobs_view.jobs.iter().position(|j| j.id == id) {
                    self.jobs_table.select(Some(index));
                }
            }
        }
    }

    /// What Enter does on a row: open the form that fixes the dataset, ask which kind of copy, or go to the job that should be helping.
    fn choose_row(&mut self, row: usize, then: ChooseThen) {
        match then {
            ChooseThen::Unprotected(gaps) => {
                if let Some(gap) = gaps.get(row) {
                    self.fix_unprotected(gap);
                }
            }
            ChooseThen::Copy { host, dataset } => {
                let form = match row {
                    0 => self.replicate_job_form(None),
                    1 => self.backup_job_form(None),
                    _ => self.mirror_job_form(None),
                };
                self.modal = Some(prefilled(form, &host, &dataset));
            }
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
            FormKind::RestoreFile { .. } => self.submit_restore_form(form),
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
            FormKind::EditDatasetProperty { ref key, .. } if key == "checksum" || key == "dedup" => self.submit_integrity_form(form),
            FormKind::CreateEncrypted { .. } => match build_create_encrypted(&form.kind, &form.fields, &self.encryption) {
                Ok(action) => {
                    let MutationCall::CreateEncryptedDataset { name, cipher, .. } = &action.call else { return None };
                    let prompt = format!(
                        "Type the dataset name to create the encrypted dataset '{name}' ({cipher}, passphrase key).\nIf you lose the passphrase the data CANNOT be recovered by anyone, including zfsctl, which stores nothing. Encryption cannot be added or removed later. Write the passphrase down somewhere safe first."
                    );
                    self.modal = Some(Modal::ConfirmTyped { prompt, expected: name.clone(), input: String::new(), action });
                    None
                }
                Err(msg) => {
                    for field in form.fields.iter_mut().skip(2) {
                        field.value.clear();
                    }
                    self.reject(form, msg)
                }
            },
            FormKind::LoadKey { .. } => match build_key_action(&form.kind, &form.fields) {
                Ok(action) => Some(action),
                Err(msg) => self.reject(form, msg),
            },
            FormKind::ChangeKey { .. } => match build_key_action(&form.kind, &form.fields) {
                Ok(action) => self.confirm_from_form(form, action),
                Err(msg) => {
                    // Typed blind, so a mismatch starts both over rather than leaving the user to guess which is wrong.
                    for field in &mut form.fields {
                        field.value.clear();
                    }
                    self.reject(form, msg)
                }
            },
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
        if self.dataset_properties.is_empty() || self.dataset_properties_for.as_deref() != Some(dataset.as_str()) {
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

    /// `checksum` and `dedup`: the value is checked against the list (and the host's OpenZFS version when it is known); turning
    /// the checksum off or dedup on asks for the dataset's name, every other change for y then Enter, each saying what it means.
    fn submit_integrity_form(&mut self, form: FormParts) -> Option<PendingAction> {
        use crate::zfs::integrity::{check_checksum, check_dedup, checksum_confirm, dedup_confirm, Confirm};
        let FormKind::EditDatasetProperty { host, dataset, key } = form.kind.clone() else { return None };
        if self.dataset_properties.is_empty() || self.dataset_properties_for.as_deref() != Some(dataset.as_str()) {
            return self.reject(form, "the dataset's properties are not loaded yet: press r and try again".to_string());
        }
        let host_name = self.hosts.get(host).cloned().unwrap_or_default();
        let version = self.storage_check.hosts.get(&host_name).and_then(|r| r.zfs_version.clone());
        let typed = field_value(&form.fields, 0);
        let new = match if key == "checksum" { check_checksum(typed, version.as_deref()) } else { check_dedup(typed, version.as_deref()) } {
            Ok(new) => new,
            Err(msg) => return self.reject(form, msg),
        };
        let current = self.dataset_properties.iter().find(|p| p.name == key).map(|p| p.value.clone()).unwrap_or_default();
        if new == current {
            return self.reject(form, format!("{key} is already {current}"));
        }
        let confirm = if key == "checksum" { checksum_confirm(&new) } else { dedup_confirm(&current, &new) };
        let action = PendingAction { host, section: crate::ui::Section::DatasetProperties, call: MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: key.clone(), value: new.clone() } };
        let command = format!("On {host_name}:\n  zfs set {key}={new} {dataset}\n\n");
        let ram = self.host_resources(&host_name).map(|(r, _)| r.physical);
        let text = match (key.as_str(), &confirm) {
            ("checksum", Confirm::Typed) => format!("{command}With the checksum off, ZFS stops checking the integrity of what is written to this dataset from now on: silent corruption will not be detected, and a scrub cannot repair it. Existing data keeps its old checksums.\nType the dataset name to confirm:"),
            ("checksum", Confirm::Plain) if current == "off" => format!("{command}Only data written from now on is checksummed with {new}. What was written while the checksum was off has none: a scrub cannot verify or repair it, only rewriting that data does. Set it? (y/n)"),
            ("checksum", Confirm::Plain) => format!("{command}Only data written from now on uses {new}; existing data keeps its old checksum. Run a scrub afterwards to verify the pool. Set it? (y/n)"),
            ("dedup", Confirm::Typed) => {
                let memory = ram.map_or_else(|| "this host's RAM is not known".to_string(), |r| format!("this host has {} of RAM", crate::zfs::disk::human_size(r)));
                let low = if ram.is_some_and(|r| r < 16 << 30) { " That is little for deduplication: expect it to be slow, or to stall, once the table outgrows memory." } else { "" };
                format!("{command}Deduplication keeps a table of every block in memory (a rule of thumb is 1 to 5 GB of RAM per TB of deduplicated data); {memory}.{low} Turning it off later does not undeduplicate what is already stored, and the table stays until that data is rewritten or deleted.\nType the dataset name to confirm:")
            }
            _ => format!("{command}The new setting applies to data written from now on; what is stored stays as it is. Set it? (y/n)"),
        };
        if confirm == Confirm::Typed {
            self.modal = Some(Modal::ConfirmTyped { prompt: text, expected: dataset, input: String::new(), action });
        } else {
            self.confirm_restore = Some((text.clone(), form.into_modal()));
            self.modal = Some(Modal::Confirm { choice: None, prompt: text, action });
        }
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
        match crate::ui::settings::build_changes_for(&self.config_path, &setting, &values) {
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
                if let Some(msg) = self.encrypted_source(&action) {
                    return self.reject(form, msg);
                }
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

    /// Why this job cannot be saved because its source is encrypted, when the loaded Datasets list of the job's host says so
    /// (the run-time guard catches what this list does not know). Restic backups re-encrypt by themselves and are allowed.
    pub(super) fn encrypted_source(&self, action: &PendingAction) -> Option<String> {
        let MutationCall::SaveJob { job, .. } = &action.call else { return None };
        if job.kind != crate::jobs::KIND_REPLICATE && job.kind != crate::jobs::KIND_MIRROR {
            return None;
        }
        // A raw replication sends an encrypted source as it is: nothing to refuse.
        if job.kind == crate::jobs::KIND_REPLICATE && job.raw {
            return None;
        }
        let dataset = job.dataset.as_deref()?;
        // A disabled job may be saved (to switch off one that now fails).
        (job.enabled && self.hosts.get(self.encryption_for) == Some(&job.host) && self.encryption.contains_key(dataset)).then(|| {
            if job.kind == crate::jobs::KIND_REPLICATE { crate::daemon::guard::not_allowed_replication(dataset) } else { crate::daemon::guard::not_allowed(dataset) }
        })
    }

    /// Puts the confirm for `action` over `form`; declining hands the form back with what was typed.
    fn confirm_from_form(&mut self, form: FormParts, action: PendingAction) -> Option<PendingAction> {
        let Some(prompt) = irreversible_prompt(&self.hosts, &action) else { return Some(action) };
        let modal = form.into_modal();
        self.confirm_restore = Some((prompt.clone(), modal));
        self.modal = Some(Modal::Confirm { choice: None, prompt, action });
        None
    }

    /// The restore form: the engine plans the copy from the picked file and the typed name (kept exactly as typed, spaces
    /// included; only the line end is dropped). A refusal is shown as it is and the form stays open; a plan opens the confirm
    /// that shows the exact command, and declining it hands the form back.
    fn submit_restore_form(&mut self, form: FormParts) -> Option<PendingAction> {
        let FormKind::RestoreFile { host, source } = form.kind.clone() else { return None };
        let name = field_value(&form.fields, 0).trim_end_matches('\n').to_string();
        let plan = match crate::zfs::snapshot_restore::plan_restore(&source, &name) {
            Ok(plan) => plan,
            Err(e) => return self.reject(form, super::refusal_text(e)),
        };
        let prompt = super::restore_prompt(&plan);
        let host_name = self.hosts.get(host).cloned().unwrap_or_default();
        let action = PendingAction { host, section: super::Section::Snapshots, call: MutationCall::RestoreFile { plan: Box::new(plan), source, host: host_name } };
        self.confirm_restore = Some((prompt.clone(), form.into_modal()));
        self.modal = Some(Modal::Confirm { choice: None, prompt, action });
        None
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

/// A new-job form with its first two fields (the host and the dataset the job is for) filled in.
fn prefilled(mut form: Modal, host: &str, dataset: &str) -> Modal {
    if let Modal::Form { fields, .. } = &mut form {
        if let [first, second, ..] = fields.as_mut_slice() {
            first.value = host.to_string();
            second.value = dataset.to_string();
        }
    }
    form
}
