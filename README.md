# docker-registry-walk

An interactive TUI (terminal UI) browser for Docker registries, written in Rust.

## Features

- Browse repositories and tags in any Docker Registry v2-compatible registry
- View image details: digest, created timestamp, OS/arch, layer sizes, total size
- Copy image pull URL to clipboard (`<registry>/<repo>:<tag>`)
- Image maintenance operations:
  - **Delete** — remove a tag by digest
  - **Copy** — cross-registry/repo image copy
  - **Retag** — create a new tag from an existing manifest
  - **Prune** — bulk-delete untagged manifests
  - **Inspect** — view raw manifest and config JSON
  - **Export** — save image as a tar archive
  - **Diff** — compare layers between two tags
- Multi-registry support with in-app switching
- Per-registry credentials stored in the OS keychain

## Build

```sh
# Prerequisites: Rust stable toolchain
cargo build --release
# Binary at target/release/docker-registry-walk
```

### Linux system dependencies

```sh
sudo apt-get install \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libssl-dev libdbus-1-dev pkg-config
```

## Install

```sh
cargo install --git https://github.com/pgmac-net/docker-registry-walk
```

## Configuration

Config file location:
- Linux/macOS: `~/.config/docker-registry-walk/config.toml`
- Windows: `%APPDATA%\docker-registry-walk\config.toml`

```toml
default_registry = "local"

[[registry]]
name = "local"
url = "https://registry.example.com"
username = "admin"
# password stored in OS keychain under "docker-registry-walk/local"
```

## Keybindings

| Key | Action |
|-----|--------|
| `↑↓` | Navigate list |
| `Tab` | Switch panel |
| `Enter` | Select |
| `c` | Copy pull URL to clipboard |
| `d` | Delete tag |
| `C` | Copy image to another registry/repo |
| `r` | Retag |
| `P` | Prune untagged manifests |
| `i` | Inspect manifest/config |
| `R` | Switch registry |
| `/` | Search/filter |
| `Esc` | Clear filter / close modal |
| `q` | Quit |

## Registry requirements

- Docker Registry API v2
- Delete must be enabled (`REGISTRY_STORAGE_DELETE_ENABLED=true`)
- Auth: anonymous, basic, or bearer token supported
