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
    Alerts,
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
/// What the backup form's F5 check was asked about, and what it found (`None` while it still runs).
#[derive(Debug, Clone, PartialEq)]
pub struct PreflightCheck {
    pub host: usize,
    pub repository: String,
    pub password_file: String,
    pub result: Option<crate::restic::Preflight>,
}

/// A check waiting for the event loop to hand it its host's runner (`App` cannot resolve a host to one itself).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingPreflight {
    pub host: usize,
    pub host_name: String,
    pub repository: String,
    pub password_file: String,
}

/// Each variant carries the index (into `App::hosts`) of the host the
/// result is *about*, so a slow result for a host the user has since
/// navigated away from can be told apart from a fresh one instead of
/// silently overwriting the currently-selected host's pane.
pub enum WorkerMsg {
    /// The report preview the Alerts screen asked for (`p`), as `Subject: ...` then the body.
    AlertsPreview(String),
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
    /// The latest health probe of the named host (the background probe, not the Add Host check above).
    HostHealth(String, crate::zfs::host_health::HostProbe),
    /// The outcome of a live account fetch for host index `usize`.
    Accounts(usize, Result<crate::zfs::accounts::HostAccounts>),
    /// The outcome of a live pool-name fetch for the job form's Pool picker.
    PoolNames(usize, Result<Vec<String>>),
    /// The outcome of the backup form's F5 check: the host index, the repository and password file it was asked
    /// about (so a result for values since edited is dropped), and what it found.
    Preflight(usize, String, String, crate::restic::Preflight),
}

pub const ACCOUNT_CACHE_TTL: Duration = Duration::from_mins(1);
pub const ACCOUNT_FETCH_STUCK_AFTER: Duration = Duration::from_secs(30);

/// How often each host is probed for its health, and how far apart the first probes of different hosts start.
const HOST_PROBE_INTERVAL: Duration = Duration::from_secs(30);
const HOST_PROBE_STAGGER: Duration = Duration::from_secs(1);
/// A probe still running after this long is reported as "no answer".
const HOST_PROBE_GIVE_UP: Duration = Duration::from_secs(45);

/// What the probe loop knows about one host.
#[derive(Debug, Default)]
pub struct HostProbeState {
    pub last: Option<crate::zfs::host_health::HostProbe>,
    pub in_flight: bool,
    pub finished_at: Option<Instant>,
    /// When the probe now running started, so one that never answers can be flagged.
    pub started_at: Option<Instant>,
    /// The detected host type, kept once seen: it cannot change while the host is up, so it is asked for once.
    pub kind: Option<crate::zfs::host_health::HostKind>,
}

/// The address shown for a host: the host part of its `ssh_target` as configured (no lookup), `local` for the hub.
pub fn address_of(ssh_target: Option<&str>) -> String {
    match ssh_target {
        None => "local".to_string(),
        Some(t) => {
            let host = t.rsplit_once('@').map_or(t, |(_, h)| h).trim();
            if host.is_empty() { "-".to_string() } else { host.to_string() }
        }
    }
}

#[allow(clippy::struct_excessive_bools)] // independent UI state flags, not a state machine
pub struct App {
    pub sections: Vec<Section>,
    pub selected_section: usize,
    pub hosts: Vec<String>,
    pub selected_host: usize,
    pub pools: Vec<Pool>,
    pub datasets: Vec<Dataset>,
    pub snapshots: Vec<Snapshot>,
    /// `true` once a snapshot list has arrived for this host (an empty list counts), so a prompt that counts
    /// snapshots can say "unknown" instead of a misleading 0.
    pub snapshots_loaded: bool,
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
    /// `config.toml`, read and (for the alert settings) written by the Alerts screen.
    pub config_path: std::path::PathBuf,
    pub alerts_view: crate::ui::alerts::AlertsView,
    last_alerts_refresh: Option<Instant>,
    /// What to show once the mutation just dispatched succeeds (see `MutationCall::done_note`).
    pub done_note: Option<String>,
    pub alerts_preview_busy: bool,
    /// A test message is being sent (it can take up to 30 s per channel): `t` waits for it.
    pub alerts_test_busy: bool,
    /// A finished preview that arrived while another modal was open, shown on the next `p`.
    alerts_preview_ready: Option<String>,
    /// How many previews were started (a test seam: a second `p` while one runs must not start another).
    pub alerts_previews_started: u32,
    /// The address shown for each host (index-aligned with `hosts`); see `address_of`.
    pub host_addresses: Vec<String>,
    /// The tag of each host (index-aligned with `hosts`; empty = none).
    pub host_tags: Vec<String>,
    /// Tags typed in the Add Host form, by lowercase host name, until the host is confirmed reachable.
    pub new_host_tags: HashMap<String, String>,
    /// The Hosts filter text (empty = none) and whether it is being typed. Only the drawn rows and Up/Down use
    /// it: `hosts` and `selected_host` stay global indices.
    pub host_filter: String,
    pub host_filter_editing: bool,
    /// The host that was active when the filter input opened, put back if the input is cancelled.
    host_filter_origin: Option<usize>,
    /// The latest health probe of each host, by host name.
    pub host_probes: HashMap<String, HostProbeState>,
    /// When probing started: host `i` is first probed `i` seconds after it.
    pub probe_epoch: Instant,
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
    /// `true` once a list has succeeded on this host, so an empty list can say "no boot
    /// environments" instead of looking like one that has not loaded yet.
    pub bootenvs_loaded: bool,
    /// Why the Boot Envs section cannot list on this host (e.g. no `bectl`, not a boot-environment
    /// layout, an unreachable host), shown in its title. `None` once a list succeeds.
    pub bootenvs_unavailable: Option<String>,
    /// The boot environment a change is being made to, and since when: while it is set (and under two minutes
    /// old, so a lost result can never lock the section for good) the keys that change things wait.
    pub bootenv_busy: Option<(String, Instant)>,
    /// After a rename, the new name the selection should land on once the list shows it.
    pub bootenv_pin: Option<String>,
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
    /// The form a confirmation came from, tagged with the confirmation's prompt: declining the confirmation puts
    /// the form back (values and all) instead of discarding it. Only used if the tag still matches the prompt.
    pub confirm_restore: Option<(String, Modal)>,
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
    pub backup_preflight: Option<PreflightCheck>,
    pub pending_preflight: Vec<PendingPreflight>,
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
        let hosts_for_addr = hosts.clone();
        App {
            sections: vec![Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots, Section::Disks, Section::Shares, Section::BootEnvs, Section::Jobs, Section::Alerts],
            selected_section: 0,
            hosts,
            selected_host: 0,
            pools: Vec::new(),
            datasets: Vec::new(),
            snapshots: Vec::new(),
            snapshots_loaded: false,
            pools_table: TableState::default(),
            datasets_table: TableState::default(),
            snapshots_table: TableState::default(),
            disks: Vec::new(),
            disks_table: TableState::default(),
            selected_disks: HashSet::new(),
            shares: Vec::new(),
            shares_table: TableState::default(),
            jobs_view: crate::ui::jobs::JobsView::default(),
            host_tags: vec![String::new(); hosts_for_addr.len()],
            new_host_tags: HashMap::new(),
            host_filter: String::new(),
            host_filter_editing: false,
            host_filter_origin: None,
            host_addresses: hosts_for_addr.iter().map(|n| if n == "local" { "local".to_string() } else { "-".to_string() }).collect(),
            host_probes: HashMap::new(),
            probe_epoch: Instant::now(),
            jobs_table: TableState::default(),
            state_dir: std::path::PathBuf::from(crate::daemon::engine::DEFAULT_STATE_DIR),
            last_jobs_refresh: None,
            config_path: std::path::PathBuf::from(crate::config::DEFAULT_CONFIG_PATH),
            alerts_view: crate::ui::alerts::AlertsView::default(),
            last_alerts_refresh: None,
            done_note: None,
            alerts_preview_busy: false,
            alerts_test_busy: false,
            alerts_preview_ready: None,
            alerts_previews_started: 0,
            nfs_available: None,
            samba_available: None,
            shares_loaded: false,
            bootenvs: Vec::new(),
            bootenvs_table: TableState::default(),
            bootenvs_loaded: false,
            bootenvs_unavailable: None,
            bootenv_busy: None,
            bootenv_pin: None,
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
            confirm_restore: None,
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
            backup_preflight: None,
            pending_preflight: Vec::new(),
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
        let for_snapshots = runner.clone();
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
        // The destroy prompts count a dataset's snapshots, so the Datasets view refreshes the snapshot list
        // (quietly) with the datasets: the counts are as fresh as the list, never "unknown" or hours old.
        if section == Section::Datasets && !self.is_refreshing(Section::Snapshots, key.1) {
            self.refresh_snapshots(for_snapshots);
            self.quiet_refresh.insert((Section::Snapshots, key.1));
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
        let with_contents = self.metrics.want_contents(now);
        self.metrics.begin();
        let host = self.selected_host;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::metrics::sample_target(runner.as_ref(), &target, with_contents);
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
            Section::Alerts => self.refresh_alerts(),
            _ => {}
        }
    }

