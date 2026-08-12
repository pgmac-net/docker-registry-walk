# docker-registry-walk

An interactive TUI browser for Docker registries, written in Rust.

## Features

- Browse repositories and tags in any Docker Registry v2-compatible registry
- View image details: digest, created timestamp, OS/arch, layer sizes, total size
- Image maintenance operations:
  - **Copy** pull URL to clipboard
  - **Copy** image cross-registry or cross-repo
  - **Retag** — push manifest under a new tag name
  - **Delete** — remove tag by digest
  - **Prune** — bulk-delete digest-only (untagged) manifests
  - **Inspect** — view raw manifest and config JSON with syntax highlighting
  - **Export** — save image as an OCI-layout tar archive (skopeo-compatible)
  - **Diff** — compare layer sets between two tags
- Multi-registry support with in-app switching (`R`)
- JFrog Artifactory support: browse the Docker repo-keys hosted on an Artifactory instance via a picker, authenticating with either HTTP Basic (username + API key/identity token) or a JFrog access token sent as `Authorization: Bearer` — the same way the Terraform `jfrog/artifactory` provider authenticates
- Per-registry credentials stored in the OS keychain — never in the config file
- Live filter and sort within repos/tags panels
- In-app keybindings reference (`?`)

## Install

### Pre-built binaries

