---

description: "Task list for TUI module refactor"
---

# Tasks: TUI Module Refactor

**Input**: Design documents from `specs/001-codebase-refactor/`

**Prerequisites**: plan.md ✅ spec.md ✅ research.md ✅ data-model.md ✅ contracts/ ✅

**Tests**: Yes — targeted App method unit tests are in scope (FR-006, spec.md).

**Organization**: Tasks follow the four-step delivery sequence from plan.md.
User stories are ordered P1 → P2 → P3 per convention, but **Phase 3 (US3 tests)
MUST be implemented before Phase 4 (US1 structural move)** — tests are the safety
net for the event loop migration.

## Format: `[ID] [P?] [Story?] Description`

- **[P]**: Can be done in any order relative to other [P] tasks in the same phase
- **[Story]**: User story this task delivers (US1/US2/US3)
- Exact file paths included in every implementation task

## Path Conventions

All source under `src/tui/`. No new directories.

---

## Phase 1: Setup

**Purpose**: Confirm baseline is green before any changes.

- [ ] T001 Verify baseline passes all gates: `cargo build && cargo clippy -- -D warnings && cargo test && cargo fmt --check`

**Checkpoint**: All four commands exit 0. Do not proceed if any fail.

---

## Phase 2: Foundational — Extract Pure App Methods

**Purpose**: Pull the seven modal-setup handlers out of `src/tui/mod.rs` into pure
`App` methods in `src/tui/app.rs`. These handlers only mutate `App` state (no
`client` or `tx` needed). Making them `App` methods is prerequisite for both the
unit tests (Phase 3) and the event loop move (Phase 4).

**⚠️ CRITICAL**: No Phase 3 or Phase 4 work can begin until this phase is complete.

- [ ] T002 [P] Add `App::copy_pull_url(&mut self)` to `src/tui/app.rs` — move logic from `handle_copy()` in `src/tui/mod.rs` (calls `crate::clipboard::copy_to_clipboard`, sets status)
- [ ] T003 [P] Add `App::start_copy_image(&mut self)` to `src/tui/app.rs` — move logic from `handle_copy_image()` in `src/tui/mod.rs` (sets `Modal::Input` for copy destination)
- [ ] T004 [P] Add `App::start_retag(&mut self)` to `src/tui/app.rs` — move logic from `handle_retag()` in `src/tui/mod.rs` (sets `Modal::Input` for new tag name)
- [ ] T005 [P] Add `App::start_registry_select(&mut self)` to `src/tui/app.rs` — move logic from `handle_registry_select()` in `src/tui/mod.rs` (sets `Modal::RegistrySelect`)
- [ ] T006 [P] Add `App::start_delete(&mut self)` to `src/tui/app.rs` — move logic from `handle_delete()` in `src/tui/mod.rs` (sets `Modal::Confirm` for manifest deletion)
- [ ] T007 [P] Add `App::start_export(&mut self)` to `src/tui/app.rs` — move logic from `handle_export()` in `src/tui/mod.rs` (sets `Modal::Input` for export path)
- [ ] T008 [P] Add `App::start_diff(&mut self)` to `src/tui/app.rs` — move logic from `handle_diff()` in `src/tui/mod.rs` (sets `Modal::Input` for diff target tag)
- [ ] T009 Update `handle_key()` in `src/tui/mod.rs` to call the new App methods (`app.start_copy_image()`, `app.start_retag()`, `app.start_registry_select()`, `app.start_delete()`, `app.start_export()`, `app.start_diff()`, `app.copy_pull_url()`) — remove the original free functions once call sites are updated (depends on T002–T008)

**Checkpoint**: `cargo build && cargo clippy -- -D warnings && cargo test` all pass.
`grep "fn handle_copy\|fn handle_retag\|fn handle_delete\|fn handle_export\|fn handle_diff\|fn handle_registry" src/tui/mod.rs` returns no matches.

---

## Phase 3: US3 — Confident Maintenance (Unit Tests)

**Goal**: First unit tests for the TUI module. Tests run without a terminal or
tokio runtime. Cover all `App` state-mutation methods including the new methods
from Phase 2.

