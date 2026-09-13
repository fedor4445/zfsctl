use crate::error::Result;
use crate::exec::CommandRunner;
use crate::ui::modal::{FormField, FormKind, Modal, MutationCall, PendingAction};
use crate::zfs::dataset::Dataset;
use crate::zfs::pool::{Pool, VdevLine};
use crate::zfs::property::Property;
use crate::zfs::snapshot::Snapshot;
use crossterm::event::KeyCode;
use ratatui::widgets::TableState;
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Hosts,
    Pools,
    Datasets,
    Snapshots,
    Vdevs,
    PoolProperties,
    DatasetProperties,
}

/// Which pane currently receives navigation keys. Meaningful only while
/// the current section is Pools or Datasets — those are the only two
/// sections with a right pane at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneFocus {
    Middle,
    Right,
}

/// Each variant carries the index (into `App::hosts`) of the host the
/// result is *about*, so a slow result for a host the user has since
/// navigated away from can be told apart from a fresh one instead of
/// silently overwriting the currently-selected host's pane.
pub enum WorkerMsg {
    Pools(usize, Result<Vec<Pool>>),
    Datasets(usize, Result<Vec<Dataset>>),
    Snapshots(usize, Result<Vec<Snapshot>>),
    /// The `String` is the name of the pool this vdev refresh was for —
    /// needed so a slow result can be told apart from one for a
    /// different pool the user has since drilled into (see the
    /// stale-pool check in `drain_worker_messages`).
    Vdevs(usize, String, Result<Vec<VdevLine>>),
    /// The `String` is the pool name this fetch was for — same
    /// stale-identity discard rationale as `Vdevs`.
    PoolProperties(usize, String, Result<Vec<Property>>),
    /// The `String` is the dataset name this fetch was for.
    DatasetProperties(usize, String, Result<Vec<Property>>),
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
    pub vdevs: Vec<VdevLine>,
    pub vdevs_table: TableState,
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
            sections: vec![Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots],
            selected_section: 0,
            hosts,
            selected_host: 0,
            pools: Vec::new(),
            datasets: Vec::new(),
            snapshots: Vec::new(),
            pools_table: TableState::default(),
            datasets_table: TableState::default(),
            snapshots_table: TableState::default(),
            vdevs: Vec::new(),
            vdevs_table: TableState::default(),
            drilled_pool: None,
            pane_focus: PaneFocus::Middle,
            pool_properties: Vec::new(),
            pool_properties_table: TableState::default(),
            pool_properties_for: None,
            dataset_properties: Vec::new(),
            dataset_properties_table: TableState::default(),
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

