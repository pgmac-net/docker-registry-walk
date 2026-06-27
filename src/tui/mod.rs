mod app;
mod detail;
mod event;
mod ui;

pub use app::App;

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
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

use crate::config::RegistryProfile;
use crate::registry::{
    BearerCredentials, ImageConfigBlob, KeyringStore, Manifest, RegistryClient, RegistryError,
    search_dockerhub,
};

use self::app::{
    ConfirmAction, Focus, InputAction, InspectModal, LayerDiffModal, LoadState, Modal,
};
use self::detail::ImageDetail;
use self::event::{AppEvent, spawn_event_reader};

const TICK_MS: u64 = 200;
const PAGE_SIZE: u32 = 100;

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

    let result = event_loop(&mut terminal, profiles, initial_idx).await;

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
    profiles: Vec<RegistryProfile>,
    initial_idx: usize,
) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(128);
    spawn_event_reader(tx.clone());

    let mut tick = interval(Duration::from_millis(TICK_MS));
    let mut app = App::new(profiles.clone(), initial_idx);

    // Pre-build client for the initial profile.
    let mut clients: HashMap<String, RegistryClient> = HashMap::new();
    let init_client = make_client_for_profile(&profiles[initial_idx]);
    clients.insert(profiles[initial_idx].name.clone(), init_client);
    let mut active_name = profiles[initial_idx].name.clone();

    // Kick off initial catalog load.
    app.repo_load = LoadState::Loading;
    spawn_repos_fetch(clients[&active_name].clone(), None, tx.clone());

    loop {
        terminal.draw(|f| ui::draw(f, &mut app))?;

        // Snapshot selections before handling event (to detect changes).
        let prev_repo = app.selected_repo().map(str::to_owned);
        let prev_tag = app.selected_tag().map(str::to_owned);

        tokio::select! {
            biased;
            Some(ev) = rx.recv() => {
                match ev {
                    AppEvent::SwitchRegistry { idx } => {
                        let profile = &app.profiles[idx];
                        let name = profile.name.clone();
                        clients.entry(name.clone()).or_insert_with(|| make_client_for_profile(profile));
                        active_name = name;
                        app.start_registry_switch(idx);
                        spawn_repos_fetch(clients[&active_name].clone(), None, tx.clone());
                    }
                    AppEvent::ReposError { msg, auth_failed } => {
                        // After a password-entry retry, a 401 means scope
                        // rejection (not wrong credentials), so treat it the
                        // same as an authz failure and offer BrowseRepo.
                        let retry_pending = app.catalog_retry_pending;
                        app.catalog_retry_pending = false;
                        let mut show_browse = !auth_failed || retry_pending;
                        let mut prompt_password = false;
                        if auth_failed && !retry_pending && matches!(app.modal, Modal::None) {
                            let profile = &app.profiles[app.active_profile_idx];
                            if let Some(username) = profile.username.clone() {
                                // If the password was already loaded from keyring, the 401
                                // is a scope rejection (Docker Hub /v2/_catalog), not a
                                // missing credential — skip the prompt and show BrowseRepo.
                                let store = KeyringStore::new(&profile.name);
                                if store.get_password(&username).is_some() {
                                    show_browse = true;
                                } else {
                                    prompt_password = true;
                                }
                            }
                        }
                        app.on_repos_error(msg, show_browse);
                        if prompt_password {
                            let profile = &app.profiles[app.active_profile_idx];
                            if let Some(username) = profile.username.clone() {
                                app.modal = Modal::Input {
                                    prompt: format!("Password for {username}:"),
                                    value: String::new(),
                                    cursor: 0,
                                    on_confirm: InputAction::EnterPassword {
                                        profile_name: profile.name.clone(),
                                        username,
                                    },
                                };
                            }
                        }
                    }
                    AppEvent::PasswordEntered { profile_name, username, password } => {
                        let store = KeyringStore::new(&profile_name);
                        let _ = store.set_password(&username, &password);
                        if let Some(profile) = app.profiles.iter().find(|p| p.name == profile_name).cloned() {
                            let client = make_client_for_profile(&profile);
                            clients.insert(profile_name.clone(), client);
                        }
                        if active_name == profile_name {
                            app.start_registry_switch(app.active_profile_idx);
                            app.catalog_retry_pending = true;
                            spawn_repos_fetch(clients[&active_name].clone(), None, tx.clone());
                        }
                    }
                    AppEvent::DockerHubSearch { query, results } => {
                        if let Modal::SearchPicker {
                            value,
                            results: modal_results,
                            selected,
                            searching,
                            ..
                        } = &mut app.modal
                            && *value == query {
                                *modal_results = results;
                                *selected = 0;
                                *searching = false;
                            }
                    }
                    AppEvent::DockerHubSearchError(_) => {
                        if let Modal::SearchPicker { searching, .. } = &mut app.modal {
                            *searching = false;
                        }
                    }
                    ev => handle_event(&mut app, ev, &clients[&active_name], &tx),
                }
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
            spawn_tags_fetch(clients[&active_name].clone(), repo, None, tx.clone());
        }

        // Detect tag selection change → reload detail.
        let new_tag = app.selected_tag().map(str::to_owned);
        if new_tag != prev_tag
            && let Some(tag) = new_tag
            && let Some(repo) = app.selected_repo().map(str::to_owned)
        {
            app.start_detail_load(tag.clone());
            spawn_detail_fetch(
                clients[&active_name].clone(),
                repo,
                tag,
                app.registry_url.clone(),
                tx.clone(),
            );
        }

        // Background pagination: load more repos if user is near the end.
        if app.should_load_more_repos() {
            app.repo_load = LoadState::Loading;
            spawn_repos_fetch(
                clients[&active_name].clone(),
                app.repos_cursor.clone(),
                tx.clone(),
            );
        }

        // Background pagination: load more tags if user is near the end.
        if app.should_load_more_tags()
            && let Some(repo) = app.current_repo.clone()
        {
            app.tag_load = LoadState::Loading;
            spawn_tags_fetch(
                clients[&active_name].clone(),
                repo,
                app.tags_cursor.clone(),
                tx.clone(),
            );
        }
    }

    Ok(())
}

