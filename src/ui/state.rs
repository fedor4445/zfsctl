use crate::error::Result;
use crate::exec::CommandRunner;
use crate::ui::modal::{FormField, FormKind, Modal, MutationCall, PendingAction};
use crate::ui::picker::PickerState;
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

mod forms;
mod modal_keys;
mod worker;
use self::forms::{bootenv_call_name, job_save_prompt, permission_target_for_row, pool_root_message, share_edit_modal, smb_edit_modal};

/// How long a completed scrub/resilver's status stays visible in the
/// status bar before `clear_stale_scan_status` clears it.
const POOL_SCAN_STATUS_DISPLAY_DURATION: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Dashboard,
    Hosts,
    Pools,
    Datasets,
    Snapshots,
    Disks,
    Shares,
    BootEnvs,
    Jobs,
    Cloud,
    Alerts,
    Settings,
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
    /// Advice about the password file's location, mode and owner, once the check has answered.
    pub file_note: Option<String>,
}

/// A check waiting for the event loop to hand it its host's runner (`App` cannot resolve a host to one itself).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingPreflight {
    pub host: usize,
    pub host_name: String,
    pub repository: String,
    pub password_file: String,
}

/// A Cloud section lookup waiting for the event loop to hand it its host's runner. Both only read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCloud {
    pub host: usize,
    pub host_name: String,
    pub work: CloudWork,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudWork {
    /// `rclone listremotes --long`.
    Remotes,
    /// `rclone size` of a cloud mirror job's `current/` and `old/`.
    Size { job_id: String, remote: String },
}

/// A replication form whose save waits for the destination's snapshot list (to say what Keep on target
/// would destroy). The form and the save are kept so either can go on once the list arrives.
#[derive(Debug, Clone, PartialEq)]
pub struct PruneCheck {
    pub host: usize,
    pub host_name: String,
    pub dest_dataset: String,
    pub prefix: String,
    pub keep: u32,
    pub form: Modal,
    pub action: PendingAction,
}

/// A destination listing waiting for the event loop to hand it the destination host's runner.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingPruneCheck {
    pub host: usize,
    pub host_name: String,
    pub dest_dataset: String,
}