    /// True while any (section, host) refresh is in flight. Used only
    /// for the coarse "refreshing..." indicator in the sidebar title.
    pub fn is_loading(&self) -> bool {
        !self.in_flight.is_empty()
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

    /// Clears data panes so a stale render from the previously-selected
    /// host is never left on screen looking current after switching hosts.
    fn clear_host_data(&mut self) {
        self.pools.clear();
        self.datasets.clear();
        self.snapshots.clear();
        self.pools_table.select(None);
        self.datasets_table.select(None);
        self.snapshots_table.select(None);
        self.pane_focus = PaneFocus::Middle;
        self.pool_properties.clear();
        self.pool_properties_table.select(None);
        self.pool_properties_for = None;
        self.dataset_properties.clear();
        self.dataset_properties_table.select(None);
        self.dataset_properties_for = None;
    }

    pub fn handle_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc | KeyCode::Backspace if self.drilled_pool.is_some() => {
                self.drilled_pool = None;
                self.vdevs.clear();
                self.vdevs_table.select(None);
            }
            KeyCode::Esc if self.pane_focus == PaneFocus::Right => {
                self.pane_focus = PaneFocus::Middle;
            }
            KeyCode::Esc => self.status_message = None,
            KeyCode::Enter if self.current_section() == Section::Pools && self.drilled_pool.is_none() && self.pane_focus == PaneFocus::Middle => {
                if let Some(i) = self.pools_table.selected() {
                    if let Some(pool) = self.pools.get(i) {
                        self.drilled_pool = Some(pool.name.clone());
                        self.vdevs.clear();
                        self.vdevs_table.select(None);
                    }
                }
            }
            KeyCode::Enter if self.pane_focus == PaneFocus::Right && self.current_section() == Section::Pools => {
                if let (Some(pool), Some(i)) = (self.selected_pool_name(), self.pool_properties_table.selected()) {
                    if let Some(prop) = self.pool_properties.get(i) {
                        self.modal = Some(Modal::Form {
                            title: format!("Edit '{}' on pool '{pool}'", prop.name),
                            fields: vec![FormField { label: prop.name.clone(), value: prop.value.clone() }],
                            focused: 0,
                            kind: FormKind::EditPoolProperty { host: self.selected_host, pool, key: prop.name.clone() },
                        });
                    }
                }
            }
            KeyCode::Enter if self.pane_focus == PaneFocus::Right && self.current_section() == Section::Datasets => {
                if let (Some(dataset), Some(i)) = (self.selected_dataset_name(), self.dataset_properties_table.selected()) {
                    if let Some(prop) = self.dataset_properties.get(i) {
                        self.modal = Some(Modal::Form {
                            title: format!("Edit '{}' on dataset '{dataset}'", prop.name),
                            fields: vec![FormField { label: prop.name.clone(), value: prop.value.clone() }],
                            focused: 0,
                            kind: FormKind::EditDatasetProperty { host: self.selected_host, dataset, key: prop.name.clone() },
                        });
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
                                kind: FormKind::ReplaceDisk { host: self.selected_host, pool, old_disk: vdev.name.clone() },
                            });
                        } else {
                            self.status_message = Some("select a disk (not a mirror/raidz group) to replace it".to_string());
                        }
                    }
                }
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
                    kind: FormKind::CreatePool { host: self.selected_host },
                });
            }
            KeyCode::Down if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => {
                move_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => {
                move_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), true);
                self.dataset_properties.clear();
                self.dataset_properties_table.select(None);
                self.dataset_properties_for = None;
            }
            KeyCode::Up if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), false);
                self.dataset_properties.clear();
                self.dataset_properties_table.select(None);
                self.dataset_properties_for = None;
            }
            KeyCode::Char('n') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                self.modal = Some(Modal::Form {
                    title: "Create Dataset".to_string(),
                    fields: vec![FormField { label: "Name".to_string(), value: String::new() }],
                    focused: 0,
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
                            kind: FormKind::CreateSnapshot { host: self.selected_host, dataset: ds.name.clone() },
                        });
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
            Modal::Form { title, mut fields, mut focused, kind } => match key {
                KeyCode::Esc => None,
                KeyCode::Tab => {
                    focused = (focused + 1) % fields.len();
                    self.modal = Some(Modal::Form { title, fields, focused, kind });
                    None
                }
                KeyCode::Enter => {
                    if focused + 1 < fields.len() {
                        focused += 1;
                        self.modal = Some(Modal::Form { title, fields, focused, kind });
                        None
                    } else {
                        match build_pending_action(&kind, &fields) {
                            Some(action) => Some(action),
                            None => {
                                self.modal = Some(Modal::Form { title, fields, focused, kind });
                                None
                            }
                        }
                    }
                }
                KeyCode::Backspace => {
                    fields[focused].value.pop();
                    self.modal = Some(Modal::Form { title, fields, focused, kind });
                    None
                }
                KeyCode::Char(c) => {
                    fields[focused].value.push(c);
                    self.modal = Some(Modal::Form { title, fields, focused, kind });
                    None
                }
                _ => {
                    self.modal = Some(Modal::Form { title, fields, focused, kind });
                    None
                }
            },
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
                        Ok(vdevs) => {
                            self.vdevs = vdevs;
                            clamp_table_selection(&mut self.vdevs_table, self.vdevs.len());
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
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
            }
        }
        refreshes_needed
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;
    use crate::zfs::property::PropertySource;
    use std::time::{Duration, Instant};

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
        assert_eq!(refreshes, vec![Section::Pools]);
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
        assert_eq!(app.current_section(), Section::Snapshots);
    }

    #[test]
    fn handle_key_quit_sets_should_quit() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('q'));
        assert!(app.should_quit);
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
        app.handle_key(KeyCode::BackTab); // Hosts -> Snapshots (wraps backward)
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
                Ok(vec![VdevLine { depth: 0, name: "tank".to_string(), health: crate::zfs::pool::PoolHealth::Online }]),
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
        app.handle_key(KeyCode::BackTab); // Hosts -> Snapshots
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
        app.handle_key(KeyCode::BackTab);
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
        assert_eq!(refreshes, vec![Section::Vdevs, Section::Pools]);
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
        assert_eq!(app.current_section(), Section::Snapshots, "must wrap backward from the first section to the last");
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
        app.handle_key(KeyCode::Tab); // -> Datasets, resets pane_focus to Middle first
        app.handle_key(KeyCode::BackTab); // -> Pools again
        app.handle_key(KeyCode::BackTab); // -> Hosts
        app.handle_key(KeyCode::Down); // switch host, triggers clear_host_data
        assert_eq!(app.pane_focus, PaneFocus::Middle);
        assert!(app.pool_properties.is_empty());
        assert_eq!(app.pool_properties_table.selected(), None);
        assert!(app.dataset_properties.is_empty());
        assert_eq!(app.dataset_properties_table.selected(), None);
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
            Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].value, "old comment", "the form must be pre-filled with the property's current value");
                assert_eq!(*kind, crate::ui::modal::FormKind::EditPoolProperty { host: 0, pool: "tank".to_string(), key: "comment".to_string() });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
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
            Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields[0].value, "lz4");
                assert_eq!(*kind, crate::ui::modal::FormKind::EditDatasetProperty { host: 0, dataset: "tank/data".to_string(), key: "compression".to_string() });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
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
}
