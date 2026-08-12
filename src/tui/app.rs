use std::collections::HashMap;
use std::time::{Duration, Instant};

use ratatui::widgets::ListState;

use crate::clipboard;
use crate::config::RegistryProfile;
use crate::ops::diff::DiffLayer;
use crate::registry::ArtifactoryRepo;

use super::detail::ImageDetail;
use super::input::InputState;
use super::jsonview::{self, RowMeta};

const STATUS_TTL: Duration = Duration::from_secs(2);
const LOAD_AHEAD: usize = 20;
pub const SPINNER: [char; 6] = ['⠋', '⠙', '⠸', '⠴', '⠦', '⠇'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Repos,
    Tags,
    Detail,
}

impl Focus {
    pub fn toggle(self) -> Self {
        match self {
            Focus::Repos => Focus::Tags,
            Focus::Tags => Focus::Detail,
            Focus::Detail => Focus::Repos,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Focus::Repos => Focus::Detail,
            Focus::Tags => Focus::Repos,
            Focus::Detail => Focus::Tags,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    Idle,
    Loading,
    Error(String),
}

/// Which retry, if any, produced the in-flight catalog load. See the field
/// doc on `App::catalog_attempt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CatalogAttempt {
    /// First load for this registry (or repo-key).
    #[default]
    Initial,
    /// Retry after silently re-reading credentials from the keyring.
    AfterReread,
    /// Retry after the user supplied a credential at the prompt.
    AfterCredential,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    NameAsc,
    NameDesc,
}

impl SortOrder {
    pub fn cycle(self) -> Self {
        match self {
            Self::NameAsc => Self::NameDesc,
            Self::NameDesc => Self::NameAsc,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::NameAsc => "↑ name",
            Self::NameDesc => "↓ name",
        }
    }
}

/// In-modal text search over the inspect JSON.
#[derive(Debug, Default)]
pub struct SearchState {
    /// True while the user is typing a query (keys route to `input`).
    pub active: bool,
    pub input: InputState,
    /// Absolute line indices of the last committed query's matches.
    pub matches: Vec<usize>,
    /// Index into `matches` of the currently-focused hit.
    pub current: usize,
    /// The committed query text (for highlighting), empty when none.
    pub query: String,
}

/// The Inspect modal: a navigable viewer over pretty-printed manifest +
/// config JSON with collapsible nodes, paging, and text search.
///
/// `cursor` and `scroll` index into `visible` (the currently-shown subset
/// of line indices), not into `lines` directly. `viewport_h` is written by
/// the renderer each frame so paging can step by a screenful.
#[derive(Debug)]
pub struct InspectModal {
    pub title: String,
    pub lines: Vec<String>,
    pub rows: Vec<RowMeta>,
    pub collapsed: Vec<bool>,
    pub visible: Vec<usize>,
    pub cursor: usize,
    pub scroll: usize,
    pub viewport_h: usize,
    pub search: SearchState,
}

impl InspectModal {
    pub fn new(title: String, lines: Vec<String>) -> Self {
        let rows = jsonview::build_rows(&lines);
        let collapsed = vec![false; lines.len()];
        let visible = jsonview::visible_lines(&rows, &collapsed);
        Self {
            title,
            lines,
            rows,
            collapsed,
            visible,
            cursor: 0,
            scroll: 0,
            viewport_h: 1,
            search: SearchState::default(),
        }
    }

    /// Absolute line index under the cursor.
    pub fn cursor_line(&self) -> usize {
        self.visible.get(self.cursor).copied().unwrap_or(0)
    }

    /// Recompute the visible set after a fold change, keeping the cursor on
    /// the same absolute line where possible.
    fn rebuild_visible(&mut self) {
        let anchor = self.cursor_line();
        self.visible = jsonview::visible_lines(&self.rows, &self.collapsed);
        self.cursor = self
            .visible
            .iter()
            .position(|&l| l >= anchor)
            .unwrap_or(self.visible.len().saturating_sub(1));
        self.ensure_visible();
    }

    /// Scroll so the cursor row sits within the viewport.
    fn ensure_visible(&mut self) {
        let h = self.viewport_h.max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + h {
            self.scroll = self.cursor + 1 - h;
        }
        let max_scroll = self.visible.len().saturating_sub(h);
        self.scroll = self.scroll.min(max_scroll);
    }

    /// Record the rendered content height and re-clamp scroll to it.
    pub fn set_viewport(&mut self, h: usize) {
        self.viewport_h = h.max(1);
        self.ensure_visible();
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.visible.len().saturating_sub(1);
        let next = (self.cursor as isize + delta).clamp(0, last as isize);
        self.cursor = next as usize;
        self.ensure_visible();
    }

    pub fn page(&mut self, pages: isize) {
        let step = self.viewport_h.max(1) as isize;
        self.move_cursor(pages * step);
    }

    pub fn jump_top(&mut self) {
        self.cursor = 0;
        self.ensure_visible();
    }

    pub fn jump_bottom(&mut self) {
        self.cursor = self.visible.len().saturating_sub(1);
        self.ensure_visible();
    }

    /// Toggle the fold of the node at (or enclosing) the cursor.
    pub fn toggle_fold(&mut self) {
        if let Some(o) = jsonview::opener_at(&self.rows, self.cursor_line()) {
            self.collapsed[o] = !self.collapsed[o];
            // Keep the cursor on the opener so repeated toggles are stable.
            self.rebuild_visible();
            if let Some(pos) = self.visible.iter().position(|&l| l == o) {
                self.cursor = pos;
                self.ensure_visible();
            }
        }
    }

    /// Collapse (`true`) or expand (`false`) the node at the cursor.
    pub fn set_fold(&mut self, collapse: bool) {
        if let Some(o) = jsonview::opener_at(&self.rows, self.cursor_line())
            && self.collapsed[o] != collapse
        {
            self.collapsed[o] = collapse;
            self.rebuild_visible();
            if let Some(pos) = self.visible.iter().position(|&l| l == o) {
                self.cursor = pos;
                self.ensure_visible();
            }
        }
    }

    pub fn collapse_all(&mut self) {
        for (i, r) in self.rows.iter().enumerate() {
            if r.opener {
                self.collapsed[i] = true;
            }
        }
        self.rebuild_visible();
    }

    pub fn expand_all(&mut self) {
        for c in &mut self.collapsed {
            *c = false;
        }
        self.rebuild_visible();
    }

    pub fn start_search(&mut self) {
        self.search.active = true;
        self.search.input.start("");
    }

    pub fn cancel_search(&mut self) {
        self.search.active = false;
    }

    /// Commit the typed query: compute matches and jump to the first hit.
    pub fn commit_search(&mut self) {
        self.search.active = false;
        let q = self.search.input.buffer.clone();
        self.search.matches = jsonview::find_matches(&self.lines, &q);
        self.search.query = q;
        self.search.current = 0;
        if let Some(&line) = self.search.matches.first() {
            self.jump_to(line);
        }
    }