fn handle_event(app: &mut App, ev: AppEvent, client: &RegistryClient, tx: &mpsc::Sender<AppEvent>) {
    match ev {
        AppEvent::Key(key) => {
            if key.kind != KeyEventKind::Press {
                return;
            }
            handle_key(app, key.code, key.modifiers, client, tx);
        }
        AppEvent::Resize(_, _) => {}
        AppEvent::Tick => app.tick(),
        AppEvent::ReposPage(repos, has_more) => app.on_repos_page(repos, has_more),
        // Handled in event_loop; should not reach here.
        AppEvent::ReposError { .. }
        | AppEvent::PasswordEntered { .. }
        | AppEvent::DockerHubSearch { .. }
        | AppEvent::DockerHubSearchError(_) => {}
        AppEvent::BrowseRepo(repo) => {
            app.start_tags_load(repo.clone());
            app.focus = Focus::Tags;
            spawn_tags_fetch(client.clone(), repo, None, tx.clone());
        }
        AppEvent::TagsPage(repo, tags, has_more) => app.on_tags_page(repo, tags, has_more),
        AppEvent::TagsError(msg) => app.on_tags_error(msg),
        AppEvent::DetailLoaded { repo, tag, detail } => {
            app.on_detail_loaded(repo, tag, *detail);
        }
        AppEvent::DetailError(msg) => app.on_detail_error(msg),
        AppEvent::DeleteTagSuccess { repo, tag } => app.on_delete_success(&repo, &tag),
        AppEvent::DeleteTagError(msg) => app.on_delete_error(msg),
        AppEvent::CopyProgress { done, total } => {
            app.set_status(format!("Copying… {done}/{total} blobs"));
        }
        AppEvent::CopySuccess { dest } => app.set_status(format!("✓ Copied to {dest}")),
        AppEvent::CopyError(msg) => app.set_status(format!("✗ Copy failed: {msg}")),
        AppEvent::RetagSuccess { new_tag } => app.on_retag_success(new_tag),
        AppEvent::RetagError(msg) => app.on_retag_error(msg),
        // Handled directly in event_loop.
        AppEvent::SwitchRegistry { .. } => {}
        AppEvent::InspectLoaded { title, lines } => {
            app.modal = Modal::Inspect(Box::new(InspectModal {
                title,
                lines,
                scroll: 0,
            }));
        }
        AppEvent::InspectError(msg) => app.set_status(format!("✗ Inspect failed: {msg}")),
        AppEvent::PruneFound { repo, tags } => {
            if tags.is_empty() {
                app.set_status(format!("No digest-tagged manifests found in {repo}"));
            } else {
                let count = tags.len();
                app.modal = Modal::Confirm {
                    message: format!("Delete {count} digest-tagged manifest(s) in '{repo}'?"),
                    on_confirm: ConfirmAction::PruneDigestTags { repo, tags },
                };
            }
        }
        AppEvent::PruneComplete { repo, count } => {
            app.set_status(format!("✓ Pruned {count} manifest(s) in {repo}"));
        }
        AppEvent::PruneError(msg) => app.set_status(format!("✗ Prune failed: {msg}")),
        AppEvent::ExportProgress { done, total } => {
            app.set_status(format!("Exporting… {done}/{total} blobs"));
        }
        AppEvent::ExportComplete { path } => app.set_status(format!("✓ Exported to {path}")),
        AppEvent::ExportError(msg) => app.set_status(format!("✗ Export failed: {msg}")),
        AppEvent::DiffLoaded {
            repo,
            tag_a,
            tag_b,
            layers,
        } => {
            app.modal = Modal::LayerDiff(Box::new(LayerDiffModal {
                repo,
                tag_a,
                tag_b,
                layers,
                scroll: 0,
            }));
        }
        AppEvent::DiffError(msg) => app.set_status(format!("✗ Diff failed: {msg}")),
    }
}

