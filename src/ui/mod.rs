pub mod alerts;
pub mod browse;
pub mod changes;
pub mod cloud;
pub mod dashboard;
pub mod dashboard_view;
pub mod hints;
pub mod jobs;
pub mod layout;
pub mod metrics;
pub mod modal;
pub mod picker;
pub mod settings;
pub mod shell;
pub mod support;
pub mod state;
pub mod theme;

pub use state::App;

use crate::error::{AppError, Result};
use crate::hosts::HostInventory;
use crate::ui::state::DatasetDetailTab;
use crate::ui::state::PaneFocus;
use crate::ui::state::Section;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::time::{Duration, Instant};

/// How often to auto-refresh a drilled-into pool's vdevs, so the
/// status bar's resilver/scrub gauge actually advances on its own
/// instead of only updating on a manual `r` press or after a mutation.
/// The event loop already wakes roughly every `event::poll` timeout
/// even with no key pressed, but refreshing that often would hammer
/// the connection (especially over SSH) for no visible benefit — this
/// throttles actual refreshes to a human-visible cadence.
///
/// Deliberately NOT gated on `pool_scan_status` already being
/// `InProgress`: the refresh triggered right after an online/replace
/// mutation succeeds can race ZFS actually starting the resilver and
/// updating `zpool status`, catching the *previous* (already
/// completed) scan first. Gating on "already known in progress" would
/// mean that race permanently prevents ever discovering the new
/// resilver at all — nothing would ever re-check. Polling
/// unconditionally while drilled in (not just while a scan is already
/// known to be running) is what actually catches that transition.
const VDEVS_AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// Runs the TUI. Terminal teardown (`disable_raw_mode`/`LeaveAlternateScreen`)
/// is always attempted before returning, regardless of which setup step
/// failed — otherwise a failure between `enable_raw_mode()` and a fully
/// constructed `Terminal` would leave the user's shell in raw mode with
/// no way to recover except closing the terminal window.
pub fn run_app(mut inventory: HostInventory, config_path: &std::path::Path, resolved: theme::Resolved, auto_refresh: Option<Duration>) -> Result<()> {
    enable_raw_mode().map_err(|e| AppError::Config(format!("enabling raw mode: {e}")))?;

    // Ensure a panic anywhere in the event loop still restores the
    // terminal — otherwise the operator's shell is left in raw mode
    // with the alternate screen active and the panic message lost.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        previous_hook(panic_info);
    }));

    let result = execute!(std::io::stdout(), EnterAlternateScreen)
        .map_err(|e| AppError::Config(format!("entering alternate screen: {e}")))
        .and_then(|()| {
            let backend = CrosstermBackend::new(std::io::stdout());
            let mut terminal = Terminal::new(backend)
                .map_err(|e| AppError::Config(format!("creating terminal: {e}")))?;

            let host_names: Vec<String> = inventory.hosts.iter().map(|h| h.name.clone()).collect();
            let mut app = App::new(host_names);
            app.theme = resolved.theme.bright();
            app.auto_refresh_interval = auto_refresh;
            app.config_path = config_path.to_path_buf();
            app.status_message = resolved.note;
            app.set_host_addresses(inventory.hosts.iter().map(|h| state::address_of(h.ssh_target.as_deref())).collect());
            app.set_host_tags(inventory.hosts.iter().map(|h| h.tag.clone().unwrap_or_default()).collect());
            app.open_dashboard();
            if let Some(first_host) = inventory.hosts.first() {
                app.refresh_pools(first_host.runner.clone());
            }

            event_loop(&mut terminal, &mut app, &mut inventory, config_path)
        });

    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen);

    result
}

