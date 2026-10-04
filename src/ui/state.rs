use crate::error::Result;
use crate::exec::CommandRunner;
use crate::ui::modal::{FormField, FormKind, Modal, MutationCall, PendingAction};
use crate::zfs::dataset::Dataset;
use crate::zfs::disk::DiskInfo;
use crate::zfs::disk::DiskMembership;
use crate::zfs::pool::{Pool, VdevLine};
use crate::zfs::property::Property;
use crate::zfs::share::SharesSnapshot;
use crate::zfs::snapshot::Snapshot;
use crossterm::event::KeyCode;
use ratatui::widgets::TableState;
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a completed scrub/resilver's status stays visible in the
/// status bar before `clear_stale_scan_status` clears it.
const POOL_SCAN_STATUS_DISPLAY_DURATION: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Hosts,
    Pools,
    Datasets,
    Snapshots,
    Disks,
    Shares,
    Vdevs,
    PoolProperties,
    DatasetProperties,
    DatasetPermissions,
    DatasetAcl,
}

/// Which pane currently receives navigation keys. Meaningful only while
/// the current section is Pools or Datasets — those are the only two
/// sections with a right pane at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneFocus {
    Middle,
    Right,
}

/// Which view the Datasets right pane shows. Meaningful only while
/// `current_section() == Section::Datasets && pane_focus ==
/// PaneFocus::Right` — the Pools right pane has exactly one view and
/// never gains this concept. `'['`/`']'` cycle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DatasetDetailTab {
    #[default]
    Properties,
    Permissions,
    Acl,
}

impl DatasetDetailTab {
    fn next(self) -> Self {
        match self {
            DatasetDetailTab::Properties => DatasetDetailTab::Permissions,
            DatasetDetailTab::Permissions => DatasetDetailTab::Acl,
            DatasetDetailTab::Acl => DatasetDetailTab::Properties,
        }
    }

    fn prev(self) -> Self {
        match self {
            DatasetDetailTab::Properties => DatasetDetailTab::Acl,
            DatasetDetailTab::Permissions => DatasetDetailTab::Properties,
            DatasetDetailTab::Acl => DatasetDetailTab::Permissions,
        }
    }
}

/// One flattened, display/selection-ready row for the Permissions
/// sub-tab's table — `zfs::permission::PermissionsSnapshot`'s three
/// buckets (sets, create-time, grants) collapsed into a single
/// selectable list the moment a refresh result arrives, so the table
/// and `'d'`/`'x'` never need to re-derive it from the snapshot shape.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionRow {
    Set { name: String, perms: Vec<String> },
    CreateTime { perms: Vec<String> },
    Grant { recipient: crate::zfs::permission::Recipient, scope: crate::zfs::permission::Scope, perms: Vec<String> },
}

/// Flattens a `PermissionsSnapshot` into one `Vec<PermissionRow>` in a
/// stable order (sets, then create-time, then grants) for display and
/// selection.
fn flatten_permissions_snapshot(snapshot: &crate::zfs::permission::PermissionsSnapshot) -> Vec<PermissionRow> {
    let mut rows = Vec::new();
    for set in &snapshot.sets {
        rows.push(PermissionRow::Set { name: set.name.clone(), perms: set.perms.clone() });
    }
    if !snapshot.create_time.is_empty() {
        rows.push(PermissionRow::CreateTime { perms: snapshot.create_time.clone() });
    }
    for grant in &snapshot.grants {
        rows.push(PermissionRow::Grant { recipient: grant.recipient.clone(), scope: grant.scope, perms: grant.perms.clone() });
    }
    rows
}

/// Each variant carries the index (into `App::hosts`) of the host the
/// result is *about*, so a slow result for a host the user has since
/// navigated away from can be told apart from a fresh one instead of
/// silently overwriting the currently-selected host's pane.
pub enum WorkerMsg {
    Pools(usize, Result<Vec<Pool>>),
    Datasets(usize, Result<Vec<Dataset>>),
    Snapshots(usize, Result<Vec<Snapshot>>),
    Disks(usize, Result<Vec<DiskInfo>>),
    Shares(usize, Result<SharesSnapshot>),
    /// The `String` is the name of the pool this vdev refresh was for —
    /// needed so a slow result can be told apart from one for a
    /// different pool the user has since drilled into (see the
    /// stale-pool check in `drain_worker_messages`).
    Vdevs(usize, String, Result<(Vec<VdevLine>, Option<crate::zfs::pool::ScanStatus>)>),
    /// The `String` is the pool name this fetch was for — same
    /// stale-identity discard rationale as `Vdevs`.
    PoolProperties(usize, String, Result<Vec<Property>>),
    /// The `String` is the dataset name this fetch was for.
    DatasetProperties(usize, String, Result<Vec<Property>>),
    /// The `String` is the dataset name this fetch was for — same
    /// stale-identity discard rationale as `DatasetProperties`.
    DatasetPermissions(usize, String, Result<crate::zfs::permission::PermissionsSnapshot>),
    /// The `String` is the **mountpoint path** this fetch was for (not
    /// the dataset name — ACLs are filesystem-path-scoped, not
    /// dataset-metadata-scoped). Same stale-identity discard rationale
    /// as `DatasetPermissions`.
    DatasetAcl(usize, String, Result<Vec<crate::acl::AclEntry>>),
    MutationDone(usize, Section, Result<()>),
}

pub struct App {
    pub sections: Vec<Section>,
    pub selected_section: usize,
    pub hosts: Vec<String>,
    pub selected_host: usize,
    pub pools: Vec<Pool>,
    pub datasets: Vec<Dataset>,
    pub snapshots: Vec<Snapshot>,
    pub pools_table: TableState,
    pub datasets_table: TableState,
    pub snapshots_table: TableState,
    pub disks: Vec<DiskInfo>,
    pub disks_table: TableState,
    /// Names of disks currently selected (via Space, in the Disks
    /// section) to build a pool or vdev from. Only ever contains names
    /// of disks whose membership is `Available` — see `drain_worker_messages`'s
    /// `WorkerMsg::Disks` arm for how a disk that stops being available
    /// (or disappears) is dropped from this set.
    pub selected_disks: HashSet<String>,
    pub shares: Vec<crate::zfs::share::Share>,
    pub shares_table: TableState,
    /// `None` means "not yet checked" — kept distinct from `Some(false)`
    /// so the Shares title doesn't flash a false "unavailable" hint
    /// before the first refresh, consistent with every other section
    /// staying quiet (an empty list) rather than asserting a default
    /// before it's actually fetched anything.
    pub nfs_available: Option<bool>,
    pub samba_available: Option<bool>,
    /// Becomes `true` the first time a Shares refresh completes for the
    /// currently-selected host (reset on host switch via
    /// `clear_host_data`). Guards `'N'` from pre-filling against a
    /// `self.shares` that hasn't been populated yet — without this,
    /// pressing `'N'` on an already-restricted dataset before ever
    /// visiting Shares opens a blank form whose submission silently
    /// widens the share to `"on"` (shared to everyone).
    pub shares_loaded: bool,
    pub vdevs: Vec<VdevLine>,
    pub vdevs_table: TableState,
    /// The drilled-into pool's most recent scrub/resilver, refreshed
    /// alongside `vdevs` (same `zpool status` call). `None` while
    /// `vdevs` is empty/stale, or if the pool has no scan history.
    pub pool_scan_status: Option<crate::zfs::pool::ScanStatus>,
    /// When `pool_scan_status` was last set from a fresh refresh result
    /// — used by `clear_stale_scan_status` to expire a `Completed`
    /// status after it's been shown for a while, so a finished
    /// resilver/scrub doesn't linger in the status bar forever.
    pool_scan_status_shown_at: Option<Instant>,
    pub drilled_pool: Option<String>,
    pub pane_focus: PaneFocus,
    pub pool_properties: Vec<Property>,
    pub pool_properties_table: TableState,
    /// The name of the pool `pool_properties` was actually fetched for —
    /// used to detect when the middle-pane `pools` list is replaced (e.g.
    /// by a fresh `WorkerMsg::Pools`) in a way that re-points the still
    /// selected index at a *different* pool than the one these properties
    /// belong to. See the stale-identity check in `drain_worker_messages`'s
    /// `WorkerMsg::Pools` arm.
    pub pool_properties_for: Option<String>,
    pub dataset_properties: Vec<Property>,
    pub dataset_properties_table: TableState,
    pub dataset_detail_tab: DatasetDetailTab,
    pub dataset_permissions: Vec<PermissionRow>,
    pub dataset_permissions_table: TableState,
    pub dataset_permissions_for: Option<String>,
    pub acl_entries: Vec<crate::acl::AclEntry>,
    pub acl_entries_table: TableState,
    pub acl_entries_for: Option<String>,
    /// See `pool_properties_for`.
    pub dataset_properties_for: Option<String>,
    pub modal: Option<Modal>,
    pub status_message: Option<String>,
    pub should_quit: bool,
    in_flight: HashSet<(Section, usize)>,
    /// Refresh requests for a `(section, host)` that arrived while a
    /// refresh for that same key was already in flight. Recorded here
    /// instead of being silently dropped, and re-dispatched (via the
    /// `Vec<Section>` returned from `drain_worker_messages`) once the
    /// in-flight one completes — see `refresh_pools` et al. and the
    /// pending-refresh handling in `drain_worker_messages`.
    pending_refresh: HashSet<(Section, usize)>,
    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
}

impl App {
    pub fn new(hosts: Vec<String>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        App {
            sections: vec![Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots, Section::Disks, Section::Shares],
            selected_section: 0,
            hosts,
            selected_host: 0,
            pools: Vec::new(),
            datasets: Vec::new(),
            snapshots: Vec::new(),
            pools_table: TableState::default(),
            datasets_table: TableState::default(),
            snapshots_table: TableState::default(),
            disks: Vec::new(),
            disks_table: TableState::default(),
            selected_disks: HashSet::new(),
            shares: Vec::new(),
            shares_table: TableState::default(),
            nfs_available: None,
            samba_available: None,
            shares_loaded: false,
            vdevs: Vec::new(),
            vdevs_table: TableState::default(),
            pool_scan_status: None,
            pool_scan_status_shown_at: None,
            drilled_pool: None,
            pane_focus: PaneFocus::Middle,
            pool_properties: Vec::new(),
            pool_properties_table: TableState::default(),
            pool_properties_for: None,
            dataset_properties: Vec::new(),
            dataset_properties_table: TableState::default(),
            dataset_detail_tab: DatasetDetailTab::default(),
            dataset_permissions: Vec::new(),
            dataset_permissions_table: TableState::default(),
            dataset_permissions_for: None,
            acl_entries: Vec::new(),
            acl_entries_table: TableState::default(),
            acl_entries_for: None,
            dataset_properties_for: None,
            modal: None,
            status_message: None,
            should_quit: false,
            in_flight: HashSet::new(),
            pending_refresh: HashSet::new(),
            tx,
            rx,
        }
    }

    pub fn current_section(&self) -> Section {
        self.sections[self.selected_section]
    }

    /// Clears a completed scrub/resilver status once it's been shown
    /// for `POOL_SCAN_STATUS_DISPLAY_DURATION` — called once per event
    /// loop tick so a finished resilver/scrub doesn't linger in the
    /// status bar forever. A no-op while `pool_scan_status` is
    /// `InProgress` (that one's supposed to stay until it finishes) or
    /// already `None`.
    pub fn clear_stale_scan_status(&mut self) {
        let Some(crate::zfs::pool::ScanStatus::Completed { .. }) = &self.pool_scan_status else {
            return;
        };
        if self.pool_scan_status_shown_at.is_some_and(|t| t.elapsed() >= POOL_SCAN_STATUS_DISPLAY_DURATION) {
            self.pool_scan_status = None;
            self.pool_scan_status_shown_at = None;
        }
    }

    /// True while any (section, host) refresh is in flight, excluding
    /// `Section::Vdevs` — the only section under passive/periodic
    /// auto-refresh (see `VDEVS_AUTO_REFRESH_INTERVAL` in `ui/mod.rs`),
    /// which would otherwise make this indicator flicker on/off every
    /// couple of seconds while just sitting in a pool's Vdevs view.
    /// Used only for the coarse "refreshing..." indicator in the
    /// sidebar title.
    pub fn is_loading(&self) -> bool {
        self.in_flight.iter().any(|(section, _)| *section != Section::Vdevs)
    }

    fn is_refreshing(&self, section: Section, host: usize) -> bool {
        self.in_flight.contains(&(section, host))
    }

    fn begin_refresh(&mut self, section: Section, host: usize) {
        self.in_flight.insert((section, host));
    }

    fn end_refresh(&mut self, section: Section, host: usize) {
        self.in_flight.remove(&(section, host));
    }

    /// Name of the currently-selected host, for pane titles.
    pub fn active_host_name(&self) -> &str {
        self.hosts.get(self.selected_host).map(|s| s.as_str()).unwrap_or("?")
    }

    /// The name of the pool currently selected in the middle pane, if
    /// any — used to know which pool the right pane's properties belong
    /// to. Valid regardless of `pane_focus`: the middle-pane selection
    /// doesn't change while the right pane has focus.
    pub fn selected_pool_name(&self) -> Option<String> {
        self.pools_table.selected().and_then(|i| self.pools.get(i)).map(|p| p.name.clone())
    }

    /// See `selected_pool_name`.
    pub fn selected_dataset_name(&self) -> Option<String> {
        self.datasets_table.selected().and_then(|i| self.datasets.get(i)).map(|d| d.name.clone())
    }

    /// The mountpoint of the currently-selected Datasets row, or `None`
    /// if it has none (a zvol, or an unmounted filesystem) -- the ACL
    /// sub-tab operates on this, not on `selected_dataset_name()`.
    pub fn selected_dataset_mountpoint(&self) -> Option<String> {
        self.datasets_table.selected().and_then(|i| self.datasets.get(i)).and_then(|d| d.mountpoint.clone())
    }

