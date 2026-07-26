# Architecture

Five top-level modules wired together in `src/main.rs`:

| Module | Purpose |
|--------|---------|
| `config` | TOML config (`Config`, `RegistryProfile`, `RegistryType`). Created at `~/.config/docker-registry-walk/config.toml` on first run. |
| `registry` | Docker Registry v2 async HTTP client + auth. |
| `tui` | Terminal UI (ratatui + crossterm). Owns the event loop. |
| `ops` | Image maintenance operations called from TUI actions. |
| `clipboard` | Clipboard write via arboard. |

## registry/

- `client.rs` — `RegistryClient` (cheaply-cloneable via `Arc<dyn Credentials>`). `Credentials` trait has two methods: `get_authorization` (cached/global token) and `get_authorization_for_challenge` (per-endpoint scoped token, used to handle Docker Hub 401 re-challenges without polluting the cache). `artifactory_repositories()` lists Docker repo-keys on a JFrog Artifactory instance (`GET /api/repositories`); `for_artifactory_repo(key)` returns a client scoped to `<base>/api/docker/<key>/`, ready to browse like any other registry.
- `auth.rs` — `BasicCredentials` (used for `RegistryType::Artifactory`), `BearerCredentials` (with TTL token cache, used for `Standard`/`DockerHub`), `KeyringStore` (OS keychain + `secret-tool` fallback), `resolve_password` (provided → keyring → interactive prompt).
- `types.rs` — API response structs (`Manifest`, `ManifestIndex`, `TagList`, `Catalog`, `ArtifactoryRepo`, etc.).
- `search.rs` — Docker Hub Hub search API (used instead of `/v2/_catalog` for `RegistryType::DockerHub`).
- `pagination.rs` — RFC 5988 `Link` header parser for paginated catalog/tag responses.

## tui/

- `app.rs` — All mutable UI state: `Focus` (Repos/Tags/Detail), `LoadState`, `SortOrder`, modal structs (`InspectModal`, `LayerDiffModal`), status message with TTL. `Modal::ArtifactoryPicker` is the repo-key picker shown after switching to an Artifactory profile (one-shot fetch, filtered locally — unlike `SearchPicker`'s incremental server-side Docker Hub search).
- `event.rs` — Async event loop; dispatches crossterm key events to state mutations and spawns async ops tasks.
- `ui.rs` — Pure render: reads `App` state and draws to the ratatui `Frame`.
- `detail.rs` — `ImageDetail` (parsed tag metadata shown in the Detail panel).

## ops/

One file per image operation (`copy`, `delete`, `diff`, `export`, `inspect`, `prune`, `retag`). Each is called from the TUI event handler and operates directly on `RegistryClient`.
