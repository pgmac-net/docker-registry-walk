use std::time::{Duration, Instant};

use ratatui::widgets::ListState;

use crate::clipboard;
use crate::config::RegistryProfile;
use crate::ops::diff::DiffLayer;

use super::detail::ImageDetail;

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

#[derive(Debug)]
pub struct InspectModal {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
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
        value: String,
        cursor: usize,
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
        value: String,
        cursor: usize,
        results: Vec<String>,
        selected: usize,
        searching: bool,
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
    pub should_quit: bool,
    pub spinner_tick: usize,
    /// Set when a password was just entered; causes the next catalog error
    /// (even 401) to open BrowseRepo rather than the password modal again.
    pub catalog_retry_pending: bool,
    status: Option<StatusMessage>,
    // Registry switcher
    pub profiles: Vec<RegistryProfile>,
    pub active_profile_idx: usize,
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
            should_quit: false,
            spinner_tick: 0,
            catalog_retry_pending: false,
            status: None,
            profiles,
            active_profile_idx: idx,
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
                    value: String::new(),
                    cursor: 0,
                    results: Vec::new(),
                    selected: 0,
                    searching: false,
                };
            } else {
                self.modal = Modal::Input {
                    prompt: "Catalog unavailable. Enter repo name to browse:".to_owned(),
                    value: String::new(),
                    cursor: 0,
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

    /// Clear all repo/tag/detail state when switching registries.
    pub fn start_registry_switch(&mut self, idx: usize) {
        self.active_profile_idx = idx;
        let profile = &self.profiles[idx];
        self.registry_name = profile.name.clone();
        self.registry_url = profile.url.clone();

        self.repos_all.clear();
        self.repos.clear();
        self.repos_state.select(Some(0));
        self.repos_cursor = None;
        self.repos_has_more = false;
        self.repo_filter.clear();
        self.repo_load = LoadState::Loading;

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
        self.catalog_retry_pending = false;
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
        self.modal = Modal::Input {
            prompt: "Copy to (repo:tag):".to_owned(),
            value: prefilled,
            cursor: 0,
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
            value: String::new(),
            cursor: 0,
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
        self.modal = Modal::Input {
            prompt: "Export OCI tar to:".to_owned(),
            value: default_path,
            cursor: 0,
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
            value: String::new(),
            cursor: 0,
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
        };
        let profile_b = RegistryProfile {
            name: "b".to_owned(),
            url: "http://b:5000".to_owned(),
            username: None,
            registry_type: RegistryType::Standard,
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
}
