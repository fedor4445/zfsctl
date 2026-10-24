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

/// Sends the `SnapshotTags` answer of a tag fetch exactly once: the real one, or an empty one when dropped
/// before that (a panic), because the answer is what clears the host's "fetch running" mark.
struct TagsReply {
    tx: Sender<WorkerMsg>,
    host: usize,
    generation: u64,
    sent: bool,
}

impl TagsReply {
    fn send(&mut self, tags: HashMap<String, Vec<String>>) {
        self.sent = true;
        let _ = self.tx.send(WorkerMsg::SnapshotTags(self.host, self.generation, tags));
    }
}

impl Drop for TagsReply {
    fn drop(&mut self) {
        if !self.sent {
            let _ = self.tx.send(WorkerMsg::SnapshotTags(self.host, self.generation, HashMap::new()));
        }
    }
}

/// How long an error status is kept against quiet (auto) refreshes.
const ERROR_STATUS_MIN_AGE: Duration = Duration::from_secs(8);

mod forms;
mod cloud;
mod hosts;
mod keys;
mod keys_other;
mod keys_zfs;
mod pickers;
mod refresh;
mod screens;
mod snapshot_views;
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
/// What a job form's live-fetched picker is for, beyond the pools or datasets a host field names.
pub(crate) enum Wanted {
    /// The pool or dataset names of the host.
    Names,
    /// `new`, the prefixes and the snapshots of this dataset.
    Snapshots(String),
    /// The host's rclone remotes, each written with this prefix (`rclone:` for a restic repository, else none).
    Remotes(&'static str),
}

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

/// A vdev add waiting for its dry run: what is being added to which pool on which host, and the form it came from (to give
/// back when the host refuses it).
#[derive(Debug, Clone, PartialEq)]
pub struct AddCheck {
    /// Tells two asks with the same host, pool and spec apart (decline, change nothing, submit again).
    pub id: u64,
    pub host: usize,
    pub pool: String,
    pub spec: Vec<String>,
    pub role: crate::zfs::vdev_role::Role,
    pub form: Modal,
}

/// How long a confirm waits for the host's `zpool add -n` layout.
const ADD_CHECK_DEADLINE: Duration = Duration::from_secs(30);

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
    /// A changes read finished: host, the request number it was started for, and what it found.
    Changes(usize, u64, Result<crate::ui::changes::ChangesOutcome>),
    /// A snapshot folder was listed: host, the request number it was started for, and what it found.
    Browse(usize, u64, Result<crate::ui::browse::BrowseOutcome>),
    /// The hold tags of the held snapshots of one host's list (key `dataset@name`), sent after `Snapshots`; the
    /// number says which list refresh they were fetched for.
    SnapshotTags(usize, u64, std::collections::HashMap<String, Vec<String>>),
    /// The dry run (`zpool add -n`) of a vdev add finished: the layout it would give, or the host's own refusal.
    AddChecked(AddCheck, std::result::Result<String, String>),
    /// The report preview the Alerts screen asked for (`p`), as `Subject: ...` then the body.
    AlertsPreview(String),
    /// A check of one host from the Hosts section finished: the line to show.
    HostChecked(String),
    Pools(usize, Result<Vec<Pool>>),
    Datasets(usize, Result<Vec<Dataset>>),
    /// The host's encrypted datasets; sent just before `Datasets` so the list never shows without them.
    Encryption(usize, std::collections::BTreeMap<String, crate::zfs::encryption::Enc>),
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
    /// A finished restore: what it verified (the status line) or the engine's refusal sentence, together with the plain result.
    /// One message, so a note can never be read for another call's `MutationDone`.
    RestoreDone(usize, std::result::Result<String, String>, Result<()>),
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
    /// The selected host's encrypted datasets (the Lock column, the save-time job guard); empty when none or not read.
    pub encryption: std::collections::BTreeMap<String, crate::zfs::encryption::Enc>,
    /// The host index `encryption` was read for (a job form may be for another host).
    pub encryption_for: usize,
    pub snapshots: Vec<Snapshot>,
    /// The hold tags of the held snapshots of the selected host, keyed `dataset@name`. A held snapshot without an
    /// entry has unknown tags.
    pub snapshot_tags: std::collections::HashMap<String, Vec<String>>,
    /// Per host, how many snapshot list refreshes have started: tags are applied only when they were fetched for the latest one.
    snapshot_gen: HashMap<usize, u64>,
    /// Hosts with a tag fetch running: a refresh that finds its host here skips its own fetch (tags stay unknown until the next refresh).
    tags_running: Arc<std::sync::Mutex<HashSet<usize>>>,
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
    /// The saved measurements were read (once, at the first jobs refresh).
    cloud_sizes_loaded: bool,
    /// `[cloud.caps]` of `config.toml`, read each time the Cloud list is asked for.
    pub cloud_caps: crate::ui::cloud::Caps,
    /// `[dashboard]` of `config.toml`, read each time the Dashboard refreshes.
    pub dashboard_thresholds: crate::config::Thresholds,
    /// `[protection]` (an unreadable or invalid one means the defaults; the daemon's check reports the problem).
    pub dashboard_protection: crate::config::ProtectionSettings,
    /// Why `[protection]` is wrong, when it is (the Dashboard then judges at Basic and says so).
    pub dashboard_protection_problem: Option<String>,
    /// The selected line of the Dashboard's Needs attention list.
    pub dashboard_table: TableState,
    pub pending_cloud: Vec<PendingCloud>,
    /// The changes view of the Snapshots section (`c` / `C`), its latest request number, and the reads waiting for a runner.
    pub changes_view: Option<crate::ui::changes::ChangesView>,
    pub changes_request: u64,
    pub pending_changes: Vec<crate::ui::changes::PendingChanges>,
    /// The browse view of the Snapshots section (`f`), its latest request number, and the reads waiting for a runner.
    pub browse_view: Option<crate::ui::browse::BrowseView>,
    pub browse_request: u64,
    pub pending_browse: Vec<crate::ui::browse::PendingBrowse>,
    /// Lookups that were started and have not answered: `remotes:<host>` or `size:<job id>`.
    cloud_busy: HashSet<String>,
    /// Where `jobs.toml`, `history.jsonl` and `daemon.json` live: the daemon's state directory.
    pub state_dir: std::path::PathBuf,
    last_jobs_refresh: Option<Instant>,
    last_cloud_refresh: Option<Instant>,
    /// `config.toml`, read and (for the alert settings) written by the Alerts screen.
    pub config_path: std::path::PathBuf,
    pub alerts_view: crate::ui::alerts::AlertsView,
    /// First visible line of an open Info pop-up (Help, About, report preview); Up/Down/PageUp/PageDown/Home/End move it.
    pub info_scroll: u16,
    /// The Settings section: the rest of `config.toml` that can be edited here.
    pub settings_view: crate::ui::settings::SettingsView,
    pub settings_table: TableState,
    /// What the daemon's daily disk check found (`smart.json`), reloaded with the Dashboard.
    pub smart: crate::daemon::smart_check::SmartFile,
    pub shares_check: crate::daemon::shares_check::SharesFile,
    pub storage_check: crate::daemon::storage_check::StorageFile,
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
    /// When `status_error` was set: a quiet (auto) refresh leaves an error younger than `ERROR_STATUS_MIN_AGE` alone.
    status_error_at: Option<Instant>,
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
    /// Vdev adds whose dry run the event loop hands a runner (see `start_add_check`).
    pub pending_add_checks: Vec<AddCheck>,
    pub add_check_ids: u64,
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
            encryption: std::collections::BTreeMap::new(),
            encryption_for: 0,
            snapshots: Vec::new(),
            snapshot_tags: std::collections::HashMap::new(),
            snapshot_gen: HashMap::new(),
            tags_running: Arc::new(std::sync::Mutex::new(HashSet::new())),
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
            cloud_sizes_loaded: false,
            cloud_caps: HashMap::new(),
            dashboard_thresholds: crate::config::Thresholds::default(),
            dashboard_protection: crate::config::ProtectionSettings::default(),
            dashboard_protection_problem: None,
            dashboard_table: TableState::default(),
            pending_cloud: Vec::new(),
            changes_view: None,
            changes_request: 0,
            pending_changes: Vec::new(),
            browse_view: None,
            browse_request: 0,
            pending_browse: Vec::new(),
            cloud_busy: HashSet::new(),
            state_dir: std::path::PathBuf::from(crate::daemon::engine::DEFAULT_STATE_DIR),
            last_jobs_refresh: None,
            last_cloud_refresh: None,
            config_path: std::path::PathBuf::from(crate::config::DEFAULT_CONFIG_PATH),
            alerts_view: crate::ui::alerts::AlertsView::default(),
            info_scroll: 0,
            settings_view: crate::ui::settings::SettingsView::default(),
            settings_table: TableState::default(),
            smart: crate::daemon::smart_check::SmartFile::default(),
            shares_check: crate::daemon::shares_check::SharesFile::default(),
            storage_check: crate::daemon::storage_check::StorageFile::default(),
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
            status_error_at: None,
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
            pending_add_checks: Vec::new(),
            add_check_ids: 0,
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

