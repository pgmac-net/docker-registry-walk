# Research: TUI Module Refactor

**Feature**: [spec.md](./spec.md)
**Date**: 2026-06-27
**Status**: Complete — all questions resolved from code inspection

## Findings

### Finding 1: Current tui/mod.rs concern breakdown

| Lines | Concern | Destination |
|-------|---------|-------------|
| 1–35 | Module declarations, imports | `mod.rs` (stays, trimmed) |
| 39–67 | `run()` — terminal setup/teardown | `mod.rs` (stays) |
| 69–234 | `event_loop()` — select! loop, client map, pagination | `event.rs` |
| 236–318 | `handle_event()` — event dispatch match | `event.rs` |
| 320–653 | `handle_key()` — key dispatch, all modal branches | `event.rs` |
| 655–840 | 12 `handle_*` free functions | Split: 7 → `App` methods, 5 → `event.rs` |
| 842–857 | `make_client_for_profile()` | `event.rs` |
| 859–1113 | 12 `spawn_*` async task functions | `event.rs` |

### Finding 2: App is already pure sync state

`src/tui/app.rs` (638 lines) has no async dependencies, no crossterm imports,
no tokio imports. It only depends on `ratatui::widgets::ListState`,
`crate::config::RegistryProfile`, `crate::ops::diff::DiffLayer`, and `super::detail::ImageDetail`.
All public methods are `&mut self` sync. **No structural changes needed to App.**

### Finding 3: handle_key dispatch split

Seven `handle_*` functions in `tui/mod.rs` (lines 663–840) only mutate `App` state —
they open modals, set clipboard, or update fields. They have no `client` or `tx`
parameters. These become `App` methods, making them unit-testable:
`App::start_copy_image()`, `App::start_retag()`, `App::start_registry_select()`,
`App::start_delete()`, `App::start_export()`, `App::start_diff()`,
`App::copy_pull_url()`.

Five remaining handlers (`handle_enter`, `handle_inspect`, `handle_prune`,
`handle_confirm`, `handle_input_confirm`) trigger async task spawns and stay in
`event.rs` as private free functions.

### Finding 4: Test coverage gap

Zero tests in the entire `tui/` module. 25 tests exist in `config.rs` (9),
`registry/auth.rs` (12), `registry/pagination.rs` (4). After refactoring, `App`
methods are the natural unit-test boundary — no terminal or tokio runtime required.

### Finding 5: #![allow(dead_code)] — deferred suppressor

Both `app.rs` and `event.rs` carry `#![allow(dead_code)]`. This hides real signal.
After the refactor adds callers (event loop → App methods) and tests, unused items
become visible. Remove the suppressors in Step 4 and fix any resulting warnings.

### Finding 6: Constants belong in event.rs

`TICK_MS = 200` and `PAGE_SIZE = 100` are defined in `tui/mod.rs` but only used
in `event_loop()` and `spawn_*` functions. Move with those functions to `event.rs`.

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| handle_key location | `event.rs` free function | Needs `&RegistryClient` + `&mpsc::Sender`; pure state mutations already on App |
| Pure handlers | Become `App` methods | Testable without terminal; matches App's role as sole state owner |
| Command enum | Rejected | Adds abstraction beyond refactor scope; handle_key-in-event.rs achieves same split cleanly |
| make_client_for_profile | Moves to `event.rs` | Only called from event_loop; no reason to expose |
| Test location | `app.rs #[cfg(test)]` | Tests state mutations; no need for separate test file |
| Delivery order | Methods → Tests → Move loop → Cleanup | Each step independently compilable and green |
