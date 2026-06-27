# Contract: tui/ Internal Module Interfaces

**Feature**: [spec.md](../spec.md)
**Date**: 2026-06-27

This is an internal refactor. The only public surface of the `tui` crate module is:
- `pub use app::App` — the application state struct
- `pub async fn run(profiles, initial_idx)` — the entry point

These are unchanged by the refactor. The contracts below document the internal
module boundaries that the refactor makes explicit.

---

## tui/mod.rs — Public surface (unchanged)

```
pub use app::App

pub async fn run(
    profiles: Vec<RegistryProfile>,
    initial_idx: usize,
) -> anyhow::Result<()>
```

**Responsibility**: Terminal lifecycle only. Sets up crossterm raw mode and
alternate screen, delegates to `event::event_loop`, tears down terminal on return
or panic. ≤ 80 lines after refactor.

---

## tui/event.rs — Internal event loop (after refactor)

```
pub(super) async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    profiles: Vec<RegistryProfile>,
    initial_idx: usize,
) -> anyhow::Result<()>

pub fn spawn_event_reader(tx: mpsc::Sender<AppEvent>)

pub enum AppEvent { ... }   // (existing — unchanged)
```

**Responsibility**: Owns the `tokio::select!` loop, all `spawn_*` async task
functions, all `handle_*` key/event dispatch functions, and `make_client_for_profile`.
Is the **sole location** for `tokio::spawn` calls in the `tui` module.

Private helpers (not exported):
- `event_loop()` — `pub(super)` so `mod.rs` can call it
- `handle_event()`, `handle_key()`, `handle_enter()`, `handle_inspect()`,
  `handle_prune()`, `handle_confirm()`, `handle_input_confirm()` — `fn` (private)
- All `spawn_*` functions — `fn` (private)
- `make_client_for_profile()` — `fn` (private)
- `TICK_MS`, `PAGE_SIZE` — `const` (private)

---

## tui/app.rs — App state (after refactor, new methods added)

All existing public methods remain. New methods extracted from `tui/mod.rs`:

```
impl App {
    // Existing (unchanged):
    pub fn new(profiles, initial_idx) -> Self
    pub fn on_repos_page(&mut self, repos, has_more)
    pub fn on_repos_error(&mut self, msg, show_browse)
    pub fn on_tags_page(&mut self, repo, tags, has_more)
    pub fn on_tags_error(&mut self, msg)
    pub fn start_detail_load(&mut self, tag)
    pub fn on_detail_loaded(&mut self, repo, tag, detail)
    pub fn on_detail_error(&mut self, msg)
    pub fn scroll_detail(&mut self, delta, max_scroll)
    pub fn start_tags_load(&mut self, repo)
    pub fn start_registry_switch(&mut self, idx)
    pub fn should_load_more_repos(&self) -> bool
    pub fn should_load_more_tags(&self) -> bool
    pub fn push_filter_char(&mut self, ch)
    pub fn pop_filter_char(&mut self)
    pub fn clear_active_filter(&mut self)
    pub fn scroll_up(&mut self)
    pub fn scroll_down(&mut self)
    pub fn selected_repo(&self) -> Option<&str>
    pub fn selected_tag(&self) -> Option<&str>
    pub fn set_status(&mut self, msg)
    pub fn status_text(&self) -> Option<&str>
    pub fn on_delete_success(&mut self, repo, tag)
    pub fn on_delete_error(&mut self, msg)
    pub fn on_retag_success(&mut self, new_tag)
    pub fn on_retag_error(&mut self, msg)
    pub fn resort_tags(&mut self)
    pub fn tick(&mut self)

    // NEW (extracted from mod.rs handle_* fns):
    pub fn copy_pull_url(&mut self)        // calls clipboard, sets status
    pub fn start_copy_image(&mut self)     // sets Modal::Input for copy
    pub fn start_retag(&mut self)          // sets Modal::Input for retag
    pub fn start_registry_select(&mut self) // sets Modal::RegistrySelect
    pub fn start_delete(&mut self)         // sets Modal::Confirm for delete
    pub fn start_export(&mut self)         // sets Modal::Input for export
    pub fn start_diff(&mut self)           // sets Modal::Input for diff
}
```

**Invariant**: `App` MUST NOT import `crossterm`, `tokio`, or `mpsc`. It is a
pure state struct with sync methods only.

---

## tui/ui.rs — Pure renderer (unchanged)

```
pub fn draw(f: &mut Frame, app: &mut App)
```

**Responsibility**: Reads `App` state, draws to ratatui `Frame`. No mutations
except ratatui `ListState` (which requires `&mut`). Unchanged by refactor.

---

## tui/detail.rs — ImageDetail parser (unchanged)

```
pub struct ImageDetail { ... }
impl ImageDetail {
    pub fn from_manifest_and_config(...) -> Self
}
```

Unchanged by refactor.
