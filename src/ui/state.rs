use crate::error::Result;
use crate::exec::CommandRunner;
use crate::ui::modal::{FormField, FormKind, Modal, MutationCall, PendingAction};
use crate::ui::picker::{build_picker_state, PickerState};
use crate::zfs::dataset::Dataset;
use crate::zfs::disk::DiskInfo;
use crate::zfs::disk::DiskMembership;
use crate::zfs::pool::{Pool, VdevLine};
use crate::zfs::property::Property;
use crate::zfs::share::SharesSnapshot;
use crate::zfs::snapshot::Snapshot;
use crossterm::event::KeyCode;
use ratatui::widgets::TableState;
use std::collections::{HashMap, HashSet};
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
    BootEnvs,
    Jobs,
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
    BootEnvs(usize, Result<Vec<crate::zfs::bootenv::BootEnv>>),
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
    /// The target is what the sample was taken for; `MetricsView::apply` drops it if the panel has moved on.
    Metrics(usize, crate::zfs::metrics::MetricsTarget, Box<Result<crate::zfs::metrics::TargetSample>>),
    MutationDone(usize, Section, Result<()>),
    /// The outcome of an Add Host SSH reachability probe. `String,
    /// String` are the candidate host's name and SSH target.
    HostProbe(String, String, Result<()>),
    /// The outcome of a live account fetch for host index `usize`.
    Accounts(usize, Result<crate::zfs::accounts::HostAccounts>),
    /// The outcome of a live pool-name fetch for the job form's Pool picker.
    PoolNames(usize, Result<Vec<String>>),
}

pub const ACCOUNT_CACHE_TTL: Duration = Duration::from_mins(1);
pub const ACCOUNT_FETCH_STUCK_AFTER: Duration = Duration::from_secs(30);

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
    pub jobs_view: crate::ui::jobs::JobsView,
    pub jobs_table: TableState,
    /// Where `jobs.toml`, `history.jsonl` and `daemon.json` live: the daemon's state directory.
    pub state_dir: std::path::PathBuf,
    last_jobs_refresh: Option<Instant>,
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
    pub bootenvs: Vec<crate::zfs::bootenv::BootEnv>,
    pub bootenvs_table: TableState,
    /// Why the Boot Envs section cannot list on this host (e.g. no `bectl`, not a boot-environment
    /// layout, an unreachable host), shown in its title. `None` once a list succeeds.
    pub bootenvs_unavailable: Option<String>,
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
    pub metrics: crate::ui::metrics::MetricsView,
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
    /// A picker popup layered on top of `modal` (which stays open and
    /// untouched underneath) -- separate state, not a `Modal` variant,
    /// so the form being edited and the picker helping edit one of its
    /// fields are independent for as long as the picker is open.
    pub picker: Option<PickerState>,
    pub status_message: Option<String>,
    pub theme: crate::ui::theme::Theme,
    /// The text of the status message that was set via `set_error`.
    /// Compared against `status_message` rather than kept as a bool, so
    /// it invalidates itself the moment the message is replaced or
    /// cleared — a stale flag can never recolor an unrelated message.
    status_error: Option<String>,
    pub should_quit: bool,
    /// Appended by `drain_worker_messages` once a probed host is
    /// confirmed reachable; drained by the event loop (which owns the
    /// live `HostInventory` -- `App` itself never touches it directly)
    /// to hot-reload each one and persist it to `config.toml`. A `Vec`,
    /// not a single `Option` slot: a final review caught that a lone
    /// slot silently lost a second host whose probe resolved in the
    /// same event-loop tick as the first's.
    pub pending_new_hosts: Vec<(String, String)>,
    /// Set by `!`; the event loop takes it, because only it owns the terminal
    /// and can suspend the TUI to run a shell.
    pub pending_shell: bool,
    /// Mutations to run on the next tick without a confirmation step (for example enabling or
    /// disabling a job). The event loop drains this; `handle_key` cannot return an action itself.
    pub pending_mutations: Vec<PendingAction>,
    pub account_cache: HashMap<usize, (Instant, crate::zfs::accounts::HostAccounts)>,
    pub account_fetch_in_flight: HashMap<usize, Instant>,
    pub pending_account_fetch: Vec<usize>,
    pub pending_pool_fetch: Vec<(usize, crate::ui::picker::LiveNames)>,
    /// Lowercased names currently being probed (submitted, not yet
    /// resolved) -- checked alongside `self.hosts` so a second
    /// submission of the same name while the first is still
    /// connecting is rejected too, not just once the first has
    /// finished and landed in `self.hosts`.
    pending_probes: HashSet<String>,
    in_flight: HashSet<(Section, usize)>,
    /// Refresh requests for a `(section, host)` that arrived while a
    /// refresh for that same key was already in flight. Recorded here
    /// instead of being silently dropped, and re-dispatched (via the
    /// `Vec<Section>` returned from `drain_worker_messages`) once the
    /// in-flight one completes — see `refresh_pools` et al. and the
    /// pending-refresh handling in `drain_worker_messages`.
    pending_refresh: HashSet<(Section, usize)>,
    /// In-flight refreshes that were started by the periodic auto-refresh
    /// rather than the user: they don't show the "refreshing..." indicator
    /// and a successful one doesn't wipe a status message the user is reading.
    quiet_refresh: HashSet<(Section, usize)>,
    /// When each `(section, host)` was last auto-refreshed.
    last_auto_refresh: HashMap<(Section, usize), Instant>,
    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
}

impl App {
    pub fn new(hosts: Vec<String>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        App {
            sections: vec![Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots, Section::Disks, Section::Shares, Section::BootEnvs, Section::Jobs],
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
            jobs_view: crate::ui::jobs::JobsView::default(),
            jobs_table: TableState::default(),
            state_dir: std::path::PathBuf::from(crate::daemon::engine::DEFAULT_STATE_DIR),
            last_jobs_refresh: None,
            nfs_available: None,
            samba_available: None,
            shares_loaded: false,
            bootenvs: Vec::new(),
            bootenvs_table: TableState::default(),
            bootenvs_unavailable: None,
            vdevs: Vec::new(),
            vdevs_table: TableState::default(),
            pool_scan_status: None,
            pool_scan_status_shown_at: None,
            drilled_pool: None,
            metrics: crate::ui::metrics::MetricsView::default(),
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
            picker: None,
            status_message: None,
            theme: crate::ui::theme::Theme::ansi(),
            status_error: None,
            should_quit: false,
            pending_new_hosts: Vec::new(),
            pending_shell: false,
            pending_mutations: Vec::new(),
            account_cache: HashMap::new(),
            account_fetch_in_flight: HashMap::new(),
            pending_account_fetch: Vec::new(),
            pending_pool_fetch: Vec::new(),
            pending_probes: HashSet::new(),
            in_flight: HashSet::new(),
            pending_refresh: HashSet::new(),
            quiet_refresh: HashSet::new(),
            last_auto_refresh: HashMap::new(),
            tx,
            rx,
        }
    }

    pub fn current_section(&self) -> Section {
        // `selected_section` is always in range; Hosts (the first section) is the harmless fallback.
        self.sections.get(self.selected_section).copied().unwrap_or(Section::Hosts)
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
        self.in_flight.iter().any(|key| key.0 != Section::Vdevs && !self.quiet_refresh.contains(key))
    }

    fn is_refreshing(&self, section: Section, host: usize) -> bool {
        self.in_flight.contains(&(section, host))
    }

    fn begin_refresh(&mut self, section: Section, host: usize) {
        self.in_flight.insert((section, host));
    }

    /// Returns whether the refresh that just finished was a quiet (auto) one.
    fn end_refresh(&mut self, section: Section, host: usize) -> bool {
        self.in_flight.remove(&(section, host));
        self.quiet_refresh.remove(&(section, host))
    }

    /// A manual refresh result clears whatever status message was showing; a quiet
    /// (auto) one only clears a stale error, so "scrub started" is not wiped every
    /// interval but a "host unreachable" error does go away once the host answers.
    fn clear_status_after_refresh(&mut self, quiet: bool) {
        if !quiet || self.status_is_error() {
            self.status_message = None;
        }
    }

    /// The list section the periodic refresh should keep fresh right now, if any:
    /// the one on screen, and only while nothing (a modal, a picker, a pool
    /// drill-down, which has its own faster poll) could be mid-interaction with it.
    /// Hosts has nothing to refresh.
    pub fn auto_refresh_section(&self) -> Option<Section> {
        if self.modal.is_some() || self.picker.is_some() || self.drilled_pool.is_some() {
            return None;
        }
        match self.current_section() {
            section @ (Section::Pools | Section::Datasets | Section::Snapshots | Section::Disks | Section::Shares | Section::BootEnvs) => Some(section),
            _ => None,
        }
    }

    /// Refreshes the visible list section if `interval` has passed since it was last
    /// auto-refreshed (immediately the first time it is shown on this host).
    pub fn auto_refresh(&mut self, runner: Arc<dyn CommandRunner>, now: Instant, interval: Duration) {
        let Some(section) = self.auto_refresh_section() else { return };
        let key = (section, self.selected_host);
        if self.last_auto_refresh.get(&key).is_some_and(|last| now.duration_since(*last) < interval) {
            return;
        }
        self.last_auto_refresh.insert(key, now);
        let already_running = self.is_refreshing(section, key.1);
        match section {
            Section::Pools => self.refresh_pools(runner),
            Section::Datasets => self.refresh_datasets(runner),
            Section::Snapshots => self.refresh_snapshots(runner),
            Section::Disks => self.refresh_disks(runner),
            Section::Shares => self.refresh_shares(runner),
            Section::BootEnvs => self.refresh_bootenvs(runner),
            _ => return,
        }
        if !already_running {
            self.quiet_refresh.insert(key);
        }
    }

    /// The object the bottom metrics panel describes right now: the drilled
    /// pool, else the highlighted pool or dataset, else nothing.
    pub fn metrics_target(&self) -> Option<crate::zfs::metrics::MetricsTarget> {
        use crate::zfs::metrics::MetricsTarget;
        if let Some(pool) = &self.drilled_pool {
            return Some(MetricsTarget::Pool(pool.clone()));
        }
        match self.current_section() {
            Section::Hosts => (!self.hosts.is_empty()).then_some(MetricsTarget::Host),
            Section::Pools => self.selected_pool_name().map(MetricsTarget::Pool),
            Section::Datasets => self.selected_dataset_name().map(MetricsTarget::Dataset),
            Section::Disks => self.disks_table.selected().and_then(|i| self.disks.get(i)).map(|d| MetricsTarget::Disk {
                name: d.name.clone(),
                pool: match &d.membership {
                    crate::zfs::disk::DiskMembership::InPool { pool, .. } => Some(pool.clone()),
                    crate::zfs::disk::DiskMembership::Available => None,
                },
            }),
            Section::Shares => self.shares_table.selected().and_then(|i| self.shares.get(i)).map(|share| match share {
                crate::zfs::share::Share::Nfs { dataset, .. } => MetricsTarget::Share { dataset: dataset.clone(), smb_name: None },
                crate::zfs::share::Share::Smb { dataset, name, .. } => MetricsTarget::Share { dataset: dataset.clone(), smb_name: Some(name.clone()) },
            }),
            Section::Snapshots => self
                .snapshots_table
                .selected()
                .and_then(|i| self.snapshots.get(i))
                .map(|s| MetricsTarget::Snapshot(format!("{}@{}", s.dataset, s.name))),
            _ => None,
        }
    }

