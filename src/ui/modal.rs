use crate::error::Result;
use crate::exec::CommandRunner;
use crate::ui::state::Section;

/// Names a concrete `zfs::*` mutation call, with exactly the arguments
/// that function needs. Built from a modal's collected input (or, for
/// `Confirm`/`ConfirmTyped`, from the row that was selected when the
/// modal opened) and run on a worker thread once the modal is
/// confirmed/submitted.
#[derive(Debug, Clone, PartialEq)]
pub enum MutationCall {
    ScrubPool { pool: String },
    ResilverPool { pool: String },
    DestroyPool { pool: String },
    CreatePool { name: String, vdev_spec: Vec<String> },
    AddVdev { pool: String, vdev_spec: Vec<String> },
    /// `zpool remove` of a log, cache or spare device.
    RemoveVdev { pool: String, device: String },
    OfflineDisk { pool: String, disk: String },
    OnlineDisk { pool: String, disk: String },
    ReplaceDisk { pool: String, old_disk: String, new_disk: String },
    SetPoolProperty { pool: String, key: String, value: String },
    CreateDataset { name: String },
    /// `zfs create` of a passphrase-encrypted dataset; the passphrase goes in on stdin.
    CreateEncryptedDataset { name: String, cipher: String, passphrase: Secret },
    DestroyDataset { name: String },
    DestroyDatasetRecursive { name: String },
    SetDatasetProperty { dataset: String, key: String, value: String },
    CreateSnapshot { dataset: String, name: String },
    HoldSnapshot { dataset: String, name: String },
    ReleaseSnapshot { dataset: String, name: String },
    DestroySnapshot { dataset: String, name: String },
    RollbackSnapshot { dataset: String, name: String },
    AllowPermission { dataset: String, target: crate::zfs::permission::PermissionTarget, perms: Vec<String> },
    UnallowPermission { dataset: String, target: crate::zfs::permission::PermissionTarget, perms: Option<Vec<String>> },
    AddAclEntry { path: String, position: usize, entry: crate::acl::AclEntry },
    RemoveAclEntry { path: String, position: usize },
    SetSmbShare { share: crate::zfs::smb::SmbShare, password: Option<Secret> },
    RemoveSmbShare { dataset: String },
    /// `zfs load-key` of an encryption root, then (when `mount`) `zfs mount` of it. The passphrase goes in on stdin.
    LoadKey { dataset: String, passphrase: Secret, mount: bool },
    /// `zfs unload-key`: the data cannot be read again until the key is loaded.
    UnloadKey { dataset: String },
    /// `zfs change-key` of an encryption root: the old passphrase stops working, the data is not re-encrypted.
    ChangeKey { dataset: String, passphrase: Secret },
    CreateBootEnv { name: String, from: Option<String> },
    ActivateBootEnv { name: String, temporary: bool },
    ClearTemporaryBootEnv { name: String },
    RenameBootEnv { from: String, to: String },
    MountBootEnv { name: String },
    UnmountBootEnv { name: String },
    DestroyBootEnv { name: String, origin: bool },
    /// Creates or replaces a scheduled job in `<state_dir>/jobs.toml`. Local file edits: the runner is not used.
    SaveJob { state_dir: std::path::PathBuf, job: Box<crate::jobs::Job> },
    /// Adds several new jobs to `jobs.toml` in one go (one snapshot job for each marked dataset). A local file edit.
    SaveJobs { state_dir: std::path::PathBuf, jobs: Vec<crate::jobs::Job> },
    /// Adds `host:dataset` entries to `[protection] ignore` in `config.toml` (the ones already there stay). A local file edit.
    IgnoreDatasets { config_path: std::path::PathBuf, entries: Vec<String> },
    /// Takes `host:dataset` entries out of `[protection] ignore` again, so they are checked once more. A local file edit.
    UnignoreDatasets { config_path: std::path::PathBuf, entries: Vec<String> },
    DeleteJob { state_dir: std::path::PathBuf, id: String },
    SetJobEnabled { state_dir: std::path::PathBuf, id: String, enabled: bool },
    /// Replaces the `[notify]` section of `config.toml`, nothing else in the file. A local file edit: the
    /// runner is not used.
    SaveNotify { config_path: std::path::PathBuf, section: String },
    /// Writes one Settings row (`setting` with the form's `values`) into `config.toml`, checked again against the
    /// file as it is at that moment. A local file edit: the runner is not used.
    SaveSettings { config_path: std::path::PathBuf, setting: crate::ui::settings::Setting, values: Vec<String> },
    /// Sets or clears `tag` and `site` in the `[[hosts]]` blocks of `names`, nothing else in the file. A local file edit.
    SaveHostKeys { config_path: std::path::PathBuf, names: Vec<String>, tag: crate::config::KeyUpdate, site: crate::config::KeyUpdate },
    /// Sends a real test message through the saved channels (`zfsctl notify-test`). The runner is not used.
    SendTestMessage { settings: Box<crate::config::NotifySettings>, hook: Option<String> },
    /// Copies one file out of a snapshot to a new name next to the original (`cp -p -n`, run as root by the engine in
    /// `zfs::snapshot_restore`, which checks the paths before and the result after). Never replaces anything; it is not a
    /// destroy, not recursive and not forced. `host` is the host's name, for the engine's messages. Boxed: the plan and the
    /// source are large.
    RestoreFile { plan: Box<crate::zfs::snapshot_restore::RestorePlan>, source: Box<crate::zfs::snapshot_restore::RestoreSource>, host: String },
}

/// A password or passphrase held only until it is piped to the command that needs it (`smbpasswd`, `zfs load-key`).
/// `Debug` never prints it, so a logged or panicking `PendingAction` can't leak it, and the memory is zeroed when it drops.
#[derive(Clone, PartialEq)]
pub struct Secret(pub String);

impl Drop for Secret {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// How alarming an action's confirmation should look. Drives the modal
/// border/title style and the hint bar's confirm key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Warn,
    Danger,
}

impl MutationCall {
    /// What to say after the call succeeded, instead of the blank status every call leaves.
    pub fn done_note(&self) -> Option<String> {
        match self {
            MutationCall::SaveNotify { .. } => Some("Alert settings saved; the daemon applies them within seconds".to_string()),
            MutationCall::SaveSettings { .. } => Some("Saved to config.toml".to_string()),
            MutationCall::IgnoreDatasets { entries, .. } => Some(format!("Stopped checking {} dataset{} (Settings, Ignored datasets, lists them)", entries.len(), if entries.len() == 1 { "" } else { "s" })),
            MutationCall::UnignoreDatasets { entries, .. } => Some(format!("Checking {} dataset{} again", entries.len(), if entries.len() == 1 { "" } else { "s" })),
            MutationCall::SaveJobs { jobs, .. } => Some(format!("Created {} snapshot job{}", jobs.len(), if jobs.len() == 1 { "" } else { "s" })),
            MutationCall::SaveHostKeys { names, .. } => Some(format!("Saved to config.toml for {}", if names.len() == 1 { names.join("") } else { format!("{} hosts", names.len()) })),
            MutationCall::HoldSnapshot { dataset, name } => Some(format!("Protected {dataset}@{name}")),
            MutationCall::ReleaseSnapshot { dataset, name } => Some(format!("Released the zfsctl hold on {dataset}@{name}")),
            MutationCall::SendTestMessage { .. } => Some("Test message sent: the mail command accepted it (run from this terminal; the daemon runs it with root's home)".to_string()),
            _ => None,
        }
    }
}