    /// Reloads the Jobs data from the state directory. These are small local files, so
    /// there is no runner and no thread. The selection stays on the same job.
    pub fn set_host_addresses(&mut self, addresses: Vec<String>) {
        self.host_addresses = addresses;
    }

    /// Whether host `index` matches `needle` (case-insensitive substring of its name, address, tag or type word).
    pub fn host_matches(&self, index: usize, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        let Some(name) = self.hosts.get(index) else { return false };
        let kind = self.host_probes.get(name).and_then(|s| s.kind.as_ref()).map_or("", |k| if matches!(k, crate::zfs::host_health::HostKind::Unknown) { "" } else { k.word() });
        [name.as_str(), self.host_address(index), self.host_tag(index), kind].iter().any(|f| f.to_lowercase().contains(&needle))
    }

    /// Global indices of the hosts the filter lets through, in order (all of them when there is no filter).
    pub fn visible_hosts(&self) -> Vec<usize> {
        (0..self.hosts.len()).filter(|i| self.host_filter.is_empty() || self.host_matches(*i, &self.host_filter)).collect()
    }

    /// Keeps the selection on a visible host when the filter hides it (so no section can act on a host the user
    /// cannot see). With no match at all the selection stays put: such a filter cannot be committed.
    fn ensure_visible_selection(&mut self) {
        if let Some(first) = self.visible_hosts().first().copied().filter(|_| !self.visible_hosts().contains(&self.selected_host)) {
            self.selected_host = first;
            self.clear_host_data();
        }
    }

