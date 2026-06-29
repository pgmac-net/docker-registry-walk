# Feature Specification: Codebase Refactor

**Feature Branch**: `001-codebase-refactor`

**Created**: 2026-06-27

**Status**: Draft

**Input**: User description: "I want to refactor the code to make it easier to read, maintain and extend in the future."

## Context

The application grew through several organic iterations and is now fully functional.
This refactor is proactive hygiene — not fixing a regression, but making the
architecture real before future features are added on top of it.

The primary target is `src/tui/mod.rs` (1113 lines). It currently owns terminal
setup, the full async event loop, all key dispatch, modal logic, data loading task
spawning, and registry switching. The `CLAUDE.md` architecture description is
aspirational: it says `event.rs` owns the async event loop, but `event.rs` is
actually only 122 lines (event types + crossterm reader spawn). The refactor makes
the described architecture real.

No other module requires structural change: `ops/` (7 focused files, 30–95 lines
each) and `registry/` are already well-structured.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Developer Orientation (Priority: P1)

A developer new to the project (or returning after a long absence) must be able
to open the `tui/` module, understand which file owns which concern, and make a
targeted change without reading the entire event loop.

**Why this priority**: Today a developer must read 1113 lines of `mod.rs` to
understand any part of TUI behaviour. Splitting concerns into named files delivers
immediate, measurable navigability.

**Independent Test**: A developer can determine where to add a new key binding,
where to modify a state transition, and where to wire a new async operation —
all by reading module-level file names and public interfaces, without reading
implementation bodies.

**Acceptance Scenarios**:

1. **Given** a developer needs to add a key binding, **When** they open `tui/`,
   **Then** `event.rs` is the obvious and correct file to edit.
2. **Given** a developer needs to change how a UI state field is updated, **When**
   they open `tui/`, **Then** `app.rs` is the obvious and correct file to edit.
3. **Given** a developer reads `tui/mod.rs`, **When** they finish reading it,
   **Then** it is ≤ 80 lines covering only terminal setup, channel creation,
   and delegating to the event loop.

---

### User Story 2 - Safe Feature Extension (Priority: P2)

A developer must be able to add a new image operation or key binding without
navigating a 1000-line event loop to find the right insertion point.

**Why this priority**: Every new operation currently requires finding and editing
the correct location inside `tui/mod.rs`'s monolithic dispatch block. Explicit
structure makes extension obvious and low-risk.

**Independent Test**: Adding a stub new image operation (no real logic) requires
changes to at most 3 files — the new `ops/` file, `ops/mod.rs`, and one clearly
marked dispatch point in `event.rs` — with no modifications to existing operation
files or `tui/mod.rs`.

**Acceptance Scenarios**:

1. **Given** a developer wants to wire a new image operation, **When** they read
   `event.rs`, **Then** all existing operation dispatch is grouped in one clearly
   identifiable location.
2. **Given** a developer adds a new auth credential type, **When** they implement
   the `Credentials` trait, **Then** no existing credential files need modification.
3. **Given** a developer adds a new feature module, **When** they integrate it,
   **Then** the integration point is a single location in `event.rs`.

---

### User Story 3 - Confident Maintenance (Priority: P3)

A developer fixing a TUI bug must be able to reproduce it with a targeted unit
test, apply the fix to one `App` method, and verify correctness without spinning
up a terminal.

**Why this priority**: Currently all TUI state mutation logic lives inline in the
event loop. There are zero tests for any TUI behaviour. Isolating state mutations
into `App` methods makes them unit-testable for the first time.

**Independent Test**: A reported TUI state bug can be reproduced by calling an
`App` method in a test, the fix applied to that method, and verified by the test
passing — with no crossterm or tokio runtime required.

**Acceptance Scenarios**:

1. **Given** a bug in focus navigation, **When** a developer writes a test calling
   `app.handle_key(KeyCode::Tab)`, **Then** the incorrect state is reproduced and
   the fix verified without a terminal.
2. **Given** a developer makes a fix to an `App` method, **When** they run
   `cargo test`, **Then** the fix is verified and no regressions in other
   `App` methods are introduced.