impl MutationCall {
    /// Deliberately has no wildcard arm: adding a `MutationCall` variant
    /// must not compile until someone decides how alarming it is.
    pub fn tone(&self) -> Tone {
        match self {
            MutationCall::DestroyPool { .. }
            | MutationCall::DestroyDataset { .. }
            | MutationCall::DestroyDatasetRecursive { .. }
            | MutationCall::DestroySnapshot { .. }
            | MutationCall::RollbackSnapshot { .. }
            | MutationCall::DestroyBootEnv { .. } => Tone::Danger,
            MutationCall::OfflineDisk { .. }
            | MutationCall::ReplaceDisk { .. }
            | MutationCall::CreatePool { .. }
            | MutationCall::AddVdev { .. }
            | MutationCall::RemoveVdev { .. }
            | MutationCall::UnallowPermission { .. }
            | MutationCall::RemoveAclEntry { .. }
            | MutationCall::RemoveSmbShare { .. }
            | MutationCall::UnloadKey { .. }
            | MutationCall::ChangeKey { .. }
            | MutationCall::CreateEncryptedDataset { .. }
            | MutationCall::ActivateBootEnv { temporary: false, .. }
            | MutationCall::ReleaseSnapshot { .. }
            | MutationCall::DeleteJob { .. } => Tone::Warn,
            MutationCall::ScrubPool { .. }
            | MutationCall::ResilverPool { .. }
            | MutationCall::OnlineDisk { .. }
            | MutationCall::CreateDataset { .. }
            | MutationCall::CreateSnapshot { .. }
            | MutationCall::HoldSnapshot { .. }
            | MutationCall::RestoreFile { .. }
            | MutationCall::AllowPermission { .. }
            | MutationCall::AddAclEntry { .. }
            | MutationCall::SetPoolProperty { .. }
            | MutationCall::SaveJob { .. }
            | MutationCall::SaveJobs { .. }
            | MutationCall::IgnoreDatasets { .. }
            | MutationCall::UnignoreDatasets { .. }
            | MutationCall::SaveNotify { .. }
            | MutationCall::SaveSettings { .. }
            | MutationCall::SaveHostKeys { .. }
            | MutationCall::SendTestMessage { .. }
            | MutationCall::SetJobEnabled { .. }
            | MutationCall::SetSmbShare { .. }
            | MutationCall::LoadKey { .. }
            | MutationCall::ActivateBootEnv { temporary: true, .. }
            | MutationCall::CreateBootEnv { .. }
            | MutationCall::ClearTemporaryBootEnv { .. }
            | MutationCall::RenameBootEnv { .. }
            | MutationCall::MountBootEnv { .. }
            | MutationCall::UnmountBootEnv { .. }
            | MutationCall::SetDatasetProperty { .. } => Tone::Neutral,
        }
    }

    /// Short capitalised name shown in a danger/warn modal's title.
    pub fn label(&self) -> &'static str {
        match self {
            MutationCall::ScrubPool { .. } => "SCRUB POOL",
            MutationCall::ResilverPool { .. } => "RESILVER POOL",
            MutationCall::DestroyPool { .. } => "DESTROY POOL",
            MutationCall::CreatePool { .. } => "CREATE POOL",
            MutationCall::AddVdev { .. } => "ADD VDEV",
            MutationCall::RemoveVdev { .. } => "REMOVE VDEV",
            MutationCall::OfflineDisk { .. } => "OFFLINE DISK",
            MutationCall::OnlineDisk { .. } => "ONLINE DISK",
            MutationCall::ReplaceDisk { .. } => "REPLACE DISK",
            MutationCall::SetPoolProperty { .. } => "SET POOL PROPERTY",
            MutationCall::CreateDataset { .. } => "CREATE DATASET",
            MutationCall::DestroyDataset { .. } => "DESTROY DATASET",
            MutationCall::DestroyDatasetRecursive { .. } => "DESTROY DATASET AND EVERYTHING UNDER IT",
            MutationCall::SetDatasetProperty { .. } => "SET DATASET PROPERTY",
            MutationCall::CreateSnapshot { .. } => "CREATE SNAPSHOT",
            MutationCall::HoldSnapshot { .. } => "HOLD SNAPSHOT",
            MutationCall::ReleaseSnapshot { .. } => "RELEASE HOLD",
            MutationCall::DestroySnapshot { .. } => "DESTROY SNAPSHOT",
            MutationCall::RollbackSnapshot { .. } => "ROLLBACK SNAPSHOT",
            MutationCall::AllowPermission { .. } => "GRANT PERMISSION",
            MutationCall::UnallowPermission { .. } => "REVOKE PERMISSION",
            MutationCall::AddAclEntry { .. } => "ADD ACL ENTRY",
            MutationCall::RemoveAclEntry { .. } => "REMOVE ACL ENTRY",
            MutationCall::SetSmbShare { .. } => "SET SMB SHARE",
            MutationCall::RemoveSmbShare { .. } => "REMOVE SMB SHARE",
            MutationCall::CreateEncryptedDataset { .. } => "CREATE ENCRYPTED DATASET",
            MutationCall::LoadKey { .. } => "LOAD KEY",
            MutationCall::UnloadKey { .. } => "UNLOAD KEY",
            MutationCall::ChangeKey { .. } => "CHANGE KEY",
            MutationCall::CreateBootEnv { .. } => "CREATE BOOT ENV",
            MutationCall::ActivateBootEnv { .. } => "ACTIVATE BOOT ENV",
            MutationCall::ClearTemporaryBootEnv { .. } => "CLEAR TEMPORARY BOOT ENV",
            MutationCall::RenameBootEnv { .. } => "RENAME BOOT ENV",
            MutationCall::MountBootEnv { .. } => "MOUNT BOOT ENV",
            MutationCall::UnmountBootEnv { .. } => "UNMOUNT BOOT ENV",
            MutationCall::DestroyBootEnv { .. } => "DESTROY BOOT ENV",
            MutationCall::SaveJob { .. } => "SAVE JOB",
            MutationCall::SaveJobs { .. } => "SAVE JOBS",
            MutationCall::IgnoreDatasets { .. } => "STOP CHECKING DATASETS",
            MutationCall::UnignoreDatasets { .. } => "CHECK DATASETS AGAIN",
            MutationCall::DeleteJob { .. } => "DELETE JOB",
            MutationCall::SetJobEnabled { .. } => "ENABLE OR DISABLE JOB",
            MutationCall::SaveNotify { .. } => "SAVE ALERT SETTINGS",
            MutationCall::SaveSettings { .. } => "SAVE SETTING",
            MutationCall::SaveHostKeys { .. } => "SAVE HOST TAG AND SITE",
            MutationCall::SendTestMessage { .. } => "SEND TEST MESSAGE",
            MutationCall::RestoreFile { .. } => "RESTORE AS A COPY",
        }
    }
}

/// What the status line says after a restore: the verified copy as the engine found it.
pub fn restored_note(done: &crate::zfs::snapshot_restore::Restored) -> String {
    let clean = crate::display_text::sanitise_for_display;
    format!("Restored {} ({} bytes, {}, owner {})", clean(&done.destination), done.size, clean(&done.mode), clean(&done.owner))
}

