//! The forms of the UI: building each form and turning a submitted one into a `PendingAction`, the
//! confirmations that guard risky saves, and the parsers for what is typed into a field.

use super::{App, FormField, FormKind, Modal, MutationCall, PendingAction, PermissionRow, Section};

/// Builds a `zpool create`/`zpool add` vdev spec from a Disks-section
/// selection: `disks` verbatim (each disk becomes its own top-level
/// vdev — a stripe) when `redundancy` is blank, or `[redundancy] +
/// disks` (one mirror/raidz group over every selected disk) otherwise.
pub(super) fn build_vdev_spec_from_selection(redundancy: &str, disks: &[String]) -> Vec<String> {
    if redundancy.is_empty() {
        disks.to_vec()
    } else {
        std::iter::once(redundancy.to_string()).chain(disks.iter().cloned()).collect()
    }
}

/// Builds the `Modal::Form` for editing (or, for a not-yet-shared
/// placeholder `Share::Nfs` with `off`/`None` fields, creating) the given
/// NFS share — used by the Shares section's `Enter` (NFS rows only — SMB
/// rows get a status-message hint instead, see `handle_key`) and the
/// Datasets section's `'N'` key alike, so there's exactly one place that
/// builds the form. `host` is the index of the currently-selected host
/// (`self.selected_host` at every call site), threaded through into the
/// resulting `FormKind` so the eventual mutation runs against the right
/// host. SMB shares route to `smb_edit_modal` (managed smb4.conf block).
/// The new/edit scrub-job form. `existing` is the job being edited; for a new job `pool`
/// is what to pre-fill (a pool picked in the Pools section, or blank).
impl App {
    pub(super) fn job_form(&self, existing: Option<crate::jobs::Job>, pool: &str) -> Modal {
        let (host, schedule, catch_up, title) = match &existing {
            Some(job) => (job.host.clone(), job.schedule.clone(), job.catch_up, format!("Edit scrub job '{}'", job.id)),
            None => (self.active_host_name().to_string(), crate::ui::jobs::DEFAULT_SCHEDULE.to_string(), true, "New scrub job".to_string()),
        };
        Modal::Form {
            title,
            fields: vec![
                FormField { label: "Host".to_string(), value: host },
                FormField { label: "Pool".to_string(), value: pool.to_string() },
                FormField { label: "Schedule".to_string(), value: schedule },
                FormField { label: "Catch up (y/n)".to_string(), value: if catch_up { "y" } else { "n" }.to_string() },
            ],
            focused: 0,
            description: None,
            kind: FormKind::Job { hosts: self.hosts.clone(), existing: existing.map(Box::new), taken_ids: self.jobs_view.jobs.iter().map(|j| j.id.clone()).collect() },
        }
    }
}

impl App {
    pub(super) fn snapshot_job_form(&self, existing: Option<crate::jobs::Job>, dataset: &str) -> Modal {
        let yn = |b: bool| if b { "y" } else { "n" }.to_string();
        let (title, host, dataset, recursive, prefix, keep, schedule, catch_up) = match &existing {
            Some(j) => (format!("Edit snapshot job '{}'", j.id), j.host.clone(), j.dataset.clone().unwrap_or_default(), j.recursive, crate::jobs::effective_prefix(j).to_string(), j.keep.unwrap_or(1), j.schedule.clone(), j.catch_up),
            None => ("New snapshot job".to_string(), self.active_host_name().to_string(), dataset.to_string(), false, crate::jobs::DEFAULT_PREFIX.to_string(), 24, "0 * * * *".to_string(), true),
        };
        Modal::Form {
            title,
            fields: vec![
                FormField { label: "Host".to_string(), value: host },
                FormField { label: "Dataset".to_string(), value: dataset },
                FormField { label: "Recursive (y/n)".to_string(), value: yn(recursive) },
                FormField { label: "Prefix".to_string(), value: prefix },
                FormField { label: "Keep (newest N)".to_string(), value: keep.to_string() },
                FormField { label: "Schedule".to_string(), value: schedule },
                FormField { label: "Catch up (y/n)".to_string(), value: yn(catch_up) },
            ],
            focused: 0,
            description: Some("Only snapshots named <prefix>-<timestamp> made by this job are ever pruned.".to_string()),
            kind: FormKind::SnapshotJob { hosts: self.hosts.clone(), existing: existing.map(Box::new), jobs: self.jobs_view.jobs.clone() },
        }
    }
}

impl App {
    pub(super) fn replicate_job_form(&self, existing: Option<crate::jobs::Job>) -> Modal {
        let yn = |b: bool| if b { "y" } else { "n" }.to_string();
        let (title, host, dataset, prefix, dest_host, dest, force, schedule, catch_up, dest_keep) = match &existing {
            Some(j) => (
                format!("Edit replication job '{}'", j.id),
                j.host.clone(),
                j.dataset.clone().unwrap_or_default(),
                crate::jobs::effective_prefix(j).to_string(),
                j.dest_host.clone().unwrap_or_default(),
                j.dest_dataset.clone().or_else(|| j.dest_dir.clone()).unwrap_or_default(),
                j.force,
                j.schedule.clone(),
                j.catch_up,
                j.dest_keep.map(|k| k.to_string()).unwrap_or_default(),
            ),
            None => ("New replication job".to_string(), self.active_host_name().to_string(), String::new(), crate::jobs::DEFAULT_PREFIX.to_string(), String::new(), String::new(), false, "30 * * * *".to_string(), true, String::new()),
        };
        Modal::Form {
            title,
            fields: vec![
                FormField { label: "Source host".to_string(), value: host },
                FormField { label: "Source dataset".to_string(), value: dataset },
                FormField { label: "Prefix (snapshots to send)".to_string(), value: prefix },
                FormField { label: "Destination host (or file)".to_string(), value: dest_host },
                FormField { label: "Destination dataset (or directory)".to_string(), value: dest },
                FormField { label: "Force: roll back the destination (y/n)".to_string(), value: yn(force) },
                FormField { label: "Schedule".to_string(), value: schedule },
                FormField { label: "Catch up (y/n)".to_string(), value: yn(catch_up) },
                FormField { label: "Keep on target (blank = keep everything)".to_string(), value: dest_keep },
            ],
            focused: 0,
            description: Some("Sends the newest <prefix>-<timestamp> snapshot, incrementally. The destination is received unmounted and rolled back only if Force is y. Keep on target destroys this job's older snapshots on a host destination after each run.".to_string()),
            kind: FormKind::ReplicateJob { hosts: self.hosts.clone(), existing: existing.map(Box::new), jobs: self.jobs_view.jobs.clone() },
        }
    }
}

impl App {
    pub(super) fn backup_job_form(&self, existing: Option<crate::jobs::Job>) -> Modal {
        let yn = |b: bool| if b { "y" } else { "n" }.to_string();
        let (title, host, dataset, path, source, fresh_keep, repository, password_file, keep, schedule, catch_up) = match &existing {
            Some(j) => (
                format!("Edit backup job '{}'", j.id),
                j.host.clone(),
                j.dataset.clone().unwrap_or_default(),
                j.path.clone().unwrap_or_default(),
                crate::ui::jobs::source_text(j),
                if crate::jobs::backup_source(j) == crate::jobs::BackupSource::Fresh { j.keep.map(|k| k.to_string()).unwrap_or_default() } else { String::new() },
                j.repository.clone().unwrap_or_default(),
                j.password_file.clone().unwrap_or_default(),
                j.retention.clone().unwrap_or_default(),
                j.schedule.clone(),
                j.catch_up,
            ),
            None => ("New backup job".to_string(), self.active_host_name().to_string(), String::new(), String::new(), crate::jobs::SOURCE_NEW.to_string(), String::new(), String::new(), String::new(), String::new(), "30 3 * * *".to_string(), true),
        };
        Modal::Form {
            title,
            fields: vec![
                FormField { label: "Host (where restic runs)".to_string(), value: host },
                FormField { label: "Dataset".to_string(), value: dataset },
                FormField { label: "Path inside the dataset (blank = all of it)".to_string(), value: path },
                FormField { label: "Source (F2 to pick: new, a prefix, or @snapshot)".to_string(), value: source },
                FormField { label: format!("Snapshots to keep (Source new only; blank = {})", crate::jobs::DEFAULT_FRESH_KEEP), value: fresh_keep },
                FormField { label: "Repository (restic -r; no credentials in it)".to_string(), value: repository },
                FormField { label: "Password file (absolute path on the host)".to_string(), value: password_file },
                FormField { label: "Keep (e.g. 7d 4w 12m 3y 5l; blank = keep everything)".to_string(), value: keep },
                FormField { label: "Schedule".to_string(), value: schedule },
                FormField { label: "Catch up (y/n)".to_string(), value: yn(catch_up) },
            ],
            focused: 0,
            description: Some("Backs up a read-only clone of a snapshot. Source: new = take a fresh snapshot each run and keep the last few of those; a prefix = the newest <prefix>-<timestamp> (it takes none); @name = always that one snapshot. The repository must already exist (restic init).".to_string()),
            kind: FormKind::BackupJob { hosts: self.hosts.clone(), existing: existing.map(Box::new), jobs: self.jobs_view.jobs.clone() },
        }
    }
}

