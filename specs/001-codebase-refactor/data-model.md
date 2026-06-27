# Data Model: TUI Module Refactor

**Feature**: [spec.md](./spec.md)
**Date**: 2026-06-27

*This is a structural refactor. No new entities are introduced. The entities below
document the existing state model that the refactor preserves and makes testable.*

## Entities

### App

The sole owner of all mutable TUI state. After refactor: pure sync struct,
no async/crossterm dependencies.

| Field | Type | Role |
|-------|------|------|
| `focus` | `Focus` | Which panel has keyboard focus |
| `filter_mode` | `Option<Focus>` | If set, keystrokes go to filter input for that panel |
| `repos` | `Vec<String>` | Displayed (filtered) repo list |
| `repos_state` | `ListState` | ratatui selection state for repo list |
| `repos_all` | `Vec<String>` | Raw loaded repos (before filter) |
| `repo_filter` | `String` | Current filter string for repos |
| `repos_cursor` | `Option<String>` | Pagination cursor for next repo page |
| `repos_has_more` | `bool` | Whether more repo pages exist |
| `repo_load` | `LoadState` | Loading / error state for repos |
| `tags` | `Vec<String>` | Displayed (filtered+sorted) tag list |
| `tags_state` | `ListState` | ratatui selection state for tag list |
| `tags_all` | `Vec<String>` | Raw loaded tags (before filter/sort) |
| `tag_filter` | `String` | Current filter string for tags |
| `tags_cursor` | `Option<String>` | Pagination cursor for next tag page |
| `tags_has_more` | `bool` | Whether more tag pages exist |
| `current_repo` | `Option<String>` | Repo whose tags are loaded |
| `tag_load` | `LoadState` | Loading / error state for tags |
| `tag_sort` | `SortOrder` | Current tag sort direction |
| `detail` | `Option<ImageDetail>` | Loaded detail for current tag |
| `detail_load` | `LoadState` | Loading / error state for detail |
| `detail_scroll` | `usize` | Scroll offset in detail panel |
| `current_tag` | `Option<String>` | Tag whose detail is loaded |
| `registry_name` | `String` | Display name of active registry |
| `registry_url` | `String` | URL of active registry |
| `modal` | `Modal` | Currently displayed modal (None = no modal) |
| `should_quit` | `bool` | Set to true → event loop exits |
| `spinner_tick` | `usize` | Wrapping counter for spinner animation |
| `catalog_retry_pending` | `bool` | True after password entry; prevents re-prompting on next 401 |
| `status` | `Option<StatusMessage>` | Timed status bar message (2s TTL) |
| `profiles` | `Vec<RegistryProfile>` | All configured registry profiles |
| `active_profile_idx` | `usize` | Index into profiles for active registry |

### Focus (enum)

| Variant | Meaning |
|---------|---------|
| `Repos` | Repo list panel has keyboard focus |
| `Tags` | Tag list panel has keyboard focus |
| `Detail` | Detail panel has keyboard focus |

**Transitions**: `Repos → Tags → Detail → Repos` (Tab/Right), reverse (BackTab/Left).

### LoadState (enum)

| Variant | Meaning |
|---------|---------|
| `Idle` | No load in progress, data may be present |
| `Loading` | Async fetch in flight |
| `Error(String)` | Last fetch failed; message displayed |

### Modal (enum)

| Variant | Displayed for |
|---------|--------------|
| `None` | Normal navigation mode |
| `Confirm { message, on_confirm }` | Y/N confirmation (delete, prune) |
| `Input { prompt, value, cursor, on_confirm }` | Text entry (copy dest, new tag, export path, etc.) |
| `RegistrySelect { selected_idx }` | Registry switcher |
| `Inspect(Box<InspectModal>)` | Raw manifest/config JSON viewer |
| `LayerDiff(Box<LayerDiffModal>)` | Layer diff viewer |
| `Help { scroll }` | Keybindings reference |
| `SearchPicker { value, cursor, results, selected, searching }` | Docker Hub live search |