impl MutationCall {
    /// Runs a `RestoreFile` and returns what the engine verified (`None` for every other call). The only call whose
    /// success text depends on what the host answered, so the worker uses this instead of `run_on` to get the text.
    pub fn restore_outcome(&self, runner: &dyn CommandRunner) -> Option<Result<crate::zfs::snapshot_restore::Restored>> {
        let MutationCall::RestoreFile { plan, source, host } = self else { return None };
        Some(crate::zfs::snapshot_restore::run_restore(runner, host, plan, source, &std::sync::atomic::AtomicBool::new(false)))
    }

    /// The single place that maps a `MutationCall` variant to its
    /// underlying `zfs::*` library call.
    pub fn run(&self, runner: &dyn CommandRunner) -> Result<()> {
        match self {
            MutationCall::ScrubPool { pool } => crate::zfs::pool::scrub_pool(runner, pool),
            MutationCall::ResilverPool { pool } => crate::zfs::pool::resilver_pool(runner, pool),
            MutationCall::DestroyPool { pool } => crate::zfs::pool::destroy_pool(runner, pool),
            MutationCall::CreatePool { name, vdev_spec } => {
                let spec: Vec<&str> = vdev_spec.iter().map(String::as_str).collect();
                crate::zfs::pool::create_pool(runner, name, &spec)
            }
            MutationCall::AddVdev { pool, vdev_spec } => {
                let spec: Vec<&str> = vdev_spec.iter().map(String::as_str).collect();
                crate::zfs::pool::add_vdev(runner, pool, &spec)
            }
            MutationCall::RemoveVdev { pool, device } => crate::zfs::pool::remove_vdev(runner, pool, device),
            MutationCall::OfflineDisk { pool, disk } => crate::zfs::pool::offline_disk(runner, pool, disk),
            MutationCall::OnlineDisk { pool, disk } => crate::zfs::pool::online_disk(runner, pool, disk),
            MutationCall::ReplaceDisk { pool, old_disk, new_disk } => {
                crate::zfs::pool::replace_disk(runner, pool, old_disk, new_disk)
            }
            MutationCall::SetPoolProperty { pool, key, value } => crate::zfs::pool::set_pool_property(runner, pool, key, value),
            MutationCall::CreateDataset { name } => crate::zfs::dataset::create_dataset(runner, name),
            MutationCall::DestroyDataset { name } => crate::zfs::dataset::destroy_dataset(runner, name),
            MutationCall::DestroyDatasetRecursive { name } => crate::zfs::dataset::destroy_dataset_recursive(runner, name),
            MutationCall::SetDatasetProperty { dataset, key, value } => {
                crate::zfs::dataset::set_dataset_property(runner, dataset, key, value)
            }
            MutationCall::CreateSnapshot { dataset, name } => crate::zfs::snapshot::create_snapshot(runner, dataset, name),
            MutationCall::HoldSnapshot { dataset, name } => crate::zfs::snapshot::hold_snapshot(runner, dataset, name),
            MutationCall::ReleaseSnapshot { dataset, name } => crate::zfs::snapshot::release_snapshot(runner, dataset, name),
            MutationCall::DestroySnapshot { dataset, name } => crate::zfs::snapshot::destroy_snapshot(runner, dataset, name),
            MutationCall::RollbackSnapshot { dataset, name } => crate::zfs::snapshot::rollback_snapshot(runner, dataset, name),
            MutationCall::AllowPermission { dataset, target, perms } => crate::zfs::permission::allow(runner, dataset, target, perms),
            MutationCall::UnallowPermission { dataset, target, perms } => {
                crate::zfs::permission::unallow(runner, dataset, target, perms.as_deref())
            }
            MutationCall::AddAclEntry { path, position, entry } => crate::acl::add_acl_entry(runner, path, *position, entry),
            MutationCall::RemoveAclEntry { path, position } => crate::acl::remove_acl_entry(runner, path, *position),
            MutationCall::SetSmbShare { share, password } => {
                crate::zfs::smb::set_share(runner, share, password.as_ref().map(|p| p.0.as_str()))
            }
            MutationCall::RemoveSmbShare { dataset } => crate::zfs::smb::remove_share(runner, dataset),
            MutationCall::LoadKey { dataset, passphrase, mount } => crate::zfs::encryption::load_key_and_mount(runner, dataset, &passphrase.0, *mount),
            MutationCall::CreateEncryptedDataset { name, cipher, passphrase } => crate::zfs::encryption::create_encrypted(runner, name, cipher, &passphrase.0),
            MutationCall::UnloadKey { dataset } => crate::zfs::encryption::unload_key(runner, dataset),
            MutationCall::ChangeKey { dataset, passphrase } => crate::zfs::encryption::change_key(runner, dataset, &passphrase.0),
            MutationCall::CreateBootEnv { name, from } => crate::zfs::bootenv::create(runner, name, from.as_deref()),
            MutationCall::ActivateBootEnv { name, temporary } => crate::zfs::bootenv::activate(runner, name, *temporary),
            MutationCall::ClearTemporaryBootEnv { name } => crate::zfs::bootenv::clear_temporary(runner, name),
            MutationCall::RenameBootEnv { from, to } => crate::zfs::bootenv::rename(runner, from, to),
            MutationCall::MountBootEnv { name } => crate::zfs::bootenv::mount(runner, name),
            MutationCall::UnmountBootEnv { name } => crate::zfs::bootenv::unmount(runner, name),
            MutationCall::DestroyBootEnv { name, origin } => crate::zfs::bootenv::destroy(runner, name, *origin),
            MutationCall::SaveJob { state_dir, job } => crate::jobs::upsert(&state_dir.join("jobs.toml"), job.as_ref().clone()),
            MutationCall::IgnoreDatasets { config_path, entries } => {
                let now: Vec<String> = crate::config::Config::load(config_path).ok().map(|c| crate::config::parse_ignore(c.protection.ignore.as_ref()).0).unwrap_or_default().into_iter().map(|(h, d)| format!("{h}:{d}")).collect();
                let mut all = now;
                for entry in entries {
                    if !all.contains(entry) {
                        all.push(entry.clone());
                    }
                }
                crate::config_edit::save(config_path, &[crate::config_edit::Change::set(&["protection", "ignore"], all.into_iter().collect::<toml_edit::Array>())])
            }
            MutationCall::UnignoreDatasets { config_path, entries } => {
                let now: Vec<String> = crate::config::Config::load(config_path).ok().map(|c| crate::config::parse_ignore(c.protection.ignore.as_ref()).0).unwrap_or_default().into_iter().map(|(h, d)| format!("{h}:{d}")).collect();
                let kept: toml_edit::Array = now.into_iter().filter(|e| !entries.contains(e)).collect();
                crate::config_edit::save(config_path, &[crate::config_edit::Change::set(&["protection", "ignore"], kept)])
            }
            MutationCall::SaveJobs { state_dir, jobs } => jobs.iter().try_for_each(|job| crate::jobs::add_new(&state_dir.join("jobs.toml"), job.clone())),
            MutationCall::DeleteJob { state_dir, id } => crate::jobs::remove(&state_dir.join("jobs.toml"), id),
            MutationCall::SetJobEnabled { state_dir, id, enabled } => crate::jobs::set_enabled(&state_dir.join("jobs.toml"), id, *enabled),
            MutationCall::SaveNotify { config_path, section } => crate::config::save_notify_section(config_path, section),
            MutationCall::SaveHostKeys { config_path, names, tag, site } => crate::config::set_host_keys_in_file(config_path, names, tag, site),
            MutationCall::SaveSettings { config_path, setting, values } => {
                let changes = crate::ui::settings::build_changes_for(config_path, setting, values).map_err(crate::error::AppError::Config)?;
                crate::config_edit::save(config_path, &changes)
            }
            MutationCall::RestoreFile { plan, source, host } => {
                // There is no cancel key for a restore, so the flag is never raised.
                crate::zfs::snapshot_restore::run_restore(runner, host, plan, source, &std::sync::atomic::AtomicBool::new(false)).map(|_| ())
            }
            MutationCall::SendTestMessage { settings, hook } => {
                let (text, code) = crate::notify_cli::notify_test(settings, hook.as_deref(), chrono::Utc::now());
                if code == 0 { Ok(()) } else { Err(crate::error::AppError::Config(text.trim().to_string())) }
            }
        }
    }
}

