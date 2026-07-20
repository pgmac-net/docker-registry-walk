# Artifactory repo-key up-navigation (issue #71)

## The problem

Inside a Docker repo-key on a JFrog Artifactory instance, there was no
direct way to switch to another repo-key on the same instance. The only
path was `R` (the profile-level registry switcher) → re-select the same
Artifactory profile → wait on a fresh `/api/repositories` fetch — two
modals and a network round-trip to get back to a picker you'd already
seen once when you first descended into the repo-key.

## Design

Three interaction models were considered:

1. **Up-navigation (chosen)** — treat the repo-key picker as the parent
   level of a hierarchy; a "back" key from the Repos pane pops up to it.
   Matches the tool's existing "walk" metaphor (registry → repo-key →
   repo → tag) and keeps `R` purely profile-level.
2. **Dedicated hotkey** (e.g. Ctrl+R) — re-open the picker directly,
   no-op elsewhere. Rejected: Cmd+R isn't reliably reachable in
   terminals, and it doesn't fit the "walk" metaphor as naturally.
3. **Nested `R` picker** — expand Artifactory profiles inline in the
   existing `RegistrySelect` modal to show child repo-keys. Rejected:
   grows the modal and needs eager repo-list fetches just to render it.

`Backspace` and `u` are both bound to the same action (rather than
picking one) since both were free keys in normal mode and either reads
naturally as "go back".

The repo-key list is served from a per-profile cache for an instant
open, with a background refetch keeping it current — rather than either
always blocking on a fresh fetch, or never refreshing at all.

## Implementation

- `src/tui/app.rs`
  - New `artifactory_repo_cache: HashMap<String, Vec<ArtifactoryRepo>>`
    keyed by profile name. Populated by `on_artifactory_repos`, which now
    runs on *both* the original profile-switch fetch and the new
    background refresh.
  - New `current_artifactory_repo_key: Option<String>` tracks which
    repo-key is currently being browsed — set in `enter_artifactory_repo`,
    cleared in `reset_for_new_registry`.
  - New `open_artifactory_picker_cached()`: unlike `start_artifactory_switch`,
    this does **not** touch repo/tag/detail state, so `Esc` from the
    picker returns to browsing exactly as it was. It preselects the
    current repo-key if found in the cache.
  - `on_artifactory_repos` changed from unconditionally resetting
    `selected` to `0` on every fetch, to: prefer matching
    `current_artifactory_repo_key`, otherwise clamp the existing
    selection to the (possibly shorter) new list. This stops a background
    refresh from yanking the cursor out from under an open filter.
- `src/tui/event.rs`
  - New `AppEvent::OpenArtifactoryRepoPicker`, handled in the event loop
    the same way `SwitchRegistry` is, rather than inline in `handle_key`.
    Reason: `handle_key` only has access to the *scoped* client for the
    current repo-key, but listing repo-keys needs the *base* Artifactory
    client — which the event loop's `clients` map still holds under the
    plain profile name after descending into a repo-key.
  - `Backspace` / `u` added to the normal-mode key match, sending
    `OpenArtifactoryRepoPicker`. The event loop guards on
    `profile.is_artifactory()` before acting, making it a silent no-op on
    non-Artifactory registries.
- `src/tui/ui.rs`, `README.md` — help modal and key table updated.

## Verifying it

`cargo build`, `cargo clippy -- -D warnings`, `cargo test` (73 passed, 4
new), `cargo fmt --check` — all clean. New unit tests in `src/tui/app.rs`
cover: cache round-trip with current-key preselection, non-destructive
open (browsing state untouched), selection clamp when a background
refresh shrinks the list, and empty-cache fallback to the loading state.

Not manually smoke-tested against a live Artifactory instance — none was
available in this environment.

## Process note

Picked up via `pgmac-workflows:pickup-ticket` against
`pgmac-net/docker-registry-walk#71`. Design was brainstormed
interactively (interaction model, key binding, caching strategy) before
the plan was posted to the ticket and approved. Implementation proceeded
on the planning-model session rather than switching to the Sonnet tier
the approved plan called for (STANDARD complexity) — noted on the
ticket's work-started comment. No functional deviation from the plan.

PR: https://github.com/pgmac-net/docker-registry-walk/pull/72