**⚠️ IMPLEMENT BEFORE PHASE 4** — these tests are the safety net for the event
loop migration. They must be green before any code moves to `event.rs`.

**Independent Test**: `cargo test tui::app` passes in < 1 second with no external
dependencies.

- [ ] T010 [P] [US3] Add `#[cfg(test)]` block to `src/tui/app.rs` with helper `fn make_app()` that builds a minimal `App` with one dummy profile
- [ ] T011 [P] [US3] Add test `new_initial_state` in `src/tui/app.rs` — asserts `focus == Focus::Repos`, `repos` empty, `modal == Modal::None`, `should_quit == false`
- [ ] T012 [P] [US3] Add test `scroll_down_up_repos` in `src/tui/app.rs` — populates `repos`, calls `scroll_down()`, asserts selection moves; calls `scroll_up()`, asserts returns to 0
- [ ] T013 [P] [US3] Add test `filter_push_pop_clear` in `src/tui/app.rs` — sets `filter_mode`, calls `push_filter_char('a')`, asserts `repo_filter == "a"`; calls `pop_filter_char()`, asserts empty; calls `clear_active_filter()`, asserts `filter_mode == None`
- [ ] T014 [P] [US3] Add test `on_repos_page_appends` in `src/tui/app.rs` — calls `on_repos_page(vec!["r1","r2"], false)`, asserts `repos == ["r1","r2"]`, `repo_load == LoadState::Idle`
- [ ] T015 [P] [US3] Add test `on_repos_page_twice_appends` in `src/tui/app.rs` — calls `on_repos_page` twice, asserts repos accumulate across pages
- [ ] T016 [P] [US3] Add test `on_tags_page_ignores_stale_repo` in `src/tui/app.rs` — sets `current_repo = Some("r1")`, calls `on_tags_page("r2", ...)`, asserts tags remain empty
- [ ] T017 [P] [US3] Add test `start_tags_load_resets_state` in `src/tui/app.rs` — pre-populate tags, call `start_tags_load("repo")`, assert `tags` empty, `tag_load == LoadState::Loading`, `current_repo == Some("repo")`
- [ ] T018 [P] [US3] Add test `start_registry_switch_resets_all` in `src/tui/app.rs` — pre-populate repos+tags, call `start_registry_switch(0)`, assert repos/tags/detail all cleared, `focus == Focus::Repos`
- [ ] T019 [P] [US3] Add test `tick_increments_spinner` in `src/tui/app.rs` — call `tick()`, assert `spinner_tick == 1`; call `tick()` again, assert `spinner_tick == 2`
- [ ] T020 [P] [US3] Add test `start_copy_image_sets_modal` in `src/tui/app.rs` — set `current_repo` and select a tag, call `start_copy_image()`, assert `modal` is `Modal::Input` with `on_confirm == InputAction::CopyImage {..}`
- [ ] T021 [P] [US3] Add test `start_retag_sets_modal` in `src/tui/app.rs` — set `current_repo` and select a tag, call `start_retag()`, assert `modal` is `Modal::Input` with `on_confirm == InputAction::Retag {..}`
- [ ] T022 [P] [US3] Add test `start_delete_sets_confirm_modal` in `src/tui/app.rs` — set `focus = Focus::Tags`, select a tag, call `start_delete()`, assert `modal` is `Modal::Confirm { on_confirm: ConfirmAction::DeleteManifest {..} }`

**Checkpoint**: `cargo test tui::app` shows ≥ 12 tests, all pass. No terminal opened.

---

## Phase 4: US1 — Developer Orientation (Move Event Loop)

**Goal**: Move everything except terminal setup out of `src/tui/mod.rs` into
`src/tui/event.rs`. `tui/mod.rs` reaches ≤ 80 lines.

**Independent Test**: `wc -l src/tui/mod.rs` ≤ 80. `grep -c "tokio::spawn" src/tui/mod.rs` returns 0. All gates pass.

**Prerequisites**: Phase 2 complete, Phase 3 green.