/// Everything needed to actually run a mutation once its modal is
/// confirmed: which host to run it against, which section to re-list
/// on success, and the call itself.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingAction {
    pub host: usize,
    pub section: Section,
    pub call: MutationCall,
}

/// A single labeled text input in a `Modal::Form`.
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    pub label: String,
    pub value: String,
}

impl Drop for FormField {
    /// A form is closed by dropping it: what was typed (a passphrase, in the key forms) is zeroed, not left in freed memory.
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.value);
    }
}

/// Which mutation a `Modal::Form` is collecting input for, plus the
/// context (host, and any row data fixed at modal-open time) that
/// doesn't come from the form's fields themselves. Field values are
/// read positionally in `build_pending_action` — the order here must
/// match the order the modal's `fields` were constructed in.
#[derive(Debug, Clone, PartialEq)]
pub enum FormKind {
    /// Fields: `[email to, mail command, mail from, check every (minutes), daily report at (HH:MM or off), remind
    /// every (hours)]`; a blank field is unset (the default).
    NotifySettings,
    /// The Settings section: one row's fields, in the order of `settings::form_fields`.
    Setting { setting: crate::ui::settings::Setting },
    /// Fields: `[host, pool, schedule, catch_up]`. `hosts` are the configured host names (for the
    /// host picker and for validation); `existing` is the job being edited, if any; `taken_ids`
    /// are the job ids already in use, so a new job can be named without a collision.
    Job { hosts: Vec<String>, existing: Option<Box<crate::jobs::Job>>, taken_ids: Vec<String> },
    /// Fields: `[host, dataset, recursive (y/n), prefix, keep, schedule, catch_up (y/n)]`. `jobs`
    /// is every current job, so the form can refuse a prefix another job already uses on the
    /// same host and dataset and can name a new job without a collision.
    SnapshotJob { hosts: Vec<String>, existing: Option<Box<crate::jobs::Job>>, jobs: Vec<crate::jobs::Job> },
    /// Fields: `[source host, source dataset, prefix, destination host (or file), destination dataset (or directory), force (y/n), schedule, catch_up (y/n), keep on target, raw (y/n)]`.
    ReplicateJob { hosts: Vec<String>, existing: Option<Box<crate::jobs::Job>>, jobs: Vec<crate::jobs::Job> },
    /// Fields: `[name, vdev_spec]`.
    CreatePool { host: usize },
    /// Fields: `[new_disk]`.
    ReplaceDisk { host: usize, pool: String, old_disk: String },
    /// Fields: `[name, redundancy]` — `redundancy` blank means a stripe
    /// (each disk its own top-level vdev); `disks` is fixed at
    /// modal-open time from the Disks section's current selection.
    CreatePoolFromDisks { host: usize, disks: Vec<String> },
    /// Fields: `[pool, redundancy]` — same `redundancy` convention as
    /// `CreatePoolFromDisks`.
    AddVdevFromDisks { host: usize, disks: Vec<String> },
    /// Fields: `[vdev_spec]`.
    AddVdev { host: usize, pool: String },
    /// Fields: `[value]` — pre-filled with the property's current value.
    EditPoolProperty { host: usize, pool: String, key: String },
    /// Fields: `[name]`.
    CreateDataset { host: usize },
    /// Fields: `[name, cipher, passphrase, repeat]`; the last two are masked.
    CreateEncrypted { host: usize },
    /// Fields: `[value]` — pre-filled with the property's current value.
    EditDatasetProperty { host: usize, dataset: String, key: String },
    /// Fields: `[name]` — the snapshot's name; the dataset is fixed at
    /// modal-open time from the selected Datasets row.
    CreateSnapshot { host: usize, dataset: String },
    /// Fields: `[networks, hosts, read_only, root]` — `networks`/
    /// `hosts` are comma-separated lists, `read_only` is `"y"`/`"n"`
    /// text, `root` is a username; every field may be blank.
    ShareNfs { host: usize, dataset: String },
    /// Fields: `[recipient, scope, permissions]` — `recipient` is
    /// `"everyone"` / `"user:<name>"` / `"group:<name>"`, `scope` is
    /// `"local"`/`"descendent"`/`"both"` (blank defaults to `"both"`),
    /// `permissions` is a required comma-separated list.
    GrantPermission { host: usize, dataset: String },
    /// Fields: `[set_name, permissions]` — both required; `set_name` is
    /// the bare name without the leading `@`.
    GrantPermissionSet { host: usize, dataset: String },
    /// Fields: `[permissions]` — required.
    GrantCreateTimePermission { host: usize, dataset: String },
    /// Fields: `[permissions]` — required; `target` is fixed at
    /// modal-open time from the selected Permissions-sub-tab row, not
    /// user-entered.
    RevokePermissions { host: usize, dataset: String, target: crate::zfs::permission::PermissionTarget },
    /// Fields: `[recipient, permissions, flags, type]` — `recipient` is
    /// `"owner"` / `"group"` / `"everyone"` / `"user:<name>"` /
    /// `"group:<name>"`, `permissions` is a required comma-separated
    /// list, `flags` is an optional comma-separated list (blank means
    /// no inheritance flags), `type` is `"allow"`/`"deny"` (blank
    /// defaults to `"allow"`). `position` is fixed at modal-open time
    /// (always the current entry count — new entries are appended).
    AddAclEntry { host: usize, path: String, position: usize },
    /// Fields: `[name, valid_user, password, read_only, guest]` -- a blank
    /// `valid_user` with guest `n` means any authenticated Samba user may
    /// connect; guest `y` (blank user only) means anonymous access and the
    /// password is ignored; a blank `password` on edit keeps
    /// the existing Samba password (only valid when the user is
    /// unchanged, see `existing_user`). The password field is masked.
    ShareSmb { host: usize, dataset: String, path: String, existing_user: Option<String> },
    /// Fields: `[passphrase, mount after loading (y/n)]`; `dataset` is the encryption root. The passphrase field is masked.
    LoadKey { host: usize, dataset: String },
    /// Fields: `[new passphrase, repeat]`; `dataset` is the encryption root. Both fields are masked.
    ChangeKey { host: usize, dataset: String },
    /// Fields: `[name, clone_from]` -- a blank `clone_from` clones the running boot environment
    /// (bectl's default). `existing` is every current name, for the duplicate check and the F2 picker.
    CreateBootEnv { host: usize, existing: Vec<String> },
    /// Fields: `[new_name]`; `from` is fixed at modal-open time.
    RenameBootEnv { host: usize, from: String, existing: Vec<String> },
    /// Fields: `[host, dataset, path, source, snapshots to keep, repository, password file, keep, schedule, catch_up (y/n)]`.
    /// `source` is `new`, `@snapshot` or a prefix; `snapshots to keep` only matters for `new`. `keep` is blank
    /// (keep everything) or text like `7d 4w 12m 3y`. `jobs` is every current job.
    BackupJob { hosts: Vec<String>, existing: Option<Box<crate::jobs::Job>>, jobs: Vec<crate::jobs::Job> },
    /// Fields: `[host, dataset, path, source, snapshots to keep, remote, speed limit, schedule, catch_up (y/n)]`.
    /// The first five are the backup form's; `remote` is an rclone `name:path`; `speed limit` is blank or like `5M`.
    MirrorJob { hosts: Vec<String>, existing: Option<Box<crate::jobs::Job>>, jobs: Vec<crate::jobs::Job> },
    /// Fields: `[name, ssh_target]` — both required. Unlike every
    /// other `FormKind`, submitting this one never produces a
    /// `PendingAction`: there's no existing host/runner to dispatch
    /// against, since the whole point is that the host doesn't exist
    /// yet. `handle_modal_key` special-cases this variant instead of
    /// routing it through `build_pending_action`.
    AddHost,
    /// Fields: `[tag, site]` of the `[[hosts]]` blocks named in `names`. With one host the fields hold its current values and
    /// empty clears; with several they start empty, empty leaves the key alone and `-` clears it.
    EditHosts { names: Vec<String> },
    /// Fields: `[restore as]` -- the new name for a file restored from a snapshot. `source` is the picked file, fixed when
    /// the form opens; `host` is the host the browser was reading. Submitting builds the plan and opens a confirm.
    RestoreFile { host: usize, source: Box<crate::zfs::snapshot_restore::RestoreSource> },
}