/// Drains every queued new host (`app.pending_new_hosts`) and applies
/// each one: pushes the name onto `app.hosts` and pushes the host
/// itself onto `inventory.hosts` in the same step, so the two
/// index-aligned lists can never drift apart. Extracted out of
/// `event_loop` (which has no test harness at all) specifically so
/// this part does: a final review found that an earlier, single-
/// `Option` version of the signal this reads lost a second host whose
/// probe resolved in the same tick as the first's -- draining a `Vec`
/// instead means every queued success is applied, never just the
/// last one.
fn apply_pending_new_hosts(app: &mut App, inventory: &mut HostInventory, config_path: &std::path::Path) {
    for (name, target) in std::mem::take(&mut app.pending_new_hosts) {
        app.hosts.push(name.clone());
        app.host_addresses.push(state::address_of(Some(&target)));
        let tag = app.new_host_tags.remove(&name.to_lowercase());
        app.host_tags.push(tag.clone().unwrap_or_default());
        if let Err(e) = inventory.apply_new_host(config_path, name, target, tag) {
            app.set_error(format!("Host added, but could not save it to config.toml (it will not survive a restart): {e}"));
        }
    }
}

/// A held Enter repeats about every 30 ms once the keyboard's initial repeat delay has passed; a deliberate second
/// press is slower. Enters inside this window of the previous one are dropped, which stops the repeats of a held key
/// (the first repeat after a long initial delay still arrives). The real protection against a held key is the
/// two-step confirmation: an unanswered confirmation ignores Enter.
const ENTER_REPEAT_WINDOW: Duration = Duration::from_millis(250);

#[derive(Default)]
struct EnterDebounce {
    last_enter: Option<Instant>,
}

impl EnterDebounce {
    /// Whether this key should be handled. Every Enter, dropped or not, restarts the window.
    fn accept(&mut self, code: KeyCode, now: Instant) -> bool {
        if code != KeyCode::Enter {
            return true;
        }
        let repeat = self.last_enter.is_some_and(|t| now.duration_since(t) < ENTER_REPEAT_WINDOW);
        self.last_enter = Some(now);
        !repeat
    }
}

/// Drains `app.pending_account_fetch` and starts one worker per host.
/// `App` cannot resolve a host index to a runner, so the event loop does
/// it (same split as `apply_pending_new_hosts`). An index with no host
/// (e.g. the inventory changed) is reported as a failed fetch so the picker
/// shows Failed and the in-flight marker clears.
fn apply_pending_account_fetches(app: &mut App, inventory: &HostInventory) {
    for host in std::mem::take(&mut app.pending_account_fetch) {
        if let Some(h) = inventory.hosts.get(host) {
            app.fetch_accounts(h.runner.clone(), host);
        } else {
            app.fail_account_fetch(host, AppError::Parse(format!("unknown host index {host}")));
        }
    }
}

/// Starts the host health probes that are due (one worker per host, never two at once for a host).
fn poll_host_probes(app: &mut App, inventory: &HostInventory) {
    for index in app.due_host_probes(Instant::now()) {
        if let Some(host) = inventory.hosts.get(index) {
            app.start_host_probe(index, host.runner.clone());
        }
    }
}

/// Hands each queued backup-form check its host's runner (same split as the pickers' fetches).
fn apply_pending_preflights(app: &mut App, inventory: &HostInventory) {
    for pending in std::mem::take(&mut app.pending_preflight) {
        if let Some(h) = inventory.hosts.get(pending.host) {
            app.run_backup_preflight(h.runner.clone(), pending);
        } else {
            let message = format!("unknown host '{}'", pending.host_name);
            app.fail_backup_preflight(pending, message);
        }
    }
}

/// Hands each queued check of one host (`c` in Hosts) its host's runner.
fn apply_pending_host_checks(app: &mut App, inventory: &HostInventory) {
    for name in std::mem::take(&mut app.pending_host_checks) {
        match inventory.hosts.iter().find(|h| h.name == name) {
            Some(h) => app.start_host_check(name, h.runner.clone()),
            None => {
                app.host_check_running = false;
                app.set_error(format!("unknown host '{name}'"));
            }
        }
    }
}

/// Hands each queued vdev add its host's runner for the `zpool add -n` dry run.
fn apply_pending_add_checks(app: &mut App, inventory: &HostInventory) {
    for check in std::mem::take(&mut app.pending_add_checks) {
        match inventory.hosts.get(check.host) {
            Some(h) => app.start_add_check(check, h.runner.clone()),
            None => {
                let why = format!("unknown host #{}", check.host);
                app.on_add_checked(check, Err(why));
            }
        }
    }
}

