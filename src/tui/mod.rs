mod app;
mod detail;
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
use url::Url;

use self::detail::ImageDetail;
use crate::registry::{ImageConfigBlob, Manifest, RegistryClient};

use self::app::{ConfirmAction, Focus, LoadState, Modal};
use self::event::{AppEvent, spawn_event_reader};

const TICK_MS: u64 = 200;
const PAGE_SIZE: u32 = 100;

pub async fn run(registry_name: String, registry_url: String) -> anyhow::Result<()> {
    let url = Url::parse(&registry_url)?;
    let client = RegistryClient::new(url);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, client, registry_name, registry_url).await;

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
    client: RegistryClient,
    registry_name: String,
    registry_url: String,
) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(128);
    spawn_event_reader(tx.clone());

    let mut tick = interval(Duration::from_millis(TICK_MS));
    let mut app = App::new(registry_name, registry_url);

    // Kick off initial catalog load.
    app.repo_load = LoadState::Loading;
    spawn_repos_fetch(client.clone(), None, tx.clone());

    loop {
        terminal.draw(|f| ui::draw(f, &mut app))?;

        // Snapshot selections before handling event (to detect changes).
        let prev_repo = app.selected_repo().map(str::to_owned);
        let prev_tag = app.selected_tag().map(str::to_owned);

        tokio::select! {
            biased;
            Some(ev) = rx.recv() => {
                handle_event(&mut app, ev);
            }
            _ = tick.tick() => {
                app.tick();
            }
        }

        if app.should_quit {
            break;
        }

        // Detect repo selection change → reload tags.
        let new_repo = app.selected_repo().map(str::to_owned);
        if new_repo != prev_repo
            && let Some(repo) = new_repo
        {
            app.start_tags_load(repo.clone());
            spawn_tags_fetch(client.clone(), repo, None, tx.clone());
        }

        // Background pagination: load more repos if user is near the end.
        // Detect tag selection change → reload detail.
        let new_tag = app.selected_tag().map(str::to_owned);
        if new_tag != prev_tag
            && let Some(tag) = new_tag
            && let Some(repo) = app.selected_repo().map(str::to_owned)
        {
            app.start_detail_load(tag.clone());
            spawn_detail_fetch(
                client.clone(),
                repo,
                tag,
                app.registry_url.clone(),
                tx.clone(),
            );
        }

        if app.should_load_more_repos() {
            app.repo_load = LoadState::Loading;
            spawn_repos_fetch(client.clone(), app.repos_cursor.clone(), tx.clone());
        }

        // Background pagination: load more tags if user is near the end.
        if app.should_load_more_tags()
            && let Some(repo) = app.current_repo.clone()
        {
            app.tag_load = LoadState::Loading;
            spawn_tags_fetch(client.clone(), repo, app.tags_cursor.clone(), tx.clone());
        }
    }

    Ok(())
}

fn handle_event(app: &mut App, ev: AppEvent) {
    match ev {
        AppEvent::Key(key) => {
            if key.kind != KeyEventKind::Press {
                return;
            }
            handle_key(app, key.code, key.modifiers);
        }
        AppEvent::Resize(_, _) => {}
        AppEvent::Tick => app.tick(),
        AppEvent::ReposPage(repos, has_more) => app.on_repos_page(repos, has_more),
        AppEvent::ReposError(msg) => app.on_repos_error(msg),
        AppEvent::TagsPage(repo, tags, has_more) => app.on_tags_page(repo, tags, has_more),
        AppEvent::TagsError(msg) => app.on_tags_error(msg),
        AppEvent::DetailLoaded { repo, tag, detail } => {
            app.on_detail_loaded(repo, tag, *detail);
        }
        AppEvent::DetailError(msg) => app.on_detail_error(msg),
    }
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    // Modal takes highest priority.
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
        return;
    }

    // Filter mode: route chars to filter input.
    if app.filter_mode.is_some() {
        match code {
            KeyCode::Esc => app.clear_active_filter(),
            KeyCode::Enter | KeyCode::Tab => {
                app.filter_mode = None;
            }
            KeyCode::Backspace => app.pop_filter_char(),
            KeyCode::Char(ch) => app.push_filter_char(ch),
            _ => {}
        }
        return;
    }

    // Normal mode.
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::Tab => app.focus = app.focus.toggle(),
        KeyCode::BackTab => app.focus = app.focus.toggle(),
        KeyCode::Up | KeyCode::Char('k') => app.scroll_up(),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_down(),
        KeyCode::Enter => handle_enter(app),
        KeyCode::Char('/') => {
            app.filter_mode = Some(app.focus);
        }
        KeyCode::Char('s') if app.focus == Focus::Tags => {
            app.tag_sort = app.tag_sort.cycle();
            app.resort_tags();
        }
        KeyCode::Char('c') => handle_copy(app),
        KeyCode::Char('d') => handle_delete(app),
        _ => {}
    }
}