### AppEvent (enum — channel message type)

Messages flowing from async tasks and crossterm reader back to the event loop.

| Variant | Sent by | Consumed by |
|---------|---------|-------------|
| `Key(KeyEvent)` | `spawn_event_reader` | event loop → `handle_key` |
| `Resize(u16, u16)` | `spawn_event_reader` | event loop (ignored) |
| `Tick` | (legacy, superseded by tick interval) | `app.tick()` |
| `ReposPage(Vec<String>, bool)` | `spawn_repos_fetch` | `app.on_repos_page()` |
| `ReposError { msg, auth_failed }` | `spawn_repos_fetch` | event loop (special-cased) |
| `PasswordEntered { .. }` | Input modal confirm | event loop (special-cased) |
| `TagsPage(String, Vec<String>, bool)` | `spawn_tags_fetch` | `app.on_tags_page()` |
| `TagsError(String)` | `spawn_tags_fetch` | `app.on_tags_error()` |
| `DetailLoaded { .. }` | `spawn_detail_fetch` | `app.on_detail_loaded()` |
| `DetailError(String)` | `spawn_detail_fetch` | `app.on_detail_error()` |
| `DeleteTagSuccess { .. }` | `spawn_delete` | `app.on_delete_success()` |
| `DeleteTagError(String)` | `spawn_delete` | `app.on_delete_error()` |
| `CopyProgress { done, total }` | `spawn_copy` | `app.set_status()` |
| `CopySuccess { dest }` | `spawn_copy` | `app.set_status()` |
| `CopyError(String)` | `spawn_copy` | `app.set_status()` |
| `RetagSuccess { new_tag }` | `spawn_retag` | `app.on_retag_success()` |
| `RetagError(String)` | `spawn_retag` | `app.on_retag_error()` |
| `SwitchRegistry { idx }` | RegistrySelect modal | event loop (special-cased) |
| `InspectLoaded { title, lines }` | `spawn_inspect` | `modal = Modal::Inspect(..)` |
| `InspectError(String)` | `spawn_inspect` | `app.set_status()` |
| `PruneFound { repo, tags }` | `spawn_prune_find` | `modal = Modal::Confirm(..)` |
| `PruneComplete { repo, count }` | `spawn_prune` | `app.set_status()` |
| `PruneError(String)` | `spawn_prune_find` / `spawn_prune` | `app.set_status()` |
| `ExportProgress { done, total }` | `spawn_export` | `app.set_status()` |
| `ExportComplete { path }` | `spawn_export` | `app.set_status()` |
| `ExportError(String)` | `spawn_export` | `app.set_status()` |
| `DiffLoaded { .. }` | `spawn_diff` | `modal = Modal::LayerDiff(..)` |
| `DiffError(String)` | `spawn_diff` | `app.set_status()` |
| `BrowseRepo(String)` | Input modal confirm | `app.start_tags_load()` + focus |
| `DockerHubSearch { query, results }` | `spawn_dockerhub_search` | SearchPicker modal update |
| `DockerHubSearchError(String)` | `spawn_dockerhub_search` | SearchPicker modal update |

### New App Methods (added by refactor)

| Method | Current location | Parameters | Returns |
|--------|-----------------|------------|---------|
| `copy_pull_url()` | `handle_copy` in mod.rs | `&mut self` | `()` (calls clipboard) |
| `start_copy_image()` | `handle_copy_image` in mod.rs | `&mut self` | `()` |
| `start_retag()` | `handle_retag` in mod.rs | `&mut self` | `()` |
| `start_registry_select()` | `handle_registry_select` in mod.rs | `&mut self` | `()` |
| `start_delete()` | `handle_delete` in mod.rs | `&mut self` | `()` |
| `start_export()` | `handle_export` in mod.rs | `&mut self` | `()` |
| `start_diff()` | `handle_diff` in mod.rs | `&mut self` | `()` |
