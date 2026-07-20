# Making popup modals resize with the terminal (issue #66)

## The bug

Every popup modal in `src/tui/ui.rs` (Confirm, Input, Switch Registry,
Help, Docker Hub search picker, Artifactory repo-key picker) computed its
`Rect` from a **fixed** column width — 50 to 70 columns depending on the
modal — centered in the terminal. The two picker modals additionally
capped their result list at a hard 10 rows.

Because that rect is recomputed every frame from the current terminal
size and clamped with `.min(area.width - 4)` / `.min(area.height - 4)`,
popups already *shrank* correctly on a small terminal. What none of them
did was *grow*: the main panes (Repos/Tags) are percentage-based
(`Constraint::Percentage`), so on a large terminal the panes fill the
screen while every popup stays pinned to its small design-minimum size.
Most noticeable on the registry/repo-key picker, which is also list
content the user actively needs to see more of — the case reported in
the ticket.

## The fix

Added one shared helper:

```rust
/// Centered popup rect. `width` scales with the terminal (`width_pct` of
/// `area.width`) but never shrinks below `min_width`, and both dimensions
/// are clamped to fit within `area` (minus a 4-cell margin).
fn popup_rect(area: Rect, min_width: u16, width_pct: u16, height: u16) -> Rect {
    let pct_width = area.width.saturating_mul(width_pct) / 100;
    let width = min_width.max(pct_width).min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}
```

This replaced seven near-identical copies of the same centering
arithmetic across `draw_confirm_modal`, `draw_input_modal`,
`draw_search_picker_modal`, `draw_artifactory_picker_modal`,
`draw_registry_select_modal`, and `draw_help_modal`. Each modal's
existing fixed width became `min_width`, with a percentage (40-60%
depending on the modal) layered on top so it grows with the terminal.

The two picker modals' row cap changed from a fixed `.min(10)` to
`area.height.saturating_sub(9).max(1)` — the same budget `popup_rect`
itself uses (4-cell margin + 3-row filter box + 2 list border rows) — so
a tall terminal shows as many results as will fit instead of capping at
10 and relying on scroll.

`draw_inspect_modal` and `draw_layer_diff_modal` were already sized as
`area - 2` on every axis, so they were untouched — they already resized
correctly.

## Verifying it

Extended the `TestBackend` regression suite from #65 with:

- Two "grows on a tall terminal" tests: render a picker with 20 results,
  selection pinned at index 0 (no scroll needed), assert an item past
  index 10 is visible. Temporarily reverted just the row-cap line back to
  the old fixed `.min(10)` and re-ran — both failed, confirming they
  actually exercise the old bug.
- A "small terminal, still visible" test and an "every popup modal
  renders without panicking on a tiny terminal" test, covering the
  `saturating_sub` clamps at the extreme end.

Not verified: an actual interactive `SIGWINCH` resize while the app is
running — no interactive terminal was available in the build environment.
The `TestBackend` tests cover the geometry logic (same code path a real
resize drives), but a live resize is worth a quick manual check.

## Process note

Picked up via `pgmac-workflows:pickup-ticket` against
`pgmac-net/docker-registry-walk#66`. Plan was posted to the ticket and
approved before implementation; no deviations from the posted plan.

PR: https://github.com/pgmac-net/docker-registry-walk/pull/69