fn handle_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    client: &RegistryClient,
    tx: &mpsc::Sender<AppEvent>,
) {
    // Modal takes highest priority.
    if matches!(app.modal, Modal::Confirm { .. }) {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let modal = std::mem::replace(&mut app.modal, Modal::None);
                if let Modal::Confirm { on_confirm, .. } = modal {
                    handle_confirm(on_confirm, client, tx);
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.modal = Modal::None;
                app.set_status("Cancelled");
            }
            _ => {}
        }
        return;
    }

    if matches!(app.modal, Modal::Input { .. }) {
        match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
                app.set_status("Cancelled");
            }
            KeyCode::Enter => {
                let modal = std::mem::replace(&mut app.modal, Modal::None);
                if let Modal::Input {
                    value, on_confirm, ..
                } = modal
                {
                    handle_input_confirm(value, on_confirm, client, tx);
                }
            }
            KeyCode::Left => {
                if let Modal::Input { cursor, .. } = &mut app.modal {
                    *cursor = cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Modal::Input { value, cursor, .. } = &mut app.modal {
                    *cursor = (*cursor + 1).min(value.chars().count());
                }
            }
            KeyCode::Home | KeyCode::Char('a') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let Modal::Input { cursor, .. } = &mut app.modal {
                    *cursor = 0;
                }
            }
            KeyCode::End | KeyCode::Char('e') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let Modal::Input { value, cursor, .. } = &mut app.modal {
                    *cursor = value.chars().count();
                }
            }
            KeyCode::Backspace => {
                if let Modal::Input { value, cursor, .. } = &mut app.modal
                    && *cursor > 0
                {
                    let byte_pos = value
                        .char_indices()
                        .nth(*cursor - 1)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    value.remove(byte_pos);
                    *cursor -= 1;
                }
            }
            KeyCode::Char(ch) => {
                if let Modal::Input { value, cursor, .. } = &mut app.modal {
                    let byte_pos = value
                        .char_indices()
                        .nth(*cursor)
                        .map(|(i, _)| i)
                        .unwrap_or(value.len());
                    value.insert(byte_pos, ch);
                    *cursor += 1;
                }
            }
            _ => {}
        }
        return;
    }

    if matches!(app.modal, Modal::SearchPicker { .. }) {
        match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
                app.set_status("Cancelled");
            }
            KeyCode::Enter => {
                let modal = std::mem::replace(&mut app.modal, Modal::None);
                if let Modal::SearchPicker {
                    value,
                    results,
                    selected,
                    ..
                } = modal
                {
                    let repo = results.into_iter().nth(selected).unwrap_or(value);
                    let _ = tx.try_send(AppEvent::BrowseRepo(repo));
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Modal::SearchPicker { selected, .. } = &mut app.modal {
                    *selected = selected.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Modal::SearchPicker {
                    results, selected, ..
                } = &mut app.modal
                    && !results.is_empty()
                {
                    *selected = (*selected + 1).min(results.len().saturating_sub(1));
                }
            }
            KeyCode::Left => {
                if let Modal::SearchPicker { cursor, .. } = &mut app.modal {
                    *cursor = cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Modal::SearchPicker { value, cursor, .. } = &mut app.modal {
                    *cursor = (*cursor + 1).min(value.chars().count());
                }
            }
            KeyCode::Home | KeyCode::Char('a') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let Modal::SearchPicker { cursor, .. } = &mut app.modal {
                    *cursor = 0;
                }
            }
            KeyCode::End | KeyCode::Char('e') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let Modal::SearchPicker { value, cursor, .. } = &mut app.modal {
                    *cursor = value.chars().count();
                }
            }
            KeyCode::Backspace => {
                if let Modal::SearchPicker {
                    value,
                    cursor,
                    searching,
                    results,
                    selected,
                    ..
                } = &mut app.modal
                    && *cursor > 0
                {
                    let byte_pos = value
                        .char_indices()
                        .nth(*cursor - 1)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    value.remove(byte_pos);
                    *cursor -= 1;
                    *results = Vec::new();
                    *selected = 0;
                    if value.trim().is_empty() {
                        *searching = false;
                    } else {
                        *searching = true;
                        spawn_dockerhub_search(value.clone(), tx.clone());
                    }
                }
            }
            KeyCode::Char(ch) => {
                if let Modal::SearchPicker {
                    value,
                    cursor,
                    searching,
                    results,
                    selected,
                    ..
                } = &mut app.modal
                {
                    let byte_pos = value
                        .char_indices()
                        .nth(*cursor)
                        .map(|(i, _)| i)
                        .unwrap_or(value.len());
                    value.insert(byte_pos, ch);
                    *cursor += 1;
                    *results = Vec::new();
                    *selected = 0;
                    *searching = true;
                    spawn_dockerhub_search(value.clone(), tx.clone());
                }
            }
            _ => {}
        }
        return;
    }

    if matches!(app.modal, Modal::Inspect(_)) {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.modal = Modal::None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Modal::Inspect(m) = &mut app.modal {
                    m.scroll = m.scroll.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Modal::Inspect(m) = &mut app.modal {
                    m.scroll = m.scroll.saturating_add(1);
                }
            }
            _ => {}
        }
        return;
    }

    if matches!(app.modal, Modal::LayerDiff(_)) {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.modal = Modal::None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Modal::LayerDiff(m) = &mut app.modal {
                    m.scroll = m.scroll.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Modal::LayerDiff(m) = &mut app.modal {
                    m.scroll = m.scroll.saturating_add(1);
                }
            }
            _ => {}
        }
        return;
    }

    if matches!(app.modal, Modal::Help { .. }) {
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
                app.modal = Modal::None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Modal::Help { scroll } = &mut app.modal {
                    *scroll = scroll.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Modal::Help { scroll } = &mut app.modal {
                    *scroll = scroll.saturating_add(1);
                }
            }
            _ => {}
        }
        return;
    }

    if matches!(app.modal, Modal::RegistrySelect { .. }) {
        let n = app.profiles.len();
        match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Modal::RegistrySelect { selected_idx } = &mut app.modal
                    && *selected_idx > 0
                {
                    *selected_idx -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Modal::RegistrySelect { selected_idx } = &mut app.modal
                    && *selected_idx + 1 < n
                {
                    *selected_idx += 1;
                }
            }
            KeyCode::Enter => {
                if let Modal::RegistrySelect { selected_idx } = app.modal {
                    app.modal = Modal::None;
                    let _ = tx.try_send(AppEvent::SwitchRegistry { idx: selected_idx });
                }
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
        KeyCode::Esc | KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::Tab | KeyCode::Right => app.focus = app.focus.toggle(),
        KeyCode::BackTab | KeyCode::Left => app.focus = app.focus.prev(),
        KeyCode::Up | KeyCode::Char('k') => app.scroll_up(),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_down(),
        KeyCode::Enter => handle_enter(app, client, tx),
        KeyCode::Char('/') => {
            app.filter_mode = Some(app.focus);
        }
        KeyCode::Char('s') if app.focus == Focus::Tags => {
            app.tag_sort = app.tag_sort.cycle();
            app.resort_tags();
        }
        KeyCode::Char('c') => handle_copy(app),
        KeyCode::Char('C') => handle_copy_image(app),
        KeyCode::Char('r') => handle_retag(app),
        KeyCode::Char('R') => handle_registry_select(app),
        KeyCode::Char('d') => handle_delete(app),
        KeyCode::Char('i') => handle_inspect(app, client, tx),
        KeyCode::Char('P') => handle_prune(app, client, tx),
        KeyCode::Char('e') => handle_export(app),
        KeyCode::Char('D') => handle_diff(app),
        KeyCode::Char('?') => app.modal = Modal::Help { scroll: 0 },
        _ => {}
    }
}