    /// Whether `section` works on the selected host's OS (see `ui::support`): a host whose OS is not known yet counts as
    /// FreeBSD, so nothing flickers while the first probe runs.
    pub fn section_supported(&self, section: Section) -> bool {
        crate::ui::support::supports(self.host_os(self.selected_host), section)
    }

    /// Test helper: the OS the first probe would have read for a host.
    #[cfg(test)]
    pub fn set_os_for_test(&mut self, host: &str, os: crate::zfs::host_health::HostOs) {
        self.host_probes.entry(host.to_string()).or_default().os = Some(os);
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

    /// Sets a failure message (rendered with the theme's `err` style).
    pub fn set_error(&mut self, msg: String) {
        self.status_error = Some(msg.clone());
        self.status_error_at = Some(Instant::now());
        self.status_message = Some(msg);
    }

    /// The Hold column: `H` when zfsctl's own tag is on the snapshot, `h` when it is held but not (known to be) by
    /// zfsctl, blank when unheld.
    pub fn hold_marker(&self, s: &Snapshot) -> &'static str {
        if s.holds == 0 {
            return "";
        }
        let ours = self.snapshot_tags.get(&format!("{}@{}", s.dataset, s.name)).is_some_and(|t| t.iter().any(|t| t == crate::zfs::snapshot::HOLD_TAG));
        if ours {
            "H"
        } else {
            "h"
        }
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
        // A changes view belongs to the host it was opened on: leaving it cancels its diff and drops it, so it can
        // never be left saying "reading" for an answer that was thrown away.
        if let Some(view) = self.changes_view.take() {
            view.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if let Some(view) = self.browse_view.take() {
            view.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        self.metrics = crate::ui::metrics::MetricsView::default();
        self.pools.clear();
        self.datasets.clear();
        self.encryption.clear();
        self.snapshots.clear();
        self.snapshot_tags.clear();
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

    fn focused_form_value(&self) -> String {
        match &self.modal {
            Some(Modal::Form { fields, focused, .. }) => fields.get(*focused).map(|f| f.value.clone()).unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Runs `action.call` on a worker thread against `runner` and
    /// reports the outcome as `WorkerMsg::MutationDone`. The modal is
    /// already closed by the time this is called (`handle_modal_key`
    /// clears it on submit) — this only executes the mutation itself.
    pub fn dispatch_mutation(&mut self, action: PendingAction, runner: Arc<dyn CommandRunner>) {
        let PendingAction { host, section, call } = action;
        // The keys already hold back what a host's OS cannot do; this is the last gate before a command is sent, so a path
        // that forgot one (or a host whose OS has not been read yet) still sends nothing FreeBSD-specific.
        let os = self.host_os(host);
        let refused = match &call {
            MutationCall::SetSmbShare { .. } | MutationCall::RemoveSmbShare { .. } => !crate::ui::support::shares_editable(os),
            _ => section == Section::BootEnvs && !crate::ui::support::supports(os, Section::BootEnvs),
        };
        if refused {
            self.status_message = Some(format!("Not sent: this is not supported on {} hosts{}.", os.map_or("unread", crate::zfs::host_health::HostOs::word), if os.is_none() { " (the host's OS has not been read yet)" } else { "" }));
            return;
        }
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
            // A restore's success text carries what the engine verified, so it travels with the plain result.
            let message = match call.restore_outcome(runner.as_ref()) {
                Some(Ok(done)) => WorkerMsg::RestoreDone(host, Ok(crate::ui::modal::restored_note(&done)), Ok(())),
                Some(Err(e)) => {
                    let sentence = match &e {
                        crate::error::AppError::Config(m) => m.clone(),
                        other => other.to_string(),
                    };
                    WorkerMsg::RestoreDone(host, Err(sentence), Err(e))
                }
                None => WorkerMsg::MutationDone(host, section, call.run_on(runner.as_ref(), os)),
            };
            let _ = tx.send(message);
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
        "{}\n\nzfsctl v{}\n\nA terminal UI for managing ZFS storage: a FreeBSD hub that manages FreeBSD, Linux and illumos hosts over ssh.\n\nBuilt in collaboration with Claude (Anthropic).\n\nPress any key to close.",
        crate::ui::logo::wordmark().join("\n"),
        env!("CARGO_PKG_VERSION")
    )
}

/// The question the hold confirm asks, from the `H` key in the list and from the changes view.
/// The engine's refusal as the plain sentence it is (without the error type's prefix).
pub(super) fn refusal_text(e: crate::error::AppError) -> String {
    match e {
        crate::error::AppError::Config(sentence) => sentence,
        other => other.to_string(),
    }
}

/// The question the restore confirm asks: the source, the destination and the exact command that will run as root. Every
/// piece of host text goes through `sanitise_for_display`.
pub(super) fn restore_prompt(plan: &crate::zfs::snapshot_restore::RestorePlan) -> String {
    // Cleaned first, then quoted the way the shell would need it: a space in a name (leading, trailing or in the middle)
    // is then visible and cannot read as the start of another operand.
    let shown = |text: &str| crate::exec::ssh::shell_quote(&visible(text));
    let command = plan.command.iter().map(|a| shown(a.as_str())).collect::<Vec<_>>().join(" ");
    let mut prompt = format!(
        "Restore as a copy\nFrom:  {}\nTo:    {}\nCommand: cp {command}\nSize: {}   Mode: {}\nThis creates one new file next to the original and never replaces anything.\nExisting files are never overwritten; if the name is taken nothing is copied.",
        shown(&plan.source),
        shown(&plan.destination),
        plan.size,
        crate::display_text::sanitise_for_display(&plan.mode),
    );
    // The names the prompt shows with direction marks blanked out come from the host as well: the source's folders and name count too.
    if looks_deceptive(&plan.dest_name) || looks_deceptive(&plan.source) || looks_deceptive(&plan.destination) {
        prompt.push_str("\nNote: this name contains unusual characters; check it letter by letter.");
    }
    prompt.push_str(" (y/n)");
    prompt
}

/// `sanitise_for_display`'s replacement of control and direction characters, but without its collapsing and trimming of
/// spaces: in a file name every space counts, and the quoted display exists to show them.
fn visible(text: &str) -> String {
    text.chars().map(|c| if c.is_control() || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}') { ' ' } else { c }).collect()
}

/// A non-ASCII character that can pass for a slash, a space or nothing at all in a name.
fn looks_deceptive(name: &str) -> bool {
    name.chars().any(|c| {
        !c.is_ascii()
            && (matches!(c, '\u{2044}' | '\u{2215}' | '\u{29F8}' | '\u{FF0F}' | '\u{2571}' | '\u{00AD}' | '\u{061C}' | '\u{FEFF}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}')
                || c.is_whitespace()
                || c.is_control())
    })
}

fn hold_prompt(target: &str) -> String {
    format!("Protect {target} with a hold?\nIt cannot be destroyed, and retention, backups and replication will not prune it, until the hold is released. (y/n)")
}

/// The Help text, for the book's drift test.
#[cfg(test)]
pub(crate) fn help_text_for_book() -> String {
    help_text()
}

/// The Help text: one blank line between key lines, so a long description that wraps cannot run into the next key.
fn help_text() -> String {
    let mut out = String::new();
    let mut previous_was_key = false;
    for line in HELP_SOURCE.lines() {
        let is_key = line.starts_with("  ");
        if is_key && previous_was_key {
            out.push('\n');
        }
        out.push_str(line);
        out.push('\n');
        previous_was_key = is_key;
    }
    out.pop();
    out
}

const HELP_SOURCE: &str = "\
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
  a                add a vdev/mirror/raidz group, or a log, cache, special, dedup or spare device
  X                remove the selected log, cache or spare device

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
  H                hold the selected snapshot, or release zfsctl's hold on it (a hold protects it from destroy and pruning)
  c                show what changed since the previous snapshot, by folder (read-only; Enter opens a folder, / filters its entries by name as you type (a|b for either, * ? wildcards), [ ] sets the depth, H holds the older snapshot, Esc closes)
  C                show what has changed since this snapshot, compared with the live dataset
  f                look inside the selected snapshot: its folders with type, size, dates, mode and owner (read-only, metadata only; needs the `browse` sudoers feature on other hosts). Enter opens a folder, ← goes up, / filters by name as you type (a|b for either, * ? wildcards), m s t by modified date, size and type, x clears, Esc closes
  R                in the snapshot browser, on a regular file: copy it back next to the original under a new name (asks for the name, then shows the exact command; y then Enter runs it; never replaces a file; needs the `restore` sudoers feature on other hosts)

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
  Enter            go to the section that owns that line; on an unprotected-data line, open the form that makes the missing job
                   (the Dashboard itself changes nothing)
  r                reload the jobs, alerts, cloud list and [dashboard] settings

Cloud
  Up / Down        select an rclone remote (the hub's, and those of hosts that have a cloud job)
  s                measure the bucket: the size of current/ and old/ of the remote's cloud mirror jobs (reads the file list, which a cloud account may count as requests)
  F5               check the selected remote (read-only): the remote of its cloud mirror job, or, when no job uses it yet, its top level
  n                set up a provider: a list (Backblaze B2, Amazon S3, Google Drive, OneDrive, Dropbox, SFTP...) and the steps for the one you pick; nothing is created
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

Up / Down / PageUp / PageDown scroll; any other key closes.";

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
