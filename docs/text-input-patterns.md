# Text-input patterns for Rust TUI apps (ratatui + crossterm)

Standard pattern for free-text entry fields (popups, single-line prompts).
Extracted from `gh-issues-tui` (`src/tui/app.rs`, `src/tui/ui.rs`), where it
was hardened over several releases, and applied here in
`docker-registry-walk` (`src/tui/input.rs`, issue #59).

## `InputState`

```rust
pub struct InputState {
    pub buffer: String,
    pub cursor: usize, // CHAR index, not byte index
}
```

`cursor` is a char index. Every mutation converts it to a byte offset via a
`byte_at(char_idx)` helper (`buffer.char_indices().nth(char_idx)`) before
touching the `String`. This is the load-bearing detail: a byte-index cursor
splits multi-byte UTF-8 codepoints and panics on `insert`/`remove` the first
time a user types a non-ASCII character (é, emoji, …). Always index by char,
convert to byte only at the point of mutation.

### Core ops

`insert(c)`, `backspace()`, `delete_char()` (Del / Ctrl+D), `left()`,
`right()`, `home()`, `end()`.

### Readline word ops

- `word_left()` / `word_right()` — Ctrl+←/→, whitespace-delimited.
- `delete_word_back()` — Ctrl+W.
- `kill_to_start()` — Ctrl+U (delete cursor→start).
- `kill_to_end()` — Ctrl+K (delete cursor→end).

These are cheap to add once `byte_at` exists and make every input field feel
like a real shell line editor instead of a bare text box.

## Key handler shape

One function per input surface, matching on `KeyCode` with a `ctrl` bool
computed once:

```rust
let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
match key.code {
    KeyCode::Esc => { /* cancel */ }
    KeyCode::Enter => { /* commit */ }
    KeyCode::Backspace => input.backspace(),
    KeyCode::Delete => input.delete_char(),
    KeyCode::Left if ctrl => input.word_left(),
    KeyCode::Right if ctrl => input.word_right(),
    KeyCode::Left => input.left(),
    KeyCode::Right => input.right(),
    KeyCode::Home => input.home(),
    KeyCode::End => input.end(),
    KeyCode::Char('a') if ctrl => input.home(),
    KeyCode::Char('e') if ctrl => input.end(),
    KeyCode::Char('w') if ctrl => input.delete_word_back(),
    KeyCode::Char('u') if ctrl => input.kill_to_start(),
    KeyCode::Char('k') if ctrl => input.kill_to_end(),
    KeyCode::Char('d') if ctrl => input.delete_char(),
    KeyCode::Char(c) if !ctrl => input.insert(c),
    _ => {}
}
```

If more than one input surface needs this (a plain prompt, a search box,
…), factor it into a single shared function that all of them call, rather
than copy-pasting the match arms per surface — that duplication is exactly
what this doc exists to prevent.

## Rendering: horizontal scroll + real cursor

Two problems with the naive approach (splice a literal `|` into the string
and hand it to a wrapping `Paragraph`): the string reflows/wraps instead of
scrolling, and the "cursor" is a fake character rather than a cursor.

**Scroll window** — stateless, recomputed every frame from `cursor` and the
box's inner `width`:

```rust
pub fn input_scroll_skip(cursor: usize, width: usize) -> usize {
    let width = width.max(1);
    cursor.saturating_sub(width.saturating_sub(1))
}
```

Then slice the visible window: `buffer.chars().skip(skip).take(width)`, and
the on-screen cursor column is `cursor - skip`. The window only moves when
the cursor would otherwise leave it — no jitter while typing in the middle
of a short string.

**Cursor glyph** — a single reversed-style span at the cursor's screen
column, so it looks like a real terminal block cursor:

```rust
fn cursor_spans(text: &str, cursor: usize) -> Vec<Span<'static>> {
    let byte = text.char_indices().nth(cursor).map(|(b, _)| b).unwrap_or(text.len());
    let mut rest = text[byte..].chars();
    let under = rest.next().unwrap_or(' ').to_string();
    let after: String = rest.collect();
    vec![
        Span::raw(text[..byte].to_string()),
        Span::styled(under, Style::default().add_modifier(Modifier::REVERSED)),
        Span::raw(after),
    ]
}
```

Render with `Paragraph::new(Line::from(cursor_spans(&visible, col)))` — no
`Wrap`, since the scroll window already guarantees the line fits.

## When NOT to use full `InputState`

Live type-ahead filters (narrow-as-you-type over a list) don't need cursor
movement — users type forward and backspace, they don't arrow around inside
a filter string. Keep those on a simpler append/backspace-only API
(`push_char` / `pop_char` / `clear`) rather than pulling in the full
`InputState` machinery. `gh-issues-tui`'s picker type-ahead
(`picker_filter_push/backspace/clear`) and this repo's `repo_filter`/
`tag_filter` both follow this simpler pattern deliberately — reserve
`InputState` for actual free-text entry (prompts, search boxes, forms).

## Multi-line text (out of scope here)

`gh-issues-tui` also has a `BodyEditor` (one `InputState` per line, plus
word-wrap and visual-row up/down navigation) for its multi-line issue-body
and comment editors. Neither `docker-registry-walk` field is multi-line, so
that piece wasn't ported — see `gh-issues-tui/src/tui/app.rs:695-920` if a
future field needs it.