    /// Keeps the metrics panel pointed at the selection and starts a sample
    /// on a worker thread when one is due. Called once per event-loop tick.
    pub fn poll_metrics(&mut self, runner: Arc<dyn CommandRunner>, now: Instant) {
        self.metrics.retarget(self.metrics_target());
        if !self.metrics.due(now) {
            return;
        }
        let Some(target) = self.metrics.target().cloned() else { return };
        self.metrics.begin();
        let host = self.selected_host;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::metrics::sample_target(runner.as_ref(), &target);
            let _ = tx.send(WorkerMsg::Metrics(host, target, Box::new(result)));
        });
    }

    /// Reloads whatever list is on screen, right now. Used after a shell
    /// session, which may have changed ZFS state behind the TUI's back.
    pub fn refresh_visible(&mut self, runner: Arc<dyn CommandRunner>) {
        if let Some(pool) = self.drilled_pool.clone() {
            self.refresh_vdevs(runner, pool);
            return;
        }
        match self.current_section() {
            Section::Pools => self.refresh_pools(runner),
            Section::Datasets => self.refresh_datasets(runner),
            Section::Snapshots => self.refresh_snapshots(runner),
            Section::Disks => self.refresh_disks(runner),
            Section::Shares => self.refresh_shares(runner),
            Section::BootEnvs => self.refresh_bootenvs(runner),
            Section::Jobs => self.refresh_jobs(),
            _ => {}
        }
    }

    /// Reloads the Jobs data from the state directory. These are small local files, so
    /// there is no runner and no thread. The selection stays on the same job.
    pub fn refresh_jobs(&mut self) {
        let selected = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)).map(|j| j.id.clone());
        self.jobs_view = crate::ui::jobs::load(&self.state_dir, &self.hosts, Some(&self.jobs_view));
        match selected.and_then(|id| self.jobs_view.jobs.iter().position(|j| j.id == id)) {
            Some(i) => self.jobs_table.select(Some(i)),
            None => clamp_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len()),
        }
    }

    /// Refreshes the Jobs data if it is stale: every 2 seconds while the section is on screen
    /// (so a run's result appears live), every 10 otherwise (so the failed-jobs indicator is
    /// right from any section). The first call always loads.
    pub fn refresh_jobs_if_stale(&mut self, now: Instant) {
        let every = if self.current_section() == Section::Jobs { Duration::from_secs(2) } else { Duration::from_secs(10) };
        if self.last_jobs_refresh.is_none_or(|t| now.duration_since(t) >= every) {
            self.last_jobs_refresh = Some(now);
            self.refresh_jobs();
        }
    }

    /// Sets a failure message (rendered with the theme's `err` style).
    pub fn set_error(&mut self, msg: String) {
        self.status_error = Some(msg.clone());
        self.status_message = Some(msg);
    }

    pub fn status_is_error(&self) -> bool {
        self.status_message.is_some() && self.status_message == self.status_error
    }

    /// Name of the currently-selected host, for pane titles.
    pub fn active_host_name(&self) -> &str {
        self.hosts.get(self.selected_host).map_or("?", std::string::String::as_str)
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
        self.metrics = crate::ui::metrics::MetricsView::default();
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
        self.bootenvs.clear();
        self.bootenvs_table.select(None);
        self.bootenvs_unavailable = None;
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

    // Arms are ordered by guard precedence, so identical bodies cannot be merged into one `|` arm
    // without changing which guard wins.
    #[allow(clippy::match_same_arms)]
    pub fn handle_key(&mut self, key: KeyCode) {
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
                        if prop.source.is_editable() {
                            self.modal = Some(Modal::Form {
                                title: format!("Edit '{}' on dataset '{dataset}'", prop.name),
                                fields: vec![FormField { label: prop.name.clone(), value: prop.value.clone() }],
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
            KeyCode::Char('n') if self.current_section() == Section::Hosts => {
                self.modal = Some(Modal::Form {
                    title: "Add Host".to_string(),
                    fields: vec![
                        FormField { label: "Name".to_string(), value: String::new() },
                        FormField { label: "SSH target (e.g. user@host)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    description: None,
                    kind: FormKind::AddHost,
                });
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
                            let Some(pool) = self.drilled_pool.clone() else { return };
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
                let Some(pool) = self.drilled_pool.clone() else { return };
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
                            fields: vec![FormField { label: "Permissions to remove (F2 to pick)".to_string(), value: String::new() }],
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
                // Only act (or explain) when a dataset row is selected; with no
                // selection the key is not advertised and must be inert.
                let Some(ds) = self.datasets_table.selected().and_then(|i| self.datasets.get(i)).cloned() else { return; };
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
            KeyCode::Char('M') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                let Some(ds) = self.datasets_table.selected().and_then(|i| self.datasets.get(i)).cloned() else { return; };
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
            KeyCode::Down if self.current_section() == Section::BootEnvs => {
                move_table_selection(&mut self.bootenvs_table, self.bootenvs.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::BootEnvs => {
                move_table_selection(&mut self.bootenvs_table, self.bootenvs.len(), false);
            }
            KeyCode::Char('J') if self.current_section() == Section::Pools && self.drilled_pool.is_none() && self.pane_focus == PaneFocus::Middle => {
                match self.selected_pool_name() {
                    Some(pool) => self.modal = Some(self.job_form(None, &pool)),
                    None => self.status_message = Some("Select a pool first, then press J to schedule a scrub for it".to_string()),
                }
            }
            KeyCode::Char('n') if self.current_section() == Section::Jobs => {
                self.modal = Some(self.job_form(None, ""));
            }
            KeyCode::Char('N') if self.current_section() == Section::Jobs => {
                self.modal = Some(self.snapshot_job_form(None, ""));
            }
            KeyCode::Char('R') if self.current_section() == Section::Jobs => {
                self.modal = Some(self.replicate_job_form(None));
            }
            KeyCode::Char('c') if self.current_section() == Section::Jobs => {
                if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)) {
                    if job.kind == crate::jobs::KIND_REPLICATE && self.jobs_view.is_running(&job.id) {
                        match crate::daemon::cancel::request(&self.state_dir.join("cancel"), &job.id) {
                            Ok(()) => self.status_message = Some(format!("Cancel requested for '{}'; it stops within a second or two", job.id)),
                            Err(e) => self.set_error(format!("Could not request the cancel: {e}")),
                        }
                    }
                }
            }
            KeyCode::Char('J') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => match self.selected_dataset_name() {
                Some(dataset) => self.modal = Some(self.snapshot_job_form(None, &dataset)),
                None => self.status_message = Some("Select a dataset first, then press J to schedule snapshots of it".to_string()),
            },
            KeyCode::Enter if self.current_section() == Section::Jobs => {
                if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)).cloned() {
                    self.modal = Some(match job.kind.as_str() {
                        crate::jobs::KIND_SNAPSHOT => self.snapshot_job_form(Some(job), ""),
                        crate::jobs::KIND_REPLICATE => self.replicate_job_form(Some(job)),
                        _ => self.job_form(Some(job.clone()), job.pool.as_deref().unwrap_or("")),
                    });
                }
            }
            KeyCode::Char(' ') if self.current_section() == Section::Jobs => {
                if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)) {
                    let enable = !job.enabled;
                    let as_enabled = crate::jobs::Job { enabled: true, ..job.clone() };
                    if let Some(other) = enable.then(|| crate::jobs::clashing_job(&self.jobs_view.jobs, &as_enabled)).flatten() {
                        let why = crate::jobs::clash_reason(other, &as_enabled).unwrap_or_default();
                        self.set_error(format!("Cannot enable '{}': it clashes with enabled job '{}': {why}", job.id, other.id));
                        return;
                    }
                    self.status_message = Some(format!("{} job '{}'…", if enable { "Enabling" } else { "Disabling" }, job.id));
                    self.pending_mutations.push(PendingAction {
                        host: self.selected_host,
                        section: Section::Jobs,
                        call: MutationCall::SetJobEnabled { state_dir: self.state_dir.clone(), id: job.id.clone(), enabled: enable },
                    });
                }
            }
            KeyCode::Char('d') if self.current_section() == Section::Jobs => {
                if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)) {
                    let prompt = format!("Delete job '{}'? It stops running; its past runs stay in the history. (y/n)", job.id);
                    let call = MutationCall::DeleteJob { state_dir: self.state_dir.clone(), id: job.id.clone() };
                    self.modal = Some(Modal::Confirm { prompt, action: PendingAction { host: self.selected_host, section: Section::Jobs, call } });
                }
            }
            KeyCode::Down if self.current_section() == Section::Jobs => {
                move_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Jobs => {
                move_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len(), false);
            }
            KeyCode::Enter if self.current_section() == Section::Shares => {
                if let Some(i) = self.shares_table.selected() {
                    if let Some(share) = self.shares.get(i) {
                        self.modal = Some(share_edit_modal(share, self.selected_host));
                    }
                }
            }
            KeyCode::Char('d') if self.current_section() == Section::Shares => {
                if let Some(i) = self.shares_table.selected() {
                    if let Some(share) = self.shares.get(i) {
                        let (prompt, call) = match share {
                            crate::zfs::share::Share::Nfs { dataset, .. } => (
                                format!("Remove NFS share for '{dataset}'? (y/n)"),
                                MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: "sharenfs".to_string(), value: "off".to_string() },
                            ),
                            crate::zfs::share::Share::Smb { dataset, name, .. } => (
                                format!("Remove SMB share '{name}' for '{dataset}'? Connected clients lose access. (y/n)"),
                                MutationCall::RemoveSmbShare { dataset: dataset.clone() },
                            ),
                        };
                        self.modal = Some(Modal::Confirm { prompt, action: PendingAction { host: self.selected_host, section: Section::Shares, call } });
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
                        FormField { label: "Redundancy (blank=stripe, or mirror/raidz1/raidz2/raidz3)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    description: Some(self.selected_disk_identities(&disks)),
                    kind: FormKind::AddVdevFromDisks { host: self.selected_host, disks },
                });
            }
            KeyCode::Char('a') if self.current_section() == Section::Disks => {
                self.status_message = Some("select at least one disk with Space first".to_string());
            }
            _ => {}
        }
    }

    /// One identity line per selected disk (by name lookup in
    /// `self.disks`; a disk that vanished shows just its name). At most
    /// `MAX_IDENTITY_LINES` are listed, then `+K more selected`, so the
    /// form's input fields can never be pushed out of a small popup.
    fn selected_disk_identities(&self, names: &[String]) -> String {
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
    fn open_vdev_confirm(&mut self, online: bool) {
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
                KeyCode::Tab | KeyCode::Down => {
                    focused = (focused + 1) % fields.len();
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
                KeyCode::BackTab | KeyCode::Up => {
                    focused = if focused == 0 { fields.len() - 1 } else { focused - 1 };
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
                KeyCode::Enter => {
                    if focused + 1 < fields.len() {
                        focused += 1;
                        self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                        None
                    } else if matches!(kind, FormKind::AddHost) {
                        let name = field_value(&fields, 0).trim().to_string();
                        let target = field_value(&fields, 1).trim().to_string();
                        let name_lower = name.to_lowercase();
                        let is_duplicate = self.hosts.iter().any(|h| h.eq_ignore_ascii_case(&name)) || self.pending_probes.contains(&name_lower);
                        if name.is_empty() || target.is_empty() {
                            self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                            None
                        } else if is_duplicate {
                            self.set_error(format!("A host named '{name}' already exists or is already being added"));
                            self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                            None
                        } else {
                            self.pending_probes.insert(name_lower);
                            self.begin_add_host(name, target);
                            None
                        }
                    } else if matches!(kind, FormKind::Job { .. }) {
                        match build_job_action(&kind, &fields, &self.state_dir, chrono::Utc::now()) {
                            Ok(mut action) => {
                                action.host = self.selected_host;
                                Some(action)
                            }
                            Err(msg) => {
                                self.set_error(msg);
                                self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
                                None
                            }
                        }
                    } else if matches!(kind, FormKind::SnapshotJob { .. }) {
                        match build_snapshot_job_action(&kind, &fields, &self.state_dir, chrono::Utc::now()) {
                            Ok(mut action) => {
                                action.host = self.selected_host;
                                Some(action)
                            }
                            Err(msg) => {
                                self.set_error(msg);
                                self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
                                None
                            }
                        }
                    } else if matches!(kind, FormKind::ReplicateJob { .. }) {
                        match build_replicate_job_action(&kind, &fields, &self.state_dir, chrono::Utc::now()) {
                            Ok(mut action) => {
                                action.host = self.selected_host;
                                Some(action)
                            }
                            Err(msg) => {
                                self.set_error(msg);
                                self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
                                None
                            }
                        }
                    } else if matches!(kind, FormKind::ShareSmb { .. }) {
                        match build_smb_action(&kind, &fields, &self.shares) {
                            Ok(action) => Some(action),
                            Err(msg) => {
                                self.set_error(msg);
                                self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
                                None
                            }
                        }
                    } else if let Some(action) = build_pending_action(&kind, &fields) { Some(action) } else {
                        self.set_error("Could not submit -- check the field values against the hints in their labels".to_string());
                        self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
                        None
                    }
                }
                KeyCode::Backspace => {
                    if let Some(field) = fields.get_mut(focused) {
                        field.value.pop();
                    }
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
                KeyCode::Char(c) => {
                    if let Some(field) = fields.get_mut(focused) {
                        field.value.push(c);
                    }
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
                KeyCode::F(2) => {
                    if let Some(mut picker) = build_picker_state(&kind, focused, field_value(&fields, focused)) {
                        if let Some((host, _)) = picker.account {
                            self.open_account_picker(&mut picker, host, field_value(&fields, focused), Instant::now());
                        }
                        if picker.pool_host.is_some() {
                            // The destination dataset lists the *destination* host's datasets.
                            let host_field = if matches!(kind, FormKind::ReplicateJob { .. }) && focused == 4 { 3 } else { 0 };
                            self.open_pool_picker(&mut picker, &kind, field_value(&fields, host_field));
                        }
                        self.picker = Some(picker);
                    }
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

    /// Fills `picker` from a fresh cache entry for `host`, or leaves it
    /// `Loading` and queues one fetch (unless a non-stuck one is already in
    /// flight). `now` is a parameter so tests can age cache/in-flight entries.
    fn open_account_picker(&mut self, picker: &mut PickerState, host: usize, current_value: &str, now: Instant) {
        if let Some((fetched_at, accounts)) = self.account_cache.get(&host) {
            if now.duration_since(*fetched_at) < ACCOUNT_CACHE_TTL {
                crate::ui::picker::fill_account_picker(picker, accounts, current_value);
                return;
            }
        }
        let stuck = self.account_fetch_in_flight.get(&host).is_none_or(|t| now.duration_since(*t) >= ACCOUNT_FETCH_STUCK_AFTER);
        if stuck {
            self.account_fetch_in_flight.insert(host, now);
            self.pending_account_fetch.push(host);
        }
    }

    /// Points the job form's Pool picker at the host named in the form's Host field and queues
    /// one fetch of its pool names; an unknown host name fails the picker right away.
    fn open_pool_picker(&mut self, picker: &mut PickerState, kind: &FormKind, host_name: &str) {
        let (hosts, what) = match kind {
            FormKind::Job { hosts, .. } => (hosts, crate::ui::picker::LiveNames::Pools),
            FormKind::SnapshotJob { hosts, .. } | FormKind::ReplicateJob { hosts, .. } => (hosts, crate::ui::picker::LiveNames::Datasets),
            _ => return,
        };
        if host_name.trim() == crate::jobs::DEST_FILE {
            picker.load = crate::ui::picker::PickerLoad::Failed("the destination is a directory on the hub: type its path".to_string());
            return;
        }
        match hosts.iter().position(|h| h == host_name.trim()) {
            Some(host) => {
                picker.pool_host = Some(host);
                self.pending_pool_fetch.push((host, what));
            }
            None => picker.load = crate::ui::picker::PickerLoad::Failed(format!("unknown host '{}'", host_name.trim())),
        }
    }

    /// Lists the host's pools or datasets on a worker thread and reports them as `WorkerMsg::PoolNames`.
    pub fn fetch_pool_names(&mut self, runner: Arc<dyn CommandRunner>, host: usize, what: crate::ui::picker::LiveNames) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = match what {
                crate::ui::picker::LiveNames::Pools => crate::zfs::pool::list_pools(runner.as_ref()).map(|v| v.into_iter().map(|p| p.name).collect()),
                crate::ui::picker::LiveNames::Datasets => crate::zfs::dataset::list_datasets(runner.as_ref()).map(|v| v.into_iter().map(|d| d.name).collect()),
            };
            let _ = tx.send(WorkerMsg::PoolNames(host, result));
        });
    }

    /// Reports a failed pool lookup without running one (the host index no longer resolves).
    pub fn fail_pool_fetch(&self, host: usize, err: crate::error::AppError) {
        let _ = self.tx.send(WorkerMsg::PoolNames(host, Err(err)));
    }

    /// Runs the account lookup on a worker thread and reports it as
    /// `WorkerMsg::Accounts`. Called by the event loop for each queued host
    /// (`App` cannot resolve a host index to a runner on its own).
    pub fn fetch_accounts(&mut self, runner: Arc<dyn CommandRunner>, host: usize) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::accounts::fetch_accounts(runner.as_ref());
            let _ = tx.send(WorkerMsg::Accounts(host, result));
        });
    }

    /// Reports a failed account lookup without running one (e.g. the host
    /// index no longer resolves), through the same channel a worker uses so
    /// the picker shows Failed and the in-flight marker clears.
    pub fn fail_account_fetch(&self, host: usize, err: crate::error::AppError) {
        let _ = self.tx.send(WorkerMsg::Accounts(host, Err(err)));
    }

    fn focused_form_value(&self) -> String {
        match &self.modal {
            Some(Modal::Form { fields, focused, .. }) => fields.get(*focused).map(|f| f.value.clone()).unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Handles a key while `self.picker` is open. Never returns a
    /// `PendingAction` -- a picker only ever edits the still-open
    /// form's focused field, it never submits anything itself.
    pub fn handle_picker_key(&mut self, key: KeyCode) {
        let Some(mut picker) = self.picker.take() else { return };
        if picker.load != crate::ui::picker::PickerLoad::Ready {
            // Loading / Failed: nothing to filter or pick; only Esc leaves.
            if key != KeyCode::Esc {
                self.picker = Some(picker);
            }
            return;
        }
        let filter_lower = picker.filter.to_lowercase();
        let filtered: Vec<String> = picker.values.iter().filter(|v| v.to_lowercase().contains(&filter_lower)).cloned().collect();
        match key {
            KeyCode::Esc => {}
            KeyCode::Up => {
                picker.selected = picker.selected.saturating_sub(1);
                self.picker = Some(picker);
            }
            KeyCode::Down => {
                if !filtered.is_empty() && picker.selected + 1 < filtered.len() {
                    picker.selected += 1;
                }
                self.picker = Some(picker);
            }
            KeyCode::Char(' ') if picker.multi_select => {
                if let Some(value) = filtered.get(picker.selected) {
                    if !picker.checked.insert(value.clone()) {
                        picker.checked.remove(value);
                    }
                }
                self.picker = Some(picker);
            }
            KeyCode::Char(' ') => {
                self.picker = Some(picker);
            }
            KeyCode::Enter => {
                if picker.multi_select {
                    let mut vals: Vec<String> = picker.checked.iter().cloned().collect();
                    vals.sort();
                    let new_value = vals.join(",");
                    if let Some(Modal::Form { fields, focused, .. }) = &mut self.modal {
                        if let Some(field) = fields.get_mut(*focused) {
                            field.value = new_value;
                        }
                    }
                } else if let Some(value) = filtered.get(picker.selected).cloned() {
                    if let Some(Modal::Form { fields, focused, .. }) = &mut self.modal {
                        if let Some(field) = fields.get_mut(*focused) {
                            field.value = crate::ui::jobs::insert_text(&value).to_string();
                        }
                    }
                } else {
                    self.picker = Some(picker);
                }
            }
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.selected = 0;
                self.picker = Some(picker);
            }
            KeyCode::Char(c) => {
                picker.filter.push(c);
                picker.selected = 0;
                self.picker = Some(picker);
            }
            _ => {
                self.picker = Some(picker);
            }
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

    /// See `refresh_pools` for why this queues rather than drops a request while in flight.
    pub fn refresh_bootenvs(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::BootEnvs, host) {
            self.pending_refresh.insert((Section::BootEnvs, host));
            return;
        }
        self.begin_refresh(Section::BootEnvs, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::bootenv::list(runner.as_ref());
            let _ = tx.send(WorkerMsg::BootEnvs(host, result));
        });
    }

    pub fn selected_bootenv(&self) -> Option<&crate::zfs::bootenv::BootEnv> {
        self.bootenvs_table.selected().and_then(|i| self.bootenvs.get(i))
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

    /// Spawns the SSH reachability probe for a candidate new host and
    /// returns immediately -- the modal is already closed by the
    /// caller before this runs, matching every other slow mutation in
    /// this app (the outcome is reported later via `status_message`,
    /// not by keeping the form open across the async gap). Uses
    /// `zfs::run_checked` (not a bare `SshRunner::run`) specifically
    /// because `SshRunner::run` alone does not check the remote
    /// command's exit status -- an unreachable host or a rejected key
    /// still returns `Ok` from the raw call, just with a non-zero
    /// `CommandOutput::status`.
    pub fn begin_add_host(&mut self, name: String, target: String) {
        self.status_message = Some(format!("Connecting to '{target}'..."));
        let tx = self.tx.clone();
        let name_for_thread = name;
        let target_for_thread = target;
        std::thread::spawn(move || {
            let runner = crate::exec::ssh::SshRunner::new(target_for_thread.clone());
            let result = crate::zfs::run_checked(&runner, "true", &[], &format!("ssh {target_for_thread}")).map(|_| ());
            let _ = tx.send(WorkerMsg::HostProbe(name_for_thread, target_for_thread, result));
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
                    let quiet = self.end_refresh(Section::Pools, host);
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
                            let previous = self.pools_table.selected().and_then(|i| self.pools.get(i)).map(|p| p.name.clone());
                            self.pools = pools;
                            pin_selection(&mut self.pools_table, previous, &self.pools, |p| p.name.clone());
                            self.clear_status_after_refresh(quiet);
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
                        Err(e) => self.set_error(e.to_string()),
                    }
                }
                WorkerMsg::Datasets(host, result) => {
                    let quiet = self.end_refresh(Section::Datasets, host);
                    let had_pending = self.pending_refresh.remove(&(Section::Datasets, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::Datasets);
                    }
                    match result {
                        Ok(datasets) => {
                            let previous = self.datasets_table.selected().and_then(|i| self.datasets.get(i)).map(|d| d.name.clone());
                            self.datasets = datasets;
                            pin_selection(&mut self.datasets_table, previous, &self.datasets, |d| d.name.clone());
                            self.clear_status_after_refresh(quiet);
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
                        Err(e) => self.set_error(e.to_string()),
                    }
                }
                WorkerMsg::Snapshots(host, result) => {
                    let quiet = self.end_refresh(Section::Snapshots, host);
                    let had_pending = self.pending_refresh.remove(&(Section::Snapshots, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::Snapshots);
                    }
                    match result {
                        Ok(snapshots) => {
                            let previous = self.snapshots_table.selected().and_then(|i| self.snapshots.get(i)).map(|s| (s.dataset.clone(), s.name.clone()));
                            self.snapshots = snapshots;
                            pin_selection(&mut self.snapshots_table, previous, &self.snapshots, |s| (s.dataset.clone(), s.name.clone()));
                            self.clear_status_after_refresh(quiet);
                        }
                        Err(e) => self.set_error(e.to_string()),
                    }
                }
                WorkerMsg::Disks(host, result) => {
                    let quiet = self.end_refresh(Section::Disks, host);
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
                            let previous = self.disks_table.selected().and_then(|i| self.disks.get(i)).map(|d| d.name.clone());
                            self.disks = disks;
                            pin_selection(&mut self.disks_table, previous, &self.disks, |d| d.name.clone());
                            self.clear_status_after_refresh(quiet);
                        }
                        Err(e) => self.set_error(e.to_string()),
                    }
                }
                WorkerMsg::Shares(host, result) => {
                    let quiet = self.end_refresh(Section::Shares, host);
                    let had_pending = self.pending_refresh.remove(&(Section::Shares, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::Shares);
                    }
                    match result {
                        Ok(snapshot) => {
                            let previous = self.shares_table.selected().and_then(|i| self.shares.get(i)).map(share_identity);
                            self.shares = snapshot.shares;
                            pin_selection(&mut self.shares_table, previous, &self.shares, share_identity);
                            self.nfs_available = Some(snapshot.nfs_available);
                            self.samba_available = Some(snapshot.samba_available);
                            self.shares_loaded = true;
                            self.clear_status_after_refresh(quiet);
                            if let Some(e) = snapshot.smb_error {
                                self.set_error(format!("SMB shares unavailable: {e}"));
                            }
                        }
                        Err(e) => self.set_error(e.to_string()),
                    }
                }
                WorkerMsg::BootEnvs(host, result) => {
                    let quiet = self.end_refresh(Section::BootEnvs, host);
                    let had_pending = self.pending_refresh.remove(&(Section::BootEnvs, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::BootEnvs);
                    }
                    match result {
                        Ok(envs) => {
                            let previous = self.selected_bootenv().map(|b| b.name.clone());
                            self.bootenvs = envs;
                            pin_selection(&mut self.bootenvs_table, previous, &self.bootenvs, |b| b.name.clone());
                            self.bootenvs_unavailable = None;
                            self.clear_status_after_refresh(quiet);
                        }
                        Err(e) => {
                            // Not `set_error`: a host without bectl would otherwise raise a fresh
                            // error every auto-refresh. The title carries the reason instead.
                            self.bootenvs.clear();
                            self.bootenvs_table.select(None);
                            self.bootenvs_unavailable = Some(e.to_string());
                        }
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
                        Err(e) => self.set_error(e.to_string()),
                    }
                }
                WorkerMsg::Metrics(host, target, result) => {
                    if host == self.selected_host {
                        self.metrics.apply(&target, *result, Instant::now());
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
                        Err(e) => self.set_error(e.to_string()),
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
                        Err(e) => self.set_error(e.to_string()),
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
                        Err(e) => self.set_error(e.to_string()),
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
                        Err(e) => self.set_error(e.to_string()),
                    }
                }
                WorkerMsg::Accounts(host, result) => {
                    self.account_fetch_in_flight.remove(&host);
                    let current = self.focused_form_value();
                    let waiting = |p: &PickerState| p.load == crate::ui::picker::PickerLoad::Loading && p.account.map(|(h, _)| h) == Some(host);
                    match result {
                        Ok(accounts) => {
                            if let Some(p) = self.picker.as_mut().filter(|p| waiting(p)) {
                                crate::ui::picker::fill_account_picker(p, &accounts, &current);
                            }
                            self.account_cache.insert(host, (Instant::now(), accounts));
                        }
                        Err(e) => {
                            if let Some(p) = self.picker.as_mut().filter(|p| waiting(p)) {
                                p.load = crate::ui::picker::PickerLoad::Failed(e.to_string());
                            }
                        }
                    }
                }
                WorkerMsg::PoolNames(host, result) => {
                    let current = self.focused_form_value();
                    let waiting = |p: &PickerState| p.load == crate::ui::picker::PickerLoad::Loading && p.pool_host == Some(host);
                    if let Some(p) = self.picker.as_mut().filter(|p| waiting(p)) {
                        match result {
                            Ok(names) => {
                                p.selected = names.iter().position(|n| *n == current.trim()).unwrap_or(0);
                                p.note = names.is_empty().then(|| "no pools on this host".to_string());
                                p.values = names;
                                p.load = crate::ui::picker::PickerLoad::Ready;
                            }
                            Err(e) => p.load = crate::ui::picker::PickerLoad::Failed(e.to_string()),
                        }
                    }
                }
                WorkerMsg::HostProbe(name, target, result) => {
                    self.pending_probes.remove(&name.to_lowercase());
                    match result {
                        Ok(()) => {
                            self.pending_new_hosts.push((name, target));
                            self.status_message = None;
                        }
                        Err(e) => {
                            self.set_error(format!("Could not add host '{name}': {e}"));
                        }
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
                        Err(e) => self.set_error(e.to_string()),
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
/// host. SMB shares route to `smb_edit_modal` (managed smb4.conf block).
/// The new/edit scrub-job form. `existing` is the job being edited; for a new job `pool`
/// is what to pre-fill (a pool picked in the Pools section, or blank).
impl App {
    fn job_form(&self, existing: Option<crate::jobs::Job>, pool: &str) -> Modal {
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
    fn snapshot_job_form(&self, existing: Option<crate::jobs::Job>, dataset: &str) -> Modal {
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
    fn replicate_job_form(&self, existing: Option<crate::jobs::Job>) -> Modal {
        let yn = |b: bool| if b { "y" } else { "n" }.to_string();
        let (title, host, dataset, prefix, dest_host, dest, force, schedule, catch_up) = match &existing {
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
            ),
            None => ("New replication job".to_string(), self.active_host_name().to_string(), String::new(), crate::jobs::DEFAULT_PREFIX.to_string(), String::new(), String::new(), false, "30 * * * *".to_string(), true),
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
            ],
            focused: 0,
            description: Some("Sends the newest <prefix>-<timestamp> snapshot, incrementally. The destination is received unmounted and rolled back only if Force is y.".to_string()),
            kind: FormKind::ReplicateJob { hosts: self.hosts.clone(), existing: existing.map(Box::new), jobs: self.jobs_view.jobs.clone() },
        }
    }
}

/// Turns a submitted replication form into a save, or says what is wrong. The job is validated
/// by `jobs::resolve`, exactly as the daemon will validate it, plus the shared-destination rule.
fn build_replicate_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
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
    let to_file = dest_host == crate::jobs::DEST_FILE;
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
        force,
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
fn build_snapshot_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
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
            force: false,
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
fn build_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
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
            force: false,
        },
    };
    Ok(PendingAction { host: 0, section: Section::Jobs, call: MutationCall::SaveJob { state_dir: state_dir.to_path_buf(), job: Box::new(job) } })
}

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
        crate::zfs::share::Share::Smb { dataset, path, .. } => smb_edit_modal(Some(share), dataset, path, host),
    }
}

