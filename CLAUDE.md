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
- **Credential selection.** `RegistryProfile::auth_kind(has_token, has_password)` is the single source of truth for which credential to build; it is pure and its whole matrix is unit-tested in `config.rs`. `make_client_for_profile` only does the impure part (keyring/env lookups). Don't reintroduce credential branching in `event.rs`. `auth = "auto"` must stay behaviour-identical to the pre-`auth` code: Basic wins for Artifactory when a username and password exist, even if a token is also available.
- **Artifactory auth.** `RegistryType::Artifactory` profiles never use `BearerCredentials` (it silently sends no `Authorization` header when no `Bearer` challenge is present). They use either `BasicCredentials` — Artifactory's Docker v2 endpoint and REST API both accept plain HTTP Basic (username + API key/identity token) — or `AccessTokenCredentials` for `auth = "token"`.
- **Access token statelessness.** `AccessTokenCredentials::get_authorization` returns `Bearer <raw token>` unconditionally and must stay stateless. `for_artifactory_repo` shares one `Arc<dyn Credentials>` between the REST client (`/api/repositories`) and the repo-key client (`/api/docker/<key>/v2/…`), which have different auth verifiers; the raw token is the only value valid at both. So `get_authorization_for_challenge` must never write state that `get_authorization` reads — caching a repo-scoped token globally reproduces the Docker Hub scope cascade by another route. Only *which realm presentation worked* is remembered.
- **Two realm guards, on purpose — plus a third comparison at a lower layer.** `is_trusted_realm` (Basic/Bearer) allows a realm sharing the last two DNS labels, because Docker Hub splits `auth.docker.io` from `registry-1.docker.io`. `is_same_origin_realm` (access token only) requires an exact host *and* port, reusing `client::same_host_and_port` but layering its own https-or-loopback scheme rule on top. Don't merge them: an access token is a platform-wide JFrog credential, and the looser rule would trust every host under the registry's registered domain. Separately, `RegistryClient::send` uses `client::same_origin` (scheme + host + port, no https requirement) to decide whether to attach `Authorization` to a request at all — e.g. a server-supplied blob-upload `Location` that may point at S3/GCS/Azure. That check must accept whatever scheme the user configured (including plain `http`), so it cannot reuse `is_same_origin_realm` either; it mirrors reqwest's own cross-origin redirect check instead.
- **`#` is reserved in profile names.** The TUI's client cache keys Artifactory repo-key clients as `<profile>#<repo-key>`; `Config::validate` rejects `#` in names to keep that unambiguous. After a credential change, rebuild *all* of a profile's entries (`rebuild_clients_for_profile`) — scoped clients hold their own credentials `Arc` and would otherwise keep the stale one.
- **Secret input is masked by action.** `InputAction::is_secret()` drives masking in `draw_input_modal`; add any new credential-collecting variant to it rather than adding a flag to `Modal::Input`. Credentials crossing `AppEvent` go in `Secret`, whose `Debug` redacts.
- **The silent credential re-read runs for credentialed clients too — deliberately.** On a first catalog auth failure, `ReposError` re-reads the keyring and retries once before prompting (`CatalogAttempt::AfterReread`), gated only on the profile having *some* credential source (`auth_prompt_for(&profile).is_some()`), **not** on whether the cached client already held a credential. Skipping it for credentialed clients looks like a free optimisation — it saves one guaranteed-useless round trip when the stored credential is simply wrong — but it cannot distinguish that from a credential that was *corrected out of band* since the client was built (a rotated or renewed token, written by `jf`, CI, or a second run of `--token`). That second case is exactly what the re-read exists to rescue silently, so skipping it would reintroduce "retype a credential the keyring already holds". A wasted request on a path that is about to show a modal anyway is the cheaper of the two. Investigated and rejected in issue #102; don't re-add without a way to compare the *old and new secret values*, not just their presence.
- **Panic hook.** `main.rs` registers a panic hook that disables raw mode and leaves the alternate screen before printing the panic message. Any new code that allocates terminal state must be safe to drop in this path.

## Release process

Push a tag `v<major>.<minor>.<patch>` (or `-rc<N>` for pre-release) — triggers `.github/workflows/release.yml`, builds four platform binaries. CI on PRs runs clippy/test/release-build across Linux/macOS/Windows (Windows `allow_failure: true`).

<!-- SPECKIT START -->
For additional context about technologies to be used, project structure,
shell commands, and other important information, read the current plan
at `specs/001-codebase-refactor/plan.md`.
<!-- SPECKIT END -->