- [ ] T023 Move constants `TICK_MS` and `PAGE_SIZE` from `src/tui/mod.rs` to top of `src/tui/event.rs`; move `make_client_for_profile()` from `src/tui/mod.rs` to `src/tui/event.rs` as a private `fn`
- [ ] T024 [US1] Move `event_loop()` from `src/tui/mod.rs` to `src/tui/event.rs` as `pub(super) async fn event_loop(...)` — update `src/tui/mod.rs` `run()` to call `event::event_loop(...)`
- [ ] T025 [US1] Move `handle_event()` from `src/tui/mod.rs` to `src/tui/event.rs` as a private `fn` (update imports as needed)
- [ ] T026 [US1] Move `handle_key()` from `src/tui/mod.rs` to `src/tui/event.rs` as a private `fn`
- [ ] T027 [P] [US1] Move `handle_enter()`, `handle_inspect()`, `handle_prune()` from `src/tui/mod.rs` to `src/tui/event.rs` as private `fn`s
- [ ] T028 [P] [US1] Move `handle_confirm()`, `handle_input_confirm()` from `src/tui/mod.rs` to `src/tui/event.rs` as private `fn`s
- [ ] T029 [P] [US1] Move `spawn_copy()`, `spawn_retag()`, `spawn_delete()`, `spawn_inspect()` from `src/tui/mod.rs` to `src/tui/event.rs`
- [ ] T030 [P] [US1] Move `spawn_prune_find()`, `spawn_prune()`, `spawn_export()`, `spawn_diff()` from `src/tui/mod.rs` to `src/tui/event.rs`
- [ ] T031 [P] [US1] Move `spawn_repos_fetch()`, `spawn_tags_fetch()`, `spawn_detail_fetch()`, `spawn_dockerhub_search()` from `src/tui/mod.rs` to `src/tui/event.rs`
- [ ] T032 [US1] Remove all moved code and now-unused imports from `src/tui/mod.rs`; verify line count ≤ 80

**Checkpoint**: `wc -l src/tui/mod.rs` ≤ 80. `grep -c "tokio::spawn" src/tui/mod.rs` = 0. `cargo build && cargo clippy -- -D warnings && cargo test` all pass.

---

## Phase 5: US2 — Safe Feature Extension (Verify Extension Points)

**Goal**: Confirm that Phase 4 produced a clear, single extension point for new
operations. No new implementation code — this phase verifies acceptance criteria
and makes the extension pattern explicit.

**Independent Test**: A developer can locate where to wire a new op in `src/tui/event.rs`
within 2 minutes of reading the file.

- [ ] T033 [P] [US2] Add a section comment `// — Operation dispatch —` in `src/tui/event.rs` immediately before the block in `handle_event()` where op result events (`CopySuccess`, `RetagSuccess`, `DeleteTagSuccess`, etc.) are handled
- [ ] T034 [P] [US2] Add a section comment `// — Async task spawners —` in `src/tui/event.rs` immediately before the first `spawn_*` function — marks the canonical location for wiring new operations

**Checkpoint**: `grep "Operation dispatch\|Async task spawners" src/tui/event.rs` shows both markers. Adding a new op requires: one new file in `src/ops/`, one line in `src/ops/mod.rs`, one match arm in `handle_event()`, one `spawn_*` function — all in ≤ 3 files.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Remove the dead code suppressors now that all methods have callers and
tests. Final gate validation.

- [ ] T035 Remove `#![allow(dead_code)]` from `src/tui/app.rs`
- [ ] T036 Remove `#![allow(dead_code)]` from `src/tui/event.rs`
- [ ] T037 Fix any warnings revealed by suppressor removal (unused imports, unreachable variants) in `src/tui/app.rs` and `src/tui/event.rs`
- [ ] T038 [P] Run full gate validation per `specs/001-codebase-refactor/quickstart.md`: `cargo build && cargo clippy -- -D warnings && cargo test && cargo fmt --check`
- [ ] T039 [P] Manual smoke test per `specs/001-codebase-refactor/quickstart.md` — exercise all 13 key flows listed there