impl FormKind {
    /// Whether the form field at `index` must be shown as `*` characters.
    pub fn masks_field(&self, index: usize) -> bool {
        match self {
            FormKind::ShareSmb { .. } => index == 2,
            FormKind::LoadKey { .. } => index == 0,
            FormKind::CreateEncrypted { .. } => index >= 2,
            FormKind::ChangeKey { .. } => index <= 1,
            _ => false,
        }
    }
}

/// A modal dialog awaiting user input before a mutation runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// y/n confirmation. Two steps, both outcomes: `y` or `n`/Esc only *select* (`choice`), and Enter
    /// applies the selection, so a stray key or a held-down Enter can never run (or dismiss) it.
    Confirm { prompt: String, action: PendingAction, choice: Option<bool> },
    /// Type-the-exact-name confirmation for irreversible operations.
    ConfirmTyped {
        prompt: String,
        expected: String,
        input: String,
        action: PendingAction,
    },
    /// Labeled text-field input for creation operations. No
    /// confirmation beyond submitting the form itself.
    Form {
        title: String,
        fields: Vec<FormField>,
        focused: usize,
        /// A one-line synopsis of what's being edited, shown above the
        /// fields — currently only populated for property edits (via
        /// `zfs::property::describe`); every other form leaves this
        /// `None`.
        description: Option<String>,
        kind: FormKind,
    },
    /// A read-only overlay with no action to confirm or submit — Help
    /// and About. Dismissed by any key.
    Info { title: String, body: String },
    /// A list to choose a row from: Up and Down move, Enter acts on the row, Esc closes. `note` is shown above the rows.
    Choose { title: String, note: String, rows: Vec<String>, selected: usize, then: ChooseThen, look: ChooseLook },
}

/// How a `Modal::Choose` is drawn beyond its rows: headings between groups of rows, and a colour per row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChooseLook {
    /// `(row, text)`: a heading line before that row (a blank line separates it from the group above).
    pub heads: Vec<(usize, String)>,
    /// One level per row (red, amber...), or empty for plain rows.
    pub levels: Vec<crate::ui::dashboard::Level>,
    /// One flag per row when rows can be marked with Space (the unprotected list), or empty when they cannot.
    pub marked: Vec<bool>,
}

/// What Enter does with the chosen row of a `Modal::Choose`.
#[derive(Debug, Clone, PartialEq)]
pub enum ChooseThen {
    /// The rows are unprotected datasets (this list, same order): open the fix for the chosen one.
    Unprotected(Vec<crate::daemon::protection::Unprotected>),
    /// The rows are the kinds of copy (replication, backup, cloud mirror) for this dataset.
    Copy { host: String, dataset: String },
    /// The rows are the providers of `cloud_guide::PROVIDERS` (this order): show the steps for the chosen one.
    Provider,
    /// The rows are the ignored `host:dataset` entries (this list, same order): Enter or `u` checks the marked ones, or the chosen one, again.
    Ignored(Vec<String>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    fn job(id: &str) -> crate::jobs::Job {
        crate::jobs::Job { id: id.into(), host: "local".into(), kind: crate::jobs::KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * 0".into(), catch_up: true, enabled: true, created_at: None, dataset: None, recursive: false, prefix: None, keep: None, dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, remote: None, bwlimit: None, force: false, raw: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None }
    }

    #[test]
    fn job_mutations_edit_jobs_toml_and_never_touch_the_runner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.toml");
        let runner = FakeRunner::new("");
        let state_dir = dir.path().to_path_buf();
        MutationCall::SaveJob { state_dir: state_dir.clone(), job: Box::new(job("a")) }.run(&runner).unwrap();
        MutationCall::SaveJob { state_dir: state_dir.clone(), job: Box::new(job("b")) }.run(&runner).unwrap();
        MutationCall::SetJobEnabled { state_dir: state_dir.clone(), id: "a".into(), enabled: false }.run(&runner).unwrap();
        assert_eq!(crate::jobs::load(&path).unwrap().iter().map(|j| j.enabled).collect::<Vec<_>>(), vec![false, true]);
        MutationCall::DeleteJob { state_dir, id: "a".into() }.run(&runner).unwrap();
        assert_eq!(crate::jobs::load(&path).unwrap().iter().map(|j| j.id.as_str()).collect::<Vec<_>>(), vec!["b"]);
        assert!(runner.calls.lock().unwrap().is_empty(), "no command was run: these are local file edits");
    }

    #[test]
    fn only_deleting_a_job_asks_for_a_warning_confirmation() {
        let state_dir = std::path::PathBuf::from("/x");
        assert_eq!(MutationCall::DeleteJob { state_dir: state_dir.clone(), id: "a".into() }.tone(), Tone::Warn);
        assert_eq!(MutationCall::SaveJob { state_dir: state_dir.clone(), job: Box::new(job("a")) }.tone(), Tone::Neutral);
        assert_eq!(MutationCall::SetJobEnabled { state_dir, id: "a".into(), enabled: true }.tone(), Tone::Neutral);
    }

    #[test]
    fn secret_never_appears_in_debug_output() {
        let call = MutationCall::SetSmbShare {
            share: crate::zfs::smb::SmbShare { name: "a".into(), dataset: "tank/a".into(), path: "/tank/a".into(), valid_user: Some("bob".into()), read_only: false, guest_ok: false },
            password: Some(Secret("hunter2".to_string())),
        };
        let shown = format!("{call:?}");
        assert!(!shown.contains("hunter2"));
        assert!(shown.contains("Secret(***)"), "the redaction marker must be printed, not nothing: {shown}");
    }

    #[test]
    fn smb_calls_run_the_library_and_removal_is_warn_toned() {
        let runner = FakeRunner::new("");
        MutationCall::RemoveSmbShare { dataset: "tank/none".into() }.run(&runner).unwrap_err(); // nothing managed there
        assert_eq!(MutationCall::RemoveSmbShare { dataset: "x".into() }.tone(), Tone::Warn);
    }

    #[test]
    fn key_calls_run_through_the_library_and_never_print_the_passphrase() {
        let secret = "correct horse battery";
        let runner = FakeRunner::new("").route("zfs", "get", "keylocation\tprompt\nkeyformat\tpassphrase\nencryptionroot\ttank/s\ncanmount\ton\n");
        let load = MutationCall::LoadKey { dataset: "tank/s".into(), passphrase: Secret(secret.into()), mount: true };
        let change = MutationCall::ChangeKey { dataset: "tank/s".into(), passphrase: Secret(secret.into()) };
        load.run(&runner).unwrap();
        change.run(&runner).unwrap();
        MutationCall::UnloadKey { dataset: "tank/s".into() }.run(&runner).unwrap();
        let verbs: Vec<String> = runner.calls.lock().unwrap().iter().map(|(_, a)| a[0].clone()).collect();
        assert_eq!(verbs, ["get", "load-key", "mount", "get", "change-key", "unload-key"]);
        assert!(!format!("{load:?}{change:?}").contains(secret));
        assert_eq!((load.tone(), change.tone()), (Tone::Neutral, Tone::Warn));
        assert_eq!(MutationCall::UnloadKey { dataset: "x".into() }.tone(), Tone::Warn);
    }

    #[test]
    fn only_the_smb_password_field_is_masked() {
        let kind = FormKind::ShareSmb { host: 0, dataset: "d".into(), path: "/d".into(), existing_user: None };
        assert!(kind.masks_field(2));
        assert!(!kind.masks_field(0) && !kind.masks_field(1) && !kind.masks_field(3));
        assert!(!FormKind::CreatePool { host: 0 }.masks_field(2));
        let change = FormKind::ChangeKey { host: 0, dataset: "d".into() };
        assert!(change.masks_field(0) && change.masks_field(1) && !change.masks_field(2), "both key fields are masked");
        let load = FormKind::LoadKey { host: 0, dataset: "d".into() };
        assert!(load.masks_field(0) && !load.masks_field(1));
    }

    #[test]
    fn scrub_pool_call_runs_scrub_pool() {
        let runner = FakeRunner::new("");
        let call = MutationCall::ScrubPool { pool: "tank".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["scrub".to_string(), "--".to_string(), "tank".to_string()]));
    }