    pub fn next_match(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        self.search.current = (self.search.current + 1) % self.search.matches.len();
        self.jump_to(self.search.matches[self.search.current]);
    }

    pub fn prev_match(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        let n = self.search.matches.len();
        self.search.current = (self.search.current + n - 1) % n;
        self.jump_to(self.search.matches[self.search.current]);
    }

    /// Move the cursor to an absolute line, expanding any folds hiding it.
    fn jump_to(&mut self, line: usize) {
        jsonview::expand_ancestors(&self.rows, &mut self.collapsed, line);
        self.visible = jsonview::visible_lines(&self.rows, &self.collapsed);
        if let Some(pos) = self.visible.iter().position(|&l| l == line) {
            self.cursor = pos;
        }
        self.ensure_visible();
    }

    pub fn has_matches(&self) -> bool {
        !self.search.matches.is_empty()
    }
}

#[derive(Debug)]
pub struct LayerDiffModal {
    pub tag_a: String,
    pub tag_b: String,
    pub layers: Vec<DiffLayer>,
    pub scroll: usize,
}

#[derive(Debug)]
pub enum Modal {
    None,
    Confirm {
        message: String,
        on_confirm: ConfirmAction,
    },
    Input {
        prompt: String,
        input: InputState,
        on_confirm: InputAction,
    },
    RegistrySelect {
        selected_idx: usize,
    },
    Inspect(Box<InspectModal>),
    LayerDiff(Box<LayerDiffModal>),
    Help {
        scroll: usize,
    },
    /// Docker Hub repository search with live results.
    SearchPicker {
        input: InputState,
        results: Vec<String>,
        selected: usize,
        searching: bool,
    },
    /// Pick which Docker repo-key to browse on a JFrog Artifactory
    /// instance. `repos` is fetched once (via `/api/repositories`) and
    /// filtered locally as the user types, unlike `SearchPicker`'s
    /// incremental server-side search.
    ArtifactoryPicker {
        filter: InputState,
        repos: Vec<ArtifactoryRepo>,
        selected: usize,
        loading: bool,
    },
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteManifest { repo: String, tag: String },
    PruneDigestTags { repo: String, tags: Vec<String> },
}

#[derive(Debug, Clone)]
pub enum InputAction {
    CopyImage {
        src_repo: String,
        src_tag: String,
    },
    Retag {
        repo: String,
        src_tag: String,
    },
    Export {
        repo: String,
        tag: String,
    },
    DiffAgainst {
        repo: String,
        tag_a: String,
    },
    /// User typed a repo name directly (e.g. after catalog failure).
    BrowseRepo,
    /// User entered a password after auth failure.
    EnterPassword {
        profile_name: String,
        username: String,
    },
    /// User entered an access token after auth failure. Carries no username —
    /// token auth does not need one.
    EnterToken {
        profile_name: String,
    },
}

impl InputAction {
    /// Whether this action collects a credential, so the input modal must
    /// mask what it echoes.
    ///
    /// Deriving masking from the action (rather than a flag on `Modal::Input`)
    /// means none of the modal's construction sites can forget to set it, and
    /// a new credential-collecting action only has to be listed here.
    pub fn is_secret(&self) -> bool {
        matches!(self, Self::EnterPassword { .. } | Self::EnterToken { .. })
    }
}

#[derive(Debug)]
struct StatusMessage {
    text: String,
    expires_at: Instant,
}

pub struct App {
    pub focus: Focus,
    pub filter_mode: Option<Focus>,
    // Displayed (filtered/sorted) lists
    pub repos: Vec<String>,
    pub repos_state: ListState,
    pub tags: Vec<String>,
    pub tags_state: ListState,
    // Raw loaded data
    repos_all: Vec<String>,
    tags_all: Vec<String>,
    // Filters
    pub repo_filter: String,
    pub tag_filter: String,
    // Pagination
    pub repos_cursor: Option<String>,
    pub repos_has_more: bool,
    pub tags_cursor: Option<String>,
    pub tags_has_more: bool,
    pub current_repo: Option<String>,
    // Load state
    pub repo_load: LoadState,
    pub tag_load: LoadState,
    pub detail_load: LoadState,
    // Sort
    pub tag_sort: SortOrder,
    // Detail panel
    pub detail: Option<ImageDetail>,
    pub detail_scroll: usize,
    pub current_tag: Option<String>,
    // Display
    pub registry_name: String,
    pub registry_url: String,
    pub modal: Modal,
    /// The Inspect modal stashed while the Help overlay is shown over it, so
    /// closing Help returns to the JSON viewer where it left off.
    pub inspect_return: Option<Box<InspectModal>>,
    pub should_quit: bool,
    pub spinner_tick: usize,
    /// Which retry, if any, produced the catalog load currently in flight.
    ///
    /// A 401 on `Initial` means the cached client's credentials might simply
    /// predate something now in the keyring (another process wrote it, or it
    /// was unlocked after startup) — worth one silent re-read before bothering
    /// the user. A 401 on `AfterReread` or `AfterCredential` means that
    /// re-read (or a prompt) already ran and failed, so it opens BrowseRepo /
    /// the credential prompt instead of retrying again.
    pub catalog_attempt: CatalogAttempt,
    status: Option<StatusMessage>,
    // Registry switcher
    pub profiles: Vec<RegistryProfile>,
    pub active_profile_idx: usize,
    /// Last-fetched repo-key list per Artifactory profile (by profile name),
    /// so re-opening the picker via up-navigation doesn't wait on a fetch.
    artifactory_repo_cache: HashMap<String, Vec<ArtifactoryRepo>>,
    /// Repo-key currently being browsed, when the active registry is an
    /// Artifactory repo-key. Used to preselect it in the picker.
    current_artifactory_repo_key: Option<String>,
}

impl App {
    pub fn new(profiles: Vec<RegistryProfile>, initial_idx: usize) -> Self {
        let mut repos_state = ListState::default();
        repos_state.select(Some(0));
        let idx = initial_idx.min(profiles.len().saturating_sub(1));
        let registry_name = profiles
            .get(idx)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let registry_url = profiles.get(idx).map(|p| p.url.clone()).unwrap_or_default();
        Self {
            focus: Focus::Repos,
            filter_mode: None,
            repos: Vec::new(),
            repos_state,
            tags: Vec::new(),
            tags_state: ListState::default(),
            repos_all: Vec::new(),
            tags_all: Vec::new(),
            repo_filter: String::new(),
            tag_filter: String::new(),
            repos_cursor: None,
            repos_has_more: false,
            tags_cursor: None,
            tags_has_more: false,
            current_repo: None,
            repo_load: LoadState::Idle,
            tag_load: LoadState::Idle,
            detail_load: LoadState::Idle,
            tag_sort: SortOrder::NameAsc,
            detail: None,
            detail_scroll: 0,
            current_tag: None,
            registry_name,
            registry_url,
            modal: Modal::None,
            inspect_return: None,
            should_quit: false,
            spinner_tick: 0,
            catalog_attempt: CatalogAttempt::Initial,
            status: None,
            profiles,
            active_profile_idx: idx,
            artifactory_repo_cache: HashMap::new(),
            current_artifactory_repo_key: None,
        }
    }

