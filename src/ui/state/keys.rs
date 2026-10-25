//! Key handling: the entry point, the keys every section shares, and the Disks section.
#![allow(clippy::wildcard_imports)]
use super::*;

impl App {
    /// See `selected_pool_name`.
    /// Whether the key `L` (load), `U` (unload) or `C` (change) would do something on the selected dataset: the hints show
    /// only the ones that would, instead of keys that just refuse.
    pub fn key_applies(&self, op: char) -> bool {
        let op = match op {
            'L' => forms::KeyOp::Load,
            'U' => forms::KeyOp::Unload,
            _ => forms::KeyOp::Change,
        };
        self.selected_dataset_name().is_some_and(|n| forms::key_target(&self.encryption, &n, op).is_ok())
    }

    /// Handles a key press. The match is cut into the `key_*` chunks below, tried in order: the first
    /// chunk with an arm for the key (and a passing guard) handles it, exactly as one long `match` would.
    pub fn handle_key(&mut self, key: KeyCode) {
        if self.host_filter_editing {
            self.handle_filter_key(key);
            return;
        }
        let _handled = self.key_changes_view(key)
            || self.key_browse_view(key)
            || self.key_global(key)
            || self.key_navigation(key)
            // A section the host's OS has no equivalent for (see `ui::support`) is dimmed: its keys do nothing.
            || (self.current_section_supported()
                && (self.key_pools(key)
                    || self.key_dataset_detail(key)
                    || self.key_dataset_list(key)
                    || self.key_lists_and_snapshots(key)
                    || self.key_jobs(key)
                    || self.key_alerts_and_shares(key)
                    || self.key_bootenvs(key)
                    || self.key_disks(key)));
    }