/// Hands each queued Cloud lookup (remote list, bucket size) its host's runner.
fn apply_pending_cloud(app: &mut App, inventory: &HostInventory) {
    for pending in std::mem::take(&mut app.pending_cloud) {
        if let Some(h) = inventory.hosts.get(pending.host) {
            app.run_cloud(h.runner.clone(), pending);
        } else {
            let message = format!("unknown host '{}'", pending.host_name);
            app.fail_cloud(pending, message);
        }
    }
}

/// Hands each queued snapshot folder read its host's runner.
fn apply_pending_browse(app: &mut App, inventory: &HostInventory) {
    for pending in std::mem::take(&mut app.pending_browse) {
        if let Some(h) = inventory.hosts.get(pending.host) {
            app.start_browse(h.runner.clone(), pending);
        } else {
            let why = crate::error::AppError::Config(format!("unknown host #{}", pending.host));
            app.fail_browse(&pending, why);
        }
    }
}

/// Hands each queued changes read its host's runner.
fn apply_pending_changes(app: &mut App, inventory: &HostInventory) {
    for pending in std::mem::take(&mut app.pending_changes) {
        if let Some(h) = inventory.hosts.get(pending.host) {
            app.start_changes(h.runner.clone(), pending);
        } else {
            let why = crate::error::AppError::Config(format!("unknown host #{}", pending.host));
            app.fail_changes(&pending, why);
        }
    }
}

/// Hands each queued destination listing (a replication form's Keep on target check) its host's runner.
fn apply_pending_prune_checks(app: &mut App, inventory: &HostInventory) {
    for pending in std::mem::take(&mut app.pending_prune_check) {
        if let Some(h) = inventory.hosts.get(pending.host) {
            app.run_prune_check(h.runner.clone(), pending);
        } else {
            let message = format!("unknown host '{}'", pending.host_name);
            app.fail_prune_check(pending, message);
        }
    }
}

/// Same split as `apply_pending_account_fetches`, for the job forms' Pool and Dataset pickers.
fn apply_pending_pool_fetches(app: &mut App, inventory: &HostInventory) {
    for (host, what) in std::mem::take(&mut app.pending_pool_fetch) {
        if let Some(h) = inventory.hosts.get(host) {
            app.fetch_pool_names(h.runner.clone(), host, what);
        } else {
            app.fail_pool_fetch(host, AppError::Parse(format!("unknown host index {host}")));
        }
    }
}