fn handle_enter(app: &mut App, client: &RegistryClient, tx: &mpsc::Sender<AppEvent>) {
    match app.focus {
        Focus::Repos if !app.tags.is_empty() => app.focus = Focus::Tags,
        Focus::Tags => handle_inspect(app, client, tx),
        _ => {}
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

fn handle_copy_image(app: &mut App) {
    let Some(tag) = app.selected_tag().map(str::to_owned) else {
        return;
    };
    let Some(repo) = app.current_repo.clone() else {
        return;
    };
    let prefilled = format!("{repo}:{tag}");
    app.modal = Modal::Input {
        prompt: "Copy to (repo:tag):".to_owned(),
        value: prefilled,
        cursor: 0,
        on_confirm: InputAction::CopyImage {
            src_repo: repo,
            src_tag: tag,
        },
    };
}

fn handle_retag(app: &mut App) {
    let Some(tag) = app.selected_tag().map(str::to_owned) else {
        return;
    };
    let Some(repo) = app.current_repo.clone() else {
        return;
    };
    app.modal = Modal::Input {
        prompt: format!("New tag for '{repo}:{tag}':"),
        value: String::new(),
        cursor: 0,
        on_confirm: InputAction::Retag { repo, src_tag: tag },
    };
}

fn handle_registry_select(app: &mut App) {
    let current = app.active_profile_idx;
    app.modal = Modal::RegistrySelect {
        selected_idx: current,
    };
}

fn handle_delete(app: &mut App) {
    if app.focus == Focus::Tags
        && let Some(tag) = app.selected_tag().map(str::to_owned)
        && let Some(repo) = app.current_repo.clone()
    {
        let msg = format!("Delete '{repo}:{tag}'?");
        app.modal = Modal::Confirm {
            message: msg,
            on_confirm: ConfirmAction::DeleteManifest { repo, tag },
        };
    }
}

fn handle_confirm(action: ConfirmAction, client: &RegistryClient, tx: &mpsc::Sender<AppEvent>) {
    match action {
        ConfirmAction::DeleteManifest { repo, tag } => {
            spawn_delete(client.clone(), repo, tag, tx.clone());
        }
        ConfirmAction::PruneDigestTags { repo, tags } => {
            spawn_prune(client.clone(), repo, tags, tx.clone());
        }
    }
}

// ------------------------------------------------------------------
// Background task spawners
// ------------------------------------------------------------------

fn handle_input_confirm(
    value: String,
    action: InputAction,
    client: &RegistryClient,
    tx: &mpsc::Sender<AppEvent>,
) {
    match action {
        InputAction::CopyImage { src_repo, src_tag } => {
            let src_tag_clone = src_tag.clone();
            let (dst_repo, dst_tag) = crate::ops::copy::parse_destination(&value, &src_tag_clone);
            spawn_copy(
                client.clone(),
                src_repo,
                src_tag,
                dst_repo.to_owned(),
                dst_tag.to_owned(),
                tx.clone(),
            );
        }
        InputAction::Retag { repo, src_tag } => {
            if !crate::ops::retag::validate_tag(&value) {
                let _ = tx.try_send(AppEvent::RetagError(format!("Invalid tag name '{value}'")));
                return;
            }
            spawn_retag(client.clone(), repo, src_tag, value, tx.clone());
        }
        InputAction::Export { repo, tag } => {
            spawn_export(client.clone(), repo, tag, value, tx.clone());
        }
        InputAction::DiffAgainst { repo, tag_a } => {
            spawn_diff(client.clone(), repo, tag_a, value, tx.clone());
        }
        InputAction::BrowseRepo => {
            if !value.is_empty() {
                let _ = tx.try_send(AppEvent::BrowseRepo(value));
            }
        }
        InputAction::EnterPassword {
            profile_name,
            username,
        } => {
            if !value.is_empty() {
                let _ = tx.try_send(AppEvent::PasswordEntered {
                    profile_name,
                    username,
                    password: value,
                });
            }
        }
    }
}

fn handle_inspect(app: &mut App, client: &RegistryClient, tx: &mpsc::Sender<AppEvent>) {
    let Some(tag) = app.selected_tag().map(str::to_owned) else {
        return;
    };
    let Some(repo) = app.current_repo.clone() else {
        return;
    };
    spawn_inspect(client.clone(), repo, tag, tx.clone());
}

fn handle_prune(app: &mut App, client: &RegistryClient, tx: &mpsc::Sender<AppEvent>) {
    let Some(repo) = app.current_repo.clone() else {
        return;
    };
    spawn_prune_find(client.clone(), repo, tx.clone());
}

fn handle_export(app: &mut App) {
    let Some(tag) = app.selected_tag().map(str::to_owned) else {
        return;
    };
    let Some(repo) = app.current_repo.clone() else {
        return;
    };
    let default_path = format!("{}-{}.tar", repo.replace('/', "-"), tag);
    app.modal = Modal::Input {
        prompt: "Export OCI tar to:".to_owned(),
        value: default_path,
        cursor: 0,
        on_confirm: InputAction::Export { repo, tag },
    };
}

fn handle_diff(app: &mut App) {
    let Some(tag) = app.selected_tag().map(str::to_owned) else {
        return;
    };
    let Some(repo) = app.current_repo.clone() else {
        return;
    };
    app.modal = Modal::Input {
        prompt: format!("Diff '{tag}' against tag:"),
        value: String::new(),
        cursor: 0,
        on_confirm: InputAction::DiffAgainst { repo, tag_a: tag },
    };
}

fn make_client_for_profile(profile: &RegistryProfile) -> RegistryClient {
    let url = match Url::parse(&profile.url) {
        Ok(u) => u,
        Err(_) => return RegistryClient::new(Url::parse("http://localhost:5000").unwrap()),
    };

    if let Some(username) = &profile.username {
        let store = KeyringStore::new(&profile.name);
        if let Some(password) = store.get_password(username) {
            let creds = BearerCredentials::new(&url, username.clone(), password);
            return RegistryClient::new(url).with_credentials(Arc::new(creds));
        }
    }

    RegistryClient::new(url)
}

fn spawn_copy(
    client: RegistryClient,
    src_repo: String,
    src_tag: String,
    dst_repo: String,
    dst_tag: String,
    tx: mpsc::Sender<AppEvent>,
) {
    tokio::spawn(async move {
        let dest = format!("{dst_repo}:{dst_tag}");
        let result = crate::ops::copy::copy_image(
            &client,
            &src_repo,
            &src_tag,
            &dst_repo,
            &dst_tag,
            |done, total| {
                let _ = tx.blocking_send(AppEvent::CopyProgress { done, total });
            },
        )
        .await;
        match result {
            Ok(()) => {
                let _ = tx.send(AppEvent::CopySuccess { dest }).await;
            }
            Err(e) => {
                let _ = tx.send(AppEvent::CopyError(e.to_string())).await;
            }
        }
    });
}

fn spawn_retag(
    client: RegistryClient,
    repo: String,
    src_tag: String,
    new_tag: String,
    tx: mpsc::Sender<AppEvent>,
) {
    tokio::spawn(async move {
        match crate::ops::retag::retag(&client, &repo, &src_tag, &new_tag).await {
            Ok(()) => {
                let _ = tx.send(AppEvent::RetagSuccess { new_tag }).await;
            }
            Err(e) => {
                let _ = tx.send(AppEvent::RetagError(e.to_string())).await;
            }
        }
    });
}

fn spawn_delete(client: RegistryClient, repo: String, tag: String, tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        match crate::ops::delete::delete_tag(&client, &repo, &tag).await {
            Ok(()) => {
                let _ = tx.send(AppEvent::DeleteTagSuccess { repo, tag }).await;
            }
            Err(e) => {
                let _ = tx.send(AppEvent::DeleteTagError(e.to_string())).await;
            }
        }
    });
}