/// Builds the `PendingAction` for a submitted form, mapping its fields
/// positionally per `FormKind`'s documented field order. Returns `None`
/// if a required field is empty — the caller keeps the form modal open
/// in that case rather than losing the user's input.
/// The text of form field `index`; a missing field reads as empty, which every caller
/// already treats as "not filled in" (forms are built with a fixed field count per kind).
fn field_value(fields: &[FormField], index: usize) -> &str {
    fields.get(index).map_or("", |f| f.value.as_str())
}

fn build_pending_action(kind: &FormKind, fields: &[FormField]) -> Option<PendingAction> {
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
        FormKind::AddVdev { host, pool } => {
            let vdev_spec_raw = field_value(fields, 0).trim();
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
            let pool = field_value(fields, 0).trim();
            if pool.is_empty() {
                return None;
            }
            let redundancy = field_value(fields, 1).trim();
            Some(PendingAction {
                host: *host,
                section: Section::Vdevs,
                call: MutationCall::AddVdev { pool: pool.to_string(), vdev_spec: build_vdev_spec_from_selection(redundancy, disks) },
            })
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
            if name.is_empty() {
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
        FormKind::AddHost | FormKind::ShareSmb { .. } | FormKind::Job { .. } | FormKind::SnapshotJob { .. } | FormKind::ReplicateJob { .. } => None,
    }
}

fn smb_edit_modal(existing: Option<&crate::zfs::share::Share>, dataset: &str, path: &str, host: usize) -> Modal {
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

fn build_smb_action(kind: &FormKind, fields: &[FormField], shares: &[crate::zfs::share::Share]) -> std::result::Result<PendingAction, String> {
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

/// A share is identified by its dataset and protocol (a dataset can have both an NFS and an SMB share).
fn share_identity(share: &crate::zfs::share::Share) -> (String, bool) {
    match share {
        crate::zfs::share::Share::Nfs { dataset, .. } => (dataset.clone(), false),
        crate::zfs::share::Share::Smb { dataset, .. } => (dataset.clone(), true),
    }
}

/// After a list is replaced, keeps the selection on the same item (by identity) even if
/// it moved; falls back to clamping the index when the item is gone or nothing was selected.
/// This is what makes a background refresh safe: a keypress still acts on the row the
/// user was looking at, never on whatever slid into its index.
fn pin_selection<T, K: PartialEq>(state: &mut TableState, previous: Option<K>, items: &[T], key: impl Fn(&T) -> K) {
    if let Some(previous) = previous {
        if let Some(index) = items.iter().position(|item| key(item) == previous) {
            state.select(Some(index));
            return;
        }
    }
    clamp_table_selection(state, items.len());
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
  F2               in a form, on a field with known values (Permissions, Flags, Type, Recipient, Valid user, Root user mapping, a pool property with fixed values such as failmode or autoexpand, a from-disks Redundancy, a job's Host, Pool or Dataset, a replication's Destination host, and Schedule): open a picker (type filter, Enter OK, Esc cancel; Space toggles in multi-select ones)
  Esc              dismiss a status message, back out of a focused pane, or cancel a modal
  ?                this Help screen
  !                open a shell on the selected host (remote hosts: your unprivileged ssh login); exit returns to zfsctl
  A                About
  q                quit

Hosts
  Up / Down        select the active host
  n                add a new host (prompts for Name and SSH target)

Pools
  Enter            drill into vdev/disk tree
  Right / Left     focus/unfocus the properties pane
  n / d            create / destroy a pool
  s / R            scrub / resilver (y/n confirm)
  J                schedule a recurring scrub of the selected pool (opens the job form)
  J                (in Datasets) schedule recurring snapshots of the selected dataset

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
  M                create/edit an SMB share
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
  Enter            edit the selected share
  d                remove the selected share

Jobs
  Up / Down        select a scheduled job
  Enter            edit the job (host, pool, schedule, catch up)
  n                new scrub job (or press J on a pool in Pools)
  N                new snapshot job (or press J on a dataset in Datasets)
  R                new replication job
  c                cancel the selected replication while it is running
  Space            enable / disable the job
  d                delete the job (y/n; its past runs stay in the history)

Press any key to close."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smb(dataset: &str) -> crate::zfs::share::Share {
        crate::zfs::share::Share::Smb {
            dataset: dataset.to_string(),
            name: "media".to_string(),
            path: format!("/{dataset}"),
            valid_user: Some("carol".to_string()),
            read_only: false,
            guest_ok: false,
        }
    }
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
        assert_eq!(app.current_section(), Section::Jobs);
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
        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
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
        app.pool_scan_status_shown_at = Some(Instant::now().checked_sub(Duration::from_secs(7)).unwrap());
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
        app.pool_scan_status_shown_at = Some(Instant::now().checked_sub(Duration::from_secs(999)).unwrap());
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
        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
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
        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
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
    fn handle_modal_key_form_backtab_moves_focus_backward_and_wraps() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char('n')); // CreatePool form, 2 fields, focused = 0
        app.handle_modal_key(KeyCode::BackTab);
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 1, "BackTab from the first field must wrap to the last"),
            other => panic!("expected Form modal, got {other:?}"),
        }
        app.handle_modal_key(KeyCode::BackTab);
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 0),
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_modal_key_form_up_and_down_move_focus_the_same_as_backtab_and_tab() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Char('n')); // CreatePool form, 2 fields, focused = 0
        app.handle_modal_key(KeyCode::Down);
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 1),
            other => panic!("expected Form modal, got {other:?}"),
        }
        app.handle_modal_key(KeyCode::Up);
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 0),
            other => panic!("expected Form modal, got {other:?}"),
        }
        app.handle_modal_key(KeyCode::Up);
        match &app.modal {
            Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 1, "Up from the first field must wrap to the last, same as BackTab"),
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn failed_submission_resets_focus_to_the_first_field_so_an_invalid_earlier_field_is_visible_without_tabbing_through_everything() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        enter_datasets_acl_tab(&mut app);
        app.acl_entries_for = Some("/tank/data".to_string());
        app.handle_key(KeyCode::Char('n')); // AddAclEntry form, 4 fields: Recipient, Permissions, Flags, Type
        for c in "fb".chars() {
            app.handle_modal_key(KeyCode::Char(c)); // bad Recipient -- needs "user:fb"
        }
        app.handle_modal_key(KeyCode::Tab);
        for c in "read_data".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab); // -> Flags
        app.handle_modal_key(KeyCode::Tab); // -> Type (focused = 3)
        app.handle_modal_key(KeyCode::Enter); // fails: bad Recipient
        match &app.modal {
            Some(Modal::Form { focused, .. }) => {
                assert_eq!(*focused, 0, "a failed submission should land back on the first field, not stay on the last one that happened to be focused");
            }
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
        assert_eq!(app.current_section(), Section::Jobs, "must wrap backward from the first section to the last");
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
        app.disks = vec![crate::zfs::disk::DiskInfo { name: "da0".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available, ..Default::default() }];
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
                ..Default::default()
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
                ..Default::default()
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
        app.disks = vec![crate::zfs::disk::DiskInfo { name: "da0".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available, ..Default::default() }];
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
                ..Default::default()
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
            crate::zfs::disk::DiskInfo { name: "da0".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available, ..Default::default() },
            crate::zfs::disk::DiskInfo { name: "da1".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available, ..Default::default() },
        ];
        app.handle_key(KeyCode::Down);
        assert_eq!(app.disks_table.selected(), Some(0));
        app.handle_key(KeyCode::Down);
        assert_eq!(app.disks_table.selected(), Some(1));
    }

    fn identity_disk(name: &str, model: &str, serial: &str) -> crate::zfs::disk::DiskInfo {
        crate::zfs::disk::DiskInfo {
            name: name.to_string(),
            size: 4_000_787_030_016,
            model: Some(model.to_string()),
            serial: Some(serial.to_string()),
            media: crate::zfs::disk::DiskMedia::Hdd(5400),
            ..Default::default()
        }
    }

    fn disk_form_description(key: char) -> String {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..4 {
            app.handle_key(KeyCode::Tab);
        } // -> Disks
        app.disks = vec![identity_disk("da0", "WDC WD40EFRX-68N32N0", "WD-WCC7K1234567"), identity_disk("da1", "Samsung SSD 860", "S3Z2NB0K123")];
        app.selected_disks.insert("da1".to_string());
        app.selected_disks.insert("da0".to_string());
        app.selected_disks.insert("gone9".to_string());
        app.handle_key(KeyCode::Char(key));
        match &app.modal {
            Some(Modal::Form { description: Some(d), fields, .. }) => {
                assert_eq!(fields.len(), 2, "fields unchanged");
                d.clone()
            }
            other => panic!("expected Form with description, got {other:?}"),
        }
    }

    #[test]
    fn disk_forms_list_each_selected_disks_identity_in_the_description() {
        for key in ['n', 'a'] {
            let d = disk_form_description(key);
            for want in ["WDC WD40EFRX-68N32N0", "WD-WCC7K1234567", "Samsung SSD 860", "S3Z2NB0K123"] {
                assert!(d.contains(want), "{key}: missing {want:?} in {d:?}");
            }
            let lines: Vec<&str> = d.lines().collect();
            assert_eq!(lines.len(), 3, "one line per selected disk: {d:?}");
            assert!(lines[0].starts_with("da0") && lines[1].starts_with("da1"));
            assert_eq!(lines[2], "gone9", "unknown disk shows just its name");
        }
    }

    fn app_with_selected_disks(disks: Vec<crate::zfs::disk::DiskInfo>) -> App {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..4 {
            app.handle_key(KeyCode::Tab);
        } // -> Disks
        for d in &disks {
            app.selected_disks.insert(d.name.clone());
        }
        app.disks = disks;
        app
    }

    #[test]
    fn disk_form_description_caps_at_six_lines_plus_more_selected() {
        let disks: Vec<_> = (0..8).map(|i| identity_disk(&format!("da{i}"), "Model", "SN")).collect();
        let app = app_with_selected_disks(disks);
        let names: Vec<String> = (0..8).map(|i| format!("da{i}")).collect();
        let d = app.selected_disk_identities(&names);
        let lines: Vec<&str> = d.lines().collect();
        assert_eq!(lines.len(), 7, "{d:?}");
        assert!(lines[5].starts_with("da5") && lines[6] == "+2 more selected", "{d:?}");
        let six: Vec<String> = names[..6].to_vec();
        assert_eq!(app.selected_disk_identities(&six).lines().count(), 6, "exactly six: no suffix");
    }

    #[test]
    fn disk_form_description_strips_escapes_newlines_and_bidi_from_model_and_serial() {
        let evil = identity_disk("da0", "Ev\u{1b}[31mil\nModel\u{202e}X", "SN\u{7}\u{9b}1");
        for key in ['n', 'a'] {
            let mut app = app_with_selected_disks(vec![evil.clone()]);
            app.handle_key(KeyCode::Char(key));
            let Some(Modal::Form { description: Some(d), .. }) = &app.modal else { panic!("no form") };
            assert_eq!(d.lines().count(), 1, "{d:?}");
            for bad in ['\u{1b}', '\n', '\u{202e}', '\u{7}', '\u{9b}'] {
                assert!(!d.contains(bad), "{bad:?} leaked into {d:?}");
            }
            assert!(d.contains("Model") && d.contains("SN"));
        }
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
                .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\n")
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
        assert!(app.shares_loaded);
    }

    #[test]
    fn shares_result_for_a_stale_host_is_discarded() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.tx
            .send(WorkerMsg::Shares(
                1,
                Ok(crate::zfs::share::SharesSnapshot {
                    shares: vec![smb("tank/data")],
                    nfs_available: true,
                    samba_available: true,
                    smb_error: None,
                }),
            ))
            .unwrap();
        app.drain_worker_messages();
        assert!(app.shares.is_empty(), "a result for a host that isn't selected must not populate the pane");
    }

    #[test]
    fn switching_host_clears_shares_state() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.shares = vec![smb("tank/data")];
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
        assert!(!app.shares_loaded);
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

    fn datasets_app_with_mount(mountpoint: Option<&str>) -> App {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets = vec![Dataset { name: "tank/media".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: mountpoint.map(str::to_string) }];
        app.datasets_table.select(Some(0));
        app.shares_loaded = true;
        app.samba_available = Some(true);
        app
    }

    #[test]
    fn shift_m_on_datasets_opens_a_blank_smb_form_for_an_unshared_dataset() {
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.handle_key(KeyCode::Char('M'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(*kind, FormKind::ShareSmb { host: 0, dataset: "tank/media".into(), path: "/tank/media".into(), existing_user: None });
                assert_eq!(fields.len(), 5);
                assert_eq!(fields[4].value, "n");
                assert_eq!(fields[0].value, "media", "suggests the dataset's last path component");
                assert_eq!(fields[1].value, "");
            }
            other => panic!("expected Form, got {other:?}"),
        }
    }

    #[test]
    fn shift_m_prefills_from_the_existing_managed_share() {
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.shares = vec![smb("tank/media")];
        app.handle_key(KeyCode::Char('M'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields[0].value, "media");
                assert_eq!(fields[1].value, "carol");
                assert_eq!(fields[2].value, "", "the password is never pre-filled");
                assert_eq!(fields[3].value, "n");
                assert!(matches!(kind, FormKind::ShareSmb { existing_user: Some(u), .. } if u == "carol"));
            }
            other => panic!("expected Form, got {other:?}"),
        }
    }

    #[test]
    fn shift_m_is_refused_without_a_mountpoint_before_shares_load_or_when_smbd_is_down() {
        let mut app = datasets_app_with_mount(None);
        app.handle_key(KeyCode::Char('M'));
        assert!(app.modal.is_none());
        assert_eq!(app.status_message.as_deref(), Some("'tank/media' has no mounted mountpoint to share"));
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.shares_loaded = false;
        app.handle_key(KeyCode::Char('M'));
        assert!(app.modal.is_none());
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Press 'r' in the Shares section")));
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.samba_available = Some(false);
        app.handle_key(KeyCode::Char('M'));
        assert!(app.modal.is_none());
        assert_eq!(app.status_message.as_deref(), Some("Samba (smbd) is not running on this host"));
    }

    #[test]
    fn shift_n_and_m_with_no_dataset_selected_are_inert() {
        for key in ['N', 'M'] {
            let mut app = datasets_app_with_mount(Some("/tank/media"));
            app.datasets_table.select(None);
            app.handle_key(KeyCode::Char(key));
            assert!(app.modal.is_none() && app.status_message.is_none(), "{key}");
        }
    }

    /// Feeds each value then Enter through `handle_modal_key` (Enter
    /// advances; the 4th submits); returns what the final Enter produced.
    fn type_into_smb_form(app: &mut App, values: [&str; 5]) -> Option<PendingAction> {
        let mut last = None;
        for v in values {
            for c in v.chars() {
                // '\u{8}' stands for Backspace (the read-only and guest fields are pre-filled with "n")
                app.handle_modal_key(if c == '\u{8}' { KeyCode::Backspace } else { KeyCode::Char(c) });
            }
            last = app.handle_modal_key(KeyCode::Enter);
        }
        last
    }

    fn open_blank_smb_form() -> App {
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.handle_key(KeyCode::Char('M'));
        // clear the suggested name so each test types its own
        for _ in 0..5 {
            app.handle_modal_key(KeyCode::Backspace);
        }
        app
    }

    #[test]
    fn submitting_a_valid_smb_form_builds_a_set_smb_share_action() {
        let mut app = open_blank_smb_form();
        let action = type_into_smb_form(&mut app, ["media", "carol", " pw 1 ", "\u{8}y", "n"]).expect("submitted");
        assert_eq!(
            action.call,
            MutationCall::SetSmbShare {
                share: crate::zfs::smb::SmbShare { name: "media".into(), dataset: "tank/media".into(), path: "/tank/media".into(), valid_user: Some("carol".into()), read_only: true, guest_ok: false },
                password: Some(crate::ui::modal::Secret(" pw 1 ".to_string())),
            },
            "the password is kept verbatim, not trimmed"
        );
        assert_eq!(action.section, Section::Shares);
    }

    #[test]
    fn smb_form_validation_errors_keep_the_form_open_with_a_specific_message() {
        let cases: [([&str; 5], &str); 6] = [
            (["", "", "", "n", "n"], "required"),
            (["bad name", "", "", "n", "n"], "letters, digits"),
            (["media", "-evil", "pw", "n", "n"], "valid Unix username"),
            (["media", "carol", "", "n", "n"], "Samba password is required"),
            (["alice", "", "", "n", "n"], "already"),
            (["media", "carol", "pw", "n", "\u{8}y"], "Guest access can't be combined"),
        ];
        for (values, expect) in cases {
            let mut app = open_blank_smb_form();
            app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/other".into(), name: "alice".into(), path: "/x".into(), valid_user: None, read_only: true, guest_ok: false }];
            assert!(type_into_smb_form(&mut app, values).is_none());
            assert!(matches!(app.modal, Some(Modal::Form { .. })), "{values:?} must keep the form open");
            let msg = app.status_message.clone().unwrap_or_default();
            assert!(msg.contains(expect), "{values:?}: '{msg}' should contain '{expect}'");
        }
    }

    #[test]
    fn editing_with_a_blank_password_is_allowed_only_when_the_user_is_unchanged() {
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.shares = vec![smb("tank/media")]; // existing user: carol
        app.handle_key(KeyCode::Char('M'));
        for _ in 0..4 {
            assert!(app.handle_modal_key(KeyCode::Enter).is_none());
        }
        let action = app.handle_modal_key(KeyCode::Enter).expect("unchanged user + blank password is fine");
        assert!(matches!(action.call, MutationCall::SetSmbShare { password: None, .. }));

        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.shares = vec![smb("tank/media")];
        app.handle_key(KeyCode::Char('M'));
        app.handle_modal_key(KeyCode::Enter); // to Valid user
        for _ in 0..5 {
            app.handle_modal_key(KeyCode::Backspace);
        }
        for c in "bob".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        for _ in 0..4 {
            assert!(app.handle_modal_key(KeyCode::Enter).is_none());
        }
        assert!(matches!(app.modal, Some(Modal::Form { .. })), "changing the user requires a password");
    }

    #[test]
    fn a_blank_user_ignores_any_typed_password() {
        let mut app = open_blank_smb_form();
        let action = type_into_smb_form(&mut app, ["pub", "", "ignored", "n", "n"]).unwrap();
        assert!(matches!(action.call, MutationCall::SetSmbShare { password: None, ref share } if share.valid_user.is_none()));
    }

    #[test]
    fn a_default_blank_form_is_not_guest_and_builds_no_guest_ok() {
        let mut app = open_blank_smb_form();
        let action = type_into_smb_form(&mut app, ["any", "", "", "n", "n"]).unwrap();
        let MutationCall::SetSmbShare { ref share, password: None } = action.call else { panic!("{:?}", action.call) };
        assert!(!share.guest_ok);
        assert!(!crate::zfs::smb::render_with_shares("", std::slice::from_ref(share)).unwrap().contains("guest ok"));
    }

    #[test]
    fn guest_y_with_a_blank_user_builds_a_guest_share_ignoring_the_password() {
        let mut app = open_blank_smb_form();
        let action = type_into_smb_form(&mut app, ["pub", "", "ignored", "n", "\u{8}y"]).unwrap();
        assert!(matches!(action.call, MutationCall::SetSmbShare { password: None, ref share } if share.guest_ok && share.valid_user.is_none()));
    }

    #[test]
    fn editing_an_existing_guest_share_prefills_guest_y() {
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/media".into(), name: "pub".into(), path: "/tank/media".into(), valid_user: None, read_only: true, guest_ok: true }];
        app.handle_key(KeyCode::Char('M'));
        let Some(Modal::Form { fields, .. }) = &app.modal else { panic!("expected form") };
        assert_eq!(fields.len(), 5);
        assert_eq!(fields[4].value, "y");
    }

    #[test]
    fn smb_form_warns_that_a_password_replaces_the_users_samba_password() {
        let modal = smb_edit_modal(None, "tank/data", "/tank/data", 0);
        let Modal::Form { description: Some(d), .. } = modal else { panic!("expected form with description") };
        assert!(d.contains("replaces"), "{d}");
    }

    #[test]
    fn enter_on_an_smb_share_row_opens_its_prefilled_form_using_the_stanza_path() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..5 {
            app.handle_key(KeyCode::Tab);
        } // -> Shares
        app.shares = vec![smb("tank/data")];
        app.shares_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        assert!(matches!(&app.modal, Some(Modal::Form { kind: FormKind::ShareSmb { path, .. }, .. }) if path == "/tank/data"));
    }

    #[test]
    fn handle_key_d_on_smb_share_row_opens_a_remove_confirm_for_the_managed_share() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..5 {
            app.handle_key(KeyCode::Tab);
        }
        app.shares = vec![smb("tank/data")];
        app.shares_table.select(Some(0));
        app.handle_key(KeyCode::Char('d'));
        match &app.modal {
            Some(Modal::Confirm { action, .. }) => assert_eq!(action.call, MutationCall::RemoveSmbShare { dataset: "tank/data".to_string() }),
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    #[test]
    fn an_smb_error_in_the_shares_snapshot_sets_an_error_status_but_keeps_the_nfs_shares() {
        let mut app = App::new(vec!["local".to_string()]);
        let nfs = crate::zfs::share::Share::Nfs { dataset: "tank/n".into(), raw: "on".into(), networks: vec![], hosts: vec![], read_only: false, root: None };
        app.tx
            .send(WorkerMsg::Shares(
                0,
                Ok(crate::zfs::share::SharesSnapshot { shares: vec![nfs], nfs_available: true, samba_available: true, smb_error: Some("boom".into()) }),
            ))
            .unwrap();
        app.drain_worker_messages();
        assert_eq!(app.shares.len(), 1, "NFS shares still populated");
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("boom")));
        assert!(app.status_is_error());
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
        assert_eq!(app.status_message, Some("NFS server not available on this host (nfsd is not running)".to_string()));
    }

    #[test]
    fn handle_key_shift_n_with_unloaded_shares_names_the_fix() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Char('N'));
        assert!(app.modal.is_none());
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Press 'r' in the Shares section")));
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

    #[test]
    fn handle_key_n_on_hosts_section_opens_add_host_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('n'));
        match &app.modal {
            Some(Modal::Form { title, fields, kind, .. }) => {
                assert_eq!(title, "Add Host");
                assert_eq!(fields.len(), 2);
                assert_eq!(*kind, FormKind::AddHost);
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    fn open_grant_permission_form(app: &mut App) {
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Right); // Right focus, Properties tab
        app.handle_key(KeyCode::Char(']')); // Permissions tab
        app.handle_key(KeyCode::Char('n')); // open Grant Permission form
    }

    #[test]
    fn f2_on_a_vocabulary_field_opens_the_picker_without_touching_the_form() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app);
        app.handle_modal_key(KeyCode::Tab); // -> Scope
        app.handle_modal_key(KeyCode::Tab); // -> Permissions (index 2)
        app.handle_modal_key(KeyCode::F(2));
        assert!(app.picker.is_some());
        match &app.modal {
            Some(Modal::Form { fields, focused, kind, .. }) => {
                assert_eq!(*focused, 2);
                assert_eq!(fields.len(), 3);
                assert_eq!(*kind, FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() });
            }
            other => panic!("expected the form to still be open, got {other:?}"),
        }
    }

    #[test]
    fn f2_on_a_field_with_no_vocabulary_is_a_no_op() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app); // focused = 0, Recipient (live account picker)
        app.handle_modal_key(KeyCode::Tab); // -> Scope, no vocabulary
        app.handle_modal_key(KeyCode::F(2));
        assert!(app.picker.is_none());
        assert!(app.modal.is_some());
    }

    #[test]
    fn typing_in_an_open_picker_filters_the_list_case_insensitively_and_resets_selection() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        app.handle_picker_key(KeyCode::Down); // move off 0 first, to prove filtering resets it
        for c in "SNAP".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        let picker = app.picker.as_ref().unwrap();
        assert_eq!(picker.selected, 0);
        assert!(picker.values.iter().filter(|v| v.to_lowercase().contains("snap")).count() >= 2); // snapshot, snapdir, snapdev, snapshot_limit
    }

    #[test]
    fn space_toggles_a_value_in_a_multi_select_picker() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        for c in "create".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        app.handle_picker_key(KeyCode::Char(' '));
        assert_eq!(app.picker.as_ref().unwrap().checked, std::collections::HashSet::from(["create".to_string()]));
        app.handle_picker_key(KeyCode::Char(' '));
        assert!(app.picker.as_ref().unwrap().checked.is_empty(), "a second space on the same highlighted value must untoggle it");
    }

    #[test]
    fn space_is_a_no_op_in_a_single_select_picker() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Char(']'));
        app.handle_key(KeyCode::Char(']')); // Acl tab
        app.acl_entries_for = Some("/tank/data".to_string());
        app.handle_key(KeyCode::Char('n')); // open Add ACL Entry form
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab); // -> Type (index 3)
        app.handle_modal_key(KeyCode::F(2));
        app.handle_picker_key(KeyCode::Char(' '));
        assert!(app.picker.as_ref().unwrap().checked.is_empty());
        assert!(app.picker.as_ref().unwrap().filter.is_empty(), "Space must not leak into the type-to-filter text");
    }

    #[test]
    fn enter_on_a_multi_select_picker_joins_checked_values_into_the_field_and_closes_it() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        for c in "create".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        app.handle_picker_key(KeyCode::Char(' '));
        for _ in 0.."create".len() {
            app.handle_picker_key(KeyCode::Backspace);
        }
        for c in "destroy".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        app.handle_picker_key(KeyCode::Char(' '));
        app.handle_picker_key(KeyCode::Enter);
        assert!(app.picker.is_none());
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[2].value, "create,destroy"),
            other => panic!("expected the form to still be open, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_a_single_select_picker_writes_the_highlighted_value_and_closes_it() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Char(']'));
        app.handle_key(KeyCode::Char(']'));
        app.acl_entries_for = Some("/tank/data".to_string());
        app.handle_key(KeyCode::Char('n'));
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        app.handle_picker_key(KeyCode::Down); // allow -> deny
        app.handle_picker_key(KeyCode::Enter);
        assert!(app.picker.is_none());
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[3].value, "deny"),
            other => panic!("expected the form to still be open, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_an_empty_filtered_single_select_picker_is_a_no_op() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Char(']'));
        app.handle_key(KeyCode::Char(']'));
        app.acl_entries_for = Some("/tank/data".to_string());
        app.handle_key(KeyCode::Char('n'));
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        for c in "nonexistent-type".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        app.handle_picker_key(KeyCode::Enter);
        assert!(app.picker.is_some(), "Enter against an empty filtered list must not close the picker");
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[3].value, "", "the field must be untouched"),
            other => panic!("expected the form to still be open, got {other:?}"),
        }
    }

    #[test]
    fn esc_cancels_the_picker_without_touching_the_field_or_the_form() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        for c in "existing".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::F(2));
        for c in "create".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        app.handle_picker_key(KeyCode::Char(' '));
        app.handle_picker_key(KeyCode::Esc);
        assert!(app.picker.is_none());
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[2].value, "existing", "Esc must leave the field exactly as it was before F2"),
            other => panic!("expected the form to still be open, got {other:?}"),
        }
    }

    #[test]
    fn up_and_down_clamp_within_the_filtered_list_without_panicking() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        app.handle_picker_key(KeyCode::Up); // already at 0, must not underflow
        assert_eq!(app.picker.as_ref().unwrap().selected, 0);
        for c in "xattr".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        // filtered list now has exactly 1 entry ("xattr" -- unique in
        // ZFS_ALLOW_PERMISSIONS; "read_xattr"/"write_xattr" only exist
        // in the separate ACL_PERMISSIONS vocabulary) -- Down must not
        // move past it
        app.handle_picker_key(KeyCode::Down);
        assert_eq!(app.picker.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn submitting_add_host_form_with_blank_name_keeps_form_open() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('n'));
        app.handle_modal_key(KeyCode::Tab); // move to SSH target field
        for c in "example.com".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(result, None);
        assert!(app.modal.is_some(), "blank Name must keep the form open");
    }

    #[test]
    fn submitting_add_host_form_with_blank_target_keeps_form_open() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('n'));
        for c in "newhost".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(result, None);
        assert!(app.modal.is_some(), "blank SSH target must keep the form open");
    }

    #[test]
    fn submitting_add_host_form_with_whitespace_only_name_keeps_form_open() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('n'));
        for c in "   ".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        for c in "example.com".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(result, None);
        assert!(app.modal.is_some(), "a whitespace-only name must be treated as blank");
    }

    #[test]
    fn submitting_add_host_form_with_case_insensitive_duplicate_name_keeps_form_open() {
        let mut app = App::new(vec!["local".to_string(), "fb16".to_string()]);
        app.handle_key(KeyCode::Char('n'));
        for c in "FB16".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        for c in "example.com".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(result, None);
        assert!(app.modal.is_some(), "a name differing only by case from an existing host must be rejected as a duplicate");
        assert!(app.status_message.is_some(), "a duplicate rejection must say why, per the spec's own testing section -- a silent re-show looks like nothing happened");
    }

    #[test]
    fn submitting_add_host_form_with_duplicate_of_local_keeps_form_open() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('n'));
        for c in "Local".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        for c in "example.com".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(result, None);
        assert!(app.modal.is_some(), "\"local\" (case-insensitively) must always be rejected as a duplicate");
        assert!(app.status_message.is_some());
    }

    #[test]
    fn submitting_valid_add_host_form_closes_modal_and_sets_connecting_status() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('n'));
        for c in "newhost".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        for c in "newhost.example.com".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(result, None);
        assert!(app.modal.is_none(), "a valid submission must close the form immediately, not wait for the async probe");
        assert_eq!(app.status_message, Some("Connecting to 'newhost.example.com'...".to_string()));
    }

    #[test]
    fn host_probe_success_queues_the_host_for_the_event_loop_to_apply() {
        // Queued, not pushed directly onto `self.hosts` here -- the
        // event loop is the one place that pushes into `app.hosts` and
        // `inventory.hosts` together (see `apply_pending_new_hosts` in
        // `ui/mod.rs`), so the two index-aligned lists can never drift
        // apart. A final review caught that an earlier single-`Option`
        // version of this signal silently dropped a second host whose
        // probe resolved in the same tick as the first's.
        let mut app = App::new(vec!["local".to_string()]);
        app.tx.send(WorkerMsg::HostProbe("newhost".to_string(), "newhost.example.com".to_string(), Ok(()))).unwrap();
        app.drain_worker_messages();
        assert!(!app.hosts.contains(&"newhost".to_string()), "pushing onto app.hosts happens in the event loop, not here");
        assert_eq!(app.pending_new_hosts, vec![("newhost".to_string(), "newhost.example.com".to_string())]);
    }

    #[test]
    fn host_probe_success_queues_every_host_even_when_several_resolve_before_one_drain() {
        let mut app = App::new(vec!["local".to_string()]);
        app.tx.send(WorkerMsg::HostProbe("hosta".to_string(), "hosta.example.com".to_string(), Ok(()))).unwrap();
        app.tx.send(WorkerMsg::HostProbe("hostb".to_string(), "hostb.example.com".to_string(), Ok(()))).unwrap();
        app.drain_worker_messages();
        assert_eq!(
            app.pending_new_hosts,
            vec![("hosta".to_string(), "hosta.example.com".to_string()), ("hostb".to_string(), "hostb.example.com".to_string())]
        );
    }

    #[test]
    fn host_probe_failure_sets_status_message_and_touches_nothing_else() {
        let mut app = App::new(vec!["local".to_string()]);
        let before = app.hosts.clone();
        app.tx
            .send(WorkerMsg::HostProbe(
                "newhost".to_string(),
                "newhost.example.com".to_string(),
                Err(crate::error::AppError::CommandFailed {
                    cmd: "ssh newhost.example.com".to_string(),
                    status: 255,
                    stderr: "Could not resolve hostname newhost.example.com".to_string(),
                }),
            ))
            .unwrap();
        app.drain_worker_messages();
        assert_eq!(app.hosts, before, "a failed probe must not add anything to the host list");
        assert!(app.pending_new_hosts.is_empty());
        assert!(app.status_message.as_ref().unwrap().contains("Could not resolve hostname"));
    }

    #[test]
    fn host_probe_failure_clears_the_in_flight_marker_so_the_name_can_be_retried() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('n'));
        for c in "newhost".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        for c in "newhost.example.com".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Enter); // starts probing "newhost"
        app.tx
            .send(WorkerMsg::HostProbe(
                "newhost".to_string(),
                "newhost.example.com".to_string(),
                Err(crate::error::AppError::CommandFailed { cmd: "ssh newhost.example.com".to_string(), status: 255, stderr: "unreachable".to_string() }),
            ))
            .unwrap();
        app.drain_worker_messages();

        // Retrying the same name after the failed probe cleared it
        // must be accepted, not rejected as still-in-flight.
        app.handle_key(KeyCode::Char('n'));
        for c in "newhost".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        for c in "newhost.example.com".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Enter);
        assert!(app.modal.is_none(), "a retry after the first probe failed must be accepted");
    }

    #[test]
    fn submitting_add_host_form_while_same_name_probe_already_in_flight_is_rejected_with_a_message() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('n'));
        for c in "newhost".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        for c in "newhost.example.com".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Enter); // first submission: starts probing "newhost", self.hosts doesn't have it yet
        assert!(app.modal.is_none());

        app.handle_key(KeyCode::Char('n'));
        for c in "NewHost".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        for c in "different.example.com".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(result, None);
        assert!(app.modal.is_some(), "a name already being probed (case-insensitively) must be rejected, not started a second time");
        assert!(app.status_message.as_ref().unwrap().to_lowercase().contains("newhost"), "the rejection must say which name collided");
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
    fn submitting_any_form_with_invalid_input_sets_a_status_message_explaining_why() {
        // A live session hit this directly: typing a bare username
        // ("fb") into the ACL Recipient field instead of "user:fb"
        // made Enter silently do nothing, with no visible explanation
        // -- indistinguishable from the UI being frozen. This is the
        // shared fallback every form's failed submission goes through
        // (not ACL-specific), so the fix and its test live here.
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        enter_datasets_acl_tab(&mut app);
        app.acl_entries_for = Some("/tank/data".to_string());
        app.handle_key(KeyCode::Char('n'));
        for c in "fb".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab); // -> Permissions
        for c in "read_data".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab); // -> Flags
        app.handle_modal_key(KeyCode::Tab); // -> Type
        let result = app.handle_modal_key(KeyCode::Enter);
        assert_eq!(result, None);
        assert!(app.modal.is_some(), "invalid input must keep the form open");
        assert!(app.status_message.is_some(), "a failed submission must say so, not silently do nothing");
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

    #[test]
    fn set_error_marks_the_current_status_message_as_an_error() {
        let mut app = App::new(vec!["local".to_string()]);
        assert!(!app.status_is_error());
        app.set_error("zpool destroy failed".to_string());
        assert_eq!(app.status_message.as_deref(), Some("zpool destroy failed"));
        assert!(app.status_is_error());
    }

    #[test]
    fn a_later_ordinary_message_is_not_styled_as_an_error() {
        let mut app = App::new(vec!["local".to_string()]);
        app.set_error("boom".to_string());
        app.status_message = Some("select a disk with Space first".to_string());
        assert!(!app.status_is_error(), "stale error flag must not recolor an unrelated message");
    }

    #[test]
    fn clearing_the_message_clears_the_error_state() {
        let mut app = App::new(vec!["local".to_string()]);
        app.set_error("boom".to_string());
        app.handle_key(crossterm::event::KeyCode::Esc);
        assert_eq!(app.status_message, None);
        assert!(!app.status_is_error());
    }

    fn accounts_fixture() -> crate::zfs::accounts::HostAccounts {
        use crate::zfs::accounts::{Account, HostAccounts};
        HostAccounts {
            users: vec![Account { name: "carol".into(), id: 1003 }, Account { name: "root".into(), id: 0 }],
            groups: vec![Account { name: "staff".into(), id: 1100 }],
            group_error: None,
        }
    }

    /// An App with an SMB share form open on its "Valid user" field (index 1) for `host`.
    fn smb_form_app(host: usize) -> App {
        let mut app = App::new(vec!["local".to_string(), "other".to_string()]);
        app.modal = Some(Modal::Form {
            title: "SMB share".to_string(),
            fields: vec![
                FormField { label: "Share name".into(), value: "media".into() },
                FormField { label: "Valid user".into(), value: "root".into() },
                FormField { label: "Password".into(), value: String::new() },
                FormField { label: "Read-only".into(), value: "n".into() },
                FormField { label: "Guest".into(), value: "n".into() },
            ],
            focused: 1,
            description: None,
            kind: FormKind::ShareSmb { host, dataset: "tank/a".into(), path: "/tank/a".into(), existing_user: None },
        });
        app
    }

    fn press_f2(app: &mut App) {
        app.handle_modal_key(KeyCode::F(2));
    }

    #[test]
    fn f2_on_an_account_field_with_no_cache_opens_a_loading_picker_and_queues_one_fetch() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        let p = app.picker.as_ref().expect("picker opens");
        assert_eq!(p.load, crate::ui::picker::PickerLoad::Loading);
        assert_eq!(app.pending_account_fetch, vec![1]);
        assert!(app.account_fetch_in_flight.contains_key(&1));
    }

    #[test]
    fn a_second_f2_while_the_fetch_is_in_flight_queues_nothing_more() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        app.picker = None;
        press_f2(&mut app);
        assert_eq!(app.pending_account_fetch, vec![1]);
        assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Loading);
    }

    #[test]
    fn a_stuck_in_flight_marker_older_than_30s_lets_the_next_f2_queue_a_fresh_fetch() {
        let mut app = smb_form_app(1);
        app.account_fetch_in_flight.insert(1, Instant::now().checked_sub(Duration::from_secs(31)).unwrap());
        press_f2(&mut app);
        assert_eq!(app.pending_account_fetch, vec![1]);
    }

    #[test]
    fn a_fresh_cache_opens_ready_immediately_and_queues_nothing() {
        let mut app = smb_form_app(1);
        app.account_cache.insert(1, (Instant::now(), accounts_fixture()));
        press_f2(&mut app);
        let p = app.picker.as_ref().unwrap();
        assert_eq!(p.load, crate::ui::picker::PickerLoad::Ready);
        assert_eq!(p.values, vec!["carol", "root"]);
        assert_eq!(p.selected, 1, "field text 'root' is preselected");
        assert!(app.pending_account_fetch.is_empty());
    }

    #[test]
    fn a_cache_older_than_60s_is_treated_as_missing() {
        let mut app = smb_form_app(1);
        app.account_cache.insert(1, (Instant::now().checked_sub(Duration::from_secs(61)).unwrap(), accounts_fixture()));
        press_f2(&mut app);
        assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Loading);
        assert_eq!(app.pending_account_fetch, vec![1]);
    }

    #[test]
    fn an_ok_result_fills_the_open_loading_picker_caches_and_clears_in_flight() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        app.tx.send(WorkerMsg::Accounts(1, Ok(accounts_fixture()))).unwrap();
        app.drain_worker_messages();
        let p = app.picker.as_ref().unwrap();
        assert_eq!(p.load, crate::ui::picker::PickerLoad::Ready);
        assert_eq!(p.values, vec!["carol", "root"]);
        assert!(app.account_cache.contains_key(&1));
        assert!(!app.account_fetch_in_flight.contains_key(&1));
    }

    #[test]
    fn an_err_result_marks_the_picker_failed_and_is_not_cached() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        let err = crate::error::AppError::Parse("ssh: connect timed out".to_string());
        app.tx.send(WorkerMsg::Accounts(1, Err(err))).unwrap();
        app.drain_worker_messages();
        assert!(matches!(&app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Failed(m) if m.contains("timed out")));
        assert!(app.account_cache.is_empty());
        assert!(app.account_fetch_in_flight.is_empty(), "a retry after Esc must be able to queue again");
    }

    #[test]
    fn a_result_for_another_host_never_touches_the_open_picker_but_is_cached() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        app.tx.send(WorkerMsg::Accounts(0, Ok(accounts_fixture()))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Loading);
        assert!(app.account_cache.contains_key(&0));
    }

    #[test]
    fn a_late_result_with_no_picker_open_is_cached_and_does_not_open_one() {
        let mut app = smb_form_app(1);
        app.tx.send(WorkerMsg::Accounts(1, Ok(accounts_fixture()))).unwrap();
        app.drain_worker_messages();
        assert!(app.picker.is_none());
        assert!(app.account_cache.contains_key(&1));
    }

    #[test]
    fn loading_and_failed_pickers_accept_only_esc() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        for key in [KeyCode::Char('x'), KeyCode::Down, KeyCode::Enter, KeyCode::Backspace] {
            app.handle_picker_key(key);
            assert!(app.picker.is_some(), "{key:?} must not close or alter a Loading picker");
            assert!(app.picker.as_ref().unwrap().filter.is_empty());
        }
        app.handle_picker_key(KeyCode::Esc);
        assert!(app.picker.is_none());
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[1].value, "root"),
            other => panic!("form must stay open: {other:?}"),
        }
    }

    #[test]
    fn a_failed_picker_accepts_only_esc() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        app.picker.as_mut().unwrap().load = crate::ui::picker::PickerLoad::Failed("x".into());
        for key in [KeyCode::Char('x'), KeyCode::Down, KeyCode::Enter, KeyCode::Backspace] {
            app.handle_picker_key(key);
            assert!(app.picker.is_some(), "{key:?} must not close a Failed picker");
            assert!(app.picker.as_ref().unwrap().filter.is_empty());
        }
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[1].value, "root"),
            other => panic!("form must stay open: {other:?}"),
        }
        app.handle_picker_key(KeyCode::Esc);
        assert!(app.picker.is_none());
    }

    #[test]
    fn a_cache_aged_59s_is_still_fresh() {
        let mut app = smb_form_app(1);
        let aged = Instant::now().checked_sub(Duration::from_secs(59)).expect("monotonic clock far enough along");
        app.account_cache.insert(1, (aged, accounts_fixture()));
        press_f2(&mut app);
        assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Ready);
        assert!(app.pending_account_fetch.is_empty());
    }

    #[test]
    fn an_in_flight_marker_aged_29s_still_blocks_a_requeue() {
        let mut app = smb_form_app(1);
        let aged = Instant::now().checked_sub(Duration::from_secs(29)).expect("monotonic clock far enough along");
        app.account_fetch_in_flight.insert(1, aged);
        press_f2(&mut app);
        assert!(app.pending_account_fetch.is_empty());
        assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Loading);
    }

    #[test]
    fn selecting_a_live_value_replaces_the_field_text() {
        let mut app = smb_form_app(1);
        app.account_cache.insert(1, (Instant::now(), accounts_fixture()));
        press_f2(&mut app);
        app.handle_picker_key(KeyCode::Up); // carol
        app.handle_picker_key(KeyCode::Enter);
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[1].value, "carol"),
            other => panic!("{other:?}"),
        }
        assert!(app.picker.is_none());
    }

    #[test]
    fn five_thousand_accounts_fill_and_filter_correctly() {
        use crate::zfs::accounts::{Account, HostAccounts};
        let users = (0..5000).map(|i| Account { name: format!("user{i:04}"), id: 2000 + i }).collect();
        let mut app = smb_form_app(1);
        app.account_cache.insert(1, (Instant::now(), HostAccounts { users, groups: vec![], group_error: None }));
        press_f2(&mut app);
        assert_eq!(app.picker.as_ref().unwrap().values.len(), 5000);
        for c in "4999".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        app.handle_picker_key(KeyCode::Enter);
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[1].value, "user4999"),
            other => panic!("{other:?}"),
        }
    }

    // ---- mutation-testing gap fillers ------------------------------------

    fn wait_until(app: &mut App, mut done: impl FnMut(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if done(app) || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn is_loading_ignores_the_vdevs_auto_refresh_but_counts_everything_else() {
        let mut app = App::new(vec!["local".to_string()]);
        assert!(!app.is_loading());
        app.in_flight.insert((Section::Vdevs, 0));
        assert!(!app.is_loading(), "the background vdev poll must not show the refreshing indicator");
        app.in_flight.insert((Section::Pools, 0));
        assert!(app.is_loading());
    }

    #[test]
    fn active_host_name_is_the_selected_host_or_a_question_mark() {
        let mut app = App::new(vec!["local".to_string(), "other".to_string()]);
        assert_eq!(app.active_host_name(), "local");
        app.selected_host = 1;
        assert_eq!(app.active_host_name(), "other");
        app.selected_host = 7;
        assert_eq!(app.active_host_name(), "?");
    }

    #[test]
    fn focused_form_value_reads_the_focused_field_and_is_empty_without_a_form() {
        let mut app = App::new(vec!["local".to_string()]);
        assert_eq!(app.focused_form_value(), "");
        app.modal = Some(Modal::Form {
            title: "t".to_string(),
            fields: vec![
                FormField { label: "a".to_string(), value: "first".to_string() },
                FormField { label: "b".to_string(), value: "second".to_string() },
            ],
            focused: 1,
            description: None,
            kind: FormKind::AddHost,
        });
        assert_eq!(app.focused_form_value(), "second");
        if let Some(Modal::Form { focused, .. }) = app.modal.as_mut() {
            *focused = 9;
        }
        assert_eq!(app.focused_form_value(), "", "out-of-range focus reads as empty");
    }

    #[test]
    fn a_cache_entry_exactly_ttl_old_is_stale() {
        let mut app = smb_form_app(1);
        let now = Instant::now();
        app.account_cache.insert(1, (now.checked_sub(ACCOUNT_CACHE_TTL).unwrap(), accounts_fixture()));
        let mut picker = crate::ui::picker::PickerState {
            values: vec![],
            filter: String::new(),
            selected: 0,
            checked: std::collections::HashSet::new(),
            multi_select: false,
            load: crate::ui::picker::PickerLoad::Loading,
            note: None,
            account: None,
            pool_host: None,
        };
        app.open_account_picker(&mut picker, 1, "", now);
        assert_eq!(picker.load, crate::ui::picker::PickerLoad::Loading, "age == TTL is not fresh");
        assert_eq!(app.pending_account_fetch, vec![1]);
    }

    #[test]
    fn refresh_datasets_populates_datasets_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank/data\t100\t200\t100\t/tank/data\tfilesystem\tyes\n"));
        app.refresh_datasets(runner);
        wait_until(&mut app, |a| !a.datasets.is_empty());
        assert_eq!(app.datasets.len(), 1);
        assert_eq!(app.datasets[0].name, "tank/data");
        assert!(!app.is_loading());
    }

    #[test]
    fn refresh_snapshots_populates_snapshots_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank/data@s1\t10\n"));
        app.refresh_snapshots(runner);
        wait_until(&mut app, |a| !a.snapshots.is_empty());
        assert_eq!(app.snapshots.len(), 1);
        assert_eq!(app.snapshots[0].name, "s1");
        assert!(!app.is_loading());
    }

    #[test]
    fn a_disk_refresh_drops_selected_disks_that_are_missing_or_no_longer_available() {
        let mut app = App::new(vec!["local".to_string()]);
        for n in ["da0", "da1", "da2"] {
            app.selected_disks.insert(n.to_string());
        }
        let disk = |name: &str, membership| DiskInfo { name: name.to_string(), size: 1, membership, ..Default::default() };
        let in_use = DiskMembership::InPool { pool: "tank".to_string(), vdev: "mirror-0".to_string() };
        app.tx.send(WorkerMsg::Disks(0, Ok(vec![disk("da0", DiskMembership::Available), disk("da1", in_use)]))).unwrap();
        app.drain_worker_messages();
        let mut kept: Vec<_> = app.selected_disks.iter().cloned().collect();
        kept.sort();
        assert_eq!(kept, vec!["da0"], "da1 is in use, da2 vanished");
    }

    fn app_with_dataset_detail_for(selected: &str) -> App {
        let mut app = App::new(vec!["local".to_string()]);
        let ds = |n: &str| Dataset { name: n.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some(format!("/{n}")) };
        app.datasets = vec![ds("tank/a"), ds("tank/b")];
        app.datasets_table.select(Some(usize::from(selected != "tank/a")));
        app.dataset_properties = vec![Property { name: "atime".to_string(), value: "on".to_string(), source: PropertySource::Local }];
        app.dataset_properties_for = Some("tank/a".to_string());
        app.dataset_permissions = vec![PermissionRow::CreateTime { perms: vec!["mount".to_string()] }];
        app.dataset_permissions_for = Some("tank/a".to_string());
        app.acl_entries_for = Some("/tank/a".to_string());
        app.acl_entries = vec![crate::acl::AclEntry {
            tag: crate::acl::AclTag::Owner,
            permissions: vec!["read_data".to_string()],
            flags: vec![],
            entry_type: crate::acl::AclEntryType::Allow,
        }];
        app
    }

    #[test]
    fn a_dataset_list_refresh_keeps_right_pane_data_that_still_belongs_to_the_selection() {
        let mut app = app_with_dataset_detail_for("tank/a");
        let same = vec![Dataset { name: "tank/a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some("/tank/a".to_string()) }];
        app.tx.send(WorkerMsg::Datasets(0, Ok(same))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.dataset_properties.len(), 1);
        assert_eq!(app.dataset_permissions.len(), 1);
        assert_eq!(app.acl_entries.len(), 1);
        assert!(app.dataset_properties_for.is_some() && app.dataset_permissions_for.is_some() && app.acl_entries_for.is_some());
    }

    #[test]
    fn a_dataset_list_refresh_that_moves_the_selection_clears_each_right_pane_cache() {
        let mut app = app_with_dataset_detail_for("tank/a");
        let moved = vec![Dataset { name: "tank/z".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some("/tank/z".to_string()) }];
        app.tx.send(WorkerMsg::Datasets(0, Ok(moved))).unwrap();
        app.drain_worker_messages();
        assert!(app.dataset_properties.is_empty() && app.dataset_properties_for.is_none());
        assert!(app.dataset_permissions.is_empty() && app.dataset_permissions_for.is_none());
        assert!(app.acl_entries.is_empty() && app.acl_entries_for.is_none());
    }

    #[test]
    fn parse_recipient_field_needs_a_name_after_user_and_group_prefixes() {
        use crate::zfs::permission::Recipient;
        assert_eq!(parse_recipient_field("user:bob"), Some(Recipient::User("bob".to_string())));
        assert_eq!(parse_recipient_field("group:wheel"), Some(Recipient::Group("wheel".to_string())));
        assert_eq!(parse_recipient_field("user:"), None);
        assert_eq!(parse_recipient_field("group:"), None);
        assert_eq!(parse_recipient_field("everyone"), Some(Recipient::Everyone));
        assert_eq!(parse_recipient_field("bob"), None);
    }

    #[test]
    fn move_table_selection_wraps_and_steps_by_one_in_both_directions() {
        let mut t = TableState::default();
        move_table_selection(&mut t, 0, true);
        assert_eq!(t.selected(), None);
        move_table_selection(&mut t, 5, true);
        assert_eq!(t.selected(), Some(0));
        move_table_selection(&mut t, 5, true);
        assert_eq!(t.selected(), Some(1));
        t.select(Some(4));
        move_table_selection(&mut t, 5, true);
        assert_eq!(t.selected(), Some(0), "forward wraps");
        move_table_selection(&mut t, 5, false);
        assert_eq!(t.selected(), Some(4), "backward from the first row wraps");
        move_table_selection(&mut t, 5, false);
        assert_eq!(t.selected(), Some(3));
        t.select(None);
        move_table_selection(&mut t, 5, false);
        assert_eq!(t.selected(), Some(4), "backward from no selection lands on the last row");
    }

    #[test]
    fn bang_requests_a_shell_on_the_selected_host() {
        let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
        app.selected_host = 1;
        app.handle_key(KeyCode::Char('!'));
        assert!(app.pending_shell);
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("fbsd16")), "{:?}", app.status_message);
    }

    #[test]
    fn bang_works_inside_a_drilled_pool_too() {
        let mut app = App::new(vec!["local".to_string()]);
        app.drilled_pool = Some("tank".to_string());
        app.handle_key(KeyCode::Char('!'));
        assert!(app.pending_shell);
    }

    #[test]
    fn bang_is_inert_with_a_modal_open_or_no_hosts() {
        let mut app = App::new(vec!["local".to_string()]);
        app.modal = Some(Modal::Info { title: "Help".to_string(), body: String::new() });
        app.handle_key(KeyCode::Char('!'));
        assert!(!app.pending_shell, "a modal owns the keyboard");
        let mut empty = App::new(Vec::new());
        empty.handle_key(KeyCode::Char('!'));
        assert!(!empty.pending_shell, "nothing to open a shell on");
    }

    #[test]
    fn help_text_documents_the_shell_key() {
        assert!(help_text().contains("  !  "), "{}", help_text());
    }

    #[test]
    fn refresh_visible_reloads_the_list_you_are_looking_at() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
        app.refresh_visible(runner);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.pools.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.pools.len(), 1);
    }

    #[test]
    fn refresh_visible_in_the_hosts_section_does_nothing() {
        let mut app = App::new(vec!["local".to_string()]);
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(""));
        app.refresh_visible(runner);
        assert!(app.pools.is_empty() && !app.is_loading());
    }

    fn app_with_jobs(jobs_list: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        crate::jobs::save(&dir.path().join("jobs.toml"), jobs_list).unwrap();
        let mut app = App::new(vec!["local".to_string()]);
        app.state_dir = dir.path().to_path_buf();
        app.selected_section = app.sections.iter().position(|s| *s == Section::Jobs).unwrap();
        (app, dir)
    }

    fn scrub_job(id: &str) -> crate::jobs::Job {
        crate::jobs::Job { id: id.into(), host: "local".into(), kind: crate::jobs::KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * 0".into(), catch_up: true, enabled: true, created_at: None, dataset: None, recursive: false, prefix: None, keep: None, dest_host: None, dest_dataset: None, dest_dir: None, force: false }
    }

    #[test]
    fn jobs_is_the_last_section_and_tab_cycles_into_and_out_of_it() {
        let mut app = App::new(vec!["local".to_string()]);
        assert_eq!(app.sections.last(), Some(&Section::Jobs));
        app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
        app.handle_key(KeyCode::Tab);
        assert_eq!(app.current_section(), Section::Jobs);
        app.handle_key(KeyCode::Tab);
        assert_eq!(app.current_section(), Section::Hosts, "wraps to the first section");
    }

    #[test]
    fn refresh_jobs_reads_the_state_dir_and_keeps_the_selection_on_the_same_job() {
        let (mut app, dir) = app_with_jobs(&[scrub_job("a"), scrub_job("b")]);
        assert!(!app.jobs_view.loaded);
        app.refresh_jobs();
        assert_eq!(app.jobs_view.jobs.len(), 2);
        app.jobs_table.select(Some(1));
        // A job is added before "b": "b" moves to row 2 and must stay selected.
        crate::jobs::save(&dir.path().join("jobs.toml"), &[scrub_job("a"), scrub_job("new"), scrub_job("b")]).unwrap();
        app.refresh_jobs();
        assert_eq!(app.jobs_table.selected(), Some(2));
        // The selected job disappears: the selection clamps instead of dangling.
        crate::jobs::save(&dir.path().join("jobs.toml"), &[scrub_job("a")]).unwrap();
        app.refresh_jobs();
        assert_eq!(app.jobs_table.selected(), Some(0));
        crate::jobs::save(&dir.path().join("jobs.toml"), &[]).unwrap();
        app.refresh_jobs();
        assert_eq!(app.jobs_table.selected(), None);
    }

    #[test]
    fn up_and_down_move_the_jobs_selection_and_wrap() {
        let (mut app, _dir) = app_with_jobs(&[scrub_job("a"), scrub_job("b")]);
        app.refresh_jobs();
        app.handle_key(KeyCode::Down);
        assert_eq!(app.jobs_table.selected(), Some(0));
        app.handle_key(KeyCode::Down);
        assert_eq!(app.jobs_table.selected(), Some(1));
        app.handle_key(KeyCode::Down);
        assert_eq!(app.jobs_table.selected(), Some(0));
        app.handle_key(KeyCode::Up);
        assert_eq!(app.jobs_table.selected(), Some(1));
    }

    #[test]
    fn jobs_refresh_happens_at_once_then_every_two_seconds_in_the_section_and_ten_elsewhere() {
        let (mut app, dir) = app_with_jobs(&[scrub_job("a")]);
        let t0 = Instant::now();
        app.refresh_jobs_if_stale(t0);
        assert!(app.jobs_view.loaded, "the first call loads immediately");
        crate::jobs::save(&dir.path().join("jobs.toml"), &[scrub_job("a"), scrub_job("b")]).unwrap();
        app.refresh_jobs_if_stale(t0 + Duration::from_secs(1));
        assert_eq!(app.jobs_view.jobs.len(), 1, "not yet: two seconds apply while the section is on screen");
        app.refresh_jobs_if_stale(t0 + Duration::from_secs(2));
        assert_eq!(app.jobs_view.jobs.len(), 2);
        crate::jobs::save(&dir.path().join("jobs.toml"), &[scrub_job("a")]).unwrap();
        app.selected_section = 0;
        app.refresh_jobs_if_stale(t0 + Duration::from_secs(5));
        assert_eq!(app.jobs_view.jobs.len(), 2, "ten seconds apply from another section");
        app.refresh_jobs_if_stale(t0 + Duration::from_secs(12));
        assert_eq!(app.jobs_view.jobs.len(), 1);
    }

    #[test]
    fn refresh_visible_in_the_jobs_section_reloads_without_a_runner() {
        let (mut app, _dir) = app_with_jobs(&[scrub_job("a")]);
        app.refresh_visible(Arc::new(FakeRunner::new("")));
        assert_eq!(app.jobs_view.jobs.len(), 1);
    }

    fn pools_app_with_one_pool() -> App {
        let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
        app.selected_host = 1;
        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
        app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
        app.pools_table.select(Some(0));
        app
    }

    fn form_values(app: &App) -> Vec<String> {
        match &app.modal {
            Some(Modal::Form { fields, .. }) => fields.iter().map(|f| f.value.clone()).collect(),
            other => panic!("expected a form, got {other:?}"),
        }
    }

    fn set_form_values(app: &mut App, values: [&str; 4]) {
        let Some(Modal::Form { fields, .. }) = &mut app.modal else { panic!("no form open") };
        for (field, value) in fields.iter_mut().zip(values) {
            field.value = value.to_string();
        }
    }

    fn at_utc(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc)
    }

    #[test]
    fn j_on_a_pool_opens_the_job_form_with_the_host_and_pool_filled_in() {
        let mut app = pools_app_with_one_pool();
        app.handle_key(KeyCode::Char('J'));
        assert_eq!(form_values(&app), vec!["fbsd16", "tank", "0 3 * * 0", "y"]);
        let Some(Modal::Form { title, kind, .. }) = &app.modal else { panic!() };
        assert!(title.contains("New scrub job"), "{title}");
        assert!(matches!(kind, FormKind::Job { existing: None, .. }));
    }

    #[test]
    fn j_with_no_pool_selected_opens_nothing_and_says_why() {
        let mut app = pools_app_with_one_pool();
        app.pools_table.select(None);
        app.handle_key(KeyCode::Char('J'));
        assert!(app.modal.is_none());
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Select a pool")), "{:?}", app.status_message);
    }

    #[test]
    fn n_in_the_jobs_section_opens_a_blank_pool_form_for_the_selected_host() {
        let (mut app, _dir) = app_with_jobs(&[]);
        app.refresh_jobs();
        app.handle_key(KeyCode::Char('n'));
        assert_eq!(form_values(&app), vec!["local", "", "0 3 * * 0", "y"]);
    }

    #[test]
    fn enter_on_a_job_opens_the_form_with_its_values_and_nothing_without_a_selection() {
        let mut job = scrub_job("scrub-local-tank");
        job.schedule = "*/10 * * * *".into();
        job.catch_up = false;
        let (mut app, _dir) = app_with_jobs(&[job]);
        app.refresh_jobs();
        app.handle_key(KeyCode::Enter);
        assert!(app.modal.is_none(), "no row is selected yet");
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Enter);
        assert_eq!(form_values(&app), vec!["local", "tank", "*/10 * * * *", "n"]);
        let Some(Modal::Form { title, kind, .. }) = &app.modal else { panic!() };
        assert!(title.contains("scrub-local-tank"), "{title}");
        assert!(matches!(kind, FormKind::Job { existing: Some(_), .. }));
    }

    fn job_kind(existing: Option<crate::jobs::Job>, taken: &[&str]) -> FormKind {
        FormKind::Job { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), taken_ids: taken.iter().map(|s| (*s).to_string()).collect() }
    }

    fn fields(values: [&str; 4]) -> Vec<FormField> {
        ["Host", "Pool", "Schedule", "Catch up (y/n)"].iter().zip(values).map(|(l, v)| FormField { label: (*l).to_string(), value: v.to_string() }).collect()
    }

    #[test]
    fn a_new_job_gets_an_id_a_creation_time_and_is_enabled() {
        let now = at_utc("2026-10-06T12:00:00Z");
        let action = build_job_action(&job_kind(None, &[]), &fields(["fbsd16", " tank ", " 0 2 * * * ", "y"]), std::path::Path::new("/state"), now).unwrap();
        assert_eq!(action.section, Section::Jobs);
        let MutationCall::SaveJob { state_dir, job } = action.call else { panic!("{:?}", action.call) };
        assert_eq!(state_dir, std::path::PathBuf::from("/state"));
        assert_eq!((job.id.as_str(), job.host.as_str(), job.pool.as_deref(), job.schedule.as_str()), ("scrub-fbsd16-tank", "fbsd16", Some("tank"), "0 2 * * *"));
        assert!(job.catch_up && job.enabled && job.kind == crate::jobs::KIND_SCRUB);
        assert_eq!(job.created_at, Some(now));
    }

    #[test]
    fn a_colliding_id_gets_a_numeric_suffix() {
        let now = at_utc("2026-10-06T12:00:00Z");
        let action = build_job_action(&job_kind(None, &["scrub-local-tank"]), &fields(["local", "tank", "0 3 * * 0", "y"]), std::path::Path::new("/s"), now).unwrap();
        let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
        assert_eq!(job.id, "scrub-local-tank-2");
    }

    #[test]
    fn an_edit_changes_the_fields_but_keeps_id_creation_time_and_enabled() {
        let mut existing = scrub_job("my-job");
        existing.enabled = false;
        existing.created_at = Some(at_utc("2026-09-01T00:00:00Z"));
        let action = build_job_action(&job_kind(Some(existing), &["my-job"]), &fields(["fbsd16", "other", "0 4 * * *", "n"]), std::path::Path::new("/s"), at_utc("2026-10-06T12:00:00Z")).unwrap();
        let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
        assert_eq!((job.id.as_str(), job.host.as_str(), job.pool.as_deref(), job.schedule.as_str(), job.catch_up), ("my-job", "fbsd16", Some("other"), "0 4 * * *", false));
        assert!(!job.enabled, "an edit does not re-enable a disabled job");
        assert_eq!(job.created_at, Some(at_utc("2026-09-01T00:00:00Z")));
    }

    #[test]
    fn a_bad_form_is_refused_with_the_reason() {
        let now = at_utc("2026-10-06T12:00:00Z");
        let try_it = |values: [&str; 4]| build_job_action(&job_kind(None, &[]), &fields(values), std::path::Path::new("/s"), now).unwrap_err();
        assert!(try_it(["nowhere", "tank", "0 3 * * 0", "y"]).contains("Unknown host 'nowhere'"));
        assert!(try_it(["local", "  ", "0 3 * * 0", "y"]).contains("Pool name is required"));
        assert!(try_it(["local", "tank", "61 * * * *", "y"]).contains("outside 0-59"));
        assert!(try_it(["local", "tank", "0 3 * * 0", "maybe"]).contains("y or n"));
    }

    #[test]
    fn submitting_a_good_form_returns_the_save_action_and_a_bad_one_stays_open_on_the_first_field() {
        let mut app = pools_app_with_one_pool();
        app.handle_key(KeyCode::Char('J'));
        set_form_values(&mut app, ["fbsd16", "tank", "61 * * * *", "y"]);
        for _ in 0..3 {
            assert!(app.handle_modal_key(KeyCode::Enter).is_none());
        }
        assert!(app.handle_modal_key(KeyCode::Enter).is_none(), "the schedule is bad");
        assert!(app.modal.is_some(), "the form stays open");
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("outside 0-59")), "{:?}", app.status_message);
        assert!(app.status_is_error());
        let Some(Modal::Form { focused, .. }) = &app.modal else { panic!() };
        assert_eq!(*focused, 0, "focus returns to the first field, as the SMB form does");
        set_form_values(&mut app, ["fbsd16", "tank", "0 3 * * 0", "y"]);
        for _ in 0..3 {
            assert!(app.handle_modal_key(KeyCode::Enter).is_none());
        }
        let action = app.handle_modal_key(KeyCode::Enter).expect("the form submits");
        assert!(matches!(action.call, MutationCall::SaveJob { .. }));
        assert!(app.modal.is_none());
    }

    #[test]
    fn f2_on_host_lists_the_configured_hosts_and_on_schedule_the_presets_inserting_only_the_cron_part() {
        let mut app = pools_app_with_one_pool();
        app.handle_key(KeyCode::Char('J'));
        app.handle_modal_key(KeyCode::F(2));
        let picker = app.picker.clone().expect("a host picker");
        assert_eq!(picker.values, vec!["local", "fbsd16"]);
        assert!(!picker.multi_select);
        app.handle_picker_key(KeyCode::Down);
        app.handle_picker_key(KeyCode::Enter);
        assert_eq!(form_values(&app)[0], "fbsd16".to_string());
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        let presets = app.picker.clone().expect("a schedule picker");
        assert_eq!(presets.values, crate::ui::jobs::preset_values());
        assert_eq!(presets.selected, 2, "the picker opens on the field's current preset");
        app.handle_picker_key(KeyCode::Up);
        app.handle_picker_key(KeyCode::Enter);
        assert_eq!(form_values(&app)[2], "0 2 * * *", "no label text lands in the field");
    }

    #[test]
    fn f2_on_catch_up_does_nothing() {
        let mut app = pools_app_with_one_pool();
        app.handle_key(KeyCode::Char('J'));
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        assert!(app.picker.is_none());
    }

    #[test]
    fn f2_on_the_pool_field_lists_the_pool_names_of_the_host_in_the_host_field() {
        let mut app = pools_app_with_one_pool();
        app.handle_key(KeyCode::Char('J'));
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        let picker = app.picker.as_ref().expect("a pool picker");
        assert_eq!(picker.load, crate::ui::picker::PickerLoad::Loading);
        assert_eq!(app.pending_pool_fetch, vec![(1, crate::ui::picker::LiveNames::Pools)]);
        app.tx.send(WorkerMsg::PoolNames(1, Ok(vec!["scratch".to_string(), "zroot".to_string()]))).unwrap();
        app.drain_worker_messages();
        let picker = app.picker.as_ref().unwrap();
        assert_eq!(picker.load, crate::ui::picker::PickerLoad::Ready);
        assert_eq!(picker.values, vec!["scratch", "zroot"]);
        app.handle_picker_key(KeyCode::Down);
        app.handle_picker_key(KeyCode::Enter);
        assert_eq!(form_values(&app)[1], "zroot");
    }

    #[test]
    fn a_failed_pool_lookup_says_so_and_an_unknown_host_is_refused_without_a_fetch() {
        let mut app = pools_app_with_one_pool();
        app.handle_key(KeyCode::Char('J'));
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        app.tx.send(WorkerMsg::PoolNames(1, Err(crate::error::AppError::Parse("ssh down".to_string())))).unwrap();
        app.drain_worker_messages();
        assert!(matches!(&app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Failed(m) if m.contains("ssh down")));

        let mut app = pools_app_with_one_pool();
        app.handle_key(KeyCode::Char('J'));
        for c in "x".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        assert!(matches!(&app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Failed(m) if m.contains("unknown host")));
        assert!(app.pending_pool_fetch.is_empty());
    }

    fn selected_jobs_app(jobs_list: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
        let (mut app, dir) = app_with_jobs(jobs_list);
        app.refresh_jobs();
        app.handle_key(KeyCode::Down);
        (app, dir)
    }

    #[test]
    fn space_on_a_job_queues_a_disable_then_an_enable_and_says_what_it_is_doing() {
        let (mut app, _dir) = selected_jobs_app(&[scrub_job("a")]);
        app.handle_key(KeyCode::Char(' '));
        let [action] = app.pending_mutations.as_slice() else { panic!("{:?}", app.pending_mutations) };
        assert_eq!(action.section, Section::Jobs);
        assert!(matches!(&action.call, MutationCall::SetJobEnabled { id, enabled: false, .. } if id == "a"), "{:?}", action.call);
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Disabling job 'a'")), "{:?}", app.status_message);
        app.pending_mutations.clear();
        app.jobs_view.jobs[0].enabled = false;
        app.handle_key(KeyCode::Char(' '));
        assert!(matches!(&app.pending_mutations[0].call, MutationCall::SetJobEnabled { enabled: true, .. }));
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Enabling job 'a'")), "{:?}", app.status_message);
    }

    #[test]
    fn space_with_no_job_selected_does_nothing() {
        let (mut app, _dir) = app_with_jobs(&[scrub_job("a")]);
        app.refresh_jobs();
        app.handle_key(KeyCode::Char(' '));
        assert!(app.pending_mutations.is_empty());
    }

    #[test]
    fn d_on_a_job_asks_for_a_warning_confirmation_and_deletes_only_that_job() {
        let (mut app, _dir) = selected_jobs_app(&[scrub_job("a"), scrub_job("b")]);
        app.handle_key(KeyCode::Char('d'));
        let Some(Modal::Confirm { prompt, action }) = &app.modal else { panic!("{:?}", app.modal) };
        assert!(prompt.contains("Delete job 'a'") && prompt.contains("history"), "{prompt}");
        assert_eq!(action.call.tone(), crate::ui::modal::Tone::Warn);
        let action = app.handle_modal_key(KeyCode::Char('y')).expect("confirmed");
        assert_eq!(action.section, Section::Jobs);
        assert!(matches!(&action.call, MutationCall::DeleteJob { id, .. } if id == "a"), "{:?}", action.call);
    }

    #[test]
    fn d_with_no_job_selected_opens_nothing_and_n_cancels_a_delete() {
        let (mut app, _dir) = app_with_jobs(&[scrub_job("a")]);
        app.refresh_jobs();
        app.handle_key(KeyCode::Char('d'));
        assert!(app.modal.is_none());
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Char('d'));
        assert!(app.handle_modal_key(KeyCode::Char('n')).is_none());
        assert!(app.modal.is_none());
    }

    #[test]
    fn help_text_documents_the_jobs_keys() {
        let text = help_text();
        let jobs = &text[text.find("\nJobs\n").expect("a Jobs block")..];
        for want in ["Enter", "Space", "  d ", "  n ", "history"] {
            assert!(jobs.contains(want), "missing {want:?} in the Jobs block:\n{jobs}");
        }
    }

    #[test]
    fn help_text_documents_the_global_keys() {
        let text = help_text();
        assert!(text.starts_with("Global"));
        assert!(text.contains("Tab / Shift+Tab"));
    }

    #[test]
    fn back_tab_steps_one_section_back_and_wraps_to_the_last() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = 2;
        app.handle_key(KeyCode::BackTab);
        assert_eq!(app.selected_section, 1);
        app.handle_key(KeyCode::BackTab);
        assert_eq!(app.selected_section, 0);
        app.handle_key(KeyCode::BackTab);
        assert_eq!(app.selected_section, app.sections.len() - 1);
    }

    #[test]
    fn tab_and_back_tab_do_nothing_while_drilled_into_a_pool() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
        app.drilled_pool = Some("tank".to_string());
        let before = app.selected_section;
        app.handle_key(KeyCode::Tab);
        assert_eq!(app.selected_section, before);
        app.handle_key(KeyCode::BackTab);
        assert_eq!(app.selected_section, before);
    }

    #[test]
    fn esc_only_clears_selected_disks_in_the_disks_section_and_only_when_some_are_selected() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_disks.insert("da0".to_string());
        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
        app.status_message = Some("hello".to_string());
        app.handle_key(KeyCode::Esc);
        assert!(app.status_message.is_none(), "Esc outside Disks dismisses the status message");
        assert_eq!(app.selected_disks.len(), 1, "and leaves the disk selection alone");
        app.selected_section = app.sections.iter().position(|s| *s == Section::Disks).unwrap();
        app.selected_disks.clear();
        app.status_message = Some("hello".to_string());
        app.handle_key(KeyCode::Esc);
        assert!(app.status_message.is_none(), "with no disks selected Esc still dismisses the message");
    }

    fn in_section(section: Section) -> App {
        let mut app = App::new(vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == section).unwrap();
        app.pane_focus = PaneFocus::Middle;
        app
    }

    fn two_props() -> Vec<Property> {
        ["x", "y"].iter().map(|n| Property { name: (*n).to_string(), value: "v".to_string(), source: PropertySource::Local }).collect()
    }

    fn pool_fixture(name: &str) -> crate::zfs::pool::Pool {
        crate::zfs::pool::Pool { name: name.to_string(), size: 1, allocated: 0, free: 1, health: crate::zfs::pool::PoolHealth::Online }
    }

    /// Presses Up with the table at row 1 and checks it moved to row 0, while
    /// every other table in `others` was left alone.
    fn up_moves(app: &mut App, table: fn(&mut App) -> &mut TableState, others: &[fn(&mut App) -> &mut TableState]) {
        table(app).select(Some(1));
        for o in others {
            o(app).select(Some(1));
        }
        app.handle_key(KeyCode::Up);
        assert_eq!(table(app).selected(), Some(0));
        for o in others {
            assert_eq!(o(app).selected(), Some(1), "Up must only move the table the user is looking at");
        }
    }

    fn down_moves(app: &mut App, table: fn(&mut App) -> &mut TableState) {
        table(app).select(Some(0));
        app.handle_key(KeyCode::Down);
        assert_eq!(table(app).selected(), Some(1));
    }

    #[test]
    fn up_steps_back_through_hosts_and_wraps() {
        let mut app = in_section(Section::Hosts);
        app.selected_host = 2;
        app.handle_key(KeyCode::Up);
        assert_eq!(app.selected_host, 1);
        app.handle_key(KeyCode::Up);
        assert_eq!(app.selected_host, 0);
        app.handle_key(KeyCode::Up);
        assert_eq!(app.selected_host, 2);
    }

    #[test]
    fn up_and_down_move_the_vdev_rows_when_drilled_and_the_pool_rows_when_not() {
        let vdev = |n: &str| crate::zfs::pool::VdevLine { depth: 0, name: n.to_string(), health: crate::zfs::pool::PoolHealth::Online };
        let mut app = in_section(Section::Pools);
        app.pools = vec![pool_fixture("tank"), pool_fixture("scratch")];
        app.vdevs = vec![vdev("tank"), vdev("da0")];
        up_moves(&mut app, |a| &mut a.pools_table, &[|a| &mut a.vdevs_table]);
        down_moves(&mut app, |a| &mut a.pools_table);
        app.drilled_pool = Some("tank".to_string());
        up_moves(&mut app, |a| &mut a.vdevs_table, &[|a| &mut a.pools_table]);
        down_moves(&mut app, |a| &mut a.vdevs_table);
    }

    #[test]
    fn up_and_down_move_the_pool_property_rows_in_the_right_pane() {
        let mut app = in_section(Section::Pools);
        app.pools = vec![pool_fixture("tank"), pool_fixture("scratch")];
        app.pool_properties = two_props();
        app.pane_focus = PaneFocus::Right;
        up_moves(&mut app, |a| &mut a.pool_properties_table, &[|a| &mut a.pools_table]);
        down_moves(&mut app, |a| &mut a.pool_properties_table);
    }

    #[test]
    fn up_and_down_move_the_row_list_of_the_active_dataset_detail_tab() {
        let mut app = in_section(Section::Datasets);
        let ds = |n: &str| Dataset { name: n.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some(format!("/{n}")) };
        app.datasets = vec![ds("tank/a"), ds("tank/b")];
        app.dataset_properties = two_props();
        app.dataset_permissions = vec![
            PermissionRow::CreateTime { perms: vec!["mount".to_string()] },
            PermissionRow::CreateTime { perms: vec!["create".to_string()] },
        ];
        let entry = |tag| crate::acl::AclEntry { tag, permissions: vec![], flags: vec![], entry_type: crate::acl::AclEntryType::Allow };
        app.acl_entries = vec![entry(crate::acl::AclTag::Owner), entry(crate::acl::AclTag::Everyone)];
        up_moves(&mut app, |a| &mut a.datasets_table, &[]); // a row change legitimately resets the detail pane
        down_moves(&mut app, |a| &mut a.datasets_table);
        // Moving the dataset row cleared the detail lists; refill them for the right-pane checks.
        app.dataset_properties = two_props();
        app.dataset_permissions = vec![
            PermissionRow::CreateTime { perms: vec!["mount".to_string()] },
            PermissionRow::CreateTime { perms: vec!["create".to_string()] },
        ];
        app.acl_entries = vec![entry(crate::acl::AclTag::Owner), entry(crate::acl::AclTag::Everyone)];
        app.pane_focus = PaneFocus::Right;
        app.dataset_detail_tab = DatasetDetailTab::Properties;
        up_moves(&mut app, |a| &mut a.dataset_properties_table, &[|a| &mut a.datasets_table, |a| &mut a.dataset_permissions_table, |a| &mut a.acl_entries_table]);
        down_moves(&mut app, |a| &mut a.dataset_properties_table);
        app.dataset_detail_tab = DatasetDetailTab::Permissions;
        up_moves(&mut app, |a| &mut a.dataset_permissions_table, &[|a| &mut a.dataset_properties_table, |a| &mut a.acl_entries_table]);
        down_moves(&mut app, |a| &mut a.dataset_permissions_table);
        app.dataset_detail_tab = DatasetDetailTab::Acl;
        up_moves(&mut app, |a| &mut a.acl_entries_table, &[|a| &mut a.dataset_properties_table, |a| &mut a.dataset_permissions_table]);
        down_moves(&mut app, |a| &mut a.acl_entries_table);
    }

    #[test]
    fn up_and_down_move_the_snapshot_disk_and_share_rows() {
        let mut app = in_section(Section::Snapshots);
        let snap = |n: &str| crate::zfs::snapshot::Snapshot { dataset: "tank/a".to_string(), name: n.to_string(), used: 1 };
        app.snapshots = vec![snap("s1"), snap("s2")];
        up_moves(&mut app, |a| &mut a.snapshots_table, &[|a| &mut a.disks_table, |a| &mut a.shares_table]);
        down_moves(&mut app, |a| &mut a.snapshots_table);

        let mut app = in_section(Section::Disks);
        app.disks = vec![
            DiskInfo { name: "da0".to_string(), size: 1, ..Default::default() },
            DiskInfo { name: "da1".to_string(), size: 1, ..Default::default() },
        ];
        up_moves(&mut app, |a| &mut a.disks_table, &[|a| &mut a.snapshots_table, |a| &mut a.shares_table]);
        down_moves(&mut app, |a| &mut a.disks_table);

        let mut app = in_section(Section::Shares);
        app.shares = vec![smb("tank/a"), smb("tank/b")];
        up_moves(&mut app, |a| &mut a.shares_table, &[|a| &mut a.snapshots_table, |a| &mut a.disks_table]);
        down_moves(&mut app, |a| &mut a.shares_table);
    }

    #[test]
    fn n_and_a_only_ask_for_a_disk_selection_in_the_disks_section() {
        for section in [Section::Snapshots, Section::Shares] {
            let mut app = in_section(section);
            app.handle_key(KeyCode::Char('n'));
            app.handle_key(KeyCode::Char('a'));
            assert!(app.status_message.is_none(), "{section:?}: {:?}", app.status_message);
            assert!(app.modal.is_none());
        }
        let mut app = in_section(Section::Disks);
        app.handle_key(KeyCode::Char('n'));
        assert_eq!(app.status_message.as_deref(), Some("select at least one disk with Space first"));
        app.status_message = None;
        app.handle_key(KeyCode::Char('a'));
        assert_eq!(app.status_message.as_deref(), Some("select at least one disk with Space first"));
    }

    #[test]
    fn removing_an_acl_entry_needs_the_selection_to_be_inside_the_list() {
        let mut app = in_section(Section::Datasets);
        app.datasets = vec![Dataset { name: "tank/a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some("/tank/a".to_string()) }];
        app.datasets_table.select(Some(0));
        app.pane_focus = PaneFocus::Right;
        app.dataset_detail_tab = DatasetDetailTab::Acl;
        app.acl_entries = vec![crate::acl::AclEntry { tag: crate::acl::AclTag::Owner, permissions: vec![], flags: vec![], entry_type: crate::acl::AclEntryType::Allow }];
        app.acl_entries_table.select(Some(1)); // one past the last entry
        app.handle_key(KeyCode::Char('d'));
        assert!(app.modal.is_none());
        app.acl_entries_table.select(Some(0));
        app.handle_key(KeyCode::Char('d'));
        assert!(matches!(app.modal, Some(Modal::Confirm { .. })));
    }

    #[test]
    fn up_and_down_on_an_empty_hosts_list_do_not_fall_through_to_another_sections_rows() {
        let mut app = App::new(vec![]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Hosts).unwrap();
        app.shares = vec![smb("tank/a"), smb("tank/b")];
        app.shares_table.select(Some(0));
        app.handle_key(KeyCode::Down);
        assert_eq!(app.shares_table.selected(), Some(0));
        app.shares_table.select(Some(1));
        app.handle_key(KeyCode::Up);
        assert_eq!(app.shares_table.selected(), Some(1));
    }

    // ---- auto refresh -----------------------------------------------------

    const TEN: Duration = Duration::from_secs(10);

    fn runner(out: &str) -> Arc<dyn CommandRunner> {
        Arc::new(FakeRunner::new(out))
    }

    #[test]
    fn auto_refresh_targets_only_the_visible_list_sections() {
        for (section, expected) in [
            (Section::Hosts, None),
            (Section::Pools, Some(Section::Pools)),
            (Section::Datasets, Some(Section::Datasets)),
            (Section::Snapshots, Some(Section::Snapshots)),
            (Section::Disks, Some(Section::Disks)),
            (Section::Shares, Some(Section::Shares)),
            (Section::BootEnvs, Some(Section::BootEnvs)),
        ] {
            assert_eq!(in_section(section).auto_refresh_section(), expected, "{section:?}");
        }
    }

    #[test]
    fn auto_refresh_never_runs_under_a_modal_a_picker_or_a_pool_drill_down() {
        let mut app = in_section(Section::Pools);
        app.drilled_pool = Some("tank".to_string());
        assert_eq!(app.auto_refresh_section(), None, "the vdev view has its own faster poll");
        app.drilled_pool = None;
        app.modal = Some(Modal::Info { title: "t".to_string(), body: "b".to_string() });
        assert_eq!(app.auto_refresh_section(), None);
        app.modal = None;
        app.picker = Some(crate::ui::picker::PickerState {
            values: vec![],
            filter: String::new(),
            selected: 0,
            checked: HashSet::new(),
            multi_select: false,
            load: crate::ui::picker::PickerLoad::Ready,
            note: None,
            account: None,
            pool_host: None,
        });
        assert_eq!(app.auto_refresh_section(), None);
    }

    #[test]
    fn auto_refresh_fires_immediately_then_waits_for_the_interval() {
        let mut app = in_section(Section::Pools);
        let t0 = Instant::now();
        app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), t0, TEN);
        wait_until(&mut app, |a| !a.pools.is_empty());
        assert_eq!(app.pools.len(), 1, "first call refreshes straight away");

        app.pools.clear();
        app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), t0 + Duration::from_secs(9), TEN);
        std::thread::sleep(Duration::from_millis(100));
        app.drain_worker_messages();
        assert!(app.pools.is_empty(), "9s < 10s: not due yet");

        app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), t0 + TEN, TEN);
        wait_until(&mut app, |a| !a.pools.is_empty());
        assert_eq!(app.pools.len(), 1, "due at exactly the interval");
    }

    #[test]
    fn auto_refresh_tracks_each_section_and_host_separately() {
        let mut app = in_section(Section::Pools);
        let t0 = Instant::now();
        app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), t0, TEN);
        wait_until(&mut app, |a| !a.pools.is_empty());
        // Switching to Snapshots right after must not be held back by Pools' timer.
        app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
        app.auto_refresh(runner("tank/a@s1\t10\n"), t0 + Duration::from_secs(1), TEN);
        wait_until(&mut app, |a| !a.snapshots.is_empty());
        assert_eq!(app.snapshots.len(), 1);
    }

    #[test]
    fn an_auto_refresh_is_quiet_but_a_manual_one_shows_the_loading_indicator() {
        let mut app = in_section(Section::Pools);
        app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), Instant::now(), TEN);
        assert!(!app.is_loading(), "background polling must not flash the refreshing indicator");
        wait_until(&mut app, |a| !a.pools.is_empty());
        app.refresh_pools(runner("tank\t100\t10\t90\tONLINE\n"));
        assert!(app.is_loading());
        wait_until(&mut app, |a| !a.is_loading());
        assert!(!app.is_loading());
    }

    #[test]
    fn a_quiet_refresh_keeps_an_informational_status_but_a_manual_one_clears_it() {
        let mut app = in_section(Section::Pools);
        app.status_message = Some("scrub started on 'tank'".to_string());
        app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), Instant::now(), TEN);
        wait_until(&mut app, |a| !a.pools.is_empty());
        assert_eq!(app.status_message.as_deref(), Some("scrub started on 'tank'"));

        app.refresh_pools(runner("tank\t100\t10\t90\tONLINE\n"));
        wait_until(&mut app, |a| a.status_message.is_none());
        assert!(app.status_message.is_none());
    }

    #[test]
    fn a_quiet_refresh_clears_a_stale_error_once_the_host_answers() {
        let mut app = in_section(Section::Pools);
        app.set_error("connection refused".to_string());
        app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), Instant::now(), TEN);
        wait_until(&mut app, |a| !a.pools.is_empty());
        assert!(app.status_message.is_none());
    }

    #[test]
    fn a_refresh_keeps_the_selection_on_the_same_item_when_the_list_reorders() {
        let mut app = in_section(Section::Pools);
        app.pools = vec![pool_fixture("alpha"), pool_fixture("bravo"), pool_fixture("charlie")];
        app.pools_table.select(Some(1)); // bravo
        let reordered = vec![pool_fixture("new"), pool_fixture("alpha"), pool_fixture("charlie"), pool_fixture("bravo")];
        app.tx.send(WorkerMsg::Pools(0, Ok(reordered))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.pools_table.selected(), Some(3));
        assert_eq!(app.selected_pool_name().as_deref(), Some("bravo"), "a keypress must still act on bravo");
    }

    #[test]
    fn a_refresh_falls_back_to_clamping_when_the_selected_item_is_gone() {
        let mut app = in_section(Section::Pools);
        app.pools = vec![pool_fixture("alpha"), pool_fixture("bravo"), pool_fixture("charlie")];
        app.pools_table.select(Some(2));
        app.tx.send(WorkerMsg::Pools(0, Ok(vec![pool_fixture("alpha")]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.pools_table.selected(), Some(0));
    }

    #[test]
    fn selection_is_pinned_by_identity_in_datasets_snapshots_and_disks_too() {
        let ds = |n: &str| Dataset { name: n.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: None };
        let mut app = in_section(Section::Datasets);
        app.datasets = vec![ds("tank/a"), ds("tank/b")];
        app.datasets_table.select(Some(1));
        app.tx.send(WorkerMsg::Datasets(0, Ok(vec![ds("tank/0"), ds("tank/a"), ds("tank/b")]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.selected_dataset_name().as_deref(), Some("tank/b"));

        let snap = |d: &str, n: &str| crate::zfs::snapshot::Snapshot { dataset: d.to_string(), name: n.to_string(), used: 1 };
        app.snapshots = vec![snap("tank/a", "s1"), snap("tank/b", "s1")];
        app.snapshots_table.select(Some(1)); // tank/b@s1: same snapshot name as another dataset's
        app.tx.send(WorkerMsg::Snapshots(0, Ok(vec![snap("tank/b", "s1"), snap("tank/a", "s1")]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.snapshots_table.selected(), Some(0), "identity is dataset@name, not just the name");

        let disk = |n: &str| DiskInfo { name: n.to_string(), size: 1, ..Default::default() };
        app.disks = vec![disk("da0"), disk("da1")];
        app.disks_table.select(Some(1));
        app.tx.send(WorkerMsg::Disks(0, Ok(vec![disk("da9"), disk("da0"), disk("da1")]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.disks_table.selected(), Some(2));
    }

    #[test]
    fn auto_refresh_starts_the_refresh_that_belongs_to_the_visible_section() {
        for section in [Section::Pools, Section::Datasets, Section::Snapshots, Section::Disks, Section::Shares, Section::BootEnvs] {
            let mut app = in_section(section);
            app.auto_refresh(runner(""), Instant::now(), TEN);
            assert!(app.in_flight.contains(&(section, 0)), "{section:?} refresh not started");
            assert_eq!(app.in_flight.len(), 1, "only the visible section is refreshed ({section:?})");
            assert!(app.quiet_refresh.contains(&(section, 0)), "{section:?} should be quiet");
        }
    }

    #[test]
    fn a_share_refresh_keeps_the_selection_on_the_same_share_and_protocol() {
        let nfs = |d: &str| crate::zfs::share::Share::Nfs { dataset: d.to_string(), raw: "on".to_string(), networks: vec![], hosts: vec![], read_only: false, root: None };
        let snapshot = |shares| crate::zfs::share::SharesSnapshot { shares, nfs_available: true, samba_available: true, smb_error: None };
        let mut app = in_section(Section::Shares);
        app.shares = vec![nfs("tank/a"), smb("tank/b")];
        app.shares_table.select(Some(1)); // the SMB share on tank/b
        app.tx.send(WorkerMsg::Shares(0, Ok(snapshot(vec![nfs("tank/0"), nfs("tank/b"), nfs("tank/a"), smb("tank/b")])))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.shares_table.selected(), Some(3), "tank/b's SMB share, not its NFS share");
    }

    #[test]
    fn a_quiet_shares_refresh_keeps_an_informational_status() {
        let mut app = in_section(Section::Shares);
        app.status_message = Some("share created".to_string());
        app.auto_refresh(runner(""), Instant::now(), TEN);
        wait_until(&mut app, |a| a.shares_loaded);
        assert!(app.shares_loaded);
        assert_eq!(app.status_message.as_deref(), Some("share created"));
    }

    #[test]
    fn n_in_jobs_opens_a_blank_snapshot_form_with_sensible_defaults() {
        let (mut app, _dir) = app_with_jobs(&[]);
        app.handle_key(KeyCode::Char('N'));
        assert_eq!(form_values(&app), ["local", "", "n", "auto", "24", "0 * * * *", "y"]);
    }

    #[test]
    fn j_on_a_dataset_prefills_the_snapshot_form() {
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.handle_key(KeyCode::Char('J'));
        assert_eq!(form_values(&app)[1], "tank/media");
    }

    #[test]
    fn enter_on_a_snapshot_job_edits_it_in_the_snapshot_form() {
        let (mut app, _dir) = app_with_jobs(&[crate::jobs::Job::test_snapshot("s", "tank/data", "hourly", 7)]);
        app.refresh_jobs();
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Enter);
        assert_eq!(form_values(&app), ["local", "tank/data", "n", "hourly", "7", "0 * * * *", "y"]);
    }

    fn snapshot_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
        FormKind::SnapshotJob { hosts: vec!["local".to_string()], existing: existing.map(Box::new), jobs }
    }

    fn snap_fields(values: [&str; 7]) -> Vec<FormField> {
        values.iter().map(|v| FormField { label: String::new(), value: (*v).to_string() }).collect()
    }

    fn snap_err(values: [&str; 7], jobs: Vec<crate::jobs::Job>) -> String {
        build_snapshot_job_action(&snapshot_kind(None, jobs), &snap_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap_err()
    }

    #[test]
    fn a_valid_snapshot_form_builds_a_save_of_a_new_job_with_a_fresh_id_and_creation_time() {
        let now = at_utc("2026-10-06T12:00:00Z");
        let action = build_snapshot_job_action(&snapshot_kind(None, vec![]), &snap_fields(["local", " tank/data ", "y", "hourly", "24", "0 * * * *", "y"]), std::path::Path::new("/state"), now).unwrap();
        assert_eq!(action.section, Section::Jobs);
        let MutationCall::SaveJob { state_dir, job } = action.call else { panic!("{:?}", action.call) };
        assert_eq!(state_dir, std::path::PathBuf::from("/state"));
        assert_eq!((job.id.as_str(), job.kind.as_str(), job.dataset.as_deref(), job.recursive, job.prefix.as_deref(), job.keep), ("snap-local-tank-data-hourly", crate::jobs::KIND_SNAPSHOT, Some("tank/data"), true, Some("hourly"), Some(24)));
        assert!(job.pool.is_none() && job.catch_up && job.enabled);
        assert_eq!(job.created_at, Some(now));
    }

    #[test]
    fn blank_prefix_and_blank_yes_no_fields_take_their_defaults() {
        let now = at_utc("2026-10-06T12:00:00Z");
        let action = build_snapshot_job_action(&snapshot_kind(None, vec![]), &snap_fields(["local", "tank", "", "", "3", "0 * * * *", ""]), std::path::Path::new("/s"), now).unwrap();
        let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
        assert_eq!((job.recursive, job.prefix.as_deref(), job.catch_up), (false, Some("auto"), true));
    }

    #[test]
    fn an_edited_snapshot_job_keeps_its_id_creation_time_and_enabled_flag() {
        let mut existing = crate::jobs::Job::test_snapshot("keep-me", "tank/data", "hourly", 3);
        existing.enabled = false;
        existing.created_at = Some(at_utc("2026-01-01T00:00:00Z"));
        let kind = snapshot_kind(Some(existing.clone()), vec![existing]);
        let action = build_snapshot_job_action(&kind, &snap_fields(["local", "tank/other", "n", "daily", "9", "0 2 * * *", "n"]), std::path::Path::new("/s"), at_utc("2026-10-06T12:00:00Z")).unwrap();
        let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
        assert_eq!((job.id.as_str(), job.enabled, job.created_at), ("keep-me", false, Some(at_utc("2026-01-01T00:00:00Z"))));
        assert_eq!((job.dataset.as_deref(), job.prefix.as_deref(), job.keep, job.catch_up), (Some("tank/other"), Some("daily"), Some(9), false));
    }

    #[test]
    fn the_snapshot_form_refuses_what_the_daemon_would_refuse() {
        let good = ["local", "tank/data", "n", "hourly", "24", "0 * * * *", "y"];
        assert!(build_snapshot_job_action(&snapshot_kind(None, vec![]), &snap_fields(good), std::path::Path::new("/x"), chrono::Utc::now()).is_ok());
        assert!(snap_err(["nope", "tank/data", "n", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Unknown host"));
        assert!(snap_err(["local", "", "n", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Dataset"));
        assert!(snap_err(["local", "-rf", "n", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Dataset"));
        assert!(snap_err(["local", "tank/data", "maybe", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Recursive"));
        assert!(snap_err(["local", "tank/data", "n", "a b", "24", "0 * * * *", "y"], vec![]).contains("Prefix"));
        assert!(snap_err(["local", "tank/data", "n", "hourly", "0", "0 * * * *", "y"], vec![]).contains("Keep"));
        assert!(snap_err(["local", "tank/data", "n", "hourly", "x", "0 * * * *", "y"], vec![]).contains("Keep"));
        assert!(snap_err(["local", "tank/data", "n", "hourly", "24", "bogus", "y"], vec![]).contains("Schedule"));
        assert!(snap_err(["local", "tank/data", "n", "hourly", "24", "0 * * * *", "q"], vec![]).contains("Catch up"));
    }

    #[test]
    fn the_snapshot_form_refuses_a_prefix_another_job_already_uses_on_that_dataset() {
        let other = crate::jobs::Job::test_snapshot("other", "tank/data", "hourly", 5);
        assert!(snap_err(["local", "tank/data", "n", "hourly", "24", "0 * * * *", "y"], vec![other.clone()]).contains("other"));
        // Editing that same job is fine, as is a different prefix.
        let kind = snapshot_kind(Some(other.clone()), vec![other]);
        assert!(build_snapshot_job_action(&kind, &snap_fields(["local", "tank/data", "n", "hourly", "9", "0 * * * *", "y"]), std::path::Path::new("/x"), chrono::Utc::now()).is_ok());
    }

    #[test]
    fn f2_on_the_snapshot_forms_dataset_field_queues_a_dataset_fetch_for_the_chosen_host() {
        let (mut app, _dir) = app_with_jobs(&[]);
        app.handle_key(KeyCode::Char('N'));
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        assert_eq!(app.pending_pool_fetch, vec![(0, crate::ui::picker::LiveNames::Datasets)]);
        app.tx.send(WorkerMsg::PoolNames(0, Ok(vec!["tank".to_string(), "tank/data".to_string()]))).unwrap();
        app.drain_worker_messages();
        app.handle_picker_key(KeyCode::Down);
        app.handle_picker_key(KeyCode::Enter);
        assert_eq!(form_values(&app)[1], "tank/data");
    }

    #[test]
    fn f2_on_the_snapshot_forms_host_and_schedule_fields_opens_their_pickers_and_the_rest_do_nothing() {
        let (mut app, _dir) = app_with_jobs(&[]);
        app.handle_key(KeyCode::Char('N'));
        app.handle_modal_key(KeyCode::F(2));
        assert_eq!(app.picker.as_ref().unwrap().values, vec!["local"]);
        app.picker = None;
        for _ in 0..5 {
            app.handle_modal_key(KeyCode::Tab);
        }
        app.handle_modal_key(KeyCode::F(2));
        assert_eq!(app.picker.as_ref().unwrap().values, crate::ui::jobs::preset_values());
        app.picker = None;
        app.handle_modal_key(KeyCode::Tab); // Catch up
        app.handle_modal_key(KeyCode::F(2));
        assert!(app.picker.is_none());
    }

    #[test]
    fn space_refuses_to_enable_a_snapshot_job_that_clashes_with_an_enabled_one() {
        let mut a = crate::jobs::Job::test_snapshot("a", "tank/data", "hourly", 3);
        a.enabled = false;
        let b = crate::jobs::Job::test_snapshot("b", "tank/data", "hourly", 48);
        let (mut app, _dir) = app_with_jobs(&[a, b]);
        app.refresh_jobs();
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Char(' '));
        assert!(app.pending_mutations.is_empty(), "{:?}", app.pending_mutations);
        let msg = app.status_message.clone().unwrap_or_default();
        assert!(msg.contains("'b'") && msg.contains("prune each other"), "{msg}");
    }

    #[test]
    fn space_still_enables_a_job_with_no_clash() {
        let mut a = crate::jobs::Job::test_snapshot("a", "tank/data", "hourly", 3);
        a.enabled = false;
        let (mut app, _dir) = app_with_jobs(&[a]);
        app.refresh_jobs();
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Char(' '));
        assert_eq!(app.pending_mutations.len(), 1);
    }

    fn repl_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
        FormKind::ReplicateJob { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), jobs }
    }

    fn repl_fields(values: [&str; 8]) -> Vec<FormField> {
        values.iter().map(|v| FormField { label: String::new(), value: (*v).to_string() }).collect()
    }

    fn repl_err(values: [&str; 8], jobs: Vec<crate::jobs::Job>) -> String {
        build_replicate_job_action(&repl_kind(None, jobs), &repl_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap_err()
    }

    const GOOD: [&str; 8] = ["local", "tank/data", "hourly", "fbsd16", "backup/data", "n", "30 * * * *", "y"];

    #[test]
    fn big_r_in_jobs_opens_a_blank_replication_form_with_defaults() {
        let (mut app, _dir) = app_with_jobs(&[]);
        app.handle_key(KeyCode::Char('R'));
        assert_eq!(form_values(&app), ["local", "", "auto", "", "", "n", "30 * * * *", "y"]);
    }

    #[test]
    fn a_valid_replication_form_builds_a_save_with_a_fresh_id_and_creation_time() {
        let now = at_utc("2026-10-06T12:00:00Z");
        let action = build_replicate_job_action(&repl_kind(None, vec![]), &repl_fields(GOOD), std::path::Path::new("/state"), now).unwrap();
        let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
        assert_eq!((job.kind.as_str(), job.host.as_str(), job.dataset.as_deref(), job.prefix.as_deref(), job.dest_host.as_deref(), job.dest_dataset.as_deref(), job.force), (crate::jobs::KIND_REPLICATE, "local", Some("tank/data"), Some("hourly"), Some("fbsd16"), Some("backup/data"), false));
        assert_eq!(job.id, "repl-local-tank-data-to-fbsd16-backup-data");
        assert_eq!(job.created_at, Some(now));
        assert!(job.enabled && job.catch_up && job.pool.is_none() && job.keep.is_none());
    }

    #[test]
    fn the_replication_form_refuses_what_the_daemon_would_refuse() {
        let with = |i: usize, v: &'static str| {
            let mut vals = GOOD;
            vals[i] = v;
            vals
        };
        assert!(repl_err(with(0, "nope"), vec![]).contains("host"));
        assert!(repl_err(with(1, "-rf"), vec![]).contains("source dataset"));
        assert!(repl_err(with(2, "a b"), vec![]).contains("prefix"));
        assert!(repl_err(with(3, ""), vec![]).contains("destination host"));
        assert!(repl_err(with(3, "nope"), vec![]).contains("host"));
        assert!(repl_err(with(4, "tank/a@b"), vec![]).contains("destination dataset"));
        assert!(repl_err(with(5, "maybe"), vec![]).contains("Force"));
        assert!(repl_err(with(6, "bogus"), vec![]).contains("Schedule"));
        assert!(repl_err(with(7, "q"), vec![]).contains("Catch up"));
        assert!(repl_err(["fbsd16", "backup/data", "hourly", "fbsd16", "backup/data", "n", "30 * * * *", "y"], vec![]).contains("same dataset"));
        let mut to_file = GOOD;
        to_file[3] = "file";
        to_file[4] = "/mnt/usb";
        assert!(repl_err(to_file, vec![]).contains("not supported"), "this build has no file destinations yet");
    }

    #[test]
    fn the_replication_form_refuses_a_destination_another_job_already_uses() {
        let other = crate::jobs::Job::test_replicate("other", "tank/elsewhere", "hourly", "fbsd16", "backup/data");
        assert!(repl_err(GOOD, vec![other.clone()]).contains("other"));
        // Editing that same job is fine.
        let kind = repl_kind(Some(other.clone()), vec![other]);
        assert!(build_replicate_job_action(&kind, &repl_fields(GOOD), std::path::Path::new("/x"), chrono::Utc::now()).is_ok());
    }

    #[test]
    fn enter_on_a_replicate_job_edits_it_in_the_replication_form() {
        let (mut app, _dir) = app_with_jobs(&[crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data")]);
        app.hosts = vec!["local".to_string(), "fbsd16".to_string()];
        app.refresh_jobs();
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Enter);
        assert_eq!(form_values(&app), ["local", "tank/data", "hourly", "fbsd16", "backup/data", "n", "30 * * * *", "y"]);
    }

    #[test]
    fn f2_on_the_replication_forms_fields_opens_the_right_pickers() {
        let (mut app, _dir) = app_with_jobs(&[]);
        app.hosts = vec!["local".to_string(), "fbsd16".to_string()];
        app.handle_key(KeyCode::Char('R'));
        app.handle_modal_key(KeyCode::F(2));
        assert_eq!(app.picker.as_ref().unwrap().values, vec!["local", "fbsd16"], "source host");
        app.picker = None;
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        assert_eq!(app.pending_pool_fetch, vec![(0, crate::ui::picker::LiveNames::Datasets)], "source dataset: the source host's datasets");
        app.picker = None;
        app.pending_pool_fetch.clear();
        app.handle_modal_key(KeyCode::Tab); // prefix: free text
        app.handle_modal_key(KeyCode::F(2));
        assert!(app.picker.is_none());
        app.handle_modal_key(KeyCode::Tab); // destination host
        app.handle_modal_key(KeyCode::F(2));
        assert_eq!(app.picker.as_ref().unwrap().values, vec!["local", "fbsd16", "file"], "destination host also offers file");
        app.picker = None;
        // Type the destination host, then F2 on the destination dataset lists *that* host's datasets.
        for c in "fbsd16".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        assert_eq!(app.pending_pool_fetch, vec![(1, crate::ui::picker::LiveNames::Datasets)]);
    }

    #[test]
    fn f2_on_the_destination_dataset_of_a_file_destination_says_to_type_a_path() {
        let (mut app, _dir) = app_with_jobs(&[]);
        app.handle_key(KeyCode::Char('R'));
        for _ in 0..3 {
            app.handle_modal_key(KeyCode::Tab);
        }
        for c in "file".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        assert!(matches!(&app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Failed(m) if m.contains("directory")));
        assert!(app.pending_pool_fetch.is_empty());
    }

    #[test]
    fn space_refuses_to_enable_a_replicate_job_that_clashes_on_its_destination() {
        let mut a = crate::jobs::Job::test_replicate("a", "tank/one", "hourly", "fbsd16", "backup/data");
        a.enabled = false;
        let b = crate::jobs::Job::test_replicate("b", "tank/two", "hourly", "fbsd16", "backup/data");
        let (mut app, _dir) = app_with_jobs(&[a, b]);
        app.refresh_jobs();
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Char(' '));
        assert!(app.pending_mutations.is_empty());
        assert!(app.status_message.clone().unwrap_or_default().contains("same destination"));
    }

    #[test]
    fn c_cancels_a_running_replication_and_is_inert_otherwise() {
        let repl = crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data");
        let (mut app, dir) = app_with_jobs(&[repl]);
        app.refresh_jobs();
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Char('c'));
        assert!(app.status_message.is_none(), "not running: nothing happens");
        assert!(crate::daemon::cancel::take_requests(&dir.path().join("cancel")).is_empty());
        let h = crate::daemon::history::History::new(dir.path().join("history.jsonl"));
        h.append(&crate::daemon::history::Event { job_id: "r".into(), run_id: "r1".into(), event: crate::daemon::history::EventKind::Started, at: chrono::Utc::now(), scheduled_for: None, result: None, message: String::new(), catch_up: false, summary: None }).unwrap();
        app.refresh_jobs();
        app.handle_key(KeyCode::Char('c'));
        assert_eq!(crate::daemon::cancel::take_requests(&dir.path().join("cancel")), ["r"]);
        assert!(app.status_message.clone().unwrap_or_default().contains("Cancel requested"));
    }

    fn be(name: &str, now: bool, reboot: bool, temp: bool, mount: Option<&str>) -> crate::zfs::bootenv::BootEnv {
        crate::zfs::bootenv::BootEnv {
            name: name.to_string(),
            flags: crate::zfs::bootenv::ActiveFlags { now, reboot, temp },
            mountpoint: mount.map(str::to_string),
            space: "1M".to_string(),
            created: "2026-10-07 19:26".to_string(),
        }
    }

    #[test]
    fn boot_envs_sits_between_shares_and_jobs() {
        let app = App::new(vec!["local".to_string()]);
        let at = |s| app.sections.iter().position(|x| *x == s).unwrap();
        assert_eq!(at(Section::BootEnvs), at(Section::Shares) + 1);
        assert_eq!(at(Section::Jobs), at(Section::BootEnvs) + 1);
        assert_eq!(app.sections.last(), Some(&Section::Jobs));
    }

    #[test]
    fn refresh_bootenvs_populates_app_state() {
        let mut app = App::new(vec!["local".to_string()]);
        app.refresh_bootenvs(runner("default\tNR\t/\t5.18G\t2026-07-24 18:48\nspare\t-\t-\t8K\t2026-10-07 19:26\n"));
        wait_until(&mut app, |a| !a.bootenvs.is_empty());
        assert_eq!(app.bootenvs.len(), 2);
        assert!(app.bootenvs[0].flags.now && app.bootenvs[0].flags.reboot);
        assert_eq!(app.bootenvs_unavailable, None);
    }

    #[test]
    fn a_failing_bectl_marks_the_section_unavailable_and_the_next_good_refresh_recovers() {
        let mut app = in_section(Section::BootEnvs);
        app.bootenvs = vec![be("stale", false, false, false, None)];
        app.bootenvs_table.select(Some(0));
        app.tx.send(WorkerMsg::BootEnvs(0, Err(crate::error::AppError::CommandFailed { cmd: "bectl list".into(), status: 127, stderr: "bectl: not found".into() }))).unwrap();
        app.drain_worker_messages();
        assert!(app.bootenvs.is_empty(), "a stale list must not look current");
        assert_eq!(app.bootenvs_table.selected(), None);
        assert!(app.bootenvs_unavailable.as_deref().is_some_and(|m| m.contains("bectl: not found")));
        // Not gated on the earlier failure: the periodic refresh still runs and recovers.
        app.auto_refresh(runner("default\tNR\t/\t1G\td\n"), Instant::now(), TEN);
        wait_until(&mut app, |a| !a.bootenvs.is_empty());
        assert_eq!(app.bootenvs_unavailable, None);
        assert_eq!(app.bootenvs.len(), 1);
    }

    #[test]
    fn a_host_with_zero_boot_environments_is_empty_not_unavailable() {
        let mut app = in_section(Section::BootEnvs);
        app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![]))).unwrap();
        app.drain_worker_messages();
        assert!(app.bootenvs.is_empty());
        assert_eq!(app.bootenvs_unavailable, None);
    }

    #[test]
    fn bootenvs_result_for_a_stale_host_is_discarded() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.tx.send(WorkerMsg::BootEnvs(1, Ok(vec![be("x", false, false, false, None)]))).unwrap();
        app.drain_worker_messages();
        assert!(app.bootenvs.is_empty());
    }

    #[test]
    fn switching_host_clears_boot_environment_state() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.bootenvs = vec![be("x", false, false, false, None)];
        app.bootenvs_table.select(Some(0));
        app.bootenvs_unavailable = Some("boom".to_string());
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::BackTab); // -> Hosts
        app.handle_key(KeyCode::Down); // switch host, triggers clear_host_data
        assert!(app.bootenvs.is_empty());
        assert_eq!(app.bootenvs_table.selected(), None);
        assert_eq!(app.bootenvs_unavailable, None);
    }

    #[test]
    fn a_refresh_keeps_the_selection_on_the_same_boot_environment_and_clamps_when_it_vanishes() {
        let mut app = in_section(Section::BootEnvs);
        app.bootenvs = vec![be("a", false, false, false, None), be("b", false, false, false, None)];
        app.bootenvs_table.select(Some(1));
        app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![be("0", false, false, false, None), be("a", false, false, false, None), be("b", false, false, false, None)]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.bootenvs_table.selected(), Some(2), "still on b");
        app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![be("a", false, false, false, None)]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.bootenvs_table.selected(), Some(0), "b vanished: clamped into range");
        app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.bootenvs_table.selected(), None);
    }

    #[test]
    fn up_and_down_move_the_boot_environment_selection() {
        let mut app = in_section(Section::BootEnvs);
        app.bootenvs = vec![be("a", false, false, false, None), be("b", false, false, false, None)];
        app.handle_key(KeyCode::Down);
        assert_eq!(app.bootenvs_table.selected(), Some(0));
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_bootenv().map(|b| b.name.as_str()), Some("b"));
        app.handle_key(KeyCode::Up);
        assert_eq!(app.bootenvs_table.selected(), Some(0));
    }
}