/// Leaves the TUI, runs a shell on the host, and puts the TUI back. The
/// terminal is restored even when the shell could not be started, so a
/// failure here can never strand the operator in a half-set-up terminal.
fn run_shell_session(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    host_name: &str,
    ssh_target: Option<&str>,
    signals: &shell::ShellSignals,
) -> Result<shell::ShellOutcome> {
    // ratatui hides the cursor on every frame that sets none, and leaving the
    // alternate screen does not bring it back; the shell would run with an invisible cursor.
    let _ = terminal.show_cursor();
    let suspended = disable_raw_mode().and_then(|()| execute!(std::io::stdout(), LeaveAlternateScreen));
    let outcome = match suspended {
        Ok(()) => shell::run_shell(host_name, ssh_target, signals),
        Err(e) => shell::ShellOutcome::SpawnFailed(format!("suspending the terminal: {e}")),
    };
    enable_raw_mode().map_err(|e| AppError::Config(format!("re-enabling raw mode: {e}")))?;
    execute!(std::io::stdout(), EnterAlternateScreen).map_err(|e| AppError::Config(format!("re-entering the alternate screen: {e}")))?;
    terminal.clear().map_err(|e| AppError::Config(format!("clearing the screen: {e}")))?;
    Ok(outcome)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    inventory: &mut HostInventory,
    config_path: &std::path::Path,
) -> Result<()> {
    // `None` = never refreshed yet, so the first poll happens immediately.
    let mut last_vdevs_auto_refresh: Option<Instant> = None;
    let mut enter_gate = EnterDebounce::default();
    let signals = shell::ShellSignals::install().map_err(|e| e.to_string());
    loop {
        let refreshes = app.drain_worker_messages();
        app.refresh_jobs_if_stale(Instant::now());
        app.refresh_cloud_if_stale(Instant::now());
        app.refresh_alerts_if_stale(Instant::now());
        app.ensure_settings_loaded();
        if app.current_section() == Section::Dashboard {
            app.refresh_checks_marker();
        }
        apply_pending_new_hosts(app, inventory, config_path);
        apply_pending_account_fetches(app, inventory);
        apply_pending_pool_fetches(app, inventory);
        apply_pending_preflights(app, inventory);
        apply_pending_host_checks(app, inventory);
        apply_pending_add_checks(app, inventory);
        apply_pending_cloud(app, inventory);
        apply_pending_prune_checks(app, inventory);
        apply_pending_changes(app, inventory);
        apply_pending_browse(app, inventory);
        poll_host_probes(app, inventory);
        app.clear_stale_scan_status();

        if last_vdevs_auto_refresh.is_none_or(|t| t.elapsed() >= VDEVS_AUTO_REFRESH_INTERVAL) {
            if let (Some(pool), Some(host)) = (app.drilled_pool.clone(), inventory.hosts.get(app.selected_host)) {
                app.refresh_vdevs(host.runner.clone(), pool);
                last_vdevs_auto_refresh = Some(Instant::now());
            }
        }
        if let (Some(interval), Some(host)) = (app.auto_refresh_interval, inventory.hosts.get(app.selected_host)) {
            app.auto_refresh(host.runner.clone(), Instant::now(), interval);
        }
        if let Some(host) = inventory.hosts.get(app.selected_host) {
            app.poll_metrics(host.runner.clone(), Instant::now());
        }
        for section in refreshes {
            if let Some(host) = inventory.hosts.get(app.selected_host) {
                match section {
                    Section::Pools => app.refresh_pools(host.runner.clone()),
                    Section::Datasets => app.refresh_datasets(host.runner.clone()),
                    Section::Snapshots => app.refresh_snapshots(host.runner.clone()),
                    Section::Disks => app.refresh_disks(host.runner.clone()),
                    Section::Shares => app.refresh_shares(host.runner.clone()),
                    Section::BootEnvs => app.refresh_bootenvs(host.runner.clone()),
                    Section::Dashboard => app.refresh_dashboard(),
                    Section::Jobs => app.refresh_jobs(),
                    Section::Cloud => app.refresh_cloud(),
                    Section::Alerts => app.refresh_alerts(),
                    Section::Settings => app.refresh_settings(),
                    Section::Vdevs => {
                        if let Some(pool) = app.drilled_pool.clone() {
                            app.refresh_vdevs(host.runner.clone(), pool);
                        }
                    }
                    Section::PoolProperties => {
                        if let Some(pool) = app.selected_pool_name() {
                            app.refresh_pool_properties(host.runner.clone(), pool);
                        }
                    }
                    Section::DatasetProperties => {
                        if let Some(dataset) = app.selected_dataset_name() {
                            app.refresh_dataset_properties(host.runner.clone(), dataset);
                        }
                    }
                    Section::DatasetPermissions => {
                        if let Some(dataset) = app.selected_dataset_name() {
                            app.refresh_dataset_permissions(host.runner.clone(), dataset);
                        }
                    }
                    Section::DatasetAcl => {
                        if let Some(path) = app.selected_dataset_mountpoint() {
                            app.refresh_dataset_acl(host.runner.clone(), path);
                        }
                    }
                    Section::Hosts => {}
                }
            }
        }

        terminal
            .draw(|f| layout::draw(f, app))
            .map_err(|e| AppError::Config(format!("drawing frame: {e}")))?;

        if event::poll(Duration::from_millis(250))
            .map_err(|e| AppError::Config(format!("polling events: {e}")))?
        {
            if let Some(key) = match event::read().map_err(|e| AppError::Config(format!("reading event: {e}")))? {
                Event::Key(key) if enter_gate.accept(key.code, Instant::now()) => Some(key),
                _ => None,
            } {
                if app.picker.is_some() {
                    app.handle_picker_key(key.code);
                } else if app.modal.is_some() {
                    if let Some(action) = app.handle_modal_key(key.code) {
                        if let Some(host) = inventory.hosts.get(action.host) {
                            app.dispatch_mutation(action, host.runner.clone());
                        }
                    }
                } else {
                    app.handle_key(key.code);
                    if key.code == KeyCode::Char('r') {
                        if inventory.hosts.get(app.selected_host).is_none() && app.current_section() == Section::Dashboard {
                            app.refresh_dashboard();
                        }
                        if let Some(host) = inventory.hosts.get(app.selected_host) {
                            if let Some(pool) = app.drilled_pool.clone() {
                                app.refresh_vdevs(host.runner.clone(), pool);
                            } else if app.pane_focus == PaneFocus::Right {
                                match app.current_section() {
                                    Section::Pools => {
                                        if let Some(pool) = app.selected_pool_name() {
                                            app.refresh_pool_properties(host.runner.clone(), pool);
                                        }
                                    }
                                    Section::Datasets => match app.dataset_detail_tab {
                                        DatasetDetailTab::Properties => {
                                            if let Some(dataset) = app.selected_dataset_name() {
                                                app.refresh_dataset_properties(host.runner.clone(), dataset);
                                            }
                                        }
                                        DatasetDetailTab::Permissions => {
                                            if let Some(dataset) = app.selected_dataset_name() {
                                                app.refresh_dataset_permissions(host.runner.clone(), dataset);
                                            }
                                        }
                                        DatasetDetailTab::Acl => {
                                            if let Some(path) = app.selected_dataset_mountpoint() {
                                                app.refresh_dataset_acl(host.runner.clone(), path);
                                            }
                                        }
                                    },
                                    _ => {}
                                }
                            } else {
                                match app.current_section() {
                                    Section::Pools => app.refresh_pools(host.runner.clone()),
                                    Section::Datasets => app.refresh_datasets(host.runner.clone()),
                                    Section::Snapshots => app.refresh_snapshots(host.runner.clone()),
                                    Section::Disks => app.refresh_disks(host.runner.clone()),
                                    Section::Shares => app.refresh_shares(host.runner.clone()),
                                    Section::BootEnvs => app.refresh_bootenvs(host.runner.clone()),
                                    Section::Dashboard => app.refresh_dashboard(),
                                    Section::Jobs => app.refresh_jobs(),
                                    Section::Cloud => app.refresh_cloud(),
                                    Section::Alerts => app.refresh_alerts(),
                                    Section::Settings => app.refresh_settings(),
                                    Section::Hosts
                                    | Section::Vdevs
                                    | Section::PoolProperties
                                    | Section::DatasetProperties
                                    | Section::DatasetPermissions
                                    | Section::DatasetAcl => {}
                                }
                            }
                        }
                    }
                    if key.code == KeyCode::Enter {
                        if let Some(pool) = app.drilled_pool.clone() {
                            if let Some(host) = inventory.hosts.get(app.selected_host) {
                                app.refresh_vdevs(host.runner.clone(), pool);
                            }
                        }
                    }
                    if key.code == KeyCode::Right && app.pane_focus == PaneFocus::Right {
                        if let Some(host) = inventory.hosts.get(app.selected_host) {
                            match app.current_section() {
                                Section::Pools => {
                                    if let Some(pool) = app.selected_pool_name() {
                                        app.refresh_pool_properties(host.runner.clone(), pool);
                                    }
                                }
                                Section::Datasets => {
                                    if let Some(dataset) = app.selected_dataset_name() {
                                        app.refresh_dataset_properties(host.runner.clone(), dataset);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    if (key.code == KeyCode::Char('[') || key.code == KeyCode::Char(']')) && app.pane_focus == PaneFocus::Right && app.current_section() == Section::Datasets {
                        if let Some(host) = inventory.hosts.get(app.selected_host) {
                            match app.dataset_detail_tab {
                                DatasetDetailTab::Permissions => {
                                    if let Some(dataset) = app.selected_dataset_name() {
                                        app.refresh_dataset_permissions(host.runner.clone(), dataset);
                                    }
                                }
                                DatasetDetailTab::Acl => {
                                    if let Some(path) = app.selected_dataset_mountpoint() {
                                        app.refresh_dataset_acl(host.runner.clone(), path);
                                    }
                                }
                                DatasetDetailTab::Properties => {}
                            }
                        }
                    }
                }
            }
        }
        for action in std::mem::take(&mut app.pending_mutations) {
            if let Some(host) = inventory.hosts.get(action.host) {
                app.dispatch_mutation(action, host.runner.clone());
            }
        }
        if app.pending_shell {
            app.pending_shell = false;
            if let Some(host) = inventory.hosts.get(app.selected_host) {
                match &signals {
                    Err(e) => app.set_error(format!("cannot open a shell: {e}")),
                    Ok(signals) => match run_shell_session(terminal, &host.name, host.ssh_target.as_deref(), signals)? {
                        shell::ShellOutcome::Finished => {
                            app.status_message = None;
                            // The shell may have changed ZFS state. Only a finished shell
                            // refreshes: a refresh would wipe a failure message.
                            app.refresh_visible(host.runner.clone());
                        }
                        shell::ShellOutcome::ConnectFailed => app.set_error(format!("the shell on {} could not connect or the connection was lost", host.name)),
                        shell::ShellOutcome::SpawnFailed(e) => app.set_error(format!("could not start a shell on {}: {e}", host.name)),
                    },
                }
            }
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn apply_pending_new_hosts_applies_every_queued_host_keeping_both_lists_aligned() {
        // A final review caught that an earlier single-`Option`
        // version of the signal this drains silently lost a second
        // host whose probe resolved in the same event-loop tick as
        // the first's. Queuing two hosts before a single drain call
        // and asserting both land, in order, in both `app.hosts` and
        // `inventory.hosts` is the regression test for that.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mut app = App::new(vec!["local".to_string()]);
        let mut inventory = HostInventory::from_config(&Config::default());
        app.new_host_tags.insert("hosta".to_string(), "lab".to_string());
        app.pending_new_hosts = vec![
            ("hosta".to_string(), "hosta.example.com".to_string()),
            ("hostb".to_string(), "hostb.example.com".to_string()),
        ];

        apply_pending_new_hosts(&mut app, &mut inventory, &config_path);

        assert_eq!(app.hosts, vec!["local".to_string(), "hosta".to_string(), "hostb".to_string()]);
        let inventory_names: Vec<&str> = inventory.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(inventory_names, vec!["local", "hosta", "hostb"]);
        assert_eq!(app.host_addresses, vec!["local", "hosta.example.com", "hostb.example.com"], "addresses stay aligned with hosts");
        assert!(app.pending_new_hosts.is_empty());
        assert_eq!(app.host_tags, vec![String::new(), "lab".to_string(), String::new()]);
        assert_eq!(inventory.hosts[1].tag.as_deref(), Some("lab"));
        assert!(app.new_host_tags.is_empty());
    }

    #[test]
    fn apply_pending_new_hosts_is_a_noop_when_nothing_is_queued() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mut app = App::new(vec!["local".to_string()]);
        let mut inventory = HostInventory::from_config(&Config::default());

        apply_pending_new_hosts(&mut app, &mut inventory, &config_path);

        assert_eq!(app.hosts, vec!["local".to_string()]);
        assert_eq!(inventory.hosts.len(), 1);
    }

    #[test]
    fn apply_pending_account_fetches_drains_the_queue_and_spawns_one_fetch_per_host() {
        use crate::exec::test_support::FakeRunner;
        use crate::hosts::Host;
        use std::sync::Arc;
        let mut app = App::new(vec!["local".to_string()]);
        let runner = FakeRunner::new("")
            .route("getent", "passwd", "root:*:0:0::/root:/bin/sh\n")
            .route("getent", "group", "wheel:*:0:root\n");
        let inventory = HostInventory { hosts: vec![Host { name: "local".into(), runner: Arc::new(runner), ssh_target: None, tag: None }], mux_dir: None };
        app.pending_account_fetch = vec![0];
        apply_pending_account_fetches(&mut app, &inventory);
        assert!(app.pending_account_fetch.is_empty());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.drain_worker_messages();
            if app.account_cache.contains_key(&0) || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(app.account_cache.contains_key(&0), "worker result must arrive and be cached");
    }

    #[test]
    fn apply_pending_account_fetches_fails_the_picker_for_a_host_index_that_no_longer_exists() {
        use crate::ui::picker::{AccountField, PickerLoad, PickerState};
        let mut app = App::new(vec!["local".to_string()]);
        let inventory = HostInventory::from_config(&Config::default());
        app.picker = Some(PickerState {
            values: vec![],
            filter: String::new(),
            selected: 0,
            checked: std::collections::HashSet::new(),
            multi_select: false,
            load: PickerLoad::Loading,
            note: None,
            account: Some((9, AccountField::User)),
            pool_host: None,
        });
        app.account_fetch_in_flight.insert(9, Instant::now());
        app.pending_account_fetch = vec![9];
        apply_pending_account_fetches(&mut app, &inventory);
        assert!(app.pending_account_fetch.is_empty());
        app.drain_worker_messages();
        assert!(matches!(&app.picker.as_ref().unwrap().load, PickerLoad::Failed(m) if m.contains("unknown host index 9")));
        assert!(app.account_fetch_in_flight.is_empty());
    }

    #[test]
    fn apply_pending_pool_fetches_lists_dataset_names_for_a_dataset_fetch() {
        use crate::exec::test_support::FakeRunner;
        use crate::hosts::Host;
        use std::sync::Arc;
        let mut app = App::new(vec!["local".to_string()]);
        let runner = FakeRunner::new("tank/data\t1\t2\t3\t/tank/data\tfilesystem\tyes\n");
        let inventory = HostInventory { hosts: vec![Host { name: "local".into(), runner: Arc::new(runner), ssh_target: None, tag: None }], mux_dir: None };
        let kind = crate::ui::modal::FormKind::SnapshotJob { hosts: vec!["local".to_string()], existing: None, jobs: Vec::new() };
        app.picker = crate::ui::picker::build_picker_state(&kind, 1, "");
        app.pending_pool_fetch = vec![(0, crate::ui::picker::LiveNames::Datasets)];
        apply_pending_pool_fetches(&mut app, &inventory);
        assert!(app.pending_pool_fetch.is_empty());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.drain_worker_messages();
            if app.picker.as_ref().is_some_and(|p| p.load == crate::ui::picker::PickerLoad::Ready) || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(app.picker.as_ref().unwrap().values, vec!["tank/data"]);
    }


    fn held_save(host: usize) -> crate::ui::state::PruneCheck {
        let mut job = crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "local", "backup/data");
        job.dest_keep = Some(3);
        let action = crate::ui::modal::PendingAction { host: 0, section: crate::ui::state::Section::Jobs, call: crate::ui::modal::MutationCall::SaveJob { state_dir: "/x".into(), job: Box::new(job) } };
        let form = crate::ui::modal::Modal::Form { title: "x".into(), fields: vec![], focused: 0, description: None, kind: crate::ui::modal::FormKind::AddHost };
        crate::ui::state::PruneCheck { host, host_name: "local".into(), dest_dataset: "backup/data".into(), prefix: "hourly".into(), keep: 3, form, action }
    }

    #[test]
    fn apply_pending_prune_checks_lists_the_destination_on_its_hosts_runner_and_carries_on_with_the_save() {
        use crate::exec::test_support::FakeRunner;
        use crate::hosts::Host;
        use std::sync::Arc;
        let mut app = App::new(vec!["local".to_string()]);
        let inventory = HostInventory { hosts: vec![Host { name: "local".into(), runner: Arc::new(FakeRunner::new("backup/data@hourly-20261006T010000Z\t1\t1\n")), ssh_target: None, tag: None }], mux_dir: None };
        app.prune_check = Some(held_save(0));
        app.pending_prune_check = vec![crate::ui::state::PendingPruneCheck { host: 0, host_name: "local".into(), dest_dataset: "backup/data".into() }];
        apply_pending_prune_checks(&mut app, &inventory);
        assert!(app.pending_prune_check.is_empty());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.pending_mutations.is_empty() && std::time::Instant::now() < deadline {
            app.drain_worker_messages();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(app.pending_mutations.len(), 1, "one own snapshot, keep 3: nothing to destroy, so it is saved");
    }

    #[test]
    fn a_prune_check_for_an_unknown_host_index_hands_the_form_back_with_a_reason() {
        let mut app = App::new(vec!["local".to_string()]);
        let inventory = HostInventory { hosts: vec![], mux_dir: None };
        app.prune_check = Some(held_save(3));
        app.pending_prune_check = vec![crate::ui::state::PendingPruneCheck { host: 3, host_name: "gone".into(), dest_dataset: "backup/data".into() }];
        apply_pending_prune_checks(&mut app, &inventory);
        app.drain_worker_messages();
        assert!(app.modal.is_some() && app.pending_mutations.is_empty());
        assert!(app.status_message.clone().unwrap_or_default().contains("unknown host 'gone'"));
    }

    #[test]
    fn apply_pending_preflights_runs_the_check_on_the_hosts_runner_and_reports_it() {
        use crate::exec::test_support::FakeRunner;
        use crate::hosts::Host;
        use std::sync::Arc;
        let mut app = App::new(vec!["local".to_string()]);
        let inventory = HostInventory { hosts: vec![Host { name: "local".into(), runner: Arc::new(FakeRunner::new("{\"id\":\"x\"}")), ssh_target: None, tag: None }], mux_dir: None };
        app.backup_preflight = Some(crate::ui::state::PreflightCheck { host: 0, repository: "/r".into(), password_file: "/pw".into(), result: None, file_note: None });
        app.pending_preflight = vec![crate::ui::state::PendingPreflight { host: 0, host_name: "local".into(), repository: "/r".into(), password_file: "/pw".into() }];
        apply_pending_preflights(&mut app, &inventory);
        assert!(app.pending_preflight.is_empty());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.backup_preflight.as_ref().is_some_and(|c| c.result.is_none()) && std::time::Instant::now() < deadline {
            app.drain_worker_messages();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(app.backup_preflight.unwrap().result, Some(crate::restic::Preflight::Ok));
    }

    #[test]
    fn a_preflight_for_an_unknown_host_index_reports_a_problem_instead_of_waiting_forever() {
        let mut app = App::new(vec!["local".to_string()]);
        let inventory = HostInventory { hosts: vec![], mux_dir: None };
        app.backup_preflight = Some(crate::ui::state::PreflightCheck { host: 3, repository: "/r".into(), password_file: "/pw".into(), result: None, file_note: None });
        app.pending_preflight = vec![crate::ui::state::PendingPreflight { host: 3, host_name: "gone".into(), repository: "/r".into(), password_file: "/pw".into() }];
        apply_pending_preflights(&mut app, &inventory);
        app.drain_worker_messages();
        assert!(matches!(app.backup_preflight.unwrap().result, Some(crate::restic::Preflight::Problem(m)) if m.contains("unknown host")));
    }

    #[test]
    fn enter_repeating_faster_than_a_deliberate_press_is_ignored() {
        let mut gate = EnterDebounce::default();
        let t0 = Instant::now();
        assert!(gate.accept(KeyCode::Enter, t0), "the first Enter counts");
        assert!(!gate.accept(KeyCode::Enter, t0 + Duration::from_millis(30)), "a held key repeating");
        assert!(!gate.accept(KeyCode::Enter, t0 + Duration::from_millis(60)), "still held: each ignored repeat extends the window");
        assert!(gate.accept(KeyCode::Enter, t0 + Duration::from_millis(60 + 251)), "a fresh press after the window counts");
    }

    #[test]
    fn other_keys_always_pass_and_do_not_open_the_window() {
        let mut gate = EnterDebounce::default();
        let t0 = Instant::now();
        assert!(gate.accept(KeyCode::Char('y'), t0));
        assert!(gate.accept(KeyCode::Char('y'), t0 + Duration::from_millis(5)), "typing is never debounced");
        assert!(gate.accept(KeyCode::Enter, t0 + Duration::from_millis(10)), "an Enter after other keys is a first Enter");
        assert!(gate.accept(KeyCode::Down, t0 + Duration::from_millis(20)));
        assert!(!gate.accept(KeyCode::Enter, t0 + Duration::from_millis(40)));
    }
}