/// Turns a submitted backup form into a save, or says what is wrong. It refuses exactly what the
/// daemon would refuse (`jobs::resolve`).
pub(super) fn build_backup_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
    let FormKind::BackupJob { hosts, existing, jobs } = kind else { return Err("not a backup job form".to_string()) };
    let yes_no = |label: &str, raw: &str, blank: bool| match raw.trim().to_lowercase().as_str() {
        "" => Ok(blank),
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        other => Err(format!("{label} must be y or n, not '{other}'")),
    };
    let host = field_value(fields, 0).trim();
    if !hosts.iter().any(|h| h == host) {
        return Err(format!("Unknown host '{host}'. Configured hosts: {}", hosts.join(", ")));
    }
    let dataset = field_value(fields, 1).trim();
    let path = field_value(fields, 2).trim();
    let (source, snapshot, prefix) = crate::ui::jobs::parse_source(field_value(fields, 3))?;
    let fresh = source.as_deref() == Some(crate::jobs::SOURCE_NEW);
    let fresh_keep = match field_value(fields, 4).trim() {
        "" => None,
        n if fresh => Some(n.parse::<u32>().ok().filter(|k| *k >= 1).ok_or_else(|| "Snapshots to keep must be a whole number, at least 1".to_string())?),
        _ => None,
    };
    let repository = field_value(fields, 5).trim();
    let password_file = field_value(fields, 6).trim();
    let keep = field_value(fields, 7).trim();
    let schedule = field_value(fields, 8).trim();
    crate::daemon::cron::Schedule::parse(schedule).map_err(|e| format!("Schedule: {e}"))?;
    let catch_up = yes_no("Catch up", field_value(fields, 9), true)?;
    let taken: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
    let base = crate::jobs::Job {
        id: String::new(),
        host: host.to_string(),
        kind: crate::jobs::KIND_BACKUP.to_string(),
        pool: None,
        dataset: Some(dataset.to_string()),
        recursive: false,
        prefix,
        keep: fresh_keep,
        dest_host: None,
        dest_dataset: None,
        dest_dir: None,
        dest_keep: None,
        remote: None,
        bwlimit: None,
        force: false,
        repository: Some(repository.to_string()),
        password_file: Some(password_file.to_string()),
        retention: (!keep.is_empty()).then(|| keep.to_string()),
        source,
        snapshot,
        path: (!path.is_empty()).then(|| path.to_string()),
        schedule: schedule.to_string(),
        catch_up,
        enabled: true,
        created_at: Some(now),
    };
    if let Some(old) = existing {
        let old_pool = old.dataset.as_deref().and_then(|d| d.split('/').next()).unwrap_or("");
        if old.host != host || old_pool != dataset.split('/').next().unwrap_or("") {
            return Err("Changing the host or pool of a backup job would strand its clone; delete this job and create a new one.".to_string());
        }
    }
    let job = match existing {
        Some(old) => crate::jobs::Job { id: old.id.clone(), enabled: old.enabled, created_at: old.created_at, ..base },
        None => crate::jobs::Job { id: crate::ui::jobs::new_backup_job_id(host, dataset, &taken), ..base },
    };
    crate::jobs::resolve(&job, hosts)?;
    Ok(PendingAction { host: 0, section: Section::Jobs, call: MutationCall::SaveJob { state_dir: state_dir.to_path_buf(), job: Box::new(job) } })
}

impl App {
    pub(super) fn mirror_job_form(&self, existing: Option<crate::jobs::Job>) -> Modal {
        let yn = |b: bool| if b { "y" } else { "n" }.to_string();
        let (title, host, dataset, path, source, fresh_keep, remote, limit, schedule, catch_up) = match &existing {
            Some(j) => (
                format!("Edit cloud mirror job '{}'", j.id),
                j.host.clone(),
                j.dataset.clone().unwrap_or_default(),
                j.path.clone().unwrap_or_default(),
                crate::ui::jobs::source_text(j),
                if crate::jobs::backup_source(j) == crate::jobs::BackupSource::Fresh { j.keep.map(|k| k.to_string()).unwrap_or_default() } else { String::new() },
                j.remote.clone().unwrap_or_default(),
                j.bwlimit.clone().unwrap_or_default(),
                j.schedule.clone(),
                j.catch_up,
            ),
            None => ("New cloud mirror job".to_string(), self.active_host_name().to_string(), String::new(), String::new(), crate::jobs::SOURCE_NEW.to_string(), String::new(), String::new(), String::new(), "30 3 * * *".to_string(), true),
        };
        Modal::Form {
            title,
            fields: vec![
                FormField { label: "Host (where rclone runs)".to_string(), value: host },
                FormField { label: "Dataset".to_string(), value: dataset },
                FormField { label: "Path inside the dataset (blank = all of it)".to_string(), value: path },
                FormField { label: "Source (F2 to pick: new, a prefix, or @snapshot)".to_string(), value: source },
                FormField { label: format!("Snapshots to keep (Source new only; blank = {})", crate::jobs::DEFAULT_FRESH_KEEP), value: fresh_keep },
                FormField { label: "Remote (rclone name:path, e.g. aws:mybucket/photos; no keys in it)".to_string(), value: remote },
                FormField { label: "Speed limit (bytes/s, e.g. 5M; blank = none)".to_string(), value: limit },
                FormField { label: "Schedule".to_string(), value: schedule },
                FormField { label: "Catch up (y/n)".to_string(), value: yn(catch_up) },
            ],
            focused: 0,
            description: Some("Copies a read-only clone of a snapshot to <remote>/current as plain files. What a run replaces or deletes is first moved to <remote>/old/<date and time>/; zfsctl never deletes anything in old/. Symlinks, devices, empty folders and child datasets are not copied.".to_string()),
            kind: FormKind::MirrorJob { hosts: self.hosts.clone(), existing: existing.map(Box::new), jobs: self.jobs_view.jobs.clone() },
        }
    }
}

/// Turns a submitted cloud mirror form into a save, or says what is wrong. It refuses exactly what the daemon
/// would refuse (`jobs::resolve`), plus a remote that overlaps another job's.
pub(super) fn build_mirror_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
    let FormKind::MirrorJob { hosts, existing, jobs } = kind else { return Err("not a cloud mirror job form".to_string()) };
    let host = field_value(fields, 0).trim();
    if !hosts.iter().any(|h| h == host) {
        return Err(format!("Unknown host '{host}'. Configured hosts: {}", hosts.join(", ")));
    }
    let dataset = field_value(fields, 1).trim();
    let path = field_value(fields, 2).trim();
    let (source, snapshot, prefix) = crate::ui::jobs::parse_source(field_value(fields, 3))?;
    let fresh = source.as_deref() == Some(crate::jobs::SOURCE_NEW);
    let fresh_keep = match field_value(fields, 4).trim() {
        "" => None,
        n if fresh => Some(n.parse::<u32>().ok().filter(|k| *k >= 1).ok_or_else(|| "Snapshots to keep must be a whole number, at least 1".to_string())?),
        _ => None,
    };
    let remote = field_value(fields, 5).trim();
    if !crate::rclone::valid_remote(remote) {
        return Err("Remote must look like aws:mybucket/photos: the name of an rclone remote, a colon and a bucket or folder (no . or .. parts), with no keys in it".to_string());
    }
    let limit = field_value(fields, 6).trim();
    if !limit.is_empty() && !crate::rclone::valid_bwlimit(limit) {
        return Err("Speed limit must be a number of bytes per second with an optional K, M or G, like 5M".to_string());
    }
    let schedule = field_value(fields, 7).trim();
    crate::daemon::cron::Schedule::parse(schedule).map_err(|e| format!("Schedule: {e}"))?;
    let catch_up = match field_value(fields, 8).trim().to_lowercase().as_str() {
        "" | "y" | "yes" => true,
        "n" | "no" => false,
        other => return Err(format!("Catch up must be y or n, not '{other}'")),
    };
    let taken: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
    let base = crate::jobs::Job {
        id: String::new(),
        host: host.to_string(),
        kind: crate::jobs::KIND_MIRROR.to_string(),
        pool: None,
        dataset: Some(dataset.to_string()),
        recursive: false,
        prefix,
        keep: fresh_keep,
        dest_host: None,
        dest_dataset: None,
        dest_dir: None,
        dest_keep: None,
        remote: Some(remote.to_string()),
        bwlimit: (!limit.is_empty()).then(|| limit.to_string()),
        force: false,
        repository: None,
        password_file: None,
        retention: None,
        source,
        snapshot,
        path: (!path.is_empty()).then(|| path.to_string()),
        schedule: schedule.to_string(),
        catch_up,
        enabled: true,
        created_at: Some(now),
    };
    if let Some(old) = existing {
        let old_pool = old.dataset.as_deref().and_then(|d| d.split('/').next()).unwrap_or("");
        if old.host != host || old_pool != dataset.split('/').next().unwrap_or("") {
            return Err("Changing the host or pool of a cloud mirror job would strand its clone; delete this job and create a new one.".to_string());
        }
    }
    let job = match existing {
        Some(old) => crate::jobs::Job { id: old.id.clone(), enabled: old.enabled, created_at: old.created_at, ..base },
        None => crate::jobs::Job { id: crate::ui::jobs::new_mirror_job_id(host, dataset, &taken), ..base },
    };
    crate::jobs::resolve(&job, hosts)?;
    if let Some(other) = crate::jobs::clashing_job(jobs, &job) {
        return Err(format!("Job '{}' cannot run alongside this one: {}", other.id, crate::jobs::clash_reason(other, &job).unwrap_or_default()));
    }
    Ok(PendingAction { host: 0, section: Section::Jobs, call: MutationCall::SaveJob { state_dir: state_dir.to_path_buf(), job: Box::new(job) } })
}

