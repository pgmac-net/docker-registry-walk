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
- GitHub Container Registry support: GHCR serves no `/v2/_catalog`, so packages are listed from the GitHub packages API (yours, or any user's or organisation's) and offered in a filterable picker
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

# GitHub Container Registry. GHCR has no /v2/_catalog, so the repository list
# comes from the GitHub packages API — which needs a personal access token
# with the `read:packages` scope. `owner` is optional: omit it to list your
# own packages, or name any user or organisation to list theirs.
[[registry]]
name = "ghcr"
url = "https://ghcr.io"
type = "ghcr"
owner = "pgmac-net"

# AWS ECR. An ECR registry is one AWS account in one region, and its hostname
# (<account-id>.dkr.ecr.<region>.amazonaws.com) is derived at runtime — so
# there is no `url` to write. Credentials come from the ordinary AWS chain,
# never the keychain. Both `aws_profile` and `region` can be switched from
# inside the TUI with `u` / Backspace.
[[registry]]
name = "ecr"
type = "ecr"
aws_profile = "default"
region = "ap-southeast-2"

# ECR Public. One global registry rather than one per region, so `region`
# does not apply.
[[registry]]
name = "ecr-public"
type = "ecr-public"
```

Per-profile keys:

| Key | Description |
|-----|-------------|
| `name` | Profile name, shown in the title bar and used as the keychain key. Must not contain `#` |
| `url` | Registry URL. For `type = "artifactory"`, the Artifactory server base rather than a `/v2/` root. Optional — and normally omitted — for the ECR types, which derive it from AWS |
| `username` | Optional. Not needed for `auth = "token"` or the ECR types |
| `type` | `standard` (default), `dockerhub`, `artifactory`, `ghcr`, `ecr`, or `ecr-public` |
| `auth` | `auto` (default), `basic`, `bearer`, or `token` |
| `owner` | `ghcr` only. User or organisation whose packages to list. Omit for your own |
| `aws_profile` | ECR only. AWS named profile to resolve credentials from. Omit to use the AWS chain (`$AWS_PROFILE`, then `default`) |
| `region` | `ecr` only. AWS region whose registry to browse. Omit to use the chain (`$AWS_REGION`, then the profile's region) |

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

1. the environment, from the variables that belong to this registry type:
   - `type = "ghcr"` → `$CR_PAT`, `$GITHUB_TOKEN`, `$GH_TOKEN`
   - everything else → `$JFROG_ACCESS_TOKEN`, `$ARTIFACTORY_ACCESS_TOKEN`
2. the OS keychain, under the account `__token__`
3. a masked prompt in the TUI, after an authentication failure

The environment wins so a token can be overridden for a single run without disturbing what is stored — and a token that came from the environment is deliberately *not* written to the keychain.

Those two sets never cross-read. A JFrog token exported in your shell can never change how you authenticate to Docker Hub or GHCR, and a `GITHUB_TOKEN` exported for `gh` — which is set on most developer machines — can never become an Artifactory credential.

`auth = "token"` works for any registry type, though how the token is *presented* differs: Artifactory receives it directly as `Authorization: Bearer <token>`, whereas GHCR exchanges it for a repository-scoped token (see [docs/ghcr-registry-browsing.md](docs/ghcr-registry-browsing.md)).

The ECR types are the exception to all of the above: they read no token environment variables and store nothing in the keychain, because their registry password is minted by AWS and expires in about twelve hours. `--token` and `--password` do not apply to them, and an ECR failure reports an AWS problem rather than prompting for a credential. See [docs/ecr-registry-browsing.md](docs/ecr-registry-browsing.md).

Leaving `auth` unset (`auto`) preserves the previous behaviour exactly: an Artifactory profile with a username and a stored password still uses HTTP Basic, and only falls back to a token when there is no password. Set `auth = "token"` explicitly to prefer the token when both exist.

See [docs/artifactory-authentication.md](docs/artifactory-authentication.md) for how this interacts with Artifactory's two authenticated surfaces.

## CLI options

| Flag | Description |
|------|-------------|
| `--registry <name>` | Open this named profile from the config on startup |
| `--url <url>` | Ad-hoc registry URL (creates a temporary "cli" profile) |
| `--username <user>` | Username for the ad-hoc registry |
| `--type <type>` | Registry flavour for the ad-hoc registry: `standard`, `dockerhub`, `artifactory`, `ghcr` |
| `--auth <mode>` | Auth mode for the ad-hoc registry: `auto`, `basic`, `bearer`, `token` |
| `--owner <owner>` | GHCR only: user or organisation whose packages to list. Omit for your own |
| `--password` | Prompt (masked) for the password and store it in the OS keychain — never to the config file, never as a CLI argument |
| `--token` | Prompt (masked) for an access token and store it in the OS keychain, same guarantees as `--password` |

## Keybindings

Press `?` for the keybindings help pane. It's **contextual**: it shows only the
keys for whatever is on screen — the focused panel, a picker, the JSON viewer —
rather than one long list. `?` reaches it from everywhere except a text prompt,
where `?` stays a normal character you can type. Closing help (`?`/`q`/`Esc`)
returns you to exactly where you were, filter text and selection included.

Full details of every context: [docs/keybindings.md](docs/keybindings.md).

### Navigation

| Key | Action |
|-----|--------|
| `↑` / `k` | Move up (or scroll, in the Detail panel) |
| `↓` / `j` | Move down (or scroll, in the Detail panel) |
| `Tab` / `→` | Next panel (Repos → Tags → Detail) |
| `Shift-Tab` / `←` | Previous panel |
| `Enter` | Move to Tags (Repos) / inspect the selected tag (Tags) |

### Filter

| Key | Action |
|-----|--------|
| `/` | Start filter in the current panel |
| `Esc` | Clear filter and exit |
| `Enter` / `Tab` | Keep filter and exit |

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
| `Home` / `g` | Jump to top |
| `End` / `G` | Jump to bottom |
| `Space` / `Enter` | Fold / unfold the node at the cursor |
| `←` / `h` | Collapse the node at the cursor |
| `→` / `l` | Expand the node at the cursor |
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
| `Backspace` / `u` | Up a level: the repo-key picker (Artifactory) or the owner picker (GHCR) |

### General

| Key | Action |
|-----|--------|
| `?` | Keybindings help |
| `q` / `Esc` | Quit |
| `Ctrl-C` | Force quit |

## Registry requirements

- Docker Registry API v2 (`/v2/` endpoint)
- For **delete / prune**: `REGISTRY_STORAGE_DELETE_ENABLED=true`
- Auth: anonymous, HTTP Basic, Bearer token (automatic token exchange with per-endpoint scope retry), or a static access token sent as `Authorization: Bearer`
- HTTPS strongly recommended; plain HTTP supported for local/internal registries

## License

MIT © 2026 [pgmac](https://github.com/pgmac). See [LICENSE](LICENSE).