**Checkpoint**: All four commands pass. Manual test shows no functional regressions.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Phase 1 (Setup)**: No dependencies — start here
- **Phase 2 (Foundational)**: Depends on Phase 1 — **BLOCKS all user story phases**
- **Phase 3 (US3 Tests)**: Depends on Phase 2 — MUST complete before Phase 4
- **Phase 4 (US1 Move)**: Depends on Phase 3 — structural migration is safe only after tests are green
- **Phase 5 (US2 Verify)**: Depends on Phase 4 — verifies Phase 4's output
- **Phase 6 (Polish)**: Depends on all story phases

### Within Phase 2

- T002–T008 [P]: independent — add different `App` methods, no file conflicts within app.rs beyond sequential additions
- T009: depends on T002–T008 (updates call sites in mod.rs)

### Within Phase 3

- T010: write helper first — T011–T022 use `make_app()`
- T011–T022 [P]: all in `src/tui/app.rs` `#[cfg(test)]`, no inter-test dependencies

### Within Phase 4

- T023: move constants + helper first (reduces import noise in later steps)
- T024: move `event_loop()` (makes mod.rs call event.rs)
- T025: move `handle_event()` (called from event_loop)
- T026: move `handle_key()` (called from handle_event)
- T027–T031 [P]: move remaining handlers + spawners (all private fns, no order dependency among them after T026)
- T032: depends on T023–T031 (removes moved code from mod.rs)

---

## Parallel Example: Phase 2

```bash
# Add all seven pure App methods in parallel (different logical sections of app.rs):
Task: "Add App::copy_pull_url() to src/tui/app.rs"
Task: "Add App::start_copy_image() to src/tui/app.rs"
Task: "Add App::start_retag() to src/tui/app.rs"
Task: "Add App::start_registry_select() to src/tui/app.rs"
Task: "Add App::start_delete() to src/tui/app.rs"
Task: "Add App::start_export() to src/tui/app.rs"
Task: "Add App::start_diff() to src/tui/app.rs"

# Then update call sites:
Task: "Update handle_key() in mod.rs to call new App methods"
```

## Parallel Example: Phase 3

```bash
# All test functions are independent — add in any order:
Task: "Test App::new() initial state"
Task: "Test App::scroll_down() / scroll_up()"
Task: "Test App::push_filter_char() / pop / clear"
# ... etc
```

---

## Implementation Strategy

### MVP Delivery (Phase 2 + Phase 3 only)

1. Complete Phase 1 (Setup)
2. Complete Phase 2 (extract App methods)
3. **STOP and validate**: `cargo test` passes, new methods callable
4. Complete Phase 3 (add tests)
5. **STOP and validate**: 12+ new tests pass, < 1 second, no terminal

At this point US3 (Confident Maintenance) is delivered. Tests give confidence to proceed.

### Full Delivery

6. Complete Phase 4 (move event loop) → delivers US1 (Developer Orientation)
7. Complete Phase 5 (verify extension points) → delivers US2 (Safe Feature Extension)
8. Complete Phase 6 (polish) → zero warnings, all gates green

### Gates After Every Task Group

```sh
cargo build                  # must compile
cargo clippy -- -D warnings  # must be zero warnings
cargo test                   # must be all green
cargo fmt --check            # must be clean
```

Never skip a gate between phases.

---

## Notes

- [P] tasks = can be done in any order within the phase; all touch different code paths
- Phase 3 is ordered before Phase 4 deliberately — tests before structural move
- The seven `start_*` / `copy_*` App methods have no async dependencies; they are pure `&mut self` sync
- `make_client_for_profile()` uses `Arc<dyn Credentials>` and `KeyringStore` — it's private infrastructure for the event loop, not App state
- After T032, `src/tui/mod.rs` should contain only: 4 `mod` declarations, 1 `pub use`, and `pub async fn run()` (~40 lines total)
- The `#![allow(dead_code)]` suppressor removal (T035–T036) may reveal unused `AppEvent` variants — investigate before deleting, they may be reserved for future use