3. **Given** an async task result arrives (e.g., repo list loaded), **When** the
   event loop calls `app.handle_result(result)`, **Then** state is updated
   correctly and the method is independently testable.

---

### Edge Cases

- A refactoring step moves logic between files: all tests still pass and the
  moved code behaves identically at runtime.
- A refactoring reveals a latent bug: the bug is deferred (filed separately)
  unless it blocks the structural change.
- An `App` method requires spawning an async task: it returns a command/intent
  value; the event loop (not `App`) does the actual spawn.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: All existing user-visible features MUST continue to work after
  refactoring (zero functional regression).
- **FR-002**: Each file in `tui/` MUST have a single, clearly bounded
  responsibility matching the architecture described in `CLAUDE.md`.
- **FR-003**: `tui/mod.rs` MUST be ≤ 80 lines after the refactor, containing
  only terminal setup, channel creation, and entry-point delegation.
- **FR-004**: `event.rs` MUST own the `tokio::select!` loop over (1) crossterm
  key events, (2) async task result receiver, and (3) tick timer. It is the sole
  location for spawning async tasks.
- **FR-005**: `App` MUST expose all state mutations as pure, synchronous methods
  (e.g., `handle_key`, `handle_result`, `handle_tick`). `App` MUST NOT depend on
  crossterm, tokio, or any async runtime.
- **FR-006**: Targeted unit tests MUST be added for `App` state mutation methods.
  Tests MUST NOT require a terminal or tokio runtime to run.
- **FR-007**: All pre-existing tests MUST continue to pass after every
  refactoring step; no test is deleted to make the suite green.
- **FR-008**: Public interfaces between modules MUST be minimal — only expose
  what callers need; internal types MUST NOT leak across module boundaries.
- **FR-009**: Dead code, unused imports, and unreachable branches MUST be removed
  during refactoring.
- **FR-010**: Error propagation MUST be consistent — errors MUST surface to the
  appropriate layer rather than being silently swallowed.

### Key Entities

- **Module**: A top-level code unit with a single responsibility, a defined public
  interface, and internal implementation hidden from callers.
- **App**: The pure-state struct in `app.rs` that owns all mutable TUI state and
  exposes sync methods for state mutation. No async dependencies.
- **Event loop**: The `tokio::select!` loop in `event.rs` that dispatches key
  events and task results to `App` methods, and spawns async tasks.
- **Task result**: An async image operation outcome piped back to the event loop
  via `mpsc` channel. The event loop — not `App` — owns the channel.
- **Operation**: A self-contained image maintenance action in `ops/` (copy,
  delete, diff, export, inspect, prune, retag); each follows the same structural
  pattern and is called from the event loop.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A developer can identify the correct file to modify for any TUI
  concern (key binding, state mutation, async dispatch, render) within 5 minutes
  of reading the `tui/` directory.
- **SC-002**: Adding a new image operation requires changes to at most 3 files
  (`ops/newop.rs`, `ops/mod.rs`, one dispatch location in `event.rs`), none of
  which are existing operation files.
- **SC-003**: 100% of pre-existing tests pass after every refactoring step; no
  test is deleted to make the suite green.
- **SC-004**: `tui/mod.rs` is ≤ 80 lines at completion of the refactor.
- **SC-005**: No new lint warnings or dead-code warnings are introduced; the
  existing zero-warning standard (`cargo clippy -- -D warnings`) is maintained
  throughout.
- **SC-006**: `event.rs` contains all `tokio::select!` event dispatch and all
  async task spawning; no async spawn calls exist in `tui/mod.rs` or `app.rs`.

## Assumptions

- The refactor is purely internal — no public CLI flags, config file keys, or
  keychain entry formats change.
- Targeted `App` method unit tests are in scope and expected as part of this
  refactor. Tests verify state transitions without a terminal or async runtime.
- Performance characteristics are not expected to change materially; the refactor
  optimises for clarity, not throughput.
- The refactor is delivered incrementally (one concern at a time): first extract
  `App` methods, then move the event loop to `event.rs`, then thin `mod.rs`.
- Latent bugs discovered during refactoring are deferred unless they block the
  structural change.
- Windows support remains `allow_failure: true` in CI — the refactor does not
  alter that policy.