/// Each variant carries the index (into `App::hosts`) of the host the
/// result is *about*, so a slow result for a host the user has since
/// navigated away from can be told apart from a fresh one instead of
/// silently overwriting the currently-selected host's pane.
pub enum WorkerMsg {
    /// The report preview the Alerts screen asked for (`p`), as `Subject: ...` then the body.
    AlertsPreview(String),
    /// A check of one host from the Hosts section finished: the host and the line to show.
    HostChecked(String, String),
    Pools(usize, Result<Vec<Pool>>),
    Datasets(usize, Result<Vec<Dataset>>),
    Snapshots(usize, Result<Vec<Snapshot>>),
    Disks(usize, Result<Vec<DiskInfo>>),
    Shares(usize, Result<SharesSnapshot>),
    BootEnvs(usize, Result<Vec<crate::zfs::bootenv::BootEnv>>),
    /// The host's name and the rclone remotes it has, or why they could not be listed.
    CloudRemotes(String, std::result::Result<Vec<(String, String)>, String>),
    /// A cloud mirror job's id and what was measured of its bucket folders.
    CloudSize(String, crate::ui::cloud::Size),
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
    /// The host's memory, CPU, load and swap from the probe just made (`None` when it could not be read), and its UTC
    /// offset if this probe had to ask for it.
    HostResources(String, Option<crate::zfs::host_resources::Resources>, Option<i32>),
    /// The outcome of a live account fetch for host index `usize`.
    Accounts(usize, Result<crate::zfs::accounts::HostAccounts>),
    /// The outcome of a live pool-name fetch for the job form's Pool picker.
    PoolNames(usize, Result<Vec<String>>),
    /// The outcome of the backup form's F5 check: the host index, the repository and password file it was asked
    /// about (so a result for values since edited is dropped), and what it found.
    Preflight(usize, String, String, crate::restic::Preflight, Option<String>),
    /// The destination's snapshots for a replication form's Keep on target check: the host index, the dataset, the listing.
    PrunePreview(usize, String, Result<Vec<crate::zfs::replication::SnapInfo>>),
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
    /// The detected operating system, kept once seen (it is read with the type, in the first successful probe).
    pub os: Option<crate::zfs::host_health::HostOs>,
    /// The short name for the Hosts table (`Linux 6.8 (Ubuntu 24)`), read with the OS.
    pub os_name: Option<String>,
    /// The last two resource readings, oldest first: the CPU share is the difference between them.
    pub resources: Vec<crate::zfs::host_resources::Resources>,
    /// The host's offset from UTC in seconds, asked for once: it turns a scrub's local time into an age.
    pub utc_offset_secs: Option<i32>,
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
    pub cloud_table: TableState,
    /// The rclone remotes each host reported (by host name), why a host could not, and what `s` measured (by job id).
    pub cloud_known: crate::ui::cloud::Known,
    pub cloud_errors: HashMap<String, String>,
    pub cloud_sizes: HashMap<String, crate::ui::cloud::Size>,
    /// `[cloud.caps]` of `config.toml`, read each time the Cloud list is asked for.
    pub cloud_caps: crate::ui::cloud::Caps,
    /// `[dashboard]` of `config.toml`, read each time the Dashboard refreshes.
    pub dashboard_thresholds: crate::config::Thresholds,
    /// The selected line of the Dashboard's Needs attention list.
    pub dashboard_table: TableState,
    pub pending_cloud: Vec<PendingCloud>,
    /// Lookups that were started and have not answered: `remotes:<host>` or `size:<job id>`.
    cloud_busy: HashSet<String>,
    /// Where `jobs.toml`, `history.jsonl` and `daemon.json` live: the daemon's state directory.
    pub state_dir: std::path::PathBuf,
    last_jobs_refresh: Option<Instant>,
    last_cloud_refresh: Option<Instant>,
    /// `config.toml`, read and (for the alert settings) written by the Alerts screen.
    pub config_path: std::path::PathBuf,
    pub alerts_view: crate::ui::alerts::AlertsView,
    /// The Settings section: the rest of `config.toml` that can be edited here.
    pub settings_view: crate::ui::settings::SettingsView,
    pub settings_table: TableState,
    /// What the daemon's daily disk check found (`smart.json`), reloaded with the Dashboard.
    pub smart: crate::daemon::smart_check::SmartFile,
    pub shares_check: crate::daemon::shares_check::SharesFile,
    /// The check that is running now (a fresh `checks-running.json`), if any.
    pub checks_marker: Option<crate::daemon::checks::Marker>,
    /// How often the list on screen reloads by itself; `None` is off. Set from `[ui]` at start and after a save in Settings.
    pub auto_refresh_interval: Option<Duration>,
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
    pub prune_check: Option<PruneCheck>,
    pub pending_prune_check: Vec<PendingPruneCheck>,
    /// Hosts whose disks and shares `c` in Hosts asked to check; the event loop hands each its runner.
    pub pending_host_checks: Vec<String>,
    /// A check of one host is running from here.
    pub host_check_running: bool,
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
            sections: vec![Section::Dashboard, Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots, Section::Disks, Section::Shares, Section::BootEnvs, Section::Jobs, Section::Cloud, Section::Alerts, Section::Settings],
            // The section after the Dashboard (Hosts): the program opens on the Dashboard through `open_dashboard`, but
            // a fresh `App` starts on Hosts, which most behaviour (and its tests) is about.
            selected_section: 1,
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
            cloud_table: TableState::default(),
            cloud_known: HashMap::new(),
            cloud_errors: HashMap::new(),
            cloud_sizes: HashMap::new(),
            cloud_caps: HashMap::new(),
            dashboard_thresholds: crate::config::Thresholds::default(),
            dashboard_table: TableState::default(),
            pending_cloud: Vec::new(),
            cloud_busy: HashSet::new(),
            state_dir: std::path::PathBuf::from(crate::daemon::engine::DEFAULT_STATE_DIR),
            last_jobs_refresh: None,
            last_cloud_refresh: None,
            config_path: std::path::PathBuf::from(crate::config::DEFAULT_CONFIG_PATH),
            alerts_view: crate::ui::alerts::AlertsView::default(),
            settings_view: crate::ui::settings::SettingsView::default(),
            settings_table: TableState::default(),
            smart: crate::daemon::smart_check::SmartFile::default(),
            shares_check: crate::daemon::shares_check::SharesFile::default(),
            checks_marker: None,
            auto_refresh_interval: crate::config::UiConfig::default().auto_refresh_interval(),
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
            prune_check: None,
            pending_prune_check: Vec::new(),
            pending_host_checks: Vec::new(),
            host_check_running: false,
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
        // `selected_section` is always in range; Hosts is the harmless fallback.
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
        let (tx, os) = (self.tx.clone(), self.host_os(host));
        std::thread::spawn(move || {
            let result = crate::zfs::metrics::sample_target(runner.as_ref(), &target, with_contents, os);
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
            Section::Dashboard => self.refresh_dashboard(),
            Section::Cloud => self.refresh_cloud(),
            Section::Alerts => self.refresh_alerts(),
            Section::Settings => self.refresh_settings(),
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
        let known_os = state.os;
        let want_offset = state.utc_offset_secs.is_none();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let probe = crate::zfs::host_health::probe_host_guarded(runner.as_ref(), detect_kind);
            let reached = matches!(probe, crate::zfs::host_health::HostProbe::Reached { .. });
            // A probe that did not ask for the OS again (it is read once) leaves it to what the app already knows.
            let os = match &probe {
                crate::zfs::host_health::HostProbe::Reached { os, .. } => os.or(known_os),
                _ => None,
            };
            let _ = tx.send(WorkerMsg::HostHealth(name.clone(), probe));
            // The Dashboard's readings are TUI-only (the daemon's monitor probes without them) and only asked of a host that answered.
            let (resources, offset) = if reached {
                (crate::zfs::host_resources::probe_resources(runner.as_ref(), os), want_offset.then(|| crate::zfs::host_resources::utc_offset_secs(runner.as_ref())).flatten())
            } else {
                (None, None)
            };
            let _ = tx.send(WorkerMsg::HostResources(name, resources, offset));
        });
    }

    /// The newest resource reading of a host and the one before it, if it has them.
    pub fn host_resources(&self, name: &str) -> Option<(&crate::zfs::host_resources::Resources, Option<&crate::zfs::host_resources::Resources>)> {
        let all = &self.host_probes.get(name)?.resources;
        let newest = all.last()?;
        Some((newest, all.len().checked_sub(2).and_then(|i| all.get(i))))
    }

    /// The health of host `index` from its latest probe and from the Jobs data (an enabled job of this host
    /// whose latest run failed).
    fn host_probe_gave_up(&self, name: &str) -> bool {
        self.host_probes.get(name).filter(|s| s.in_flight).and_then(|s| s.started_at).is_some_and(|t| t.elapsed() > HOST_PROBE_GIVE_UP)
    }

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

    /// Makes the Dashboard the section on screen: what zfsctl shows when it starts.
    pub fn open_dashboard(&mut self) {
        if let Some(position) = self.sections.iter().position(|s| *s == Section::Dashboard) {
            self.selected_section = position;
        }
    }

    /// `c` in Hosts: queue a check of the selected host's disks and shares, unless one is already running (from here, or
    /// by the daemon or another zfsctl, which the marker file tells).
    fn request_host_check(&mut self) {
        let Some(host) = self.hosts.get(self.selected_host).cloned() else { return };
        if self.host_check_running {
            self.set_error("A check is already running".to_string());
            return;
        }
        if let Some(other) = crate::daemon::checks::fresh_marker(&self.state_dir, chrono::Utc::now()) {
            let since = other.since.with_timezone(&chrono::Local).format("%H:%M");
            self.set_error(format!("A check is already running ({}, since {since})", other.by));
            return;
        }
        self.host_check_running = true;
        self.status_error = None;
        self.status_message = Some(format!("Checking {host}..."));
        self.pending_host_checks.push(host);
    }

    /// Runs both checks on `host` in the background, with the same code as the daemon's daily pass, and merges the answers
    /// into `smart.json` and `shares.json`. A running marker tells the Dashboard (and the daemon) meanwhile; a host that
    /// does not answer within the daemon's per-host deadline is given up on, so `c` and the daemon are never held up.
    pub fn start_host_check(&mut self, host: String, runner: Arc<dyn CommandRunner>) {
        self.start_host_check_with(host, runner, crate::daemon::smart_check::HOST_DEADLINE);
    }

    pub fn start_host_check_with(&mut self, host: String, runner: Arc<dyn CommandRunner>, deadline: Duration) {
        let (state_dir, tx) = (self.state_dir.clone(), self.tx.clone());
        std::thread::spawn(move || {
            // A crash in the worker must still answer, or `c` would stay busy for the rest of the session.
            let line = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Some(_running) = crate::daemon::checks::RunGuard::start(&state_dir, "tui", Some(&host)) else {
                    return format!("{host}: another check is running; try again in a moment");
                };
                let done = crate::daemon::checks::collect(&state_dir, &[(host.clone(), runner)], chrono::Utc::now(), deadline);
                done.first().map_or_else(|| format!("{host}: nothing to check"), |(_, checks)| crate::daemon::checks::describe(&host, checks))
            }))
            .unwrap_or_else(|_| format!("{host}: the check crashed; what it had found may not be saved"));
            let _ = tx.send(WorkerMsg::HostChecked(host, line));
        });
    }

    /// Whether `section` works on the selected host's OS (see `ui::support`): a host whose OS is not known yet counts as
    /// FreeBSD, so nothing flickers while the first probe runs.
    pub fn section_supported(&self, section: Section) -> bool {
        crate::ui::support::supports(self.host_os(self.selected_host), section)
    }

    /// Whether shares can be created, edited and removed on the selected host (FreeBSD's tools only; see `ui::support`).
    pub fn shares_editable(&self) -> bool {
        crate::ui::support::shares_editable(self.host_os(self.selected_host))
    }

    /// Whether `key` of the Boot Envs section is offered for the selected host's OS (see `ui::support`).
    pub fn bootenv_key_offered(&self, key: char) -> bool {
        crate::ui::support::bootenv_key_offered(self.host_os(self.selected_host), key)
    }

    pub fn current_section_supported(&self) -> bool {
        self.section_supported(self.current_section())
    }

    /// The OS of host number `host` once a probe has read it; `None` until then (and for a host that does not exist).
    pub fn host_os(&self, host: usize) -> Option<crate::zfs::host_health::HostOs> {
        self.hosts.get(host).and_then(|name| self.host_probes.get(name)).and_then(|s| s.os)
    }

    /// Re-reads what the daily and manual checks found (`smart.json`, `shares.json`) and the running marker.
    fn reload_check_files(&mut self) {
        self.smart = crate::daemon::smart_check::SmartFile::load(&self.state_dir.join("smart.json"));
        self.shares_check = crate::daemon::shares_check::SharesFile::load(&self.state_dir.join("shares.json"));
        self.refresh_checks_marker();
    }

    /// Re-reads the marker of a running check. Cheap (one small file), so it is also done every pass of the event loop while the
    /// Dashboard is open: the red notice must appear and vanish with the check, not with the next 60-second refresh.
    pub fn refresh_checks_marker(&mut self) {
        self.checks_marker = crate::daemon::checks::fresh_marker(&self.state_dir, chrono::Utc::now());
    }

    /// Reloads everything the Dashboard shows that is not a host probe: the jobs, the alert settings, the cloud remotes and the
    /// `[dashboard]` thresholds (an unreadable `config.toml` means the defaults). Probes arrive on their own every 30 s.
    pub fn refresh_dashboard(&mut self) {
        self.dashboard_thresholds = crate::config::Config::load(&self.config_path).map(|c| c.dashboard.resolve()).unwrap_or_default();
        self.reload_check_files();
        self.refresh_alerts();
        self.refresh_cloud();
    }

    /// The Dashboard as it stands: the host probes and readings, the jobs, the cloud list and the alert settings, judged by
    /// the thresholds. Cheap (everything is in memory), so it is built on every draw and every key.
    pub fn dashboard(&self) -> crate::ui::dashboard::Dashboard {
        let hosts = self
            .hosts
            .iter()
            .enumerate()
            .map(|(i, name)| {
                // A probe that gave up waiting leaves no old reading on screen as if it were current.
                let gave_up = self.host_probe_gave_up(name);
                crate::ui::dashboard::HostInput {
                    name,
                    assessment: self.host_assessment(i),
                    probe: self.host_probes.get(name).and_then(|s| s.last.as_ref()).filter(|_| !gave_up),
                    resources: self.host_resources(name).filter(|_| !gave_up),
                    utc_offset_secs: self.host_probes.get(name).and_then(|s| s.utc_offset_secs),
                    os_name: self.host_probes.get(name).and_then(|s| s.os_name.as_deref()),
                }
            })
            .collect();
        crate::ui::dashboard::build(&crate::ui::dashboard::Inputs {
            hosts,
            jobs: &self.jobs_view,
            cloud_rows: self.cloud_rows(),
            cloud_sizes: &self.cloud_sizes,
            cloud_caps: &self.cloud_caps,
            alerts: &self.alerts_view,
            thresholds: &self.dashboard_thresholds,
            smart: &self.smart,
            shares: &self.shares_check,
            running: self.checks_marker.as_ref(),
            now: chrono::Utc::now(),
        })
    }

    /// Asks for the rclone remotes when the Cloud section is on screen and they were last asked for a minute ago or
    /// never (the first time it is opened). The remote list changes only when someone runs `rclone config`.
    pub fn refresh_cloud_if_stale(&mut self, now: Instant) {
        if matches!(self.current_section(), Section::Cloud | Section::Dashboard) && self.last_cloud_refresh.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(60)) {
            self.last_cloud_refresh = Some(now);
            if self.current_section() == Section::Dashboard {
                self.refresh_dashboard();
            } else {
                self.refresh_cloud();
            }
        }
    }

    /// Refreshes the Jobs data if it is stale: every 2 seconds while the section is on screen
    /// (so a run's result appears live), every 10 otherwise (so the failed-jobs indicator is
    /// right from any section). The first call always loads.
    pub fn refresh_jobs_if_stale(&mut self, now: Instant) {
        let every = if matches!(self.current_section(), Section::Jobs | Section::Cloud | Section::Dashboard) { Duration::from_secs(2) } else { Duration::from_secs(10) };
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

    /// Reloads the Settings rows from `config.toml`; the cap rows list the remotes the Cloud screen knows. The selection
    /// stays on its row number, or is dropped when there are no rows.
    pub fn refresh_settings(&mut self) {
        let mut remotes: Vec<String> = self.cloud_rows().into_iter().map(|r| r.name).collect();
        remotes.sort();
        remotes.dedup();
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        self.settings_view = crate::ui::settings::load(&self.config_path, &remotes, no_color);
        let rows = self.settings_view.rows.len();
        match self.settings_table.selected() {
            _ if rows == 0 => self.settings_table.select(None),
            Some(i) if i < rows => {}
            _ => self.settings_table.select(Some(0)),
        }
    }

    /// Loads the Settings rows when the section has just been opened (by any key), and forgets them once it is left,
    /// so the next visit reads the file again. Called on every pass of the event loop, before drawing.
    pub fn ensure_settings_loaded(&mut self) {
        if self.current_section() != Section::Settings {
            self.settings_view.loaded = false;
        } else if !self.settings_view.loaded {
            self.refresh_settings();
        }
    }

    /// The edit form for the selected setting, or `None` when nothing is selected.
    fn settings_form(&self) -> Option<Modal> {
        let row = self.settings_table.selected().and_then(|i| self.settings_view.rows.get(i))?;
        let pair = row.raw.len() > 1;
        let description = if pair { "Fill both numbers, or leave both empty for the defaults." } else { "Leave empty to go back to the default." };
        Some(Modal::Form {
            title: row.label.clone(),
            fields: crate::ui::settings::form_fields(row).into_iter().map(|(label, value)| FormField { label, value }).collect(),
            focused: 0,
            description: Some(description.to_string()),
            kind: FormKind::Setting { setting: row.setting.clone() },
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
        if !matches!(self.current_section(), Section::Alerts | Section::Dashboard) {
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
    pub fn acl_unsupported(&self) -> Option<String> {
        if let (false, Some(os)) = (self.section_supported(Section::DatasetAcl), self.host_os(self.selected_host)) {
            return Some(crate::ui::support::unsupported_text(os, Section::DatasetAcl));
        }
        let name = self.selected_dataset_name()?;
        if self.dataset_properties_for.as_deref() != Some(name.as_str()) {
            return None;
        }
        let acltype = self.dataset_properties.iter().find(|p| p.name == "acltype")?;
        (acltype.value == "posix").then(|| "this dataset has acltype=posix: the ACL tab edits NFSv4 ACLs only".to_string())
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
            Modal::Form { kind: FormKind::SnapshotJob { existing, .. } | FormKind::ReplicateJob { existing, .. } | FormKind::BackupJob { existing, .. } | FormKind::MirrorJob { existing, .. }, .. } => existing.as_deref(),
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

    /// Handles a key press. The match is cut into the `key_*` chunks below, tried in order: the first
    /// chunk with an arm for the key (and a passing guard) handles it, exactly as one long `match` would.
    pub fn handle_key(&mut self, key: KeyCode) {
        if self.host_filter_editing {
            self.handle_filter_key(key);
            return;
        }
        let _handled = self.key_global(key)
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
    fn key_global(&mut self, key: KeyCode) -> bool {
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
            _ => return false,
        }
        true
    }

    /// Drilling into a pool, editing a property, Tab and the pane focus, and the Hosts section.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    fn key_navigation(&mut self, key: KeyCode) -> bool {
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
            KeyCode::Char('c') if self.current_section() == Section::Hosts && self.modal.is_none() && self.picker.is_none() => self.request_host_check(),
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

    /// The Pools section: a drilled pool's vdevs, the property pane and the pool list.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    fn key_pools(&mut self, key: KeyCode) -> bool {
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
            _ => return false,
        }
        true
    }

    /// The Datasets section's right pane: the Properties, Permissions and ACL tabs.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    fn key_dataset_detail(&mut self, key: KeyCode) -> bool {
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
    fn key_dataset_list(&mut self, key: KeyCode) -> bool {
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
    fn key_lists_and_snapshots(&mut self, key: KeyCode) -> bool {
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

    /// The Jobs section (and the `J` job shortcuts of Pools and Datasets).
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    fn key_jobs(&mut self, key: KeyCode) -> bool {
        match key {
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
            KeyCode::Char('C') if matches!(self.current_section(), Section::Jobs | Section::Cloud) => {
                self.backup_preflight = None;
                self.modal = Some(self.mirror_job_form(None));
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
                        crate::jobs::KIND_MIRROR => {
                            self.backup_preflight = None;
                            self.mirror_job_form(Some(job))
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
                        return true;
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
            _ => return false,
        }
        true
    }

    /// The Alerts section, Up/Down in the Jobs list, and the Shares section.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    fn key_alerts_and_shares(&mut self, key: KeyCode) -> bool {
        match key {
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
            KeyCode::Down if self.current_section() == Section::Settings => {
                move_table_selection(&mut self.settings_table, self.settings_view.rows.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Settings => {
                move_table_selection(&mut self.settings_table, self.settings_view.rows.len(), false);
            }
            KeyCode::Enter if self.current_section() == Section::Settings && self.modal.is_none() && self.picker.is_none() => {
                if let Some(problem) = self.settings_view.problem.clone() {
                    self.set_error(format!("{problem}: fix config.toml before editing here"));
                } else if let Some(form) = self.settings_form() {
                    self.modal = Some(form);
                }
            }
            KeyCode::Down if self.current_section() == Section::Dashboard => {
                let lines = self.dashboard().attention.len();
                move_table_selection(&mut self.dashboard_table, lines, true);
            }
            KeyCode::Up if self.current_section() == Section::Dashboard => {
                let lines = self.dashboard().attention.len();
                move_table_selection(&mut self.dashboard_table, lines, false);
            }
            // Navigation only: the Dashboard shows, the section that owns the problem fixes.
            KeyCode::Enter if self.current_section() == Section::Dashboard => {
                let item = self.dashboard_table.selected().and_then(|i| self.dashboard().attention.get(i).cloned());
                if let Some(item) = item {
                    if let Some(position) = self.sections.iter().position(|s| *s == item.goto) {
                        self.selected_section = position;
                        self.pane_focus = PaneFocus::Middle;
                    }
                    match (item.goto, item.target) {
                        (Section::Hosts | Section::Pools | Section::Disks | Section::Shares, Some(name)) => {
                            if let Some(index) = self.hosts.iter().position(|h| *h == name) {
                                if index != self.selected_host {
                                    self.selected_host = index;
                                    // What the previous host's screens loaded must not look like this host's.
                                    self.clear_host_data();
                                }
                            }
                        }
                        (Section::Jobs, Some(id)) => {
                            if let Some(index) = self.jobs_view.jobs.iter().position(|j| j.id == id) {
                                self.jobs_table.select(Some(index));
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Down if self.current_section() == Section::Cloud => {
                let rows = self.cloud_rows().len();
                move_table_selection(&mut self.cloud_table, rows, true);
            }
            KeyCode::Up if self.current_section() == Section::Cloud => {
                let rows = self.cloud_rows().len();
                move_table_selection(&mut self.cloud_table, rows, false);
            }
            KeyCode::Char('s') if self.current_section() == Section::Cloud => self.request_cloud_sizes(),
            KeyCode::F(5) if self.current_section() == Section::Cloud => self.request_cloud_check(),
            KeyCode::Down if self.current_section() == Section::Jobs => {
                move_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Jobs => {
                move_table_selection(&mut self.jobs_table, self.jobs_view.jobs.len(), false);
            }
            KeyCode::Enter if self.current_section() == Section::Shares && self.shares_editable() => {
                if let Some(i) = self.shares_table.selected() {
                    if let Some(share) = self.shares.get(i) {
                        self.modal = Some(share_edit_modal(share, self.selected_host));
                    }
                }
            }
            KeyCode::Char('d') if self.current_section() == Section::Shares && self.shares_editable() => {
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
            _ => return false,
        }
        true
    }

    /// The Boot Envs section.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    fn key_bootenvs(&mut self, key: KeyCode) -> bool {
        match key {
            // A key `beadm` cannot serve (see `ui::support::bootenv_key_offered`): nothing happens, and it is not advertised.
            KeyCode::Char(c) if self.current_section() == Section::BootEnvs && !self.bootenv_key_offered(c) => {}
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
                let Some(name) = self.selected_bootenv().filter(|b| b.can_activate()).map(|b| b.name.clone()) else { return true };
                let prompt = format!("Make '{name}' the boot environment for every future boot? (y/n)");
                let call = MutationCall::ActivateBootEnv { name, temporary: false };
                self.modal = Some(Modal::Confirm { choice: None, prompt, action: PendingAction { host: self.selected_host, section: Section::BootEnvs, call } });
            }
            KeyCode::Char('t') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_activate_temporarily()).map(|b| b.name.clone()) else { return true };
                let prompt = format!("Boot '{name}' on the NEXT boot only? Later boots go back to the usual boot environment. (y/n)");
                let call = MutationCall::ActivateBootEnv { name, temporary: true };
                self.modal = Some(Modal::Confirm { choice: None, prompt, action: PendingAction { host: self.selected_host, section: Section::BootEnvs, call } });
            }
            KeyCode::Char('T') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_clear_temporary()).map(|b| b.name.clone()) else { return true };
                self.status_message = Some(format!("Clearing the next-boot activation of '{name}'…"));
                let call = MutationCall::ClearTemporaryBootEnv { name };
                self.queue_bootenv_change(call);
            }
            KeyCode::Char('R') if self.current_section() == Section::BootEnvs => {
                let Some(from) = self.selected_bootenv().map(|b| b.name.clone()) else { return true };
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
                let Some(name) = self.selected_bootenv().filter(|b| b.can_mount()).map(|b| b.name.clone()) else { return true };
                self.status_message = Some(format!("Mounting '{name}'…"));
                let call = MutationCall::MountBootEnv { name };
                self.queue_bootenv_change(call);
            }
            KeyCode::Char('u') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_unmount()).map(|b| b.name.clone()) else { return true };
                self.status_message = Some(format!("Unmounting '{name}'…"));
                let call = MutationCall::UnmountBootEnv { name };
                self.queue_bootenv_change(call);
            }
            KeyCode::Char(key @ ('d' | 'D')) if self.current_section() == Section::BootEnvs => {
                let Some(be) = self.selected_bootenv() else { return true };
                let name = be.name.clone();
                if !be.can_destroy() {
                    self.status_message = Some(format!("'{name}' cannot be destroyed: it is running, set for the next boot, or marked temporary (T clears that)"));
                    return true;
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
            _ => return false,
        }
        true
    }

    /// The Disks section.
    /// Part of `handle_key`: the arms keep their original order, so an arm in an earlier chunk still wins
    /// over a later one. Returns whether the key was handled.
    #[allow(clippy::match_same_arms)]
    fn key_disks(&mut self, key: KeyCode) -> bool {
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
            _ => return false,
        }
        true
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
            FormKind::SnapshotJob { hosts, .. } | FormKind::ReplicateJob { hosts, .. } | FormKind::BackupJob { hosts, .. } | FormKind::MirrorJob { hosts, .. } => (hosts, crate::ui::picker::LiveNames::Datasets),
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

    /// The Cloud table's rows: every rclone remote the hosts reported, plus every remote a job names.
    pub fn cloud_rows(&self) -> Vec<crate::ui::cloud::Row> {
        crate::ui::cloud::rows(&self.jobs_view.jobs, &self.cloud_known)
    }

    fn selected_cloud_row(&self) -> Option<crate::ui::cloud::Row> {
        self.cloud_table.selected().and_then(|i| self.cloud_rows().into_iter().nth(i))
    }

    /// Reloads the jobs and asks the hub, and each host that has a job writing to a remote, which remotes it has.
    /// Nothing is asked twice while an answer is awaited.
    /// Reads `[cloud.caps]` from `config.toml` again (an unreadable file means no caps).
    fn reload_cloud_caps(&mut self) {
        self.cloud_caps = crate::config::Config::load(&self.config_path).map(|c| c.cloud.caps).unwrap_or_default().into_iter().map(|(name, text)| (name, crate::config::parse_cap(&text).ok_or(text))).collect();
    }

    /// After a save in Settings: the theme, the auto-refresh interval, the Dashboard thresholds and the cloud caps
    /// follow `config.toml` without a restart. A theme name that does not resolve changes nothing and says why.
    pub fn apply_saved_settings(&mut self) {
        self.apply_saved_settings_with(&crate::ui::theme::ThemeEnv::from_process());
    }

    pub fn apply_saved_settings_with(&mut self, env: &crate::ui::theme::ThemeEnv) {
        let Ok(config) = crate::config::Config::load(&self.config_path) else { return };
        match crate::ui::theme::resolve(config.ui.theme.as_deref(), env) {
            Ok(resolved) => {
                let changed = self.theme.name != resolved.theme.name;
                self.theme = resolved.theme.bright();
                // The "Saved" note of the save that got us here stays unless the theme changed and has something to say.
                if changed && resolved.note.is_some() {
                    self.status_message = resolved.note;
                }
            }
            Err(e) => self.set_error(e.to_string()),
        }
        self.auto_refresh_interval = config.ui.auto_refresh_interval();
        self.dashboard_thresholds = config.dashboard.resolve();
        self.reload_cloud_caps();
    }

    pub fn refresh_cloud(&mut self) {
        self.refresh_jobs();
        self.reload_cloud_caps();
        let mut hosts: Vec<usize> = self.hosts.iter().position(|h| h == "local").into_iter().collect();
        for job in &self.jobs_view.jobs {
            if crate::ui::cloud::remote_name(job).is_none() {
                continue;
            }
            if let Some(i) = self.hosts.iter().position(|h| *h == job.host) {
                if !hosts.contains(&i) {
                    hosts.push(i);
                }
            }
        }
        for host in hosts {
            let host_name = self.hosts[host].clone();
            if self.cloud_busy.insert(format!("remotes:{host_name}")) {
                self.pending_cloud.push(PendingCloud { host, host_name, work: CloudWork::Remotes });
            }
        }
        let rows = self.cloud_rows().len();
        clamp_table_selection(&mut self.cloud_table, rows);
    }

    /// `s` in Cloud: measures the bucket folders of the selected remote's cloud mirror jobs, one lookup each.
    fn request_cloud_sizes(&mut self) {
        let Some(row) = self.selected_cloud_row() else { return };
        let mut asked = 0;
        for job in self.jobs_view.jobs.iter().filter(|j| row.job_ids.contains(&j.id) && j.kind == crate::jobs::KIND_MIRROR) {
            let (Some(remote), Some(host)) = (job.remote.clone(), self.hosts.iter().position(|h| *h == job.host)) else { continue };
            if self.cloud_busy.insert(format!("size:{}", job.id)) {
                self.pending_cloud.push(PendingCloud { host, host_name: job.host.clone(), work: CloudWork::Size { job_id: job.id.clone(), remote } });
            }
            asked += 1;
        }
        self.status_message = Some(if asked == 0 { "Nothing to measure: no cloud mirror job uses this remote".to_string() } else { "Measuring the bucket (it lists the files, which a cloud account may count as requests)...".to_string() });
    }

    /// F5 in Cloud: the form's remote check, for the first cloud mirror job of the selected remote.
    fn request_cloud_check(&mut self) {
        let Some(row) = self.selected_cloud_row() else { return };
        let target = self.jobs_view.jobs.iter().find(|j| row.job_ids.contains(&j.id) && j.kind == crate::jobs::KIND_MIRROR).and_then(|j| j.remote.clone().map(|r| (j.host.clone(), r)));
        match target {
            Some((host, remote)) => self.request_mirror_check(&host, &remote),
            None => self.status_message = Some("Nothing to check: F5 tries the remote of a cloud mirror job, and none uses this one".to_string()),
        }
    }

    /// Runs one queued Cloud lookup on a worker thread and reports it as `WorkerMsg::CloudRemotes` / `CloudSize`.
    pub fn run_cloud(&mut self, runner: Arc<dyn CommandRunner>, pending: PendingCloud) {
        let tx = self.tx.clone();
        std::thread::spawn(move || match pending.work {
            CloudWork::Remotes => {
                let result = crate::ui::cloud::list_remotes(runner.as_ref(), &pending.host_name);
                let _ = tx.send(WorkerMsg::CloudRemotes(pending.host_name, result));
            }
            CloudWork::Size { job_id, remote } => {
                let size = crate::ui::cloud::measure(runner.as_ref(), &pending.host_name, &remote, chrono::Utc::now());
                let _ = tx.send(WorkerMsg::CloudSize(job_id, size));
            }
        });
    }

    /// Reports a lookup that could not start (the host index no longer resolves) through the same channel.
    pub fn fail_cloud(&self, pending: PendingCloud, message: String) {
        let now = chrono::Utc::now();
        let _ = match pending.work {
            CloudWork::Remotes => self.tx.send(WorkerMsg::CloudRemotes(pending.host_name, Err(message))),
            CloudWork::Size { job_id, .. } => self.tx.send(WorkerMsg::CloudSize(job_id, crate::ui::cloud::Size { current: None, old: None, at: now, problem: Some(message) })),
        };
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
        self.backup_preflight = Some(PreflightCheck { host, repository: repository.to_string(), password_file: password_file.to_string(), result: None, file_note: None });
        self.pending_preflight.push(PendingPreflight { host, host_name: host_name.to_string(), repository: repository.to_string(), password_file: password_file.to_string() });
    }

    /// F5 in the cloud mirror form: queues one check of the remote on the form's host. It rides the backup form's
    /// check (an empty `password_file` marks it as a remote check), so a result for edited values is dropped the same way.
    pub(super) fn request_mirror_check(&mut self, host_name: &str, remote: &str) {
        let Some(host) = self.hosts.iter().position(|h| h == host_name) else {
            self.set_error(format!("Unknown host '{host_name}'. Configured hosts: {}", self.hosts.join(", ")));
            return;
        };
        if !crate::rclone::valid_remote(remote) {
            self.set_error("Check: enter the remote first, like aws:mybucket/photos (the name of an rclone remote, a colon and a bucket or folder, with no keys in it)".to_string());
            return;
        }
        let same = |c: &PreflightCheck| c.host == host && c.repository == remote && c.password_file.is_empty();
        if self.backup_preflight.as_ref().is_some_and(|c| same(c) && c.result.is_none()) {
            return;
        }
        self.backup_preflight = Some(PreflightCheck { host, repository: remote.to_string(), password_file: String::new(), result: None, file_note: None });
        self.pending_preflight.push(PendingPreflight { host, host_name: host_name.to_string(), repository: remote.to_string(), password_file: String::new() });
    }

    /// Runs one queued check on a worker thread and reports it as `WorkerMsg::Preflight`.
    pub fn run_backup_preflight(&mut self, runner: Arc<dyn CommandRunner>, pending: PendingPreflight) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let report = if pending.password_file.is_empty() {
                crate::preflight::run_remote(runner, &pending.host_name, &pending.repository, crate::preflight::DEADLINE)
            } else {
                crate::preflight::run(runner, &pending.host_name, &pending.repository, &pending.password_file, crate::preflight::DEADLINE)
            };
            let _ = tx.send(WorkerMsg::Preflight(pending.host, pending.repository, pending.password_file, report.result, report.file_note));
        });
    }

    /// Holds a replication form's save while the destination's snapshots are listed, so the confirmation can
    /// name what Keep on target would destroy. The listing runs once the event loop supplies the runner.
    pub(super) fn request_prune_check(&mut self, form: Modal, action: PendingAction) {
        let MutationCall::SaveJob { job, .. } = &action.call else { return };
        let (dest_host, dest_dataset) = (job.dest_host.clone().unwrap_or_default(), job.dest_dataset.clone().unwrap_or_default());
        let Some(host) = self.hosts.iter().position(|h| *h == dest_host) else {
            self.set_error(format!("Unknown destination host '{dest_host}'"));
            self.modal = Some(form);
            return;
        };
        self.status_message = Some(format!("Checking the snapshots on {dest_host}..."));
        self.pending_prune_check.push(PendingPruneCheck { host, host_name: dest_host.clone(), dest_dataset: dest_dataset.clone() });
        let (prefix, keep) = (crate::jobs::effective_prefix(job).to_string(), job.dest_keep.unwrap_or(1));
        self.prune_check = Some(PruneCheck { host, host_name: dest_host, dest_dataset, prefix, keep, form, action });
    }

    /// Lists the destination on a worker thread and reports it as `WorkerMsg::PrunePreview`.
    pub fn run_prune_check(&mut self, runner: Arc<dyn CommandRunner>, pending: PendingPruneCheck) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::replication::list_snapshot_infos_or_empty(&*runner, &pending.dest_dataset);
            let _ = tx.send(WorkerMsg::PrunePreview(pending.host, pending.dest_dataset, result));
        });
    }

    /// Reports a listing that could not start (the host index no longer resolves) through the same channel.
    pub fn fail_prune_check(&self, pending: PendingPruneCheck, message: String) {
        let _ = self.tx.send(WorkerMsg::PrunePreview(pending.host, pending.dest_dataset, Err(crate::error::AppError::Parse(message))));
    }

    /// Reports a check that could not start (the host index no longer resolves) through the same channel.
    pub fn fail_backup_preflight(&self, pending: PendingPreflight, message: String) {
        let _ = self.tx.send(WorkerMsg::Preflight(pending.host, pending.repository, pending.password_file, crate::restic::Preflight::Problem(message), None));
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
        let (tx, os) = (self.tx.clone(), self.host_os(host));
        std::thread::spawn(move || {
            let result = call.run_on(runner.as_ref(), os);
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
        let (tx, os) = (self.tx.clone(), self.host_os(host));
        std::thread::spawn(move || {
            let result = crate::zfs::disk::list_disks_with_membership(runner.as_ref(), os);
            let _ = tx.send(WorkerMsg::Disks(host, result));
        });
    }

    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight.
    pub fn refresh_shares(&mut self, runner: Arc<dyn CommandRunner>) {
        if !self.section_supported(Section::Shares) {
            self.shares.clear();
            (self.nfs_available, self.samba_available) = (None, None);
            self.shares_loaded = false;
            return;
        }
        let host = self.selected_host;
        if self.is_refreshing(Section::Shares, host) {
            self.pending_refresh.insert((Section::Shares, host));
            return;
        }
        self.begin_refresh(Section::Shares, host);
        let (tx, os) = (self.tx.clone(), self.host_os(host));
        std::thread::spawn(move || {
            let result = crate::zfs::share::list_shares_for(runner.as_ref(), os);
            let _ = tx.send(WorkerMsg::Shares(host, result));
        });
    }

    /// See `refresh_pools` for why this queues rather than drops a request while in flight.
    pub fn refresh_bootenvs(&mut self, runner: Arc<dyn CommandRunner>) {
        if !self.section_supported(Section::BootEnvs) {
            self.bootenvs.clear();
            self.bootenvs_unavailable = None;
            return;
        }
        let host = self.selected_host;
        if self.is_refreshing(Section::BootEnvs, host) {
            self.pending_refresh.insert((Section::BootEnvs, host));
            return;
        }
        self.begin_refresh(Section::BootEnvs, host);
        let (tx, os) = (self.tx.clone(), self.host_os(host));
        std::thread::spawn(move || {
            let result = crate::zfs::bootenv::list_for(runner.as_ref(), os);
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
        if !self.section_supported(Section::DatasetAcl) {
            self.acl_entries.clear();
            return;
        }
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
  F5               in the backup job form: check the repository and password file on the job's host; in the cloud mirror form: check the remote (both read-only; never block saving)
  Esc              dismiss a status message, back out of a focused pane, or cancel a modal (in a y/n confirmation Esc selects No, then Enter)
  y / n, Enter     a y/n confirmation takes two keys: y (yes) or n / Esc (no) selects, then Enter applies it; Enter alone does nothing. Forms that cannot be undone (create pool, add vdev, replace disk, revoke, shrinking a volume, pool features) end in one, showing the exact command
  ?                this Help screen
  !                open a shell on the selected host (remote hosts: your unprivileged ssh login); exit returns to zfsctl
  A                About
  q                quit

Hosts
  Up / Down        select the active host
  /                filter the host list by name, address, tag or type (Enter keeps it, Esc clears it)
  c                check the selected host's disks (smartctl) and shares (NFS, Samba) now; the result joins the Dashboard (read-only)
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
  C                new cloud mirror job (rclone: copy a snapshot clone to a cloud bucket as plain files)
  c                cancel the selected replication or backup while it is running
  Space            enable / disable the job
  d                delete the job (y/n; its past runs stay in the history)

Dashboard
  Up / Down        select a line of Needs attention
  Enter            go to the section that owns that line (the Dashboard itself changes nothing)
  r                reload the jobs, alerts, cloud list and [dashboard] settings

Cloud
  Up / Down        select an rclone remote (the hub's, and those of hosts that have a cloud job)
  s                measure the bucket: the size of current/ and old/ of the remote's cloud mirror jobs (reads the file list, which a cloud account may count as requests)
  F5               check the remote of its cloud mirror job (read-only)
  C                new cloud mirror job
  r                refresh the list of remotes and jobs

Alerts
  e                edit the alert settings (saved into the [notify] section of config.toml only; y/n to confirm)
  t                send a test message through the saved settings (a real message; asks first)
  p                preview the daily report as it would be sent now (sends nothing)
  Up / Down        scroll the recent events

Settings
  Up / Down        select a setting (theme, auto-refresh, Dashboard thresholds, cloud caps)
  Enter            edit the setting in a form (blank = default; saved into config.toml after a y/n confirmation)
  r                reload the settings from config.toml

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