/// Turns a submitted replication form into a save, or says what is wrong. The job is validated
/// by `jobs::resolve`, exactly as the daemon will validate it, plus the shared-destination rule.
pub(super) fn build_replicate_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
    let FormKind::ReplicateJob { hosts, existing, jobs } = kind else { return Err("not a replication job form".to_string()) };
    let yes_no = |label: &str, raw: &str, blank: bool| match raw.trim().to_lowercase().as_str() {
        "" => Ok(blank),
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        other => Err(format!("{label} must be y or n, not '{other}'")),
    };
    let host = field_value(fields, 0).trim();
    if !hosts.iter().any(|h| h == host) {
        return Err(format!("Unknown host '{host}'. Configured hosts: {}", hosts.join(", ")));
    }
    let dataset = field_value(fields, 1).trim();
    let prefix = match field_value(fields, 2).trim() {
        "" => crate::jobs::DEFAULT_PREFIX,
        p => p,
    };
    let dest_host = field_value(fields, 3).trim();
    let dest = field_value(fields, 4).trim();
    let force = yes_no("Force", field_value(fields, 5), false)?;
    let schedule = field_value(fields, 6).trim();
    crate::daemon::cron::Schedule::parse(schedule).map_err(|e| format!("Schedule: {e}"))?;
    let catch_up = yes_no("Catch up", field_value(fields, 7), true)?;
    let dest_keep = match field_value(fields, 8).trim() {
        "" => None,
        raw => match raw.parse::<u32>() {
            Ok(n) if n >= 1 => Some(n),
            _ => return Err(format!("Keep on target must be a whole number of at least 1, or blank to keep everything, not '{raw}'")),
        },
    };
    let to_file = dest_host == crate::jobs::DEST_FILE;
    if to_file && dest_keep.is_some() {
        return Err("Keep on target is only for a destination host, not for a file: stream files form chains and are never pruned".to_string());
    }
    let taken: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
    let base = crate::jobs::Job {
        id: String::new(),
        host: host.to_string(),
        kind: crate::jobs::KIND_REPLICATE.to_string(),
        pool: None,
        dataset: Some(dataset.to_string()),
        recursive: false,
        prefix: Some(prefix.to_string()),
        keep: None,
        dest_host: Some(dest_host.to_string()),
        dest_dataset: (!to_file).then(|| dest.to_string()),
        dest_dir: to_file.then(|| dest.to_string()),
        dest_keep,
        remote: None,
        bwlimit: None,
        force,
        repository: None,
        password_file: None,
        retention: None, source: None, snapshot: None, path: None,
        schedule: schedule.to_string(),
        catch_up,
        enabled: true,
        created_at: Some(now),
    };
    let job = match existing {
        Some(old) => crate::jobs::Job { id: old.id.clone(), enabled: old.enabled, created_at: old.created_at, ..base },
        None => crate::jobs::Job { id: crate::ui::jobs::new_replicate_job_id(host, dataset, dest_host, dest, &taken), ..base },
    };
    crate::jobs::resolve(&job, hosts)?;
    if let Some(other) = crate::jobs::clashing_job(jobs, &job) {
        return Err(format!("Job '{}' cannot run alongside this one: {}", other.id, crate::jobs::clash_reason(other, &job).unwrap_or_default()));
    }
    Ok(PendingAction { host: 0, section: Section::Jobs, call: MutationCall::SaveJob { state_dir: state_dir.to_path_buf(), job: Box::new(job) } })
}

/// Turns a submitted snapshot-job form into a save, or says what is wrong. It refuses exactly
/// what the daemon would refuse (`jobs::resolve`) plus a prefix another job already uses.
pub(super) fn build_snapshot_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
    let FormKind::SnapshotJob { hosts, existing, jobs } = kind else { return Err("not a snapshot job form".to_string()) };
    let host = field_value(fields, 0).trim();
    if !hosts.iter().any(|h| h == host) {
        return Err(format!("Unknown host '{host}'. Configured hosts: {}", hosts.join(", ")));
    }
    let dataset = field_value(fields, 1).trim();
    if !crate::jobs::valid_dataset(dataset) {
        return Err("Dataset must look like tank/data (letters, digits and _ - : . /)".to_string());
    }
    let yes_no = |label: &str, raw: &str, blank: bool| match raw.trim().to_lowercase().as_str() {
        "" => Ok(blank),
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        other => Err(format!("{label} must be y or n, not '{other}'")),
    };
    let recursive = yes_no("Recursive", field_value(fields, 2), false)?;
    let prefix = match field_value(fields, 3).trim() {
        "" => crate::jobs::DEFAULT_PREFIX,
        p => p,
    };
    if !crate::jobs::valid_prefix(prefix) {
        return Err("Prefix may only use letters, digits, _ - and .".to_string());
    }
    let keep: u32 = field_value(fields, 4).trim().parse().ok().filter(|k| *k >= 1).ok_or_else(|| "Keep must be a whole number, at least 1".to_string())?;
    let schedule = field_value(fields, 5).trim();
    crate::daemon::cron::Schedule::parse(schedule).map_err(|e| format!("Schedule: {e}"))?;
    let catch_up = yes_no("Catch up", field_value(fields, 6), true)?;
    let taken: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
    let job = match existing {
        Some(old) => crate::jobs::Job { host: host.to_string(), dataset: Some(dataset.to_string()), recursive, prefix: Some(prefix.to_string()), keep: Some(keep), schedule: schedule.to_string(), catch_up, ..(**old).clone() },
        None => crate::jobs::Job {
            id: crate::ui::jobs::new_snapshot_job_id(host, dataset, prefix, &taken),
            host: host.to_string(),
            kind: crate::jobs::KIND_SNAPSHOT.to_string(),
            pool: None,
            dataset: Some(dataset.to_string()),
            recursive,
            prefix: Some(prefix.to_string()),
            keep: Some(keep),
            dest_host: None,
            dest_dataset: None,
            dest_dir: None,
            dest_keep: None,
            remote: None,
            bwlimit: None,
            force: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None,
            schedule: schedule.to_string(),
            catch_up,
            enabled: true,
            created_at: Some(now),
        },
    };
    if let Some(other) = crate::jobs::clashing_job(jobs, &job) {
        return Err(format!("Job '{}' already snapshots {dataset} with prefix '{prefix}': they would prune each other. Use another prefix.", other.id));
    }
    Ok(PendingAction { host: 0, section: Section::Jobs, call: MutationCall::SaveJob { state_dir: state_dir.to_path_buf(), job: Box::new(job) } })
}

/// Turns a submitted job form into a save, or says what is wrong with it. An edit keeps the
/// job's id, creation time and enabled flag; a new job gets a fresh id and `created_at = now`.
pub(super) fn build_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
    let FormKind::Job { hosts, existing, taken_ids } = kind else { return Err("not a job form".to_string()) };
    let host = field_value(fields, 0).trim();
    if !hosts.iter().any(|h| h == host) {
        return Err(format!("Unknown host '{host}'. Configured hosts: {}", hosts.join(", ")));
    }
    let pool = field_value(fields, 1).trim();
    if pool.is_empty() {
        return Err("Pool name is required".to_string());
    }
    let schedule = field_value(fields, 2).trim();
    crate::daemon::cron::Schedule::parse(schedule).map_err(|e| format!("Schedule: {e}"))?;
    let catch_up = match field_value(fields, 3).trim().to_lowercase().as_str() {
        "" | "y" | "yes" => true,
        "n" | "no" => false,
        other => return Err(format!("Catch up must be y or n, not '{other}'")),
    };
    let job = match existing {
        Some(old) => crate::jobs::Job { host: host.to_string(), pool: Some(pool.to_string()), schedule: schedule.to_string(), catch_up, ..(**old).clone() },
        None => crate::jobs::Job {
            id: crate::ui::jobs::new_job_id(host, pool, taken_ids),
            host: host.to_string(),
            kind: crate::jobs::KIND_SCRUB.to_string(),
            pool: Some(pool.to_string()),
            schedule: schedule.to_string(),
            catch_up,
            enabled: true,
            created_at: Some(now),
            dataset: None,
            recursive: false,
            prefix: None,
            keep: None,
            dest_host: None,
            dest_dataset: None,
            dest_dir: None,
            dest_keep: None,
            remote: None,
            bwlimit: None,
            force: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None,
        },
    };
    Ok(PendingAction { host: 0, section: Section::Jobs, call: MutationCall::SaveJob { state_dir: state_dir.to_path_buf(), job: Box::new(job) } })
}

