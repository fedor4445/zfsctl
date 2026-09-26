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
        kind: FormKind,
    },
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