    #[test]
    fn resilver_pool_call_runs_resilver_pool() {
        let runner = FakeRunner::new("");
        let call = MutationCall::ResilverPool { pool: "tank".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["resilver".to_string(), "--".to_string(), "tank".to_string()]));
    }

    #[test]
    fn scrub_pool_call_surfaces_failure() {
        let runner = FakeRunner::failing("pool is busy", 1);
        let call = MutationCall::ScrubPool { pool: "tank".to_string() };
        assert!(call.run(&runner).is_err());
    }

    #[test]
    fn destroy_pool_call_runs_destroy_pool() {
        let runner = FakeRunner::new("");
        let call = MutationCall::DestroyPool { pool: "scratch".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["destroy".to_string(), "--".to_string(), "scratch".to_string()]));
    }

    #[test]
    fn create_pool_call_splits_vdev_spec_and_runs_create_pool() {
        let runner = FakeRunner::new("");
        let call = MutationCall::CreatePool {
            name: "tank".to_string(),
            vdev_spec: vec!["mirror".to_string(), "da0".to_string(), "da1".to_string()],
        };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            (
                "zpool".to_string(),
                vec!["create".to_string(), "--".to_string(), "tank".to_string(), "mirror".to_string(), "da0".to_string(), "da1".to_string()]
            )
        );
    }

    #[test]
    fn add_vdev_call_splits_vdev_spec_and_runs_add_vdev() {
        let runner = FakeRunner::new("");
        let call = MutationCall::AddVdev {
            pool: "tank".to_string(),
            vdev_spec: vec!["mirror".to_string(), "da2".to_string(), "da3".to_string()],
        };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            (
                "zpool".to_string(),
                vec!["add".to_string(), "--".to_string(), "tank".to_string(), "mirror".to_string(), "da2".to_string(), "da3".to_string()]
            )
        );
    }

    #[test]
    fn offline_disk_call_runs_offline_disk() {
        let runner = FakeRunner::new("");
        let call = MutationCall::OfflineDisk { pool: "tank".to_string(), disk: "da1".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["offline".to_string(), "--".to_string(), "tank".to_string(), "da1".to_string()]));
    }

    #[test]
    fn online_disk_call_runs_online_disk() {
        let runner = FakeRunner::new("");
        let call = MutationCall::OnlineDisk { pool: "tank".to_string(), disk: "da1".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["online".to_string(), "--".to_string(), "tank".to_string(), "da1".to_string()]));
    }

    #[test]
    fn replace_disk_call_runs_replace_disk() {
        let runner = FakeRunner::new("");
        let call = MutationCall::ReplaceDisk { pool: "tank".to_string(), old_disk: "da1".to_string(), new_disk: "da2".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zpool".to_string(), vec!["replace".to_string(), "--".to_string(), "tank".to_string(), "da1".to_string(), "da2".to_string()])
        );
    }

    #[test]
    fn create_dataset_call_runs_create_dataset() {
        let runner = FakeRunner::new("");
        let call = MutationCall::CreateDataset { name: "tank/newds".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["create".to_string(), "--".to_string(), "tank/newds".to_string()]));
    }

    #[test]
    fn destroy_dataset_call_runs_destroy_dataset() {
        let runner = FakeRunner::new("");
        let call = MutationCall::DestroyDataset { name: "tank/olds".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        let destroys: Vec<_> = calls.iter().filter(|c| c.1[0] == "destroy").collect();
        assert_eq!(*destroys[0], ("zfs".to_string(), vec!["destroy".to_string(), "--".to_string(), "tank/olds".to_string()]));
    }

    #[test]
    fn create_snapshot_call_runs_create_snapshot() {
        let runner = FakeRunner::new("");
        let call = MutationCall::CreateSnapshot { dataset: "tank/data".to_string(), name: "manual-1".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["snapshot".to_string(), "--".to_string(), "tank/data@manual-1".to_string()]));
    }

    #[test]
    fn destroy_snapshot_call_runs_destroy_snapshot() {
        let runner = FakeRunner::new("").route("zfs", "get", "tank/data@old-1\t0\n");
        let call = MutationCall::DestroySnapshot { dataset: "tank/data".to_string(), name: "old-1".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        let destroys: Vec<_> = calls.iter().filter(|c| c.1[0] == "destroy").collect();
        assert_eq!(destroys.len(), 1);
        assert_eq!(*destroys[0], ("zfs".to_string(), vec!["destroy".to_string(), "--".to_string(), "tank/data@old-1".to_string()]));
    }

    #[test]
    fn hold_call_runs_hold_snapshot() {
        let runner = FakeRunner::new("");
        let call = MutationCall::HoldSnapshot { dataset: "tank/data".to_string(), name: "s1".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["hold".to_string(), "--".to_string(), "zfsctl".to_string(), "tank/data@s1".to_string()]));
    }

    #[test]
    fn release_call_runs_release_snapshot() {
        let runner = FakeRunner::new("");
        let call = MutationCall::ReleaseSnapshot { dataset: "tank/data".to_string(), name: "s1".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["release".to_string(), "--".to_string(), "zfsctl".to_string(), "tank/data@s1".to_string()]));
    }

    /// A host with one folder: `ls` lists what the table holds, `cp` adds the destination to it.
    struct RestoreHost {
        table: std::sync::Mutex<Vec<(String, String)>>,
        cps: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl CommandRunner for RestoreHost {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<crate::exec::CommandOutput> {
            if cmd == "zfs" {
                if args[0] == "list" {
                    return Ok(crate::exec::CommandOutput { stdout: "tank/data\tyes\t/tank/data\n".to_string(), stderr: String::new(), status: 0 });
                }
                assert_eq!(args, ["get", "-H", "-o", "value", "normalization", "tank/data"]);
                return Ok(crate::exec::CommandOutput { stdout: "none\n".to_string(), stderr: String::new(), status: 0 });
            }
            assert_eq!(cmd, "cp");
            self.cps.lock().unwrap().push(args.iter().map(ToString::to_string).collect());
            let dest = args[args.len() - 1].rsplit('/').next().unwrap().to_string();
            self.table.lock().unwrap().push(("1234".to_string(), dest));
            Ok(crate::exec::CommandOutput { stdout: String::new(), stderr: String::new(), status: 0 })
        }
        fn run_with_stdin(&self, _c: &str, _a: &[&str], _s: &str) -> Result<crate::exec::CommandOutput> {
            Err(crate::error::AppError::Config("no stdin".into()))
        }
        fn run_unprivileged(&self, _c: &str, _a: &[&str]) -> Result<crate::exec::CommandOutput> {
            Err(crate::error::AppError::Config("no unprivileged".into()))
        }
        fn command(&self, _cmd: &str, args: &[&str]) -> Result<std::process::Command> {
            let dir = args[args.len() - 1];
            let rows: Vec<(String, String)> = if dir.contains(".zfs/snapshot") { vec![("1234".into(), "a.txt".into())] } else { self.table.lock().unwrap().clone() };
            let body = format!("total 1\n{}\n", rows.iter().map(|(size, name)| format!("-rw-r--r--  1 alice alice {size} 1700000000 {name}")).collect::<Vec<_>>().join("\n"));
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg("printf \"%s\" \"$0\"").arg(body);
            Ok(c)
        }
    }

    fn restore_call() -> MutationCall {
        use crate::zfs::snapshot_browse::{EntryKind, ListEntry};
        let entry = ListEntry { name: "a.txt".into(), kind: EntryKind::File, mode: "-rw-r--r--".into(), owner: "alice".into(), group: "alice".into(), size: 1234, time: None, link_target: None, ambiguous: false };
        let source = crate::zfs::snapshot_restore::RestoreSource { dataset: "tank/data".into(), snapshot: "s1".into(), mountpoint: "/tank/data".into(), folder: vec![], entry, };
        let plan = crate::zfs::snapshot_restore::plan_restore(&source, "a.restored-s1.txt").unwrap();
        MutationCall::RestoreFile { plan: Box::new(plan), source: Box::new(source), host: "hub".into() }
    }

    #[test]
    fn restore_call_runs_the_restore_engine_and_reports_what_it_verified() {
        let host = RestoreHost { table: std::sync::Mutex::new(vec![("1234".into(), "a.txt".into())]), cps: std::sync::Mutex::new(vec![]) };
        let call = restore_call();
        call.run(&host).unwrap();
        assert_eq!(host.cps.lock().unwrap().as_slice(), [["-p", "-n", "--", "/tank/data/.zfs/snapshot/s1/a.txt", "/tank/data/a.restored-s1.txt"].map(String::from).to_vec()]);
        let done = call.restore_outcome(&RestoreHost { table: std::sync::Mutex::new(vec![("1234".into(), "a.txt".into())]), cps: std::sync::Mutex::new(vec![]) }).unwrap().unwrap();
        assert_eq!(restored_note(&done), "Restored /tank/data/a.restored-s1.txt (1234 bytes, -rw-r--r--, owner alice)");
    }

    #[test]
    fn restore_call_stops_before_cp_when_the_name_is_taken_and_is_a_neutral_non_destroy() {
        let host = RestoreHost { table: std::sync::Mutex::new(vec![("1234".into(), "a.txt".into()), ("5".into(), "a.restored-s1.txt".into())]), cps: std::sync::Mutex::new(vec![]) };
        let err = restore_call().run(&host).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert!(host.cps.lock().unwrap().is_empty());
        assert_eq!(restore_call().tone(), Tone::Neutral);
        assert_eq!(restore_call().label(), "RESTORE AS A COPY");
        assert!(restore_call().done_note().is_none());
        assert!(MutationCall::ScrubPool { pool: "p".into() }.restore_outcome(&host).is_none());
    }

    #[test]
    fn tone_label_and_note_of_the_new_calls() {
        let hold = MutationCall::HoldSnapshot { dataset: "tank/a".into(), name: "s".into() };
        let release = MutationCall::ReleaseSnapshot { dataset: "tank/a".into(), name: "s".into() };
        assert_eq!(hold.tone(), Tone::Neutral);
        assert_eq!(release.tone(), Tone::Warn);
        assert_eq!(hold.label(), "HOLD SNAPSHOT");
        assert_eq!(release.label(), "RELEASE HOLD");
        assert_eq!(hold.done_note().unwrap(), "Protected tank/a@s");
        assert_eq!(release.done_note().unwrap(), "Released the zfsctl hold on tank/a@s");
    }

    #[test]
    fn rollback_snapshot_call_runs_rollback_snapshot() {
        let runner = FakeRunner::new("");
        let call = MutationCall::RollbackSnapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["rollback".to_string(), "--".to_string(), "tank/data@2026-09-01".to_string()]));
    }

    #[test]
    fn set_pool_property_call_runs_set_pool_property() {
        let runner = FakeRunner::new("");
        let call = MutationCall::SetPoolProperty { pool: "tank".to_string(), key: "comment".to_string(), value: "my pool".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zpool".to_string(), vec!["set".to_string(), "comment=my pool".to_string(), "tank".to_string()])
        );
    }

    #[test]
    fn set_dataset_property_call_runs_set_dataset_property() {
        let runner = FakeRunner::new("");
        let call = MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "compression".to_string(), value: "zstd".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zfs".to_string(), vec!["set".to_string(), "compression=zstd".to_string(), "tank/data".to_string()])
        );
    }
}

