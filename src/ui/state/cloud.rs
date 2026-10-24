//! The Cloud section and the remote, backup and prune checks that ride on it.
#![allow(clippy::wildcard_imports)]
use super::*;

impl App {
    /// Asks for the rclone remotes when the Cloud section is on screen and they were last asked for a minute ago or
    /// never (the first time it is opened). The remote list changes only when someone runs `rclone config`.
    pub fn refresh_cloud_if_stale(&mut self, now: Instant) {
        if matches!(self.current_section(), Section::Cloud | Section::Dashboard) && self.last_cloud_refresh.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(60)) {
            self.last_cloud_refresh = Some(now);
            if self.current_section() == Section::Dashboard {
                self.refresh_dashboard();
            } else {
                self.refresh_cloud();
            }
        }
    }

    /// The Cloud table's rows: every rclone remote the hosts reported, plus every remote a job names.
    pub fn cloud_rows(&self) -> Vec<crate::ui::cloud::Row> {
        crate::ui::cloud::rows(&self.jobs_view.jobs, &self.cloud_known)
    }

    pub(super) fn selected_cloud_row(&self) -> Option<crate::ui::cloud::Row> {
        self.cloud_table.selected().and_then(|i| self.cloud_rows().into_iter().nth(i))
    }

    /// Reloads the jobs and asks the hub, and each host that has a job writing to a remote, which remotes it has.
    /// Nothing is asked twice while an answer is awaited.
    /// Reads `[cloud.caps]` from `config.toml` again (an unreadable file means no caps).
    pub(super) fn reload_cloud_caps(&mut self) {
        self.cloud_caps = crate::config::Config::load(&self.config_path).map(|c| c.cloud.caps).unwrap_or_default().into_iter().map(|(name, text)| (name, crate::config::parse_cap(&text).ok_or(text))).collect();
    }

    pub fn refresh_cloud(&mut self) {
        self.refresh_jobs();
        self.reload_cloud_caps();
        let mut hosts: Vec<usize> = self.hosts.iter().position(|h| h == "local").into_iter().collect();
        for job in &self.jobs_view.jobs {
            if crate::ui::cloud::remote_name(job).is_none() {
                continue;
            }
            if let Some(i) = self.hosts.iter().position(|h| *h == job.host) {
                if !hosts.contains(&i) {
                    hosts.push(i);
                }
            }
        }
        for host in hosts {
            let host_name = self.hosts[host].clone();
            if self.cloud_busy.insert(format!("remotes:{host_name}")) {
                self.pending_cloud.push(PendingCloud { host, host_name, work: CloudWork::Remotes });
            }
        }
        let rows = self.cloud_rows().len();
        clamp_table_selection(&mut self.cloud_table, rows);
    }

    /// `s` in Cloud: measures the bucket folders of the selected remote's cloud mirror jobs, one lookup each.
    pub(super) fn request_cloud_sizes(&mut self) {
        let Some(row) = self.selected_cloud_row() else { return };
        let mut asked = 0;
        for job in self.jobs_view.jobs.iter().filter(|j| row.job_ids.contains(&j.id) && j.kind == crate::jobs::KIND_MIRROR) {
            let (Some(remote), Some(host)) = (job.remote.clone(), self.hosts.iter().position(|h| *h == job.host)) else { continue };
            if self.cloud_busy.insert(format!("size:{}", job.id)) {
                self.pending_cloud.push(PendingCloud { host, host_name: job.host.clone(), work: CloudWork::Size { job_id: job.id.clone(), remote } });
            }
            asked += 1;
        }
        self.status_message = Some(if asked == 0 { "Nothing to measure: no cloud mirror job uses this remote".to_string() } else { "Measuring the bucket (it lists the files, which a cloud account may count as requests)...".to_string() });
    }

    /// F5 in Cloud: the form's remote check, for the first cloud mirror job of the selected remote, or for the remote itself
    /// (its top level, read-only) when no job uses it yet.
    pub(super) fn request_cloud_check(&mut self) {
        let Some(row) = self.selected_cloud_row() else { return };
        match crate::ui::cloud::check_target(&row, &self.jobs_view.jobs) {
            Some(remote) => self.request_remote_check(&row.host, &remote),
            None => self.status_message = Some("Nothing to check: this remote's name is not one a job could use".to_string()),
        }
    }

    /// Runs one queued Cloud lookup on a worker thread and reports it as `WorkerMsg::CloudRemotes` / `CloudSize`.
    pub fn run_cloud(&mut self, runner: Arc<dyn CommandRunner>, pending: PendingCloud) {
        let tx = self.tx.clone();
        std::thread::spawn(move || match pending.work {
            CloudWork::Remotes => {
                let result = crate::ui::cloud::list_remotes(runner.as_ref(), &pending.host_name);
                let _ = tx.send(WorkerMsg::CloudRemotes(pending.host_name, result));
            }
            CloudWork::Size { job_id, remote } => {
                let size = crate::ui::cloud::measure(runner.as_ref(), &pending.host_name, &remote, chrono::Utc::now());
                let _ = tx.send(WorkerMsg::CloudSize(job_id, size));
            }
        });
    }

    /// Reports a lookup that could not start (the host index no longer resolves) through the same channel.
    pub fn fail_cloud(&self, pending: PendingCloud, message: String) {
        let now = chrono::Utc::now();
        let _ = match pending.work {
            CloudWork::Remotes => self.tx.send(WorkerMsg::CloudRemotes(pending.host_name, Err(message))),
            CloudWork::Size { job_id, .. } => self.tx.send(WorkerMsg::CloudSize(job_id, crate::ui::cloud::Size { current: None, old: None, at: now, problem: Some(message) })),
        };
    }

    /// F5 in the backup form: validates what the save would validate, then queues the check for the event loop.
    /// A check already running for the same values is left alone.
    pub(super) fn request_backup_preflight(&mut self, host_name: &str, repository: &str, password_file: &str) {
        let Some(host) = self.hosts.iter().position(|h| h == host_name) else {
            self.set_error(format!("Unknown host '{host_name}'. Configured hosts: {}", self.hosts.join(", ")));
            return;
        };
        if !crate::restic::valid_repository(repository) {
            self.set_error("Check: enter the repository first (a path, sftp:user@host:/path or rclone:remote:path, with no password in it)".to_string());
            return;
        }
        if !crate::restic::valid_password_file(password_file) {
            self.set_error("Check: the password file must be an absolute path on the host, such as /root/.restic-password".to_string());
            return;
        }
        let same = |c: &PreflightCheck| c.host == host && c.repository == repository && c.password_file == password_file;
        if self.backup_preflight.as_ref().is_some_and(|c| same(c) && c.result.is_none()) {
            return;
        }
        self.backup_preflight = Some(PreflightCheck { host, repository: repository.to_string(), password_file: password_file.to_string(), result: None, file_note: None });
        self.pending_preflight.push(PendingPreflight { host, host_name: host_name.to_string(), repository: repository.to_string(), password_file: password_file.to_string() });
    }

    /// F5 in the cloud mirror form: queues one check of the remote on the form's host. It rides the backup form's
    /// check (an empty `password_file` marks it as a remote check), so a result for edited values is dropped the same way.
    pub(super) fn request_mirror_check(&mut self, host_name: &str, remote: &str) {
        if !crate::rclone::valid_remote(remote) {
            self.set_error("Check: enter the remote first, like aws:mybucket/photos (the name of an rclone remote, a colon and a bucket or folder, with no keys in it)".to_string());
            return;
        }
        self.request_remote_check(host_name, remote);
    }

    /// Queues the read-only check of `remote` (already validated by the caller) on the host's runner, once while one runs.
    pub(super) fn request_remote_check(&mut self, host_name: &str, remote: &str) {
        let Some(host) = self.hosts.iter().position(|h| h == host_name) else {
            self.set_error(format!("Unknown host '{host_name}'. Configured hosts: {}", self.hosts.join(", ")));
            return;
        };
        let same = |c: &PreflightCheck| c.host == host && c.repository == remote && c.password_file.is_empty();
        if self.backup_preflight.as_ref().is_some_and(|c| same(c) && c.result.is_none()) {
            return;
        }
        self.backup_preflight = Some(PreflightCheck { host, repository: remote.to_string(), password_file: String::new(), result: None, file_note: None });
        self.pending_preflight.push(PendingPreflight { host, host_name: host_name.to_string(), repository: remote.to_string(), password_file: String::new() });
    }

    /// Runs one queued check on a worker thread and reports it as `WorkerMsg::Preflight`.
    pub fn run_backup_preflight(&mut self, runner: Arc<dyn CommandRunner>, pending: PendingPreflight) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let report = if pending.password_file.is_empty() {
                crate::preflight::run_remote(runner, &pending.host_name, &pending.repository, crate::preflight::DEADLINE)
            } else {
                crate::preflight::run(runner, &pending.host_name, &pending.repository, &pending.password_file, crate::preflight::DEADLINE)
            };
            let _ = tx.send(WorkerMsg::Preflight(pending.host, pending.repository, pending.password_file, report.result, report.file_note));
        });
    }

    /// Holds a replication form's save while the destination's snapshots are listed, so the confirmation can
    /// name what Keep on target would destroy. The listing runs once the event loop supplies the runner.
    pub(super) fn request_prune_check(&mut self, form: Modal, action: PendingAction) {
        let MutationCall::SaveJob { job, .. } = &action.call else { return };
        let (dest_host, dest_dataset) = (job.dest_host.clone().unwrap_or_default(), job.dest_dataset.clone().unwrap_or_default());
        let Some(host) = self.hosts.iter().position(|h| *h == dest_host) else {
            self.set_error(format!("Unknown destination host '{dest_host}'"));
            self.modal = Some(form);
            return;
        };
        self.status_message = Some(format!("Checking the snapshots on {dest_host}..."));
        self.pending_prune_check.push(PendingPruneCheck { host, host_name: dest_host.clone(), dest_dataset: dest_dataset.clone() });
        let (prefix, keep) = (crate::jobs::effective_prefix(job).to_string(), job.dest_keep.unwrap_or(1));
        self.prune_check = Some(PruneCheck { host, host_name: dest_host, dest_dataset, prefix, keep, form, action });
    }

    /// Lists the destination on a worker thread and reports it as `WorkerMsg::PrunePreview`.
    pub fn run_prune_check(&mut self, runner: Arc<dyn CommandRunner>, pending: PendingPruneCheck) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::replication::list_snapshot_infos_or_empty(&*runner, &pending.dest_dataset);
            let _ = tx.send(WorkerMsg::PrunePreview(pending.host, pending.dest_dataset, result));
        });
    }

    /// Reports a listing that could not start (the host index no longer resolves) through the same channel.
    pub fn fail_prune_check(&self, pending: PendingPruneCheck, message: String) {
        let _ = self.tx.send(WorkerMsg::PrunePreview(pending.host, pending.dest_dataset, Err(crate::error::AppError::Parse(message))));
    }

    /// Reports a check that could not start (the host index no longer resolves) through the same channel.
    pub fn fail_backup_preflight(&self, pending: PendingPreflight, message: String) {
        let _ = self.tx.send(WorkerMsg::Preflight(pending.host, pending.repository, pending.password_file, crate::restic::Preflight::Problem(message), None));
    }
}