    // ------------------------------------------------------------------
    // Page arrival
    // ------------------------------------------------------------------

    pub fn on_repos_page(&mut self, repos: Vec<String>, has_more: bool) {
        self.repos_has_more = has_more;
        self.repos_all.extend(repos);
        self.repos_cursor = self.repos_all.last().cloned();
        self.repo_load = LoadState::Idle;
        self.apply_repo_filter();
    }

    pub fn on_repos_error(&mut self, msg: String, show_browse: bool) {
        let is_dh = self
            .profiles
            .get(self.active_profile_idx)
            .is_some_and(|p| p.is_dockerhub());
        let display = if is_dh {
            "We can't show you these, please search for an image".to_owned()
        } else {
            msg.clone()
        };
        self.repo_load = LoadState::Error(display);
        self.set_status(format!("Repos error: {msg}"));
        if show_browse && matches!(self.modal, Modal::None) {
            if is_dh {
                self.modal = Modal::SearchPicker {
                    input: InputState::default(),
                    results: Vec::new(),
                    selected: 0,
                    searching: false,
                };
            } else {
                self.modal = Modal::Input {
                    prompt: "Catalog unavailable. Enter repo name to browse:".to_owned(),
                    input: InputState::default(),
                    on_confirm: InputAction::BrowseRepo,
                };
            }
        }
    }

    pub fn on_tags_page(&mut self, repo: String, tags: Vec<String>, has_more: bool) {
        if self.current_repo.as_deref() != Some(&repo) {
            return;
        }
        let was_empty = self.tags_all.is_empty();
        self.tags_has_more = has_more;
        self.tags_all.extend(tags);
        self.tags_cursor = self.tags_all.last().cloned();
        self.tag_load = LoadState::Idle;
        self.apply_tag_filter_sort();
        if was_empty && !self.tags.is_empty() {
            self.tags_state.select(Some(0));
        }
    }

    pub fn on_tags_error(&mut self, msg: String) {
        self.tag_load = LoadState::Error(msg.clone());
        self.set_status(format!("Tags error: {msg}"));
    }

    // ------------------------------------------------------------------
    // Tag loading lifecycle
    // ------------------------------------------------------------------

    pub fn start_detail_load(&mut self, tag: String) {
        self.current_tag = Some(tag);
        self.detail = None;
        self.detail_scroll = 0;
        self.detail_load = LoadState::Loading;
    }

    pub fn on_detail_loaded(&mut self, repo: String, tag: String, detail: ImageDetail) {
        if self.current_repo.as_deref() == Some(&repo) && self.current_tag.as_deref() == Some(&tag)
        {
            self.detail = Some(detail);
            self.detail_load = LoadState::Idle;
        }
    }

    pub fn on_detail_error(&mut self, msg: String) {
        self.detail_load = LoadState::Error(msg.clone());
        self.set_status(format!("Detail error: {msg}"));
    }

    pub fn start_tags_load(&mut self, repo: String) {
        self.current_repo = Some(repo);
        self.tags_all.clear();
        self.tags.clear();
        self.tags_state.select(None);
        self.tags_cursor = None;
        self.tags_has_more = false;
        self.tag_filter.clear();
        self.tag_load = LoadState::Loading;
        self.detail = None;
        self.detail_load = LoadState::Idle;
        self.current_tag = None;
    }

    /// Clear all repo/tag/detail state, keeping neither list nor selection
    /// from the previous registry (or Artifactory repo-key).
    fn reset_for_new_registry(&mut self) {
        self.repos_all.clear();
        self.repos.clear();
        self.repos_state.select(Some(0));
        self.repos_cursor = None;
        self.repos_has_more = false;
        self.repo_filter.clear();

        self.tags_all.clear();
        self.tags.clear();
        self.tags_state.select(None);
        self.tags_cursor = None;
        self.tags_has_more = false;
        self.tag_filter.clear();
        self.tag_load = LoadState::Idle;

        self.current_repo = None;
        self.current_tag = None;
        self.detail = None;
        self.detail_load = LoadState::Idle;
        self.detail_scroll = 0;
        self.focus = Focus::Repos;
        self.filter_mode = None;
        self.catalog_attempt = CatalogAttempt::Initial;
        self.current_artifactory_repo_key = None;
    }

    /// Clear all repo/tag/detail state when switching registries.
    pub fn start_registry_switch(&mut self, idx: usize) {
        self.active_profile_idx = idx;
        let profile = &self.profiles[idx];
        self.registry_name = profile.name.clone();
        self.registry_url = profile.url.clone();
        self.reset_for_new_registry();
        self.repo_load = LoadState::Loading;
    }

    /// Reload the catalog in place after a credential change (a silent
    /// keyring re-read or a prompt).
    ///
    /// Deliberately narrower than [`Self::start_registry_switch`], which goes
    /// through `reset_for_new_registry` and so clears
    /// `current_artifactory_repo_key`. Using that here would visually eject the
    /// user back to the repo-key picker on a *successful* re-auth, while the
    /// client stayed scoped to the repo-key they were browsing.
    ///
    /// Does not touch `catalog_attempt` — the caller sets it to whichever
    /// retry stage this reload is for, since `restart_catalog_load` itself
    /// has no way to know which one that is.
    pub fn restart_catalog_load(&mut self) {
        self.repos_all.clear();
        self.repos.clear();
        self.repos_state.select(Some(0));
        self.repos_cursor = None;
        self.repos_has_more = false;
        self.repo_filter.clear();

        self.tags_all.clear();
        self.tags.clear();
        self.tags_state.select(None);
        self.tags_cursor = None;
        self.tags_has_more = false;
        self.tag_filter.clear();
        self.tag_load = LoadState::Idle;

        self.current_repo = None;
        self.current_tag = None;
        self.detail = None;
        self.detail_load = LoadState::Idle;
        self.detail_scroll = 0;
        self.focus = Focus::Repos;
        self.filter_mode = None;

        self.repo_load = LoadState::Loading;
    }

