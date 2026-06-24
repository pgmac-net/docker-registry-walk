#![allow(dead_code)]

use std::time::{Duration, Instant};

use ratatui::widgets::ListState;

const STATUS_TTL: Duration = Duration::from_secs(2);

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
    pub repos: Vec<String>,
    pub repos_state: ListState,
    pub tags: Vec<String>,
    pub tags_state: ListState,
    pub registry_name: String,
    pub registry_url: String,
    pub modal: Modal,
    pub should_quit: bool,
    status: Option<StatusMessage>,
}

impl App {
    pub fn new(registry_name: String, registry_url: String) -> Self {
        let mut repos_state = ListState::default();
        repos_state.select(Some(0));
        Self {
            focus: Focus::Repos,
            repos: Vec::new(),
            repos_state,
            tags: Vec::new(),
            tags_state: ListState::default(),
            registry_name,
            registry_url,
            modal: Modal::None,
            should_quit: false,
            status: None,
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(StatusMessage {
            text: msg.into(),
            expires_at: Instant::now() + STATUS_TTL,
        });
    }

    pub fn status_text(&self) -> Option<&str> {
        self.status.as_ref().map(|s| s.text.as_str())
    }

    pub fn tick(&mut self) {
        if let Some(s) = &self.status
            && Instant::now() >= s.expires_at
        {
            self.status = None;
        }
    }

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
}
