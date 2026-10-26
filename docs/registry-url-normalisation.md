# Registry URL normalisation (#148)

## Problem

`--url registry.example.com` (no scheme) connected to `localhost:5000`. The URL
failed parsing, and `make_client_for_profile_at` silently substituted
`http://localhost:5000`. The ad-hoc `cli` profile is added after
`Config::validate` runs, so nothing caught it earlier.

## Decisions

1. A URL with no `://` gets `https://` prepended (docker CLI convention). Plain
   HTTP needs an explicit `http://`.
2. Applies to `--url` and to `url` in `config.toml`. Schemeless config URLs
   previously made the whole config get ignored with a warning.
3. A `--url` that is still unparseable exits non-zero before the TUI starts.
4. The localhost fallback is gone; the unreachable parse-failure path in
   `make_client_for_profile_at` now returns `not_connected_client()`
   (`not-connected.invalid`), so it fails visibly.

## Implementation

- `config::normalize_registry_url`, `config::checked_registry_url`
- `Config::parse` normalises each profile URL, then validates; `Config::load` uses it.
- `main.rs` runs `checked_registry_url` on `--url`.

## Not covered

A config profile already named `cli` collides with the ad-hoc profile's
client-cache key. Separate issue if wanted.