    /// Switch to a JFrog Artifactory profile: clear repo/tag/detail state
    /// and open the repo-key picker instead of fetching a catalog directly
    /// (an Artifactory instance's base URL isn't itself a `/v2/` root).
    pub fn start_artifactory_switch(&mut self, idx: usize) {
        self.active_profile_idx = idx;
        let profile = &self.profiles[idx];
        self.registry_name = profile.name.clone();
        self.registry_url = profile.url.clone();
        self.reset_for_new_registry();
        self.repo_load = LoadState::Idle;
        self.modal = Modal::ArtifactoryPicker {
            filter: InputState::default(),
            repos: Vec::new(),
            selected: 0,
            loading: true,
        };
    }

    /// Fill in the repo-key list once `/api/repositories` returns. Also
    /// refreshes the profile's cache, so the next up-navigation open has
    /// this list ready instantly.
    pub fn on_artifactory_repos(&mut self, repos: Vec<ArtifactoryRepo>) {
        let profile_name = self.profiles[self.active_profile_idx].name.clone();
        self.artifactory_repo_cache
            .insert(profile_name, repos.clone());

        let current_key = self.current_artifactory_repo_key.clone();
        if let Modal::ArtifactoryPicker {
            repos: r,
            selected,
            loading,
            ..
        } = &mut self.modal
        {
            *r = repos;
            *loading = false;
            let len = r.len();
            *selected = if len == 0 {
                0
            } else if let Some(idx) = current_key
                .as_ref()
                .and_then(|key| r.iter().position(|repo| &repo.key == key))
            {
                idx
            } else {
                (*selected).min(len - 1)
            };
        }
    }

    /// Re-open the repo-key picker for the active Artifactory profile using
    /// the cached list from the last fetch, so it appears instantly instead
    /// of waiting on a network round-trip. The caller is expected to spawn a
    /// background refetch (see `on_artifactory_repos`) to keep it current.
    ///
    /// Unlike `start_artifactory_switch`, this does not touch repo/tag/detail
    /// state — `Esc` from the picker returns to browsing exactly as it was.
    pub fn open_artifactory_picker_cached(&mut self) {
        let profile_name = &self.profiles[self.active_profile_idx].name;
        let cached = self
            .artifactory_repo_cache
            .get(profile_name)
            .cloned()
            .unwrap_or_default();
        let selected = self
            .current_artifactory_repo_key
            .as_ref()
            .and_then(|key| cached.iter().position(|r| &r.key == key))
            .unwrap_or(0);
        self.modal = Modal::ArtifactoryPicker {
            filter: InputState::default(),
            repos: cached,
            selected,
            loading: true,
        };
    }

    pub fn on_artifactory_repos_error(&mut self, msg: String) {
        if let Modal::ArtifactoryPicker { loading, .. } = &mut self.modal {
            *loading = false;
        }
        self.set_status(format!("Artifactory repos error: {msg}"));
    }

    /// The picker's repo list filtered by its current filter buffer
    /// (substring match on repo-key, case-insensitive).
    pub fn artifactory_filtered_repos(&self) -> Vec<&ArtifactoryRepo> {
        let Modal::ArtifactoryPicker { filter, repos, .. } = &self.modal else {
            return Vec::new();
        };
        let f = filter.buffer.to_lowercase();
        if f.is_empty() {
            repos.iter().collect()
        } else {
            repos
                .iter()
                .filter(|r| r.key.to_lowercase().contains(&f))
                .collect()
        }
    }

    /// Descend into a chosen Artifactory repo-key: from here on it behaves
    /// exactly like any other registry (`scoped_url` is its `/v2/` root's
    /// non-`/v2/` base, used for display and pull-URL construction).
    pub fn enter_artifactory_repo(&mut self, repo_key: &str, scoped_url: String) {
        let profile_name = self.profiles[self.active_profile_idx].name.clone();
        self.registry_name = format!("{profile_name}/{repo_key}");
        self.registry_url = scoped_url;
        self.modal = Modal::None;
        self.reset_for_new_registry();
        self.current_artifactory_repo_key = Some(repo_key.to_owned());
        self.repo_load = LoadState::Loading;
    }

    // ------------------------------------------------------------------
    // Pagination hints
    // ------------------------------------------------------------------

    pub fn should_load_more_repos(&self) -> bool {
        if !self.repos_has_more || self.repo_load != LoadState::Idle {
            return false;
        }
        let selected = self.repos_state.selected().unwrap_or(0);
        selected + LOAD_AHEAD >= self.repos.len()
    }

    pub fn should_load_more_tags(&self) -> bool {
        if !self.tags_has_more || self.tag_load != LoadState::Idle {
            return false;
        }
        let selected = self.tags_state.selected().unwrap_or(0);
        selected + LOAD_AHEAD >= self.tags.len()
    }

    // ------------------------------------------------------------------
    // Filters
    // ------------------------------------------------------------------

    pub fn push_filter_char(&mut self, ch: char) {
        match self.filter_mode {
            Some(Focus::Repos) => {
                self.repo_filter.push(ch);
                self.apply_repo_filter();
            }
            Some(Focus::Tags) => {
                self.tag_filter.push(ch);
                self.apply_tag_filter_sort();
            }
            Some(Focus::Detail) | None => {}
        }
    }

    pub fn pop_filter_char(&mut self) {
        match self.filter_mode {
            Some(Focus::Repos) => {
                self.repo_filter.pop();
                self.apply_repo_filter();
            }
            Some(Focus::Tags) => {
                self.tag_filter.pop();
                self.apply_tag_filter_sort();
            }
            Some(Focus::Detail) | None => {}
        }
    }

    pub fn clear_active_filter(&mut self) {
        match self.filter_mode {
            Some(Focus::Repos) => {
                self.repo_filter.clear();
                self.apply_repo_filter();
            }
            Some(Focus::Tags) => {
                self.tag_filter.clear();
                self.apply_tag_filter_sort();
            }
            Some(Focus::Detail) | None => {}
        }
        self.filter_mode = None;
    }

    fn apply_repo_filter(&mut self) {
        let filter = self.repo_filter.to_lowercase();
        self.repos = if filter.is_empty() {
            self.repos_all.clone()
        } else {
            self.repos_all
                .iter()
                .filter(|r| r.to_lowercase().contains(&filter))
                .cloned()
                .collect()
        };
        self.clamp_repo_selection();
    }

    fn apply_tag_filter_sort(&mut self) {
        let filter = self.tag_filter.to_lowercase();
        let mut filtered: Vec<String> = if filter.is_empty() {
            self.tags_all.clone()
        } else {
            self.tags_all
                .iter()
                .filter(|t| t.to_lowercase().contains(&filter))
                .cloned()
                .collect()
        };
        match self.tag_sort {
            SortOrder::NameAsc => filtered.sort(),
            SortOrder::NameDesc => {
                filtered.sort();
                filtered.reverse();
            }
        }
        self.tags = filtered;
        self.clamp_tag_selection();
    }

