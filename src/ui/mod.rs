pub mod layout;
pub mod modal;
pub mod state;

pub use state::App;

use crate::error::{AppError, Result};
use crate::hosts::HostInventory;
use crate::ui::state::PaneFocus;
use crate::ui::state::Section;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::time::Duration;

/// Runs the TUI. Terminal teardown (`disable_raw_mode`/`LeaveAlternateScreen`)
/// is always attempted before returning, regardless of which setup step
/// failed — otherwise a failure between `enable_raw_mode()` and a fully
/// constructed `Terminal` would leave the user's shell in raw mode with
/// no way to recover except closing the terminal window.
pub fn run_app(inventory: HostInventory) -> Result<()> {
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
        .and_then(|_| {
            let backend = CrosstermBackend::new(std::io::stdout());
            let mut terminal = Terminal::new(backend)
                .map_err(|e| AppError::Config(format!("creating terminal: {e}")))?;

            let host_names: Vec<String> = inventory.hosts.iter().map(|h| h.name.clone()).collect();
            let mut app = App::new(host_names);
            if let Some(first_host) = inventory.hosts.first() {
                app.refresh_pools(first_host.runner.clone());
            }

            event_loop(&mut terminal, &mut app, &inventory)
        });

    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen);

    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    inventory: &HostInventory,
) -> Result<()> {
    loop {
        let refreshes = app.drain_worker_messages();
        for section in refreshes {
            if let Some(host) = inventory.hosts.get(app.selected_host) {
                match section {
                    Section::Pools => app.refresh_pools(host.runner.clone()),
                    Section::Datasets => app.refresh_datasets(host.runner.clone()),
                    Section::Snapshots => app.refresh_snapshots(host.runner.clone()),
                    Section::Disks => app.refresh_disks(host.runner.clone()),
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
                if app.modal.is_some() {
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
                                    Section::Datasets => {
                                        if let Some(dataset) = app.selected_dataset_name() {
                                            app.refresh_dataset_properties(host.runner.clone(), dataset);
                                        }
                                    }
                                    _ => {}
                                }
                            } else {
                                match app.current_section() {
                                    Section::Pools => app.refresh_pools(host.runner.clone()),
                                    Section::Datasets => app.refresh_datasets(host.runner.clone()),
                                    Section::Snapshots => app.refresh_snapshots(host.runner.clone()),
                                    Section::Disks => app.refresh_disks(host.runner.clone()),
                                    Section::Hosts
                                    | Section::Vdevs
                                    | Section::PoolProperties
                                    | Section::DatasetProperties => {}
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
                }
            }
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}
