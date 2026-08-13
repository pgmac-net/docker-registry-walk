use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tokio::time::interval;
use url::Url;

use crate::config::{AuthKind, AuthMode, RegistryProfile};
use crate::ops::diff::DiffLayer;
use crate::registry::{
    AccessTokenCredentials, ArtifactoryRepo, BasicCredentials, BearerCredentials, Credentials,
    GhcrCredentials, ImageConfigBlob, KeyringStore, Manifest, RegistryClient, RegistryError,
    Secret, TOKEN_ACCOUNT, list_owners, list_packages, resolve_access_token, sanitize_pasted_token,
    search_dockerhub,
};

use super::app::{
    App, CatalogAttempt, ConfirmAction, Focus, InputAction, InspectModal, LayerDiffModal,
    LoadState, Modal, help_context_for,
};
// Only named directly in tests — production code only ever calls
// `help_context_for` and stores its result, never spells the type.
#[cfg(test)]
use super::app::HelpContext;
use super::detail::ImageDetail;
use super::input::InputState;
use super::ui;

const TICK_MS: u64 = 200;
const PAGE_SIZE: u32 = 100;

#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Resize,
    /// A terminal bracketed paste, delivered as one string rather than one
    /// `Key` event per character.
    Paste(String),
    ReposPage(Vec<String>, bool),
    ReposError {
        msg: String,
        /// True when the error is a 401 Unauthorized — credentials wrong or absent.
        auth_failed: bool,
    },
    PasswordEntered {
        profile_name: String,
        username: String,
        password: Secret,
    },
    TokenEntered {
        profile_name: String,
        token: Secret,
    },
    TagsPage(String, Vec<String>, bool),
    TagsError(String),
    DetailLoaded {
        repo: String,
        tag: String,
        detail: Box<ImageDetail>,
    },
    DetailError(String),
    DeleteTagSuccess {
        repo: String,
        tag: String,
    },
    DeleteTagError(String),
    CopyProgress {
        done: usize,
        total: usize,
    },
    CopySuccess {
        dest: String,
    },
    CopyError(String),
    RetagSuccess {
        new_tag: String,
    },
    RetagError(String),
    SwitchRegistry {
        idx: usize,
    },
    InspectLoaded {
        title: String,
        lines: Vec<String>,
    },
    InspectError(String),
    PruneFound {
        repo: String,
        tags: Vec<String>,
    },
    PruneComplete {
        repo: String,
        count: usize,
    },
    PruneError(String),
    ExportProgress {
        done: usize,
        total: usize,
    },
    ExportComplete {
        path: String,
    },
    ExportError(String),
    DiffLoaded {
        tag_a: String,
        tag_b: String,
        layers: Vec<DiffLayer>,
    },
    DiffError(String),
    /// Jump directly to a repository (e.g. entered by the user after catalog failure).
    BrowseRepo(String),
    /// Results from a Docker Hub search query.
    DockerHubSearch {
        query: String,
        results: Vec<String>,
    },
    DockerHubSearchError(String),
    /// Docker repo-keys hosted on a JFrog Artifactory instance.
    ArtifactoryRepos(Vec<ArtifactoryRepo>),
    ArtifactoryReposError(String),
    /// User picked a repo-key from the Artifactory picker.
    ArtifactoryRepoSelected(String),
    /// Up-navigation from inside an Artifactory repo-key back to the
    /// repo-key picker (`Backspace` / `u`).
    OpenArtifactoryRepoPicker,
    /// Container packages listed from the GitHub packages API, in `owner/name`
    /// form. `truncated` reports that the page cap cut the listing short.
    GhcrPackages {
        packages: Vec<String>,
        truncated: bool,
    },
    GhcrPackagesError(String),
    /// Up-navigation from a GHCR package list to the owner picker
    /// (`Backspace` / `u`).
    OpenGhcrOwnerPicker,
    /// Owner suggestions from the GitHub API (the token holder and their orgs).
    GhcrOwners(Vec<String>),
    GhcrOwnersError(String),
    /// User chose whose packages to browse.
    GhcrOwnerSelected(String),
}