pub(super) fn share_edit_modal(share: &crate::zfs::share::Share, host: usize) -> Modal {
    match share {
        crate::zfs::share::Share::Nfs { dataset, raw, networks, hosts, read_only, root } => {
            let description = if crate::zfs::share::sharenfs_editable_losslessly(raw) {
                None
            } else {
                Some(format!(
                    "Warning: current value '{raw}' uses clauses this form doesn't support (e.g. sec=, crossmnt). Submitting will drop them."
                ))
            };
            Modal::Form {
                title: format!("NFS share: '{dataset}'"),
                fields: vec![
                    FormField { label: "Networks".to_string(), value: networks.join(",") },
                    FormField { label: "Hosts".to_string(), value: hosts.join(",") },
                    FormField { label: "Read-only (y/n)".to_string(), value: if *read_only { "y".to_string() } else { "n".to_string() } },
                    FormField { label: "Root user mapping".to_string(), value: root.clone().unwrap_or_default() },
                ],
                focused: 0,
                description,
                kind: FormKind::ShareNfs { host, dataset: dataset.clone() },
            }
        }
        crate::zfs::share::Share::Smb { dataset, path, .. } => smb_edit_modal(Some(share), dataset, path, host),
    }
}

/// Turns the Alerts form into the save and the text of its confirmation, or says what is wrong. The values are
/// checked by `NotifyConfig::resolve`, exactly as the daemon will check them.
pub(super) fn build_notify_action(fields: &[FormField], config_path: &std::path::Path) -> std::result::Result<(PendingAction, String), String> {
    let text = |i: usize| Some(field_value(fields, i).trim().to_string()).filter(|v| !v.is_empty());
    let number = |i: usize, what: &str| -> std::result::Result<Option<u64>, String> {
        text(i).map(|v| v.parse::<u64>().map_err(|_| format!("{what} must be a whole number"))).transpose()
    };
    let cfg = crate::config::NotifyConfig {
        mail_to: text(0),
        mail_command: text(1),
        mail_from: text(2),
        check_interval_minutes: number(3, "Check every (minutes)")?,
        report_time: text(4),
        remind_hours: number(5, "Remind every (hours)")?,
    };
    let settings = cfg.resolve()?;
    let to = if settings.has_mail() { format!("Email to {}", settings.mail_to.join(", ")) } else { "Email off".to_string() };
    let report = settings.report_time.map_or("no daily report".to_string(), |t| format!("report at {}", t.format("%H:%M")));
    let remind = settings.remind.map_or("never remind".to_string(), |d| format!("remind every {} hours", d.as_secs() / 3600));
    let command = if settings.has_mail() { format!("\n  Mail command (runs as root on the hub): {}", settings.mail_command) } else { String::new() };
    let prompt = format!("Save alert settings?\n  {to}{command}\n  Check every {} min, {report}, {remind}\nThe daemon applies them within seconds. (y/n)", settings.check_interval.as_secs() / 60);
    let section = crate::config::render_notify_section(&cfg);
    Ok((PendingAction { host: 0, section: Section::Alerts, call: MutationCall::SaveNotify { config_path: config_path.to_path_buf(), section } }, prompt))
}

/// What the quota checks need, from the selected dataset's property list (`zfs get all -p`: sizes in bytes, `-` or `0` unset).
pub(super) fn quota_facts(props: &[crate::zfs::property::Property]) -> crate::zfs::quota::Facts {
    let num = |name: &str| props.iter().find(|p| p.name == name).and_then(|p| p.value.parse::<u64>().ok()).unwrap_or(0);
    crate::zfs::quota::Facts {
        used: num("used"),
        referenced: num("referenced"),
        available: num("available"),
        quota: num("quota"),
        refquota: num("refquota"),
        reservation: num("reservation"),
        refreservation: num("refreservation"),
        is_volume: props.iter().any(|p| p.name == "type" && p.value == "volume"),
    }
}

/// The pool, the `zpool add` words and the role of what the Add vdev forms ask for, or why it is refused: the raw form's spec
/// is checked word by word, the Disks form builds it from its Role and Redundancy fields.
pub(super) fn add_vdev_request(kind: &FormKind, fields: &[FormField]) -> Result<(String, Vec<String>, crate::zfs::vdev_role::Role), String> {
    use crate::zfs::vdev_role::{build_spec, check_raw_spec, Role};
    match kind {
        FormKind::AddVdev { pool, .. } => {
            let spec: Vec<String> = field_value(fields, 0).split_whitespace().map(String::from).collect();
            let role = check_raw_spec(&spec)?;
            Ok((pool.clone(), spec, role))
        }
        FormKind::AddVdevFromDisks { disks, .. } => {
            let pool = field_value(fields, 0).trim();
            if pool.is_empty() {
                return Err("Target pool name: required".to_string());
            }
            let role = Role::parse(field_value(fields, 1)).ok_or_else(|| "Role: data, log, cache, special, dedup or spare".to_string())?;
            let spec = build_spec(role, field_value(fields, 2), disks)?;
            Ok((pool.to_string(), spec, role))
        }
        _ => Err("not an add-vdev form".to_string()),
    }
}

/// What the confirm says while the host has not shown the layout yet.
pub(super) const CHECKING_LAYOUT: &str = "Checking the resulting layout";

/// The part of `zpool add -n`'s output worth reading: the header, the pool line, and what the add changes: the class heading
/// of the role being added with its devices, or, for data, the last of the data vdevs. The whole tree of a big pool would push
/// the new vdev off the screen.
pub(super) fn layout_excerpt(text: &str, role: crate::zfs::vdev_role::Role) -> String {
    use crate::zfs::vdev_role::Role;
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    // header, then the pool line (the first indented line); the rest is the vdev tree
    let Some(pool_at) = lines.iter().position(|l| indent(l) > 0) else { return lines.join("\n") };
    let (head, tree) = (&lines[..=pool_at], &lines[pool_at + 1..]);
    let level = indent(lines[pool_at]);
    let heading = |l: &&str| indent(l) == level && matches!(l.trim(), "logs" | "cache" | "spares" | "special" | "dedup");
    let wanted = match role {
        Role::Log => Some("logs"),
        Role::Cache => Some("cache"),
        Role::Spare => Some("spares"),
        Role::Special => Some("special"),
        Role::Dedup => Some("dedup"),
        Role::Data => None,
    };
    let shown: Vec<&str> = match wanted.and_then(|w| tree.iter().position(|l| l.trim() == w && indent(l) == level)) {
        Some(start) => {
            let end = tree[start + 1..].iter().position(|l| heading(l)).map_or(tree.len(), |n| start + 1 + n);
            let mut out = vec!["  ..."];
            out.extend(&tree[start..end]);
            out
        }
        None => {
            let data_end = tree.iter().position(|l| heading(l)).unwrap_or(tree.len());
            let data = &tree[..data_end];
            let mut out = Vec::new();
            if data.len() > 10 {
                out.push("  ...");
            }
            out.extend(&data[data.len().saturating_sub(10)..]);
            out
        }
    };
    let mut all: Vec<&str> = head.to_vec();
    all.extend(shown);
    all.join("\n")
}

/// The confirmation for adding vdevs: the exact command, the layout `zpool add -n` says it would give (once the host has
/// answered), and what the role means for undoing it.
pub(super) fn add_vdev_prompt(host: &str, pool: &str, spec: &[String], role: crate::zfs::vdev_role::Role, layout: Option<&str>) -> String {
    let shown = match layout {
        None => format!("\n\n{CHECKING_LAYOUT}..."),
        Some(text) => format!("\n\n{}", layout_excerpt(text, role)),
    };
    let why = if role.irreversible() {
        "A vdev added to a pool usually cannot be removed again. Add it?".to_string()
    } else {
        format!("A {} device can be removed again with zpool remove. Add it?", role.word())
    };
    format!("On {host}:\n  zpool add {pool} {}{shown}\n\n{why} (y/n)", spec.join(" "))
}

/// The text of form field `index`; a missing field reads as empty, which every caller
/// already treats as "not filled in" (forms are built with a fixed field count per kind).
pub(super) fn field_value(fields: &[FormField], index: usize) -> &str {
    fields.get(index).map_or("", |f| f.value.as_str())
}

