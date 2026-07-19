//! UTF-8-safe single-line text input with readline-style editing.
//!
//! Ported from `gh-issues-tui` (`src/tui/app.rs`); see
//! `docs/text-input-patterns.md` for the full writeup.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

/// Single-line text buffer with a char-indexed cursor. Every mutation
/// converts the char index to a byte offset via `byte_at` so multi-byte
/// UTF-8 (é, emoji, …) never splits mid-codepoint.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InputState {
    pub buffer: String,
    pub cursor: usize,
}

impl InputState {
    pub fn start(&mut self, initial: &str) {
        self.buffer = initial.to_owned();
        self.cursor = self.buffer.chars().count();
    }

    pub fn insert(&mut self, c: char) {
        let byte = self.byte_at(self.cursor);
        self.buffer.insert(byte, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let byte = self.byte_at(self.cursor);
            self.buffer.remove(byte);
        }
    }

    /// Delete/Ctrl+D: remove the character under the cursor.
    pub fn delete_char(&mut self) {
        if self.cursor < self.buffer.chars().count() {
            let byte = self.byte_at(self.cursor);
            self.buffer.remove(byte);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.buffer.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.buffer.chars().count();
    }

    /// Ctrl+←: to the start of the current or previous word
    /// (whitespace-delimited).
    pub fn word_left(&mut self) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.cursor = i;
    }

    /// Ctrl+→: to the end of the current or next word.
    pub fn word_right(&mut self) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let mut i = self.cursor;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        self.cursor = i;
    }

    /// Ctrl+W: delete the word before the cursor (and the whitespace
    /// between it and the cursor).
    pub fn delete_word_back(&mut self) {
        let end = self.byte_at(self.cursor);
        self.word_left();
        let start = self.byte_at(self.cursor);
        self.buffer.replace_range(start..end, "");
    }

    /// Ctrl+U: delete from the cursor back to the start of the line.
    pub fn kill_to_start(&mut self) {
        let byte = self.byte_at(self.cursor);
        self.buffer.replace_range(..byte, "");
        self.cursor = 0;
    }

    /// Ctrl+K: delete from the cursor to the end of the line.
    pub fn kill_to_end(&mut self) {
        let byte = self.byte_at(self.cursor);
        self.buffer.truncate(byte);
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.buffer
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.buffer.len())
    }
}

/// The char index to start displaying from so a single-line input's cursor
/// always stays within a `width`-wide window. Stateless: recomputed from
/// `cursor` and `width` each frame, so the window only moves when the
/// cursor's position relative to the current window requires it.
pub fn input_scroll_skip(cursor: usize, width: usize) -> usize {
    let width = width.max(1);
    cursor.saturating_sub(width.saturating_sub(1))
}

/// Split `text` into spans with a single reversed-style char at `cursor`
/// (a char index into `text`), rendering a terminal-style block cursor.
pub fn cursor_spans(text: &str, cursor: usize) -> Vec<Span<'static>> {
    let byte = text
        .char_indices()
        .nth(cursor)
        .map(|(b, _)| b)
        .unwrap_or(text.len());
    let mut rest = text[byte..].chars();
    let under = rest.next().unwrap_or(' ').to_string();
    let after: String = rest.collect();
    vec![
        Span::raw(text[..byte].to_string()),
        Span::styled(under, Style::default().add_modifier(Modifier::REVERSED)),
        Span::raw(after),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str, cursor: usize) -> InputState {
        InputState {
            buffer: text.to_owned(),
            cursor,
        }
    }

    #[test]
    fn edits_utf8_safely() {
        let mut i = InputState::default();
        i.start("héllo");
        i.left();
        i.backspace();
        assert_eq!(i.buffer, "hélo");
        i.insert('x');
        assert_eq!(i.buffer, "hélxo");
    }

    #[test]
    fn word_left_skips_trailing_whitespace_then_word() {
        let mut i = input("foo-bar  baz héllo", 18);
        i.word_left();
        assert_eq!(i.cursor, 13); // start of "héllo"
        i.word_left();
        assert_eq!(i.cursor, 9); // start of "baz"
        i.word_left();
        assert_eq!(i.cursor, 0); // start of "foo-bar"
    }

    #[test]
    fn word_right_skips_to_end_of_next_word() {
        let mut i = input("one two  three", 0);
        i.word_right();
        assert_eq!(i.cursor, 3);
        i.word_right();
        assert_eq!(i.cursor, 7);
        i.word_right();
        assert_eq!(i.cursor, 14);
    }

    #[test]
    fn delete_word_back_removes_word_and_gap() {
        let mut i = input("héllo world", 6);
        i.delete_word_back();
        assert_eq!(i.buffer, "world");
        assert_eq!(i.cursor, 0);
    }

    #[test]
    fn kill_to_start_and_end() {
        let mut i = input("abc", 1);
        i.kill_to_start();
        assert_eq!(i.buffer, "bc");
        assert_eq!(i.cursor, 0);

        let mut j = input("abc", 1);
        j.kill_to_end();
        assert_eq!(j.buffer, "a");
    }

    #[test]
    fn scroll_skip_keeps_cursor_in_window() {
        assert_eq!(input_scroll_skip(0, 10), 0);
        assert_eq!(input_scroll_skip(9, 10), 0);
        assert_eq!(input_scroll_skip(10, 10), 1);
        assert_eq!(input_scroll_skip(25, 10), 16);
        assert_eq!(input_scroll_skip(5, 0), 5);
    }
}
