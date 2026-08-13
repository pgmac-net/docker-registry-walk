# Keybindings help pane

The in-app help pane (`?`) is **contextual**: it shows only the keys for
whatever is currently on screen, rather than one long scrolling list of
everything. Issue [#112](https://github.com/pgmac-net/docker-registry-walk/issues/112).

## Why

The pane used to be a single ~66-line list covering every surface, and by the
time GHCR added three more surfaces (its two pickers plus the owner picker) it
had also drifted from reality: some entries were wrong, several key aliases
were undocumented, and seven modals had no coverage at all. Fixing every gap
in one list would have pushed it past 100 lines in a 30-row window with no
scroll indicator. Going contextual fixes both problems at once — most contexts
now fit on screen without scrolling.

## Reaching it

`?` opens help from every surface **except a text prompt** (`Modal::Input`,
used for entering a new tag name, a copy destination, a password, and so on).
There, `?` is a legitimate character a value might need, so it stays a normal
character rather than being reserved — the trade-off is that a prompt shows
its own inline hint (`[Enter] confirm  [Esc] cancel`) instead of being
reachable through `?`.

Inside the Inspect JSON viewer, `?` is *also* a normal character while a
search query is being typed — same reasoning, narrower scope.

Filterable pickers (Artifactory repo-keys, GHCR packages, the GHCR owner
picker, Docker Hub search) do **not** need the same exception: none of their
values can contain `?` (repository names, package names, GitHub logins), so
reserving the key there costs nothing.

Closing help (`?`, `q`, or `Esc`) returns to exactly where it was opened —
including a picker's typed filter text and highlighted row. See
`App::help_return` / `event::open_help` in `src/tui/`.

## Contexts

| Context | Reachable from | Sections shown |
|---|---|---|
| `Normal(Repos)` | Repos panel focused, no modal | Navigation, Filter, Repository operations, Registry, General |
| `Normal(Tags)` | Tags panel focused, no modal | Navigation, Filter, Image operations, Tags panel, General |
| `Normal(Detail)` | Detail panel focused, no modal | Navigation, Image operations (just `c`), General |
| `Inspect` | JSON viewer open, not searching | Full viewer keymap, including close (`Esc`/`q`) and search (`Esc`/`Enter`) |
| `SearchPicker` | Docker Hub search open | Type-to-search, `↑↓` only (`j`/`k` insert text) |
| `FilterPicker` | Artifactory repo-key picker, or GHCR package picker | Type-to-filter, `↑↓`, `Enter`, `Esc` |
| `OwnerPicker` | GHCR owner picker | As `FilterPicker`, plus the `Use "…"` row for a typed owner not in the suggestion list |
| `RegistrySelect` | `R` — the registry switcher | `↑↓`/`jk`, `Enter`, `Esc` |
| `LayerDiff` | `D` — the layer diff overlay | `↑↓`/`jk`, `Esc`/`q` |

`Normal` is further split by which panel has focus, matching the footer
keybinding bar, which already varies with focus — a key like `s` (cycle tag
sort) is meaningless while browsing Repos, so it doesn't appear there.

Each context is a self-contained list with its own headers rather than
sharing a common "Navigation" / "General" pair: the same key means different
things in different places (a picker's `Esc` cancels the picker, not the
app), so forcing shared sections onto incompatible keymaps would misdescribe
them. The mapping from a `Modal` (and, for `Normal`, the focused `Focus`) to
its `HelpContext` lives in the pure `app::help_context_for`, which both the
key handler and the renderer call — so a modal can never open help for one
context and render another.

## Corrections made alongside going contextual

The audit that motivated this ticket found the old single list had drifted:

- `Esc` **clears** the active filter; `Enter` (and `Tab`) **keep** it and
  exit. The old copy called both "Exit filter mode", which was true but hid
  the difference — and the footer bar already got this right.
- `Enter` on a selected tag opens the Inspect viewer; the old copy only
  described "move focus to Tags when in Repos".
- `Shift-Tab` / `←` (previous panel) was undocumented; only `Tab` (next panel)
  was mentioned, and `→` as its alias wasn't either.
- The Inspect viewer's close keys (`Esc`/`q`), fold aliases (`h`/`l` beside
  `←`/`→`), and `Home`/`End` (beside `g`/`G`) were all missing.
- Three pickers' own inline hints (not just the help pane) claimed `j`/`k`
  navigate alongside the arrow keys. They don't — in all three, letters are
  typed into the filter or search box instead, since a repository name,
  package name, or search query could contain either letter. Fixed in
  `ARTIFACTORY_PICKER_LABELS`, `GHCR_PICKER_LABELS`, and the Docker Hub search
  modal's own labels, to match what `GHCR_OWNER_PICKER_LABELS` already said
  correctly.

## Scrolling

Most contexts now fit without scrolling — that's the point of splitting one
long list into several short ones. When a context still doesn't fit (a very
small terminal, or `Normal`, which stayed the longest of the set), the title
shows a position indicator (`12-30/34`) and a scroll hint; otherwise the title
is just the close hint, since an indicator that's always present would be
noise once most contexts don't need one.
