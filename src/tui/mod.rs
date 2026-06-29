mod app;
mod detail;
mod event;
mod ui;

use std::io;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::config::RegistryProfile;

pub async fn run(mut profiles: Vec<RegistryProfile>, initial_idx: usize) -> anyhow::Result<()> {
    if profiles.is_empty() {
        profiles.push(RegistryProfile {
            name: "local".to_owned(),
            url: "http://localhost:5000".to_owned(),
            username: None,
            registry_type: Default::default(),
        });
    }
    let initial_idx = initial_idx.min(profiles.len().saturating_sub(1));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event::event_loop(&mut terminal, profiles, initial_idx).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}