    fn handle_filter_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char(c) => {
                self.host_filter.push(c);
                self.ensure_visible_selection();
            }
            KeyCode::Backspace => {
                self.host_filter.pop();
                self.ensure_visible_selection();
            }
            KeyCode::Esc => {
                self.host_filter.clear();
                self.host_filter_editing = false;
                if let Some(origin) = self.host_filter_origin.take().filter(|o| *o != self.selected_host && *o < self.hosts.len()) {
                    self.selected_host = origin;
                    self.clear_host_data();
                }
            }
            KeyCode::Enter if self.host_filter.is_empty() || !self.visible_hosts().is_empty() => {
                self.host_filter_editing = false;
                self.host_filter_origin = None;
            }
            _ => {}
        }
    }

    pub fn set_host_tags(&mut self, tags: Vec<String>) {
        self.host_tags = tags;
    }

    pub fn host_tag(&self, index: usize) -> &str {
        self.host_tags.get(index).map_or("", String::as_str)
    }

    pub fn host_address(&self, index: usize) -> &str {
        self.host_addresses.get(index).map_or("-", String::as_str)
    }

    /// Hosts whose probe is due: never probed (host `i` becomes due `i` seconds after start, so the hosts are
    /// not all probed in the same second) or last finished at least the interval ago; never while one is running.
    pub fn due_host_probes(&self, now: Instant) -> Vec<usize> {
        self.hosts
            .iter()
            .enumerate()
            .filter(|(i, name)| {
                let state = self.host_probes.get(*name);
                if state.is_some_and(|s| s.in_flight) {
                    return false;
                }
                match state.and_then(|s| s.finished_at) {
                    None => now >= self.probe_epoch + HOST_PROBE_STAGGER * u32::try_from(*i).unwrap_or(u32::MAX),
                    Some(done) => now.duration_since(done) >= HOST_PROBE_INTERVAL,
                }
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Starts one probe on a worker thread (the event loop resolves the runner; `App` cannot).
    pub fn start_host_probe(&mut self, index: usize, runner: Arc<dyn CommandRunner>) {
        let Some(name) = self.hosts.get(index).cloned() else { return };
        let state = self.host_probes.entry(name.clone()).or_default();
        state.in_flight = true;
        state.started_at = Some(Instant::now());
        let detect_kind = state.kind.is_none();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let probe = crate::zfs::host_health::probe_host_guarded(runner.as_ref(), detect_kind);
            let _ = tx.send(WorkerMsg::HostHealth(name, probe));
        });
    }

    /// The health of host `index` from its latest probe and from the Jobs data (an enabled job of this host
    /// whose latest run failed).
    pub fn host_assessment(&self, index: usize) -> crate::zfs::host_health::Assessment {
        let Some(name) = self.hosts.get(index) else { return crate::zfs::host_health::assess(None, false) };
        // A probe that has been running far longer than any healthy one (a hung ssh, a wedged pool) must not
        // leave the last good answer on screen.
        if let Some(waited) = self.host_probes.get(name).filter(|s| s.in_flight).and_then(|s| s.started_at).map(|t| t.elapsed()).filter(|d| *d > HOST_PROBE_GIVE_UP) {
            return crate::zfs::host_health::Assessment { level: crate::zfs::host_health::Level::Warning, reason: format!("no answer for {}s", waited.as_secs()) };
        }
        let job_failed = self.jobs_view.jobs.iter().any(|j| j.enabled && &j.host == name && self.jobs_view.last_run(&j.id).is_some_and(|r| r.result == Some(crate::daemon::history::RunResult::Failed)));
        crate::zfs::host_health::assess(self.host_probes.get(name).and_then(|s| s.last.as_ref()), job_failed)
    }

    /// `(failed, total)` over the enabled jobs of host `name`; failed means the latest run failed.
    pub fn host_jobs_counts(&self, name: &str) -> (usize, usize) {
        let mine = || self.jobs_view.jobs.iter().filter(|j| j.enabled && j.host == name);
        let failed = mine().filter(|j| self.jobs_view.last_run(&j.id).is_some_and(|r| r.result == Some(crate::daemon::history::RunResult::Failed))).count();
        (failed, mine().count())
    }

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

    /// `t`: asks (two keys) before sending a real test message through the saved channels.
    fn ask_to_send_test(&mut self) {
        if self.alerts_test_busy {
            self.status_message = Some("A test message is already being sent".to_string());
            return;
        }
        let Some(settings) = self.alerts_view.settings.clone() else {
            let problem = self.alerts_view.problem.clone().unwrap_or_default();
            self.set_error(format!("{problem}: fix config.toml before testing"));
            return;
        };
        let hook = self.alerts_view.hook.clone();
        if !settings.has_mail() && hook.is_none() {
            self.status_message = Some("Nothing to test: set an email address (e) or a notify_command in config.toml".to_string());
            return;
        }
        let mut prompt = String::from("Send a test message?\n");
        if settings.has_mail() {
            {
                use std::fmt::Write as _;
                let _ = write!(prompt, "  To: {}\n  Command: {}\n", settings.mail_to.join(", "), settings.mail_command);
            }
        }
        match (&hook, settings.has_mail()) {
            (Some(_), true) => prompt.push_str("  Also through the notify hook\n"),
            (Some(_), false) => prompt.push_str("  Through the notify hook\n"),
            (None, _) => {}
        }
        prompt.push_str("This sends a real message. (y/n)");
        let action = PendingAction { host: self.selected_host, section: Section::Alerts, call: MutationCall::SendTestMessage { settings: Box::new(settings), hook } };
        self.modal = Some(Modal::Confirm { choice: None, prompt, action });
    }

    /// `p`: builds the report as it would be sent now on a worker thread (probing the hosts can take a while) and
    /// shows it in a pop-up when ready. Nothing is sent or recorded.
    fn start_alerts_preview(&mut self) {
        if let Some(text) = self.alerts_preview_ready.take() {
            self.modal = Some(Modal::Info { title: "Report preview (nothing sent)".to_string(), body: text });
            return;
        }
        if self.alerts_preview_busy {
            return;
        }
        self.alerts_preview_busy = true;
        self.alerts_previews_started += 1;
        self.status_message = Some("Preparing the report preview (probing the hosts)…".to_string());
        let (path, state_dir, tx) = (self.config_path.clone(), self.state_dir.clone(), self.tx.clone());
        std::thread::spawn(move || {
            // A crash in the worker must still answer, or `p` would stay busy for the rest of the session.
            let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match crate::config::Config::load(&path) {
                Ok(config) => crate::notify_cli::report_text_with(&config, &state_dir, chrono::Utc::now(), &|hosts| {
                    crate::daemon::monitor::probe_all(hosts, &|_| true, crate::daemon::monitor::PROBE_DEADLINE, &crate::daemon::monitor::Busy::default())
                }),
                Err(e) => format!("config.toml could not be read: {e}"),
            }))
            .unwrap_or_else(|_| "The report preview crashed; nothing was sent.".to_string());
            let _ = tx.send(WorkerMsg::AlertsPreview(text));
        });
    }

    /// The edit form for the alert settings, pre-filled from the file; `None` while `config.toml` cannot be read.
    pub fn alerts_form(&self) -> Option<Modal> {
        if self.alerts_view.problem.as_deref().is_some_and(|p| p.starts_with("config.toml could not be read")) {
            return None;
        }
        let c = &self.alerts_view.saved;
        let field = |label: &str, value: String| FormField { label: label.to_string(), value };
        Some(Modal::Form {
            title: "Alert settings".to_string(),
            fields: vec![
                field("Email to", c.mail_to.clone().unwrap_or_default()),
                field("Mail command", c.mail_command.clone().unwrap_or_default()),
                field("Mail from", c.mail_from.clone().unwrap_or_default()),
                field("Check every (minutes)", c.check_interval_minutes.map(|n| n.to_string()).unwrap_or_default()),
                field("Daily report at (HH:MM or off)", c.report_time.clone().unwrap_or_default()),
                field("Remind every (hours, 0 = never)", c.remind_hours.map(|n| n.to_string()).unwrap_or_default()),
            ],
            focused: 0,
            description: Some("Blank means the default. Clear Email to to turn email off.".to_string()),
            kind: FormKind::NotifySettings,
        })
    }

    /// Reloads the Alerts screen's data from `config.toml` and the state directory; the scroll position stays.
    pub fn refresh_alerts(&mut self) {
        let (scroll, old_len) = (self.alerts_view.scroll, self.alerts_view.events.len());
        self.alerts_view = crate::ui::alerts::load(&self.config_path, &self.state_dir);
        let new_len = self.alerts_view.events.len();
        // New events are added at the front (newest first): when the operator has scrolled down, move with them so the
        // line being read stays where it is.
        let moved = if scroll > 0 { scroll + new_len.saturating_sub(old_len) } else { 0 };
        self.alerts_view.scroll = moved.min(new_len.saturating_sub(1));
    }

    /// Reloads the Alerts data every 5 seconds while that section is on screen (so a new event or a changed
    /// file shows up), and never otherwise.
    pub fn refresh_alerts_if_stale(&mut self, now: Instant) {
        if self.current_section() != Section::Alerts {
            return;
        }
        if self.last_alerts_refresh.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(5)) {
            self.last_alerts_refresh = Some(now);
            self.refresh_alerts();
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

    /// Why the ACL tab cannot edit the selected dataset, from its `acltype` property (only trusted when the
    /// loaded properties belong to this very dataset).
    pub fn acl_unsupported(&self) -> Option<&'static str> {
        let name = self.selected_dataset_name()?;
        if self.dataset_properties_for.as_deref() != Some(name.as_str()) {
            return None;
        }
        let acltype = self.dataset_properties.iter().find(|p| p.name == "acltype")?;
        (acltype.value == "posix").then_some("this dataset has acltype=posix: the ACL tab edits NFSv4 ACLs only")
    }

    /// Why `D` (zfs destroy -r) must not run on `name`, if it must not. A pool root would wipe the whole pool
    /// (verified: `zfs destroy -r <pool>` removes every dataset and snapshot); boot-environment datasets have
    /// their own, safer section; and anything holding the running system is never a candidate.
    fn recursive_destroy_refusal(&self, name: &str) -> Option<String> {
        if !name.contains('/') {
            return Some(pool_root_message(name));
        }
        if name.ends_with("/ROOT") || name.contains("/ROOT/") {
            return Some(format!("'{name}' is part of the boot environments: use the Boot Envs section, which refuses the running and next-boot ones"));
        }
        let prefix = format!("{name}/");
        let holds_root = self.datasets.iter().any(|d| d.mountpoint.as_deref() == Some("/") && (d.name == name || d.name.starts_with(&prefix)));
        holds_root.then(|| format!("'{name}' holds the running system (something under it is mounted at /): it cannot be destroyed recursively"))
    }

    /// How many child datasets and snapshots sit under `name` (the dataset's own snapshots included), from
    /// the lists loaded at the last refresh. Snapshots are `None` until a snapshot list has arrived.
    fn counts_under(&self, name: &str) -> (usize, Option<usize>) {
        let prefix = format!("{name}/");
        let children = self.datasets.iter().filter(|d| d.name.starts_with(&prefix)).count();
        let snapshots = self.snapshots_loaded.then(|| self.snapshots.iter().filter(|s| s.dataset == name || s.dataset.starts_with(&prefix)).count());
        (children, snapshots)
    }

    fn has_anything_under(&self, name: &str) -> bool {
        let (children, snapshots) = self.counts_under(name);
        children > 0 || snapshots.is_some_and(|n| n > 0)
    }

    /// "2 child datasets and 3 snapshots", or "... and snapshots: unknown (...)" before any snapshot list loaded.
    fn what_is_under(&self, name: &str) -> String {
        fn plural(n: usize, one: &str, many: &str) -> String {
            format!("{n} {}", if n == 1 { one } else { many })
        }
        let (children, snapshots) = self.counts_under(name);
        let kids = plural(children, "child dataset", "child datasets");
        match snapshots {
            Some(n) => format!("{kids} and {}", plural(n, "snapshot", "snapshots")),
            None => format!("{kids}; snapshots: unknown (open the Snapshots section to count them)"),
        }
    }

    /// A job save that newly arms something destructive (a forced receive, a retention that deletes snapshots or
    /// backups) is shown as a confirmation first; any other save goes straight through.
    fn confirm_job_save(&mut self, form: Modal, action: PendingAction) -> Option<PendingAction> {
        let MutationCall::SaveJob { job, .. } = &action.call else { return Some(action) };
        let old = match &form {
            Modal::Form { kind: FormKind::SnapshotJob { existing, .. } | FormKind::ReplicateJob { existing, .. } | FormKind::BackupJob { existing, .. }, .. } => existing.as_deref(),
            _ => None,
        };
        // The snapshot list is only known for the host on screen.
        let known = (self.snapshots_loaded && job.host == self.active_host_name()).then_some(self.snapshots.as_slice());
        match job_save_prompt(old, job, known) {
            Some(prompt) => {
                // Declining returns to the form, not to nothing.
                self.confirm_restore = Some((prompt.clone(), form));
                self.modal = Some(Modal::Confirm { choice: None, prompt, action });
                None
            }
            None => Some(action),
        }
    }

    /// Clears data panes so a stale render from the previously-selected
    /// host is never left on screen looking current after switching hosts.
    fn clear_host_data(&mut self) {
        self.metrics = crate::ui::metrics::MetricsView::default();
        self.pools.clear();
        self.datasets.clear();
        self.snapshots.clear();
        self.snapshots_loaded = false;
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
        self.bootenvs_loaded = false;
        self.bootenv_busy = None;
        self.bootenv_pin = None;
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
        if self.host_filter_editing {
            self.handle_filter_key(key);
            return;
        }
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
            KeyCode::Esc if self.current_section() == Section::Hosts && !self.host_filter.is_empty() => {
                self.host_filter.clear();
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
                self.status_message = self.acl_unsupported().map(str::to_string);
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
            KeyCode::Char('D') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                if let Some(name) = self.selected_dataset_name() {
                    if let Some(why) = self.recursive_destroy_refusal(&name) {
                        self.status_message = Some(why);
                        return;
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
                            return;
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
            KeyCode::Char('B') if self.current_section() == Section::Jobs => {
                self.backup_preflight = None;
                self.modal = Some(self.backup_job_form(None));
            }
            KeyCode::Char('R') if self.current_section() == Section::Jobs => {
                self.modal = Some(self.replicate_job_form(None));
            }
            KeyCode::Char('c') if self.current_section() == Section::Jobs => {
                if let Some(job) = self.jobs_table.selected().and_then(|i| self.jobs_view.jobs.get(i)) {
                    if crate::jobs::is_cancellable(job) && self.jobs_view.is_running(&job.id) {
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
                        crate::jobs::KIND_BACKUP => {
                            self.backup_preflight = None;
                            self.backup_job_form(Some(job))
                        }
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
                    self.modal = Some(Modal::Confirm { choice: None, prompt, action: PendingAction { host: self.selected_host, section: Section::Jobs, call } });
                }
            }
            KeyCode::Char('e') if self.current_section() == Section::Alerts && self.modal.is_none() && self.picker.is_none() => {
                if let Some(form) = self.alerts_form() {
                    self.modal = Some(form);
                } else {
                    let problem = self.alerts_view.problem.clone().unwrap_or_default();
                    self.set_error(format!("{problem}: fix config.toml before editing here"));
                }
            }
            KeyCode::Char('t') if self.current_section() == Section::Alerts && self.modal.is_none() && self.picker.is_none() => self.ask_to_send_test(),
            KeyCode::Char('p') if self.current_section() == Section::Alerts && self.modal.is_none() && self.picker.is_none() => self.start_alerts_preview(),
            KeyCode::Down if self.current_section() == Section::Alerts => {
                let last = self.alerts_view.events.len().saturating_sub(1);
                self.alerts_view.scroll = (self.alerts_view.scroll + 1).min(last);
            }
            KeyCode::Up if self.current_section() == Section::Alerts => {
                self.alerts_view.scroll = self.alerts_view.scroll.saturating_sub(1);
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
                        self.modal = Some(Modal::Confirm { choice: None, prompt, action: PendingAction { host: self.selected_host, section: Section::Shares, call } });
                    }
                }
            }
            // A change to a boot environment is still running: do nothing else to the section until it has finished.
            KeyCode::Char('n' | 'a' | 't' | 'T' | 'R' | 'm' | 'u' | 'd' | 'D') if self.current_section() == Section::BootEnvs && self.bootenv_busy_name().is_some() => {
                let name = self.bootenv_busy_name().unwrap_or_default().to_string();
                self.status_message = Some(format!("Still working on '{name}'… wait for it to finish"));
            }
            KeyCode::Char('n') if self.current_section() == Section::BootEnvs && self.bootenvs_unavailable.is_none() => {
                let existing: Vec<String> = self.bootenvs.iter().map(|b| b.name.clone()).collect();
                self.modal = Some(Modal::Form {
                    title: "New boot environment".to_string(),
                    fields: vec![
                        FormField { label: "Name (letters, digits, _ - . :)".to_string(), value: String::new() },
                        FormField { label: "Clone from (F2 to pick; blank = the running boot environment)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    description: None,
                    kind: FormKind::CreateBootEnv { host: self.selected_host, existing },
                });
            }
            KeyCode::Char('a') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_activate()).map(|b| b.name.clone()) else { return };
                let prompt = format!("Make '{name}' the boot environment for every future boot? (y/n)");
                let call = MutationCall::ActivateBootEnv { name, temporary: false };
                self.modal = Some(Modal::Confirm { choice: None, prompt, action: PendingAction { host: self.selected_host, section: Section::BootEnvs, call } });
            }
            KeyCode::Char('t') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_activate_temporarily()).map(|b| b.name.clone()) else { return };
                let prompt = format!("Boot '{name}' on the NEXT boot only? Later boots go back to the usual boot environment. (y/n)");
                let call = MutationCall::ActivateBootEnv { name, temporary: true };
                self.modal = Some(Modal::Confirm { choice: None, prompt, action: PendingAction { host: self.selected_host, section: Section::BootEnvs, call } });
            }
            KeyCode::Char('T') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_clear_temporary()).map(|b| b.name.clone()) else { return };
                self.status_message = Some(format!("Clearing the next-boot activation of '{name}'…"));
                let call = MutationCall::ClearTemporaryBootEnv { name };
                self.queue_bootenv_change(call);
            }
            KeyCode::Char('R') if self.current_section() == Section::BootEnvs => {
                let Some(from) = self.selected_bootenv().map(|b| b.name.clone()) else { return };
                let existing: Vec<String> = self.bootenvs.iter().map(|b| b.name.clone()).collect();
                self.modal = Some(Modal::Form {
                    title: format!("Rename boot environment '{from}'"),
                    fields: vec![FormField { label: "New name (letters, digits, _ - . :)".to_string(), value: String::new() }],
                    focused: 0,
                    description: None,
                    kind: FormKind::RenameBootEnv { host: self.selected_host, from, existing },
                });
            }
            KeyCode::Char('m') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_mount()).map(|b| b.name.clone()) else { return };
                self.status_message = Some(format!("Mounting '{name}'…"));
                let call = MutationCall::MountBootEnv { name };
                self.queue_bootenv_change(call);
            }
            KeyCode::Char('u') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_unmount()).map(|b| b.name.clone()) else { return };
                self.status_message = Some(format!("Unmounting '{name}'…"));
                let call = MutationCall::UnmountBootEnv { name };
                self.queue_bootenv_change(call);
            }
            KeyCode::Char(key @ ('d' | 'D')) if self.current_section() == Section::BootEnvs => {
                let Some(be) = self.selected_bootenv() else { return };
                let name = be.name.clone();
                if !be.can_destroy() {
                    self.status_message = Some(format!("'{name}' cannot be destroyed: it is running, set for the next boot, or marked temporary (T clears that)"));
                    return;
                }
                let origin = key == 'D';
                let also = if origin { " and its origin snapshot" } else { "" };
                self.modal = Some(Modal::ConfirmTyped {
                    prompt: format!("Type the boot environment name to destroy '{name}'{also}:"),
                    expected: name.clone(),
                    input: String::new(),
                    action: PendingAction { host: self.selected_host, section: Section::BootEnvs, call: MutationCall::DestroyBootEnv { name, origin } },
                });
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
        self.modal = Some(Modal::Confirm { choice: None,
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
            // Two steps for both outcomes: `y` / `n` / Esc only select, Enter applies the selection.
            // Enter with nothing selected, and every other key, change nothing.
            Modal::Confirm { prompt, action, choice } => match (key, choice) {
                (KeyCode::Enter, Some(true)) => {
                    self.confirm_restore = None;
                    Some(action)
                }
                (KeyCode::Enter, Some(false)) => {
                    // A confirmation that came from a form hands the form back, values intact.
                    if let Some((tag, form)) = self.confirm_restore.take() {
                        if tag == prompt {
                            self.modal = Some(form);
                        }
                    }
                    None
                }
                (KeyCode::Char('y' | 'Y'), _) => {
                    self.modal = Some(Modal::Confirm { prompt, action, choice: Some(true) });
                    None
                }
                (KeyCode::Char('n' | 'N') | KeyCode::Esc, _) => {
                    self.modal = Some(Modal::Confirm { prompt, action, choice: Some(false) });
                    None
                }
                _ => {
                    self.modal = Some(Modal::Confirm { prompt, action, choice });
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
                    } else if matches!(kind, FormKind::NotifySettings) {
                        match build_notify_action(&fields, &self.config_path) {
                            Ok((mut action, prompt)) => {
                                action.host = self.selected_host;
                                // Declining returns to the form, not to nothing.
                                self.confirm_restore = Some((prompt.clone(), Modal::Form { title, fields, focused, description, kind }));
                                self.modal = Some(Modal::Confirm { choice: None, prompt, action });
                                None
                            }
                            Err(msg) => {
                                self.set_error(msg);
                                self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
                                None
                            }
                        }
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
                            self.pending_probes.insert(name_lower.clone());
                            if let Some(tag) = crate::config::clean_tag(field_value(&fields, 2)) {
                                self.new_host_tags.insert(name_lower, tag);
                            }
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
                                self.confirm_job_save(Modal::Form { title, fields, focused, description, kind }, action)
                            }
                            Err(msg) => {
                                self.set_error(msg);
                                self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
                                None
                            }
                        }
                    } else if matches!(kind, FormKind::BackupJob { .. }) {
                        match build_backup_job_action(&kind, &fields, &self.state_dir, chrono::Utc::now()) {
                            Ok(mut action) => {
                                action.host = self.selected_host;
                                self.confirm_job_save(Modal::Form { title, fields, focused, description, kind }, action)
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
                                self.confirm_job_save(Modal::Form { title, fields, focused, description, kind }, action)
                            }
                            Err(msg) => {
                                self.set_error(msg);
                                self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
                                None
                            }
                        }
                    } else if matches!(kind, FormKind::CreateBootEnv { .. } | FormKind::RenameBootEnv { .. }) {
                        match build_bootenv_action(&kind, &fields) {
                            Ok(action) => {
                                if let MutationCall::RenameBootEnv { to, .. } = &action.call {
                                    self.bootenv_pin = Some(to.clone());
                                }
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
                    } else if let Some(action) = build_pending_action(&kind, &fields) {
                        // What cannot be taken back is shown as the exact command and needs y then Enter.
                        if let Some(prompt) = irreversible_prompt(&self.hosts, &action) {
                            self.confirm_restore = Some((prompt.clone(), Modal::Form { title, fields, focused, description, kind }));
                            self.modal = Some(Modal::Confirm { choice: None, prompt, action });
                            None
                        } else {
                            Some(action)
                        }
                    } else {
                        self.set_error(form_problem(&kind, &fields));
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
                KeyCode::F(5) if matches!(kind, FormKind::BackupJob { .. }) => {
                    self.request_backup_preflight(field_value(&fields, 0).trim(), field_value(&fields, 5).trim(), field_value(&fields, 6).trim());
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
                            // The backup form's Source field lists the snapshots of the form's dataset.
                            let snapshots_of = (matches!(kind, FormKind::BackupJob { .. }) && focused == 3).then(|| field_value(&fields, 1).trim().to_string());
                            self.open_pool_picker(&mut picker, &kind, field_value(&fields, host_field), snapshots_of);
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
    fn open_pool_picker(&mut self, picker: &mut PickerState, kind: &FormKind, host_name: &str, snapshots_of: Option<String>) {
        let (hosts, mut what) = match kind {
            FormKind::Job { hosts, .. } => (hosts, crate::ui::picker::LiveNames::Pools),
            FormKind::SnapshotJob { hosts, .. } | FormKind::ReplicateJob { hosts, .. } | FormKind::BackupJob { hosts, .. } => (hosts, crate::ui::picker::LiveNames::Datasets),
            _ => return,
        };
        if host_name.trim() == crate::jobs::DEST_FILE {
            picker.load = crate::ui::picker::PickerLoad::Failed("the destination is a directory on the hub: type its path".to_string());
            return;
        }
        if let Some(dataset) = snapshots_of {
            if !crate::jobs::valid_dataset(&dataset) {
                picker.load = crate::ui::picker::PickerLoad::Failed("enter the dataset first: the snapshots listed are that dataset's".to_string());
                return;
            }
            what = crate::ui::picker::LiveNames::Snapshots(dataset);
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
                crate::ui::picker::LiveNames::Snapshots(dataset) => {
                    crate::zfs::replication::list_snapshot_infos_or_empty(runner.as_ref(), &dataset).map(|v| crate::ui::picker::source_choices(&v.into_iter().map(|s| s.name).collect::<Vec<_>>()))
                }
            };
            let _ = tx.send(WorkerMsg::PoolNames(host, result));
        });
    }

    /// F5 in the backup form: validates what the save would validate, then queues the check for the event loop.
    /// A check already running for the same values is left alone.
    fn request_backup_preflight(&mut self, host_name: &str, repository: &str, password_file: &str) {
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
        self.backup_preflight = Some(PreflightCheck { host, repository: repository.to_string(), password_file: password_file.to_string(), result: None });
        self.pending_preflight.push(PendingPreflight { host, host_name: host_name.to_string(), repository: repository.to_string(), password_file: password_file.to_string() });
    }

    /// Runs one queued check on a worker thread and reports it as `WorkerMsg::Preflight`.
    pub fn run_backup_preflight(&mut self, runner: Arc<dyn CommandRunner>, pending: PendingPreflight) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::preflight::run(runner, &pending.host_name, &pending.repository, &pending.password_file, crate::preflight::DEADLINE);
            let _ = tx.send(WorkerMsg::Preflight(pending.host, pending.repository, pending.password_file, result));
        });
    }

    /// Reports a check that could not start (the host index no longer resolves) through the same channel.
    pub fn fail_backup_preflight(&self, pending: PendingPreflight, message: String) {
        let _ = self.tx.send(WorkerMsg::Preflight(pending.host, pending.repository, pending.password_file, crate::restic::Preflight::Problem(message)));
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
        if let Some(note) = call.done_note() {
            self.done_note = Some(note);
        }
        if matches!(call, MutationCall::SendTestMessage { .. }) {
            self.alerts_test_busy = true;
            self.status_message = Some("Sending the test message…".to_string());
        }
        if section == Section::BootEnvs {
            self.bootenv_busy = Some((bootenv_call_name(&call).to_string(), Instant::now()));
        }
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

    /// The boot environment a change is in flight for, unless that marker is stale.
    fn bootenv_busy_name(&self) -> Option<&str> {
        const STALE: Duration = Duration::from_mins(2);
        self.bootenv_busy.as_ref().filter(|(_, since)| since.elapsed() < STALE).map(|(name, _)| name.as_str())
    }

    /// Queues a change to run on the next loop tick and marks the section busy right away, so a second key
    /// pressed before it is dispatched cannot queue a duplicate.
    fn queue_bootenv_change(&mut self, call: MutationCall) {
        self.bootenv_busy = Some((bootenv_call_name(&call).to_string(), Instant::now()));
        self.pending_mutations.push(PendingAction { host: self.selected_host, section: Section::BootEnvs, call });
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
    /// `probe_reachable`, which checks the exit status (a raw `SshRunner`
    /// call returns `Ok` even for an unreachable host or a rejected
    /// key, just with a non-zero `CommandOutput::status`) and runs
    /// without sudo.
    pub fn begin_add_host(&mut self, name: String, target: String) {
        self.status_message = Some(format!("Connecting to '{target}'..."));
        let tx = self.tx.clone();
        let name_for_thread = name;
        let target_for_thread = target;
        std::thread::spawn(move || {
            let runner = crate::exec::ssh::SshRunner::new(target_for_thread.clone());
            let result = probe_reachable(&runner, &target_for_thread);
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
                            self.snapshots_loaded = true;
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
                            // A just-renamed boot environment keeps the selection under its new name, once the
                            // list shows it (an earlier refresh that still has the old name must not use up the pin).
                            let wanted = self.bootenv_pin.clone().filter(|n| envs.iter().any(|b| &b.name == n));
                            let previous = wanted.clone().or_else(|| self.selected_bootenv().map(|b| b.name.clone()));
                            if wanted.is_some() {
                                self.bootenv_pin = None;
                            }
                            self.bootenvs = envs;
                            pin_selection(&mut self.bootenvs_table, previous, &self.bootenvs, |b| b.name.clone());
                            self.bootenvs_unavailable = None;
                            self.bootenvs_loaded = true;
                            self.clear_status_after_refresh(quiet);
                        }
                        Err(e) => {
                            // Not `set_error`: a host without bectl would otherwise raise a fresh
                            // error every auto-refresh. The title carries the reason instead.
                            self.bootenvs.clear();
                            self.bootenvs_table.select(None);
                            self.bootenvs_unavailable = Some(e.to_string());
                            self.bootenvs_loaded = false;
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
                WorkerMsg::Preflight(host, repository, password_file, result) => {
                    // Only the answer to the question still asked is kept: a result for edited values is dropped.
                    if let Some(check) = self.backup_preflight.as_mut().filter(|c| c.host == host && c.repository == repository && c.password_file == password_file && c.result.is_none()) {
                        check.result = Some(result);
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
                WorkerMsg::HostHealth(name, probe) => {
                    // A result for a host that was removed or renamed while the probe ran is dropped.
                    if self.hosts.contains(&name) {
                        let state = self.host_probes.entry(name).or_default();
                        state.in_flight = false;
                        if let crate::zfs::host_health::HostProbe::Reached { kind: Some(k), .. } = &probe {
                            state.kind = Some(k.clone());
                        }
                        state.last = Some(probe);
                        state.finished_at = Some(Instant::now());
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
                            self.new_host_tags.remove(&name.to_lowercase());
                            self.set_error(format!("Could not add host '{name}': {e}"));
                        }
                    }
                }
                WorkerMsg::AlertsPreview(text) => {
                    self.alerts_preview_busy = false;
                    if self.modal.is_some() || self.picker.is_some() {
                        // Never replace something the operator is typing: keep it for the next `p`.
                        self.alerts_preview_ready = Some(text);
                        self.status_message = Some("The report preview is ready: press p on the Alerts screen".to_string());
                    } else {
                        self.status_message = None;
                        self.modal = Some(Modal::Info { title: "Report preview (nothing sent)".to_string(), body: text });
                    }
                }
                WorkerMsg::MutationDone(host, section, result) => {
                    if section == Section::Alerts {
                        self.alerts_test_busy = false;
                    }
                    // The Alerts calls are local to the hub: their result matters whichever host is selected now.
                    if host != self.selected_host && section != Section::Alerts {
                        continue;
                    }
                    if section == Section::BootEnvs {
                        self.bootenv_busy = None;
                        if result.is_err() {
                            self.bootenv_pin = None;
                        }
                    }
                    let note = if section == Section::Alerts { self.done_note.take() } else { None };
                    match result {
                        Ok(()) => {
                            self.status_message = note;
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

impl App {
    fn backup_job_form(&self, existing: Option<crate::jobs::Job>) -> Modal {
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
fn build_backup_job_action(kind: &FormKind, fields: &[FormField], state_dir: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> std::result::Result<PendingAction, String> {
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
            force: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None,
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

/// Turns the Alerts form into the save and the text of its confirmation, or says what is wrong. The values are
/// checked by `NotifyConfig::resolve`, exactly as the daemon will check them.
fn build_notify_action(fields: &[FormField], config_path: &std::path::Path) -> std::result::Result<(PendingAction, String), String> {
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

/// The text of form field `index`; a missing field reads as empty, which every caller
/// already treats as "not filled in" (forms are built with a fixed field count per kind).
fn field_value(fields: &[FormField], index: usize) -> &str {
    fields.get(index).map_or("", |f| f.value.as_str())
}

/// The confirmation text for a form whose result cannot be undone: the host, the exact command on a line of
/// its own, then what makes it irreversible.
fn irreversible_prompt(hosts: &[String], action: &PendingAction) -> Option<String> {
    let host = hosts.get(action.host).map_or("?", String::as_str);
    let ask = |command: String, why: &str| Some(format!("On {host}:\n  {command}\n\n{why} (y/n)"));
    match &action.call {
        MutationCall::CreatePool { name, vdev_spec } => ask(
            format!("zpool create {name} {}", vdev_spec.join(" ")),
            "This overwrites whatever is on those disks. Create the pool?",
        ),
        MutationCall::AddVdev { pool, vdev_spec } => ask(
            format!("zpool add {pool} {}", vdev_spec.join(" ")),
            "A vdev added to a pool usually cannot be removed again. Add it?",
        ),
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
        _ => None,
    }
}

/// The confirmation for saving a job that, from its next run on, destroys data the previous version of the job
/// (`old`, when editing) did not: a replication that now forces the receive, a snapshot job whose `keep` would
/// destroy snapshots, a backup job whose retention would forget backups. `snapshots` is the list for the job's
/// host when it is known; without it a snapshot job cannot say how many it would destroy.
fn job_save_prompt(old: Option<&crate::jobs::Job>, new: &crate::jobs::Job, snapshots: Option<&[crate::zfs::snapshot::Snapshot]>) -> Option<String> {
    use crate::jobs::{effective_prefix, KIND_BACKUP, KIND_REPLICATE, KIND_SNAPSHOT};
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
        _ => None,
    }
}

/// Builds the `PendingAction` for a submitted form, mapping its fields
/// positionally per `FormKind`'s documented field order. Returns `None`
/// if a required field is empty — the caller keeps the form modal open
/// in that case rather than losing the user's input.
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
        FormKind::NotifySettings | FormKind::AddHost | FormKind::ShareSmb { .. } | FormKind::CreateBootEnv { .. } | FormKind::RenameBootEnv { .. } | FormKind::BackupJob { .. } | FormKind::Job { .. } | FormKind::SnapshotJob { .. } | FormKind::ReplicateJob { .. } => None,
    }
}

/// The boot environment a boot-environment call is about (the old name for a rename).
fn bootenv_call_name(call: &MutationCall) -> &str {
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

fn build_bootenv_action(kind: &FormKind, fields: &[FormField]) -> std::result::Result<PendingAction, String> {
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
fn pool_root_message(pool: &str) -> String {
    format!("'{pool}' is a pool: destroying it as a dataset would remove everything in it. Use the Pools section (d) to destroy a pool")
}

/// Splits `user:carol`, ` User : carol `, `GROUP:wheel` into a lower-case keyword and a trimmed name (the
/// name keeps its case). `None` when there is no colon or the name is blank.
fn split_recipient(s: &str) -> Option<(String, &str)> {
    let (keyword, name) = s.split_once(':')?;
    let name = name.trim();
    (!name.is_empty()).then(|| (keyword.trim().to_ascii_lowercase(), name))
}

fn parse_recipient_field(s: &str) -> Option<crate::zfs::permission::Recipient> {
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
    // A token starting with `-` would be read as an option by zfs/setfacl: refuse it here, with the form open.
    if perms.is_empty() || perms.iter().any(|p| p.starts_with('-')) {
        None
    } else {
        Some(perms)
    }
}

/// What is wrong with a required permissions field, in the words the form shows.
fn perms_problem(s: &str) -> String {
    match s.split(',').map(str::trim).find(|p| p.starts_with('-')) {
        Some(token) => format!("Permissions: '{token}' is not a permission (it starts with '-')"),
        None => "Permissions: choose at least one (F2 lists them)".to_string(),
    }
}

/// The reason `build_pending_action` returned `None` for this form, naming the field, so the form can stay
/// open with the typing intact and say what to fix.
fn form_problem(kind: &FormKind, fields: &[FormField]) -> String {
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

/// The Help text, for the book's drift test.
#[cfg(test)]
pub(crate) fn help_text_for_book() -> String {
    help_text()
}

fn help_text() -> String {
    "\
Global
  Tab / Shift+Tab  switch sections
  Up / Down        select a row (host/pool/dataset/snapshot/disk/share, or a property/permission/ACL row)
  r                refresh the focused pane
  F2               in a form, on a field with known values (Permissions, Flags, Type, Recipient, Valid user, Root user mapping, a pool property with fixed values such as failmode or autoexpand, a from-disks Redundancy, a job's Host, Pool or Dataset, a replication's Destination host, and Schedule): open a picker (type filter, Enter OK, Esc cancel; Space toggles in multi-select ones)
  F5               in the backup job form: check the repository and password file on the job's host (read-only; never blocks saving)
  Esc              dismiss a status message, back out of a focused pane, or cancel a modal (in a y/n confirmation Esc selects No, then Enter)
  y / n, Enter     a y/n confirmation takes two keys: y (yes) or n / Esc (no) selects, then Enter applies it; Enter alone does nothing. Forms that cannot be undone (create pool, add vdev, replace disk, revoke, shrinking a volume, pool features) end in one, showing the exact command
  ?                this Help screen
  !                open a shell on the selected host (remote hosts: your unprivileged ssh login); exit returns to zfsctl
  A                About
  q                quit

Hosts
  Up / Down        select the active host
  /                filter the host list by name, address, tag or type (Enter keeps it, Esc clears it)
  n                add a new host (prompts for Name, SSH target and an optional Tag)

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
  n / d            create / destroy a dataset (type its name; zfs refuses if it has snapshots or children)
  D                destroy a dataset AND its child datasets and snapshots (type its name; says how many; never -R)
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

Boot Envs
  n                new boot environment (F2 on the second field picks one to clone)
  a                make the selected one the boot environment for every future boot (confirms)
  t / T            use it for the next boot only (confirms) / clear that
  R                rename
  m / u            mount / unmount (the Mountpoint column shows where)
  d / D            destroy (type its name) / destroy and remove its origin snapshot

Jobs
  Up / Down        select a scheduled job
  Enter            edit the job (host, pool, schedule, catch up)
  n                new scrub job (or press J on a pool in Pools)
  N                new snapshot job (or press J on a dataset in Datasets)
  R                new replication job
  B                new backup job (restic: back up a snapshot through a temporary clone)
  c                cancel the selected replication or backup while it is running
  Space            enable / disable the job
  d                delete the job (y/n; its past runs stay in the history)

Alerts
  e                edit the alert settings (saved into the [notify] section of config.toml only; y/n to confirm)
  t                send a test message through the saved settings (a real message; asks first)
  p                preview the daily report as it would be sent now (sends nothing)
  Up / Down        scroll the recent events

Press any key to close."
        .to_string()
}

/// The Add Host reachability check: plain `true` over ssh, deliberately WITHOUT sudo. The documented sudoers
/// entry is path-pinned and has no `true`, so `sudo -n true` would reject every correctly set-up host. A
/// non-zero exit (rejected key, unreachable host) is an error carrying ssh's own message.
fn probe_reachable(runner: &dyn CommandRunner, target: &str) -> Result<()> {
    let out = runner.run_unprivileged("true", &[])?;
    if out.success() {
        Ok(())
    } else {
        Err(crate::error::AppError::CommandFailed { cmd: format!("ssh {target}"), status: out.status, stderr: out.stderr })
    }
}

#[cfg(test)]
mod tests;