Download the binary for your platform from the [latest release](https://github.com/pgmac-net/docker-registry-walk/releases/latest), make it executable, and place it on your `PATH`.

```sh
# Linux example
chmod +x docker-registry-walk-linux-x86_64
sudo mv docker-registry-walk-linux-x86_64 /usr/local/bin/docker-registry-walk
```

### From source

```sh
# Prerequisites: Rust stable toolchain (https://rustup.rs)
cargo install --git https://github.com/pgmac-net/docker-registry-walk
```

#### Linux system dependencies

```sh
sudo apt-get install \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libssl-dev libdbus-1-dev pkg-config
```

## Configuration

Config file location:
- **Linux / macOS**: `~/.config/docker-registry-walk/config.toml`
- **Windows**: `%APPDATA%\docker-registry-walk\config.toml`

Created automatically with example content on first run.

```toml
# Registry to open on startup (optional — defaults to first entry).
default_registry = "local"

[[registry]]
name = "local"
url = "https://registry.example.com"
# username = "admin"   # uncomment if auth is required

[[registry]]
name = "staging"
url = "https://staging-registry.example.com"
username = "ci"

# JFrog Artifactory: one instance can host many Docker repo-keys, each its
# own browsable registry. `url` is the Artifactory server base (not a /v2/
# root) — after switching to this registry (`R`), pick which repo-key to
# browse from the picker that opens automatically.
[[registry]]
name = "artifactory"
url = "https://artifactory.example.com/artifactory"
username = "ci"
type = "artifactory"

# The same instance authenticated with a JFrog access token instead. No
# username needed — see "Access tokens" below.
[[registry]]
name = "artifactory-token"
url = "https://artifactory.example.com/artifactory"
type = "artifactory"
auth = "token"
```

Per-profile keys:

| Key | Description |
|-----|-------------|
| `name` | Profile name, shown in the title bar and used as the keychain key. Must not contain `#` |
| `url` | Registry URL. For `type = "artifactory"`, the Artifactory server base rather than a `/v2/` root |
| `username` | Optional. Not needed for `auth = "token"` |
| `type` | `standard` (default), `dockerhub`, or `artifactory` |
| `auth` | `auto` (default), `basic`, `bearer`, or `token` |

### Credentials / keyring

Secrets are **never** written to the config file, and never passed as a flag value. They are stored in the OS keychain (macOS Keychain, GNOME Secret Service, Windows Credential Manager) under the key `docker-registry-walk/<registry-name>`.

Store a password on first use — `--password` takes no value; it prompts interactively with masked input, so the password never appears in your shell history or process list:

```sh
docker-registry-walk --registry local --password
```

Or add a registry on the fly without a config entry:

```sh
docker-registry-walk --url https://registry.example.com --username admin --password
```

The prompted password is written to the keychain; subsequent runs read from there automatically.

### Access tokens

Set `auth = "token"` on a profile to authenticate with `Authorization: Bearer <token>` instead of a username and password — the same way the Terraform `jfrog/artifactory` provider authenticates. No username is required.

```sh
# Store the token in the keychain (prompts, masked — takes no value).
docker-registry-walk --registry artifactory --token

# Or supply it via the environment, as Terraform does.
JFROG_ACCESS_TOKEN=<token> docker-registry-walk --registry artifactory
```

The token is resolved in this order:

1. `$JFROG_ACCESS_TOKEN`
2. `$ARTIFACTORY_ACCESS_TOKEN`
3. the OS keychain, under the account `__token__`
4. a masked prompt in the TUI, after an authentication failure

The environment wins so a token can be overridden for a single run without disturbing what is stored — and a token that came from the environment is deliberately *not* written to the keychain.

`auth = "token"` works for any registry type (a GitHub / GitLab / Harbor personal access token is presented the same way); the `JFROG_*` / `ARTIFACTORY_*` variables are only consulted for `type = "artifactory"` profiles, so a JFrog token exported in your shell can never change how you authenticate to Docker Hub.

Leaving `auth` unset (`auto`) preserves the previous behaviour exactly: an Artifactory profile with a username and a stored password still uses HTTP Basic, and only falls back to a token when there is no password. Set `auth = "token"` explicitly to prefer the token when both exist.

See [docs/artifactory-authentication.md](docs/artifactory-authentication.md) for how this interacts with Artifactory's two authenticated surfaces.

## CLI options

| Flag | Description |
|------|-------------|
| `--registry <name>` | Open this named profile from the config on startup |
| `--url <url>` | Ad-hoc registry URL (creates a temporary "cli" profile) |
| `--username <user>` | Username for the ad-hoc registry |
| `--type <type>` | Registry flavour for the ad-hoc registry: `standard`, `dockerhub`, `artifactory` |
| `--auth <mode>` | Auth mode for the ad-hoc registry: `auto`, `basic`, `bearer`, `token` |
| `--password` | Prompt (masked) for the password and store it in the OS keychain — never to the config file, never as a CLI argument |
| `--token` | Prompt (masked) for an access token and store it in the OS keychain, same guarantees as `--password` |

## Keybindings

Press `?` inside the app for the full interactive reference. Summary:

### Navigation

| Key | Action |
|-----|--------|
| `↑` / `k` | Move up |
| `↓` / `j` | Move down |
| `Tab` | Cycle panel (Repos → Tags → Detail) |
| `Enter` | Move focus to Tags when in Repos |

### Filter

| Key | Action |
|-----|--------|
| `/` | Start filter in the current panel |
| `Esc` / `Enter` | Exit filter mode |

### Image operations (require a tag selected)

| Key | Action |
|-----|--------|
| `c` | Copy pull URL to clipboard |
| `C` | Copy image to another registry/repo |
| `r` | Retag |
| `d` | Delete tag |
| `i` | Inspect manifest & config JSON |
| `e` | Export as OCI tar archive |
| `D` | Diff layers against another tag |

### Inspect viewer (inside the JSON overlay)

| Key | Action |
|-----|--------|
| `↑` / `↓` / `k` / `j` | Move cursor |
| `PgUp` / `PgDn` | Page up / down |
| `g` / `G` | Jump to top / bottom |
| `Space` / `Enter` | Fold / unfold the node at the cursor |
| `←` / `→` | Collapse / expand the node at the cursor |
| `H` / `L` | Collapse all / expand all |
| `/` | Search JSON text (`Enter` to run, `Esc` to cancel) |
| `n` / `N` | Jump to next / previous match |
| `?` | Keybindings help (returns to the viewer on close) |
| `q` / `Esc` | Close the viewer |

Folding a node hides its whole subtree, so collapsing the root line folds the
entire document. Search auto-expands any folds hiding a match.

### Repository operations (require a repo selected)

| Key | Action |
|-----|--------|
| `P` | Prune digest-only (untagged) manifests |

### Tags panel

| Key | Action |
|-----|--------|
| `s` | Cycle sort order (↑ / ↓ name) |

### Registry

| Key | Action |
|-----|--------|
| `R` | Switch registry (in-app) |
| `Backspace` / `u` | Back to repo-key picker (Artifactory only) |

### General

| Key | Action |
|-----|--------|
| `?` | Keybindings help |
| `q` / `Ctrl-C` | Quit |

## Registry requirements

- Docker Registry API v2 (`/v2/` endpoint)
- For **delete / prune**: `REGISTRY_STORAGE_DELETE_ENABLED=true`
- Auth: anonymous, HTTP Basic, Bearer token (automatic token exchange with per-endpoint scope retry), or a static access token sent as `Authorization: Bearer`
- HTTPS strongly recommended; plain HTTP supported for local/internal registries

## License

MIT © 2026 [pgmac](https://github.com/pgmac). See [LICENSE](LICENSE).