    /// Quit, help, about, the shell and the Esc ladder.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    pub(super) fn key_global(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Info { title: "Help".to_string(), body: help_text() });
            }
            KeyCode::Char('A') => {
                self.modal = Some(Modal::Info { title: "About".to_string(), body: about_text() });
            }
            KeyCode::Char('!') if self.modal.is_none() && self.picker.is_none() && !self.hosts.is_empty() => {
                self.pending_shell = true;
                self.status_message = Some(format!("Opening a shell on {}…", self.active_host_name()));
            }
            KeyCode::Esc | KeyCode::Backspace if self.drilled_pool.is_some() => {
                self.drilled_pool = None;
                self.vdevs.clear();
                self.vdevs_table.select(None);
                self.pool_scan_status = None;
                self.pool_scan_status_shown_at = None;
            }
            KeyCode::Esc if self.pane_focus == PaneFocus::Right => {
                self.pane_focus = PaneFocus::Middle;
            }
            KeyCode::Esc if self.current_section() == Section::Disks && !self.selected_disks.is_empty() => {
                self.selected_disks.clear();
            }
            KeyCode::Esc if self.current_section() == Section::Hosts && !self.marked_hosts.is_empty() && self.modal.is_none() => self.marked_hosts.clear(),
            KeyCode::Esc if self.current_section() == Section::Hosts && !self.host_filter.is_empty() => {
                self.host_filter.clear();
            }
            KeyCode::Esc => self.status_message = None,
            _ => return false,
        }
        true
    }

    /// Drilling into a pool, editing a property, Tab and the pane focus, and the Hosts section.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    pub(super) fn key_navigation(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Enter if self.current_section() == Section::Pools && self.drilled_pool.is_none() && self.pane_focus == PaneFocus::Middle => {
                if let Some(i) = self.pools_table.selected() {
                    if let Some(pool) = self.pools.get(i) {
                        self.drilled_pool = Some(pool.name.clone());
                        self.vdevs.clear();
                        self.vdevs_table.select(None);
                        self.pool_scan_status = None;
                        self.pool_scan_status_shown_at = None;
                    }
                }
            }
            KeyCode::Enter if self.pane_focus == PaneFocus::Right && self.current_section() == Section::Pools => {
                if let (Some(pool), Some(i)) = (self.selected_pool_name(), self.pool_properties_table.selected()) {
                    if let Some(prop) = self.pool_properties.get(i) {
                        if prop.source.is_editable() {
                            self.modal = Some(Modal::Form {
                                title: format!("Edit '{}' on pool '{pool}'", prop.name),
                                fields: vec![FormField { label: prop.name.clone(), value: prop.value.clone() }],
                                focused: 0,
                                description: crate::zfs::property::describe(&prop.name).map(str::to_string),
                                kind: FormKind::EditPoolProperty { host: self.selected_host, pool, key: prop.name.clone() },
                            });
                        } else {
                            self.status_message = Some(format!("'{}' is a read-only property", prop.name));
                        }
                    }
                }
            }
            KeyCode::Enter
                if self.pane_focus == PaneFocus::Right
                    && self.current_section() == Section::Datasets
                    && self.dataset_detail_tab == DatasetDetailTab::Properties =>
            {
                if let (Some(dataset), Some(i)) = (self.selected_dataset_name(), self.dataset_properties_table.selected()) {
                    if let Some(prop) = self.dataset_properties.get(i) {
                        let blocks = prop.name == "special_small_blocks";
                        let guided = crate::zfs::quota::KEYS.contains(&prop.name.as_str()) || blocks;
                        if guided && matches!(prop.name.as_str(), "quota" | "refquota") && self.dataset_properties.iter().any(|p| p.name == "type" && p.value == "volume") {
                            self.status_message = Some(format!("'{}' does not apply to a volume: it has its volsize and the reservations", prop.name));
                        } else if prop.source.is_editable() {
                            // A size is shown as 10G (or none) and typed the same way; the exact bytes are sent.
                            let (label, value) = if blocks {
                                ("special_small_blocks (0, 4K, 32K ... 16M)".to_string(), prop.value.parse::<u64>().ok().filter(|b| *b > 0).map_or_else(|| "0".to_string(), crate::zfs::quota::format_size))
                            } else if matches!(prop.name.as_str(), "checksum" | "dedup") {
                                (format!("{} (F2 lists the values)", prop.name), prop.value.clone())
                            } else if guided {
                                (format!("{} (10G, 500M, none)", prop.name), prop.value.parse::<u64>().ok().filter(|b| *b > 0).map_or_else(|| "none".to_string(), crate::zfs::quota::format_size))
                            } else {
                                (prop.name.clone(), prop.value.clone())
                            };
                            self.modal = Some(Modal::Form {
                                title: format!("Edit '{}' on dataset '{dataset}'", prop.name),
                                fields: vec![FormField { label, value }],
                                focused: 0,
                                description: crate::zfs::property::describe(&prop.name).map(str::to_string),
                                kind: FormKind::EditDatasetProperty { host: self.selected_host, dataset, key: prop.name.clone() },
                            });
                        } else {
                            self.status_message = Some(format!("'{}' is a read-only property", prop.name));
                        }
                    }
                }
            }
            KeyCode::Tab if self.drilled_pool.is_none() => {
                self.selected_section = (self.selected_section + 1) % self.sections.len();
                self.pane_focus = PaneFocus::Middle;
            }
            KeyCode::BackTab if self.drilled_pool.is_none() => {
                self.selected_section = if self.selected_section == 0 {
                    self.sections.len() - 1
                } else {
                    self.selected_section - 1
                };
                self.pane_focus = PaneFocus::Middle;
            }
            KeyCode::Right
                if (self.current_section() == Section::Pools || self.current_section() == Section::Datasets)
                    && self.drilled_pool.is_none()
                    && self.pane_focus == PaneFocus::Middle =>
            {
                self.pane_focus = PaneFocus::Right;
                match self.current_section() {
                    Section::Pools => {
                        self.pool_properties.clear();
                        self.pool_properties_table.select(None);
                        self.pool_properties_for = None;
                    }
                    Section::Datasets => {
                        self.dataset_properties.clear();
                        self.dataset_properties_table.select(None);
                        self.dataset_properties_for = None;
                        self.dataset_detail_tab = DatasetDetailTab::default();
                        self.dataset_permissions.clear();
                        self.dataset_permissions_table.select(None);
                        self.dataset_permissions_for = None;
                        self.acl_entries.clear();
                        self.acl_entries_table.select(None);
                        self.acl_entries_for = None;
                    }
                    _ => {}
                }
            }
            KeyCode::Left if self.pane_focus == PaneFocus::Right => {
                self.pane_focus = PaneFocus::Middle;
            }
            KeyCode::Down | KeyCode::Up if self.current_section() == Section::Hosts && self.drilled_pool.is_none() && !self.hosts.is_empty() => {
                let visible = self.visible_hosts();
                if !visible.is_empty() {
                    let at = visible.iter().position(|i| *i == self.selected_host).unwrap_or(0);
                    let next = if key == KeyCode::Down { (at + 1) % visible.len() } else { (at + visible.len() - 1) % visible.len() };
                    if let Some(&host) = visible.get(next) {
                        self.selected_host = host;
                        self.clear_host_data();
                    }
                }
            }
            KeyCode::Char('/') if self.current_section() == Section::Hosts && self.modal.is_none() && self.picker.is_none() && self.drilled_pool.is_none() => {
                self.host_filter_editing = true;
                self.host_filter_origin = Some(self.selected_host);
            }
            KeyCode::Char('c') if self.current_section() == Section::Hosts && self.modal.is_none() && self.picker.is_none() => self.request_host_check(),
            KeyCode::Char(' ') if self.current_section() == Section::Hosts && self.modal.is_none() && self.picker.is_none() => {
                if let Some(name) = self.hosts.get(self.selected_host).cloned() {
                    if !self.marked_hosts.remove(&name) {
                        self.marked_hosts.insert(name);
                    }
                }
            }
            KeyCode::Char('e') if self.current_section() == Section::Hosts && self.modal.is_none() && self.picker.is_none() => self.begin_edit_hosts(),
            KeyCode::Char('n') if self.current_section() == Section::Hosts => {
                self.host_filter.clear();
                self.modal = Some(Modal::Form {
                    title: "Add Host".to_string(),
                    fields: vec![
                        FormField { label: "Name".to_string(), value: String::new() },
                        FormField { label: "SSH target (e.g. user@host)".to_string(), value: String::new() },
                        FormField { label: "Tag (optional)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    description: None,
                    kind: FormKind::AddHost,
                });
            }
            _ => return false,
        }
        true
    }

    /// The Disks section.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    pub(super) fn key_disks(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Char(' ') if self.current_section() == Section::Disks => {
                if let Some(i) = self.disks_table.selected() {
                    if let Some(disk) = self.disks.get(i) {
                        match &disk.membership {
                            crate::zfs::disk::DiskMembership::Available => {
                                if !self.selected_disks.remove(&disk.name) {
                                    self.selected_disks.insert(disk.name.clone());
                                }
                            }
                            crate::zfs::disk::DiskMembership::InPool { pool, .. } => {
                                self.status_message = Some(format!("disk '{}' already in pool '{pool}'", disk.name));
                            }
                            crate::zfs::disk::DiskMembership::InUse { what } => {
                                self.status_message = Some(format!("disk '{}' is in use ({what}): not a disk to build a pool on", disk.name));
                            }
                        }
                    }
                }
            }
            KeyCode::Char('n') if self.current_section() == Section::Disks && !self.selected_disks.is_empty() => {
                let mut disks: Vec<String> = self.selected_disks.iter().cloned().collect();
                disks.sort();
                self.modal = Some(Modal::Form {
                    title: format!("Create Pool from {} selected disk(s)", disks.len()),
                    fields: vec![
                        FormField { label: "Name".to_string(), value: String::new() },
                        FormField { label: "Redundancy (blank=stripe, or mirror/raidz1/raidz2/raidz3)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    description: Some(self.selected_disk_identities(&disks)),
                    kind: FormKind::CreatePoolFromDisks { host: self.selected_host, disks },
                });
            }
            KeyCode::Char('n') if self.current_section() == Section::Disks => {
                self.status_message = Some("select at least one disk with Space first".to_string());
            }
            KeyCode::Char('a') if self.current_section() == Section::Disks && !self.selected_disks.is_empty() => {
                let mut disks: Vec<String> = self.selected_disks.iter().cloned().collect();
                disks.sort();
                self.modal = Some(Modal::Form {
                    title: format!("Add vdev from {} selected disk(s)", disks.len()),
                    fields: vec![
                        FormField { label: "Target pool name".to_string(), value: String::new() },
                        FormField { label: "Role (data, log, cache, special, dedup, spare)".to_string(), value: "data".to_string() },
                        FormField { label: "Redundancy (blank=stripe, or mirror/raidz1/raidz2/raidz3; special and dedup default to mirror)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    description: Some(self.selected_disk_identities(&disks)),
                    kind: FormKind::AddVdevFromDisks { host: self.selected_host, disks },
                });
            }
            KeyCode::Char('a') if self.current_section() == Section::Disks => {
                self.status_message = Some("select at least one disk with Space first".to_string());
            }
            _ => return false,
        }
        true
    }

    /// One identity line per selected disk (by name lookup in
    /// `self.disks`; a disk that vanished shows just its name). At most
    /// `MAX_IDENTITY_LINES` are listed, then `+K more selected`, so the
    /// form's input fields can never be pushed out of a small popup.
    pub(super) fn selected_disk_identities(&self, names: &[String]) -> String {
        const MAX_IDENTITY_LINES: usize = 6;
        let mut lines: Vec<String> = names
            .iter()
            .take(MAX_IDENTITY_LINES)
            .map(|n| match self.disks.iter().find(|d| &d.name == n) {
                Some(d) => crate::ui::layout::sanitise_for_display(&d.identity_line()),
                None => crate::ui::layout::sanitise_for_display(n),
            })
            .collect();
        if names.len() > MAX_IDENTITY_LINES {
            lines.push(format!("+{} more selected", names.len() - MAX_IDENTITY_LINES));
        }
        lines.join("\n")
    }

    /// Shared by the offline/online disk action keys: looks up the
    /// selected vdev row, and — only if it's a leaf disk, not a
    /// mirror/raidz group node — opens the matching `Confirm` modal.
    /// `online` selects which of the two operations to build.
    pub(super) fn open_vdev_confirm(&mut self, online: bool) {
        let Some(i) = self.vdevs_table.selected() else { return };
        let Some(vdev) = self.vdevs.get(i) else { return };
        if !crate::zfs::pool::is_leaf_disk(&vdev.name) {
            self.status_message = Some("select a disk (not a mirror/raidz group) to offline/online it".to_string());
            return;
        }
        let Some(pool) = self.drilled_pool.clone() else { return };
        let disk = vdev.name.clone();
        let (verb, call) = if online {
            ("Online", MutationCall::OnlineDisk { pool: pool.clone(), disk: disk.clone() })
        } else {
            ("Offline", MutationCall::OfflineDisk { pool: pool.clone(), disk: disk.clone() })
        };
        self.modal = Some(Modal::Confirm { choice: None,
            prompt: format!("{verb} disk '{disk}' in pool '{pool}'? (y/n)"),
            action: PendingAction { host: self.selected_host, section: Section::Vdevs, call },
        });
    }
}
