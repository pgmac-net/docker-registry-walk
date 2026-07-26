# CLAUDE.md

Rust/ratatui TUI for browsing and managing images across Docker Registry v2, Docker Hub, and JFrog Artifactory. Module map: `docs/ARCHITECTURE.md`.

## Commands

```sh
cargo build --release
cargo test <module>::tests     # e.g. registry::auth::tests
cargo clippy -- -D warnings    # must pass with zero warnings
cargo fmt --check              # used in CI
```

Linux build deps (clipboard, keyring, TLS):
```sh
sudo apt-get install \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libssl-dev libdbus-1-dev pkg-config
```

## Key design invariants

- **Passwords never in config, never in argv.** `RegistryProfile` has no password field. Passwords go via `KeyringStore` (keyring crate → `secret-tool` fallback). The `--password` CLI flag is a no-value boolean — it prompts interactively (masked, via `registry::prompt_password`) rather than accepting the secret as a flag value, so it never lands in shell history or `ps` output.
- **Bearer token scoping.** `BearerCredentials` caches a global token (`get_authorization`) but does NOT cache per-endpoint scoped tokens returned from `get_authorization_for_challenge`. Mixing the two caused cascading 401s on Docker Hub — don't merge those code paths.
- **Docker Hub special case.** `RegistryProfile::is_dockerhub()` controls whether the TUI uses the Hub search API for repos. Auto-detected from URL for backward compatibility; can also be set with `type = "dockerhub"` in config.
- **URL path-prefix joining.** `RegistryClient` normalizes `base_url` to end with `/` and strips the leading `/` off every request path before joining, so registries mounted under a URL prefix (e.g. JFrog Artifactory's `/artifactory/api/docker/<repo-key>/v2/...`) keep their prefix. `Url::join` treats a leading-`/` path as absolute and silently replaces the base path otherwise — don't reintroduce leading-`/` joins against `base_url` directly.
- **Artifactory auth.** `RegistryType::Artifactory` profiles use `BasicCredentials`, not `BearerCredentials` — Artifactory's Docker v2 endpoint and REST API authenticate via plain HTTP Basic (username + API key/identity token), and `BearerCredentials` silently sends no `Authorization` header when no `Bearer` challenge is present.
- **Panic hook.** `main.rs` registers a panic hook that disables raw mode and leaves the alternate screen before printing the panic message. Any new code that allocates terminal state must be safe to drop in this path.

## Release process

Push a tag `v<major>.<minor>.<patch>` (or `-rc<N>` for pre-release) — triggers `.github/workflows/release.yml`, builds four platform binaries. CI on PRs runs clippy/test/release-build across Linux/macOS/Windows (Windows `allow_failure: true`).

<!-- SPECKIT START -->
For additional context about technologies to be used, project structure,
shell commands, and other important information, read the current plan
at `specs/001-codebase-refactor/plan.md`.
<!-- SPECKIT END -->
