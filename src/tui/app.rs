#![allow(dead_code)]

use std::time::{Duration, Instant};

use ratatui::widgets::ListState;

const STATUS_TTL: Duration = Duration::from_secs(2);
const LOAD_AHEAD: usize = 20;
pub const SPINNER: [char; 6] = ['⠋', '⠙', '⠸', '⠴', '⠦', '⠇'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Repos,
    Tags,
}

impl Focus {
    pub fn toggle(self) -> Self {
        match self {
            Focus::Repos => Focus::Tags,
            Focus::Tags => Focus::Repos,
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
pub enum Modal {
    None,
    Confirm {
        message: String,
        on_confirm: ConfirmAction,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum ConfirmAction {
    DeleteManifest,
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
    // Sort
    pub tag_sort: SortOrder,
    // Display
    pub registry_name: String,
    pub registry_url: String,
    pub modal: Modal,
    pub should_quit: bool,
    pub spinner_tick: usize,
    status: Option<StatusMessage>,
}

impl App {
    pub fn new(registry_name: String, registry_url: String) -> Self {
        let mut repos_state = ListState::default();
        repos_state.select(Some(0));
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
            tag_sort: SortOrder::NameAsc,
            registry_name,
            registry_url,
            modal: Modal::None,
            should_quit: false,
            spinner_tick: 0,
            status: None,
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

    pub fn on_repos_error(&mut self, msg: String) {
        self.repo_load = LoadState::Error(msg.clone());
        self.set_status(format!("Repos error: {msg}"));
    }

    pub fn on_tags_page(&mut self, repo: String, tags: Vec<String>, has_more: bool) {
        if self.current_repo.as_deref() != Some(&repo) {
            return;
        }
        self.tags_has_more = has_more;
        self.tags_all.extend(tags);
        self.tags_cursor = self.tags_all.last().cloned();
        self.tag_load = LoadState::Idle;
        self.apply_tag_filter_sort();
    }

    pub fn on_tags_error(&mut self, msg: String) {
        self.tag_load = LoadState::Error(msg.clone());
        self.set_status(format!("Tags error: {msg}"));
    }

    // ------------------------------------------------------------------
    // Tag loading lifecycle
    // ------------------------------------------------------------------

    pub fn start_tags_load(&mut self, repo: String) {
        self.current_repo = Some(repo);
        self.tags_all.clear();
        self.tags.clear();
        self.tags_state.select(None);
        self.tags_cursor = None;
        self.tags_has_more = false;
        self.tag_filter.clear();
        self.tag_load = LoadState::Loading;
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
            None => {}
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
            None => {}
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
            None => {}
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
}