#[cfg(test)]
mod tone_tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    #[test]
    fn irreversible_actions_are_danger() {
        for call in [
            MutationCall::DestroyPool { pool: "tank".into() },
            MutationCall::DestroyDataset { name: "tank/a".into() },
            MutationCall::DestroySnapshot { dataset: "tank/a".into(), name: "s".into() },
            MutationCall::RollbackSnapshot { dataset: "tank/a".into(), name: "s".into() },
        ] {
            assert_eq!(call.tone(), Tone::Danger, "{call:?}");
        }
    }

    #[test]
    fn disruptive_but_recoverable_actions_are_warn() {
        for call in [
            MutationCall::OfflineDisk { pool: "tank".into(), disk: "da0".into() },
            MutationCall::ReplaceDisk { pool: "tank".into(), old_disk: "da0".into(), new_disk: "da1".into() },
            MutationCall::RemoveAclEntry { path: "/tank".into(), position: 0 },
        ] {
            assert_eq!(call.tone(), Tone::Warn, "{call:?}");
        }
    }

    #[test]
    fn routine_actions_are_neutral() {
        for call in [
            MutationCall::ScrubPool { pool: "tank".into() },
            MutationCall::OnlineDisk { pool: "tank".into(), disk: "da0".into() },
            MutationCall::CreateDataset { name: "tank/a".into() },
        ] {
            assert_eq!(call.tone(), Tone::Neutral, "{call:?}");
        }
    }

    #[test]
    fn labels_name_the_action_in_capitals() {
        assert_eq!(MutationCall::DestroyPool { pool: "t".into() }.label(), "DESTROY POOL");
        assert_eq!(MutationCall::RollbackSnapshot { dataset: "t".into(), name: "s".into() }.label(), "ROLLBACK SNAPSHOT");
        assert_eq!(MutationCall::OfflineDisk { pool: "t".into(), disk: "d".into() }.label(), "OFFLINE DISK");
    }

    #[test]
    fn boot_environment_calls_run_bectl_with_the_right_argv() {
        let cases: Vec<(MutationCall, Vec<&str>)> = vec![
            (MutationCall::CreateBootEnv { name: "n".into(), from: None }, vec!["create", "n"]),
            (MutationCall::CreateBootEnv { name: "n".into(), from: Some("o".into()) }, vec!["create", "-e", "o", "n"]),
            (MutationCall::ActivateBootEnv { name: "n".into(), temporary: false }, vec!["activate", "n"]),
            (MutationCall::ActivateBootEnv { name: "n".into(), temporary: true }, vec!["activate", "-t", "n"]),
            (MutationCall::ClearTemporaryBootEnv { name: "n".into() }, vec!["activate", "-T", "n"]),
            (MutationCall::RenameBootEnv { from: "a".into(), to: "b".into() }, vec!["rename", "a", "b"]),
            (MutationCall::MountBootEnv { name: "n".into() }, vec!["mount", "n"]),
            (MutationCall::UnmountBootEnv { name: "n".into() }, vec!["umount", "n"]),
            (MutationCall::DestroyBootEnv { name: "n".into(), origin: false }, vec!["destroy", "n"]),
            (MutationCall::DestroyBootEnv { name: "n".into(), origin: true }, vec!["destroy", "-o", "n"]),
        ];
        for (call, want) in cases {
            let runner = FakeRunner::new("");
            call.run(&runner).unwrap();
            let calls = runner.calls.lock().unwrap();
            assert_eq!(calls[0].0, "bectl", "{call:?}");
            assert_eq!(calls[0].1, want, "{call:?}");
        }
    }

    #[test]
    fn boot_environment_tones_match_how_hard_each_is_to_undo() {
        let tone = |c: MutationCall| c.tone();
        assert_eq!(tone(MutationCall::DestroyBootEnv { name: "n".into(), origin: false }), Tone::Danger);
        assert_eq!(tone(MutationCall::ActivateBootEnv { name: "n".into(), temporary: false }), Tone::Warn);
        assert_eq!(tone(MutationCall::ActivateBootEnv { name: "n".into(), temporary: true }), Tone::Neutral);
        for call in [
            MutationCall::CreateBootEnv { name: "n".into(), from: None },
            MutationCall::ClearTemporaryBootEnv { name: "n".into() },
            MutationCall::RenameBootEnv { from: "a".into(), to: "b".into() },
            MutationCall::MountBootEnv { name: "n".into() },
            MutationCall::UnmountBootEnv { name: "n".into() },
        ] {
            assert_eq!(call.tone(), Tone::Neutral, "{call:?}");
        }
    }

    #[test]
    fn a_failing_bectl_surfaces_through_the_mutation() {
        let runner = FakeRunner::failing("cannot destroy active boot environment", 5);
        let err = MutationCall::DestroyBootEnv { name: "default".into(), origin: false }.run(&runner).unwrap_err().to_string();
        assert!(err.contains("cannot destroy active boot environment"), "{err}");
    }


    #[test]
    fn destroying_a_dataset_recursively_uses_dash_r_never_dash_capital_r_and_is_danger_toned() {
        let runner = FakeRunner::new("");
        let call = MutationCall::DestroyDatasetRecursive { name: "tank/data".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        let destroy = calls.iter().find(|c| c.1[0] == "destroy").unwrap();
        assert_eq!(*destroy, ("zfs".to_string(), vec!["destroy".to_string(), "-r".to_string(), "--".to_string(), "tank/data".to_string()]));
        assert_eq!(call.tone(), Tone::Danger);
        assert!(!destroy.1.iter().any(|a| a == "-R"), "-R would also destroy clones elsewhere");
    }

    #[test]
    fn creating_a_pool_overwrites_disks_so_it_is_a_warning() {
        assert_eq!(MutationCall::CreatePool { name: "p".into(), vdev_spec: vec!["da0".into()] }.tone(), Tone::Warn);
    }

    #[test]
    fn save_notify_writes_the_section_through_the_config_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        MutationCall::SaveNotify { config_path: path.clone(), section: "[notify]\nmail_to = \"a@x.org\"\n".into() }.run(&crate::exec::test_support::FakeRunner::new("")).unwrap();
        assert!(std::fs::read_to_string(path).unwrap().contains("a@x.org"));
        let call = MutationCall::SaveNotify { config_path: "x".into(), section: String::new() };
        assert_eq!(call.tone(), Tone::Neutral);
        assert!(call.done_note().unwrap().contains("saved"));
        assert!(MutationCall::DeleteJob { state_dir: "x".into(), id: "j".into() }.done_note().is_none());
    }

    #[test]
    fn a_test_message_succeeds_only_when_every_channel_took_it() {
        let ok = crate::config::NotifyConfig { mail_to: Some("a@x.org".into()), mail_command: Some("cat > /dev/null".into()), ..Default::default() }.resolve().unwrap();
        let run = |s: &crate::config::NotifySettings, hook: Option<&str>| MutationCall::SendTestMessage { settings: Box::new(s.clone()), hook: hook.map(str::to_string) }.run(&crate::exec::test_support::FakeRunner::new(""));
        assert!(run(&ok, Some("true")).is_ok());
        let bad = crate::config::NotifyConfig { mail_to: Some("a@x.org".into()), mail_command: Some("exit 78".into()), ..Default::default() }.resolve().unwrap();
        assert!(run(&bad, None).unwrap_err().to_string().contains("mail: failed: exited with status 78"));
        let call = MutationCall::SendTestMessage { settings: Box::new(ok), hook: None };
        assert_eq!(call.tone(), Tone::Neutral);
        let note = call.done_note().unwrap();
        assert!(note.contains("accepted it") && note.contains("this terminal") && note.contains("daemon"), "a passing test must not claim more than it proves: {note}");
    }
}
