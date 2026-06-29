# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```sh
cargo build                    # debug build
cargo build --release          # release build
cargo test                     # run all tests
cargo test <module>::tests     # run tests for a specific module (e.g. registry::auth::tests)
cargo clippy -- -D warnings    # lint (must pass with zero warnings)
cargo fmt                      # format in place
cargo fmt --check              # format check (used in CI)
```

Linux system dependencies required to build (needed for clipboard, keyring, TLS):

```sh
sudo apt-get install \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libssl-dev libdbus-1-dev pkg-config
```

## Architecture

Five top-level modules wired together in `src/main.rs`:

| Module | Purpose |
|--------|---------|
| `config` | TOML config (`Config`, `RegistryProfile`, `RegistryType`). Created at `~/.config/docker-registry-walk/config.toml` on first run. |
| `registry` | Docker Registry v2 async HTTP client + auth. |
| `tui` | Terminal UI (ratatui + crossterm). Owns the event loop. |
| `ops` | Image maintenance operations called from TUI actions. |
| `clipboard` | Clipboard write via arboard. |

### registry/

- `client.rs` — `RegistryClient` (cheaply-cloneable via `Arc<dyn Credentials>`). `Credentials` trait has two methods: `get_authorization` (cached/global token) and `get_authorization_for_challenge` (per-endpoint scoped token, used to handle Docker Hub 401 re-challenges without polluting the cache).
- `auth.rs` — `BasicCredentials`, `BearerCredentials` (with TTL token cache), `KeyringStore` (OS keychain + `secret-tool` fallback), `resolve_password` (provided → keyring → interactive prompt).
- `types.rs` — API response structs (`Manifest`, `ManifestIndex`, `TagList`, `Catalog`, etc.).
- `search.rs` — Docker Hub Hub search API (used instead of `/v2/_catalog` for `RegistryType::DockerHub`).
- `pagination.rs` — RFC 5988 `Link` header parser for paginated catalog/tag responses.

### tui/

- `app.rs` — All mutable UI state: `Focus` (Repos/Tags/Detail), `LoadState`, `SortOrder`, modal structs (`InspectModal`, `LayerDiffModal`), status message with TTL.
- `event.rs` — Async event loop; dispatches crossterm key events to state mutations and spawns async ops tasks.
- `ui.rs` — Pure render: reads `App` state and draws to the ratatui `Frame`.
- `detail.rs` — `ImageDetail` (parsed tag metadata shown in the Detail panel).

### ops/

One file per image operation (`copy`, `delete`, `diff`, `export`, `inspect`, `prune`, `retag`). Each is called from the TUI event handler and operates directly on `RegistryClient`.

## Key design invariants

- **Passwords never in config.** `RegistryProfile` has no password field. Passwords go via `KeyringStore` (keyring crate → `secret-tool` fallback). The `--password` CLI flag writes to keychain then exits.
- **Bearer token scoping.** `BearerCredentials` caches a global token (`get_authorization`) but does NOT cache per-endpoint scoped tokens returned from `get_authorization_for_challenge`. Mixing the two caused cascading 401s on Docker Hub — don't merge those code paths.
- **Docker Hub special case.** `RegistryProfile::is_dockerhub()` controls whether the TUI uses the Hub search API for repos. Auto-detected from URL for backward compatibility; can also be set with `type = "dockerhub"` in config.
- **Panic hook.** `main.rs` registers a panic hook that disables raw mode and leaves the alternate screen before printing the panic message. Any new code that allocates terminal state must be safe to drop in this path.

## Release process

- Stable release: push a tag matching `v<major>.<minor>.<patch>` — triggers `.github/workflows/release.yml`, builds four platform binaries, creates a GitHub release.
- Pre-release: tag `v<major>.<minor>.<patch>-rc<N>` — same flow, marked as pre-release.
- CI on PRs: runs `clippy`, `cargo test`, and `cargo build --release` across Linux, macOS, and Windows (Windows is `allow_failure: true`).

<!-- SPECKIT START -->
For additional context about technologies to be used, project structure,
shell commands, and other important information, read the current plan
at `specs/001-codebase-refactor/plan.md`.
<!-- SPECKIT END -->
