mod app;
mod event;
mod ui;

pub use app::App;

use std::io;
use std::time::Duration;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tokio::time::interval;

use self::app::{ConfirmAction, Focus, Modal};
use self::event::{AppEvent, spawn_event_reader};

const TICK_MS: u64 = 200;

pub async fn run(registry_name: String, registry_url: String) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, registry_name, registry_url).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    registry_name: String,
    registry_url: String,
) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
    spawn_event_reader(tx.clone());

    let mut tick = interval(Duration::from_millis(TICK_MS));
    let mut app = App::new(registry_name, registry_url);

    loop {
        terminal.draw(|f| ui::draw(f, &mut app))?;

        tokio::select! {
            Some(ev) = rx.recv() => {
                match ev {
                    AppEvent::Key(key) => {
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        if !handle_key(&mut app, key.code, key.modifiers) {
                            break;
                        }
                    }
                    AppEvent::Resize(_, _) => {}
                    AppEvent::Tick => app.tick(),
                }
            }
            _ = tick.tick() => {
                app.tick();
                // Spawn a fake tick event through the same path to wake the loop.
                // The interval handles the actual timing; this branch updates state.
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

/// Returns `false` when the app should quit.
fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    // Modal takes priority.
    if let Modal::Confirm { on_confirm, .. } = app.modal {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                handle_confirm(app, on_confirm);
                app.modal = Modal::None;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.modal = Modal::None;
                app.set_status("Cancelled");
            }
            _ => {}
        }
        return true;
    }

    match code {
        KeyCode::Char('q') => return false,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return false,
        KeyCode::Tab => app.focus = app.focus.toggle(),
        KeyCode::BackTab => app.focus = app.focus.toggle(),
        KeyCode::Up | KeyCode::Char('k') => app.scroll_up(),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_down(),
        KeyCode::Enter => handle_enter(app),
        KeyCode::Char('d') => handle_delete(app),
        _ => {}
    }
    true
}

fn handle_enter(app: &mut App) {
    match app.focus {
        Focus::Repos => {
            // Tag loading is handled by the async layer (PGM-270).
            // For now, move focus to tags if there are any.
            if !app.tags.is_empty() {
                app.focus = Focus::Tags;
                app.tags_state.select(Some(0));
            }
        }
        Focus::Tags => {}
    }
}

fn handle_delete(app: &mut App) {
    match app.focus {
        Focus::Tags => {
            if let Some(tag) = app.selected_tag() {
                let msg = format!("Delete tag '{tag}'?");
                app.modal = Modal::Confirm {
                    message: msg,
                    on_confirm: ConfirmAction::DeleteManifest,
                };
            }
        }
        Focus::Repos => {}
    }
}

fn handle_confirm(app: &mut App, action: ConfirmAction) {
    match action {
        ConfirmAction::DeleteManifest => {
            // Actual deletion wired in PGM-273.
            app.set_status("Delete queued (not yet implemented)");
        }
    }
}