    /// Clears data panes so a stale render from the previously-selected
    /// host is never left on screen looking current after switching hosts.
    fn clear_host_data(&mut self) {
        self.pools.clear();
        self.datasets.clear();
        self.snapshots.clear();
        self.pools_table.select(None);
        self.datasets_table.select(None);
        self.snapshots_table.select(None);
        self.disks.clear();
        self.disks_table.select(None);
        self.selected_disks.clear();
        self.shares.clear();
        self.shares_table.select(None);
        self.nfs_available = None;
        self.samba_available = None;
        self.shares_loaded = false;
        self.pane_focus = PaneFocus::Middle;
        self.pool_properties.clear();
        self.pool_properties_table.select(None);
        self.pool_properties_for = None;
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

    pub fn handle_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Info { title: "Help".to_string(), body: help_text() });
            }
            KeyCode::Char('A') => {
                self.modal = Some(Modal::Info { title: "About".to_string(), body: about_text() });
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
            KeyCode::Esc => self.status_message = None,
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
                        if !prop.source.is_editable() {
                            self.status_message = Some(format!("'{}' is a read-only property", prop.name));
                        } else {
                            self.modal = Some(Modal::Form {
                                title: format!("Edit '{}' on pool '{pool}'", prop.name),
                                fields: vec![FormField { label: prop.name.clone(), value: prop.value.clone() }],
                                focused: 0,
                                description: crate::zfs::property::describe(&prop.name).map(str::to_string),
                                kind: FormKind::EditPoolProperty { host: self.selected_host, pool, key: prop.name.clone() },
                            });
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
                        if !prop.source.is_editable() {
                            self.status_message = Some(format!("'{}' is a read-only property", prop.name));
                        } else {
                            self.modal = Some(Modal::Form {
                                title: format!("Edit '{}' on dataset '{dataset}'", prop.name),
                                fields: vec![FormField { label: prop.name.clone(), value: prop.value.clone() }],
                                focused: 0,
                                description: crate::zfs::property::describe(&prop.name).map(str::to_string),
                                kind: FormKind::EditDatasetProperty { host: self.selected_host, dataset, key: prop.name.clone() },
                            });
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
            KeyCode::Down if self.current_section() == Section::Hosts && self.drilled_pool.is_none() && !self.hosts.is_empty() => {
                self.selected_host = (self.selected_host + 1) % self.hosts.len();
                self.clear_host_data();
            }
            KeyCode::Up if self.current_section() == Section::Hosts && self.drilled_pool.is_none() && !self.hosts.is_empty() => {
                self.selected_host = if self.selected_host == 0 {
                    self.hosts.len() - 1
                } else {
                    self.selected_host - 1
                };
                self.clear_host_data();
            }
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
            KeyCode::Char('x') if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                if let Some(i) = self.vdevs_table.selected() {
                    if let Some(vdev) = self.vdevs.get(i) {
                        if crate::zfs::pool::is_leaf_disk(&vdev.name) {
                            let pool = self.drilled_pool.clone().unwrap();
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
            KeyCode::Char('a') if self.current_section() == Section::Pools && self.drilled_pool.is_some() => {
                let pool = self.drilled_pool.clone().unwrap();
                self.modal = Some(Modal::Form {
                    title: format!("Add vdev to pool '{pool}'"),
                    fields: vec![FormField { label: "Vdev spec (e.g. mirror da2 da3)".to_string(), value: String::new() }],
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
                        self.modal = Some(Modal::Confirm {
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
                        self.modal = Some(Modal::Confirm {
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
                        FormField { label: "Vdev spec (e.g. mirror da0 da1)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    description: None,
                    kind: FormKind::CreatePool { host: self.selected_host },
                });
            }
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
                            FormField { label: "Permissions".to_string(), value: String::new() },
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
                            FormField { label: "Permissions".to_string(), value: String::new() },
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
                        fields: vec![FormField { label: "Permissions".to_string(), value: String::new() }],
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
                        self.modal = Some(Modal::Confirm {
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
                            fields: vec![FormField { label: "Permissions to remove".to_string(), value: String::new() }],
                            focused: 0,
                            description: None,
                            kind: FormKind::RevokePermissions { host: self.selected_host, dataset, target },
                        });
                    }
                }
            }
            KeyCode::Char('n')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Acl =>
            {
                if let Some(path) = self.selected_dataset_mountpoint() {
                    if self.acl_entries_for.as_deref() != Some(path.as_str()) {
                        self.status_message = Some("ACL not loaded yet -- press 'r' to refresh before adding an entry".to_string());
                    } else {
                        let position = self.acl_entries.len();
                        self.modal = Some(Modal::Form {
                            title: format!("Add ACL entry on '{path}'"),
                            fields: vec![
                                FormField { label: "Recipient (owner / group / everyone / user:<name> / group:<name>)".to_string(), value: String::new() },
                                FormField { label: "Permissions".to_string(), value: String::new() },
                                FormField { label: "Flags (blank = none)".to_string(), value: String::new() },
                                FormField { label: "Type (allow/deny, blank=allow)".to_string(), value: String::new() },
                            ],
                            focused: 0,
                            description: None,
                            kind: FormKind::AddAclEntry { host: self.selected_host, path, position },
                        });
                    }
                }
            }
            KeyCode::Char('d')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Acl =>
            {
                if let (Some(path), Some(i)) = (self.selected_dataset_mountpoint(), self.acl_entries_table.selected()) {
                    if i < self.acl_entries.len() {
                        self.modal = Some(Modal::Confirm {
                            prompt: format!("Remove ACL entry #{i} on '{path}'? (y/n)"),
                            action: PendingAction { host: self.selected_host, section: Section::DatasetAcl, call: MutationCall::RemoveAclEntry { path, position: i } },
                        });
                    }
                }
            }
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
            KeyCode::Char('d') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                if let Some(i) = self.datasets_table.selected() {
                    if let Some(ds) = self.datasets.get(i) {
                        self.modal = Some(Modal::ConfirmTyped {
                            prompt: format!("Type the dataset name to destroy '{}':", ds.name),
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
            KeyCode::Char('N') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                if !self.shares_loaded {
                    self.status_message = Some("Press 'r' in the Shares section at least once (to load current share state) before creating/editing a share here".to_string());
                } else if self.nfs_available == Some(false) {
                    self.status_message = Some("NFS server not available on this host".to_string());
                } else if let Some(i) = self.datasets_table.selected() {
                    if let Some(ds) = self.datasets.get(i) {
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
            }
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
            KeyCode::Enter if self.current_section() == Section::Shares => {
                if let Some(i) = self.shares_table.selected() {
                    if let Some(share) = self.shares.get(i) {
                        match share {
                            crate::zfs::share::Share::Nfs { .. } => {
                                self.modal = Some(share_edit_modal(share, self.selected_host));
                            }
                            crate::zfs::share::Share::Smb { .. } => {
                                self.status_message = Some(
                                    "SMB share creation/editing isn't supported on FreeBSD (zfs set sharesmb doesn't actually work) — 'd' still removes it".to_string(),
                                );
                            }
                        }
                    }
                }
            }
            KeyCode::Char('d') if self.current_section() == Section::Shares => {
                if let Some(i) = self.shares_table.selected() {
                    if let Some(share) = self.shares.get(i) {
                        let (dataset, key, protocol) = match share {
                            crate::zfs::share::Share::Nfs { dataset, .. } => (dataset.clone(), "sharenfs", "NFS"),
                            crate::zfs::share::Share::Smb { dataset, .. } => (dataset.clone(), "sharesmb", "SMB"),
                        };
                        self.modal = Some(Modal::Confirm {
                            prompt: format!("Remove {protocol} share for '{dataset}'? (y/n)"),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Shares,
                                call: MutationCall::SetDatasetProperty { dataset, key: key.to_string(), value: "off".to_string() },
                            },
                        });
                    }
                }
            }
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
                    description: None,
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
                        FormField { label: "Redundancy (blank=stripe, or mirror/raidz1/raidz2/raidz3)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    description: None,
                    kind: FormKind::AddVdevFromDisks { host: self.selected_host, disks },
                });
            }
            KeyCode::Char('a') if self.current_section() == Section::Disks => {
                self.status_message = Some("select at least one disk with Space first".to_string());
            }
            _ => {}
        }
    }

    /// Shared by the offline/online disk action keys: looks up the
    /// selected vdev row, and — only if it's a leaf disk, not a
    /// mirror/raidz group node — opens the matching `Confirm` modal.
    /// `online` selects which of the two operations to build.
    fn open_vdev_confirm(&mut self, online: bool) {
        let Some(i) = self.vdevs_table.selected() else { return };
        let Some(vdev) = self.vdevs.get(i) else { return };
        if !crate::zfs::pool::is_leaf_disk(&vdev.name) {
            self.status_message = Some("select a disk (not a mirror/raidz group) to offline/online it".to_string());
            return;
        }
        let pool = self.drilled_pool.clone().unwrap();
        let disk = vdev.name.clone();
        let (verb, call) = if online {
            ("Online", MutationCall::OnlineDisk { pool: pool.clone(), disk: disk.clone() })
        } else {
            ("Offline", MutationCall::OfflineDisk { pool: pool.clone(), disk: disk.clone() })
        };
        self.modal = Some(Modal::Confirm {
            prompt: format!("{verb} disk '{disk}' in pool '{pool}'? (y/n)"),
            action: PendingAction { host: self.selected_host, section: Section::Vdevs, call },
        });
    }

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
            Modal::Confirm { prompt, action } => match key {
                KeyCode::Char('y') | KeyCode::Enter => Some(action),
                KeyCode::Char('n') | KeyCode::Esc => None,
                _ => {
                    self.modal = Some(Modal::Confirm { prompt, action });
                    None
                }
            },
            Modal::ConfirmTyped { prompt, expected, mut input, action } => match key {
                KeyCode::Esc => None,
                KeyCode::Enter => {
                    if input == expected {
                        Some(action)
                    } else {
                        self.modal = Some(Modal::ConfirmTyped { prompt, expected, input, action });
                        None
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.modal = Some(Modal::ConfirmTyped { prompt, expected, input, action });
                    None
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    self.modal = Some(Modal::ConfirmTyped { prompt, expected, input, action });
                    None
                }
                _ => {
                    self.modal = Some(Modal::ConfirmTyped { prompt, expected, input, action });
                    None
                }
            },
            Modal::Form { title, mut fields, mut focused, description, kind } => match key {
                KeyCode::Esc => None,
                KeyCode::Tab => {
                    focused = (focused + 1) % fields.len();
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
                KeyCode::Enter => {
                    if focused + 1 < fields.len() {
                        focused += 1;
                        self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                        None
                    } else {
                        match build_pending_action(&kind, &fields) {
                            Some(action) => Some(action),
                            None => {
                                self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                                None
                            }
                        }
                    }
                }
                KeyCode::Backspace => {
                    fields[focused].value.pop();
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
                KeyCode::Char(c) => {
                    fields[focused].value.push(c);
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
                _ => {
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
            },
            Modal::Info { .. } => None,
        }
    }

    /// Runs `action.call` on a worker thread against `runner` and
    /// reports the outcome as `WorkerMsg::MutationDone`. The modal is
    /// already closed by the time this is called (`handle_modal_key`
    /// clears it on submit) — this only executes the mutation itself.
    pub fn dispatch_mutation(&mut self, action: PendingAction, runner: Arc<dyn CommandRunner>) {
        let PendingAction { host, section, call } = action;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = call.run(runner.as_ref());
            let _ = tx.send(WorkerMsg::MutationDone(host, section, result));
        });
    }

    /// Does not spawn a second refresh if one for the current host is
    /// already in flight — otherwise holding 'r' against an unreachable
    /// host would spawn an unbounded number of blocking threads/subprocesses,
    /// one per key-repeat tick. Keyed by host too, so a hung refresh for
    /// one host never blocks refreshing a different host. The request is
    /// not simply dropped, though: it's recorded in `pending_refresh` and
    /// re-dispatched (see `drain_worker_messages`) once the in-flight one
    /// completes, so a manual 'r' press (or a post-mutation refresh) that
    /// arrives mid-flight is never silently lost.
    pub fn refresh_pools(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Pools, host) {
            self.pending_refresh.insert((Section::Pools, host));
            return;
        }
        self.begin_refresh(Section::Pools, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::pool::list_pools(runner.as_ref());
            let _ = tx.send(WorkerMsg::Pools(host, result));
        });
    }

    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight.
    pub fn refresh_datasets(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Datasets, host) {
            self.pending_refresh.insert((Section::Datasets, host));
            return;
        }
        self.begin_refresh(Section::Datasets, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::dataset::list_datasets(runner.as_ref());
            let _ = tx.send(WorkerMsg::Datasets(host, result));
        });
    }

    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight.
    pub fn refresh_snapshots(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Snapshots, host) {
            self.pending_refresh.insert((Section::Snapshots, host));
            return;
        }
        self.begin_refresh(Section::Snapshots, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::snapshot::list_snapshots(runner.as_ref());
            let _ = tx.send(WorkerMsg::Snapshots(host, result));
        });
    }

    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight. Fetches every disk on the host
    /// and its pool/vdev membership in one call — see
    /// `zfs::disk::list_disks_with_membership`.
    pub fn refresh_disks(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Disks, host) {
            self.pending_refresh.insert((Section::Disks, host));
            return;
        }
        self.begin_refresh(Section::Disks, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::disk::list_disks_with_membership(runner.as_ref());
            let _ = tx.send(WorkerMsg::Disks(host, result));
        });
    }

    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight.
    pub fn refresh_shares(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Shares, host) {
            self.pending_refresh.insert((Section::Shares, host));
            return;
        }
        self.begin_refresh(Section::Shares, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::share::list_shares_with_availability(runner.as_ref());
            let _ = tx.send(WorkerMsg::Shares(host, result));
        });
    }

    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight. `pool` is the name of the
    /// currently drilled-into pool.
    ///
    /// The in-flight key is `(Section::Vdevs, host)` — it does not
    /// include the pool name. This means drilling into a *different*
    /// pool while a previous pool's vdev refresh for the same host is
    /// still in flight will queue the new request as pending rather than
    /// starting it immediately; the user can press 'r' again once the
    /// stale one clears. This is intentionally simple: correctness (never
    /// showing one pool's disks under another pool's title) is fully
    /// handled by the stale-pool check in `drain_worker_messages`, so the
    /// in-flight key doesn't need to be pool-aware too.
    pub fn refresh_vdevs(&mut self, runner: Arc<dyn CommandRunner>, pool: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Vdevs, host) {
            self.pending_refresh.insert((Section::Vdevs, host));
            return;
        }
        self.begin_refresh(Section::Vdevs, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::pool::get_pool_vdevs(runner.as_ref(), &pool);
            let _ = tx.send(WorkerMsg::Vdevs(host, pool, result));
        });
    }

    /// See `refresh_pools` for the in-flight/pending-refresh behavior.
    /// Not pool-aware in its in-flight key, same simplifying tradeoff
    /// `refresh_vdevs` makes for pools — correctness (never showing one
    /// pool's properties under another's) is fully handled by the
    /// stale-identity discard in `drain_worker_messages`.
    pub fn refresh_pool_properties(&mut self, runner: Arc<dyn CommandRunner>, pool: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::PoolProperties, host) {
            self.pending_refresh.insert((Section::PoolProperties, host));
            return;
        }
        self.begin_refresh(Section::PoolProperties, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::pool::get_pool_properties(runner.as_ref(), &pool);
            let _ = tx.send(WorkerMsg::PoolProperties(host, pool, result));
        });
    }

    /// See `refresh_pool_properties`.
    pub fn refresh_dataset_properties(&mut self, runner: Arc<dyn CommandRunner>, dataset: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::DatasetProperties, host) {
            self.pending_refresh.insert((Section::DatasetProperties, host));
            return;
        }
        self.begin_refresh(Section::DatasetProperties, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::dataset::get_dataset_properties(runner.as_ref(), &dataset);
            let _ = tx.send(WorkerMsg::DatasetProperties(host, dataset, result));
        });
    }

    /// See `refresh_dataset_properties`.
    pub fn refresh_dataset_permissions(&mut self, runner: Arc<dyn CommandRunner>, dataset: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::DatasetPermissions, host) {
            self.pending_refresh.insert((Section::DatasetPermissions, host));
            return;
        }
        self.begin_refresh(Section::DatasetPermissions, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::permission::list_permissions(runner.as_ref(), &dataset);
            let _ = tx.send(WorkerMsg::DatasetPermissions(host, dataset, result));
        });
    }

    /// See `refresh_dataset_permissions`. `path` is the dataset's
    /// mountpoint, not its name -- ACLs are filesystem-path-scoped.
    pub fn refresh_dataset_acl(&mut self, runner: Arc<dyn CommandRunner>, path: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::DatasetAcl, host) {
            self.pending_refresh.insert((Section::DatasetAcl, host));
            return;
        }
        self.begin_refresh(Section::DatasetAcl, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::acl::list_acl(runner.as_ref(), &path);
            let _ = tx.send(WorkerMsg::DatasetAcl(host, path, result));
        });
    }

    /// Drains all pending worker messages, applying each to `self`.
    /// Returns the list of sections that need re-listing, either because
    /// a mutation targeting the currently-selected host just succeeded,
    /// or because a refresh request for that section arrived while a
    /// same-section refresh was already in flight and got queued in
    /// `pending_refresh` (see `refresh_pools` et al.) instead of being
    /// dropped — the caller (event loop) looks up the right runner and
    /// triggers the actual refresh in both cases, since `App` has no way
    /// to resolve a host index to a runner on its own.
    pub fn drain_worker_messages(&mut self) -> Vec<Section> {
        let mut refreshes_needed = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Pools(host, result) => {
                    self.end_refresh(Section::Pools, host);
                    // A refresh request that arrived while this one was in
                    // flight (see `refresh_pools`) is fulfilled by queuing a
                    // fresh one now, rather than staying lost forever.
                    let had_pending = self.pending_refresh.remove(&(Section::Pools, host));
                    if host != self.selected_host {
                        // Late result for a host we've since navigated
                        // away from — discard silently, it's not an error.
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::Pools);
                    }
                    match result {
                        Ok(pools) => {
                            self.pools = pools;
                            clamp_table_selection(&mut self.pools_table, self.pools.len());
                            self.status_message = None;
                            // The list may have been reordered/resized in a
                            // way that re-points the still-selected index at
                            // a different pool than the one `pool_properties`
                            // was fetched for — if so, the cached properties
                            // are now stale and would render under the
                            // wrong pool's title (see Finding 1). Only
                            // invalidate on an actual mismatch; the common
                            // case (never viewed, or still the same pool)
                            // must be left untouched.
                            if self.pool_properties_for.is_some() && self.pool_properties_for != self.selected_pool_name() {
                                self.pool_properties.clear();
                                self.pool_properties_table.select(None);
                                self.pool_properties_for = None;
                            }
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::Datasets(host, result) => {
                    self.end_refresh(Section::Datasets, host);
                    let had_pending = self.pending_refresh.remove(&(Section::Datasets, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::Datasets);
                    }
                    match result {
                        Ok(datasets) => {
                            self.datasets = datasets;
                            clamp_table_selection(&mut self.datasets_table, self.datasets.len());
                            self.status_message = None;
                            // See the matching comment in the `Pools` arm.
                            if self.dataset_properties_for.is_some() && self.dataset_properties_for != self.selected_dataset_name() {
                                self.dataset_properties.clear();
                                self.dataset_properties_table.select(None);
                                self.dataset_properties_for = None;
                            }
                            if self.dataset_permissions_for.is_some() && self.dataset_permissions_for != self.selected_dataset_name() {
                                self.dataset_permissions.clear();
                                self.dataset_permissions_table.select(None);
                                self.dataset_permissions_for = None;
                            }
                            if self.acl_entries_for.is_some() && self.acl_entries_for != self.selected_dataset_mountpoint() {
                                self.acl_entries.clear();
                                self.acl_entries_table.select(None);
                                self.acl_entries_for = None;
                            }
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::Snapshots(host, result) => {
                    self.end_refresh(Section::Snapshots, host);
                    let had_pending = self.pending_refresh.remove(&(Section::Snapshots, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::Snapshots);
                    }
                    match result {
                        Ok(snapshots) => {
                            self.snapshots = snapshots;
                            clamp_table_selection(&mut self.snapshots_table, self.snapshots.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::Disks(host, result) => {
                    self.end_refresh(Section::Disks, host);
                    let had_pending = self.pending_refresh.remove(&(Section::Disks, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::Disks);
                    }
                    match result {
                        Ok(disks) => {
                            // Drop any selected disk that no longer
                            // exists, or has stopped being Available
                            // (claimed by a concurrent operation, or a
                            // slow refresh landing late) — a stale
                            // selection must not be submittable against
                            // a device that's no longer free.
                            self.selected_disks.retain(|name| {
                                disks.iter().any(|d| &d.name == name && d.membership == DiskMembership::Available)
                            });
                            self.disks = disks;
                            clamp_table_selection(&mut self.disks_table, self.disks.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::Shares(host, result) => {
                    self.end_refresh(Section::Shares, host);
                    let had_pending = self.pending_refresh.remove(&(Section::Shares, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::Shares);
                    }
                    match result {
                        Ok(snapshot) => {
                            self.shares = snapshot.shares;
                            clamp_table_selection(&mut self.shares_table, self.shares.len());
                            self.nfs_available = Some(snapshot.nfs_available);
                            self.samba_available = Some(snapshot.samba_available);
                            self.shares_loaded = true;
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::Vdevs(host, pool, result) => {
                    self.end_refresh(Section::Vdevs, host);
                    let had_pending = self.pending_refresh.remove(&(Section::Vdevs, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::Vdevs);
                    }
                    // Stale-pool check: a slow refresh for a pool the user
                    // has since backed out of (or replaced with a different
                    // drill-down) must not overwrite the vdev pane with the
                    // wrong pool's disks under the current pool's title.
                    if Some(&pool) != self.drilled_pool.as_ref() {
                        continue;
                    }
                    match result {
                        Ok((vdevs, scan_status)) => {
                            self.vdevs = vdevs;
                            clamp_table_selection(&mut self.vdevs_table, self.vdevs.len());
                            self.pool_scan_status = scan_status;
                            self.pool_scan_status_shown_at = Some(Instant::now());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::PoolProperties(host, pool, result) => {
                    self.end_refresh(Section::PoolProperties, host);
                    let had_pending = self.pending_refresh.remove(&(Section::PoolProperties, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::PoolProperties);
                    }
                    if Some(&pool) != self.selected_pool_name().as_ref() {
                        continue;
                    }
                    match result {
                        Ok(props) => {
                            self.pool_properties = props;
                            clamp_table_selection(&mut self.pool_properties_table, self.pool_properties.len());
                            self.pool_properties_for = Some(pool);
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::DatasetProperties(host, dataset, result) => {
                    self.end_refresh(Section::DatasetProperties, host);
                    let had_pending = self.pending_refresh.remove(&(Section::DatasetProperties, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::DatasetProperties);
                    }
                    if Some(&dataset) != self.selected_dataset_name().as_ref() {
                        continue;
                    }
                    match result {
                        Ok(props) => {
                            self.dataset_properties = props;
                            clamp_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len());
                            self.dataset_properties_for = Some(dataset);
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::DatasetPermissions(host, dataset, result) => {
                    self.end_refresh(Section::DatasetPermissions, host);
                    let had_pending = self.pending_refresh.remove(&(Section::DatasetPermissions, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::DatasetPermissions);
                    }
                    if Some(&dataset) != self.selected_dataset_name().as_ref() {
                        continue;
                    }
                    match result {
                        Ok(snapshot) => {
                            self.dataset_permissions = flatten_permissions_snapshot(&snapshot);
                            clamp_table_selection(&mut self.dataset_permissions_table, self.dataset_permissions.len());
                            self.dataset_permissions_for = Some(dataset);
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::DatasetAcl(host, path, result) => {
                    self.end_refresh(Section::DatasetAcl, host);
                    let had_pending = self.pending_refresh.remove(&(Section::DatasetAcl, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::DatasetAcl);
                    }
                    if Some(&path) != self.selected_dataset_mountpoint().as_ref() {
                        continue;
                    }
                    match result {
                        Ok(entries) => {
                            self.acl_entries = entries;
                            clamp_table_selection(&mut self.acl_entries_table, self.acl_entries.len());
                            self.acl_entries_for = Some(path);
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::MutationDone(host, section, result) => {
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(()) => {
                            self.status_message = None;
                            refreshes_needed.push(section);
                            if section == Section::Vdevs {
                                // A disk going offline/online/being replaced
                                // changes the parent pool's health too —
                                // the one deliberate two-section exception.
                                refreshes_needed.push(Section::Pools);
                            }
                            if section == Section::Vdevs || section == Section::Pools {
                                // Both changing a pool's vdev tree
                                // (add/replace vdev) and creating/destroying
                                // a pool outright change which disks are
                                // claimed — the Disks section must catch up,
                                // and any selection made against the
                                // pre-mutation disk list is stale now.
                                refreshes_needed.push(Section::Disks);
                                self.selected_disks.clear();
                            }
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
            }
        }
        refreshes_needed
    }
}

/// Builds a `zpool create`/`zpool add` vdev spec from a Disks-section
/// selection: `disks` verbatim (each disk becomes its own top-level
/// vdev — a stripe) when `redundancy` is blank, or `[redundancy] +
/// disks` (one mirror/raidz group over every selected disk) otherwise.
fn build_vdev_spec_from_selection(redundancy: &str, disks: &[String]) -> Vec<String> {
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
/// host. SMB share creation/editing is not supported — `zfs set
/// sharesmb=on` does not actually work on FreeBSD (confirmed empirically:
/// it prints "No SMB support in FreeBSD yet." and fails, despite storing
/// the property) — so zfsctl never calls this with a `Share::Smb`.
fn share_edit_modal(share: &crate::zfs::share::Share, host: usize) -> Modal {
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
        crate::zfs::share::Share::Smb { .. } => {
            unreachable!("share_edit_modal is only ever called with a Share::Nfs — see handle_key's Shares Enter arm and Datasets 'N' arm")
        }
    }
}

/// Builds the `PendingAction` for a submitted form, mapping its fields
/// positionally per `FormKind`'s documented field order. Returns `None`
/// if a required field is empty — the caller keeps the form modal open
/// in that case rather than losing the user's input.
fn build_pending_action(kind: &FormKind, fields: &[FormField]) -> Option<PendingAction> {
    match kind {
        FormKind::CreatePool { host } => {
            let name = fields[0].value.trim();
            let vdev_spec_raw = fields[1].value.trim();
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
            let name = fields[0].value.trim();
            if name.is_empty() {
                return None;
            }
            let redundancy = fields[1].value.trim();
            Some(PendingAction {
                host: *host,
                section: Section::Pools,
                call: MutationCall::CreatePool { name: name.to_string(), vdev_spec: build_vdev_spec_from_selection(redundancy, disks) },
            })
        }
        FormKind::AddVdev { host, pool } => {
            let vdev_spec_raw = fields[0].value.trim();
            if vdev_spec_raw.is_empty() {
                return None;
            }
            let vdev_spec: Vec<String> = vdev_spec_raw.split_whitespace().map(String::from).collect();
            Some(PendingAction {
                host: *host,
                section: Section::Vdevs,
                call: MutationCall::AddVdev { pool: pool.clone(), vdev_spec },
            })
        }
        FormKind::AddVdevFromDisks { host, disks } => {
            let pool = fields[0].value.trim();
            if pool.is_empty() {
                return None;
            }
            let redundancy = fields[1].value.trim();
            Some(PendingAction {
                host: *host,
                section: Section::Vdevs,
                call: MutationCall::AddVdev { pool: pool.to_string(), vdev_spec: build_vdev_spec_from_selection(redundancy, disks) },
            })
        }
        FormKind::ReplaceDisk { host, pool, old_disk } => {
            let new_disk = fields[0].value.trim();
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
            let value = fields[0].value.clone();
            Some(PendingAction {
                host: *host,
                section: Section::PoolProperties,
                call: MutationCall::SetPoolProperty { pool: pool.clone(), key: key.clone(), value },
            })
        }
        FormKind::CreateDataset { host } => {
            let name = fields[0].value.trim();
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
            let value = fields[0].value.clone();
            Some(PendingAction {
                host: *host,
                section: Section::DatasetProperties,
                call: MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: key.clone(), value },
            })
        }
        FormKind::CreateSnapshot { host, dataset } => {
            let name = fields[0].value.trim();
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
            let networks: Vec<String> = fields[0].value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let hosts: Vec<String> = fields[1].value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let read_only = fields[2].value.trim().to_lowercase().starts_with('y');
            let root = fields[3].value.trim();
            let root = if root.is_empty() { None } else { Some(root) };
            let value = crate::zfs::share::assemble_sharenfs(&networks, &hosts, read_only, root);
            Some(PendingAction {
                host: *host,
                section: Section::Shares,
                call: MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: "sharenfs".to_string(), value },
            })
        }
        FormKind::GrantPermission { host, dataset } => {
            let recipient = parse_recipient_field(&fields[0].value)?;
            let scope = parse_scope_field(&fields[1].value)?;
            let perms = parse_required_perms_field(&fields[2].value)?;
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
            let name = fields[0].value.trim();
            if name.is_empty() {
                return None;
            }
            let perms = parse_required_perms_field(&fields[1].value)?;
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
            let perms = parse_required_perms_field(&fields[0].value)?;
            Some(PendingAction {
                host: *host,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission { dataset: dataset.clone(), target: crate::zfs::permission::PermissionTarget::CreateTime, perms },
            })
        }
        FormKind::RevokePermissions { host, dataset, target } => {
            let perms = parse_required_perms_field(&fields[0].value)?;
            Some(PendingAction {
                host: *host,
                section: Section::DatasetPermissions,
                call: MutationCall::UnallowPermission { dataset: dataset.clone(), target: target.clone(), perms: Some(perms) },
            })
        }
        FormKind::AddAclEntry { host, path, position } => {
            let tag = parse_acl_recipient_field(&fields[0].value)?;
            let permissions = parse_required_perms_field(&fields[1].value)?;
            let flags = parse_optional_flags_field(&fields[2].value);
            let entry_type = parse_acl_type_field(&fields[3].value)?;
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
    }
}

/// Parses the `Recipient` field's mini-language: `everyone`,
/// `user:<name>`, or `group:<name>` — no bare-name fallback, since this
/// app always passes explicit `-u`/`-g` to `zfs allow` (see the spec).
fn parse_recipient_field(s: &str) -> Option<crate::zfs::permission::Recipient> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("everyone") {
        return Some(crate::zfs::permission::Recipient::Everyone);
    }
    if let Some(name) = s.strip_prefix("user:") {
        if !name.is_empty() {
            return Some(crate::zfs::permission::Recipient::User(name.to_string()));
        }
        return None;
    }
    if let Some(name) = s.strip_prefix("group:") {
        if !name.is_empty() {
            return Some(crate::zfs::permission::Recipient::Group(name.to_string()));
        }
        return None;
    }
    None
}

/// Parses the `Scope` field — blank defaults to `Both`, anything else
/// must be exactly `local`/`descendent`/`both` (case-insensitive) or
/// the caller must treat it as invalid. Deliberately strict (unlike
/// most optional fields in this app): silently falling back to the
/// *widest* scope on a typo would be a real footgun for a permissions
/// feature specifically.
fn parse_scope_field(s: &str) -> Option<crate::zfs::permission::Scope> {
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
fn parse_required_perms_field(s: &str) -> Option<Vec<String>> {
    let perms: Vec<String> = s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
    if perms.is_empty() {
        None
    } else {
        Some(perms)
    }
}

/// Parses the ACL `Recipient` field's mini-language: `owner`, `group`,
/// `everyone`, `user:<name>`, or `group:<name>` — five shapes (one more
/// than the Permissions sub-tab's three, since ACLs distinguish the
/// owning group `group@` from a specific named group).
fn parse_acl_recipient_field(s: &str) -> Option<crate::acl::AclTag> {
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
    if let Some(name) = s.strip_prefix("user:") {
        if !name.is_empty() {
            return Some(crate::acl::AclTag::User(name.to_string()));
        }
        return None;
    }
    if let Some(name) = s.strip_prefix("group:") {
        if !name.is_empty() {
            return Some(crate::acl::AclTag::NamedGroup(name.to_string()));
        }
        return None;
    }
    None
}

/// Parses the ACL `Type` field — blank defaults to `Allow` (the more
/// common case), anything else must be exactly `allow`/`deny`
/// (case-insensitive). Deliberately strict, same reasoning as
/// Permissions' `Scope` field: silently defaulting a typo to the
/// *less restrictive* `allow` would be a real footgun.
fn parse_acl_type_field(s: &str) -> Option<crate::acl::AclEntryType> {
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
fn parse_optional_flags_field(s: &str) -> Vec<String> {
    s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// Reconstructs the `PermissionTarget` a `PermissionRow` was flattened
/// from — used by `'d'`/`'x'` to revoke exactly the target the selected
/// row represents.
fn permission_target_for_row(row: &PermissionRow) -> crate::zfs::permission::PermissionTarget {
    match row {
        PermissionRow::Set { name, .. } => crate::zfs::permission::PermissionTarget::Set(name.clone()),
        PermissionRow::CreateTime { .. } => crate::zfs::permission::PermissionTarget::CreateTime,
        PermissionRow::Grant { recipient, scope, .. } => {
            crate::zfs::permission::PermissionTarget::Grant { recipient: recipient.clone(), scope: *scope }
        }
    }
}

/// Moves a `TableState`'s selection by one row within `[0, len)`,
/// wrapping at either end (consistent with the Hosts/section navigation
/// style elsewhere in `App`). No-ops (selection stays `None`) when
/// `len == 0` — there's nothing to select in an empty list.
fn move_table_selection(state: &mut TableState, len: usize, forward: bool) {
    if len == 0 {
        state.select(None);
        return;
    }
    let next = match (state.selected(), forward) {
        (None, true) => 0,
        (None, false) => len - 1,
        (Some(i), true) => (i + 1) % len,
        (Some(i), false) => {
            if i == 0 {
                len - 1
            } else {
                i - 1
            }
        }
    };
    state.select(Some(next));
}

/// Pulls a `TableState`'s selection back into range after the
/// underlying list shrinks (e.g. a refresh returns fewer rows than
/// before). Selects `None` if the list is now empty.
fn clamp_table_selection(state: &mut TableState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    if let Some(i) = state.selected() {
        if i >= len {
            state.select(Some(len - 1));
        }
    }
}

fn about_text() -> String {
    format!(
        "zfsctl v{}\n\nA terminal UI for managing ZFS storage on FreeBSD.\n\nBuilt in collaboration with Claude (Anthropic).\n\nPress any key to close.",
        env!("CARGO_PKG_VERSION")
    )
}

fn help_text() -> String {
    "\
Global
  Tab / Shift+Tab  switch sections
  Up / Down        select a row (host/pool/dataset/snapshot/disk/share, or a property/permission/ACL row)
  r                refresh the focused pane
  Esc              dismiss a status message, back out of a focused pane, or cancel a modal
  ?                this Help screen
  A                About
  q                quit

Pools
  Enter            drill into vdev/disk tree
  Right / Left     focus/unfocus the properties pane
  n / d            create / destroy a pool
  s / R            scrub / resilver (y/n confirm)

Vdevs (inside a drilled-into pool)
  o / O            offline / online the selected disk
  x                replace the selected disk
  a                add a vdev/mirror/raidz group

Pool/Dataset properties pane
  Enter            edit the selected property
  r                re-fetch for the current pool/dataset

Datasets
  Right / Left     focus/unfocus the detail pane
  n / d            create / destroy a dataset
  s                snapshot the selected dataset
  N                create/edit an NFS share
  '[' / ']'        cycle Properties / Permissions / ACL sub-tabs (once focused)

Datasets: Permissions sub-tab
  n / s / c        grant: to a recipient / as a named set / as create-time
  d / x            revoke everything / revoke specific permissions

Datasets: ACL sub-tab
  n                add an ACL entry (always appended at the end)
  d                remove the selected entry by position

Snapshots
  d                destroy (type dataset@name to confirm)
  b                roll back (type dataset@name to confirm)

Disks
  Space            toggle-select a disk
  n / a            create a pool / add a vdev from the selected disk(s)
  Esc              clear selection

Shares
  Enter            edit an NFS share (SMB shares are view/remove only)
  d                remove the selected share

Press any key to close."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;
    use crate::zfs::property::PropertySource;

    #[test]
    fn flatten_permissions_snapshot_produces_one_row_per_entry_in_a_stable_order() {
        let snapshot = crate::zfs::permission::PermissionsSnapshot {
            sets: vec![crate::zfs::permission::PermissionSet { name: "admin".to_string(), perms: vec!["snapshot".to_string()] }],
            create_time: vec!["destroy".to_string()],
            grants: vec![crate::zfs::permission::PermissionGrant {
                recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
                scope: crate::zfs::permission::Scope::Both,
                perms: vec!["mount".to_string()],
            }],
        };
        let rows = flatten_permissions_snapshot(&snapshot);
        assert_eq!(
            rows,
            vec![
                PermissionRow::Set { name: "admin".to_string(), perms: vec!["snapshot".to_string()] },
                PermissionRow::CreateTime { perms: vec!["destroy".to_string()] },
                PermissionRow::Grant {
                    recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
                    scope: crate::zfs::permission::Scope::Both,
                    perms: vec!["mount".to_string()],
                },
            ]
        );
    }

    #[test]
    fn refresh_dataset_permissions_populates_app_state_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(
            "---- Permissions on tank/data -------------------------------------\nLocal+Descendent permissions:\n        user fb snapshot\n",
        ));
        app.refresh_dataset_permissions(runner, "tank/data".to_string());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.dataset_permissions.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            app.dataset_permissions,
            vec![PermissionRow::Grant { recipient: crate::zfs::permission::Recipient::User("fb".to_string()), scope: crate::zfs::permission::Scope::Both, perms: vec!["snapshot".to_string()] }]
        );
        assert_eq!(app.dataset_permissions_for, Some("tank/data".to_string()));
    }

    #[test]
    fn dataset_permissions_result_for_a_stale_dataset_is_discarded() {
        let mut app = App::new(vec!["local".to_string()]);
        app.tx
            .send(WorkerMsg::DatasetPermissions(0, "tank/old".to_string(), Ok(crate::zfs::permission::PermissionsSnapshot::default())))
            .unwrap();
        app.datasets = vec![Dataset { name: "tank/new".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.drain_worker_messages();
        assert_eq!(app.dataset_permissions_for, None, "a result for a dataset that's no longer selected must not populate the pane");
    }

    #[test]
    fn entering_right_focus_on_datasets_resets_to_properties_tab() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.dataset_detail_tab = DatasetDetailTab::Permissions;
        app.handle_key(KeyCode::Right);
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Properties, "re-entering the right pane must never silently keep a stale sub-tab");
    }

    #[test]
    fn bracket_keys_toggle_dataset_detail_tab_only_in_datasets_right_focus() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Char('[')); // Pools right pane has no sub-tab -- no-op
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Properties);

        app.handle_key(KeyCode::Tab); // -> Datasets
        app.handle_key(KeyCode::Right); // enter right focus
        app.handle_key(KeyCode::Char(']'));
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Permissions);
        app.handle_key(KeyCode::Char('['));
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Properties);
    }

    #[test]
    fn bracket_keys_cycle_through_all_three_dataset_detail_tabs() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.handle_key(KeyCode::Right); // enter right focus, Properties tab
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Properties);
        app.handle_key(KeyCode::Char(']'));
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Permissions);
        app.handle_key(KeyCode::Char(']'));
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Acl);
        app.handle_key(KeyCode::Char(']')); // wraps back to Properties
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Properties);
        app.handle_key(KeyCode::Char('[')); // wraps the other way
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Acl);
        app.handle_key(KeyCode::Char('['));
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Permissions);
    }

    #[test]
    fn refresh_dataset_acl_populates_app_state_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(
            "# file: /tank/data\n# owner: root\n# group: wheel\n            owner@:read_data/write_data/execute::allow\n",
        ));
        app.refresh_dataset_acl(runner, "/tank/data".to_string());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.acl_entries.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            app.acl_entries,
            vec![crate::acl::AclEntry {
                tag: crate::acl::AclTag::Owner,
                permissions: vec!["read_data".to_string(), "write_data".to_string(), "execute".to_string()],
                flags: vec![],
                entry_type: crate::acl::AclEntryType::Allow,
            }]
        );
        assert_eq!(app.acl_entries_for, Some("/tank/data".to_string()));
    }

    #[test]
    fn acl_result_for_a_stale_mountpoint_is_discarded() {
        let mut app = App::new(vec!["local".to_string()]);
        app.tx.send(WorkerMsg::DatasetAcl(0, "/tank/old".to_string(), Ok(vec![]))).unwrap();
        app.datasets = vec![Dataset { name: "tank/new".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/new".to_string()) }];
        app.datasets_table.select(Some(0));
        app.acl_entries_for = Some("placeholder".to_string());
        app.drain_worker_messages();
        assert_eq!(app.acl_entries_for, Some("placeholder".to_string()), "a result for a mountpoint that's no longer selected must not overwrite the current state");
    }

    #[test]
    fn datasets_list_refresh_that_repoints_selection_clears_stale_acl_entries() {
        // Same class of bug Permissions' final review caught for
        // dataset_permissions_for -- built in from the start here.
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/old".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/old".to_string()) }];
        app.datasets_table.select(Some(0));
        app.acl_entries = vec![crate::acl::AclEntry { tag: crate::acl::AclTag::Owner, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow }];
        app.acl_entries_table.select(Some(0));
        app.acl_entries_for = Some("/tank/old".to_string());

        app.tx
            .send(WorkerMsg::Datasets(
                0,
                Ok(vec![Dataset { name: "tank/new".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/new".to_string()) }]),
            ))
            .unwrap();
        app.drain_worker_messages();

        assert!(app.acl_entries.is_empty(), "stale ACL entries for tank/old must be cleared once the list refresh re-points the selection at tank/new");
        assert_eq!(app.acl_entries_for, None);
    }

    #[test]
    fn handle_key_s_on_pools_opens_scrub_confirm_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab); // Hosts -> Pools
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Char('s'));
        match app.modal {
            Some(crate::ui::modal::Modal::Confirm { action, .. }) => {
                assert_eq!(action.host, 0);
                assert_eq!(action.section, Section::Pools);
                assert_eq!(action.call, crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() });
            }
            other => panic!("expected Confirm modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_s_on_pools_is_noop_while_right_focused() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab); // Hosts -> Pools
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Right); // focus properties pane
        assert_eq!(app.pane_focus, PaneFocus::Right);
        app.handle_key(KeyCode::Char('s'));
        assert!(app.modal.is_none(), "scrub key must be a no-op while the properties pane is focused");
    }

    #[test]
    fn handle_key_d_on_pools_is_noop_while_right_focused() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Char('d'));
        assert!(app.modal.is_none(), "destroy key must be a no-op while the properties pane is focused");
    }

    #[test]
    fn handle_key_n_on_pools_is_noop_while_right_focused() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Char('n'));
        assert!(app.modal.is_none(), "create-pool key must be a no-op while the properties pane is focused");
    }

    #[test]
    fn handle_key_n_on_datasets_is_noop_while_right_focused() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Char('n'));
        assert!(app.modal.is_none(), "create-dataset key must be a no-op while the properties pane is focused");
    }

    #[test]
    fn handle_key_d_on_datasets_is_noop_while_right_focused() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Char('d'));
        assert!(app.modal.is_none(), "destroy-dataset key must be a no-op while the properties pane is focused");
    }

    #[test]
    fn handle_key_s_on_datasets_is_noop_while_right_focused() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Char('s'));
        assert!(app.modal.is_none(), "create-snapshot key must be a no-op while the properties pane is focused");
    }

    #[test]
    fn handle_key_s_with_no_row_selected_does_not_open_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char('s'));
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_key_d_on_pools_opens_destroy_confirm_typed_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "scratch".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Char('d'));
        match app.modal {
            Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
                assert_eq!(expected, "scratch");
                assert_eq!(action.call, crate::ui::modal::MutationCall::DestroyPool { pool: "scratch".to_string() });
            }
            other => panic!("expected ConfirmTyped modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_modal_key_confirm_yes_returns_action_and_clears_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        let action = crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Pools,
            call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
        };
        app.modal = Some(crate::ui::modal::Modal::Confirm { prompt: "Scrub tank? (y/n)".to_string(), action: action.clone() });
        let result = app.handle_modal_key(KeyCode::Char('y'));
        assert_eq!(result, Some(action));
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_modal_key_confirm_enter_also_submits() {
        let mut app = App::new(vec!["local".to_string()]);
        let action = crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Pools,
            call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
        };
        app.modal = Some(crate::ui::modal::Modal::Confirm { prompt: "Scrub tank? (y/n)".to_string(), action });
        let result = app.handle_modal_key(KeyCode::Enter);
        assert!(result.is_some());
    }

    #[test]
    fn handle_modal_key_confirm_no_cancels_and_clears_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        let action = crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Pools,
            call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
        };
        app.modal = Some(crate::ui::modal::Modal::Confirm { prompt: "Scrub tank? (y/n)".to_string(), action });
        let result = app.handle_modal_key(KeyCode::Char('n'));
        assert_eq!(result, None);
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_modal_key_confirm_unrecognized_key_keeps_modal_open() {
        let mut app = App::new(vec!["local".to_string()]);
        let action = crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Pools,
            call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
        };
        app.modal = Some(crate::ui::modal::Modal::Confirm { prompt: "Scrub tank? (y/n)".to_string(), action });
        let result = app.handle_modal_key(KeyCode::Char('x'));
        assert_eq!(result, None);
        assert!(app.modal.is_some(), "an unrecognized key must not silently dismiss the confirmation");
    }

    #[test]
    fn handle_modal_key_confirm_typed_requires_exact_match() {
        let mut app = App::new(vec!["local".to_string()]);
        let action = crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Pools,
            call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
        };
        app.modal = Some(crate::ui::modal::Modal::ConfirmTyped {
            prompt: "Type the pool name:".to_string(),
            expected: "tank".to_string(),
            input: "tan".to_string(),
            action: action.clone(),
        });
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(result, None, "submitting with a mismatched confirmation must not run the action");
        match &app.modal {
            Some(crate::ui::modal::Modal::ConfirmTyped { input, .. }) => assert_eq!(input, "tan"),
            other => panic!("expected the modal to stay open, got {other:?}"),
        }

        // Type the missing character, then submit again.
        let result = app.handle_modal_key(KeyCode::Char('k'));
        assert_eq!(result, None);
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(result, Some(action));
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_modal_key_confirm_typed_backspace_edits_input() {
        let mut app = App::new(vec!["local".to_string()]);
        let action = crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Pools,
            call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
        };
        app.modal = Some(crate::ui::modal::Modal::ConfirmTyped {
            prompt: "Type the pool name:".to_string(),
            expected: "tank".to_string(),
            input: "tanx".to_string(),
            action,
        });
        app.handle_modal_key(KeyCode::Backspace);
        match &app.modal {
            Some(crate::ui::modal::Modal::ConfirmTyped { input, .. }) => assert_eq!(input, "tan"),
            other => panic!("expected the modal to stay open, got {other:?}"),
        }
    }

    #[test]
    fn handle_modal_key_confirm_typed_esc_cancels() {
        let mut app = App::new(vec!["local".to_string()]);
        let action = crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Pools,
            call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
        };
        app.modal = Some(crate::ui::modal::Modal::ConfirmTyped {
            prompt: "Type the pool name:".to_string(),
            expected: "tank".to_string(),
            input: "tank".to_string(),
            action,
        });
        let result = app.handle_modal_key(KeyCode::Esc);
        assert_eq!(result, None);
        assert!(app.modal.is_none());
    }

    #[test]
    fn dispatch_mutation_success_queues_refresh_for_its_section() {
        let mut app = App::new(vec!["local".to_string()]);
        let action = crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Pools,
            call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
        };
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(""));
        app.dispatch_mutation(action, runner);

        let deadline = Instant::now() + Duration::from_secs(2);
        let refreshes = loop {
            let r = app.drain_worker_messages();
            if !r.is_empty() || Instant::now() > deadline {
                break r;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(refreshes, vec![Section::Pools, Section::Disks]);
        assert_eq!(app.status_message, None);
    }

    #[test]
    fn dispatch_mutation_failure_sets_status_message_and_queues_no_refresh() {
        let mut app = App::new(vec!["local".to_string()]);
        let action = crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Pools,
            call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
        };
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::failing("pool is busy", 1));
        app.dispatch_mutation(action, runner);

        let deadline = Instant::now() + Duration::from_secs(2);
        let refreshes = loop {
            let r = app.drain_worker_messages();
            if app.status_message.is_some() || Instant::now() > deadline {
                break r;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(refreshes.is_empty());
        assert!(app.status_message.is_some());
    }

    #[test]
    fn dispatch_mutation_result_for_stale_host_is_discarded() {
        // Simulates: a mutation was dispatched against host 0, the user
        // switched to host 1, and only then did host 0's result arrive.
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.tx.send(WorkerMsg::MutationDone(0, Section::Pools, Ok(()))).unwrap();
        app.selected_host = 1; // navigated away before the result arrived
        let refreshes = app.drain_worker_messages();
        assert!(refreshes.is_empty(), "a mutation result for a host we've navigated away from must not queue a refresh");
        assert_eq!(app.status_message, None);
    }

    #[test]
    fn handle_key_navigates_sections_and_wraps() {
        let mut app = App::new(vec!["local".to_string()]);
        assert_eq!(app.current_section(), Section::Hosts);
        app.handle_key(KeyCode::Tab);
        assert_eq!(app.current_section(), Section::Pools);
        app.handle_key(KeyCode::BackTab);
        assert_eq!(app.current_section(), Section::Hosts);
        app.handle_key(KeyCode::BackTab);
        assert_eq!(app.current_section(), Section::Shares);
    }

    #[test]
    fn handle_key_quit_sets_should_quit() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn handle_key_question_mark_opens_help_info_modal_from_any_section() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools, to confirm it's not section-gated
        app.handle_key(KeyCode::Char('?'));
        match &app.modal {
            Some(Modal::Info { title, .. }) => assert_eq!(title, "Help"),
            other => panic!("expected Info modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_capital_a_opens_about_info_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('A'));
        match &app.modal {
            Some(Modal::Info { title, body }) => {
                assert_eq!(title, "About");
                assert!(body.contains("zfsctl"));
                assert!(body.contains("Claude"));
            }
            other => panic!("expected Info modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_capital_a_does_not_shadow_section_gated_lowercase_a() {
        // The existing section-gated lowercase 'a' bindings (e.g. add
        // vdev) must keep working -- only capital 'A' is the new global
        // About key.
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('A'));
        assert!(app.modal.is_some());
        app.handle_modal_key(KeyCode::Esc);
        assert!(app.modal.is_none());
        app.handle_key(KeyCode::Char('a'));
        assert!(app.modal.is_none(), "lowercase 'a' outside any section-gated context must stay a no-op, not open About");
    }

    #[test]
    fn esc_closes_the_info_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('?'));
        assert!(app.modal.is_some());
        let result = app.handle_modal_key(KeyCode::Esc);
        assert!(result.is_none());
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_key_navigates_hosts_and_wraps() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        assert_eq!(app.selected_host, 0);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_host, 1);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_host, 0);
        app.handle_key(KeyCode::Up);
        assert_eq!(app.selected_host, 1);
    }

    #[test]
    fn drain_worker_messages_updates_pools_on_success() {
        let mut app = App::new(vec!["local".to_string()]);
        let pool = Pool {
            name: "tank".to_string(),
            size: 100,
            allocated: 10,
            free: 90,
            health: crate::zfs::pool::PoolHealth::Online,
        };
        app.tx.send(WorkerMsg::Pools(0, Ok(vec![pool.clone()]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.pools, vec![pool]);
        assert!(!app.is_refreshing(Section::Pools, 0));
    }

    #[test]
    fn drain_worker_messages_sets_status_message_on_failure() {
        let mut app = App::new(vec!["local".to_string()]);
        app.tx
            .send(WorkerMsg::Datasets(0, Err(crate::error::AppError::Parse("boom".to_string()))))
            .unwrap();
        app.drain_worker_messages();
        assert!(app.status_message.is_some());
    }

    #[test]
    fn esc_clears_existing_status_message() {
        let mut app = App::new(vec!["local".to_string()]);
        app.status_message = Some("something went wrong".to_string());
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.status_message, None);
    }

    #[test]
    fn drain_worker_messages_clears_status_message_on_subsequent_success() {
        let mut app = App::new(vec!["local".to_string()]);
        app.status_message = Some("stale error".to_string());
        app.tx.send(WorkerMsg::Pools(0, Ok(vec![]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.status_message, None);
    }

    #[test]
    fn refresh_pools_populates_pools_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
        app.refresh_pools(runner);

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.pools.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.pools.len(), 1);
        assert_eq!(app.pools[0].name, "tank");
    }

    #[test]
    fn drain_worker_messages_discards_result_for_stale_host() {
        // Simulates: a refresh was in flight for host 0, the user switched
        // to host 1, and only then did host 0's (slow) result arrive.
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.selected_host = 1;
        let pool = Pool {
            name: "tank".to_string(),
            size: 100,
            allocated: 10,
            free: 90,
            health: crate::zfs::pool::PoolHealth::Online,
        };
        app.begin_refresh(Section::Pools, 0);
        app.tx.send(WorkerMsg::Pools(0, Ok(vec![pool]))).unwrap();
        app.drain_worker_messages();

        assert!(app.pools.is_empty(), "stale result for host 0 must not overwrite the pane while host 1 is selected");
        assert!(!app.is_refreshing(Section::Pools, 0), "the in-flight flag must still clear even though the result was discarded");
        assert_eq!(app.status_message, None, "a discarded stale result is not an error");
    }

    #[test]
    fn switching_selected_host_clears_pane_data() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.pools = vec![Pool {
            name: "tank".to_string(),
            size: 100,
            allocated: 10,
            free: 90,
            health: crate::zfs::pool::PoolHealth::Online,
        }];
        app.datasets = vec![crate::zfs::dataset::Dataset {
            name: "tank/data".to_string(),
            kind: crate::zfs::dataset::DatasetKind::Filesystem,
            used: 1,
            available: 2,
            referenced: 1,
            mountpoint: None,
        }];
        app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "snap1".to_string(), used: 1 }];
        app.pools_table.select(Some(0));
        app.datasets_table.select(Some(0));
        app.snapshots_table.select(Some(0));

        app.handle_key(KeyCode::Down); // still on Hosts section by default; switches host

        assert!(app.pools.is_empty());
        assert!(app.datasets.is_empty());
        assert!(app.snapshots.is_empty());
        assert_eq!(app.pools_table.selected(), None);
        assert_eq!(app.datasets_table.selected(), None);
        assert_eq!(app.snapshots_table.selected(), None);
    }

    #[test]
    fn handle_key_navigates_pool_rows_and_wraps() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![
            Pool { name: "a".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
            Pool { name: "b".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
        ];
        app.handle_key(KeyCode::Tab); // Hosts -> Pools
        assert_eq!(app.current_section(), Section::Pools);
        assert_eq!(app.pools_table.selected(), None);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.pools_table.selected(), Some(0));
        app.handle_key(KeyCode::Down);
        assert_eq!(app.pools_table.selected(), Some(1));
        app.handle_key(KeyCode::Down);
        assert_eq!(app.pools_table.selected(), Some(0), "must wrap back to the first row");
        app.handle_key(KeyCode::Up);
        assert_eq!(app.pools_table.selected(), Some(1), "must wrap back to the last row");
    }

    #[test]
    fn handle_key_pool_row_navigation_is_noop_on_empty_list() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.pools_table.selected(), None);
    }

    #[test]
    fn drain_worker_messages_clamps_selected_pool_row_when_list_shrinks() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools_table.select(Some(2));
        app.begin_refresh(Section::Pools, 0);
        app.tx
            .send(WorkerMsg::Pools(0, Ok(vec![Pool {
                name: "only".to_string(),
                size: 0,
                allocated: 0,
                free: 0,
                health: crate::zfs::pool::PoolHealth::Online,
            }])))
            .unwrap();
        app.drain_worker_messages();
        assert_eq!(app.pools_table.selected(), Some(0));
    }

    #[test]
    fn handle_key_navigates_dataset_rows_and_wraps() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![
            Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
            Dataset { name: "b".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
        ];
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // Hosts -> Pools -> Datasets
        assert_eq!(app.current_section(), Section::Datasets);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.datasets_table.selected(), Some(0));
        app.handle_key(KeyCode::Up);
        assert_eq!(app.datasets_table.selected(), Some(1), "must wrap back to the last row");
    }

    #[test]
    fn drain_worker_messages_clamps_selected_dataset_row_when_list_shrinks() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets_table.select(Some(2));
        app.begin_refresh(Section::Datasets, 0);
        app.tx
            .send(WorkerMsg::Datasets(0, Ok(vec![Dataset {
                name: "only".to_string(),
                kind: crate::zfs::dataset::DatasetKind::Filesystem,
                used: 0,
                available: 0,
                referenced: 0,
                mountpoint: None,
            }])))
            .unwrap();
        app.drain_worker_messages();
        assert_eq!(app.datasets_table.selected(), Some(0));
    }

    #[test]
    fn handle_key_navigates_snapshot_rows_and_wraps() {
        let mut app = App::new(vec!["local".to_string()]);
        app.snapshots = vec![
            Snapshot { dataset: "tank/data".to_string(), name: "a".to_string(), used: 0 },
            Snapshot { dataset: "tank/data".to_string(), name: "b".to_string(), used: 0 },
        ];
        app.handle_key(KeyCode::BackTab); // Hosts -> Shares (wraps backward)
        app.handle_key(KeyCode::BackTab); // Shares -> Disks
        app.handle_key(KeyCode::BackTab); // Disks -> Snapshots
        assert_eq!(app.current_section(), Section::Snapshots);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.snapshots_table.selected(), Some(0));
        app.handle_key(KeyCode::Up);
        assert_eq!(app.snapshots_table.selected(), Some(1), "must wrap back to the last row");
    }

    #[test]
    fn drain_worker_messages_clamps_selected_snapshot_row_when_list_shrinks() {
        let mut app = App::new(vec!["local".to_string()]);
        app.snapshots_table.select(Some(2));
        app.begin_refresh(Section::Snapshots, 0);
        app.tx
            .send(WorkerMsg::Snapshots(0, Ok(vec![Snapshot {
                dataset: "tank/data".to_string(),
                name: "only".to_string(),
                used: 0,
            }])))
            .unwrap();
        app.drain_worker_messages();
        assert_eq!(app.snapshots_table.selected(), Some(0));
    }

    #[test]
    fn refresh_pools_does_not_spawn_second_thread_while_in_flight() {
        let mut app = App::new(vec!["local".to_string()]);
        let fake = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
        let runner: Arc<dyn CommandRunner> = fake.clone();

        app.refresh_pools(runner.clone());
        assert!(app.is_refreshing(Section::Pools, 0));
        // A second refresh while the first is still in flight must be a
        // no-op — otherwise holding 'r' spawns unbounded threads/processes.
        app.refresh_pools(runner.clone());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.pools.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(
            fake.calls.lock().unwrap().len(),
            1,
            "second refresh_pools call while one is in flight must not spawn another thread"
        );
    }

    #[test]
    fn enter_on_selected_pool_drills_into_vdevs() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab); // Hosts -> Pools
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.drilled_pool, Some("tank".to_string()));
    }

    #[test]
    fn enter_with_no_row_selected_does_not_drill_in() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.drilled_pool, None);
    }

    #[test]
    fn esc_backs_out_of_vdev_drill_down() {
        let mut app = App::new(vec!["local".to_string()]);
        app.drilled_pool = Some("tank".to_string());
        app.vdevs = vec![VdevLine { depth: 0, name: "tank".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
        app.vdevs_table.select(Some(0));
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.drilled_pool, None);
        assert!(app.vdevs.is_empty());
        assert_eq!(app.vdevs_table.selected(), None);
    }

    #[test]
    fn esc_with_no_drill_down_active_clears_status_message_as_before() {
        let mut app = App::new(vec!["local".to_string()]);
        app.status_message = Some("stale".to_string());
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.status_message, None);
    }

    #[test]
    fn up_down_while_drilled_in_navigate_vdev_rows_not_pool_rows() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.drilled_pool = Some("tank".to_string());
        app.vdevs = vec![
            VdevLine { depth: 0, name: "tank".to_string(), health: crate::zfs::pool::PoolHealth::Online },
            VdevLine { depth: 1, name: "da0".to_string(), health: crate::zfs::pool::PoolHealth::Online },
        ];
        app.handle_key(KeyCode::Down);
        assert_eq!(app.vdevs_table.selected(), Some(0));
        assert_eq!(app.pools_table.selected(), Some(0), "pool row selection must be untouched while drilled in");
    }

    #[test]
    fn refresh_vdevs_populates_vdevs_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        // In the real event loop, `refresh_vdevs` is only ever called while
        // drilled into the pool being refreshed — the stale-pool check in
        // `drain_worker_messages` requires this to accept the result.
        app.drilled_pool = Some("tank".to_string());
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(
            "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME  STATE     READ WRITE CKSUM\n\ttank  ONLINE       0     0     0\n\nerrors: No known data errors\n",
        ));
        app.refresh_vdevs(runner, "tank".to_string());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.vdevs.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.vdevs.len(), 1);
        assert_eq!(app.vdevs[0].name, "tank");
    }

    #[test]
    fn refresh_vdevs_populates_pool_scan_status_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        app.drilled_pool = Some("tpool".to_string());
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(
            "  pool: tpool\n state: ONLINE\n  scan: resilver in progress since Sat Sep 26 12:09:06 2026\nconfig:\n\n\tNAME   STATE     READ WRITE CKSUM\n\ttpool  ONLINE       0     0     0\n\nerrors: No known data errors\n",
        ));
        app.refresh_vdevs(runner, "tpool".to_string());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if app.pool_scan_status.is_some() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            app.pool_scan_status,
            Some(crate::zfs::pool::ScanStatus::InProgress {
                summary: "resilver in progress since Sat Sep 26 12:09:06 2026".to_string(),
                percent_done: None,
                eta: None,
            })
        );
    }

    #[test]
    fn pool_scan_status_is_cleared_on_backing_out_of_drilldown() {
        let mut app = App::new(vec!["local".to_string()]);
        app.drilled_pool = Some("tank".to_string());
        app.pool_scan_status = Some(crate::zfs::pool::ScanStatus::InProgress { summary: "resilver in progress".to_string(), percent_done: None, eta: None });
        app.handle_key(KeyCode::Esc);
        assert!(app.pool_scan_status.is_none());
    }

    #[test]
    fn pool_scan_status_is_cleared_on_drilling_into_a_pool() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab); // -> Pools
        app.pools_table.select(Some(0));
        app.pool_scan_status = Some(crate::zfs::pool::ScanStatus::Completed { summary: "stale from a previous drilldown".to_string(), errors: false });
        app.handle_key(KeyCode::Enter);
        assert!(app.pool_scan_status.is_none());
    }

    #[test]
    fn clear_stale_scan_status_clears_completed_status_after_the_display_duration() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pool_scan_status = Some(crate::zfs::pool::ScanStatus::Completed { summary: "resilvered 501M in 00:00:32 with 0 errors".to_string(), errors: false });
        app.pool_scan_status_shown_at = Some(Instant::now() - Duration::from_secs(7));
        app.clear_stale_scan_status();
        assert!(app.pool_scan_status.is_none());
    }

    #[test]
    fn clear_stale_scan_status_leaves_a_recently_shown_completed_status_alone() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pool_scan_status = Some(crate::zfs::pool::ScanStatus::Completed { summary: "resilvered 501M in 00:00:32 with 0 errors".to_string(), errors: false });
        app.pool_scan_status_shown_at = Some(Instant::now());
        app.clear_stale_scan_status();
        assert!(app.pool_scan_status.is_some(), "must not clear before it's been shown for the full display duration");
    }

    #[test]
    fn clear_stale_scan_status_never_clears_an_in_progress_status() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pool_scan_status = Some(crate::zfs::pool::ScanStatus::InProgress { summary: "resilver in progress".to_string(), percent_done: Some(50.0), eta: None });
        app.pool_scan_status_shown_at = Some(Instant::now() - Duration::from_secs(999));
        app.clear_stale_scan_status();
        assert!(app.pool_scan_status.is_some(), "an in-progress resilver/scrub must stay visible regardless of how long it's been running");
    }

    #[test]
    fn vdevs_result_for_a_different_pool_than_currently_drilled_is_discarded() {
        // Simulates: the user drilled into "tank" (slow refresh in flight),
        // backed out, drilled into "scratch" instead — "tank"'s result then
        // arrives late and must not be shown under "scratch"'s title.
        let mut app = App::new(vec!["local".to_string()]);
        app.drilled_pool = Some("scratch".to_string());
        app.status_message = Some("stale but unrelated".to_string());
        app.vdevs = vec![VdevLine { depth: 0, name: "scratch".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
        app.vdevs_table.select(Some(0));

        app.tx
            .send(WorkerMsg::Vdevs(
                0,
                "tank".to_string(),
                Ok((vec![VdevLine { depth: 0, name: "tank".to_string(), health: crate::zfs::pool::PoolHealth::Online }], None)),
            ))
            .unwrap();
        app.drain_worker_messages();

        assert_eq!(app.vdevs.len(), 1);
        assert_eq!(app.vdevs[0].name, "scratch", "a vdev result for a pool other than the currently drilled-into one must not overwrite the pane");
        assert_eq!(
            app.status_message,
            Some("stale but unrelated".to_string()),
            "a discarded stale-pool result must not clear an unrelated status message"
        );
    }

    #[test]
    fn refresh_pool_properties_populates_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.pools_table.select(Some(0));
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank\tautoexpand\toff\tdefault\n"));
        app.refresh_pool_properties(runner, "tank".to_string());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.pool_properties.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.pool_properties.len(), 1);
        assert_eq!(app.pool_properties[0].name, "autoexpand");
    }

    #[test]
    fn pool_properties_result_for_a_different_pool_than_selected_is_discarded() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "scratch".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.pools_table.select(Some(0)); // selected pool is "scratch"
        app.tx
            .send(WorkerMsg::PoolProperties(0, "tank".to_string(), Ok(vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }])))
            .unwrap();
        app.drain_worker_messages();
        assert!(app.pool_properties.is_empty(), "a result for a pool that's no longer selected must not overwrite the pane");
        assert_eq!(app.status_message, None);
    }

    #[test]
    fn refresh_dataset_properties_populates_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank/data\tcompression\tlz4\tlocal\n"));
        app.refresh_dataset_properties(runner, "tank/data".to_string());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.dataset_properties.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.dataset_properties.len(), 1);
        assert_eq!(app.dataset_properties[0].name, "compression");
    }

    #[test]
    fn dataset_properties_result_for_a_different_dataset_than_selected_is_discarded() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/other".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.tx
            .send(WorkerMsg::DatasetProperties(0, "tank/data".to_string(), Ok(vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }])))
            .unwrap();
        app.drain_worker_messages();
        assert!(app.dataset_properties.is_empty());
    }

    #[test]
    fn refresh_pool_properties_does_not_spawn_second_thread_while_in_flight() {
        let mut app = App::new(vec!["local".to_string()]);
        let fake = Arc::new(FakeRunner::new("tank\tautoexpand\toff\tdefault\n"));
        let runner: Arc<dyn CommandRunner> = fake.clone();
        app.refresh_pool_properties(runner.clone(), "tank".to_string());
        app.refresh_pool_properties(runner.clone(), "tank".to_string());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.pool_properties.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fake.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn refresh_request_while_in_flight_is_queued_and_redispatched_on_completion() {
        // Simulates: the user presses 'r' while a refresh for the same
        // (section, host) is already in flight (e.g. a slow SSH call, or a
        // post-mutation auto-refresh landing mid-flight). The second
        // request must not be silently dropped — it must run once the
        // first one completes.
        let mut app = App::new(vec!["local".to_string()]);
        let fake = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
        let runner: Arc<dyn CommandRunner> = fake.clone();

        app.refresh_pools(runner.clone());
        assert!(app.is_refreshing(Section::Pools, 0));

        // Second request while the first is still in flight.
        app.refresh_pools(runner.clone());
        assert!(
            app.pending_refresh.contains(&(Section::Pools, 0)),
            "the second request must be remembered rather than dropped"
        );

        // Let the first refresh complete and drain its result.
        let deadline = Instant::now() + Duration::from_secs(2);
        let refreshes = loop {
            let r = app.drain_worker_messages();
            if !r.is_empty() || Instant::now() > deadline {
                break r;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(refreshes, vec![Section::Pools], "the queued request must surface for the event loop to redispatch");
        assert!(
            !app.pending_refresh.contains(&(Section::Pools, 0)),
            "the pending flag must be cleared once it has been surfaced for redispatch"
        );

        // Simulate the event loop redispatching the queued request.
        app.refresh_pools(runner.clone());
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if fake.calls.lock().unwrap().len() >= 2 || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fake.calls.lock().unwrap().len(), 2, "the redispatched refresh must actually run against the runner");
    }

    #[test]
    fn refresh_pools_for_one_host_does_not_block_refresh_for_another_host() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        let fake_a = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
        let runner_a: Arc<dyn CommandRunner> = fake_a.clone();

        // Host 0's refresh is "in flight" (never drained in this test, so it
        // stays in flight for the duration).
        app.refresh_pools(runner_a.clone());
        assert!(app.is_refreshing(Section::Pools, 0));

        // Switching to host 1 and refreshing must not be blocked by host 0's
        // still-in-flight refresh.
        app.selected_host = 1;
        let fake_b = Arc::new(FakeRunner::new("scratch\t50\t5\t45\tONLINE\n"));
        let runner_b: Arc<dyn CommandRunner> = fake_b.clone();
        app.refresh_pools(runner_b.clone());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.pools.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.pools.len(), 1);
        assert_eq!(app.pools[0].name, "scratch", "host 1's refresh must have run, not been blocked by host 0's in-flight one");
    }

    #[test]
    fn handle_key_n_on_pools_opens_create_pool_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // Hosts -> Pools
        app.handle_key(KeyCode::Char('n'));
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { fields, focused, .. }) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(*focused, 0);
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_n_on_datasets_opens_create_dataset_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // Hosts -> Pools -> Datasets
        app.handle_key(KeyCode::Char('n'));
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 1);
                assert_eq!(*kind, crate::ui::modal::FormKind::CreateDataset { host: 0 });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_modal_key_create_dataset_form_submits() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char('n'));
        for c in "tank/newds".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(
            result,
            Some(crate::ui::modal::PendingAction {
                host: 0,
                section: Section::Datasets,
                call: crate::ui::modal::MutationCall::CreateDataset { name: "tank/newds".to_string() },
            })
        );
    }

    #[test]
    fn handle_key_d_on_datasets_opens_destroy_confirm_typed_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![crate::zfs::dataset::Dataset {
            name: "tank/olds".to_string(),
            kind: crate::zfs::dataset::DatasetKind::Filesystem,
            used: 0,
            available: 0,
            referenced: 0,
            mountpoint: None,
        }];
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Char('d'));
        match app.modal {
            Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
                assert_eq!(expected, "tank/olds");
                assert_eq!(action.call, crate::ui::modal::MutationCall::DestroyDataset { name: "tank/olds".to_string() });
            }
            other => panic!("expected ConfirmTyped modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_s_on_datasets_opens_create_snapshot_form_with_implied_dataset() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![crate::zfs::dataset::Dataset {
            name: "tank/data".to_string(),
            kind: crate::zfs::dataset::DatasetKind::Filesystem,
            used: 0,
            available: 0,
            referenced: 0,
            mountpoint: None,
        }];
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Char('s'));
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 1);
                assert_eq!(*kind, crate::ui::modal::FormKind::CreateSnapshot { host: 0, dataset: "tank/data".to_string() });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_modal_key_create_snapshot_form_submits_with_implied_dataset() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![crate::zfs::dataset::Dataset {
            name: "tank/data".to_string(),
            kind: crate::zfs::dataset::DatasetKind::Filesystem,
            used: 0,
            available: 0,
            referenced: 0,
            mountpoint: None,
        }];
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Char('s'));
        for c in "manual-1".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(
            result,
            Some(crate::ui::modal::PendingAction {
                host: 0,
                section: Section::Snapshots,
                call: crate::ui::modal::MutationCall::CreateSnapshot {
                    dataset: "tank/data".to_string(),
                    name: "manual-1".to_string(),
                },
            })
        );
    }

    #[test]
    fn handle_key_d_on_snapshots_opens_destroy_confirm_typed_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "old-1".to_string(), used: 0 }];
        app.handle_key(KeyCode::BackTab); // Hosts -> Shares (wraps backward)
        app.handle_key(KeyCode::BackTab); // Shares -> Disks
        app.handle_key(KeyCode::BackTab); // Disks -> Snapshots
        app.snapshots_table.select(Some(0));
        app.handle_key(KeyCode::Char('d'));
        match app.modal {
            Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
                assert_eq!(expected, "tank/data@old-1");
                assert_eq!(
                    action.call,
                    crate::ui::modal::MutationCall::DestroySnapshot { dataset: "tank/data".to_string(), name: "old-1".to_string() }
                );
            }
            other => panic!("expected ConfirmTyped modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_b_on_snapshots_opens_rollback_confirm_typed_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string(), used: 0 }];
        app.handle_key(KeyCode::BackTab); // Hosts -> Shares (wraps backward)
        app.handle_key(KeyCode::BackTab); // Shares -> Disks
        app.handle_key(KeyCode::BackTab); // Disks -> Snapshots
        app.snapshots_table.select(Some(0));
        app.handle_key(KeyCode::Char('b'));
        match app.modal {
            Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
                assert_eq!(expected, "tank/data@2026-09-01");
                assert_eq!(
                    action.call,
                    crate::ui::modal::MutationCall::RollbackSnapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string() }
                );
            }
            other => panic!("expected ConfirmTyped modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_modal_key_form_char_edits_focused_field() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char('n'));
        app.handle_modal_key(KeyCode::Char('t'));
        app.handle_modal_key(KeyCode::Char('a'));
        app.handle_modal_key(KeyCode::Char('n'));
        app.handle_modal_key(KeyCode::Char('k'));
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { fields, .. }) => assert_eq!(fields[0].value, "tank"),
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_modal_key_form_tab_moves_focus_and_wraps() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char('n'));
        app.handle_modal_key(KeyCode::Tab);
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 1),
            other => panic!("expected Form modal, got {other:?}"),
        }
        app.handle_modal_key(KeyCode::Tab);
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 0, "must wrap back to the first field"),
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_modal_key_form_enter_on_non_last_field_advances_focus() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char('n'));
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(result, None);
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 1),
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_modal_key_form_enter_on_last_field_with_empty_required_value_keeps_form_open() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char('n'));
        app.handle_modal_key(KeyCode::Char('t')); // name = "t"
        app.handle_modal_key(KeyCode::Tab); // move to vdev spec field, leave it empty
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(result, None, "an empty required field must block submission");
        assert!(app.modal.is_some(), "the form must stay open so the user can fill in the missing field");
    }

    #[test]
    fn handle_modal_key_form_enter_on_last_field_with_valid_values_submits() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char('n'));
        for c in "tank".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        for c in "mirror da0 da1".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(
            result,
            Some(crate::ui::modal::PendingAction {
                host: 0,
                section: Section::Pools,
                call: crate::ui::modal::MutationCall::CreatePool {
                    name: "tank".to_string(),
                    vdev_spec: vec!["mirror".to_string(), "da0".to_string(), "da1".to_string()],
                },
            })
        );
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_modal_key_form_esc_cancels() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char('n'));
        let result = app.handle_modal_key(KeyCode::Esc);
        assert_eq!(result, None);
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_modal_key_form_backspace_edits_focused_field() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char('n'));
        app.handle_modal_key(KeyCode::Char('t'));
        app.handle_modal_key(KeyCode::Char('x'));
        app.handle_modal_key(KeyCode::Backspace);
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { fields, .. }) => assert_eq!(fields[0].value, "t"),
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_o_on_leaf_disk_opens_offline_confirm_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.drilled_pool = Some("tank".to_string());
        app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
        app.vdevs_table.select(Some(0));
        app.handle_key(KeyCode::Char('o'));
        match &app.modal {
            Some(crate::ui::modal::Modal::Confirm { action, .. }) => {
                assert_eq!(action.section, Section::Vdevs);
                assert_eq!(action.call, crate::ui::modal::MutationCall::OfflineDisk { pool: "tank".to_string(), disk: "da1".to_string() });
            }
            other => panic!("expected Confirm modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_o_on_group_vdev_row_does_not_open_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.drilled_pool = Some("tank".to_string());
        app.vdevs = vec![VdevLine { depth: 1, name: "mirror-0".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
        app.vdevs_table.select(Some(0));
        app.handle_key(KeyCode::Char('o'));
        assert!(app.modal.is_none());
        assert!(app.status_message.is_some(), "selecting a group row should hint that a disk must be selected instead");
    }

    #[test]
    fn handle_key_shift_o_on_leaf_disk_opens_online_confirm_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.drilled_pool = Some("tank".to_string());
        app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Offline }];
        app.vdevs_table.select(Some(0));
        app.handle_key(KeyCode::Char('O'));
        match &app.modal {
            Some(crate::ui::modal::Modal::Confirm { action, .. }) => {
                assert_eq!(action.call, crate::ui::modal::MutationCall::OnlineDisk { pool: "tank".to_string(), disk: "da1".to_string() });
            }
            other => panic!("expected Confirm modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_x_on_leaf_disk_opens_replace_disk_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.drilled_pool = Some("tank".to_string());
        app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Faulted }];
        app.vdevs_table.select(Some(0));
        app.handle_key(KeyCode::Char('x'));
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 1);
                assert_eq!(
                    *kind,
                    crate::ui::modal::FormKind::ReplaceDisk { host: 0, pool: "tank".to_string(), old_disk: "da1".to_string() }
                );
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_a_in_vdev_drilldown_opens_add_vdev_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.drilled_pool = Some("tank".to_string());
        app.vdevs = vec![VdevLine { depth: 1, name: "mirror-0".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
        app.vdevs_table.select(Some(0));
        app.handle_key(KeyCode::Char('a'));
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 1);
                assert_eq!(*kind, crate::ui::modal::FormKind::AddVdev { host: 0, pool: "tank".to_string() });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_modal_key_add_vdev_form_submits_with_correct_pool_and_vdev_spec() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.drilled_pool = Some("tank".to_string());
        app.vdevs = vec![VdevLine { depth: 1, name: "mirror-0".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
        app.vdevs_table.select(Some(0));
        app.handle_key(KeyCode::Char('a'));
        for c in "mirror da2 da3".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(
            result,
            Some(crate::ui::modal::PendingAction {
                host: 0,
                section: Section::Vdevs,
                call: crate::ui::modal::MutationCall::AddVdev {
                    pool: "tank".to_string(),
                    vdev_spec: vec!["mirror".to_string(), "da2".to_string(), "da3".to_string()],
                },
            })
        );
    }

    #[test]
    fn handle_key_x_on_group_vdev_row_does_not_open_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.drilled_pool = Some("tank".to_string());
        app.vdevs = vec![VdevLine { depth: 1, name: "raidz1-0".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
        app.vdevs_table.select(Some(0));
        app.handle_key(KeyCode::Char('x'));
        assert!(app.modal.is_none());
        assert!(app.status_message.is_some());
    }

    #[test]
    fn handle_modal_key_replace_disk_form_submits_with_correct_pool_and_disks() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.drilled_pool = Some("tank".to_string());
        app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Faulted }];
        app.vdevs_table.select(Some(0));
        app.handle_key(KeyCode::Char('x'));
        for c in "da2".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(
            result,
            Some(crate::ui::modal::PendingAction {
                host: 0,
                section: Section::Vdevs,
                call: crate::ui::modal::MutationCall::ReplaceDisk {
                    pool: "tank".to_string(),
                    old_disk: "da1".to_string(),
                    new_disk: "da2".to_string(),
                },
            })
        );
    }

    #[test]
    fn dispatch_mutation_on_vdev_section_queues_both_vdevs_and_pools_refresh() {
        let mut app = App::new(vec!["local".to_string()]);
        let action = crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Vdevs,
            call: crate::ui::modal::MutationCall::OfflineDisk { pool: "tank".to_string(), disk: "da1".to_string() },
        };
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(""));
        app.dispatch_mutation(action, runner);

        let deadline = Instant::now() + Duration::from_secs(2);
        let refreshes = loop {
            let r = app.drain_worker_messages();
            if !r.is_empty() || Instant::now() > deadline {
                break r;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(refreshes, vec![Section::Vdevs, Section::Pools, Section::Disks]);
    }

    #[test]
    fn right_arrow_on_pools_enters_right_focus_and_clears_properties() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // Hosts -> Pools
        app.pool_properties = vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }];
        app.pool_properties_table.select(Some(0));
        assert_eq!(app.pane_focus, PaneFocus::Middle);
        app.handle_key(KeyCode::Right);
        assert_eq!(app.pane_focus, PaneFocus::Right);
        assert!(app.pool_properties.is_empty(), "entering right focus clears stale properties until Task 5's fetch repopulates them");
        assert_eq!(app.pool_properties_table.selected(), None);
    }

    #[test]
    fn right_arrow_on_hosts_is_a_noop() {
        let mut app = App::new(vec!["local".to_string()]);
        assert_eq!(app.current_section(), Section::Hosts);
        app.handle_key(KeyCode::Right);
        assert_eq!(app.pane_focus, PaneFocus::Middle);
    }

    #[test]
    fn left_arrow_returns_focus_to_middle() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Right);
        assert_eq!(app.pane_focus, PaneFocus::Right);
        app.handle_key(KeyCode::Left);
        assert_eq!(app.pane_focus, PaneFocus::Middle);
    }

    #[test]
    fn esc_returns_focus_to_middle_when_right_focused_without_clearing_status_message() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Right);
        app.status_message = Some("keep me".to_string());
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.pane_focus, PaneFocus::Middle);
        assert_eq!(app.status_message, Some("keep me".to_string()), "backing out of right-focus takes priority over the generic Esc-clears-status-message arm, same precedence as the vdev drill-down");
    }

    #[test]
    fn esc_still_clears_status_message_when_middle_focused() {
        let mut app = App::new(vec!["local".to_string()]);
        app.status_message = Some("stale".to_string());
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.status_message, None);
    }

    #[test]
    fn tab_cycles_sections_and_resets_pane_focus() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Right);
        assert_eq!(app.pane_focus, PaneFocus::Right);
        app.handle_key(KeyCode::Tab); // -> Datasets
        assert_eq!(app.current_section(), Section::Datasets);
        assert_eq!(app.pane_focus, PaneFocus::Middle, "switching sections resets focus back to the middle pane");
    }

    #[test]
    fn backtab_cycles_sections_backward() {
        let mut app = App::new(vec!["local".to_string()]);
        assert_eq!(app.current_section(), Section::Hosts);
        app.handle_key(KeyCode::BackTab);
        assert_eq!(app.current_section(), Section::Shares, "must wrap backward from the first section to the last");
    }

    #[test]
    fn up_down_on_pools_middle_focus_clears_properties_on_row_change() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.pools = vec![
            Pool { name: "a".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
            Pool { name: "b".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
        ];
        app.pool_properties = vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }];
        app.pool_properties_table.select(Some(0));
        app.handle_key(KeyCode::Down);
        assert_eq!(app.pools_table.selected(), Some(0));
        assert!(app.pool_properties.is_empty(), "moving to a different pool row must not leave the previous row's properties visible");
        assert_eq!(app.pool_properties_table.selected(), None);
    }

    #[test]
    fn up_down_on_pools_right_focus_moves_properties_table_not_pools_table() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.pools = vec![Pool { name: "a".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.pool_properties = vec![
            Property { name: "p1".to_string(), value: "v1".to_string(), source: PropertySource::Local },
            Property { name: "p2".to_string(), value: "v2".to_string(), source: PropertySource::Local },
        ];
        app.handle_key(KeyCode::Down);
        assert_eq!(app.pool_properties_table.selected(), Some(0));
        assert_eq!(app.pools_table.selected(), Some(0), "pools_table selection must be untouched while right-focused");
    }

    #[test]
    fn up_down_on_datasets_middle_focus_clears_properties_on_row_change() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets = vec![
            Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
            Dataset { name: "b".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
        ];
        app.dataset_properties = vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }];
        app.dataset_properties_table.select(Some(0));
        app.handle_key(KeyCode::Down);
        assert!(app.dataset_properties.is_empty());
        assert_eq!(app.dataset_properties_table.selected(), None);
    }

    #[test]
    fn up_down_on_datasets_right_focus_moves_properties_table() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets = vec![Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.dataset_properties = vec![Property { name: "p1".to_string(), value: "v1".to_string(), source: PropertySource::Local }];
        app.handle_key(KeyCode::Down);
        assert_eq!(app.dataset_properties_table.selected(), Some(0));
    }

    #[test]
    fn switching_host_resets_pane_focus_and_clears_properties() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Right);
        app.pool_properties = vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }];
        app.disks = vec![crate::zfs::disk::DiskInfo { name: "da0".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available }];
        app.selected_disks.insert("da0".to_string());
        app.handle_key(KeyCode::Tab); // -> Datasets, resets pane_focus to Middle first
        app.handle_key(KeyCode::BackTab); // -> Pools again
        app.handle_key(KeyCode::BackTab); // -> Hosts
        app.handle_key(KeyCode::Down); // switch host, triggers clear_host_data
        assert_eq!(app.pane_focus, PaneFocus::Middle);
        assert!(app.pool_properties.is_empty());
        assert_eq!(app.pool_properties_table.selected(), None);
        assert!(app.dataset_properties.is_empty());
        assert_eq!(app.dataset_properties_table.selected(), None);
        assert!(app.disks.is_empty());
        assert_eq!(app.disks_table.selected(), None);
        assert!(app.selected_disks.is_empty());
    }

    #[test]
    fn refresh_disks_populates_disks_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        let runner: Arc<dyn CommandRunner> = Arc::new(
            FakeRunner::new("")
                .route("geom", "disk", "Geom name: da0\nProviders:\n1. Name: da0\n   Mediasize: 268435456 (256M)\n")
                .route("zpool", "list", "tank\t0\t0\t0\tONLINE\n")
                .route(
                    "zpool",
                    "status",
                    "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME  STATE     READ WRITE CKSUM\n\ttank  ONLINE       0     0     0\n\t  da0 ONLINE       0     0     0\n\nerrors: No known data errors\n",
                ),
        );
        app.refresh_disks(runner);

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.disks.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.disks.len(), 1);
        assert_eq!(
            app.disks[0].membership,
            crate::zfs::disk::DiskMembership::InPool { pool: "tank".to_string(), vdev: "da0".to_string() }
        );
    }

    #[test]
    fn refresh_disks_does_not_spawn_second_thread_while_in_flight() {
        let mut app = App::new(vec!["local".to_string()]);
        let fake = Arc::new(
            FakeRunner::new("")
                .route("geom", "disk", "Geom name: da0\nProviders:\n1. Name: da0\n   Mediasize: 268435456 (256M)\n")
                .route("zpool", "list", "")
                .route("zpool", "status", ""),
        );
        let runner: Arc<dyn CommandRunner> = fake.clone();
        app.refresh_disks(runner.clone());
        app.refresh_disks(runner.clone());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.disks.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fake.calls.lock().unwrap().len(), 2, "geom disk list + zpool list; zpool status is skipped since the empty zpool list fixture has no pools");
    }

    #[test]
    fn disks_result_for_a_stale_host_is_discarded() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.tx
            .send(WorkerMsg::Disks(1, Ok(vec![crate::zfs::disk::DiskInfo {
                name: "da0".to_string(),
                size: 0,
                membership: crate::zfs::disk::DiskMembership::Available,
            }])))
            .unwrap();
        app.drain_worker_messages();
        assert!(app.disks.is_empty(), "a result for a host that isn't selected must not populate the pane");
    }

    #[test]
    fn stale_disk_selection_is_dropped_when_refresh_shows_it_claimed_or_gone() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_disks.insert("da0".to_string());
        app.selected_disks.insert("da1".to_string());
        app.tx
            .send(WorkerMsg::Disks(
                0,
                Ok(vec![
                    crate::zfs::disk::DiskInfo {
                        name: "da0".to_string(),
                        size: 0,
                        membership: crate::zfs::disk::DiskMembership::InPool { pool: "tank".to_string(), vdev: "da0".to_string() },
                    },
                    // da1 is missing entirely from this refresh (e.g. unplugged)
                ]),
            ))
            .unwrap();
        app.drain_worker_messages();
        assert!(app.selected_disks.is_empty(), "da0 became claimed and da1 disappeared — both must be dropped from the stale selection");
    }

    #[test]
    fn datasets_list_refresh_that_repoints_selection_clears_stale_permissions() {
        // Same class of bug the Properties pane already guards against
        // (see pools_list_replacement_that_re_points_selected_index_at_a_different_pool_clears_stale_properties):
        // view "tank/old"'s permissions, then a list-changing refresh
        // re-points the selected index at a different dataset. The
        // stale permissions (and their selection) must be cleared
        // rather than silently kept and shown under the new dataset's
        // title — or worse, acted on by 'd'/'x' against the wrong
        // dataset.
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/old".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.dataset_permissions = vec![PermissionRow::Set { name: "admin".to_string(), perms: vec!["snapshot".to_string()] }];
        app.dataset_permissions_table.select(Some(0));
        app.dataset_permissions_for = Some("tank/old".to_string());

        app.tx
            .send(WorkerMsg::Datasets(
                0,
                Ok(vec![Dataset { name: "tank/new".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }]),
            ))
            .unwrap();
        app.drain_worker_messages();

        assert!(app.dataset_permissions.is_empty(), "stale permissions for tank/old must be cleared once the list refresh re-points the selection at tank/new");
        assert_eq!(app.dataset_permissions_for, None);
    }

    #[test]
    fn selected_pool_name_and_selected_dataset_name_helpers() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.pools_table.select(Some(0));
        assert_eq!(app.selected_pool_name(), Some("tank".to_string()));
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        assert_eq!(app.selected_dataset_name(), Some("tank/data".to_string()));
        assert_eq!(App::new(vec!["local".to_string()]).selected_pool_name(), None);
    }

    #[test]
    fn pools_list_replacement_that_re_points_selected_index_at_a_different_pool_clears_stale_properties() {
        // Reviewer's repro: view "tank/b"'s properties (index N), then a
        // list-changing mutation reorders/replaces the list so index N now
        // points at a different pool. The stale properties (and their
        // selection) must be cleared rather than silently kept and shown
        // under the new pool's title.
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![
            Pool { name: "aa".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
            Pool { name: "b".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
        ];
        app.handle_key(KeyCode::Tab); // Hosts -> Pools
        app.pools_table.select(Some(1)); // "b" selected at index 1
        app.handle_key(KeyCode::Right); // enter Right focus
        assert_eq!(app.selected_pool_name(), Some("b".to_string()));

        // Simulate the successful fetch of "b"'s properties landing.
        app.tx
            .send(WorkerMsg::PoolProperties(
                0,
                "b".to_string(),
                Ok(vec![Property { name: "comment".to_string(), value: "b's value".to_string(), source: PropertySource::Local }]),
            ))
            .unwrap();
        app.drain_worker_messages();
        assert_eq!(app.pool_properties.len(), 1);
        app.pool_properties_table.select(Some(0));

        // Now a list refresh lands whose new list re-sorts "aa" before "b" —
        // index 1 (still selected) now points at a pool named "different",
        // not "b" at all.
        app.tx
            .send(WorkerMsg::Pools(
                0,
                Ok(vec![
                    Pool { name: "aaa".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
                    Pool { name: "different".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
                ]),
            ))
            .unwrap();
        app.drain_worker_messages();

        assert_eq!(app.selected_pool_name(), Some("different".to_string()), "index 1 now points at a different pool");
        assert!(
            app.pool_properties.is_empty(),
            "stale properties for the old pool must be cleared once the list no longer matches what they were fetched for"
        );
        assert_eq!(app.pool_properties_table.selected(), None);
    }

    #[test]
    fn datasets_list_replacement_that_re_points_selected_index_at_a_different_dataset_clears_stale_properties() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![
            Dataset { name: "tank/aa".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
            Dataset { name: "tank/b".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
        ];
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets_table.select(Some(1)); // "tank/b"
        app.handle_key(KeyCode::Right);
        assert_eq!(app.selected_dataset_name(), Some("tank/b".to_string()));

        app.tx
            .send(WorkerMsg::DatasetProperties(
                0,
                "tank/b".to_string(),
                Ok(vec![Property { name: "compression".to_string(), value: "lz4".to_string(), source: PropertySource::Local }]),
            ))
            .unwrap();
        app.drain_worker_messages();
        assert_eq!(app.dataset_properties.len(), 1);
        app.dataset_properties_table.select(Some(0));

        app.tx
            .send(WorkerMsg::Datasets(
                0,
                Ok(vec![
                    Dataset { name: "tank/aaa".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
                    Dataset { name: "tank/different".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
                ]),
            ))
            .unwrap();
        app.drain_worker_messages();

        assert_eq!(app.selected_dataset_name(), Some("tank/different".to_string()));
        assert!(
            app.dataset_properties.is_empty(),
            "stale properties for the old dataset must be cleared once the list no longer matches what they were fetched for"
        );
        assert_eq!(app.dataset_properties_table.selected(), None);
    }

    #[test]
    fn pools_list_replacement_that_keeps_same_pool_at_same_index_does_not_clear_properties() {
        // The common, correct case: a refresh lands but the selected pool's
        // identity at the selected index is unchanged — properties must
        // survive.
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab);
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.tx
            .send(WorkerMsg::PoolProperties(
                0,
                "tank".to_string(),
                Ok(vec![Property { name: "comment".to_string(), value: "v".to_string(), source: PropertySource::Local }]),
            ))
            .unwrap();
        app.drain_worker_messages();
        app.pool_properties_table.select(Some(0));

        // Refresh returns the same pool (e.g. updated size) at the same index.
        app.tx
            .send(WorkerMsg::Pools(
                0,
                Ok(vec![Pool { name: "tank".to_string(), size: 999, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }]),
            ))
            .unwrap();
        app.drain_worker_messages();

        assert_eq!(app.pool_properties.len(), 1, "properties must survive a refresh that still points at the same pool");
        assert_eq!(app.pool_properties_table.selected(), Some(0));
    }

    #[test]
    fn enter_on_right_focused_pool_property_opens_prefilled_edit_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.pool_properties = vec![Property { name: "comment".to_string(), value: "old comment".to_string(), source: PropertySource::Local }];
        app.pool_properties_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { fields, kind, description, .. }) => {
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].value, "old comment", "the form must be pre-filled with the property's current value");
                assert_eq!(*kind, crate::ui::modal::FormKind::EditPoolProperty { host: 0, pool: "tank".to_string(), key: "comment".to_string() });
                assert_eq!(description.as_deref(), Some("A free-form text comment attached to this pool"));
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_right_focused_property_with_no_known_description_leaves_it_none() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.pool_properties = vec![Property { name: "some_future_property".to_string(), value: "x".to_string(), source: PropertySource::Local }];
        app.pool_properties_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { description, .. }) => assert_eq!(*description, None),
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_right_focused_read_only_pool_property_does_not_open_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.pool_properties = vec![Property { name: "size".to_string(), value: "4000000000000".to_string(), source: PropertySource::Other("-".to_string()) }];
        app.pool_properties_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        assert!(app.modal.is_none());
        assert_eq!(app.status_message, Some("'size' is a read-only property".to_string()));
    }

    #[test]
    fn enter_on_right_focused_pool_property_with_none_selected_does_not_open_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Enter);
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_modal_key_edit_pool_property_form_submits_with_new_value() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.pool_properties = vec![Property { name: "comment".to_string(), value: "old".to_string(), source: PropertySource::Local }];
        app.pool_properties_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        // clear the pre-filled value, then type a new one
        for _ in 0.."old".len() {
            app.handle_modal_key(KeyCode::Backspace);
        }
        for c in "new comment".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(
            result,
            Some(crate::ui::modal::PendingAction {
                host: 0,
                section: Section::PoolProperties,
                call: crate::ui::modal::MutationCall::SetPoolProperty {
                    pool: "tank".to_string(),
                    key: "comment".to_string(),
                    value: "new comment".to_string(),
                },
            })
        );
    }

    #[test]
    fn enter_on_right_focused_dataset_property_opens_prefilled_edit_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.dataset_properties = vec![Property { name: "compression".to_string(), value: "lz4".to_string(), source: PropertySource::Local }];
        app.dataset_properties_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { fields, kind, description, .. }) => {
                assert_eq!(fields[0].value, "lz4");
                assert_eq!(*kind, crate::ui::modal::FormKind::EditDatasetProperty { host: 0, dataset: "tank/data".to_string(), key: "compression".to_string() });
                assert_eq!(
                    description.as_deref(),
                    Some("Compression algorithm applied to data written to this dataset (e.g. off, lz4, zstd, gzip)")
                );
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_right_focused_read_only_dataset_property_does_not_open_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.dataset_properties = vec![Property { name: "used".to_string(), value: "1234567".to_string(), source: PropertySource::Other("-".to_string()) }];
        app.dataset_properties_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        assert!(app.modal.is_none());
        assert_eq!(app.status_message, Some("'used' is a read-only property".to_string()));
    }

    #[test]
    fn handle_modal_key_edit_dataset_property_form_submits_with_new_value() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Right);
        app.dataset_properties = vec![Property { name: "compression".to_string(), value: "lz4".to_string(), source: PropertySource::Local }];
        app.dataset_properties_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        for _ in 0.."lz4".len() {
            app.handle_modal_key(KeyCode::Backspace);
        }
        for c in "zstd".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(
            result,
            Some(crate::ui::modal::PendingAction {
                host: 0,
                section: Section::DatasetProperties,
                call: crate::ui::modal::MutationCall::SetDatasetProperty {
                    dataset: "tank/data".to_string(),
                    key: "compression".to_string(),
                    value: "zstd".to_string(),
                },
            })
        );
    }

    #[test]
    fn space_toggles_selection_on_available_disk() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.handle_key(KeyCode::Tab); // -> Snapshots
        app.handle_key(KeyCode::Tab); // -> Disks
        app.disks = vec![crate::zfs::disk::DiskInfo { name: "da0".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available }];
        app.disks_table.select(Some(0));
        app.handle_key(KeyCode::Char(' '));
        assert!(app.selected_disks.contains("da0"));
        app.handle_key(KeyCode::Char(' '));
        assert!(!app.selected_disks.contains("da0"), "pressing Space again deselects it");
    }

    #[test]
    fn space_on_claimed_disk_sets_hint_and_does_not_select() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..4 {
            app.handle_key(KeyCode::Tab);
        } // -> Disks
        app.disks = vec![crate::zfs::disk::DiskInfo {
            name: "da0".to_string(),
            size: 0,
            membership: crate::zfs::disk::DiskMembership::InPool { pool: "tank".to_string(), vdev: "mirror-0".to_string() },
        }];
        app.disks_table.select(Some(0));
        app.handle_key(KeyCode::Char(' '));
        assert!(app.selected_disks.is_empty());
        assert_eq!(app.status_message, Some("disk 'da0' already in pool 'tank'".to_string()));
    }

    #[test]
    fn esc_clears_disk_selection_in_disks_section() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..4 {
            app.handle_key(KeyCode::Tab);
        } // -> Disks
        app.selected_disks.insert("da0".to_string());
        app.handle_key(KeyCode::Esc);
        assert!(app.selected_disks.is_empty());
    }

    #[test]
    fn up_down_in_disks_section_moves_disks_table_selection() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..4 {
            app.handle_key(KeyCode::Tab);
        } // -> Disks
        app.disks = vec![
            crate::zfs::disk::DiskInfo { name: "da0".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available },
            crate::zfs::disk::DiskInfo { name: "da1".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available },
        ];
        app.handle_key(KeyCode::Down);
        assert_eq!(app.disks_table.selected(), Some(0));
        app.handle_key(KeyCode::Down);
        assert_eq!(app.disks_table.selected(), Some(1));
    }

    #[test]
    fn handle_key_n_in_disks_with_selection_opens_create_pool_from_disks_form() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..4 {
            app.handle_key(KeyCode::Tab);
        } // -> Disks
        app.selected_disks.insert("da1".to_string());
        app.selected_disks.insert("da0".to_string());
        app.handle_key(KeyCode::Char('n'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(
                    *kind,
                    FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string(), "da1".to_string()] },
                    "disks must be sorted for a deterministic snapshot"
                );
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_n_in_disks_without_selection_sets_hint() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..4 {
            app.handle_key(KeyCode::Tab);
        } // -> Disks
        app.handle_key(KeyCode::Char('n'));
        assert!(app.modal.is_none());
        assert_eq!(app.status_message, Some("select at least one disk with Space first".to_string()));
    }

    #[test]
    fn handle_key_a_in_disks_with_selection_opens_add_vdev_from_disks_form() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..4 {
            app.handle_key(KeyCode::Tab);
        } // -> Disks
        app.selected_disks.insert("da0".to_string());
        app.handle_key(KeyCode::Char('a'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(*kind, FormKind::AddVdevFromDisks { host: 0, disks: vec!["da0".to_string()] });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_a_in_disks_without_selection_sets_hint() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..4 {
            app.handle_key(KeyCode::Tab);
        } // -> Disks
        app.handle_key(KeyCode::Char('a'));
        assert!(app.modal.is_none());
        assert_eq!(app.status_message, Some("select at least one disk with Space first".to_string()));
    }

    #[test]
    fn build_pending_action_create_pool_from_disks_blank_redundancy_stripes_each_disk() {
        let kind = FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string(), "da1".to_string()] };
        let fields = vec![
            FormField { label: "Name".to_string(), value: "tank".to_string() },
            FormField { label: "Redundancy".to_string(), value: String::new() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action,
            PendingAction {
                host: 0,
                section: Section::Pools,
                call: MutationCall::CreatePool { name: "tank".to_string(), vdev_spec: vec!["da0".to_string(), "da1".to_string()] },
            }
        );
    }

    #[test]
    fn build_pending_action_create_pool_from_disks_with_redundancy_groups_all_disks() {
        let kind = FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string(), "da1".to_string()] };
        let fields = vec![
            FormField { label: "Name".to_string(), value: "tank".to_string() },
            FormField { label: "Redundancy".to_string(), value: "mirror".to_string() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action.call,
            MutationCall::CreatePool { name: "tank".to_string(), vdev_spec: vec!["mirror".to_string(), "da0".to_string(), "da1".to_string()] }
        );
    }

    #[test]
    fn build_pending_action_create_pool_from_disks_rejects_empty_name() {
        let kind = FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string()] };
        let fields = vec![
            FormField { label: "Name".to_string(), value: "  ".to_string() },
            FormField { label: "Redundancy".to_string(), value: String::new() },
        ];
        assert_eq!(build_pending_action(&kind, &fields), None);
    }

    #[test]
    fn build_pending_action_add_vdev_from_disks_rejects_empty_pool_name() {
        let kind = FormKind::AddVdevFromDisks { host: 0, disks: vec!["da0".to_string()] };
        let fields = vec![
            FormField { label: "Target pool name".to_string(), value: "   ".to_string() },
            FormField { label: "Redundancy".to_string(), value: String::new() },
        ];
        assert_eq!(build_pending_action(&kind, &fields), None);
    }

    #[test]
    fn build_pending_action_add_vdev_from_disks() {
        let kind = FormKind::AddVdevFromDisks { host: 0, disks: vec!["da2".to_string(), "da3".to_string()] };
        let fields = vec![
            FormField { label: "Target pool name".to_string(), value: "tank".to_string() },
            FormField { label: "Redundancy".to_string(), value: "mirror".to_string() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action,
            PendingAction {
                host: 0,
                section: Section::Vdevs,
                call: MutationCall::AddVdev { pool: "tank".to_string(), vdev_spec: vec!["mirror".to_string(), "da2".to_string(), "da3".to_string()] },
            }
        );
    }

    #[test]
    fn selected_disks_survives_a_failed_create_pool_from_disks_submission() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..4 {
            app.handle_key(KeyCode::Tab);
        } // -> Disks
        app.selected_disks.insert("da0".to_string());
        app.handle_key(KeyCode::Char('n'));
        for c in "tank".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab); // move to the Redundancy field
        let result = app.handle_modal_key(KeyCode::Enter);
        assert!(result.is_some(), "the form still submits and returns a PendingAction");
        assert!(
            app.selected_disks.contains("da0"),
            "the selection must survive the form submit itself — it's only cleared once the mutation actually succeeds"
        );
    }

    #[test]
    fn selected_disks_cleared_once_the_resulting_mutation_succeeds() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_disks.insert("da0".to_string());
        app.tx.send(WorkerMsg::MutationDone(0, Section::Pools, Ok(()))).unwrap();
        app.drain_worker_messages();
        assert!(app.selected_disks.is_empty());
    }

    #[test]
    fn handle_key_shift_r_on_pools_opens_resilver_confirm_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.handle_key(KeyCode::Tab); // -> Pools
        app.pools_table.select(Some(0));
        app.handle_key(KeyCode::Char('R'));
        match &app.modal {
            Some(Modal::Confirm { action, .. }) => {
                assert_eq!(action.call, MutationCall::ResilverPool { pool: "tank".to_string() });
            }
            other => panic!("expected Confirm modal, got {other:?}"),
        }
    }

    #[test]
    fn mutation_done_for_pools_section_also_queues_disks_refresh() {
        let mut app = App::new(vec!["local".to_string()]);
        app.tx.send(WorkerMsg::MutationDone(0, Section::Pools, Ok(()))).unwrap();
        let refreshes = app.drain_worker_messages();
        assert!(refreshes.contains(&Section::Pools));
        assert!(refreshes.contains(&Section::Disks), "a Pools-section mutation (create/destroy pool) changes disk membership too");
    }

    #[test]
    fn refresh_shares_populates_app_state_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        let runner: Arc<dyn CommandRunner> = Arc::new(
            FakeRunner::new("")
                .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\ntank/data\tsharesmb\toff\tdefault\n")
                .route("service", "nfsd", "nfsd is running as pid 1234.\n")
                .route("service", "samba_server", "smbd is not running.\n"),
        );
        app.refresh_shares(runner);

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.shares.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.shares.len(), 1);
        assert_eq!(app.nfs_available, Some(true));
        assert_eq!(app.samba_available, Some(false));
        assert_eq!(app.shares_loaded, true);
    }

    #[test]
    fn shares_result_for_a_stale_host_is_discarded() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.tx
            .send(WorkerMsg::Shares(
                1,
                Ok(crate::zfs::share::SharesSnapshot {
                    shares: vec![crate::zfs::share::Share::Smb { dataset: "tank/data".to_string(), name: None }],
                    nfs_available: true,
                    samba_available: true,
                }),
            ))
            .unwrap();
        app.drain_worker_messages();
        assert!(app.shares.is_empty(), "a result for a host that isn't selected must not populate the pane");
    }

    #[test]
    fn switching_host_clears_shares_state() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/data".to_string(), name: None }];
        app.shares_table.select(Some(0));
        app.nfs_available = Some(true);
        app.samba_available = Some(false);
        app.shares_loaded = true;
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::BackTab); // -> Hosts
        app.handle_key(KeyCode::Down); // switch host, triggers clear_host_data
        assert!(app.shares.is_empty());
        assert_eq!(app.shares_table.selected(), None);
        assert_eq!(app.nfs_available, None);
        assert_eq!(app.samba_available, None);
        assert_eq!(app.shares_loaded, false);
    }

    #[test]
    fn enter_on_nfs_share_row_opens_prefilled_edit_form() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..5 {
            app.handle_key(KeyCode::Tab);
        } // -> Shares
        app.shares = vec![crate::zfs::share::Share::Nfs {
            dataset: "tank/data".to_string(),
            raw: "-maproot=nobody -network 172.16.10.0/24".to_string(),
            networks: vec!["172.16.10.0/24".to_string()],
            hosts: vec![],
            read_only: false,
            root: Some("nobody".to_string()),
        }];
        app.shares_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 4);
                assert_eq!(fields[0].value, "172.16.10.0/24");
                assert_eq!(fields[1].value, "");
                assert_eq!(fields[2].value, "n");
                assert_eq!(fields[3].value, "nobody");
                assert_eq!(*kind, FormKind::ShareNfs { host: 0, dataset: "tank/data".to_string() });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_smb_share_row_sets_hint_instead_of_opening_a_form() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..5 {
            app.handle_key(KeyCode::Tab);
        } // -> Shares
        app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/data".to_string(), name: Some("myshare".to_string()) }];
        app.shares_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        assert!(app.modal.is_none(), "SMB share creation/editing isn't supported on FreeBSD; Enter on an SMB row must not open an edit form");
        assert!(app.status_message.is_some());
    }

    #[test]
    fn handle_key_d_on_nfs_share_row_opens_remove_confirm_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..5 {
            app.handle_key(KeyCode::Tab);
        } // -> Shares
        app.shares = vec![crate::zfs::share::Share::Nfs {
            dataset: "tank/data".to_string(),
            raw: "on".to_string(),
            networks: vec![],
            hosts: vec![],
            read_only: false,
            root: None,
        }];
        app.shares_table.select(Some(0));
        app.handle_key(KeyCode::Char('d'));
        match &app.modal {
            Some(Modal::Confirm { action, .. }) => {
                assert_eq!(
                    action.call,
                    MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharenfs".to_string(), value: "off".to_string() }
                );
            }
            other => panic!("expected Confirm modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_d_on_smb_share_row_opens_remove_confirm_modal() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..5 {
            app.handle_key(KeyCode::Tab);
        } // -> Shares
        app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/data".to_string(), name: None }];
        app.shares_table.select(Some(0));
        app.handle_key(KeyCode::Char('d'));
        match &app.modal {
            Some(Modal::Confirm { action, .. }) => {
                assert_eq!(
                    action.call,
                    MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharesmb".to_string(), value: "off".to_string() }
                );
            }
            other => panic!("expected Confirm modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_shift_n_on_datasets_opens_blank_share_nfs_form_when_not_yet_shared() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.nfs_available = Some(true);
        app.shares_loaded = true;
        app.handle_key(KeyCode::Char('N'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                // read_only renders as "n" (not blank) for a false placeholder,
                // consistent with `share_edit_modal`'s handling of real shares
                // (see `enter_on_nfs_share_row_opens_prefilled_edit_form`) —
                // it still assembles to the same "on" value downstream.
                assert_eq!(fields[0].value, "");
                assert_eq!(fields[1].value, "");
                assert_eq!(fields[2].value, "n");
                assert_eq!(fields[3].value, "");
                assert_eq!(*kind, FormKind::ShareNfs { host: 0, dataset: "tank/data".to_string() });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_shift_n_on_datasets_sets_hint_when_nfs_unavailable() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.nfs_available = Some(false);
        app.shares_loaded = true;
        app.handle_key(KeyCode::Char('N'));
        assert!(app.modal.is_none());
        assert_eq!(app.status_message, Some("NFS server not available on this host".to_string()));
    }

    #[test]
    fn handle_key_shift_n_on_datasets_sets_hint_when_shares_not_yet_loaded() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.nfs_available = Some(true); // availability known, but the share list itself was never refreshed
        app.handle_key(KeyCode::Char('N'));
        assert!(app.modal.is_none(), "must not pre-fill from a self.shares that was never populated — could silently widen an existing restricted share");
        assert!(app.status_message.is_some());
    }

    #[test]
    fn build_pending_action_share_nfs_blank_fields_assembles_to_on() {
        let kind = FormKind::ShareNfs { host: 0, dataset: "tank/data".to_string() };
        let fields = vec![
            FormField { label: "Networks".to_string(), value: String::new() },
            FormField { label: "Hosts".to_string(), value: String::new() },
            FormField { label: "Read-only (y/n)".to_string(), value: String::new() },
            FormField { label: "Root user mapping".to_string(), value: String::new() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action,
            PendingAction {
                host: 0,
                section: Section::Shares,
                call: MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharenfs".to_string(), value: "on".to_string() },
            }
        );
    }

    #[test]
    fn build_pending_action_grant_permission_requires_recipient_and_permissions() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        let blank = vec![
            FormField { label: "Recipient".to_string(), value: String::new() },
            FormField { label: "Scope".to_string(), value: String::new() },
            FormField { label: "Permissions".to_string(), value: String::new() },
        ];
        assert!(build_pending_action(&kind, &blank).is_none(), "blank Permissions must keep the form open");

        let no_recipient = vec![
            FormField { label: "Recipient".to_string(), value: "fb".to_string() },
            FormField { label: "Scope".to_string(), value: String::new() },
            FormField { label: "Permissions".to_string(), value: "snapshot".to_string() },
        ];
        assert!(build_pending_action(&kind, &no_recipient).is_none(), "a bare name with no user:/group: prefix must keep the form open");
    }

    #[test]
    fn build_pending_action_grant_permission_assembles_user_local() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        let fields = vec![
            FormField { label: "Recipient".to_string(), value: "user:fb".to_string() },
            FormField { label: "Scope".to_string(), value: "local".to_string() },
            FormField { label: "Permissions".to_string(), value: "snapshot, create".to_string() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action,
            PendingAction {
                host: 0,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission {
                    dataset: "tank/data".to_string(),
                    target: crate::zfs::permission::PermissionTarget::Grant {
                        recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
                        scope: crate::zfs::permission::Scope::Local,
                    },
                    perms: vec!["snapshot".to_string(), "create".to_string()],
                },
            }
        );
    }

    #[test]
    fn build_pending_action_grant_permission_blank_scope_defaults_to_both() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        let fields = vec![
            FormField { label: "Recipient".to_string(), value: "everyone".to_string() },
            FormField { label: "Scope".to_string(), value: String::new() },
            FormField { label: "Permissions".to_string(), value: "mount".to_string() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action.call,
            MutationCall::AllowPermission {
                dataset: "tank/data".to_string(),
                target: crate::zfs::permission::PermissionTarget::Grant { recipient: crate::zfs::permission::Recipient::Everyone, scope: crate::zfs::permission::Scope::Both },
                perms: vec!["mount".to_string()],
            }
        );
    }

    #[test]
    fn build_pending_action_grant_permission_garbage_scope_keeps_form_open() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        let fields = vec![
            FormField { label: "Recipient".to_string(), value: "everyone".to_string() },
            FormField { label: "Scope".to_string(), value: "lcoal".to_string() },
            FormField { label: "Permissions".to_string(), value: "mount".to_string() },
        ];
        assert!(build_pending_action(&kind, &fields).is_none(), "a scope typo must not silently fall back to Both");
    }

    #[test]
    fn build_pending_action_grant_permission_set_assembles() {
        let kind = FormKind::GrantPermissionSet { host: 0, dataset: "tank/data".to_string() };
        let fields = vec![
            FormField { label: "Set name".to_string(), value: "admin".to_string() },
            FormField { label: "Permissions".to_string(), value: "snapshot,rollback".to_string() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action,
            PendingAction {
                host: 0,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission {
                    dataset: "tank/data".to_string(),
                    target: crate::zfs::permission::PermissionTarget::Set("admin".to_string()),
                    perms: vec!["snapshot".to_string(), "rollback".to_string()],
                },
            }
        );
    }

    #[test]
    fn build_pending_action_grant_create_time_permission_assembles() {
        let kind = FormKind::GrantCreateTimePermission { host: 0, dataset: "tank/data".to_string() };
        let fields = vec![FormField { label: "Permissions".to_string(), value: "destroy".to_string() }];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action,
            PendingAction {
                host: 0,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission {
                    dataset: "tank/data".to_string(),
                    target: crate::zfs::permission::PermissionTarget::CreateTime,
                    perms: vec!["destroy".to_string()],
                },
            }
        );
    }

    #[test]
    fn build_pending_action_revoke_permissions_assembles() {
        let kind = FormKind::RevokePermissions {
            host: 0,
            dataset: "tank/data".to_string(),
            target: crate::zfs::permission::PermissionTarget::Set("admin".to_string()),
        };
        let fields = vec![FormField { label: "Permissions to remove".to_string(), value: "rollback".to_string() }];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action,
            PendingAction {
                host: 0,
                section: Section::DatasetPermissions,
                call: MutationCall::UnallowPermission {
                    dataset: "tank/data".to_string(),
                    target: crate::zfs::permission::PermissionTarget::Set("admin".to_string()),
                    perms: Some(vec!["rollback".to_string()]),
                },
            }
        );
    }

    #[test]
    fn build_pending_action_revoke_permissions_blank_keeps_form_open() {
        let kind = FormKind::RevokePermissions { host: 0, dataset: "tank/data".to_string(), target: crate::zfs::permission::PermissionTarget::CreateTime };
        let fields = vec![FormField { label: "Permissions to remove".to_string(), value: String::new() }];
        assert!(build_pending_action(&kind, &fields).is_none());
    }

    #[test]
    fn build_pending_action_add_acl_entry_requires_recipient_and_permissions() {
        let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 3 };
        let blank = vec![
            FormField { label: "Recipient".to_string(), value: String::new() },
            FormField { label: "Permissions".to_string(), value: String::new() },
            FormField { label: "Flags".to_string(), value: String::new() },
            FormField { label: "Type".to_string(), value: String::new() },
        ];
        assert!(build_pending_action(&kind, &blank).is_none(), "blank Permissions must keep the form open");

        let bad_recipient = vec![
            FormField { label: "Recipient".to_string(), value: "nobody".to_string() },
            FormField { label: "Permissions".to_string(), value: "read_data".to_string() },
            FormField { label: "Flags".to_string(), value: String::new() },
            FormField { label: "Type".to_string(), value: String::new() },
        ];
        assert!(build_pending_action(&kind, &bad_recipient).is_none(), "a bare name with no user:/group: prefix (and not owner/group/everyone) must keep the form open");
    }

    #[test]
    fn build_pending_action_add_acl_entry_assembles_named_user_with_flags_and_deny() {
        let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 3 };
        let fields = vec![
            FormField { label: "Recipient".to_string(), value: "user:nobody".to_string() },
            FormField { label: "Permissions".to_string(), value: "read_data, write_data".to_string() },
            FormField { label: "Flags".to_string(), value: "file_inherit,dir_inherit".to_string() },
            FormField { label: "Type".to_string(), value: "deny".to_string() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action,
            PendingAction {
                host: 0,
                section: Section::DatasetAcl,
                call: MutationCall::AddAclEntry {
                    path: "/tank/data".to_string(),
                    position: 3,
                    entry: crate::acl::AclEntry {
                        tag: crate::acl::AclTag::User("nobody".to_string()),
                        permissions: vec!["read_data".to_string(), "write_data".to_string()],
                        flags: vec!["file_inherit".to_string(), "dir_inherit".to_string()],
                        entry_type: crate::acl::AclEntryType::Deny,
                    },
                },
            }
        );
    }

    #[test]
    fn build_pending_action_add_acl_entry_blank_flags_and_type_default_correctly() {
        let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
        let fields = vec![
            FormField { label: "Recipient".to_string(), value: "everyone".to_string() },
            FormField { label: "Permissions".to_string(), value: "read_data".to_string() },
            FormField { label: "Flags".to_string(), value: String::new() },
            FormField { label: "Type".to_string(), value: String::new() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action.call,
            MutationCall::AddAclEntry {
                path: "/tank/data".to_string(),
                position: 0,
                entry: crate::acl::AclEntry {
                    tag: crate::acl::AclTag::Everyone,
                    permissions: vec!["read_data".to_string()],
                    flags: vec![],
                    entry_type: crate::acl::AclEntryType::Allow,
                },
            }
        );
    }

    #[test]
    fn build_pending_action_add_acl_entry_garbage_type_keeps_form_open() {
        let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
        let fields = vec![
            FormField { label: "Recipient".to_string(), value: "owner".to_string() },
            FormField { label: "Permissions".to_string(), value: "read_data".to_string() },
            FormField { label: "Flags".to_string(), value: String::new() },
            FormField { label: "Type".to_string(), value: "dny".to_string() },
        ];
        assert!(build_pending_action(&kind, &fields).is_none(), "a Type typo must not silently default to allow");
    }

    #[test]
    fn build_pending_action_add_acl_entry_owner_and_group_recipients() {
        let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
        let owner_fields = vec![
            FormField { label: "Recipient".to_string(), value: "owner".to_string() },
            FormField { label: "Permissions".to_string(), value: "read_data".to_string() },
            FormField { label: "Flags".to_string(), value: String::new() },
            FormField { label: "Type".to_string(), value: String::new() },
        ];
        let action = build_pending_action(&kind, &owner_fields).unwrap();
        match action.call {
            MutationCall::AddAclEntry { entry, .. } => assert_eq!(entry.tag, crate::acl::AclTag::Owner),
            other => panic!("expected AddAclEntry, got {other:?}"),
        }

        let group_fields = vec![
            FormField { label: "Recipient".to_string(), value: "group:admins".to_string() },
            FormField { label: "Permissions".to_string(), value: "read_data".to_string() },
            FormField { label: "Flags".to_string(), value: String::new() },
            FormField { label: "Type".to_string(), value: String::new() },
        ];
        let action = build_pending_action(&kind, &group_fields).unwrap();
        match action.call {
            MutationCall::AddAclEntry { entry, .. } => assert_eq!(entry.tag, crate::acl::AclTag::NamedGroup("admins".to_string())),
            other => panic!("expected AddAclEntry, got {other:?}"),
        }
    }

    fn enter_datasets_permissions_tab(app: &mut App) {
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.handle_key(KeyCode::Right); // -> Right focus, Properties tab
        app.handle_key(KeyCode::Char(']')); // -> Permissions tab
    }

    #[test]
    fn handle_key_n_on_permissions_tab_with_no_dataset_selected_does_not_panic_or_open_form() {
        let mut app = App::new(vec!["local".to_string()]);
        enter_datasets_permissions_tab(&mut app);
        app.handle_key(KeyCode::Char('n'));
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_key_n_on_permissions_tab_opens_grant_permission_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.handle_key(KeyCode::Char('n'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 3);
                assert_eq!(*kind, FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_s_on_permissions_tab_opens_grant_permission_set_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.handle_key(KeyCode::Char('s'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(*kind, FormKind::GrantPermissionSet { host: 0, dataset: "tank/data".to_string() });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_c_on_permissions_tab_opens_create_time_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.handle_key(KeyCode::Char('c'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 1);
                assert_eq!(*kind, FormKind::GrantCreateTimePermission { host: 0, dataset: "tank/data".to_string() });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_d_and_x_on_permissions_tab_with_no_row_selected_do_not_panic() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.handle_key(KeyCode::Char('d'));
        assert!(app.modal.is_none());
        app.handle_key(KeyCode::Char('x'));
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_key_d_on_selected_permission_row_opens_full_revoke_confirm() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.dataset_permissions = vec![PermissionRow::Set { name: "admin".to_string(), perms: vec!["snapshot".to_string()] }];
        app.dataset_permissions_table.select(Some(0));
        app.handle_key(KeyCode::Char('d'));
        match &app.modal {
            Some(Modal::Confirm { action, .. }) => {
                assert_eq!(
                    action.call,
                    MutationCall::UnallowPermission { dataset: "tank/data".to_string(), target: crate::zfs::permission::PermissionTarget::Set("admin".to_string()), perms: None }
                );
            }
            other => panic!("expected Confirm modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_d_on_selected_grant_row_revokes_only_that_rows_own_permissions() {
        // Reviewer's repro: fb has "snapshot" as Local+Descendent and
        // "mount" as Local only, so two separate rows show. Revoking the
        // "Local / mount" row must not also delete fb's Local half of
        // "snapshot" -- it must only ever touch exactly the permissions
        // that row displays, not everything for that recipient+scope.
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.dataset_permissions = vec![PermissionRow::Grant {
            recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
            scope: crate::zfs::permission::Scope::Local,
            perms: vec!["mount".to_string()],
        }];
        app.dataset_permissions_table.select(Some(0));
        app.handle_key(KeyCode::Char('d'));
        match &app.modal {
            Some(Modal::Confirm { action, .. }) => {
                assert_eq!(
                    action.call,
                    MutationCall::UnallowPermission {
                        dataset: "tank/data".to_string(),
                        target: crate::zfs::permission::PermissionTarget::Grant {
                            recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
                            scope: crate::zfs::permission::Scope::Local,
                        },
                        perms: Some(vec!["mount".to_string()]),
                    },
                    "must revoke only this row's own permissions, not perms: None (which would also wipe the Local half of any other grant for the same recipient)"
                );
            }
            other => panic!("expected Confirm modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_x_on_selected_permission_row_opens_revoke_specific_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.dataset_permissions = vec![PermissionRow::Grant {
            recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
            scope: crate::zfs::permission::Scope::Both,
            perms: vec!["snapshot".to_string(), "mount".to_string()],
        }];
        app.dataset_permissions_table.select(Some(0));
        app.handle_key(KeyCode::Char('x'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 1);
                assert_eq!(
                    *kind,
                    FormKind::RevokePermissions {
                        host: 0,
                        dataset: "tank/data".to_string(),
                        target: crate::zfs::permission::PermissionTarget::Grant { recipient: crate::zfs::permission::Recipient::User("fb".to_string()), scope: crate::zfs::permission::Scope::Both },
                    }
                );
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    fn enter_datasets_acl_tab(app: &mut App) {
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.handle_key(KeyCode::Right); // -> Right focus, Properties tab
        app.handle_key(KeyCode::Char(']')); // -> Permissions tab
        app.handle_key(KeyCode::Char(']')); // -> Acl tab
    }

    #[test]
    fn handle_key_n_on_acl_tab_with_no_dataset_selected_does_not_panic_or_open_form() {
        let mut app = App::new(vec!["local".to_string()]);
        enter_datasets_acl_tab(&mut app);
        app.handle_key(KeyCode::Char('n'));
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_key_n_on_acl_tab_with_no_mountpoint_does_not_panic_or_open_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/vol".to_string(), kind: crate::zfs::dataset::DatasetKind::Volume, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_acl_tab(&mut app);
        app.handle_key(KeyCode::Char('n'));
        assert!(app.modal.is_none(), "a zvol (no mountpoint) must not open an ACL form");
    }

    #[test]
    fn handle_key_n_on_acl_tab_opens_add_acl_entry_form_appending_at_the_current_count() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        enter_datasets_acl_tab(&mut app);
        app.acl_entries = vec![
            crate::acl::AclEntry { tag: crate::acl::AclTag::Owner, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow },
            crate::acl::AclEntry { tag: crate::acl::AclTag::OwningGroup, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow },
        ];
        app.acl_entries_for = Some("/tank/data".to_string());
        app.handle_key(KeyCode::Char('n'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 4);
                assert_eq!(*kind, FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 2 });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_n_on_acl_tab_before_acl_has_loaded_shows_status_and_does_not_open_form() {
        // Catches a real bug a final review found: pressing 'n' while
        // acl_entries is still empty (fetch in flight, fetch failed, or
        // just-cleared by a Datasets refresh) computed position as 0
        // and silently prepended instead of appending -- `setfacl -a 0`
        // puts the new entry FIRST, ahead of every existing entry,
        // confirmed live. Must refuse until acl_entries_for matches.
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        enter_datasets_acl_tab(&mut app);
        // acl_entries is empty and acl_entries_for is None -- never refreshed.
        app.handle_key(KeyCode::Char('n'));
        assert!(app.modal.is_none(), "must not open a form that would compute the wrong (prepend) position");
        assert!(app.status_message.is_some());
    }

    #[test]
    fn handle_key_d_on_acl_tab_with_no_row_selected_does_not_panic() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        enter_datasets_acl_tab(&mut app);
        app.handle_key(KeyCode::Char('d'));
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_key_d_on_selected_acl_row_opens_remove_confirm_with_its_position() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        enter_datasets_acl_tab(&mut app);
        app.acl_entries = vec![
            crate::acl::AclEntry { tag: crate::acl::AclTag::Owner, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow },
            crate::acl::AclEntry { tag: crate::acl::AclTag::OwningGroup, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow },
        ];
        app.acl_entries_table.select(Some(1));
        app.handle_key(KeyCode::Char('d'));
        match &app.modal {
            Some(Modal::Confirm { action, .. }) => {
                assert_eq!(action.call, MutationCall::RemoveAclEntry { path: "/tank/data".to_string(), position: 1 });
            }
            other => panic!("expected Confirm modal, got {other:?}"),
        }
    }

    #[test]
    fn build_pending_action_share_nfs_assembles_filled_fields() {
        let kind = FormKind::ShareNfs { host: 0, dataset: "tank/data".to_string() };
        let fields = vec![
            FormField { label: "Networks".to_string(), value: "172.16.10.0/24".to_string() },
            FormField { label: "Hosts".to_string(), value: String::new() },
            FormField { label: "Read-only (y/n)".to_string(), value: "n".to_string() },
            FormField { label: "Root user mapping".to_string(), value: "nobody".to_string() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action.call,
            MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharenfs".to_string(), value: "-maproot=nobody -network 172.16.10.0/24".to_string() }
        );
    }

}
