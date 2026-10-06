pub mod hints;
pub mod jobs;
pub mod layout;
pub mod metrics;
pub mod modal;
pub mod picker;
pub mod shell;
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
            app.theme = resolved.theme;
            app.status_message = resolved.note;
            if let Some(first_host) = inventory.hosts.first() {
                app.refresh_pools(first_host.runner.clone());
            }

            event_loop(&mut terminal, &mut app, &mut inventory, config_path, auto_refresh)
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
        if let Err(e) = inventory.apply_new_host(config_path, name, target) {
            app.set_error(format!("Host added, but could not save it to config.toml (it will not survive a restart): {e}"));
        }
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
    auto_refresh: Option<Duration>,
) -> Result<()> {
    // `None` = never refreshed yet, so the first poll happens immediately.
    let mut last_vdevs_auto_refresh: Option<Instant> = None;
    let signals = shell::ShellSignals::install().map_err(|e| e.to_string());
    loop {
        let refreshes = app.drain_worker_messages();
        app.refresh_jobs_if_stale(Instant::now());
        apply_pending_new_hosts(app, inventory, config_path);
        apply_pending_account_fetches(app, inventory);
        app.clear_stale_scan_status();

        if last_vdevs_auto_refresh.is_none_or(|t| t.elapsed() >= VDEVS_AUTO_REFRESH_INTERVAL) {
            if let (Some(pool), Some(host)) = (app.drilled_pool.clone(), inventory.hosts.get(app.selected_host)) {
                app.refresh_vdevs(host.runner.clone(), pool);
                last_vdevs_auto_refresh = Some(Instant::now());
            }
        }
        if let (Some(interval), Some(host)) = (auto_refresh, inventory.hosts.get(app.selected_host)) {
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
                    Section::Jobs => app.refresh_jobs(),
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
            if let Event::Key(key) =
                event::read().map_err(|e| AppError::Config(format!("reading event: {e}")))?
            {
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
                                    Section::Jobs => app.refresh_jobs(),
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
                        shell::ShellOutcome::ConnectFailed => app.set_error(format!("could not open a shell on {}", host.name)),
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
        app.pending_new_hosts = vec![
            ("hosta".to_string(), "hosta.example.com".to_string()),
            ("hostb".to_string(), "hostb.example.com".to_string()),
        ];

        apply_pending_new_hosts(&mut app, &mut inventory, &config_path);

        assert_eq!(app.hosts, vec!["local".to_string(), "hosta".to_string(), "hostb".to_string()]);
        let inventory_names: Vec<&str> = inventory.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(inventory_names, vec!["local", "hosta", "hostb"]);
        assert!(app.pending_new_hosts.is_empty());
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
        let inventory = HostInventory { hosts: vec![Host { name: "local".into(), runner: Arc::new(runner), ssh_target: None }] };
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
        });
        app.account_fetch_in_flight.insert(9, Instant::now());
        app.pending_account_fetch = vec![9];
        apply_pending_account_fetches(&mut app, &inventory);
        assert!(app.pending_account_fetch.is_empty());
        app.drain_worker_messages();
        assert!(matches!(&app.picker.as_ref().unwrap().load, PickerLoad::Failed(m) if m.contains("unknown host index 9")));
        assert!(app.account_fetch_in_flight.is_empty());
    }
}
