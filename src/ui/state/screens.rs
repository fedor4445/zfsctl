//! The Dashboard, Settings and Alerts screens: building and refreshing what they show, and their forms.
#![allow(clippy::wildcard_imports)]
use super::*;

impl App {
    /// Makes the Dashboard the section on screen: what zfsctl shows when it starts.
    pub fn open_dashboard(&mut self) {
        if let Some(position) = self.sections.iter().position(|s| *s == Section::Dashboard) {
            self.selected_section = position;
        }
    }

    /// Reloads everything the Dashboard shows that is not a host probe: the jobs, the alert settings, the cloud remotes and the
    /// `[dashboard]` thresholds (an unreadable `config.toml` means the defaults). Probes arrive on their own every 30 s.
    pub fn refresh_dashboard(&mut self) {
        let config = crate::config::Config::load(&self.config_path);
        self.dashboard_thresholds = config.as_ref().map(|c| c.dashboard.resolve()).unwrap_or_default();
        (self.dashboard_protection, self.dashboard_protection_problem) = config.map(|c| c.protection.resolve_or_basic()).unwrap_or_default();
        self.reload_check_files();
        self.refresh_alerts();
        self.refresh_cloud();
    }

    /// The Dashboard as it stands: the host probes and readings, the jobs, the cloud list and the alert settings, judged by
    /// the thresholds. Cheap (everything is in memory), so it is built on every draw and every key.
    pub fn dashboard(&self) -> crate::ui::dashboard::Dashboard {
        let hosts = self
            .hosts
            .iter()
            .enumerate()
            .map(|(i, name)| {
                // A probe that gave up waiting leaves no old reading on screen as if it were current.
                let gave_up = self.host_probe_gave_up(name);
                crate::ui::dashboard::HostInput {
                    name,
                    assessment: self.host_assessment(i),
                    probe: self.host_probes.get(name).and_then(|s| s.last.as_ref()).filter(|_| !gave_up),
                    resources: self.host_resources(name).filter(|_| !gave_up),
                    utc_offset_secs: self.host_probes.get(name).and_then(|s| s.utc_offset_secs),
                    os_name: self.host_probes.get(name).and_then(|s| s.os_name.as_deref()),
                    tag: self.host_tag(i),
                }
            })
            .collect();
        crate::ui::dashboard::build(&crate::ui::dashboard::Inputs {
            hosts,
            jobs: &self.jobs_view,
            cloud_rows: self.cloud_rows(),
            cloud_sizes: &self.cloud_sizes,
            cloud_caps: &self.cloud_caps,
            alerts: &self.alerts_view,
            thresholds: &self.dashboard_thresholds,
            protection: &self.dashboard_protection,
            protection_problem: self.dashboard_protection_problem.as_deref(),
            smart: &self.smart,
            shares: &self.shares_check,
            storage: &self.storage_check,
            running: self.checks_marker.as_ref(),
            now: chrono::Utc::now(),
        })
    }

    /// `t`: asks (two keys) before sending a real test message through the saved channels.
    pub(super) fn ask_to_send_test(&mut self) {
        if self.alerts_test_busy {
            self.status_message = Some("A test message is already being sent".to_string());
            return;
        }
        let Some(settings) = self.alerts_view.settings.clone() else {
            let problem = self.alerts_view.problem.clone().unwrap_or_default();
            self.set_error(format!("{problem}: fix config.toml before testing"));
            return;
        };
        let hook = self.alerts_view.hook.clone();
        if !settings.has_mail() && hook.is_none() {
            self.status_message = Some("Nothing to test: set an email address (e) or a notify_command in config.toml".to_string());
            return;
        }
        let mut prompt = String::from("Send a test message?\n");
        if settings.has_mail() {
            {
                use std::fmt::Write as _;
                let _ = write!(prompt, "  To: {}\n  Command: {}\n", settings.mail_to.join(", "), settings.mail_command);
            }
        }
        match (&hook, settings.has_mail()) {
            (Some(_), true) => prompt.push_str("  Also through the notify hook\n"),
            (Some(_), false) => prompt.push_str("  Through the notify hook\n"),
            (None, _) => {}
        }
        prompt.push_str("This sends a real message. (y/n)");
        let action = PendingAction { host: self.selected_host, section: Section::Alerts, call: MutationCall::SendTestMessage { settings: Box::new(settings), hook } };
        self.modal = Some(Modal::Confirm { choice: None, prompt, action });
    }

