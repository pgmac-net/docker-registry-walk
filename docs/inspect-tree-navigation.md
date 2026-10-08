# Inspect viewer: tree navigation (issues #130, #134, #136)

## The problem

In the Inspect JSON viewer, pressing `←` on a line inside a nested element
collapsed the *enclosing* element in one press (`set_fold(true)` resolved
its target through `jsonview::opener_at`, which returns the enclosing opener
for a non-opener line). The reporter wanted the file-tree behaviour instead:
`←` goes back to the parent and leaves it open; a second `←` closes it. They
also wanted a quick way to reach the top and bottom of the items within an
element.

## Behaviour

| Key | Cursor on | Result |
|---|---|---|
| `←` / `h` | child line, closing bracket, or collapsed opener | move to the parent opener (parent stays open) |
| `←` / `h` | open opener | collapse it |
| `→` / `l` | collapsed opener | expand it |
| `→` / `l` | open opener | move to its first child |
| `→` / `l` | leaf | no-op |
| `[` | anywhere inside an element | first direct child of the element containing the cursor line |
| `]` | anywhere inside an element | last direct child (a block child is reported as its opener line) |

`Home`/`End`/`g`/`G` still jump to the top/bottom of the whole document, and
`Space`/`Enter` keeps its one-press "fold whatever I'm in" behaviour.

The "element" for `[`/`]` is always the *parent* block, even when the cursor
is on an opener line — they move among siblings and never dive in. At top
level (document root, blank line, `── config ──` separator) there is no
parent, so `←` on a collapsed root and `[`/`]` are no-ops. `[`/`]` never
change folds, and are typable characters inside the `/` search sub-mode.

## Decisions

- **New `[`/`]` keys rather than two-stage `Home`/`End`.** No existing binding
  changes meaning; both keys were free in the viewer.
- **Right mirrors Left.** Expand-then-step-in makes `←`/`→` a symmetric pair
  for walking the tree.
- **`Space`/`Enter` unchanged.** It stays the fast fold shortcut next to the
  careful `←` ladder; the ticket only names the left arrow.

## Implementation

- `src/tui/jsonview.rs` — pure `parent_of`, `first_child`, `last_child`
  (children are walked by skipping nested blocks via `close_idx`).
- `src/tui/app.rs` — `InspectModal::{step_out, step_in, jump_first_sibling,
  jump_last_sibling}`; `set_fold` removed (no remaining callers).
- `src/tui/event.rs` — key bindings; `src/tui/ui.rs` — Help rows.

## Verification

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test` (247 passed). New tests cover the full `←` ladder to the root,
closing-bracket lines, `→` expand/enter/leaf, sibling jumps (including from an
opener line and at top level), and non-JSON input. Not smoke-tested against a
live registry (no registry or interactive terminal in this environment).

## Process note

Picked up via `pgmac-workflows:pickup-ticket`. Plan posted and approved on the
ticket; rated STANDARD and implemented on Sonnet 5.5 as planned (planning ran
on Opus 5.5, the Fable 5 fallback). No deviations.

## Close children (`c`, issue #134)

`H` collapses the whole document, root included. `c` is the scoped variant:
it collapses every block *inside* the element containing the cursor and leaves
that element, and all its ancestors, open.

- **Element** = the block containing the cursor line (same rule as `[`/`]`).
  On a top-level opener with no parent (the manifest or config root) that open
  opener is the element; on the blank line, the `── config ──` separator, or a
  collapsed top-level opener it is a no-op.
- **Depth** = all descendants, not just direct children. Re-expanding a folded
  child therefore shows its own children folded too.
- **Cursor** never moves off its line: it is always a direct child (or bracket)
  of the element, so it cannot end up inside a newly folded block.
- Idempotent; already-folded blocks stay folded; no-op on input without blocks.
- Implementation: `InspectModal::collapse_children` (`src/tui/app.rs`), reusing
  `jsonview::parent_of`; bound to `c` in the Inspect arm of `src/tui/event.rs`,
  with a Help row in `src/tui/ui.rs`.

Deviation from plan: the plan proposed a helper to place the cursor on the
folded child hiding it. Implementing showed the cursor can never be hidden by
`c`, so the helper and its placement logic were dropped.

## Open children (`o`, issue #136)

`o` is the opposite of `c`: it expands every block inside the element
containing the cursor (all descendants), leaving the element and its ancestors
as they were. It is the scoped counterpart to `L` (expand the whole document),
as `c` is to `H`.

- Same element rule as `c`, `[` and `]`, including the top-level case: an open
  top-level opener is its own element; a *collapsed* top-level opener, the blank
  line and the separator are a no-op (use `→`/`Space` to open a collapsed root).
- Cursor never moves; idempotent; `c` then `o` restores the original view.
- Implementation: `c` and `o` share `InspectModal::set_children_collapsed`
  (`src/tui/app.rs`); `o` is bound in `src/tui/event.rs` with a Help row in
  `src/tui/ui.rs`.