/// The confirmation text for a form whose result cannot be undone: the host, the exact command on a line of
/// its own, then what makes it irreversible.
pub(super) fn irreversible_prompt(hosts: &[String], action: &PendingAction) -> Option<String> {
    let host = hosts.get(action.host).map_or("?", String::as_str);
    let ask = |command: String, why: &str| Some(format!("On {host}:\n  {command}\n\n{why} (y/n)"));
    match &action.call {
        MutationCall::CreatePool { name, vdev_spec } => ask(
            format!("zpool create {name} {}", vdev_spec.join(" ")),
            "This overwrites whatever is on those disks. Create the pool?",
        ),
        MutationCall::AddVdev { pool, vdev_spec } => {
            // Which role this is decides the wording; a spec that cannot be read gets the cautious (irreversible) text.
            let role = crate::zfs::vdev_role::check_raw_spec(vdev_spec).unwrap_or(crate::zfs::vdev_role::Role::Data);
            Some(add_vdev_prompt(host, pool, vdev_spec, role, None))
        }
        MutationCall::ReplaceDisk { pool, old_disk, new_disk } => ask(
            format!("zpool replace {pool} {old_disk} {new_disk}"),
            "The new disk is overwritten. Replace it?",
        ),
        MutationCall::UnallowPermission { dataset, target, perms } => {
            use crate::zfs::permission::{PermissionTarget, Recipient};
            let whose = match target {
                PermissionTarget::CreateTime => "create-time permissions".to_string(),
                PermissionTarget::Set(name) => format!("permission set @{name}"),
                PermissionTarget::Grant { recipient: Recipient::User(u), .. } => format!("user {u}"),
                PermissionTarget::Grant { recipient: Recipient::Group(g), .. } => format!("group {g}"),
                PermissionTarget::Grant { recipient: Recipient::Everyone, .. } => "everyone".to_string(),
            };
            Some(format!(
                "On {host}:\n  Revoke {} from {whose} on {dataset}\n\nThe delegation is removed. Revoke it? (y/n)",
                perms.as_ref().map_or_else(|| "all delegated permissions".to_string(), |p| p.join(","))
            ))
        }
        MutationCall::SetDatasetProperty { dataset, key, value } if key == "volsize" => ask(
            format!("zfs set volsize={value} {dataset}"),
            "Shrinking a volume destroys the data past the new size. Set it?",
        ),
        MutationCall::SetPoolProperty { pool, key, value } if key.starts_with("feature@") => ask(
            format!("zpool set {key}={value} {pool}"),
            "Enabling a pool feature cannot be undone and can make the pool unreadable by older systems. Set it?",
        ),
        MutationCall::UnloadKey { dataset } => ask(
            format!("zfs unload-key {dataset}"),
            "The data under it cannot be read again until its key is loaded with the passphrase; without the passphrase it stays locked for good. zfs refuses while the dataset (or one under it) is mounted. Unload the key?",
        ),
        MutationCall::ChangeKey { dataset, .. } => ask(
            format!("zfs change-key {dataset}"),
            "The old passphrase stops loading this key from now on. The data is not re-encrypted: copies sent elsewhere earlier (raw replicas, snapshots) still need the old passphrase. Keep the new one safe: nothing else stores it. Change the key?",
        ),
        _ => None,
    }
}

/// The confirmation shown before saving a replication job whose Keep on target would destroy snapshots on the
/// destination now: how many, which (oldest and newest of them) and how many stay. `doomed` is oldest first.
pub(super) fn prune_prompt(dest_host: &str, dest_ds: &str, doomed: &[String], kept: usize) -> String {
    let (oldest, newest) = (doomed.first().map_or("?", String::as_str), doomed.last().map_or("?", String::as_str));
    let noun = if doomed.len() == 1 { "snapshot" } else { "snapshots" };
    format!("This will destroy {} {noun} on {dest_host} {dest_ds}: oldest {oldest} ... newest destroyed {newest}; {kept} kept (counting the one the next run adds).\nOnly this job's own snapshots are destroyed, and they cannot be brought back.", doomed.len())
}

/// The confirmation for saving a job that, from its next run on, destroys data the previous version of the job
/// (`old`, when editing) did not: a replication that now forces the receive, a snapshot job whose `keep` would
/// destroy snapshots, a backup job whose retention would forget backups. `snapshots` is the list for the job's
/// host when it is known; without it a snapshot job cannot say how many it would destroy.
pub(super) fn job_save_prompt(old: Option<&crate::jobs::Job>, new: &crate::jobs::Job, snapshots: Option<&[crate::zfs::snapshot::Snapshot]>) -> Option<String> {
    use crate::jobs::{effective_prefix, KIND_BACKUP, KIND_MIRROR, KIND_REPLICATE, KIND_SNAPSHOT};
    let dataset = new.dataset.as_deref().unwrap_or("?");
    match new.kind.as_str() {
        KIND_REPLICATE if new.force => {
            let (dest_host, dest) = (new.dest_host.as_deref().unwrap_or("?"), new.dest_dataset.as_deref().unwrap_or("?"));
            let changed = old.is_none_or(|o| !o.force || o.dest_host != new.dest_host || o.dest_dataset != new.dest_dataset);
            changed.then(|| {
                format!(
                    "Replication {dataset} -> {dest_host}:{dest} with Force on.\nEvery run does `zfs receive -F` into {dest} on {dest_host}:\n  it rolls the destination back and destroys the snapshots and changes newer than the last common one.\n\nSave this job? (y/n)"
                )
            })
        }
        KIND_SNAPSHOT => {
            let keep = new.keep?;
            let prefix = effective_prefix(new);
            let widened = old.is_none_or(|o| o.dataset != new.dataset || o.prefix != new.prefix || o.recursive != new.recursive);
            if !widened && old.and_then(|o| o.keep).is_some_and(|k| k <= keep) {
                return None;
            }
            let how_many = match snapshots {
                None => "How many exist now is unknown here.".to_string(),
                Some(list) => {
                    let mut per_dataset: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
                    for snap in list {
                        let in_scope = snap.dataset == dataset || (new.recursive && snap.dataset.starts_with(&format!("{dataset}/")));
                        if in_scope && crate::daemon::snapshot::is_own(prefix, &snap.name) {
                            *per_dataset.entry(snap.dataset.as_str()).or_default() += 1;
                        }
                    }
                    // The run takes one more snapshot per dataset before it prunes.
                    let doomed: usize = per_dataset.values().map(|c| (c + 1).saturating_sub(keep as usize)).sum();
                    if doomed == 0 {
                        return None;
                    }
                    format!("The next run would destroy {doomed} that exist now.")
                }
            };
            Some(format!(
                "Snapshot job for {dataset}, keep {keep}.\nEvery run destroys this job's own `{prefix}-<timestamp>` snapshots beyond the newest {keep}. {how_many}\n\nSave this job? (y/n)"
            ))
        }
        KIND_BACKUP => {
            let mut parts = Vec::new();
            if crate::jobs::backup_source(new) == crate::jobs::BackupSource::Fresh {
                let keep = new.keep.unwrap_or(crate::jobs::DEFAULT_FRESH_KEEP);
                let was_fresh = old.is_some_and(|o| crate::jobs::backup_source(o) == crate::jobs::BackupSource::Fresh && o.dataset == new.dataset);
                if !was_fresh || old.and_then(|o| o.keep).unwrap_or(crate::jobs::DEFAULT_FRESH_KEEP) > keep {
                    parts.push(format!(
                        "Every run takes a snapshot of {dataset} and then destroys this job's own `{}-<timestamp>` snapshots beyond the newest {keep}.",
                        crate::jobs::fresh_prefix(&new.id)
                    ));
                }
            }
            if let Some(retention) = new.retention.as_deref() {
                if old.is_none_or(|o| o.retention.as_deref() != Some(retention)) {
                    let repo = new.repository.as_deref().unwrap_or("?");
                    parts.push(format!("Every successful run keeps only {retention} of this job's backups in {repo} (`restic forget --prune`); older backups are deleted for good."));
                }
            }
            (!parts.is_empty()).then(|| format!("Backup job for {dataset}.\n{}\n\nSave this job? (y/n)", parts.join("\n")))
        }
        KIND_MIRROR => {
            let (remote, name) = (new.remote.as_deref().unwrap_or("?"), new.remote.as_deref().and_then(|r| r.split(':').next()).unwrap_or("?"));
            let changed = old.is_none_or(|o| o.remote != new.remote || o.dataset != new.dataset || o.path != new.path || o.source != new.source);
            changed.then(|| {
                let moved = if old.is_some_and(|o| o.path != new.path || o.source != new.source) {
                    "\nThe folder or snapshot it reads changed: files only the previous one had are moved to old/ on the next run."
                } else {
                    ""
                };
                format!(
                    "Cloud mirror of {dataset} to {remote}.\nEvery run makes {remote}/current match the dataset, using the rclone '{name}' remote's keys on that host. Files a run replaces or deletes are first moved to {remote}/old/<date and time>/; zfsctl never deletes anything in old/.{moved}\n\nSave this job? (y/n)"
                )
            })
        }
        _ => None,
    }
}

