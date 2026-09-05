pub mod layout;
pub mod state;

pub use state::App;

use crate::error::{AppError, Result};
use crate::hosts::HostInventory;
use crate::ui::state::Section;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::time::Duration;

pub fn run_app(inventory: HostInventory) -> Result<()> {
    enable_raw_mode().map_err(|e| AppError::Config(format!("enabling raw mode: {e}")))?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|e| AppError::Config(format!("entering alternate screen: {e}")))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|e| AppError::Config(format!("creating terminal: {e}")))?;

    let host_names: Vec<String> = inventory.hosts.iter().map(|h| h.name.clone()).collect();
    let mut app = App::new(host_names);
    if let Some(first_host) = inventory.hosts.first() {
        app.refresh_pools(first_host.runner.clone());
    }

    let result = event_loop(&mut terminal, &mut app, &inventory);

    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);

    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    inventory: &HostInventory,
) -> Result<()> {
    loop {
        app.drain_worker_messages();
        terminal
            .draw(|f| layout::draw(f, app))
            .map_err(|e| AppError::Config(format!("drawing frame: {e}")))?;

        if event::poll(Duration::from_millis(250))
            .map_err(|e| AppError::Config(format!("polling events: {e}")))?
        {
            if let Event::Key(key) =
                event::read().map_err(|e| AppError::Config(format!("reading event: {e}")))?
            {
                app.handle_key(key.code);
                if key.code == KeyCode::Char('r') {
                    if let Some(host) = inventory.hosts.get(app.selected_host) {
                        match app.current_section() {
                            Section::Pools => app.refresh_pools(host.runner.clone()),
                            Section::Datasets => app.refresh_datasets(host.runner.clone()),
                            Section::Snapshots => app.refresh_snapshots(host.runner.clone()),
                            Section::Hosts => {}
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
