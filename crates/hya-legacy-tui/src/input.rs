//! Single-line text input with emacs-style motion and grapheme-aware editing.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputState {
    text: String,
    /// Grapheme index of the cursor, in the range `0..=grapheme_count()`.
    cursor: usize,
    /// Display columns hidden on the left side of the visible window.
    scroll_cols: usize,
}

impl InputState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = grapheme_count(&text);
        Self {
            text,
            cursor,
            scroll_cols: 0,
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn scroll_offset(&self) -> usize {
        self.scroll_cols
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = grapheme_count(&self.text);
        self.scroll_cols = 0;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.scroll_cols = 0;
    }

    pub fn take_text(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.scroll_cols = 0;
        text
    }

    pub fn insert(&mut self, ch: char) {
        self.insert_str(&ch.to_string());
    }

    pub fn insert_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let byte = grapheme_to_byte(&self.text, self.cursor);
        self.text.insert_str(byte, s);
        self.cursor += grapheme_count(s);
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(grapheme_count(&self.text));
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = grapheme_count(&self.text);
    }

    pub fn move_word_left(&mut self) {
        let gr = graphemes(&self.text);
        if self.cursor == 0 {
            return;
        }
        let mut i = self.cursor - 1;
        while i > 0 && is_space_grapheme(gr[i]) {
            i -= 1;
        }
        if is_word_grapheme(gr[i]) {
            while i > 0 && is_word_grapheme(gr[i - 1]) {
                i -= 1;
            }
        }
        self.cursor = i;
    }

    pub fn move_word_right(&mut self) {
        let gr = graphemes(&self.text);
        let mut i = self.cursor;
        if i >= gr.len() {
            return;
        }
        if is_word_grapheme(gr[i]) {
            while i < gr.len() && is_word_grapheme(gr[i]) {
                i += 1;
            }
        } else {
            while i < gr.len() && !is_word_grapheme(gr[i]) {
                i += 1;
            }
            while i < gr.len() && is_word_grapheme(gr[i]) {
                i += 1;
            }
        }
        self.cursor = i;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = grapheme_to_byte(&self.text, self.cursor - 1);
        let end = grapheme_to_byte(&self.text, self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        if self.cursor >= grapheme_count(&self.text) {
            return;
        }
        let start = grapheme_to_byte(&self.text, self.cursor);
        let end = grapheme_to_byte(&self.text, self.cursor + 1);
        self.text.replace_range(start..end, "");
    }

    pub fn delete_word_backward(&mut self) {
        let original = self.cursor;
        self.move_word_left();
        self.delete_range(self.cursor, original);
    }

    pub fn delete_word_forward(&mut self) {
        let original = self.cursor;
        self.move_word_right();
        self.delete_range(original, self.cursor);
        self.cursor = original;
    }

    pub fn delete_to_start(&mut self) {
        self.delete_range(0, self.cursor);
        self.cursor = 0;
    }

    pub fn delete_to_end(&mut self) {
        let end = grapheme_count(&self.text);
        self.delete_range(self.cursor, end);
    }

    /// Total display width of the text in terminal columns.
    #[must_use]
    pub fn total_width(&self) -> usize {
        self.text.width()
    }

    /// Display column of the cursor relative to the start of the text.
    #[must_use]
    pub fn cursor_column(&self) -> usize {
        graphemes(&self.text)
            .iter()
            .take(self.cursor)
            .map(|g| g.width())
            .sum()
    }

    /// Ensure the cursor column is inside the visible window of `width` columns.
    pub fn scroll_to_cursor(&mut self, width: usize) {
        let col = self.cursor_column();
        if width == 0 {
            self.scroll_cols = 0;
            return;
        }
        if col < self.scroll_cols {
            self.scroll_cols = col;
        } else if col >= self.scroll_cols + width {
            self.scroll_cols = col.saturating_sub(width).saturating_add(1);
        }
    }

    /// Return the visible substring and its starting display column.
    #[must_use]
    pub fn visible_slice(&self, width: usize) -> (&str, usize) {
        if width == 0 {
            return ("", self.scroll_cols);
        }
        let mut col = 0usize;
        let mut start_byte = 0usize;
        let mut end_byte = self.text.len();
        let target_end = self.scroll_cols + width;
        let mut found_start = false;
        for (byte, g) in self.text.grapheme_indices(true) {
            let w = g.width();
            if !found_start && col + w > self.scroll_cols {
                start_byte = byte;
                found_start = true;
            }
            if found_start && col >= target_end {
                end_byte = byte;
                break;
            }
            col += w;
        }
        (
            &self.text[start_byte..end_byte.min(self.text.len())],
            self.scroll_cols,
        )
    }

    fn delete_range(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let byte_start = grapheme_to_byte(&self.text, start);
        let byte_end = grapheme_to_byte(&self.text, end.min(grapheme_count(&self.text)));
        self.text.replace_range(byte_start..byte_end, "");
    }
}