/// Builds the `PendingAction` for a submitted form, mapping its fields
/// positionally per `FormKind`'s documented field order. Returns `None`
/// if a required field is empty — the caller keeps the form modal open
/// in that case rather than losing the user's input.
pub(super) fn build_pending_action(kind: &FormKind, fields: &[FormField]) -> Option<PendingAction> {
    match kind {
        FormKind::CreatePool { host } => {
            let name = field_value(fields, 0).trim();
            let vdev_spec_raw = field_value(fields, 1).trim();
            if name.is_empty() || vdev_spec_raw.is_empty() {
                return None;
            }
            let vdev_spec: Vec<String> = vdev_spec_raw.split_whitespace().map(String::from).collect();
            Some(PendingAction {
                host: *host,
                section: Section::Pools,
                call: MutationCall::CreatePool { name: name.to_string(), vdev_spec },
            })
        }
        FormKind::CreatePoolFromDisks { host, disks } => {
            let name = field_value(fields, 0).trim();
            if name.is_empty() {
                return None;
            }
            let redundancy = field_value(fields, 1).trim();
            Some(PendingAction {
                host: *host,
                section: Section::Pools,
                call: MutationCall::CreatePool { name: name.to_string(), vdev_spec: build_vdev_spec_from_selection(redundancy, disks) },
            })
        }
        FormKind::AddVdev { host, .. } | FormKind::AddVdevFromDisks { host, .. } => {
            let (pool, vdev_spec, _) = add_vdev_request(kind, fields).ok()?;
            Some(PendingAction { host: *host, section: Section::Vdevs, call: MutationCall::AddVdev { pool, vdev_spec } })
        }
        FormKind::ReplaceDisk { host, pool, old_disk } => {
            let new_disk = field_value(fields, 0).trim();
            if new_disk.is_empty() {
                return None;
            }
            Some(PendingAction {
                host: *host,
                section: Section::Vdevs,
                call: MutationCall::ReplaceDisk {
                    pool: pool.clone(),
                    old_disk: old_disk.clone(),
                    new_disk: new_disk.to_string(),
                },
            })
        }
        FormKind::EditPoolProperty { host, pool, key } => {
            let value = field_value(fields, 0).to_string();
            Some(PendingAction {
                host: *host,
                section: Section::PoolProperties,
                call: MutationCall::SetPoolProperty { pool: pool.clone(), key: key.clone(), value },
            })
        }
        FormKind::CreateDataset { host } => {
            let name = field_value(fields, 0).trim();
            if name.is_empty() {
                return None;
            }
            Some(PendingAction {
                host: *host,
                section: Section::Datasets,
                call: MutationCall::CreateDataset { name: name.to_string() },
            })
        }
        FormKind::EditDatasetProperty { host, dataset, key } => {
            let value = field_value(fields, 0).to_string();
            Some(PendingAction {
                host: *host,
                section: Section::DatasetProperties,
                call: MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: key.clone(), value },
            })
        }
        FormKind::CreateSnapshot { host, dataset } => {
            let name = field_value(fields, 0).trim();
            if name.is_empty() {
                return None;
            }
            Some(PendingAction {
                host: *host,
                section: Section::Snapshots,
                call: MutationCall::CreateSnapshot { dataset: dataset.clone(), name: name.to_string() },
            })
        }
        FormKind::ShareNfs { host, dataset } => {
            let networks: Vec<String> = field_value(fields, 0).split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let hosts: Vec<String> = field_value(fields, 1).split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let read_only = field_value(fields, 2).trim().to_lowercase().starts_with('y');
            let root = field_value(fields, 3).trim();
            let root = if root.is_empty() { None } else { Some(root) };
            let value = crate::zfs::share::assemble_sharenfs(&networks, &hosts, read_only, root);
            Some(PendingAction {
                host: *host,
                section: Section::Shares,
                call: MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: "sharenfs".to_string(), value },
            })
        }
        FormKind::GrantPermission { host, dataset } => {
            let recipient = parse_recipient_field(field_value(fields, 0))?;
            let scope = parse_scope_field(field_value(fields, 1))?;
            let perms = parse_required_perms_field(field_value(fields, 2))?;
            Some(PendingAction {
                host: *host,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission {
                    dataset: dataset.clone(),
                    target: crate::zfs::permission::PermissionTarget::Grant { recipient, scope },
                    perms,
                },
            })
        }
        FormKind::GrantPermissionSet { host, dataset } => {
            let name = field_value(fields, 0).trim();
            if name.is_empty() || name.contains('@') {
                return None;
            }
            let perms = parse_required_perms_field(field_value(fields, 1))?;
            Some(PendingAction {
                host: *host,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission {
                    dataset: dataset.clone(),
                    target: crate::zfs::permission::PermissionTarget::Set(name.to_string()),
                    perms,
                },
            })
        }
        FormKind::GrantCreateTimePermission { host, dataset } => {
            let perms = parse_required_perms_field(field_value(fields, 0))?;
            Some(PendingAction {
                host: *host,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission { dataset: dataset.clone(), target: crate::zfs::permission::PermissionTarget::CreateTime, perms },
            })
        }
        FormKind::RevokePermissions { host, dataset, target } => {
            let perms = parse_required_perms_field(field_value(fields, 0))?;
            Some(PendingAction {
                host: *host,
                section: Section::DatasetPermissions,
                call: MutationCall::UnallowPermission { dataset: dataset.clone(), target: target.clone(), perms: Some(perms) },
            })
        }
        FormKind::AddAclEntry { host, path, position } => {
            let tag = parse_acl_recipient_field(field_value(fields, 0))?;
            let permissions = parse_required_perms_field(field_value(fields, 1))?;
            let flags = parse_optional_flags_field(field_value(fields, 2));
            let entry_type = parse_acl_type_field(field_value(fields, 3))?;
            Some(PendingAction {
                host: *host,
                section: Section::DatasetAcl,
                call: MutationCall::AddAclEntry {
                    path: path.clone(),
                    position: *position,
                    entry: crate::acl::AclEntry { tag, permissions, flags, entry_type },
                },
            })
        }
        // `FormKind::AddHost` never reaches here -- `handle_modal_key`
        // intercepts it before calling `build_pending_action` at all,
        // since there's no existing host/dataset/pool context for this
        // function's `PendingAction` to carry. This arm exists only so
        // the match stays exhaustive; it degrades safely (same as any
        // other invalid-for-this-function case) rather than panicking
        // if some future edit ever did route it here by mistake.
        //
        // `handle_modal_key` routes `ShareSmb` through `build_smb_action`
        // instead: it needs the current share list for the duplicate-name check.
        FormKind::NotifySettings | FormKind::Setting { .. } | FormKind::AddHost | FormKind::ShareSmb { .. } | FormKind::LoadKey { .. } | FormKind::ChangeKey { .. } | FormKind::CreateEncrypted { .. } | FormKind::CreateBootEnv { .. } | FormKind::RenameBootEnv { .. } | FormKind::BackupJob { .. } | FormKind::MirrorJob { .. } | FormKind::Job { .. } | FormKind::SnapshotJob { .. } | FormKind::ReplicateJob { .. } => None,
    }
}

/// The boot environment a boot-environment call is about (the old name for a rename).
pub(super) fn bootenv_call_name(call: &MutationCall) -> &str {
    match call {
        MutationCall::CreateBootEnv { name, .. }
        | MutationCall::ActivateBootEnv { name, .. }
        | MutationCall::ClearTemporaryBootEnv { name }
        | MutationCall::MountBootEnv { name }
        | MutationCall::UnmountBootEnv { name }
        | MutationCall::DestroyBootEnv { name, .. } => name,
        MutationCall::RenameBootEnv { from, .. } => from,
        _ => "",
    }
}

pub(super) fn build_bootenv_action(kind: &FormKind, fields: &[FormField]) -> std::result::Result<PendingAction, String> {
    use crate::zfs::bootenv::validate_name;
    match kind {
        FormKind::CreateBootEnv { host, existing } => {
            let name = field_value(fields, 0).trim();
            validate_name(name)?;
            if existing.iter().any(|e| e == name) {
                return Err(format!("A boot environment named '{name}' already exists"));
            }
            let from = field_value(fields, 1).trim();
            let from = if from.is_empty() {
                None
            } else if existing.iter().any(|e| e == from) {
                Some(from.to_string())
            } else {
                return Err(format!("There is no boot environment named '{from}' to clone"));
            };
            Ok(PendingAction { host: *host, section: Section::BootEnvs, call: MutationCall::CreateBootEnv { name: name.to_string(), from } })
        }
        FormKind::RenameBootEnv { host, from, existing } => {
            let to = field_value(fields, 0).trim();
            validate_name(to)?;
            if to == from {
                return Err("That is already its name".to_string());
            }
            if existing.iter().any(|e| e == to) {
                return Err(format!("A boot environment named '{to}' already exists"));
            }
            Ok(PendingAction { host: *host, section: Section::BootEnvs, call: MutationCall::RenameBootEnv { from: from.clone(), to: to.to_string() } })
        }
        _ => Err("not a boot environment form".to_string()),
    }
}

