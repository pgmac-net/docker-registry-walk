# Implementation Plan: TUI Module Refactor

**Branch**: `pgmac/ai-time` | **Date**: 2026-06-27 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `specs/001-codebase-refactor/spec.md`

## Summary

Decompose `src/tui/mod.rs` (1113 lines, 6 concerns) into the architecture already
described in `CLAUDE.md`: a thin entry-point `mod.rs` (≤80 lines), an `event.rs`
that owns the async event loop and all task spawners, and an `app.rs` with pure
sync state-mutation methods. Add targeted unit tests for `App` methods — the first
TUI tests in the project — that run without a terminal or async runtime.

## Technical Context

**Language/Version**: Rust stable, edition 2024

**Primary Dependencies**: ratatui 0.30, crossterm 0.29, tokio (full), mpsc channels

**Storage**: N/A

**Testing**: `cargo test` — new tests in `src/tui/app.rs` `#[cfg(test)]` block,
no terminal or tokio runtime required

**Target Platform**: Linux / macOS / Windows (TUI desktop app)

**Project Type**: Desktop TUI application

**Performance Goals**: No change — refactor is clarity-only, not throughput

**Constraints**: `cargo clippy -- -D warnings` must pass throughout every step;
all 25 existing tests must remain green; zero functional regression

**Scale/Scope**: 1113 lines → 3 focused files; ~12 new App method unit tests

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Credential Security | ✅ Not applicable | No credential code changes |
| II. Registry v2 Protocol | ✅ Not applicable | No protocol code changes |
| III. Module Isolation | ✅ **Directly addressed** | This refactor makes Principle III real: `tui/mod.rs` currently violates single-responsibility; after refactor each file has one job |
| IV. Zero-Warning Quality | ⚠️ **Gate active** | `cargo clippy -- -D warnings` must pass after EVERY step; removing `#![allow(dead_code)]` from app.rs and event.rs is part of the work |
| V. Async-First I/O | ✅ Preserved | Event loop stays async via tokio; only moves file |

**Gate result**: PASS with one active constraint — Principle IV requires zero new
warnings throughout. The `#![allow(dead_code)]` suppressors must be removed as
methods gain callers through the refactor.

## Project Structure

### Documentation (this feature)

```text
specs/001-codebase-refactor/
├── plan.md              # This file
├── research.md          # Phase 0 output (see below)
├── data-model.md        # Phase 1 output (see below)
├── quickstart.md        # Phase 1 output (see below)
└── tasks.md             # Created by /speckit-tasks
```

### Source Code (affected files only)

```text
src/tui/
├── mod.rs       # BEFORE: 1113 lines (6 concerns)
│                # AFTER:  ~40 lines (module decls + pub use + run() entry point)
├── event.rs     # BEFORE: 122 lines (AppEvent enum + spawn_event_reader only)
│                # AFTER:  ~700 lines (+ event_loop, handle_event, handle_key,
│                #          all 12 handle_* fns, all 12 spawn_* fns,
│                #          make_client_for_profile, TICK_MS, PAGE_SIZE)
├── app.rs       # BEFORE: 638 lines (pure state — already correct)
│                # AFTER:  ~680 lines (+ #[cfg(test)] block with ~12 unit tests)
├── detail.rs    # No change
└── ui.rs        # No change
```

**Structure Decision**: Single project (existing layout). Only `tui/` is touched.

## Complexity Tracking

> No constitution violations to justify.

---

## Phase 0: Research

*All unknowns resolved from code inspection. No external research required.*

### Decision: Where does `handle_key()` live after the split?

`handle_key()` does two things: mutates `App` state AND triggers async spawns
(needs `&RegistryClient` and `&mpsc::Sender<AppEvent>`). Two options:

- **Option A** — Command enum: `App::handle_key()` returns `Option<Command>`;
  `event.rs` dispatches commands. Pure but adds a new type and two-step dispatch.
- **Option B** — Keep in `event.rs`: `handle_key(app, code, mods, client, tx)`
  stays a free function in `event.rs`. Event loop calls it directly.

**Decision: Option B.** This is a refactor, not a redesign. `handle_key` and all
the `handle_*` helpers belong in `event.rs` because they need the async
infrastructure. Pure `App` state mutations (e.g., `scroll_up`, `set_status`,
`modal = Modal::None`) already exist as `App` methods and stay there.

### Decision: What moves to `App` methods vs stays in `event.rs`?

Examining `tui/mod.rs` handlers:

| Handler | Needs client/tx? | Move to App? |
|---------|-------------------|--------------|
| `handle_copy` (clipboard) | No | Yes → `App::copy_pull_url()` |
| `handle_copy_image` (modal) | No | Yes → `App::start_copy_image()` |
| `handle_retag` (modal) | No | Yes → `App::start_retag()` |
| `handle_registry_select` (modal) | No | Yes → `App::start_registry_select()` |
| `handle_delete` (modal) | No | Yes → `App::start_delete()` |
| `handle_export` (modal) | No | Yes → `App::start_export()` |
| `handle_diff` (modal) | No | Yes → `App::start_diff()` |
| `handle_enter` | Yes (calls inspect) | No — stays in event.rs |
| `handle_inspect` | Yes (spawns) | No — stays in event.rs |
| `handle_prune` | Yes (spawns) | No — stays in event.rs |
| `handle_confirm` | Yes (spawns) | No — stays in event.rs |
| `handle_input_confirm` | Yes (spawns) | No — stays in event.rs |

Seven modal-setup handlers become `App` methods (pure state, testable).
Five spawn-triggering handlers stay in `event.rs`.

### Decision: `make_client_for_profile` location

Used only in `event_loop()`. Moves to `event.rs` as a private free function.

### Decision: Constants

`TICK_MS` and `PAGE_SIZE` move to `event.rs` (only used there).

### Decision: `#![allow(dead_code)]` removal

Both `app.rs` and `event.rs` carry this suppressor. After:
- App methods gain callers (via event.rs) and tests (via #[cfg(test)]), dead_code
  warnings become real signal → suppressor removed from `app.rs`.
- `event.rs` types are all used by the event loop → suppressor removed from `event.rs`.

Removal is done at the end of the refactor (Step 4), after tests confirm coverage.

### Decision: Test scope

Tests added in `src/tui/app.rs` `#[cfg(test)]`. No terminal or tokio required.
Test targets:
1. `App::new()` — initial state fields
2. `app.scroll_down()` / `app.scroll_up()` — selection movement
3. `app.push_filter_char()` / `pop_filter_char()` / `clear_active_filter()` — filter
4. `app.on_repos_page()` — data arrival state update
5. `app.on_tags_page()` — data arrival state update
6. `app.start_tags_load()` — repository selection transition
7. `app.start_registry_switch()` — registry change resets all state
8. `app.tick()` — spinner increment, status TTL expiry
9. `app.start_copy_image()` (new) — modal set correctly
10. `app.start_retag()` (new) — modal set correctly
11. `app.start_delete()` (new) — modal set correctly when tag selected
12. `app.should_load_more_repos()` / `should_load_more_tags()` — pagination hints

---

## Phase 1: Design & Contracts

### Data Model

See [data-model.md](./data-model.md).

### Internal Module Contracts

See [contracts/tui-internal.md](./contracts/tui-internal.md).

### Quickstart Validation Guide

See [quickstart.md](./quickstart.md).

---

## Incremental Delivery Sequence

The refactor is delivered in four sequential steps, each independently compilable
and passing `cargo clippy -- -D warnings` and `cargo test`.

### Step 1 — Extract modal-setup handlers into App methods

Move `handle_copy`, `handle_copy_image`, `handle_retag`, `handle_registry_select`,
`handle_delete`, `handle_export`, `handle_diff` from free functions in `tui/mod.rs`
into methods on `App` in `app.rs`. Keep the calls in `tui/mod.rs`'s `handle_key`
pointing to `app.start_copy_image()` etc. No behavior change.

**Checkpoint**: `cargo test` passes; `cargo clippy -- -D warnings` passes.

### Step 2 — Add App method unit tests

Write `#[cfg(test)]` tests in `app.rs` covering all 12 test targets above.
All tests pass without a terminal.

**Checkpoint**: `cargo test` shows new test count; all pass.

### Step 3 — Move event loop and all spawners to `event.rs`

Move from `tui/mod.rs` to `tui/event.rs`:
- `event_loop()` function
- `handle_event()` function
- `handle_key()` function
- `handle_enter()`, `handle_inspect()`, `handle_prune()`, `handle_confirm()`,
  `handle_input_confirm()` (spawn-triggering handlers)
- All 12 `spawn_*` functions
- `make_client_for_profile()` function
- `TICK_MS`, `PAGE_SIZE` constants

`tui/mod.rs` calls `event::event_loop(terminal, profiles, initial_idx)` where
`event_loop` is now `pub(super)` in `event.rs`.

**Checkpoint**: `cargo test` passes; `cargo clippy -- -D warnings` passes;
`tui/mod.rs` line count is ≤ 80.

### Step 4 — Remove dead_code suppressors and clean up imports

Remove `#![allow(dead_code)]` from `app.rs` and `event.rs`. Fix any resulting
warnings (unused imports, unreachable variants, etc.). Verify final line counts.

**Checkpoint**: `cargo clippy -- -D warnings` passes with zero warnings;
`cargo test` passes; `wc -l src/tui/mod.rs` ≤ 80.
