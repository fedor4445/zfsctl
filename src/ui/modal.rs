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
    OfflineDisk { pool: String, disk: String },
    OnlineDisk { pool: String, disk: String },
    ReplaceDisk { pool: String, old_disk: String, new_disk: String },
    SetPoolProperty { pool: String, key: String, value: String },
    CreateDataset { name: String },
    DestroyDataset { name: String },
    SetDatasetProperty { dataset: String, key: String, value: String },
    CreateSnapshot { dataset: String, name: String },
    DestroySnapshot { dataset: String, name: String },
    RollbackSnapshot { dataset: String, name: String },
    AllowPermission { dataset: String, target: crate::zfs::permission::PermissionTarget, perms: Vec<String> },
    UnallowPermission { dataset: String, target: crate::zfs::permission::PermissionTarget, perms: Option<Vec<String>> },
    AddAclEntry { path: String, position: usize, entry: crate::acl::AclEntry },
    RemoveAclEntry { path: String, position: usize },
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
    /// Deliberately has no wildcard arm: adding a `MutationCall` variant
    /// must not compile until someone decides how alarming it is.
    pub fn tone(&self) -> Tone {
        match self {
            MutationCall::DestroyPool { .. }
            | MutationCall::DestroyDataset { .. }
            | MutationCall::DestroySnapshot { .. }
            | MutationCall::RollbackSnapshot { .. } => Tone::Danger,
            MutationCall::OfflineDisk { .. }
            | MutationCall::ReplaceDisk { .. }
            | MutationCall::UnallowPermission { .. }
            | MutationCall::RemoveAclEntry { .. } => Tone::Warn,
            MutationCall::ScrubPool { .. }
            | MutationCall::ResilverPool { .. }
            | MutationCall::OnlineDisk { .. }
            | MutationCall::CreatePool { .. }
            | MutationCall::AddVdev { .. }
            | MutationCall::CreateDataset { .. }
            | MutationCall::CreateSnapshot { .. }
            | MutationCall::AllowPermission { .. }
            | MutationCall::AddAclEntry { .. }
            | MutationCall::SetPoolProperty { .. }
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
            MutationCall::OfflineDisk { .. } => "OFFLINE DISK",
            MutationCall::OnlineDisk { .. } => "ONLINE DISK",
            MutationCall::ReplaceDisk { .. } => "REPLACE DISK",
            MutationCall::SetPoolProperty { .. } => "SET POOL PROPERTY",
            MutationCall::CreateDataset { .. } => "CREATE DATASET",
            MutationCall::DestroyDataset { .. } => "DESTROY DATASET",
            MutationCall::SetDatasetProperty { .. } => "SET DATASET PROPERTY",
            MutationCall::CreateSnapshot { .. } => "CREATE SNAPSHOT",
            MutationCall::DestroySnapshot { .. } => "DESTROY SNAPSHOT",
            MutationCall::RollbackSnapshot { .. } => "ROLLBACK SNAPSHOT",
            MutationCall::AllowPermission { .. } => "GRANT PERMISSION",
            MutationCall::UnallowPermission { .. } => "REVOKE PERMISSION",
            MutationCall::AddAclEntry { .. } => "ADD ACL ENTRY",
            MutationCall::RemoveAclEntry { .. } => "REMOVE ACL ENTRY",
        }
    }
}

impl MutationCall {
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
            MutationCall::OfflineDisk { pool, disk } => crate::zfs::pool::offline_disk(runner, pool, disk),
            MutationCall::OnlineDisk { pool, disk } => crate::zfs::pool::online_disk(runner, pool, disk),
            MutationCall::ReplaceDisk { pool, old_disk, new_disk } => {
                crate::zfs::pool::replace_disk(runner, pool, old_disk, new_disk)
            }
            MutationCall::SetPoolProperty { pool, key, value } => crate::zfs::pool::set_pool_property(runner, pool, key, value),
            MutationCall::CreateDataset { name } => crate::zfs::dataset::create_dataset(runner, name),
            MutationCall::DestroyDataset { name } => crate::zfs::dataset::destroy_dataset(runner, name),
            MutationCall::SetDatasetProperty { dataset, key, value } => {
                crate::zfs::dataset::set_dataset_property(runner, dataset, key, value)
            }
            MutationCall::CreateSnapshot { dataset, name } => crate::zfs::snapshot::create_snapshot(runner, dataset, name),
            MutationCall::DestroySnapshot { dataset, name } => crate::zfs::snapshot::destroy_snapshot(runner, dataset, name),
            MutationCall::RollbackSnapshot { dataset, name } => crate::zfs::snapshot::rollback_snapshot(runner, dataset, name),
            MutationCall::AllowPermission { dataset, target, perms } => crate::zfs::permission::allow(runner, dataset, target, perms),
            MutationCall::UnallowPermission { dataset, target, perms } => {
                crate::zfs::permission::unallow(runner, dataset, target, perms.as_deref())
            }
            MutationCall::AddAclEntry { path, position, entry } => crate::acl::add_acl_entry(runner, path, *position, entry),
            MutationCall::RemoveAclEntry { path, position } => crate::acl::remove_acl_entry(runner, path, *position),
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

/// Which mutation a `Modal::Form` is collecting input for, plus the
/// context (host, and any row data fixed at modal-open time) that
/// doesn't come from the form's fields themselves. Field values are
/// read positionally in `build_pending_action` — the order here must
/// match the order the modal's `fields` were constructed in.
#[derive(Debug, Clone, PartialEq)]
pub enum FormKind {
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
    /// Fields: `[name, ssh_target]` — both required. Unlike every
    /// other `FormKind`, submitting this one never produces a
    /// `PendingAction`: there's no existing host/runner to dispatch
    /// against, since the whole point is that the host doesn't exist
    /// yet. `handle_modal_key` special-cases this variant instead of
    /// routing it through `build_pending_action`.
    AddHost,
}

/// A modal dialog awaiting user input before a mutation runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// Simple y/n confirmation for reversible/lower-risk operations.
    Confirm { prompt: String, action: PendingAction },
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

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
        assert_eq!(calls[0], ("zfs".to_string(), vec!["destroy".to_string(), "--".to_string(), "tank/olds".to_string()]));
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
        let runner = FakeRunner::new("");
        let call = MutationCall::DestroySnapshot { dataset: "tank/data".to_string(), name: "old-1".to_string() };
        call.run(&runner).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["destroy".to_string(), "--".to_string(), "tank/data@old-1".to_string()]));
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
}
