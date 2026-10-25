//! Key handling for pools, datasets and snapshots.
#![allow(clippy::wildcard_imports)]
use super::*;

impl App {
    /// The Pools section: a drilled pool's vdevs, the property pane and the pool list.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    pub(super) fn key_pools(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Down if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                move_table_selection(&mut self.vdevs_table, self.vdevs.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                move_table_selection(&mut self.vdevs_table, self.vdevs.len(), false);
            }
            KeyCode::Char('o') if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                self.open_vdev_confirm(false);
            }
            KeyCode::Char('O') if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                self.open_vdev_confirm(true);
            }
            KeyCode::Char('X') if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                let Some(pool) = self.drilled_pool.clone() else { return true };
                let picked = self.vdevs_table.selected().and_then(|i| crate::zfs::pool::removable_device(&self.vdevs, i));
                match picked {
                    Some((device, class)) => {
                        let host = self.hosts.get(self.selected_host).map_or("?", String::as_str);
                        let prompt = format!("On {host}:\n  zpool remove {pool} {device}\n\nThe {class} device is taken out of the pool; it can be added again later. Remove it? (y/n)");
                        self.modal = Some(Modal::Confirm { choice: None, prompt, action: PendingAction { host: self.selected_host, section: Section::Vdevs, call: MutationCall::RemoveVdev { pool, device } } });
                    }
                    None => self.status_message = Some("only a log, cache or spare device can be removed here (select its row)".to_string()),
                }
            }
            KeyCode::Char('x') if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                if let Some(i) = self.vdevs_table.selected() {
                    if let Some(vdev) = self.vdevs.get(i) {
                        if crate::zfs::pool::is_leaf_disk(&vdev.name) {
                            let Some(pool) = self.drilled_pool.clone() else { return true };
                            self.modal = Some(Modal::Form {
                                title: format!("Replace disk '{}' in pool '{pool}'", vdev.name),
                                fields: vec![FormField { label: "New device".to_string(), value: String::new() }],
                                focused: 0,
                                description: None,
                                kind: FormKind::ReplaceDisk { host: self.selected_host, pool, old_disk: vdev.name.clone() },
                            });
                        } else {
                            self.status_message = Some("select a disk (not a mirror/raidz group) to replace it".to_string());
                        }
                    }
                }
            }
            KeyCode::Char('a') if self.current_section() == Section::Pools => {
                // Only meaningful while drilled into a pool; otherwise `a` does nothing here.
                let Some(pool) = self.drilled_pool.clone() else { return true };
                self.modal = Some(Modal::Form {
                    title: format!("Add vdev to pool '{pool}'"),
                    fields: vec![FormField { label: "Vdev spec (e.g. mirror da2 da3, log da4, cache da5, special mirror da6 da7, spare da8; -f first to force)".to_string(), value: String::new() }],
                    focused: 0,
                    description: None,
                    kind: FormKind::AddVdev { host: self.selected_host, pool },
                });
            }
            KeyCode::Down if self.current_section() == Section::Pools && self.pane_focus == PaneFocus::Right => {
                move_table_selection(&mut self.pool_properties_table, self.pool_properties.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Pools && self.pane_focus == PaneFocus::Right => {
                move_table_selection(&mut self.pool_properties_table, self.pool_properties.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::Pools => {
                move_table_selection(&mut self.pools_table, self.pools.len(), true);
                self.pool_properties.clear();
                self.pool_properties_table.select(None);
                self.pool_properties_for = None;
            }
            KeyCode::Up if self.current_section() == Section::Pools => {
                move_table_selection(&mut self.pools_table, self.pools.len(), false);
                self.pool_properties.clear();
                self.pool_properties_table.select(None);
                self.pool_properties_for = None;
            }
            KeyCode::Char('s') if self.current_section() == Section::Pools && self.drilled_pool.is_none() && self.pane_focus == PaneFocus::Middle => {
                if let Some(i) = self.pools_table.selected() {
                    if let Some(pool) = self.pools.get(i) {
                        self.modal = Some(Modal::Confirm { choice: None,
                            prompt: format!("Scrub pool '{}'? (y/n)", pool.name),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Pools,
                                call: MutationCall::ScrubPool { pool: pool.name.clone() },
                            },
                        });
                    }
                }
            }
            KeyCode::Char('R') if self.current_section() == Section::Pools && self.drilled_pool.is_none() && self.pane_focus == PaneFocus::Middle => {
                if let Some(i) = self.pools_table.selected() {
                    if let Some(pool) = self.pools.get(i) {
                        self.modal = Some(Modal::Confirm { choice: None,
                            prompt: format!("Resilver pool '{}'? (y/n)", pool.name),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Pools,
                                call: MutationCall::ResilverPool { pool: pool.name.clone() },
                            },
                        });
                    }
                }
            }
            KeyCode::Char('d') if self.current_section() == Section::Pools && self.drilled_pool.is_none() && self.pane_focus == PaneFocus::Middle => {
                if let Some(i) = self.pools_table.selected() {
                    if let Some(pool) = self.pools.get(i) {
                        self.modal = Some(Modal::ConfirmTyped {
                            prompt: format!("Type the pool name to destroy '{}':", pool.name),
                            expected: pool.name.clone(),
                            input: String::new(),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Pools,
                                call: MutationCall::DestroyPool { pool: pool.name.clone() },
                            },
                        });
                    }
                }
            }
            KeyCode::Char('n') if self.current_section() == Section::Pools && self.drilled_pool.is_none() && self.pane_focus == PaneFocus::Middle => {
                self.modal = Some(Modal::Form {
                    title: "Create Pool".to_string(),
                    fields: vec![
                        FormField { label: "Name".to_string(), value: String::new() },
                        FormField { label: "Vdev spec (e.g. mirror da0 da1, or -o ashift=12 mirror da0 da1)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    description: None,
                    kind: FormKind::CreatePool { host: self.selected_host },
                });
            }
            _ => return false,
        }
        true
    }

    /// The Datasets section's right pane: the Properties, Permissions and ACL tabs.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    pub(super) fn key_dataset_detail(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Down if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => match self.dataset_detail_tab {
                DatasetDetailTab::Properties => move_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len(), true),
                DatasetDetailTab::Permissions => move_table_selection(&mut self.dataset_permissions_table, self.dataset_permissions.len(), true),
                DatasetDetailTab::Acl => move_table_selection(&mut self.acl_entries_table, self.acl_entries.len(), true),
            },
            KeyCode::Up if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => match self.dataset_detail_tab {
                DatasetDetailTab::Properties => move_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len(), false),
                DatasetDetailTab::Permissions => move_table_selection(&mut self.dataset_permissions_table, self.dataset_permissions.len(), false),
                DatasetDetailTab::Acl => move_table_selection(&mut self.acl_entries_table, self.acl_entries.len(), false),
            },
            KeyCode::Char('[') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => {
                self.dataset_detail_tab = self.dataset_detail_tab.prev();
            }
            KeyCode::Char(']') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => {
                self.dataset_detail_tab = self.dataset_detail_tab.next();
            }
            KeyCode::Char('n')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Permissions =>
            {
                if let Some(dataset) = self.selected_dataset_name() {
                    self.modal = Some(Modal::Form {
                        title: format!("Grant permission on '{dataset}'"),
                        fields: vec![
                            FormField { label: "Recipient (everyone / user:<name> / group:<name>)".to_string(), value: String::new() },
                            FormField { label: "Scope (local/descendent/both, blank=both)".to_string(), value: String::new() },
                            FormField { label: "Permissions (F2 to pick)".to_string(), value: String::new() },
                        ],
                        focused: 0,
                        description: None,
                        kind: FormKind::GrantPermission { host: self.selected_host, dataset },
                    });
                }
            }
            KeyCode::Char('s')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Permissions =>
            {
                if let Some(dataset) = self.selected_dataset_name() {
                    self.modal = Some(Modal::Form {
                        title: format!("Grant/add-to permission set on '{dataset}'"),
                        fields: vec![
                            FormField { label: "Set name (without @)".to_string(), value: String::new() },
                            FormField { label: "Permissions (F2 to pick)".to_string(), value: String::new() },
                        ],
                        focused: 0,
                        description: None,
                        kind: FormKind::GrantPermissionSet { host: self.selected_host, dataset },
                    });
                }
            }
            KeyCode::Char('c')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Permissions =>
            {
                if let Some(dataset) = self.selected_dataset_name() {
                    self.modal = Some(Modal::Form {
                        title: format!("Grant create-time permission on '{dataset}'"),
                        fields: vec![FormField { label: "Permissions (F2 to pick)".to_string(), value: String::new() }],
                        focused: 0,
                        description: None,
                        kind: FormKind::GrantCreateTimePermission { host: self.selected_host, dataset },
                    });
                }
            }
            KeyCode::Char('d')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Permissions =>
            {
                if let (Some(dataset), Some(i)) = (self.selected_dataset_name(), self.dataset_permissions_table.selected()) {
                    if let Some(row) = self.dataset_permissions.get(i) {
                        let target = permission_target_for_row(row);
                        // A `Grant` row's own `perms` is only the
                        // subset ZFS shows at *this* scope -- a
                        // same-recipient permission can also exist at a
                        // different scope (e.g. shown as a separate
                        // "Local" row alongside a "Local+Descendent"
                        // one for the same user). Passing `None` here
                        // would revoke everything for that
                        // recipient+scope, including permissions this
                        // row never displayed. `Set`/`CreateTime` rows
                        // have no such split -- the row already is the
                        // entire target, so `None` (confirmed live to
                        // delete the whole set/create-time delegation)
                        // is correct and unchanged for them.
                        let perms = match row {
                            PermissionRow::Grant { perms, .. } => Some(perms.clone()),
                            PermissionRow::Set { .. } | PermissionRow::CreateTime { .. } => None,
                        };
                        self.modal = Some(Modal::Confirm { choice: None,
                            prompt: format!("Revoke all permissions for this target on '{dataset}'? (y/n)"),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::DatasetPermissions,
                                call: MutationCall::UnallowPermission { dataset, target, perms },
                            },
                        });
                    }
                }
            }
            KeyCode::Char('x')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Permissions =>
            {
                if let (Some(dataset), Some(i)) = (self.selected_dataset_name(), self.dataset_permissions_table.selected()) {
                    if let Some(row) = self.dataset_permissions.get(i) {
                        let target = permission_target_for_row(row);
                        self.modal = Some(Modal::Form {
                            title: format!("Revoke specific permissions on '{dataset}'"),
                            fields: vec![FormField { label: "Permissions to remove (F2 to pick)".to_string(), value: String::new() }],
                            focused: 0,
                            description: None,
                            kind: FormKind::RevokePermissions { host: self.selected_host, dataset, target },
                        });
                    }
                }
            }
            // acltype=posix: this tab edits NFSv4 ACLs only. Say so instead of offering keys that cannot work.
            KeyCode::Char('n' | 'd')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Acl
                    && self.acl_unsupported().is_some() =>
            {
                self.status_message = self.acl_unsupported();
            }
            KeyCode::Char('n')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Acl =>
            {
                if let Some(path) = self.selected_dataset_mountpoint() {
                    if self.acl_entries_for.as_deref() == Some(path.as_str()) {
                        let position = self.acl_entries.len();
                        self.modal = Some(Modal::Form {
                            title: format!("Add ACL entry on '{path}'"),
                            fields: vec![
                                FormField { label: "Recipient (owner / group / everyone / user:<name> / group:<name>)".to_string(), value: String::new() },
                                FormField { label: "Permissions (F2 to pick)".to_string(), value: String::new() },
                                FormField { label: "Flags (blank = none, F2 to pick)".to_string(), value: String::new() },
                                FormField { label: "Type (allow/deny, blank=allow, F2 to pick)".to_string(), value: String::new() },
                            ],
                            focused: 0,
                            description: None,
                            kind: FormKind::AddAclEntry { host: self.selected_host, path, position },
                        });
                    } else {
                        self.status_message = Some("ACL not loaded yet -- press 'r' to refresh before adding an entry".to_string());
                    }
                }
            }
            KeyCode::Char('d')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Acl =>
            {
                if let (Some(path), Some(i)) = (self.selected_dataset_mountpoint(), self.acl_entries_table.selected()) {
                    if let Some(entry) = self.acl_entries.get(i) {
                        self.modal = Some(Modal::Confirm { choice: None,
                            prompt: format!("Remove ACL entry #{i} ({}) on '{path}'? (y/n)", entry.describe()),
                            action: PendingAction { host: self.selected_host, section: Section::DatasetAcl, call: MutationCall::RemoveAclEntry { path, position: i } },
                        });
                    }
                }
            }
            _ => return false,
        }
        true
    }

    /// The Datasets section's list: Up/Down, and the keys for the selected dataset.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    pub(super) fn key_dataset_list(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Down if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), true);
                self.dataset_properties.clear();
                self.dataset_properties_table.select(None);
                self.dataset_properties_for = None;
                self.dataset_permissions.clear();
                self.dataset_permissions_table.select(None);
                self.dataset_permissions_for = None;
                self.acl_entries.clear();
                self.acl_entries_table.select(None);
                self.acl_entries_for = None;
            }
            KeyCode::Up if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), false);
                self.dataset_properties.clear();
                self.dataset_properties_table.select(None);
                self.dataset_properties_for = None;
                self.dataset_permissions.clear();
                self.dataset_permissions_table.select(None);
                self.dataset_permissions_for = None;
                self.acl_entries.clear();
                self.acl_entries_table.select(None);
                self.acl_entries_for = None;
            }
            KeyCode::Char('n') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                self.modal = Some(Modal::Form {
                    title: "Create Dataset".to_string(),
                    fields: vec![FormField { label: "Name".to_string(), value: String::new() }],
                    focused: 0,
                    description: None,
                    kind: FormKind::CreateDataset { host: self.selected_host },
                });
            }
            KeyCode::Char('E') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                self.modal = Some(forms::create_encrypted_form(self.selected_host));
            }
            KeyCode::Char('D') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                if let Some(name) = self.selected_dataset_name() {
                    if let Some(why) = self.recursive_destroy_refusal(&name) {
                        self.status_message = Some(why);
                        return true;
                    }
                    let what = self.what_is_under(&name);
                    self.modal = Some(Modal::ConfirmTyped {
                        prompt: format!("Type the dataset name to destroy '{name}' AND everything under it: {what}.\nIf something is busy, zfs stops part-way and what it already destroyed stays destroyed. Snapshot clones inside it go with it; clones outside it are never touched (zfs refuses if any depend on it). (Counts as of the last refresh.)"),
                        expected: name.clone(),
                        input: String::new(),
                        action: PendingAction { host: self.selected_host, section: Section::Datasets, call: MutationCall::DestroyDatasetRecursive { name } },
                    });
                }
            }
            KeyCode::Char('d') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                if let Some(i) = self.datasets_table.selected() {
                    if let Some(ds) = self.datasets.get(i) {
                        if !ds.name.contains('/') {
                            self.status_message = Some(pool_root_message(&ds.name));
                            return true;
                        }
                        let under = self.has_anything_under(&ds.name);
                        let warning = if under { format!("\nIt has {}, so zfs will refuse; press Esc, then D, to destroy them too.", self.what_is_under(&ds.name)) } else { String::new() };
                        self.modal = Some(Modal::ConfirmTyped {
                            prompt: format!("Type the dataset name to destroy '{}':{warning}", ds.name),
                            expected: ds.name.clone(),
                            input: String::new(),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Datasets,
                                call: MutationCall::DestroyDataset { name: ds.name.clone() },
                            },
                        });
                    }
                }
            }
            KeyCode::Char('s') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                if let Some(i) = self.datasets_table.selected() {
                    if let Some(ds) = self.datasets.get(i) {
                        self.modal = Some(Modal::Form {
                            title: format!("Snapshot dataset '{}'", ds.name),
                            fields: vec![FormField { label: "Snapshot name".to_string(), value: String::new() }],
                            focused: 0,
                            description: None,
                            kind: FormKind::CreateSnapshot { host: self.selected_host, dataset: ds.name.clone() },
                        });
                    }
                }
            }
            KeyCode::Char(c @ ('L' | 'U' | 'C')) if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                let Some(name) = self.selected_dataset_name() else { return true };
                let op = match c {
                    'L' => forms::KeyOp::Load,
                    'U' => forms::KeyOp::Unload,
                    _ => forms::KeyOp::Change,
                };
                match forms::key_target(&self.encryption, &name, op) {
                    Err(why) => self.status_message = Some(why),
                    Ok(root) => {
                        let host = self.selected_host;
                        self.modal = Some(match op {
                            forms::KeyOp::Load => forms::load_key_form(host, &root),
                            forms::KeyOp::Change => forms::change_key_form(host, &root),
                            forms::KeyOp::Unload => {
                                let action = PendingAction { host, section: Section::Datasets, call: MutationCall::UnloadKey { dataset: root } };
                                let prompt = forms::irreversible_prompt(&self.hosts, &action).unwrap_or_default();
                                Modal::Confirm { prompt, action, choice: None }
                            }
                        });
                    }
                }
            }
            // Sharing is FreeBSD's (see `ui::support`): on a host whose OS has no equivalent yet the keys do nothing.
            KeyCode::Char('N') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle && self.shares_editable() => {
                // Only act (or explain) when a dataset row is selected; with no
                // selection the key is not advertised and must be inert.
                let Some(ds) = self.datasets_table.selected().and_then(|i| self.datasets.get(i)).cloned() else { return true; };
                if !self.shares_loaded {
                    self.status_message = Some("Press 'r' in the Shares section at least once (to load current share state) before creating/editing a share here".to_string());
                } else if self.nfs_available == Some(false) {
                    self.status_message = Some("NFS server not available on this host (nfsd is not running)".to_string());
                } else {
                    let existing = self.shares.iter().find(|s| matches!(s, crate::zfs::share::Share::Nfs { dataset, .. } if dataset == &ds.name));
                    let placeholder = crate::zfs::share::Share::Nfs {
                        dataset: ds.name.clone(),
                        raw: "off".to_string(),
                        networks: vec![],
                        hosts: vec![],
                        read_only: false,
                        root: None,
                    };
                    self.modal = Some(share_edit_modal(existing.unwrap_or(&placeholder), self.selected_host));
                }
            }
            KeyCode::Char('M') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle && self.shares_editable() => {
                let Some(ds) = self.datasets_table.selected().and_then(|i| self.datasets.get(i)).cloned() else { return true; };
                if !self.shares_loaded {
                    self.status_message = Some("Press 'r' in the Shares section at least once (to load current share state) before creating/editing a share here".to_string());
                } else if self.samba_available == Some(false) {
                    self.status_message = Some("Samba (smbd) is not running on this host".to_string());
                } else {
                    match &ds.mountpoint {
                        None => self.status_message = Some(format!("'{}' has no mounted mountpoint to share", ds.name)),
                        Some(path) => {
                            let existing = self.shares.iter().find(|s| matches!(s, crate::zfs::share::Share::Smb { dataset, .. } if dataset == &ds.name));
                            self.modal = Some(smb_edit_modal(existing, &ds.name, path, self.selected_host));
                        }
                    }
                }
            }
            _ => return false,
        }
        true
    }

    /// The Snapshots section, and Up/Down in the Disks, Shares and Boot Envs lists.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    pub(super) fn key_lists_and_snapshots(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Down if self.current_section() == Section::Snapshots => {
                move_table_selection(&mut self.snapshots_table, self.snapshots.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Snapshots => {
                move_table_selection(&mut self.snapshots_table, self.snapshots.len(), false);
            }
            KeyCode::Char('d') if self.current_section() == Section::Snapshots => {
                if let Some(i) = self.snapshots_table.selected() {
                    if let Some(snap) = self.snapshots.get(i) {
                        let target = format!("{}@{}", snap.dataset, snap.name);
                        self.modal = Some(Modal::ConfirmTyped {
                            prompt: format!("Type the snapshot name to destroy '{target}':"),
                            expected: target,
                            input: String::new(),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Snapshots,
                                call: MutationCall::DestroySnapshot { dataset: snap.dataset.clone(), name: snap.name.clone() },
                            },
                        });
                    }
                }
            }
            KeyCode::Char(which @ ('c' | 'C')) if self.current_section() == Section::Snapshots && self.changes_view.is_none() => {
                if let Some(snap) = self.snapshots_table.selected().and_then(|i| self.snapshots.get(i)) {
                    use crate::ui::changes::{Against, ChangesView, PendingChanges};
                    self.changes_request += 1;
                    let pending = PendingChanges {
                        host: self.selected_host,
                        request: self.changes_request,
                        dataset: snap.dataset.clone(),
                        name: snap.name.clone(),
                        against: if which == 'c' { Against::Previous } else { Against::Live },
                        cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    };
                    self.changes_view = Some(ChangesView::new(&pending));
                    self.pending_changes.push(pending);
                }
            }
            KeyCode::Char('f') if self.current_section() == Section::Snapshots && self.changes_view.is_none() && self.browse_view.is_none() => {
                if let Some(snap) = self.snapshots_table.selected().and_then(|i| self.snapshots.get(i)) {
                    use crate::ui::browse::{BrowseView, PendingBrowse};
                    let probe = self.hosts.get(self.selected_host).and_then(|name| self.host_probes.get(name));
                    match probe.and_then(|p| p.os) {
                        None => {
                            self.set_error("this host has not been probed yet, so its type is not known; try again in a moment".to_string());
                            return true;
                        }
                        Some(crate::zfs::host_health::HostOs::FreeBsd) => {}
                        Some(_) => {
                            self.set_error("browsing a snapshot needs a FreeBSD host".to_string());
                            return true;
                        }
                    }
                    let utc_offset = probe.and_then(|p| p.utc_offset_secs);
                    self.browse_request += 1;
                    let pending = PendingBrowse {
                        host: self.selected_host,
                        request: self.browse_request,
                        dataset: snap.dataset.clone(),
                        name: snap.name.clone(),
                        path: Vec::new(),
                        mountpoint: None,
                        cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    };
                    self.browse_view = Some(BrowseView::new(&pending, utc_offset));
                    self.pending_browse.push(pending);
                }
            }
            KeyCode::Char('H') if self.current_section() == Section::Snapshots => {
                if let Some(snap) = self.snapshots_table.selected().and_then(|i| self.snapshots.get(i)) {
                    let target = format!("{}@{}", snap.dataset, snap.name);
                    let (dataset, name) = (snap.dataset.clone(), snap.name.clone());
                    let tags = self.snapshot_tags.get(&target);
                    let action = |call| PendingAction { host: self.selected_host, section: Section::Snapshots, call };
                    if snap.holds == 0 {
                        let prompt = hold_prompt(&target);
                        self.modal = Some(Modal::Confirm { choice: None, prompt, action: action(MutationCall::HoldSnapshot { dataset, name }) });
                    } else if tags.is_some_and(|t| t.iter().any(|t| t == crate::zfs::snapshot::HOLD_TAG)) {
                        self.modal = Some(Modal::ConfirmTyped {
                            prompt: format!("Type the snapshot name to release the zfsctl hold on '{target}' — it can then be destroyed and pruned again:"),
                            expected: target,
                            input: String::new(),
                            action: action(MutationCall::ReleaseSnapshot { dataset, name }),
                        });
                    } else if let Some(t) = tags {
                        self.status_message = Some(match crate::display_text::tag_list(t) {
                            l if l.is_empty() => "held by another tag (name not printable); zfsctl will not release it".to_string(),
                            l => format!("held by another tag: {l}; zfsctl will not release it"),
                        });
                    } else {
                        self.status_message = Some("hold tags not loaded yet; press r".to_string());
                    }
                }
            }
            KeyCode::Char('b') if self.current_section() == Section::Snapshots => {
                if let Some(i) = self.snapshots_table.selected() {
                    if let Some(snap) = self.snapshots.get(i) {
                        let target = format!("{}@{}", snap.dataset, snap.name);
                        self.modal = Some(Modal::ConfirmTyped {
                            prompt: format!("Type the snapshot name to roll back to '{target}':"),
                            expected: target,
                            input: String::new(),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Snapshots,
                                call: MutationCall::RollbackSnapshot { dataset: snap.dataset.clone(), name: snap.name.clone() },
                            },
                        });
                    }
                }
            }
            KeyCode::Down if self.current_section() == Section::Disks => {
                move_table_selection(&mut self.disks_table, self.disks.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Disks => {
                move_table_selection(&mut self.disks_table, self.disks.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::Shares => {
                move_table_selection(&mut self.shares_table, self.shares.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Shares => {
                move_table_selection(&mut self.shares_table, self.shares.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::BootEnvs => {
                move_table_selection(&mut self.bootenvs_table, self.bootenvs.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::BootEnvs => {
                move_table_selection(&mut self.bootenvs_table, self.bootenvs.len(), false);
            }
            _ => return false,
        }
        true
    }
}