fn spawn_inspect(client: RegistryClient, repo: String, tag: String, tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        let title = format!("{repo}:{tag}");
        match crate::ops::inspect::inspect(&client, &repo, &tag).await {
            Ok(result) => {
                let lines = crate::ops::inspect::build_lines(&result);
                let _ = tx.send(AppEvent::InspectLoaded { title, lines }).await;
            }
            Err(e) => {
                let _ = tx.send(AppEvent::InspectError(e.to_string())).await;
            }
        }
    });
}

fn spawn_prune_find(client: RegistryClient, repo: String, tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        match crate::ops::prune::find_digest_tags(&client, &repo).await {
            Ok(tags) => {
                let _ = tx.send(AppEvent::PruneFound { repo, tags }).await;
            }
            Err(e) => {
                let _ = tx.send(AppEvent::PruneError(e.to_string())).await;
            }
        }
    });
}

fn spawn_prune(
    client: RegistryClient,
    repo: String,
    tags: Vec<String>,
    tx: mpsc::Sender<AppEvent>,
) {
    tokio::spawn(async move {
        match crate::ops::prune::prune_digest_tags(&client, &repo, &tags).await {
            Ok(count) => {
                let _ = tx.send(AppEvent::PruneComplete { repo, count }).await;
            }
            Err(e) => {
                let _ = tx.send(AppEvent::PruneError(e.to_string())).await;
            }
        }
    });
}