/// Spawn a blocking thread that forwards crossterm events to `tx`.
///
/// The thread exits automatically when `tx` is closed (i.e. when the app quits
/// and the receiver is dropped).
pub fn spawn_event_reader(tx: mpsc::Sender<AppEvent>) {
    std::thread::spawn(move || {
        loop {
            match crossterm::event::poll(Duration::from_millis(20)) {
                Ok(true) => match crossterm::event::read() {
                    Ok(Event::Key(k)) => {
                        if tx.blocking_send(AppEvent::Key(k)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Resize(_, _)) if tx.blocking_send(AppEvent::Resize).is_err() => {
                        break;
                    }
                    Ok(Event::Resize(_, _)) => {}
                    Ok(Event::Paste(s)) => {
                        if tx.blocking_send(AppEvent::Paste(s)).is_err() {
                            break;
                        }
                    }
                    _ => {}
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });
}

// ------------------------------------------------------------------
// Event loop
// ------------------------------------------------------------------

pub(super) async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    profiles: Vec<RegistryProfile>,
    initial_idx: usize,
) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(128);
    spawn_event_reader(tx.clone());

    let mut tick = interval(Duration::from_millis(TICK_MS));
    let mut app = App::new(profiles.clone(), initial_idx);

    // Draw before the first client build. `tui::run` has already entered the
    // alternate screen, and the build reads the keyring — which is fast in the
    // normal case but blocks indefinitely on a locked Secret Service
    // collection (a D-Bus `Unlock` prompt). Without this frame that leaves the
    // user staring at a blank screen with nothing to indicate the app is even
    // running, since the loop's own `terminal.draw` is not reached until the
    // build returns.
    app.repo_load = LoadState::Loading;
    app.set_status(format!("Connecting to {}…", profiles[initial_idx].name));
    terminal.draw(|f| ui::draw(f, &mut app))?;

    // Pre-build client for the initial profile.
    let mut clients: HashMap<String, RegistryClient> = HashMap::new();
    let init_client = make_client_for_profile(&profiles[initial_idx]).await;
    clients.insert(profiles[initial_idx].name.clone(), init_client);
    let mut active_name = profiles[initial_idx].name.clone();

    // Kick off initial catalog load — or, for an Artifactory profile, fetch
    // the repo-key list and let the user pick one first.
    if profiles[initial_idx].is_artifactory() {
        app.start_artifactory_switch(initial_idx);
        spawn_artifactory_repos_fetch(clients[&active_name].clone(), tx.clone());
    } else if profiles[initial_idx].is_ghcr() {
        app.start_ghcr_switch(initial_idx);
        spawn_ghcr_packages_fetch(profiles[initial_idx].clone(), tx.clone());
    } else {
        app.repo_load = LoadState::Loading;
        spawn_repos_fetch(clients[&active_name].clone(), None, tx.clone());
    }

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
                        // Cloned so no borrow of `app.profiles[idx]` is held
                        // across the await below (`app` is mutated first).
                        let profile = app.profiles[idx].clone();
                        let name = profile.name.clone();

                        // Update app state *before* building the client, so the
                        // pre-build frame below shows the registry being
                        // switched to rather than the one being left.
                        if profile.is_artifactory() {
                            app.start_artifactory_switch(idx);
                        } else if profile.is_ghcr() {
                            app.start_ghcr_switch(idx);
                        } else {
                            app.start_registry_switch(idx);
                        }
                        active_name = name.clone();

                        if !clients.contains_key(&name) {
                            // Only on a cache miss — the build reads the
                            // keyring, so this is the path that can block. A
                            // cached switch draws no extra frame.
                            app.set_status(format!("Connecting to {name}…"));
                            terminal.draw(|f| ui::draw(f, &mut app))?;
                            let client = make_client_for_profile(&profile).await;
                            clients.insert(name.clone(), client);
                        }

                        if profile.is_artifactory() {
                            spawn_artifactory_repos_fetch(clients[&active_name].clone(), tx.clone());
                        } else if profile.is_ghcr() {
                            // Takes the profile, not the client: the packages
                            // API lives on api.github.com, a different origin
                            // from the client's ghcr.io base URL.
                            spawn_ghcr_packages_fetch(profile.clone(), tx.clone());
                        } else {
                            spawn_repos_fetch(clients[&active_name].clone(), None, tx.clone());
                        }
                    }
                    AppEvent::ArtifactoryRepos(repos) => {
                        app.on_artifactory_repos(repos);
                    }
                    AppEvent::ArtifactoryReposError(msg) => {
                        app.on_artifactory_repos_error(msg);
                    }
                    AppEvent::ArtifactoryRepoSelected(repo_key) => {
                        let profile_name = app.profiles[app.active_profile_idx].name.clone();
                        if let Some(base_client) =
                            artifactory_root_client(&clients, &profile_name).cloned()
                        {
                            match base_client.for_artifactory_repo(&repo_key) {
                                Ok(scoped_client) => {
                                    let scoped_url = scoped_client.base_url().to_string();
                                    let composite = format!("{profile_name}#{repo_key}");
                                    clients.insert(composite.clone(), scoped_client);
                                    active_name = composite;
                                    app.enter_artifactory_repo(&repo_key, scoped_url);
                                    spawn_repos_fetch(
                                        clients[&active_name].clone(),
                                        None,
                                        tx.clone(),
                                    );
                                }
                                Err(e) => {
                                    app.set_status(format!("✗ Artifactory: {e}"));
                                }
                            }
                        }
                    }
                    AppEvent::OpenArtifactoryRepoPicker => {
                        let idx = app.active_profile_idx;
                        if app.profiles[idx].is_artifactory() {
                            let profile_name = app.profiles[idx].name.clone();
                            app.open_artifactory_picker_cached();
                            if let Some(base_client) = clients.get(&profile_name).cloned() {
                                spawn_artifactory_repos_fetch(base_client, tx.clone());
                            }
                        }
                    }
                    AppEvent::OpenGhcrOwnerPicker => {
                        let idx = app.active_profile_idx;
                        if app.profiles[idx].is_ghcr() {
                            let profile = app.profiles[idx].clone();
                            app.open_ghcr_owner_picker();
                            spawn_ghcr_owners_fetch(profile, tx.clone());
                        }
                    }
                    AppEvent::GhcrOwners(owners) => app.on_ghcr_owners(owners),
                    AppEvent::GhcrOwnersError(msg) => app.on_ghcr_owners_error(msg),
                    AppEvent::GhcrOwnerSelected(owner) => {
                        // Only the profile's owner changes — the client is
                        // still pointed at ghcr.io, so it needs no rebuild.
                        app.apply_ghcr_owner(owner);
                        let profile = app.profiles[app.active_profile_idx].clone();
                        spawn_ghcr_packages_fetch(profile, tx.clone());
                    }
                    AppEvent::GhcrPackages {
                        packages,
                        truncated,
                    } => {
                        app.on_ghcr_packages(packages, truncated);
                    }
                    AppEvent::GhcrPackagesError(msg) => {
                        app.on_ghcr_packages_error(msg);
                    }
                    AppEvent::ReposError { msg, auth_failed } => {
                        // After a silent-reread or password-entry retry, a
                        // 401 means scope rejection (not wrong credentials),
                        // so treat it the same as an authz failure and offer
                        // BrowseRepo.
                        let attempt = app.catalog_attempt;
                        app.catalog_attempt = CatalogAttempt::Initial;
                        // Docker Hub never allows /v2/_catalog; a 401 there is
                        // always a policy restriction, not wrong credentials.
                        let is_dh = app
                            .profiles
                            .get(app.active_profile_idx)
                            .is_some_and(|p| p.is_dockerhub());
                        let profile = app.profiles[app.active_profile_idx].clone();

                        // A first auth failure may only mean the cached
                        // client's credentials predate something now in the
                        // keyring — another process wrote it, or it was
                        // unlocked after startup — so it's worth one silent
                        // re-read before bothering the user. Skipped for a
                        // profile with no credential source at all
                        // (`auth_prompt_for` returns `None`), since that would
                        // be a guaranteed-useless retry.
                        let should_reread = auth_failed
                            && !is_dh
                            && attempt == CatalogAttempt::Initial
                            && matches!(app.modal, Modal::None)
                            && auth_prompt_for(&profile).is_some();

                        if should_reread {
                            rebuild_clients_for_profile(&mut clients, &profile).await;
                            app.catalog_attempt = CatalogAttempt::AfterReread;
                            app.restart_catalog_load();
                            if let Some(client) = clients.get(&active_name).cloned() {
                                spawn_repos_fetch(client, None, tx.clone());
                            }
                        } else {
                            let show_browse =
                                !auth_failed || attempt == CatalogAttempt::AfterCredential || is_dh;
                            app.on_repos_error(msg, show_browse);
                            if auth_failed
                                && attempt != CatalogAttempt::AfterCredential
                                && !is_dh
                                && matches!(app.modal, Modal::None)
                            {
                                let profile_name = profile.name.clone();
                                app.modal = match auth_prompt_for(&profile) {
                                    Some(AuthPrompt::Password { username }) => Modal::Input {
                                        prompt: format!("Password for {username}:"),
                                        input: InputState::default(),
                                        on_confirm: InputAction::EnterPassword {
                                            profile_name,
                                            username,
                                        },
                                    },
                                    Some(AuthPrompt::Token) => Modal::Input {
                                        prompt: format!("Access token for {profile_name}:"),
                                        input: InputState::default(),
                                        on_confirm: InputAction::EnterToken { profile_name },
                                    },
                                    None => Modal::None,
                                };
                            }
                        }
                    }
                    AppEvent::PasswordEntered { profile_name, username, password } => {
                        store_secret(profile_name.clone(), username, password.expose().to_owned()).await;
                        retry_after_credential_change(
                            &mut app, &mut clients, &profile_name, &active_name, &tx,
                        ).await;
                    }
                    AppEvent::TokenEntered { profile_name, token } => {
                        store_secret(profile_name.clone(), TOKEN_ACCOUNT.to_owned(), token.expose().to_owned()).await;
                        retry_after_credential_change(
                            &mut app, &mut clients, &profile_name, &active_name, &tx,
                        ).await;
                    }
                    AppEvent::DockerHubSearch { query, results } => {
                        if let Modal::SearchPicker {
                            input,
                            results: modal_results,
                            selected,
                            searching,
                            ..
                        } = &mut app.modal
                            && input.buffer == query {
                                *modal_results = results;
                                *selected = 0;
                                *searching = false;
                            }
                    }
                    AppEvent::DockerHubSearchError(msg) => {
                        if let Modal::SearchPicker { searching, .. } = &mut app.modal {
                            *searching = false;
                        }
                        app.set_status(format!("✗ Search: {msg}"));
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

// ------------------------------------------------------------------
// Event dispatch
// ------------------------------------------------------------------

fn handle_event(app: &mut App, ev: AppEvent, client: &RegistryClient, tx: &mpsc::Sender<AppEvent>) {
    match ev {
        AppEvent::Key(key) => {
            if key.kind != KeyEventKind::Press {
                return;
            }
            handle_key(app, key.code, key.modifiers, client, tx);
        }
        AppEvent::Resize => {}
        AppEvent::Paste(text) => handle_paste(app, &text, tx),
        AppEvent::ReposPage(repos, has_more) => app.on_repos_page(repos, has_more),
        // Handled in event_loop; should not reach here.
        AppEvent::ReposError { .. }
        | AppEvent::PasswordEntered { .. }
        | AppEvent::TokenEntered { .. }
        | AppEvent::DockerHubSearch { .. }
        | AppEvent::DockerHubSearchError(_)
        | AppEvent::ArtifactoryRepos(_)
        | AppEvent::ArtifactoryReposError(_)
        | AppEvent::ArtifactoryRepoSelected(_)
        | AppEvent::OpenArtifactoryRepoPicker
        | AppEvent::GhcrPackages { .. }
        | AppEvent::GhcrPackagesError(_)
        | AppEvent::OpenGhcrOwnerPicker
        | AppEvent::GhcrOwners(_)
        | AppEvent::GhcrOwnersError(_)
        | AppEvent::GhcrOwnerSelected(_) => {}
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
        // — Operation dispatch — Add new operation result arms below this line.
        // To wire a new op: add a spawn_* fn, add AppEvent variants, add arm here.
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
            app.modal = Modal::Inspect(Box::new(InspectModal::new(title, lines)));
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
            tag_a,
            tag_b,
            layers,
            ..
        } => {
            app.modal = Modal::LayerDiff(Box::new(LayerDiffModal {
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
                    input, on_confirm, ..
                } = modal
                {
                    handle_input_confirm(input.buffer, on_confirm, client, tx);
                }
            }
            _ => {
                if let Modal::Input { input, .. } = &mut app.modal {
                    apply_input_key(input, code, modifiers);
                }
            }
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
                    input,
                    results,
                    selected,
                    ..
                } = modal
                {
                    let repo = results.into_iter().nth(selected).unwrap_or(input.buffer);
                    let _ = tx.try_send(AppEvent::BrowseRepo(repo));
                }
            }
            KeyCode::Up => {
                if let Modal::SearchPicker { selected, .. } = &mut app.modal {
                    *selected = selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if let Modal::SearchPicker {
                    results, selected, ..
                } = &mut app.modal
                    && !results.is_empty()
                {
                    *selected = (*selected + 1).min(results.len().saturating_sub(1));
                }
            }
            KeyCode::Char('?') => open_help(app),
            _ => {
                if let Modal::SearchPicker {
                    input,
                    results,
                    selected,
                    searching,
                    ..
                } = &mut app.modal
                {
                    let before = input.buffer.clone();
                    if apply_input_key(input, code, modifiers) && input.buffer != before {
                        *results = Vec::new();
                        *selected = 0;
                        if input.buffer.trim().is_empty() {
                            *searching = false;
                        } else {
                            *searching = true;
                            spawn_dockerhub_search(input.buffer.clone(), tx.clone());
                        }
                    }
                }
            }
        }
        return;
    }

    if matches!(app.modal, Modal::ArtifactoryPicker { .. }) {
        match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
                app.set_status("Cancelled");
            }
            KeyCode::Enter => {
                let selected = if let Modal::ArtifactoryPicker { selected, .. } = &app.modal {
                    *selected
                } else {
                    0
                };
                if let Some(repo_key) = app
                    .artifactory_filtered_repos()
                    .get(selected)
                    .map(|r| r.key.clone())
                {
                    let _ = tx.try_send(AppEvent::ArtifactoryRepoSelected(repo_key));
                }
            }
            KeyCode::Up => {
                if let Modal::ArtifactoryPicker { selected, .. } = &mut app.modal {
                    *selected = selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                let n = app.artifactory_filtered_repos().len();
                if let Modal::ArtifactoryPicker { selected, .. } = &mut app.modal
                    && n > 0
                {
                    *selected = (*selected + 1).min(n - 1);
                }
            }
            KeyCode::Char('?') => open_help(app),
            _ => {
                if let Modal::ArtifactoryPicker {
                    filter, selected, ..
                } = &mut app.modal
                    && apply_input_key(filter, code, modifiers)
                {
                    *selected = 0;
                }
            }
        }
        return;
    }

    if matches!(app.modal, Modal::GhcrPicker { .. }) {
        match code {
            KeyCode::Esc => {
                // The Repos pane already holds the same list, so cancelling
                // leaves somewhere to browse rather than an empty screen.
                app.modal = Modal::None;
                app.set_status("Cancelled");
            }
            KeyCode::Enter => {
                let selected = if let Modal::GhcrPicker { selected, .. } = &app.modal {
                    *selected
                } else {
                    0
                };
                if let Some(repo) = app
                    .ghcr_filtered_packages()
                    .get(selected)
                    .map(|p| (*p).clone())
                {
                    app.modal = Modal::None;
                    // Moving the selection is enough: the event loop reloads
                    // tags whenever the selected repo changes. Sending
                    // `BrowseRepo` as well would fetch the same tags twice,
                    // and `on_tags_page` appends — so the tag list came back
                    // doubled. Fall back to it only when the repo isn't in the
                    // pane to select, which a repo filter can cause.
                    if !app.select_repo_by_name(&repo) {
                        let _ = tx.try_send(AppEvent::BrowseRepo(repo));
                    }
                }
            }
            KeyCode::Up => {
                if let Modal::GhcrPicker { selected, .. } = &mut app.modal {
                    *selected = selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                let n = app.ghcr_filtered_packages().len();
                if let Modal::GhcrPicker { selected, .. } = &mut app.modal
                    && n > 0
                {
                    *selected = (*selected + 1).min(n - 1);
                }
            }
            KeyCode::Char('?') => open_help(app),
            _ => {
                if let Modal::GhcrPicker {
                    filter, selected, ..
                } = &mut app.modal
                    && apply_input_key(filter, code, modifiers)
                {
                    *selected = 0;
                }
            }
        }
        return;
    }

    if matches!(app.modal, Modal::GhcrOwnerPicker { .. }) {
        match code {
            KeyCode::Esc => {
                app.modal = Modal::None;
                app.set_status("Cancelled");
            }
            KeyCode::Enter => {
                let selected = if let Modal::GhcrOwnerPicker { selected, .. } = &app.modal {
                    *selected
                } else {
                    0
                };
                // `ghcr_owner_rows` is the same function the renderer uses, so
                // the row picked here is the row that was highlighted.
                if let Some(owner) = app
                    .ghcr_owner_rows()
                    .get(selected)
                    .map(|c| c.owner().to_owned())
                {
                    let _ = tx.try_send(AppEvent::GhcrOwnerSelected(owner));
                }
            }
            KeyCode::Up => {
                if let Modal::GhcrOwnerPicker { selected, .. } = &mut app.modal {
                    *selected = selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                let n = app.ghcr_owner_rows().len();
                if let Modal::GhcrOwnerPicker { selected, .. } = &mut app.modal
                    && n > 0
                {
                    *selected = (*selected + 1).min(n - 1);
                }
            }
            KeyCode::Char('?') => open_help(app),
            _ => {
                if let Modal::GhcrOwnerPicker {
                    input, selected, ..
                } = &mut app.modal
                    && apply_input_key(input, code, modifiers)
                {
                    // Typing rebuilds the rows (the "Use …" row appears and
                    // disappears), so a stale index would point at the wrong one.
                    *selected = 0;
                }
            }
        }
        return;
    }

    if matches!(app.modal, Modal::Inspect(_)) {
        // `?` opens Help over the viewer. Only when not typing a search query
        // — there `?` is a literal character, same as any other picker's text
        // input.
        let searching = matches!(&app.modal, Modal::Inspect(m) if m.search.active);
        if code == KeyCode::Char('?') && !searching {
            open_help(app);
            return;
        }

        let Modal::Inspect(m) = &mut app.modal else {
            return;
        };

        // Search-entry sub-mode: keys build the query.
        if m.search.active {
            match code {
                KeyCode::Esc => m.cancel_search(),
                KeyCode::Enter => m.commit_search(),
                _ => {
                    apply_input_key(&mut m.search.input, code, modifiers);
                }
            }
            return;
        }

        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.modal = Modal::None;
            }
            KeyCode::Up | KeyCode::Char('k') => m.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => m.move_cursor(1),
            KeyCode::PageUp => m.page(-1),
            KeyCode::PageDown => m.page(1),
            KeyCode::Home | KeyCode::Char('g') => m.jump_top(),
            KeyCode::End | KeyCode::Char('G') => m.jump_bottom(),
            KeyCode::Left | KeyCode::Char('h') => m.set_fold(true),
            KeyCode::Right | KeyCode::Char('l') => m.set_fold(false),
            KeyCode::Char(' ') | KeyCode::Enter => m.toggle_fold(),
            KeyCode::Char('H') => m.collapse_all(),
            KeyCode::Char('L') => m.expand_all(),
            KeyCode::Char('/') => m.start_search(),
            KeyCode::Char('n') => m.next_match(),
            KeyCode::Char('N') => m.prev_match(),
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
            KeyCode::Char('?') => open_help(app),
            _ => {}
        }
        return;
    }

    if matches!(app.modal, Modal::Help { .. }) {
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
                // Restore whatever `?` was pressed over — a picker's typed
                // filter and selection included, not just "which modal".
                app.modal = match app.help_return.take() {
                    Some(m) => *m,
                    None => Modal::None,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Modal::Help { scroll, .. } = &mut app.modal {
                    *scroll = scroll.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Modal::Help { scroll, .. } = &mut app.modal {
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
            KeyCode::Char('?') => open_help(app),
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
        KeyCode::Char('c') => app.copy_pull_url(),
        KeyCode::Char('C') => app.start_copy_image(),
        KeyCode::Char('r') => app.start_retag(),
        KeyCode::Char('R') => app.start_registry_select(),
        KeyCode::Backspace | KeyCode::Char('u') => {
            // Up-navigation, so which level it lands on depends on the
            // registry's hierarchy: Artifactory is repo-key → repo → tag, GHCR
            // is owner → package → tag. Each handler is a no-op for the other
            // registry type.
            //
            // For GHCR this is deliberately *not* the package picker: the Repos
            // pane already holds the whole package list (one fetch fills both),
            // so re-listing packages would add nothing, whereas the owner is
            // otherwise only settable in config.
            let event = if app.profiles[app.active_profile_idx].is_ghcr() {
                AppEvent::OpenGhcrOwnerPicker
            } else {
                AppEvent::OpenArtifactoryRepoPicker
            };
            let _ = tx.try_send(event);
        }
        KeyCode::Char('d') => app.start_delete(),
        KeyCode::Char('i') => handle_inspect(app, client, tx),
        KeyCode::Char('P') => handle_prune(app, client, tx),
        KeyCode::Char('e') => app.start_export(),
        KeyCode::Char('D') => app.start_diff(),
        KeyCode::Char('?') => open_help(app),
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

/// Open Help over whatever is currently on screen.
///
/// Stashes the current modal (including `Modal::None` in normal mode) so
/// closing Help restores it exactly as it was — a picker's typed filter text
/// and selection included, not just "which picker was open". The context
/// shown is derived from that same modal via `help_context_for`, so Help can
/// never show sections for a surface other than the one `?` was pressed on.
fn open_help(app: &mut App) {
    let focus = app.focus;
    let current = std::mem::replace(&mut app.modal, Modal::None);
    let context = help_context_for(&current, focus);
    app.help_return = Some(Box::new(current));
    app.modal = Modal::Help { scroll: 0, context };
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

/// Keys shared by every single-line `InputState` field: readline-style
/// editing (see `docs/text-input-patterns.md`). Returns true if the key was
/// recognized as an edit/navigation key.
fn apply_input_key(input: &mut InputState, code: KeyCode, modifiers: KeyModifiers) -> bool {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Backspace => input.backspace(),
        KeyCode::Delete => input.delete_char(),
        KeyCode::Left if ctrl => input.word_left(),
        KeyCode::Right if ctrl => input.word_right(),
        KeyCode::Left => input.left(),
        KeyCode::Right => input.right(),
        KeyCode::Home => input.home(),
        KeyCode::End => input.end(),
        KeyCode::Char('a') if ctrl => input.home(),
        KeyCode::Char('e') if ctrl => input.end(),
        KeyCode::Char('w') if ctrl => input.delete_word_back(),
        KeyCode::Char('u') if ctrl => input.kill_to_start(),
        KeyCode::Char('k') if ctrl => input.kill_to_end(),
        KeyCode::Char('d') if ctrl => input.delete_char(),
        KeyCode::Char(c) if !ctrl => input.insert(c),
        _ => return false,
    }
    true
}

/// Route a bracketed-paste string to whichever text-entry surface is
/// currently active, mirroring `handle_key`'s modal-priority order.
///
/// `EnableBracketedPaste` is terminal-wide — it cannot be scoped to one
/// modal — so every surface that today accepts characters via
/// `apply_input_key`/`push_filter_char` needs a paste path too, or pasting
/// into it would silently do nothing once bracketed paste is on (worse than
/// today's slow-but-working char-by-char). `Modal::Confirm` and normal mode
/// take no text and are excluded, matching how a stray key is already
/// ignored there.
fn handle_paste(app: &mut App, text: &str, tx: &mpsc::Sender<AppEvent>) {
    if let Modal::Input { input, .. } = &mut app.modal {
        input.insert_str(text);
        return;
    }

    if let Modal::SearchPicker {
        input,
        results,
        selected,
        searching,
        ..
    } = &mut app.modal
    {
        let before = input.buffer.clone();
        input.insert_str(text);
        if input.buffer != before {
            *results = Vec::new();
            *selected = 0;
            if input.buffer.trim().is_empty() {
                *searching = false;
            } else {
                *searching = true;
                spawn_dockerhub_search(input.buffer.clone(), tx.clone());
            }
        }
        return;
    }

    if let Modal::ArtifactoryPicker {
        filter, selected, ..
    } = &mut app.modal
    {
        let before = filter.buffer.clone();
        filter.insert_str(text);
        if filter.buffer != before {
            *selected = 0;
        }
        return;
    }

    if let Modal::Inspect(m) = &mut app.modal
        && m.search.active
    {
        m.search.input.insert_str(text);
        return;
    }

    if app.filter_mode.is_some() {
        app.push_filter_str(text);
    }
}

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
                    password: Secret::new(value),
                });
            }
        }
        InputAction::EnterToken { profile_name } => {
            // Tokens are usually pasted, so strip the quoting, `Bearer `
            // prefix and stray whitespace that a paste tends to bring along.
            let token = sanitize_pasted_token(&value);
            if !token.is_empty() {
                let _ = tx.try_send(AppEvent::TokenEntered {
                    profile_name,
                    token: Secret::new(token),
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

// ------------------------------------------------------------------
// Client factory
// ------------------------------------------------------------------

/// Secrets resolved for a profile, or absent when it needs none / has none
/// stored.
#[derive(Default)]
struct ResolvedSecrets {
    token: Option<String>,
    password: Option<String>,
}

/// Resolve whichever secrets a profile's configured [`AuthMode`] can use.
///
/// Keyring I/O is async-hostile: the `keyring` crate talks to the D-Bus
/// Secret Service, and its fallback forks the `secret-tool` subprocess. Either
/// can block for an unbounded time if the collection is locked (an `Unlock`
/// prompt with no prompter to answer it), so this must not tie up a runtime
/// worker thread — hence `spawn_blocking` rather than calling straight through.
///
/// `profile.name`/`username`/`wants_access_token()` are copied out before the
/// blocking closure so it owns everything it touches (`spawn_blocking`
/// requires `'static`).
async fn resolve_secrets(profile: &RegistryProfile) -> ResolvedSecrets {
    let name = profile.name.clone();
    let username = profile.username.clone();
    let wants_token = profile.wants_access_token();
    // Which variables are legitimate for *this* registry type; the decision is
    // `RegistryProfile::token_env_vars`, so a GHCR profile never reads a JFrog
    // variable and an Artifactory one never reads GITHUB_TOKEN.
    let env_vars = profile.token_env_vars();

    tokio::task::spawn_blocking(move || {
        let store = KeyringStore::new(&name);

        // Only look for a token when the profile could actually use one, so a
        // JFROG_ACCESS_TOKEN exported in a shell cannot change how an
        // unrelated registry is authenticated.
        let token = if wants_token {
            resolve_access_token(&store, env_vars)
        } else {
            None
        };
        let password = username.as_deref().and_then(|u| store.get_password(u));

        ResolvedSecrets { token, password }
    })
    .await
    // A panic inside the closure (not a keyring failure — those are already
    // `None`) degrades to no secrets, same as any other miss: an anonymous
    // client, a 401, and the existing credential prompt.
    .unwrap_or_default()
}

/// Store a secret in the keyring off the runtime worker pool.
///
/// A write can block just as a read can — creating an item in a locked
/// collection prompts for `Unlock` unconditionally, so this is not merely
/// symmetric with [`resolve_secrets`], it is the more likely of the two to
/// hit that case.
async fn store_secret(profile_name: String, account: String, secret: String) {
    let _ = tokio::task::spawn_blocking(move || {
        KeyringStore::new(&profile_name).set_password(&account, &secret)
    })
    .await;
}

/// Build the client for a profile, resolving whichever secrets its configured
/// [`AuthMode`] can use.
///
/// The choice of credential lives in `RegistryProfile::auth_kind`, which is
/// pure and unit-tested against the whole matrix; this function only does the
/// impure part (keyring and environment lookups, via [`resolve_secrets`]) and
/// the construction.
async fn make_client_for_profile(profile: &RegistryProfile) -> RegistryClient {
    let url = match Url::parse(&profile.url) {
        Ok(u) => u,
        // Bad URL costs no keyring I/O — checked before resolving secrets.
        Err(_) => return RegistryClient::new(Url::parse("http://localhost:5000").unwrap()),
    };

    let ResolvedSecrets { token, password } = resolve_secrets(profile).await;

    let creds: Arc<dyn Credentials> = match profile.auth_kind(token.is_some(), password.is_some()) {
        AuthKind::None => return RegistryClient::new(url),
        AuthKind::AccessToken => Arc::new(AccessTokenCredentials::new(
            &url,
            token.unwrap_or_default(),
            profile.username.clone(),
        )),
        // `token` stays an Option: GHCR's exchange works anonymously too, and
        // that is the only way to browse public packages without a PAT.
        AuthKind::Ghcr => Arc::new(GhcrCredentials::new(&url, token)),
        // Artifactory's Docker v2 endpoint and REST API both accept plain HTTP
        // Basic (username + API key / identity token); no Bearer challenge is
        // issued, and `BearerCredentials` sends no header at all without one.
        AuthKind::Basic => Arc::new(BasicCredentials::new(
            profile.username.as_deref().unwrap_or_default(),
            password.as_deref().unwrap_or_default(),
        )),
        AuthKind::Bearer => Arc::new(BearerCredentials::new(
            &url,
            profile.username.clone().unwrap_or_default(),
            password.unwrap_or_default(),
        )),
    };

    RegistryClient::new(url).with_credentials(creds)
}

/// Whether `key` is this profile's client-cache key, either the root entry or
/// an Artifactory repo-key scoped entry (`<profile>#<repo-key>`).
///
/// Config validation rejects `#` in profile names, so the prefix is
/// unambiguous.
fn is_client_key_for(key: &str, profile_name: &str) -> bool {
    key == profile_name
        || key
            .strip_prefix(profile_name)
            .is_some_and(|rest| rest.starts_with('#'))
}

/// Rebuild a profile's root client and every already-scoped
/// `<profile>#<repo-key>` entry derived from it.
///
/// Rebuilding only the root is not enough: scoped clients hold their own
/// `Arc<dyn Credentials>` cloned at the time they were derived, so they would
/// keep using the credential that just failed. Re-deriving them from the fresh
/// root is what makes a newly-entered password or token take effect in the
/// session the user is actually in.
async fn rebuild_clients_for_profile(
    clients: &mut HashMap<String, RegistryClient>,
    profile: &RegistryProfile,
) {
    // Collect first: the map is mutated below.
    let scoped_keys: Vec<String> = clients
        .keys()
        .filter_map(|key| {
            key.strip_prefix(&profile.name)
                .and_then(|rest| rest.strip_prefix('#'))
                .map(str::to_owned)
        })
        .collect();

    let root = make_client_for_profile(profile).await;

    for repo_key in scoped_keys {
        if let Ok(scoped) = root.for_artifactory_repo(&repo_key) {
            clients.insert(format!("{}#{repo_key}", profile.name), scoped);
        }
    }

    clients.insert(profile.name.clone(), root);
}

/// Re-derive a profile's clients after the user entered a new credential, and
/// reload the catalog when the profile on screen is the one that changed.
async fn retry_after_credential_change(
    app: &mut App,
    clients: &mut HashMap<String, RegistryClient>,
    profile_name: &str,
    active_name: &str,
    tx: &mpsc::Sender<AppEvent>,
) {
    let Some(profile) = app
        .profiles
        .iter()
        .find(|p| p.name == profile_name)
        .cloned()
    else {
        return;
    };

    rebuild_clients_for_profile(clients, &profile).await;

    // The active client may be an Artifactory repo-key scoped entry
    // (`<profile>#<repo-key>`), so match on the profile rather than requiring
    // the key to equal the profile name — otherwise re-auth silently does
    // nothing whenever the user is inside a repo-key.
    if !is_client_key_for(active_name, profile_name) {
        return;
    }

    if let Some(client) = clients.get(active_name).cloned() {
        app.catalog_attempt = CatalogAttempt::AfterCredential;
        app.restart_catalog_load();
        spawn_repos_fetch(client, None, tx.clone());
    }
}

/// Whether an error means "your credentials are the problem", so it is worth
/// offering to re-enter them.
///
/// 403 counts: Artifactory answers a valid-but-under-privileged token that way
/// rather than with 401, and treating it as a generic failure left the user
/// looking at "catalog unavailable" with no way to supply a better credential.
fn is_auth_failure(e: &RegistryError) -> bool {
    matches!(
        e,
        RegistryError::Unauthorized | RegistryError::UnexpectedStatus { status: 403, .. }
    )
}

/// Which credential the user should be asked for after an auth failure.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AuthPrompt {
    Password { username: String },
    Token,
}

/// What to prompt for on a 401, or `None` when the profile is anonymous and
/// there is nothing useful to ask.
///
/// Token-authenticated profiles have no username, which the previous
/// username-gated check treated as "nothing to ask" — so they got no prompt at
/// all.
fn auth_prompt_for(profile: &RegistryProfile) -> Option<AuthPrompt> {
    if profile.wants_access_token() && profile.username.is_none() {
        return Some(AuthPrompt::Token);
    }
    if profile.auth == AuthMode::Token {
        return Some(AuthPrompt::Token);
    }
    profile
        .username
        .clone()
        .map(|username| AuthPrompt::Password { username })
}

/// The client to scope from when the user picks an Artifactory repo-key.
/// Always the profile's own (unscoped) client — never a client already
/// scoped to a different repo-key, which would double-append
/// `api/docker/<key>/` onto an already-scoped base (issue #78).
fn artifactory_root_client<'a>(
    clients: &'a HashMap<String, RegistryClient>,
    profile_name: &str,
) -> Option<&'a RegistryClient> {
    clients.get(profile_name)
}

// ------------------------------------------------------------------
// Async task spawners
// ------------------------------------------------------------------

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
                let _ = tx
                    .send(AppEvent::ReposError {
                        msg: e.to_string(),
                        auth_failed: is_auth_failure(&e),
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

fn spawn_artifactory_repos_fetch(client: RegistryClient, tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        match client.artifactory_repositories().await {
            Ok(repos) => {
                let _ = tx.send(AppEvent::ArtifactoryRepos(repos)).await;
            }
            Err(e) => {
                let _ = tx
                    .send(AppEvent::ArtifactoryReposError(e.to_string()))
                    .await;
            }
        }
    });
}

/// Fetch owner suggestions for the GHCR owner picker.
///
/// Cross-host for the same reason as `spawn_ghcr_packages_fetch`, and resolves
/// the token the same way.
///
/// A missing token is reported rather than treated as an error: without one
/// there is nobody to be "the token holder", and `/user/orgs` needs `read:org`
/// on top of the `read:packages` that browsing requires — so an empty
/// suggestion list is an ordinary outcome. The picker stays usable either way,
/// because the typed owner is always selectable.
fn spawn_ghcr_owners_fetch(profile: RegistryProfile, tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        let ResolvedSecrets { token, .. } = resolve_secrets(&profile).await;

        let Some(token) = token else {
            let _ = tx
                .send(AppEvent::GhcrOwnersError("no GitHub token".to_owned()))
                .await;
            return;
        };

        let _ = tx
            .send(AppEvent::GhcrOwners(list_owners(&token).await))
            .await;
    });
}

/// Fetch a GHCR profile's container packages from the GitHub packages API.
///
/// Takes the profile rather than a `RegistryClient` on purpose. The packages
/// API is on `api.github.com`, a different origin from the client's `ghcr.io`
/// base URL, and `RegistryClient::send` deliberately strips `Authorization`
/// from off-origin requests — so the client cannot carry this call, and
/// teaching it to would punch a hole in that guard. The token is therefore
/// re-resolved here, through the same `resolve_secrets` (and so the same
/// `spawn_blocking`) used to build the client.
fn spawn_ghcr_packages_fetch(profile: RegistryProfile, tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        let ResolvedSecrets { token, .. } = resolve_secrets(&profile).await;

        // GitHub exposes no anonymous package listing, even for public
        // packages, so say that plainly instead of sending a request that can
        // only 401.
        let Some(token) = token else {
            let _ = tx
                .send(AppEvent::GhcrPackagesError(
                    "no GitHub token — set $CR_PAT or run with --token \
                     (needs the read:packages scope)"
                        .to_owned(),
                ))
                .await;
            return;
        };

        match list_packages(profile.owner.as_deref(), &token).await {
            Ok(list) => {
                let _ = tx
                    .send(AppEvent::GhcrPackages {
                        packages: list.repos,
                        truncated: list.truncated,
                    })
                    .await;
            }
            Err(e) => {
                let _ = tx.send(AppEvent::GhcrPackagesError(e.to_string())).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RegistryType;

    /// Re-selecting an Artifactory repo-key (e.g. via the picker reopened
    /// mid-browse, issue #78) must always scope from the profile's root
    /// client, never from a client that's already scoped to a *different*
    /// repo-key — otherwise `for_artifactory_repo` double-appends
    /// `api/docker/<key>/` onto an already-scoped base and every request
    /// 404s.
    #[test]
    fn artifactory_root_client_ignores_already_scoped_entries() {
        let root = RegistryClient::new(Url::parse("https://art.example.com/").unwrap());
        let scoped = root.for_artifactory_repo("old-repo").unwrap();
        assert_ne!(root.base_url(), scoped.base_url());

        let mut clients = HashMap::new();
        clients.insert("profileA".to_owned(), root.clone());
        clients.insert("profileA#old-repo".to_owned(), scoped);

        let found = artifactory_root_client(&clients, "profileA").unwrap();
        assert_eq!(found.base_url(), root.base_url());
    }

    // -----------------------------------------------------------------------
    // Client factory
    // -----------------------------------------------------------------------

    /// An anonymous profile, so building a client does no keyring or
    /// environment I/O.
    fn anon_profile(name: &str, url: &str, kind: RegistryType) -> RegistryProfile {
        RegistryProfile {
            name: name.to_owned(),
            url: url.to_owned(),
            username: None,
            registry_type: kind,
            auth: AuthMode::Auto,
            owner: None,
        }
    }

    #[test]
    fn resolve_secrets_returns_nothing_for_anonymous_profile() {
        // No username and not in token mode: must touch no keyring at all,
        // not merely return empty results from one.
        let profile = anon_profile("local", "http://localhost:5000/", RegistryType::Standard);
        let secrets = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(resolve_secrets(&profile));
        assert!(secrets.token.is_none());
        assert!(secrets.password.is_none());
    }

    #[test]
    fn make_client_for_profile_falls_back_on_unparseable_url() {
        let profile = anon_profile("broken", "not-a-url", RegistryType::Standard);
        let client = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(make_client_for_profile(&profile));
        assert_eq!(client.base_url().as_str(), "http://localhost:5000/");
    }

    // -----------------------------------------------------------------------
    // Client-cache keys
    // -----------------------------------------------------------------------

    #[test]
    fn is_client_key_for_matches_root_and_scoped_only() {
        assert!(is_client_key_for("art", "art"));
        assert!(is_client_key_for("art#docker-local", "art"));

        // A different profile that merely shares a prefix must not match, or a
        // credential change would clobber an unrelated registry's client.
        assert!(!is_client_key_for("artifactory", "art"));
        assert!(!is_client_key_for("artifactory#docker-local", "art"));
        assert!(!is_client_key_for("other#docker-local", "art"));
        assert!(!is_client_key_for("", "art"));
    }

    #[test]
    fn rebuild_clients_for_profile_refreshes_scoped_entries() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            // Scoped clients hold their own credentials `Arc`, so a rebuild
            // that only replaced the root would leave them using the stale
            // credential.
            let profile = anon_profile(
                "art",
                "https://art.example.com/artifactory/",
                RegistryType::Artifactory,
            );

            let root = make_client_for_profile(&profile).await;
            let scoped_a = root.for_artifactory_repo("docker-local").unwrap();
            let scoped_b = root.for_artifactory_repo("docker-remote").unwrap();

            let mut clients = HashMap::new();
            clients.insert("art".to_owned(), root.clone());
            clients.insert("art#docker-local".to_owned(), scoped_a.clone());
            clients.insert("art#docker-remote".to_owned(), scoped_b.clone());
            // An unrelated profile must be left alone.
            let other = make_client_for_profile(&anon_profile(
                "other",
                "https://other.example.com/",
                RegistryType::Standard,
            ))
            .await;
            clients.insert("other".to_owned(), other.clone());

            rebuild_clients_for_profile(&mut clients, &profile).await;

            assert_eq!(clients.len(), 4, "no entries added or dropped");
            assert_eq!(clients["art"].base_url(), root.base_url());
            // Re-derived from the fresh root, so still singly-scoped — not
            // double-appended (the issue #78 failure mode).
            assert_eq!(
                clients["art#docker-local"].base_url(),
                scoped_a.base_url(),
                "scoped base URL must be unchanged and un-doubled"
            );
            assert_eq!(clients["art#docker-remote"].base_url(), scoped_b.base_url());
            assert_eq!(clients["other"].base_url(), other.base_url());
        });
    }

    // -----------------------------------------------------------------------
    // Which credential to prompt for
    // -----------------------------------------------------------------------

    #[test]
    fn auth_prompt_for_token_when_auth_is_token() {
        let mut profile = anon_profile("r", "https://r.example.com/", RegistryType::Standard);
        profile.auth = AuthMode::Token;
        assert_eq!(auth_prompt_for(&profile), Some(AuthPrompt::Token));

        // Even with a username configured: token mode does not want a password.
        profile.username = Some("u".to_owned());
        assert_eq!(auth_prompt_for(&profile), Some(AuthPrompt::Token));
    }

    #[test]
    fn auth_prompt_for_token_for_artifactory_without_username() {
        // The case the old username-gated check dropped entirely: no username
        // meant no modal at all, so a token-only profile could never re-auth.
        let profile = anon_profile(
            "art",
            "https://art.example.com/artifactory/",
            RegistryType::Artifactory,
        );
        assert_eq!(auth_prompt_for(&profile), Some(AuthPrompt::Token));
    }

    #[test]
    fn auth_prompt_for_password_when_username_present() {
        let mut profile = anon_profile(
            "art",
            "https://art.example.com/artifactory/",
            RegistryType::Artifactory,
        );
        profile.username = Some("ci".to_owned());
        assert_eq!(
            auth_prompt_for(&profile),
            Some(AuthPrompt::Password {
                username: "ci".to_owned()
            })
        );
    }

    #[test]
    fn auth_prompt_for_none_when_anonymous() {
        // Nothing useful to ask for: no username, and not a token profile.
        let profile = anon_profile("local", "http://localhost:5000/", RegistryType::Standard);
        assert_eq!(auth_prompt_for(&profile), None);
    }

    /// The silent-reread guard in the `ReposError` handler reuses
    /// `auth_prompt_for(&profile).is_some()` to decide whether a profile has
    /// any credential source worth re-reading. An anonymous profile has
    /// none, so re-reading it would be a guaranteed-useless retry — this
    /// pins the exact condition that skips it.
    #[test]
    fn silent_reread_skipped_for_anonymous_profile() {
        let anonymous = anon_profile("local", "http://localhost:5000/", RegistryType::Standard);
        assert!(auth_prompt_for(&anonymous).is_none());

        let with_username = RegistryProfile {
            username: Some("admin".to_owned()),
            ..anon_profile("local", "http://localhost:5000/", RegistryType::Standard)
        };
        assert!(auth_prompt_for(&with_username).is_some());

        let mut token_profile = anon_profile(
            "art",
            "https://art.example.com/artifactory/",
            RegistryType::Artifactory,
        );
        token_profile.auth = AuthMode::Token;
        assert!(auth_prompt_for(&token_profile).is_some());
    }

    #[test]
    fn is_auth_failure_covers_401_and_403() {
        assert!(is_auth_failure(&RegistryError::Unauthorized));
        // Artifactory's answer for a valid but under-privileged token.
        assert!(is_auth_failure(&RegistryError::UnexpectedStatus {
            status: 403,
            url: "https://art.example.com/v2/_catalog".to_owned(),
        }));

        assert!(!is_auth_failure(&RegistryError::UnexpectedStatus {
            status: 500,
            url: "https://art.example.com/v2/_catalog".to_owned(),
        }));
        assert!(!is_auth_failure(&RegistryError::NotFound("x".to_owned())));
        assert!(!is_auth_failure(&RegistryError::InvalidResponse(
            "x".to_owned()
        )));
    }

    // -----------------------------------------------------------------------
    // handle_paste — routes a bracketed paste to whichever surface is active
    // -----------------------------------------------------------------------

    fn app_with_modal(modal: Modal) -> App {
        let profile = anon_profile("local", "http://localhost:5000/", RegistryType::Standard);
        let mut app = App::new(vec![profile], 0);
        app.modal = modal;
        app
    }

    #[test]
    fn handle_paste_routes_to_input_modal() {
        let (tx, _rx) = mpsc::channel(8);
        let mut app = app_with_modal(Modal::Input {
            prompt: "Access token:".to_owned(),
            input: InputState::default(),
            on_confirm: InputAction::EnterToken {
                profile_name: "local".to_owned(),
            },
        });

        handle_paste(&mut app, "a-long-pasted-token", &tx);

        let Modal::Input { input, .. } = &app.modal else {
            panic!("expected Modal::Input");
        };
        assert_eq!(input.buffer, "a-long-pasted-token");
    }

    #[test]
    fn handle_paste_routes_to_search_picker_and_resets_selection() {
        // spawn_dockerhub_search does a real tokio::spawn, so this needs a
        // runtime — same convention used elsewhere in this file (e.g.
        // resolve_secrets_returns_nothing_for_anonymous_profile).
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (tx, _rx) = mpsc::channel(8);
            let mut app = app_with_modal(Modal::SearchPicker {
                input: InputState::default(),
                results: vec!["stale-result".to_owned()],
                selected: 3,
                searching: false,
            });

            handle_paste(&mut app, "nginx", &tx);

            let Modal::SearchPicker {
                input,
                results,
                selected,
                searching,
            } = &app.modal
            else {
                panic!("expected Modal::SearchPicker");
            };
            assert_eq!(input.buffer, "nginx");
            assert!(results.is_empty(), "stale results must be cleared");
            assert_eq!(*selected, 0);
            assert!(*searching);
        });
    }

    // -----------------------------------------------------------------------
    // Contextual help — `?` stashes and restores whatever it was pressed over
    // -----------------------------------------------------------------------

    fn test_client() -> RegistryClient {
        RegistryClient::new(Url::parse("http://localhost:5000").unwrap())
    }

    fn input_with(text: &str) -> InputState {
        let mut input = InputState::default();
        for c in text.chars() {
            input.insert(c);
        }
        input
    }

    /// The whole point of generalising `inspect_return` to `help_return`: a
    /// filter typed into a picker, and the row highlighted, must not be lost
    /// to a help lookup. Losing them would be worse than not having help at
    /// all on this surface.
    #[test]
    fn help_over_a_picker_preserves_its_filter_and_selection() {
        let (tx, _rx) = mpsc::channel(8);
        let client = test_client();
        let mut app = app_with_modal(Modal::ArtifactoryPicker {
            filter: input_with("docker-loc"),
            repos: vec![],
            selected: 2,
            loading: false,
        });

        handle_key(
            &mut app,
            KeyCode::Char('?'),
            KeyModifiers::NONE,
            &client,
            &tx,
        );
        assert!(matches!(
            app.modal,
            Modal::Help {
                context: HelpContext::FilterPicker,
                ..
            }
        ));

        // Close: '?' also closes Help, same as Esc/q.
        handle_key(
            &mut app,
            KeyCode::Char('?'),
            KeyModifiers::NONE,
            &client,
            &tx,
        );

        let Modal::ArtifactoryPicker {
            filter, selected, ..
        } = &app.modal
        else {
            panic!("expected the picker to come back, filter and all");
        };
        assert_eq!(filter.buffer, "docker-loc");
        assert_eq!(*selected, 2);
    }

    /// Normal mode has no modal to stash — `Modal::None` — and that has to
    /// round-trip cleanly too, not leave the app stuck showing Help.
    #[test]
    fn help_over_normal_mode_returns_to_normal_mode() {
        let (tx, _rx) = mpsc::channel(8);
        let client = test_client();
        let mut app = app_with_modal(Modal::None);
        app.focus = Focus::Tags;

        handle_key(
            &mut app,
            KeyCode::Char('?'),
            KeyModifiers::NONE,
            &client,
            &tx,
        );
        assert!(matches!(
            app.modal,
            Modal::Help {
                context: HelpContext::Normal(Focus::Tags),
                ..
            }
        ));

        handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, &client, &tx);
        assert!(matches!(app.modal, Modal::None));
    }

    /// `?` is a legitimate character in a text prompt (it cannot be reserved
    /// there the way it is in pickers, since a value could need one), so it
    /// must type a literal `?` instead of opening Help.
    #[test]
    fn help_key_is_a_literal_character_inside_input_modal() {
        let (tx, _rx) = mpsc::channel(8);
        let client = test_client();
        let mut app = app_with_modal(Modal::Input {
            prompt: "New tag:".to_owned(),
            input: InputState::default(),
            on_confirm: InputAction::BrowseRepo,
        });

        handle_key(
            &mut app,
            KeyCode::Char('?'),
            KeyModifiers::NONE,
            &client,
            &tx,
        );

        let Modal::Input { input, .. } = &app.modal else {
            panic!("expected Modal::Input to stay open, not Help");
        };
        assert_eq!(input.buffer, "?");
    }

    /// Mirrors the pre-existing Inspect-search exception, generalised: `?`
    /// must still be typeable into the Inspect JSON search query.
    #[test]
    fn help_key_is_a_literal_character_while_searching_inspect() {
        let (tx, _rx) = mpsc::channel(8);
        let client = test_client();
        let mut modal = InspectModal::new("img:tag".to_owned(), vec!["{}".to_owned()]);
        modal.start_search();
        let mut app = app_with_modal(Modal::Inspect(Box::new(modal)));

        handle_key(
            &mut app,
            KeyCode::Char('?'),
            KeyModifiers::NONE,
            &client,
            &tx,
        );

        let Modal::Inspect(m) = &app.modal else {
            panic!("expected Modal::Inspect to stay open, not Help");
        };
        assert_eq!(m.search.input.buffer, "?");
    }

    #[test]
    fn handle_paste_routes_to_artifactory_filter() {
        let (tx, _rx) = mpsc::channel(8);
        let mut app = app_with_modal(Modal::ArtifactoryPicker {
            filter: InputState::default(),
            repos: Vec::new(),
            selected: 5,
            loading: false,
        });

        handle_paste(&mut app, "docker-local", &tx);

        let Modal::ArtifactoryPicker {
            filter, selected, ..
        } = &app.modal
        else {
            panic!("expected Modal::ArtifactoryPicker");
        };
        assert_eq!(filter.buffer, "docker-local");
        assert_eq!(*selected, 0, "selection must reset on a changed filter");
    }

    #[test]
    fn handle_paste_routes_to_inspect_search_when_active() {
        let (tx, _rx) = mpsc::channel(8);
        let mut inspect = InspectModal::new("img:tag".to_owned(), vec!["{}".to_owned()]);
        inspect.start_search();
        let mut app = app_with_modal(Modal::Inspect(Box::new(inspect)));

        handle_paste(&mut app, "sha256:abc", &tx);

        let Modal::Inspect(m) = &app.modal else {
            panic!("expected Modal::Inspect");
        };
        assert_eq!(m.search.input.buffer, "sha256:abc");
    }

    #[test]
    fn handle_paste_routes_to_list_filter_when_filter_mode_active() {
        let (tx, _rx) = mpsc::channel(8);
        let mut app = app_with_modal(Modal::None);
        app.repos = vec!["nginx".to_owned(), "budgeteer".to_owned()];
        app.filter_mode = Some(Focus::Repos);

        handle_paste(&mut app, "ngi", &tx);

        assert_eq!(app.repo_filter, "ngi");
    }

    #[test]
    fn handle_paste_is_a_noop_in_normal_mode_and_confirm_modal() {
        let (tx, _rx) = mpsc::channel(8);

        let mut normal = app_with_modal(Modal::None);
        handle_paste(&mut normal, "ignored", &tx);
        assert_eq!(normal.repo_filter, "");
        assert!(matches!(normal.modal, Modal::None));

        let mut confirming = app_with_modal(Modal::Confirm {
            message: "Delete?".to_owned(),
            on_confirm: ConfirmAction::DeleteManifest {
                repo: "r".to_owned(),
                tag: "t".to_owned(),
            },
        });
        handle_paste(&mut confirming, "ignored", &tx);
        assert!(matches!(confirming.modal, Modal::Confirm { .. }));
    }
}