pub(super) fn smb_edit_modal(existing: Option<&crate::zfs::share::Share>, dataset: &str, path: &str, host: usize) -> Modal {
    let (name, user, read_only, existing_user, guest_ok) = if let Some(crate::zfs::share::Share::Smb { name, valid_user, read_only, guest_ok, .. }) = existing {
        (name.clone(), valid_user.clone().unwrap_or_default(), *read_only, valid_user.clone(), *guest_ok)
    } else {
        let suggestion = dataset.rsplit('/').next().unwrap_or("").to_string();
        let suggestion = if crate::zfs::smb::validate_share_name(&suggestion).is_ok() { suggestion } else { String::new() };
        (suggestion, String::new(), false, None, false)
    };
    Modal::Form {
        title: format!("SMB share: '{dataset}'"),
        fields: vec![
            FormField { label: "Share name".to_string(), value: name },
            FormField { label: "Valid user (blank = any Samba user)".to_string(), value: user },
            FormField { label: "Password (blank on edit = unchanged)".to_string(), value: String::new() },
            FormField { label: "Read-only (y/n)".to_string(), value: if read_only { "y".to_string() } else { "n".to_string() } },
            FormField { label: "Guest access (y/n)".to_string(), value: if guest_ok { "y".to_string() } else { "n".to_string() } },
        ],
        focused: 0,
        description: Some(format!("Shares {path} as \\\\<host>\\<name>. The Valid user must already be a Unix account; the password is its separate Samba password. Setting a password replaces that user's Samba password for every share they use. Blank user + guest n: any Samba user may connect. Guest y: anonymous access with no password (needs a blank user).")),
        kind: FormKind::ShareSmb { host, dataset: dataset.to_string(), path: path.to_string(), existing_user },
    }
}

/// Which key operation `L`, `U` or `C` asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum KeyOp {
    Load,
    Unload,
    Change,
}

/// The dataset a key operation acts on (the encryption root of `name`), or why it cannot be done. Passphrase keys only:
/// a raw or hex key lives in a file, which zfsctl does not handle yet.
pub(super) fn key_target(encryption: &std::collections::BTreeMap<String, crate::zfs::encryption::Enc>, name: &str, op: KeyOp) -> std::result::Result<String, String> {
    let Some(enc) = encryption.get(name) else {
        return Err(format!("{name} is not encrypted (if it just became one, press r to read it again)"));
    };
    if enc.key_format != "passphrase" {
        return Err(format!("{name} has a {} key kept in a file: load it at the shell with zfs load-key (key files are not handled here yet)", enc.key_format));
    }
    match op {
        KeyOp::Load if !enc.locked => Err(format!("The key of {} is already loaded", enc.root)),
        KeyOp::Unload if enc.locked => Err(format!("The key of {} is already locked", enc.root)),
        KeyOp::Change if enc.locked => Err(format!("{} is locked: load the key first, then change it", enc.root)),
        KeyOp::Change if enc.root != name => Err(format!("{name} uses the key of {root}: change the key on {root}", root = enc.root)),
        _ => Ok(enc.root.clone()),
    }
}

pub(super) fn load_key_form(host: usize, root: &str) -> Modal {
    Modal::Form {
        title: format!("Load key: '{root}'"),
        fields: vec![
            FormField { label: "Passphrase".to_string(), value: String::new() },
            FormField { label: "Mount it after loading (y/n)".to_string(), value: "y".to_string() },
        ],
        focused: 0,
        description: Some(format!("Loads the key of {root}; the datasets under it that use the same key unlock with it. The passphrase goes to zfs on its input, never in a command line, and is not kept.")),
        kind: FormKind::LoadKey { host, dataset: root.to_string() },
    }
}

pub(super) fn change_key_form(host: usize, root: &str) -> Modal {
    Modal::Form {
        title: format!("Change key: '{root}'"),
        fields: vec![
            FormField { label: "New passphrase (8 or more characters)".to_string(), value: String::new() },
            FormField { label: "Repeat".to_string(), value: String::new() },
        ],
        focused: 0,
        description: Some("Sets a new passphrase for this dataset and the ones that use its key. Nothing is stored by zfsctl: lose it and the data cannot be recovered.".to_string()),
        kind: FormKind::ChangeKey { host, dataset: root.to_string() },
    }
}

pub(super) fn create_encrypted_form(host: usize) -> Modal {
    Modal::Form {
        title: "Create encrypted dataset".to_string(),
        fields: vec![
            FormField { label: "Name (with its pool)".to_string(), value: String::new() },
            FormField { label: "Cipher".to_string(), value: crate::zfs::encryption::CIPHERS[0].to_string() },
            FormField { label: "Passphrase (8 or more characters)".to_string(), value: String::new() },
            FormField { label: "Repeat".to_string(), value: String::new() },
        ],
        focused: 0,
        description: Some("Encryption can only be chosen now: it cannot be added to or removed from a dataset later. Datasets created under it use the same key. zfsctl stores no key: if the passphrase is lost the data cannot be recovered by anyone.".to_string()),
        kind: FormKind::CreateEncrypted { host },
    }
}

/// Checks the form and builds the call. `parent_encrypted` says the dataset would sit under an encrypted one.
pub(super) fn build_create_encrypted(kind: &FormKind, fields: &[FormField], encryption: &std::collections::BTreeMap<String, crate::zfs::encryption::Enc>) -> std::result::Result<PendingAction, String> {
    let FormKind::CreateEncrypted { host } = kind else { return Err("not the encrypted-dataset form".to_string()) };
    let name = field_value(fields, 0).trim();
    if name.is_empty() || name.starts_with('-') || name.contains(char::is_whitespace) || !name.contains('/') {
        return Err("Name: give the full dataset name including its pool, such as tank/secure".to_string());
    }
    if let Some(parent) = name.rsplit_once('/').map(|(p, _)| p).filter(|p| encryption.contains_key(*p)) {
        return Err(format!("{parent} is encrypted: what is created under it uses its key already. Use n for a plain new dataset there"));
    }
    let cipher = field_value(fields, 1).trim();
    if !crate::zfs::encryption::CIPHERS.contains(&cipher) {
        return Err(format!("Cipher: one of {}", crate::zfs::encryption::CIPHERS.join(", ")));
    }
    let passphrase = field_value(fields, 2);
    crate::zfs::encryption::check_passphrase(passphrase, true)?;
    if passphrase != field_value(fields, 3) {
        return Err("The two passphrases do not match".to_string());
    }
    Ok(PendingAction { host: *host, section: Section::Datasets, call: MutationCall::CreateEncryptedDataset { name: name.to_string(), cipher: cipher.to_string(), passphrase: crate::ui::modal::Secret(passphrase.to_string()) } })
}

pub(super) fn build_key_action(kind: &FormKind, fields: &[FormField]) -> std::result::Result<PendingAction, String> {
    match kind {
        FormKind::LoadKey { host, dataset } => {
            let passphrase = field_value(fields, 0);
            crate::zfs::encryption::check_passphrase(passphrase, false)?;
            let mount = field_value(fields, 1).trim().to_lowercase().starts_with('y');
            Ok(PendingAction { host: *host, section: Section::Datasets, call: MutationCall::LoadKey { dataset: dataset.clone(), passphrase: crate::ui::modal::Secret(passphrase.to_string()), mount } })
        }
        FormKind::ChangeKey { host, dataset } => {
            let passphrase = field_value(fields, 0);
            crate::zfs::encryption::check_passphrase(passphrase, true)?;
            if passphrase != field_value(fields, 1) {
                return Err("The two passphrases do not match".to_string());
            }
            Ok(PendingAction { host: *host, section: Section::Datasets, call: MutationCall::ChangeKey { dataset: dataset.clone(), passphrase: crate::ui::modal::Secret(passphrase.to_string()) } })
        }
        _ => Err("not a key form".to_string()),
    }
}