fn spawn_export(
    client: RegistryClient,
    repo: String,
    tag: String,
    path: String,
    tx: mpsc::Sender<AppEvent>,
) {
    tokio::spawn(async move {
        let dest = std::path::PathBuf::from(&path);
        let result =
            crate::ops::export::export_image(&client, &repo, &tag, &dest, |done, total| {
                let _ = tx.blocking_send(AppEvent::ExportProgress { done, total });
            })
            .await;
        match result {
            Ok(()) => {
                let _ = tx.send(AppEvent::ExportComplete { path }).await;
            }
            Err(e) => {
                let _ = tx.send(AppEvent::ExportError(e.to_string())).await;
            }
        }
    });
}

fn spawn_diff(
    client: RegistryClient,
    repo: String,
    tag_a: String,
    tag_b: String,
    tx: mpsc::Sender<AppEvent>,
) {
    tokio::spawn(async move {
        match crate::ops::diff::diff_tags(&client, &repo, &tag_a, &tag_b).await {
            Ok(layers) => {
                let _ = tx
                    .send(AppEvent::DiffLoaded {
                        repo,
                        tag_a,
                        tag_b,
                        layers,
                    })
                    .await;
            }
            Err(e) => {
                let _ = tx.send(AppEvent::DiffError(e.to_string())).await;
            }
        }
    });
}

fn spawn_repos_fetch(client: RegistryClient, cursor: Option<String>, tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        match client.catalog_page(PAGE_SIZE, cursor.as_deref()).await {
            Ok((catalog, has_more)) => {
                let _ = tx
                    .send(AppEvent::ReposPage(catalog.repositories, has_more))
                    .await;
            }
            Err(e) => {
                let auth_failed = matches!(e, RegistryError::Unauthorized);
                let _ = tx
                    .send(AppEvent::ReposError {
                        msg: e.to_string(),
                        auth_failed,
                    })
                    .await;
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

fn spawn_dockerhub_search(query: String, tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        match search_dockerhub(&query).await {
            Ok(results) => {
                let _ = tx.send(AppEvent::DockerHubSearch { query, results }).await;
            }
            Err(e) => {
                let _ = tx.send(AppEvent::DockerHubSearchError(e.to_string())).await;
            }
        }
    });
}