    fn clamp_repo_selection(&mut self) {
        let len = self.repos.len();
        if len == 0 {
            self.repos_state.select(None);
        } else {
            let i = self.repos_state.selected().unwrap_or(0).min(len - 1);
            self.repos_state.select(Some(i));
        }
    }

    fn clamp_tag_selection(&mut self) {
        let len = self.tags.len();
        if len == 0 {
            self.tags_state.select(None);
        } else if self.tags_state.selected().is_none() {
            self.tags_state.select(Some(0));
        } else {
            let i = self.tags_state.selected().unwrap_or(0).min(len - 1);
            self.tags_state.select(Some(i));
        }
    }

    // ------------------------------------------------------------------
    // Navigation
    // ------------------------------------------------------------------

    pub fn scroll_up(&mut self) {
        match self.focus {
            Focus::Repos => {
                let i = self.repos_state.selected().unwrap_or(0);
                if i > 0 {
                    self.repos_state.select(Some(i - 1));
                    self.tags.clear();
                    self.tags_state.select(None);
                }
            }
            Focus::Tags => {
                let i = self.tags_state.selected().unwrap_or(0);
                if i > 0 {
                    self.tags_state.select(Some(i - 1));
                }
            }
            Focus::Detail => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
            }
        }
    }

    pub fn scroll_down(&mut self) {
        match self.focus {
            Focus::Repos => {
                let len = self.repos.len();
                if len == 0 {
                    return;
                }
                let i = self.repos_state.selected().unwrap_or(0);
                if i + 1 < len {
                    self.repos_state.select(Some(i + 1));
                    self.tags.clear();
                    self.tags_state.select(None);
                }
            }
            Focus::Tags => {
                let len = self.tags.len();
                if len == 0 {
                    return;
                }
                let i = self.tags_state.selected().unwrap_or(0);
                if i + 1 < len {
                    self.tags_state.select(Some(i + 1));
                }
            }
            Focus::Detail => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
            }
        }
    }

    pub fn selected_repo(&self) -> Option<&str> {
        self.repos_state
            .selected()
            .and_then(|i| self.repos.get(i))
            .map(String::as_str)
    }

    pub fn selected_tag(&self) -> Option<&str> {
        self.tags_state
            .selected()
            .and_then(|i| self.tags.get(i))
            .map(String::as_str)
    }

    // ------------------------------------------------------------------
    // Status
    // ------------------------------------------------------------------

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(StatusMessage {
            text: msg.into(),
            expires_at: Instant::now() + STATUS_TTL,
        });
    }

    pub fn status_text(&self) -> Option<&str> {
        self.status.as_ref().map(|s| s.text.as_str())
    }

    pub fn on_delete_success(&mut self, repo: &str, tag: &str) {
        self.tags_all.retain(|t| t != tag);
        self.apply_tag_filter_sort();
        if self.current_tag.as_deref() == Some(tag) {
            self.detail = None;
            self.detail_load = LoadState::Idle;
            self.current_tag = None;
        }
        self.set_status(format!("✓ Deleted {repo}:{tag}"));
    }

    pub fn on_delete_error(&mut self, msg: String) {
        self.set_status(format!("✗ Delete failed: {msg}"));
    }

    pub fn on_retag_success(&mut self, new_tag: String) {
        if !self.tags_all.contains(&new_tag) {
            self.tags_all.push(new_tag.clone());
            self.apply_tag_filter_sort();
        }
        self.set_status(format!("✓ Tagged as {new_tag}"));
    }

    pub fn on_retag_error(&mut self, msg: String) {
        self.set_status(format!("✗ Retag failed: {msg}"));
    }

    pub fn resort_tags(&mut self) {
        self.apply_tag_filter_sort();
    }

    pub fn tick(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
        if let Some(s) = &self.status
            && Instant::now() >= s.expires_at
        {
            self.status = None;
        }
    }

    // ------------------------------------------------------------------
    // Modal-setup handlers (pure state, no async dependencies)
    // ------------------------------------------------------------------

    pub fn copy_pull_url(&mut self) {
        let Some(pull_url) = self.detail.as_ref().map(|d| d.pull_url.clone()) else {
            return;
        };
        match clipboard::copy_to_clipboard(&pull_url) {
            Ok(()) => self.set_status(format!("✓ Copied: {pull_url}")),
            Err(e) => self.set_status(format!("Clipboard error: {e}")),
        }
    }

    pub fn start_copy_image(&mut self) {
        let Some(tag) = self.selected_tag().map(str::to_owned) else {
            return;
        };
        let Some(repo) = self.current_repo.clone() else {
            return;
        };
        let prefilled = format!("{repo}:{tag}");
        let mut input = InputState::default();
        input.start(&prefilled);
        self.modal = Modal::Input {
            prompt: "Copy to (repo:tag):".to_owned(),
            input,
            on_confirm: InputAction::CopyImage {
                src_repo: repo,
                src_tag: tag,
            },
        };
    }

    pub fn start_retag(&mut self) {
        let Some(tag) = self.selected_tag().map(str::to_owned) else {
            return;
        };
        let Some(repo) = self.current_repo.clone() else {
            return;
        };
        self.modal = Modal::Input {
            prompt: format!("New tag for '{repo}:{tag}':"),
            input: InputState::default(),
            on_confirm: InputAction::Retag { repo, src_tag: tag },
        };
    }

    pub fn start_registry_select(&mut self) {
        let current = self.active_profile_idx;
        self.modal = Modal::RegistrySelect {
            selected_idx: current,
        };
    }

    pub fn start_delete(&mut self) {
        if self.focus == Focus::Tags
            && let Some(tag) = self.selected_tag().map(str::to_owned)
            && let Some(repo) = self.current_repo.clone()
        {
            let msg = format!("Delete '{repo}:{tag}'?");
            self.modal = Modal::Confirm {
                message: msg,
                on_confirm: ConfirmAction::DeleteManifest { repo, tag },
            };
        }
    }

    pub fn start_export(&mut self) {
        let Some(tag) = self.selected_tag().map(str::to_owned) else {
            return;
        };
        let Some(repo) = self.current_repo.clone() else {
            return;
        };
        let default_path = format!("{}-{}.tar", repo.replace('/', "-"), tag);
        let mut input = InputState::default();
        input.start(&default_path);
        self.modal = Modal::Input {
            prompt: "Export OCI tar to:".to_owned(),
            input,
            on_confirm: InputAction::Export { repo, tag },
        };
    }

    pub fn start_diff(&mut self) {
        let Some(tag) = self.selected_tag().map(str::to_owned) else {
            return;
        };
        let Some(repo) = self.current_repo.clone() else {
            return;
        };
        self.modal = Modal::Input {
            prompt: format!("Diff '{tag}' against tag:"),
            input: InputState::default(),
            on_confirm: InputAction::DiffAgainst { repo, tag_a: tag },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RegistryProfile, RegistryType};

    fn make_app() -> App {
        let profile = RegistryProfile {
            name: "test".to_owned(),
            url: "http://localhost:5000".to_owned(),
            username: None,
            registry_type: RegistryType::Standard,
            ..Default::default()
        };
        App::new(vec![profile], 0)
    }

    fn make_app_with_repos(repos: Vec<&str>) -> App {
        let mut app = make_app();
        app.on_repos_page(repos.into_iter().map(str::to_owned).collect(), false);
        app
    }

    fn make_app_with_tags(repo: &str, tags: Vec<&str>) -> App {
        let mut app = make_app();
        app.start_tags_load(repo.to_owned());
        app.on_tags_page(
            repo.to_owned(),
            tags.into_iter().map(str::to_owned).collect(),
            false,
        );
        app
    }

    #[test]
    fn new_initial_state() {
        let app = make_app();
        assert_eq!(app.focus, Focus::Repos);
        assert!(!app.should_quit);
        assert!(app.repos.is_empty());
        assert!(app.tags.is_empty());
        assert!(matches!(app.modal, Modal::None));
        assert_eq!(app.repo_load, LoadState::Idle);
        assert_eq!(app.tag_load, LoadState::Idle);
        assert_eq!(app.spinner_tick, 0);
        assert!(app.current_repo.is_none());
    }

    #[test]
    fn scroll_down_up_repos() {
        let mut app = make_app_with_repos(vec!["a", "b", "c"]);
        assert_eq!(app.repos_state.selected(), Some(0));
        app.scroll_down();
        assert_eq!(app.repos_state.selected(), Some(1));
        app.scroll_up();
        assert_eq!(app.repos_state.selected(), Some(0));
        // scroll_up at top stays at 0
        app.scroll_up();
        assert_eq!(app.repos_state.selected(), Some(0));
    }

    #[test]
    fn filter_push_pop_clear() {
        // "crow" contains no 'a', so only alpha+aleph match
        let mut app = make_app_with_repos(vec!["alpha", "crow", "aleph"]);
        app.filter_mode = Some(Focus::Repos);

        app.push_filter_char('a');
        assert_eq!(app.repo_filter, "a");
        assert_eq!(app.repos.len(), 2); // alpha, aleph

        app.push_filter_char('l');
        assert_eq!(app.repo_filter, "al");
        assert_eq!(app.repos.len(), 2); // alpha, aleph

        app.pop_filter_char();
        assert_eq!(app.repo_filter, "a");

        app.clear_active_filter();
        assert_eq!(app.repo_filter, "");
        assert!(app.filter_mode.is_none());
        assert_eq!(app.repos.len(), 3);
    }

    #[test]
    fn on_repos_page_populates_list() {
        let mut app = make_app();
        app.on_repos_page(vec!["r1".to_owned(), "r2".to_owned()], false);
        assert_eq!(app.repos, vec!["r1", "r2"]);
        assert_eq!(app.repo_load, LoadState::Idle);
        assert!(!app.repos_has_more);
    }

    #[test]
    fn on_repos_page_twice_accumulates() {
        let mut app = make_app();
        app.on_repos_page(vec!["r1".to_owned()], true);
        app.on_repos_page(vec!["r2".to_owned()], false);
        assert_eq!(app.repos, vec!["r1", "r2"]);
        assert!(!app.repos_has_more);
    }

    #[test]
    fn on_tags_page_ignores_stale_repo() {
        let mut app = make_app();
        app.start_tags_load("r1".to_owned());
        app.on_tags_page("r2".to_owned(), vec!["latest".to_owned()], false);
        assert!(app.tags.is_empty());
        assert_eq!(app.tag_load, LoadState::Loading);
    }

    #[test]
    fn start_tags_load_resets_state() {
        let mut app = make_app_with_tags("old", vec!["v1"]);
        assert!(!app.tags.is_empty());

        app.start_tags_load("new".to_owned());
        assert!(app.tags.is_empty());
        assert_eq!(app.tag_load, LoadState::Loading);
        assert_eq!(app.current_repo.as_deref(), Some("new"));
        assert!(app.detail.is_none());
    }

    #[test]
    fn start_registry_switch_resets_all() {
        let profile_a = RegistryProfile {
            name: "a".to_owned(),
            url: "http://a:5000".to_owned(),
            username: None,
            registry_type: RegistryType::Standard,
            ..Default::default()
        };
        let profile_b = RegistryProfile {
            name: "b".to_owned(),
            url: "http://b:5000".to_owned(),
            username: None,
            registry_type: RegistryType::Standard,
            ..Default::default()
        };
        let mut app = App::new(vec![profile_a, profile_b], 0);
        app.on_repos_page(vec!["r1".to_owned()], false);
        app.start_tags_load("r1".to_owned());
        app.on_tags_page("r1".to_owned(), vec!["v1".to_owned()], false);

        app.start_registry_switch(1);

        assert!(app.repos.is_empty());
        assert!(app.tags.is_empty());
        assert!(app.current_repo.is_none());
        assert_eq!(app.focus, Focus::Repos);
        assert_eq!(app.active_profile_idx, 1);
        assert_eq!(app.registry_name, "b");
    }

    fn artifactory_repo(key: &str) -> ArtifactoryRepo {
        ArtifactoryRepo {
            key: key.to_owned(),
            repo_type: "LOCAL".to_owned(),
            url: format!("https://artifactory.example.com/artifactory/{key}"),
            package_type: "Docker".to_owned(),
        }
    }

    #[test]
    fn start_artifactory_switch_opens_loading_picker() {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: "https://artifactory.example.com/artifactory".to_owned(),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);
        app.start_artifactory_switch(0);
        assert!(matches!(
            app.modal,
            Modal::ArtifactoryPicker { loading: true, .. }
        ));
        assert_eq!(app.repo_load, LoadState::Idle);
    }

    #[test]
    fn on_artifactory_repos_fills_picker_and_clears_loading() {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: "https://artifactory.example.com/artifactory".to_owned(),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);
        app.start_artifactory_switch(0);
        app.on_artifactory_repos(vec![
            artifactory_repo("docker-local"),
            artifactory_repo("docker-remote"),
        ]);
        assert!(matches!(
            app.modal,
            Modal::ArtifactoryPicker { loading: false, ref repos, .. } if repos.len() == 2
        ));
    }

    #[test]
    fn artifactory_filtered_repos_matches_substring_case_insensitive() {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: "https://artifactory.example.com/artifactory".to_owned(),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);
        app.start_artifactory_switch(0);
        app.on_artifactory_repos(vec![
            artifactory_repo("docker-local"),
            artifactory_repo("maven-local"),
        ]);

        assert_eq!(app.artifactory_filtered_repos().len(), 2);

        if let Modal::ArtifactoryPicker { filter, .. } = &mut app.modal {
            filter.start("DOCKER");
        }
        let filtered = app.artifactory_filtered_repos();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].key, "docker-local");
    }

    #[test]
    fn enter_artifactory_repo_closes_modal_and_resets_state() {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: "https://artifactory.example.com/artifactory".to_owned(),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);
        app.start_artifactory_switch(0);
        app.on_artifactory_repos(vec![artifactory_repo("docker-local")]);

        let scoped_url =
            "https://artifactory.example.com/artifactory/api/docker/docker-local/".to_owned();
        app.enter_artifactory_repo("docker-local", scoped_url.clone());

        assert!(matches!(app.modal, Modal::None));
        assert_eq!(app.registry_name, "art/docker-local");
        assert_eq!(app.registry_url, scoped_url);
        assert_eq!(app.repo_load, LoadState::Loading);
        assert!(app.repos.is_empty());
    }

    #[test]
    fn restart_catalog_load_preserves_artifactory_repo_key() {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: "https://artifactory.example.com/artifactory".to_owned(),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);
        app.start_artifactory_switch(0);
        app.on_artifactory_repos(vec![artifactory_repo("docker-local")]);

        let scoped_url =
            "https://artifactory.example.com/artifactory/api/docker/docker-local/".to_owned();
        app.enter_artifactory_repo("docker-local", scoped_url.clone());
        app.on_repos_page(vec!["nginx".to_owned()], false);

        // Simulate a successful re-auth while browsing inside a repo-key.
        app.catalog_attempt = CatalogAttempt::AfterCredential;
        app.restart_catalog_load();

        // The catalog reloads...
        assert_eq!(app.repo_load, LoadState::Loading);
        assert!(app.repos.is_empty());
        // ...and restart_catalog_load leaves the stage alone — it's the
        // caller's job to set it, since the reload alone can't say which
        // retry it's for.
        assert_eq!(app.catalog_attempt, CatalogAttempt::AfterCredential);

        // ...but the user is NOT ejected from the repo-key they were in.
        // `start_registry_switch` would have cleared all three of these, while
        // the client stayed scoped to the repo-key.
        assert_eq!(
            app.current_artifactory_repo_key.as_deref(),
            Some("docker-local")
        );
        assert_eq!(app.registry_name, "art/docker-local");
        assert_eq!(app.registry_url, scoped_url);
    }

    fn app_inside_artifactory_repo() -> App {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: "https://artifactory.example.com/artifactory".to_owned(),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);
        app.start_artifactory_switch(0);
        app.on_artifactory_repos(vec![
            artifactory_repo("docker-local"),
            artifactory_repo("docker-remote"),
        ]);
        let scoped_url =
            "https://artifactory.example.com/artifactory/api/docker/docker-local/".to_owned();
        app.enter_artifactory_repo("docker-local", scoped_url);
        app.on_repos_page(vec!["myimage".to_owned()], false);
        app
    }

    #[test]
    fn open_artifactory_picker_cached_uses_cache_and_preselects_current_key() {
        let mut app = app_inside_artifactory_repo();

        app.open_artifactory_picker_cached();

        assert!(matches!(
            app.modal,
            Modal::ArtifactoryPicker { loading: true, ref repos, selected: 0, .. }
                if repos.len() == 2 && repos[0].key == "docker-local"
        ));
    }

    #[test]
    fn open_artifactory_picker_cached_does_not_reset_browsing_state() {
        let mut app = app_inside_artifactory_repo();
        assert_eq!(app.repos, vec!["myimage"]);

        app.open_artifactory_picker_cached();

        // Esc from the picker (Modal::None) should return to exactly this.
        assert_eq!(app.repos, vec!["myimage"]);
        assert_eq!(
            app.current_artifactory_repo_key.as_deref(),
            Some("docker-local")
        );
        assert_eq!(app.registry_name, "art/docker-local");
    }

    #[test]
    fn on_artifactory_repos_clamps_selection_on_background_refresh() {
        let mut app = app_inside_artifactory_repo();
        app.open_artifactory_picker_cached();
        if let Modal::ArtifactoryPicker { selected, .. } = &mut app.modal {
            *selected = 1;
        }

        // Refresh lands with a shorter list (docker-remote dropped upstream).
        app.on_artifactory_repos(vec![artifactory_repo("docker-local")]);

        assert!(matches!(
            app.modal,
            Modal::ArtifactoryPicker { loading: false, selected: 0, ref repos, .. }
                if repos.len() == 1
        ));
    }

    #[test]
    fn open_artifactory_picker_cached_empty_cache_opens_loading() {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: "https://artifactory.example.com/artifactory".to_owned(),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);

        app.open_artifactory_picker_cached();

        assert!(matches!(
            app.modal,
            Modal::ArtifactoryPicker { loading: true, ref repos, selected: 0, .. }
                if repos.is_empty()
        ));
    }

    #[test]
    fn tick_increments_spinner() {
        let mut app = make_app();
        assert_eq!(app.spinner_tick, 0);
        app.tick();
        assert_eq!(app.spinner_tick, 1);
        app.tick();
        assert_eq!(app.spinner_tick, 2);
    }

    #[test]
    fn tick_expires_status() {
        use std::time::{Duration, Instant};
        let mut app = make_app();
        app.status = Some(StatusMessage {
            text: "hello".to_owned(),
            expires_at: Instant::now() - Duration::from_secs(1),
        });
        app.tick();
        assert!(app.status_text().is_none());
    }

    #[test]
    fn start_copy_image_sets_modal() {
        let mut app = make_app_with_tags("myrepo", vec!["v1"]);
        app.start_copy_image();
        assert!(matches!(
            app.modal,
            Modal::Input {
                ref on_confirm,
                ..
            } if matches!(on_confirm, InputAction::CopyImage { src_repo, src_tag }
                if src_repo == "myrepo" && src_tag == "v1")
        ));
    }

    #[test]
    fn start_copy_image_noop_without_tag() {
        let mut app = make_app();
        app.start_copy_image();
        assert!(matches!(app.modal, Modal::None));
    }

    #[test]
    fn start_retag_sets_modal() {
        let mut app = make_app_with_tags("myrepo", vec!["v1"]);
        app.start_retag();
        assert!(matches!(
            app.modal,
            Modal::Input {
                ref on_confirm,
                ..
            } if matches!(on_confirm, InputAction::Retag { repo, src_tag }
                if repo == "myrepo" && src_tag == "v1")
        ));
    }

    #[test]
    fn start_delete_sets_confirm_modal() {
        let mut app = make_app_with_tags("myrepo", vec!["v1"]);
        app.focus = Focus::Tags;
        app.start_delete();
        assert!(matches!(
            app.modal,
            Modal::Confirm {
                ref on_confirm,
                ..
            } if matches!(on_confirm, ConfirmAction::DeleteManifest { repo, tag }
                if repo == "myrepo" && tag == "v1")
        ));
    }

    #[test]
    fn start_delete_noop_when_not_tags_focus() {
        let mut app = make_app_with_tags("myrepo", vec!["v1"]);
        app.focus = Focus::Repos;
        app.start_delete();
        assert!(matches!(app.modal, Modal::None));
    }

    #[test]
    fn should_load_more_repos_when_has_more() {
        let mut app = make_app();
        app.on_repos_page(vec!["r1".to_owned()], true);
        // selected=0, repos.len()=1, LOAD_AHEAD=20 → 0+20 >= 1 → true
        assert!(app.should_load_more_repos());
    }

    #[test]
    fn should_load_more_repos_false_when_no_more() {
        let mut app = make_app();
        app.on_repos_page(vec!["r1".to_owned()], false);
        assert!(!app.should_load_more_repos());
    }

    #[test]
    fn should_load_more_tags_when_has_more() {
        let mut app = make_app_with_tags("r", vec!["v1"]);
        app.tags_has_more = true;
        assert!(app.should_load_more_tags());
    }

    // ---- InspectModal (JSON viewer) ----

    const INSPECT_JSON: &str = r#"{
  "schemaVersion": 2,
  "config": {
    "digest": "sha256:abc",
    "size": 1234
  },
  "layers": [
    {
      "digest": "sha256:def"
    }
  ]
}"#;

    fn inspect_modal() -> InspectModal {
        let lines = INSPECT_JSON.lines().map(str::to_owned).collect();
        let mut m = InspectModal::new("img:tag".to_owned(), lines);
        m.set_viewport(4); // a small viewport to exercise paging/scroll
        m
    }

    #[test]
    fn cursor_moves_and_clamps_at_ends() {
        let mut m = inspect_modal();
        assert_eq!(m.cursor, 0);
        m.move_cursor(-1); // clamp at top
        assert_eq!(m.cursor, 0);
        m.jump_bottom();
        let last = m.visible.len() - 1;
        assert_eq!(m.cursor, last);
        m.move_cursor(1); // clamp at bottom
        assert_eq!(m.cursor, last);
    }

    #[test]
    fn page_steps_by_viewport_height() {
        let mut m = inspect_modal(); // viewport_h == 4
        m.page(1);
        assert_eq!(m.cursor, 4);
    }

    #[test]
    fn toggle_fold_keeps_cursor_on_the_opener() {
        let mut m = inspect_modal();
        // Move cursor onto the "config" opener (absolute line 2).
        m.move_cursor(2);
        assert_eq!(m.cursor_line(), 2);
        m.toggle_fold();
        // config interior hidden; cursor still on the opener line.
        assert_eq!(m.cursor_line(), 2);
        assert!(m.collapsed[2]);
        assert!(!m.visible.contains(&3));
        m.toggle_fold(); // unfold restores interior, cursor unchanged
        assert_eq!(m.cursor_line(), 2);
        assert!(m.visible.contains(&3));
    }

    #[test]
    fn collapse_all_then_expand_all_round_trips() {
        let mut m = inspect_modal();
        let full = m.visible.len();
        m.collapse_all();
        assert!(m.visible.len() < full);
        assert!(m.visible.contains(&0)); // root opener still shown
        m.expand_all();
        assert_eq!(m.visible.len(), full);
    }

    #[test]
    fn search_jumps_to_match_and_cycles() {
        let mut m = inspect_modal();
        m.start_search();
        assert!(m.search.active);
        for c in "digest".chars() {
            m.search.input.insert(c);
        }
        m.commit_search();
        assert!(!m.search.active);
        assert_eq!(m.search.matches, vec![3, 8]);
        assert_eq!(m.cursor_line(), 3); // first hit
        m.next_match();
        assert_eq!(m.cursor_line(), 8);
        m.next_match(); // wraps back to first
        assert_eq!(m.cursor_line(), 3);
        m.prev_match(); // wraps to last
        assert_eq!(m.cursor_line(), 8);
    }

    #[test]
    fn search_expands_folds_hiding_a_match() {
        let mut m = inspect_modal();
        m.collapse_all();
        assert!(!m.visible.contains(&8)); // nested "digest" hidden
        m.start_search();
        for c in "sha256:def".chars() {
            m.search.input.insert(c);
        }
        m.commit_search();
        assert_eq!(m.cursor_line(), 8);
        assert!(m.visible.contains(&8)); // ancestors auto-expanded
    }

    #[test]
    fn input_action_is_secret_only_for_credential_actions() {
        assert!(
            InputAction::EnterPassword {
                profile_name: "p".to_owned(),
                username: "u".to_owned(),
            }
            .is_secret()
        );

        for action in [
            InputAction::BrowseRepo,
            InputAction::CopyImage {
                src_repo: "r".to_owned(),
                src_tag: "t".to_owned(),
            },
            InputAction::Retag {
                repo: "r".to_owned(),
                src_tag: "t".to_owned(),
            },
            InputAction::Export {
                repo: "r".to_owned(),
                tag: "t".to_owned(),
            },
            InputAction::DiffAgainst {
                repo: "r".to_owned(),
                tag_a: "t".to_owned(),
            },
        ] {
            assert!(!action.is_secret(), "{action:?} must not be masked");
        }
    }

    #[test]
    fn catalog_attempt_defaults_to_initial() {
        let profile = RegistryProfile {
            name: "local".to_owned(),
            url: "http://localhost:5000".to_owned(),
            username: None,
            registry_type: RegistryType::Standard,
            ..Default::default()
        };
        let app = App::new(vec![profile], 0);
        assert_eq!(app.catalog_attempt, CatalogAttempt::Initial);
    }

    #[test]
    fn reset_for_new_registry_resets_catalog_attempt() {
        // A stale AfterCredential/AfterReread stage from the previous
        // registry must not suppress a legitimate silent-reread or prompt on
        // the next one.
        let profiles = vec![
            RegistryProfile {
                name: "a".to_owned(),
                url: "http://a.example.com".to_owned(),
                username: None,
                registry_type: RegistryType::Standard,
                ..Default::default()
            },
            RegistryProfile {
                name: "b".to_owned(),
                url: "http://b.example.com".to_owned(),
                username: None,
                registry_type: RegistryType::Standard,
                ..Default::default()
            },
        ];
        let mut app = App::new(profiles, 0);
        app.catalog_attempt = CatalogAttempt::AfterCredential;

        app.start_registry_switch(1);

        assert_eq!(app.catalog_attempt, CatalogAttempt::Initial);
    }
}