    /// `p`: builds the report as it would be sent now on a worker thread (probing the hosts can take a while) and
    /// shows it in a pop-up when ready. Nothing is sent or recorded.
    pub(super) fn start_alerts_preview(&mut self) {
        if let Some(text) = self.alerts_preview_ready.take() {
            self.modal = Some(Modal::Info { title: "Report preview (nothing sent)".to_string(), body: text });
            return;
        }
        if self.alerts_preview_busy {
            return;
        }
        self.alerts_preview_busy = true;
        self.alerts_previews_started += 1;
        self.status_message = Some("Preparing the report preview (probing the hosts)…".to_string());
        let (path, state_dir, tx) = (self.config_path.clone(), self.state_dir.clone(), self.tx.clone());
        std::thread::spawn(move || {
            // A crash in the worker must still answer, or `p` would stay busy for the rest of the session.
            let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match crate::config::Config::load(&path) {
                Ok(config) => crate::notify_cli::report_text_with(&config, &state_dir, chrono::Utc::now(), &|hosts| {
                    crate::daemon::monitor::probe_all(hosts, &|_| true, crate::daemon::monitor::PROBE_DEADLINE, &crate::daemon::monitor::Busy::default())
                }),
                Err(e) => format!("config.toml could not be read: {e}"),
            }))
            .unwrap_or_else(|_| "The report preview crashed; nothing was sent.".to_string());
            let _ = tx.send(WorkerMsg::AlertsPreview(text));
        });
    }

    /// The edit form for the alert settings, pre-filled from the file; `None` while `config.toml` cannot be read.
    pub fn alerts_form(&self) -> Option<Modal> {
        if self.alerts_view.problem.as_deref().is_some_and(|p| p.starts_with("config.toml could not be read")) {
            return None;
        }
        let c = &self.alerts_view.saved;
        let field = |label: &str, value: String| FormField { label: label.to_string(), value };
        Some(Modal::Form {
            title: "Alert settings".to_string(),
            fields: vec![
                field("Email to", c.mail_to.clone().unwrap_or_default()),
                field("Mail command", c.mail_command.clone().unwrap_or_default()),
                field("Mail from", c.mail_from.clone().unwrap_or_default()),
                field("Check every (minutes)", c.check_interval_minutes.map(|n| n.to_string()).unwrap_or_default()),
                field("Daily report at (HH:MM or off)", c.report_time.clone().unwrap_or_default()),
                field("Remind every (hours, 0 = never)", c.remind_hours.map(|n| n.to_string()).unwrap_or_default()),
            ],
            focused: 0,
            description: Some("Blank means the default. Clear Email to to turn email off.".to_string()),
            kind: FormKind::NotifySettings,
        })
    }

    /// Reloads the Settings rows from `config.toml`; the cap rows list the remotes the Cloud screen knows. The selection
    /// stays on its row number, or is dropped when there are no rows.
    pub fn refresh_settings(&mut self) {
        let mut remotes: Vec<String> = self.cloud_rows().into_iter().map(|r| r.name).collect();
        remotes.sort();
        remotes.dedup();
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        self.settings_view = crate::ui::settings::load(&self.config_path, &remotes, no_color);
        let rows = self.settings_view.rows.len();
        match self.settings_table.selected() {
            _ if rows == 0 => self.settings_table.select(None),
            Some(i) if i < rows => {}
            _ => self.settings_table.select(Some(0)),
        }
    }

    /// Loads the Settings rows when the section has just been opened (by any key), and forgets them once it is left,
    /// so the next visit reads the file again. Called on every pass of the event loop, before drawing.
    pub fn ensure_settings_loaded(&mut self) {
        if self.current_section() != Section::Settings {
            self.settings_view.loaded = false;
        } else if !self.settings_view.loaded {
            self.refresh_settings();
        }
    }

    /// The edit form for the selected setting, or `None` when nothing is selected.
    pub(super) fn settings_form(&self) -> Option<Modal> {
        let row = self.settings_table.selected().and_then(|i| self.settings_view.rows.get(i))?;
        let pair = row.raw.len() > 1;
        let description = if pair { "Fill both numbers, or leave both empty for the defaults." } else { "Leave empty to go back to the default." };
        Some(Modal::Form {
            title: row.label.clone(),
            fields: crate::ui::settings::form_fields(row).into_iter().map(|(label, value)| FormField { label, value }).collect(),
            focused: 0,
            description: Some(description.to_string()),
            kind: FormKind::Setting { setting: row.setting.clone() },
        })
    }

    /// Reloads the Alerts screen's data from `config.toml` and the state directory; the scroll position stays.
    pub fn refresh_alerts(&mut self) {
        let (scroll, old_len) = (self.alerts_view.scroll, self.alerts_view.events.len());
        self.alerts_view = crate::ui::alerts::load(&self.config_path, &self.state_dir);
        let new_len = self.alerts_view.events.len();
        // New events are added at the front (newest first): when the operator has scrolled down, move with them so the
        // line being read stays where it is.
        let moved = if scroll > 0 { scroll + new_len.saturating_sub(old_len) } else { 0 };
        self.alerts_view.scroll = moved.min(new_len.saturating_sub(1));
    }

    /// Reloads the Alerts data every 5 seconds while that section is on screen (so a new event or a changed
    /// file shows up), and never otherwise.
    pub fn refresh_alerts_if_stale(&mut self, now: Instant) {
        if !matches!(self.current_section(), Section::Alerts | Section::Dashboard) {
            return;
        }
        if self.last_alerts_refresh.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(5)) {
            self.last_alerts_refresh = Some(now);
            self.refresh_alerts();
        }
    }

    /// After a save in Settings: the theme, the auto-refresh interval, the Dashboard thresholds and the cloud caps
    /// follow `config.toml` without a restart. A theme name that does not resolve changes nothing and says why.
    pub fn apply_saved_settings(&mut self) {
        self.apply_saved_settings_with(&crate::ui::theme::ThemeEnv::from_process());
    }

    pub fn apply_saved_settings_with(&mut self, env: &crate::ui::theme::ThemeEnv) {
        let Ok(config) = crate::config::Config::load(&self.config_path) else { return };
        match crate::ui::theme::resolve(config.ui.theme.as_deref(), env) {
            Ok(resolved) => {
                let changed = self.theme.name != resolved.theme.name;
                self.theme = resolved.theme.bright();
                // The "Saved" note of the save that got us here stays unless the theme changed and has something to say.
                if changed && resolved.note.is_some() {
                    self.status_message = resolved.note;
                }
            }
            Err(e) => self.set_error(e.to_string()),
        }
        self.auto_refresh_interval = config.ui.auto_refresh_interval();
        self.dashboard_thresholds = config.dashboard.resolve();
        (self.dashboard_protection, self.dashboard_protection_problem) = config.protection.resolve_or_basic();
        self.reload_cloud_caps();
    }
}
