<!--
SYNC IMPACT REPORT
==================
Version change: [unversioned template] → 1.0.0
Modified principles: N/A (initial population)
Added sections: All sections populated from template placeholders
Removed sections: None
Templates requiring updates:
  ✅ .specify/templates/plan-template.md — Constitution Check section is generic; no updates needed
  ✅ .specify/templates/spec-template.md — No constitution-specific references; no updates needed
  ✅ .specify/templates/tasks-template.md — No constitution-specific references; no updates needed
  ✅ .specify/templates/checklist-template.md — No constitution-specific references; no updates needed
Follow-up TODOs: None — all placeholders resolved
-->

# docker-registry-walk Constitution

## Core Principles

### I. Credential Security (NON-NEGOTIABLE)

Passwords MUST never be stored in the config file. `RegistryProfile` MUST NOT contain
a password field. All credentials are stored exclusively in the OS keychain via the
`keyring` crate (with `secret-tool` as a Linux fallback). The `--password` CLI flag
writes to the keychain and exits immediately — it never persists credentials to disk.
Any new feature that involves authentication MUST route credential storage through
`KeyringStore`; bypassing this for convenience is not permitted.

**Rationale**: Config files are frequently committed to version control, shared across
environments, and backed up in plaintext. Centralising secrets in the OS keychain
eliminates the entire class of credential-leak vulnerabilities at the storage layer.

### II. Docker Registry v2 Protocol Compliance

The registry client MUST implement the Docker Registry HTTP API v2 specification.
Bearer token handling MUST distinguish global tokens (`get_authorization`) from
per-endpoint scoped tokens (`get_authorization_for_challenge`). Per-endpoint scoped
tokens MUST NOT be cached in the global token store — mixing the two paths caused
cascading 401s on Docker Hub and is explicitly forbidden. Docker Hub repository
listing MUST use the Hub search API, not `/v2/_catalog`, when `RegistryType::DockerHub`
is detected (auto-detected from URL or explicit `type = "dockerhub"` in config).

**Rationale**: Correct protocol compliance is the product's primary contract. Docker
Hub's non-standard auth model requires special-casing; conflating it with generic
registry auth produces silent failures that are hard to reproduce.

### III. Module Isolation

The codebase MUST maintain exactly five top-level modules: `config`, `registry`, `tui`,
`ops`, and `clipboard`. Each module owns a single responsibility:

- `config` — TOML configuration and profile types only
- `registry` — async HTTP client, auth, API response types, pagination
- `tui` — terminal UI state, event loop, and rendering (no business logic)
- `ops` — image operations (`copy`, `delete`, `diff`, `export`, `inspect`, `prune`, `retag`)
- `clipboard` — clipboard write abstraction

Business logic MUST live in `ops/`, not in `tui/`. `RegistryClient` MUST remain
cheaply cloneable via `Arc<dyn Credentials>` so async tasks can share it without
locking. Cross-module coupling MUST be minimized; prefer passing data over sharing state.

**Rationale**: Clear module boundaries make the codebase navigable, testable in
isolation, and safe to extend. Leaking business logic into the TUI layer makes
operations untestable without a running terminal.

### IV. Zero-Warning Quality

All code MUST pass `cargo clippy -- -D warnings` with zero warnings before merging.
Code MUST be formatted with `cargo fmt` (CI enforces `cargo fmt --check`). All CI
checks — lint, test, and release build — MUST pass on Linux and macOS. The panic hook
registered in `main.rs` MUST disable raw mode and leave the alternate screen before
printing; any new code that allocates terminal state MUST be safe to drop in that path.

**Rationale**: Warnings are defects-in-waiting. Treating them as errors in CI prevents
gradual quality erosion. Consistent formatting reduces review noise. A correct panic
hook is a safety invariant — a broken terminal is worse than a crash.

### V. Async-First I/O

All network I/O and file I/O MUST be async, using `tokio` as the runtime. Blocking
calls MUST NOT be made inside async contexts. The TUI event loop owns async dispatch;
long-running operations are spawned as tasks from the event handler and communicate
results back via channels. New I/O code MUST use `tokio::fs`, `reqwest` (async), or
equivalent async primitives.

**Rationale**: A blocking call on the async executor stalls the entire TUI event loop,
making the application unresponsive. Async-first is non-negotiable for interactive
terminal applications with concurrent network activity.

## Technology Stack

- **Language**: Rust (stable toolchain, edition 2024)
- **TUI framework**: ratatui + crossterm
- **Async runtime**: tokio (full features)
- **HTTP client**: reqwest (JSON + streaming features)
- **CLI parsing**: clap (derive feature)
- **Credentials**: keyring crate (OS keychain: macOS Keychain / GNOME Secret Service / Windows Credential Manager; `secret-tool` fallback on Linux)
- **Clipboard**: arboard
- **Serialization**: serde + serde_json
- **Configuration**: toml
- **Error handling**: anyhow (application), thiserror (library errors)
- **Target platforms**: Linux (primary), macOS, Windows (`allow_failure: true` in CI)

Linux system dependencies for development builds: `libxcb-render0-dev`,
`libxcb-shape0-dev`, `libxcb-xfixes0-dev`, `libxkbcommon-dev`, `libssl-dev`,
`libdbus-1-dev`, `pkg-config`.

## Release Process

Releases are fully automated via GitHub Actions (`.github/workflows/release.yml`):

- **Stable release**: push a tag matching `v<major>.<minor>.<patch>` — builds four
  platform binaries and creates a GitHub release.
- **Pre-release**: tag `v<major>.<minor>.<patch>-rc<N>` — same pipeline, marked as
  pre-release.
- **CI on PRs**: runs `clippy`, `cargo test`, and `cargo build --release` across Linux
  and macOS (Windows `allow_failure: true`).
- **Docker builds** in GitHub Actions use BuildKit deployed to the pvek8s cluster
  (TCP connection).
- Commits MUST NOT go directly to `main`. All changes require a branch and PR.
- The `main` branch MUST be up to date with remote before branching.

## Governance

This constitution supersedes all other development practices and informal conventions.
Amendments require: (1) a written rationale, (2) team approval via PR review, and
(3) a migration plan for any existing code that violates the new rule. Version
increments follow semantic versioning:

- **MAJOR**: backward-incompatible principle removal or redefinition
- **MINOR**: new principle or section added / materially expanded guidance
- **PATCH**: clarifications, wording fixes, non-semantic refinements

All PRs MUST verify compliance with the five Core Principles before merging.
Complexity violations MUST be documented in the plan's Complexity Tracking table
with explicit justification. Apply YAGNI — no abstractions beyond what the current
task requires. Runtime development guidance lives in `CLAUDE.md`.

**Version**: 1.0.0 | **Ratified**: 2026-06-24 | **Last Amended**: 2026-06-27
