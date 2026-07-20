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
- JFrog Artifactory support: browse the Docker repo-keys hosted on an Artifactory instance via a picker, with credentials sent as HTTP Basic (username + API key/identity token)
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
```

### Credentials / keyring

Passwords are **never** written to the config file. They are stored in the OS keychain (macOS Keychain, GNOME Secret Service, Windows Credential Manager) under the key `docker-registry-walk/<registry-name>`.

Store a password on first use — `--password` takes no value; it prompts interactively with masked input, so the password never appears in your shell history or process list:

```sh
docker-registry-walk --registry local --password
```

Or add a registry on the fly without a config entry:

```sh
docker-registry-walk --url https://registry.example.com --username admin --password
```

The prompted password is written to the keychain; subsequent runs read from there automatically.

## CLI options

| Flag | Description |
|------|-------------|
| `--registry <name>` | Open this named profile from the config on startup |
| `--url <url>` | Ad-hoc registry URL (creates a temporary "cli" profile) |
| `--username <user>` | Username for the ad-hoc registry |
| `--password` | Prompt (masked) for the password and store it in the OS keychain — never to the config file, never as a CLI argument |

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
- Auth: anonymous, HTTP Basic, or Bearer token (automatic token exchange with per-endpoint scope retry)
- HTTPS strongly recommended; plain HTTP supported for local/internal registries

## License

MIT © 2026 [pgmac](https://github.com/pgmac). See [LICENSE](LICENSE).
