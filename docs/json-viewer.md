# JSON viewer: fold, paging, search (issue #74)

## The problem

The Inspect overlay (`i`) shows a tag's raw manifest and config JSON. It
rendered as flat, syntax-highlighted text with a single scroll offset and
only `↑`/`↓` line-at-a-time movement. For real manifests — deep `config`
objects, long `layers`/`history` arrays — that meant a lot of scrolling
to find anything, no way to hide sections you don't care about, and no
text search.

The ticket asked for three things:

1. Collapse/expand child elements from the current layer (root fold
   affects everything; a nested node folds only its own subtree; with
   sibling entries, only the selected one folds).
2. Page-up / page-down for faster movement.
3. Text search within the content.

## Design

### Fold model — indentation, not a rebuilt JSON tree

The overlay already receives machine-formatted JSON (2-space indent,
deterministic from `serde_json::to_string_pretty`). Two options:

- **Re-model the document as a `serde_json::Value` tree** and render from
  that. Rejected: it throws away the existing `colorize_json_line`
  renderer and the manifest/config concatenation, and duplicates
  structure the pretty-printer already encoded in the text.
- **Derive folds from the text (chosen).** A line whose trimmed content
  ends in `{` or `[` opens a collapsible block; its matching closer is
  found by a brace-matching stack. Everything downstream — colouring,
  the `── config ──` separator, multiple concatenated documents — keeps
  working unchanged.

This is safe precisely because the input is machine-generated: it always
nests cleanly, so a closer always matches the innermost open block.

### Cursor + visible-set model

The modal now tracks a **cursor** (a selected line) in addition to
scroll. Folding operates on the node at (or enclosing) the cursor, which
gives the ticket's behaviour for free:

- Cursor on the root `{` → fold hides the entire document.
- Cursor on a nested opener → only that subtree folds.
- Cursor inside a leaf → the innermost enclosing node folds; sibling
  entries at the same level are untouched.

`cursor` and `scroll` index into a precomputed `visible` list (the subset
of line indices currently shown), so a collapsed node's interior simply
isn't in the list. Cursor movement, paging, and search all operate on
that list; toggling a fold rebuilds it while keeping the cursor on the
same absolute line.

### Search

`/` opens a one-line search entry (reusing the shared `InputState`).
`Enter` runs a case-insensitive substring match over every line and jumps
to the first hit; `n`/`N` cycle. A hit hidden inside a collapsed node
auto-expands its ancestors so it becomes visible. Matches are highlighted
in place; the title shows `(current/total)`.

### Help without losing your place

`?` opens the keybindings help over the viewer. Since only one modal is
active at a time, the open `InspectModal` is stashed in
`App.inspect_return` and restored when Help closes (`?`/`q`/`Esc`), so the
viewer comes back with its cursor, folds, and search intact rather than
dropping to the main list. (`?` is treated as a literal character while a
search query is being typed.)

## Implementation

- `src/tui/jsonview.rs` (new) — pure fold logic, fully unit-tested:
  `build_rows` (brace-matching → per-line `RowMeta`), `visible_lines`,
  `opener_at`, `expand_ancestors`, `find_matches`, `close_bracket`.
- `src/tui/app.rs` — `InspectModal` gained `rows`, `collapsed`,
  `visible`, `cursor`, `viewport_h`, and a `SearchState`, plus methods for
  cursor movement, paging, fold toggling, collapse/expand-all, and search.
  `InspectModal::new` builds the fold model up front.
- `src/tui/event.rs` — the Inspect key arm rewritten: cursor/paging keys,
  `Space`/`Enter`/`←`/`→`/`H`/`L` for folding, and a `/`-driven search
  sub-mode routing keys to the query input.
- `src/tui/ui.rs` — `draw_inspect_modal` now takes `&mut InspectModal`
  (drawn before the shared modal match) so it can record its viewport
  height each frame for paging. Renders fold gutters (`▸`/`▾`), a `⋯`
  marker on collapsed openers, cursor highlight, search-hit highlight,
  and a footer that shows the live search entry or a key hint. Help modal
  and README key tables updated.

### Edge cases

- Malformed JSON (the pretty-printer fell back to lossy text) has no
  openers, so the viewer degrades to a plain cursor/scroll/search view.
- Non-ASCII lines skip in-place search highlighting (byte offsets from a
  lowercased haystack could otherwise split a codepoint) — they still
  match and are navigable, just not colour-highlighted.
- Fold state lives for the open session only; re-opening Inspect rebuilds
  it fresh.

## Verifying it

`cargo build`, `cargo clippy --all-targets -- -D warnings`,
`cargo fmt --check`, `cargo test` — all clean, **89 passed** (16 new:
10 in `jsonview`, 6 `InspectModal` behaviour tests). The new tests cover
opener/closer matching, inline-empty containers as leaves, collapse
hiding exactly the interior + closer, root collapse, nested collapse
surviving a parent expand, `opener_at` resolution, ancestor expansion,
case-insensitive matching, cursor clamping, paging by viewport height,
fold keeping the cursor on the opener, collapse-all/expand-all round-trip,
search cycling/wrapping, and search auto-expanding folds. The render path
is exercised by the small-terminal no-panic test.

Not manually smoke-tested against a live registry — a TUI needs an
interactive terminal and a registry to inspect, neither available in this
environment; behaviour is covered by the logic and render tests above.

## Process note

Picked up via `pgmac-workflows:pickup-ticket` against
`pgmac-net/docker-registry-walk#74`. Plan posted to the ticket and
approved before implementation. Rated COMPLEX (new cursor/fold/visible
display model plus three interacting features across app/event/ui). The
plan called for Fable 5; Fable 5 was unavailable this session, so
implementation proceeded on Opus (the recorded fallback) — noted on the
work-started comment. No functional deviation from the plan.

PR: https://github.com/pgmac-net/docker-registry-walk/pull/76