fn handle_enter(app: &mut App) {
    if app.focus == Focus::Repos && !app.tags.is_empty() {
        app.focus = Focus::Tags;
        if app.tags_state.selected().is_none() {
            // tags_state selection set by on_tags_page already
        }
    }
}

fn handle_copy(app: &mut App) {
    let Some(pull_url) = app.detail.as_ref().map(|d| d.pull_url.clone()) else {
        return;
    };
    match crate::clipboard::copy_to_clipboard(&pull_url) {
        Ok(()) => app.set_status(format!("✓ Copied: {pull_url}")),
        Err(e) => app.set_status(format!("Clipboard error: {e}")),
    }
}

fn handle_delete(app: &mut App) {
    if app.focus == Focus::Tags
        && let Some(tag) = app.selected_tag()
    {
        let msg = format!("Delete tag '{tag}'?");
        app.modal = Modal::Confirm {
            message: msg,
            on_confirm: ConfirmAction::DeleteManifest,
        };
    }
}

fn handle_confirm(app: &mut App, action: ConfirmAction) {
    match action {
        ConfirmAction::DeleteManifest => {
            app.set_status("Delete queued (not yet implemented)");
        }
    }
}

// ------------------------------------------------------------------
// Background task spawners
// ------------------------------------------------------------------

fn spawn_repos_fetch(client: RegistryClient, cursor: Option<String>, tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        match client.catalog_page(PAGE_SIZE, cursor.as_deref()).await {
            Ok((catalog, has_more)) => {
                let _ = tx
                    .send(AppEvent::ReposPage(catalog.repositories, has_more))
                    .await;
            }
            Err(e) => {
                let _ = tx.send(AppEvent::ReposError(e.to_string())).await;
            }
        }
    });
}

fn spawn_tags_fetch(
    client: RegistryClient,
    repo: String,
    cursor: Option<String>,
    tx: mpsc::Sender<AppEvent>,
) {
    tokio::spawn(async move {
        match client.tags_page(&repo, PAGE_SIZE, cursor.as_deref()).await {
            Ok((tag_list, has_more)) => {
                let _ = tx
                    .send(AppEvent::TagsPage(repo, tag_list.tags, has_more))
                    .await;
            }
            Err(e) => {
                let _ = tx.send(AppEvent::TagsError(e.to_string())).await;
            }
        }
    });
}

fn spawn_detail_fetch(
    client: RegistryClient,
    repo: String,
    tag: String,
    registry_url: String,
    tx: mpsc::Sender<AppEvent>,
) {
    tokio::spawn(async move {
        let manifest_resp = match client.get_manifest(&repo, &tag).await {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(AppEvent::DetailError(e.to_string())).await;
                return;
            }
        };

        let config: Option<ImageConfigBlob> = match &manifest_resp.manifest {
            Manifest::Image(img) => match client.get_blob(&repo, &img.config.digest).await {
                Ok(bytes) => serde_json::from_slice::<ImageConfigBlob>(&bytes).ok(),
                Err(_) => None,
            },
            Manifest::Index(_) => None,
        };

        let d = ImageDetail::from_manifest_and_config(
            &manifest_resp,
            config.as_ref(),
            &repo,
            &tag,
            &registry_url,
        );
        let _ = tx
            .send(AppEvent::DetailLoaded {
                repo,
                tag,
                detail: Box::new(d),
            })
            .await;
    });
}