pub(super) fn build_smb_action(kind: &FormKind, fields: &[FormField], shares: &[crate::zfs::share::Share]) -> std::result::Result<PendingAction, String> {
    let FormKind::ShareSmb { host, dataset, path, existing_user } = kind else {
        return Err("not an SMB form".to_string());
    };
    let name = field_value(fields, 0).trim();
    crate::zfs::smb::validate_share_name(name)?;
    let user = field_value(fields, 1).trim();
    if !user.is_empty() {
        crate::zfs::smb::validate_user(user)?;
    }
    let password = field_value(fields, 2); // verbatim: spaces can be part of a password
    let read_only = field_value(fields, 3).trim().to_lowercase().starts_with('y');
    let guest_ok = field_value(fields, 4).trim().to_lowercase().starts_with('y');
    if guest_ok && !user.is_empty() {
        return Err("Guest access can't be combined with a Valid user".to_string());
    }
    let clash = shares.iter().any(|s| matches!(s, crate::zfs::share::Share::Smb { dataset: d, name: n, .. } if d != dataset && n.eq_ignore_ascii_case(name)));
    if clash {
        return Err(format!("A share named '{name}' already exists on another dataset"));
    }
    let password = if user.is_empty() || guest_ok {
        None
    } else if password.is_empty() {
        if existing_user.as_deref() == Some(user) {
            None
        } else {
            return Err("A Samba password is required when setting or changing the valid user".to_string());
        }
    } else {
        Some(crate::ui::modal::Secret(password.to_string()))
    };
    Ok(PendingAction {
        host: *host,
        section: Section::Shares,
        call: MutationCall::SetSmbShare {
            share: crate::zfs::smb::SmbShare {
                name: name.to_string(),
                dataset: dataset.clone(),
                path: path.clone(),
                valid_user: if user.is_empty() { None } else { Some(user.to_string()) },
                read_only,
                guest_ok,
            },
            password,
        },
    })
}

/// Parses the `Recipient` field's mini-language: `everyone`,
/// `user:<name>`, or `group:<name>` — no bare-name fallback, since this
/// app always passes explicit `-u`/`-g` to `zfs allow` (see the spec).
pub(super) fn pool_root_message(pool: &str) -> String {
    format!("'{pool}' is a pool: destroying it as a dataset would remove everything in it. Use the Pools section (d) to destroy a pool")
}

/// Splits `user:carol`, ` User : carol `, `GROUP:wheel` into a lower-case keyword and a trimmed name (the
/// name keeps its case). `None` when there is no colon or the name is blank.
pub(super) fn split_recipient(s: &str) -> Option<(String, &str)> {
    let (keyword, name) = s.split_once(':')?;
    let name = name.trim();
    (!name.is_empty()).then(|| (keyword.trim().to_ascii_lowercase(), name))
}

pub(super) fn parse_recipient_field(s: &str) -> Option<crate::zfs::permission::Recipient> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("everyone") {
        return Some(crate::zfs::permission::Recipient::Everyone);
    }
    match split_recipient(s)? {
        (keyword, name) if keyword == "user" => Some(crate::zfs::permission::Recipient::User(name.to_string())),
        (keyword, name) if keyword == "group" => Some(crate::zfs::permission::Recipient::Group(name.to_string())),
        _ => None,
    }
}

/// Parses the `Scope` field — blank defaults to `Both`, anything else
/// must be exactly `local`/`descendent`/`both` (case-insensitive) or
/// the caller must treat it as invalid. Deliberately strict (unlike
/// most optional fields in this app): silently falling back to the
/// *widest* scope on a typo would be a real footgun for a permissions
/// feature specifically.
pub(super) fn parse_scope_field(s: &str) -> Option<crate::zfs::permission::Scope> {
    let s = s.trim();
    if s.is_empty() {
        return Some(crate::zfs::permission::Scope::Both);
    }
    if s.eq_ignore_ascii_case("local") {
        return Some(crate::zfs::permission::Scope::Local);
    }
    if s.eq_ignore_ascii_case("descendent") {
        return Some(crate::zfs::permission::Scope::Descendent);
    }
    if s.eq_ignore_ascii_case("both") {
        return Some(crate::zfs::permission::Scope::Both);
    }
    None
}

/// Parses a required comma-separated `Permissions` field — splits,
/// trims, drops empty tokens; `None` if that leaves nothing (blank, or
/// just commas/whitespace), since there's no sensible "blank means X"
/// default for a permission list (unlike Shares' optional fields).
pub(super) fn parse_required_perms_field(s: &str) -> Option<Vec<String>> {
    let perms: Vec<String> = s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
    // A token starting with `-` would be read as an option by zfs/setfacl: refuse it here, with the form open.
    if perms.is_empty() || perms.iter().any(|p| p.starts_with('-')) {
        None
    } else {
        Some(perms)
    }
}

/// What is wrong with a required permissions field, in the words the form shows.
pub(super) fn perms_problem(s: &str) -> String {
    match s.split(',').map(str::trim).find(|p| p.starts_with('-')) {
        Some(token) => format!("Permissions: '{token}' is not a permission (it starts with '-')"),
        None => "Permissions: choose at least one (F2 lists them)".to_string(),
    }
}

/// The reason `build_pending_action` returned `None` for this form, naming the field, so the form can stay
/// open with the typing intact and say what to fix.
pub(super) fn form_problem(kind: &FormKind, fields: &[FormField]) -> String {
    let value = |i: usize| field_value(fields, i);
    match kind {
        FormKind::GrantPermission { .. } => {
            if parse_recipient_field(value(0)).is_none() {
                "Recipient: use everyone, user:<name> or group:<name>".to_string()
            } else if parse_scope_field(value(1)).is_none() {
                "Scope: local, descendent or both (blank means both)".to_string()
            } else {
                perms_problem(value(2))
            }
        }
        FormKind::GrantPermissionSet { .. } => {
            let name = value(0).trim();
            if name.is_empty() {
                "Set name: required".to_string()
            } else if name.contains('@') {
                "Set name: type it without the leading '@'".to_string()
            } else {
                perms_problem(value(1))
            }
        }
        FormKind::GrantCreateTimePermission { .. } | FormKind::RevokePermissions { .. } => perms_problem(value(0)),
        FormKind::AddVdev { .. } | FormKind::AddVdevFromDisks { .. } => add_vdev_request(kind, fields).err().unwrap_or_else(|| "Could not submit".to_string()),
        FormKind::AddAclEntry { .. } => {
            if parse_acl_recipient_field(value(0)).is_none() {
                "Recipient: use owner, group, everyone, user:<name> or group:<name>".to_string()
            } else if parse_required_perms_field(value(1)).is_none() {
                perms_problem(value(1))
            } else {
                "Type: allow or deny (blank means allow)".to_string()
            }
        }
        _ => "Could not submit -- check the field values against the hints in their labels".to_string(),
    }
}

/// Parses the ACL `Recipient` field's mini-language: `owner`, `group`,
/// `everyone`, `user:<name>`, or `group:<name>` — five shapes (one more
/// than the Permissions sub-tab's three, since ACLs distinguish the
/// owning group `group@` from a specific named group).
pub(super) fn parse_acl_recipient_field(s: &str) -> Option<crate::acl::AclTag> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("owner") {
        return Some(crate::acl::AclTag::Owner);
    }
    if s.eq_ignore_ascii_case("group") {
        return Some(crate::acl::AclTag::OwningGroup);
    }
    if s.eq_ignore_ascii_case("everyone") {
        return Some(crate::acl::AclTag::Everyone);
    }
    match split_recipient(s)? {
        (keyword, name) if keyword == "user" => Some(crate::acl::AclTag::User(name.to_string())),
        (keyword, name) if keyword == "group" => Some(crate::acl::AclTag::NamedGroup(name.to_string())),
        _ => None,
    }
}

/// Parses the ACL `Type` field — blank defaults to `Allow` (the more
/// common case), anything else must be exactly `allow`/`deny`
/// (case-insensitive). Deliberately strict, same reasoning as
/// Permissions' `Scope` field: silently defaulting a typo to the
/// *less restrictive* `allow` would be a real footgun.
pub(super) fn parse_acl_type_field(s: &str) -> Option<crate::acl::AclEntryType> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("allow") {
        return Some(crate::acl::AclEntryType::Allow);
    }
    if s.eq_ignore_ascii_case("deny") {
        return Some(crate::acl::AclEntryType::Deny);
    }
    None
}

/// Parses an optional comma-separated `Flags` field — splits, trims,
/// drops empty tokens; blank (or just commas/whitespace) is valid and
/// means no inheritance flags, unlike the required `Permissions` field.
pub(super) fn parse_optional_flags_field(s: &str) -> Vec<String> {
    s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// Reconstructs the `PermissionTarget` a `PermissionRow` was flattened
/// from — used by `'d'`/`'x'` to revoke exactly the target the selected
/// row represents.
pub(super) fn permission_target_for_row(row: &PermissionRow) -> crate::zfs::permission::PermissionTarget {
    match row {
        PermissionRow::Set { name, .. } => crate::zfs::permission::PermissionTarget::Set(name.clone()),
        PermissionRow::CreateTime { .. } => crate::zfs::permission::PermissionTarget::CreateTime,
        PermissionRow::Grant { recipient, scope, .. } => {
            crate::zfs::permission::PermissionTarget::Grant { recipient: recipient.clone(), scope: *scope }
        }
    }
}