fn graphemes(s: &str) -> Vec<&str> {
    s.graphemes(true).collect()
}

fn grapheme_count(s: &str) -> usize {
    s.graphemes(true).count()
}

fn grapheme_to_byte(s: &str, index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    let count = grapheme_count(s);
    if index >= count {
        return s.len();
    }
    s.grapheme_indices(true)
        .nth(index)
        .map_or(s.len(), |(byte, _)| byte)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_word_grapheme(g: &str) -> bool {
    g.chars().any(is_word_char)
}

fn is_space_grapheme(g: &str) -> bool {
    g.chars().next().is_some_and(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_cursor_end() {
        let mut s = InputState::new();
        s.insert('h');
        s.insert('i');
        assert_eq!(s.text(), "hi");
        assert_eq!(s.cursor(), 2);
        assert_eq!(s.cursor_column(), 2);
    }

    #[test]
    fn move_left_right() {
        let mut s = InputState::with_text("abc");
        s.move_left();
        assert_eq!(s.cursor(), 2);
        s.move_left();
        assert_eq!(s.cursor(), 1);
        s.move_right();
        assert_eq!(s.cursor(), 2);
        s.move_right();
        assert_eq!(s.cursor(), 3);
    }

    #[test]
    fn move_home_end() {
        let mut s = InputState::with_text("abc");
        s.move_home();
        assert_eq!(s.cursor(), 0);
        s.move_end();
        assert_eq!(s.cursor(), 3);
    }

    #[test]
    fn backspace_and_delete() {
        let mut s = InputState::with_text("abc");
        s.move_left();
        s.backspace();
        assert_eq!(s.text(), "ac");
        assert_eq!(s.cursor(), 1);
        s.move_home();
        s.delete();
        assert_eq!(s.text(), "c");
    }

    #[test]
    fn word_motion() {
        let mut s = InputState::with_text("hello world  foo");
        s.move_home();
        s.move_word_right();
        assert_eq!(s.cursor(), 5); // end of "hello"
        s.move_word_right();
        assert_eq!(s.cursor(), 11); // end of "world"
        s.move_word_right();
        assert_eq!(s.cursor(), 16); // end of "foo"
        s.move_word_left();
        assert_eq!(s.cursor(), 13); // start of "foo"
        s.move_word_left();
        assert_eq!(s.cursor(), 6); // start of "world"
    }

    #[test]
    fn delete_word_backward() {
        let mut s = InputState::with_text("hello world");
        s.move_end();
        s.delete_word_backward();
        assert_eq!(s.text(), "hello ");
        assert_eq!(s.cursor(), 6);
    }

    #[test]
    fn delete_word_forward() {
        let mut s = InputState::with_text("hello world");
        s.move_home();
        s.delete_word_forward();
        assert_eq!(s.text(), " world");
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn delete_to_start_and_end() {
        let mut s = InputState::with_text("hello world");
        s.move_home();
        s.move_word_right();
        s.delete_to_end();
        assert_eq!(s.text(), "hello");
        s.move_end();
        s.delete_to_start();
        assert_eq!(s.text(), "");
    }

    #[test]
    fn cjk_width_and_backspace() {
        let mut s = InputState::with_text("中文");
        assert_eq!(s.total_width(), 4);
        assert_eq!(s.cursor_column(), 4);
        s.move_left();
        assert_eq!(s.cursor_column(), 2);
        s.backspace();
        assert_eq!(s.text(), "文");
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.total_width(), 2);
    }

    #[test]
    fn visible_slice_scrolls() {
        let mut s = InputState::with_text("abcdefghijklmnopqrstuvwxyz");
        s.move_end();
        s.scroll_to_cursor(10);
        let (slice, offset) = s.visible_slice(10);
        assert_eq!(offset, s.cursor_column() - 10 + 1);
        assert!(slice.width() <= 10);
        assert!(slice.contains('z'));
    }

    #[test]
    fn insert_at_cursor_middle() {
        let mut s = InputState::with_text("ac");
        s.move_left();
        s.insert('b');
        assert_eq!(s.text(), "abc");
        assert_eq!(s.cursor(), 2);
    }

    #[test]
    fn set_text_resets_cursor() {
        let mut s = InputState::with_text("old");
        s.move_home();
        s.set_text("new longer");
        assert_eq!(s.text(), "new longer");
        assert_eq!(s.cursor(), 10);
    }

    #[test]
    fn take_text_clears_text_cursor_and_scroll() {
        let mut s = InputState::with_text("abcdefghijklmnopqrstuvwxyz");
        s.scroll_to_cursor(10);

        assert_eq!(s.take_text(), "abcdefghijklmnopqrstuvwxyz");
        assert_eq!(s.text(), "");
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.scroll_offset(), 0);
    }
}
