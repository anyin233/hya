//! Frozen cross-cutting contracts shared by render, theme, keymap, prompt, and screens.
//!
//! These shapes are locked in W0 so parallel agents in later waves compose without churn
//! (PLAN.md "Permanent lanes"). Solvers/behaviors that depend on these (the flex layout
//! solver, the keymap dispatcher, the full prompt) land in their waves; this module keeps
//! app-specific contracts local and compatibility-re-exports shared primitives from `hya_tui_lib`.

// Shared geometry, color, and flex-layout primitives live in `hya_tui_lib`.
pub use hya_tui_lib::{
    Align, FlexDirection, FlexSpec, Justify, LayoutResult, NodeId, Rect, RenderNode, Rgba,
    SizeHint, Wrap,
};

// ---------------------------------------------------------------------------
// Input model (shared by keymap dispatch and the prompt input layer).
// ---------------------------------------------------------------------------

/// A logical key (modifiers carried separately on `KeyEvent`).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
    F(u8),
}

/// A single key press with modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl KeyEvent {
    #[must_use]
    pub fn new(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
        }
    }
}

/// A (possibly multi-stroke) chord, e.g. a leader sequence `<leader> t`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChord(pub Vec<KeyEvent>);

/// A canonical command id, e.g. `session.list`. Keymap config keys map to these.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindingId(pub String);

/// The shared precedence boundary between a focused multiline editor and the app keymap.
/// Implemented by the prompt editor (W7) so the keymap dispatcher (W3d) routes keys
/// correctly without redefining input precedence.
pub trait ManagedTextareaInputLayer {
    /// Return `true` if this layer consumed the key (keymap dispatch then skips it).
    fn handle_key(&mut self, key: &KeyEvent) -> bool;
}

// ---------------------------------------------------------------------------
// Prompt document model (frozen; the full editor behavior is W7).
// W4's minimal prompt MUST instantiate this exact type so W4->W7 is additive, not a rewrite.
// ---------------------------------------------------------------------------

/// A structured prompt attachment tracked alongside the editor text.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum PromptPart {
    /// A file mention (`@path`), optionally with a line range.
    File { path: String, url: String },
    /// An agent mention (`@agent`).
    Agent { name: String },
    /// Summarized pasted text whose visible placeholder hides the real content.
    SyntheticText { value: String },
}

/// What an extmark marks in the editor text.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtmarkKind {
    FileMention,
    AgentMention,
    PastedPlaceholder,
}

/// A tracked visual range in the editor text, linking display ranges to `parts`.
#[derive(Debug, Clone, PartialEq)]
pub struct Extmark {
    /// Display-offset start/end (see `prompt::display`).
    pub start: usize,
    pub end: usize,
    pub kind: ExtmarkKind,
    /// Index into `PromptDoc::parts`, if this extmark backs a structured part.
    pub part_index: Option<usize>,
}

/// The editor document: visible text + structured parts + extmarks linking them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromptDoc {
    pub text: String,
    pub parts: Vec<PromptPart>,
    pub extmarks: Vec<Extmark>,
    /// `normal` | `shell` submit mode (shell entered via `!`).
    pub shell_mode: bool,
    /// Cursor as a byte offset into `text`, always at a `char` boundary.
    pub cursor: usize,
}

impl PromptDoc {
    pub fn insert_str(&mut self, value: &str) {
        self.clamp_cursor();
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
    }

    pub fn insert_char(&mut self, ch: char) {
        self.clamp_cursor();
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
    }

    pub fn clear_input(&mut self) {
        self.text.clear();
        self.parts.clear();
        self.extmarks.clear();
        self.shell_mode = false;
        self.cursor = 0;
    }

    pub fn backspace(&mut self) {
        self.clamp_cursor();
        if self.cursor == 0 {
            return;
        }
        let start = self.prev_boundary(self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete(&mut self) {
        self.clamp_cursor();
        if self.cursor >= self.text.len() {
            return;
        }
        let end = self.next_boundary(self.cursor);
        self.text.replace_range(self.cursor..end, "");
    }

    pub fn move_left(&mut self) {
        self.clamp_cursor();
        self.cursor = self.prev_boundary(self.cursor);
    }

    pub fn move_right(&mut self) {
        self.clamp_cursor();
        self.cursor = self.next_boundary(self.cursor);
    }

    pub fn move_line_home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }

    pub fn move_line_end(&mut self) {
        self.cursor = self.line_end(self.cursor);
    }

    pub fn move_buffer_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_buffer_end(&mut self) {
        self.cursor = self.text.len();
    }

    pub fn move_word_left(&mut self) {
        self.cursor = self.word_start(self.cursor);
    }

    pub fn move_word_right(&mut self) {
        self.cursor = self.word_end(self.cursor);
    }

    pub fn delete_word_left(&mut self) {
        let start = self.word_start(self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete_word_right(&mut self) {
        let end = self.word_end(self.cursor);
        self.text.replace_range(self.cursor..end, "");
    }

    pub fn delete_to_line_start(&mut self) {
        let start = self.line_start(self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete_to_line_end(&mut self) {
        let end = self.line_end(self.cursor);
        self.text.replace_range(self.cursor..end, "");
    }

    /// Move up one logical line keeping the column; `false` if already on the first line.
    pub fn move_up_line(&mut self) -> bool {
        let line_start = self.line_start(self.cursor);
        if line_start == 0 {
            return false;
        }
        let col = self.text[line_start..self.cursor].chars().count();
        let prev_start = self.line_start(line_start - 1);
        self.cursor = self.column_byte(prev_start, line_start - 1, col);
        true
    }

    /// Move down one logical line keeping the column; `false` if already on the last line.
    pub fn move_down_line(&mut self) -> bool {
        let line_end = self.line_end(self.cursor);
        if line_end >= self.text.len() {
            return false;
        }
        let line_start = self.line_start(self.cursor);
        let col = self.text[line_start..self.cursor].chars().count();
        let next_start = line_end + 1;
        let next_end = self.line_end(next_start);
        self.cursor = self.column_byte(next_start, next_end, col);
        true
    }

    fn clamp_cursor(&mut self) {
        if self.cursor > self.text.len() {
            self.cursor = self.text.len();
        } else if !self.text.is_char_boundary(self.cursor) {
            self.cursor = self.prev_boundary(self.cursor);
        }
    }

    fn prev_boundary(&self, index: usize) -> usize {
        self.text[..index]
            .char_indices()
            .next_back()
            .map_or(0, |(byte, _)| byte)
    }

    fn next_boundary(&self, index: usize) -> usize {
        self.text[index..]
            .chars()
            .next()
            .map_or(index, |ch| index + ch.len_utf8())
    }

    fn line_start(&self, index: usize) -> usize {
        self.text[..index].rfind('\n').map_or(0, |byte| byte + 1)
    }

    fn line_end(&self, index: usize) -> usize {
        self.text[index..]
            .find('\n')
            .map_or(self.text.len(), |byte| index + byte)
    }

    fn column_byte(&self, start: usize, end: usize, col: usize) -> usize {
        self.text[start..end]
            .char_indices()
            .nth(col)
            .map_or(end, |(byte, _)| start + byte)
    }

    fn word_start(&self, index: usize) -> usize {
        let mut idx = index;
        while idx > 0 {
            let prev = self.prev_boundary(idx);
            if self.text[prev..idx].starts_with(char::is_whitespace) {
                idx = prev;
            } else {
                break;
            }
        }
        while idx > 0 {
            let prev = self.prev_boundary(idx);
            if self.text[prev..idx].starts_with(char::is_whitespace) {
                break;
            }
            idx = prev;
        }
        idx
    }

    fn word_end(&self, index: usize) -> usize {
        let mut idx = index;
        while idx < self.text.len() {
            if self.text[idx..].starts_with(char::is_whitespace) {
                idx = self.next_boundary(idx);
            } else {
                break;
            }
        }
        while idx < self.text.len() {
            if self.text[idx..].starts_with(char::is_whitespace) {
                break;
            }
            idx = self.next_boundary(idx);
        }
        idx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str, cursor: usize) -> PromptDoc {
        PromptDoc {
            text: text.to_owned(),
            cursor,
            ..PromptDoc::default()
        }
    }

    #[test]
    fn insert_char_inserts_at_cursor() {
        let mut d = doc("ac", 1);
        d.insert_char('b');
        assert_eq!(d.text, "abc");
        assert_eq!(d.cursor, 2);
    }

    #[test]
    fn backspace_and_delete_remove_around_cursor() {
        let mut d = doc("abc", 3);
        d.backspace();
        assert_eq!((d.text.as_str(), d.cursor), ("ab", 2));
        let mut d = doc("abc", 0);
        d.backspace();
        assert_eq!((d.text.as_str(), d.cursor), ("abc", 0));
        d.delete();
        assert_eq!((d.text.as_str(), d.cursor), ("bc", 0));
    }

    #[test]
    fn move_left_right_cross_multibyte_boundaries() {
        let mut d = doc("你好", 0);
        d.move_right();
        assert_eq!(d.cursor, 3);
        d.move_right();
        assert_eq!(d.cursor, 6);
        d.move_left();
        assert_eq!(d.cursor, 3);
    }

    #[test]
    fn line_and_buffer_motions() {
        let mut d = doc("ab\ncd", 4);
        d.move_line_home();
        assert_eq!(d.cursor, 3);
        d.move_line_end();
        assert_eq!(d.cursor, 5);
        d.move_buffer_home();
        assert_eq!(d.cursor, 0);
        d.move_buffer_end();
        assert_eq!(d.cursor, 5);
    }

    #[test]
    fn word_motion_and_word_delete() {
        let mut d = doc("foo bar baz", 11);
        d.move_word_left();
        assert_eq!(d.cursor, 8);
        d.move_word_left();
        assert_eq!(d.cursor, 4);
        let mut d = doc("foo bar", 7);
        d.delete_word_left();
        assert_eq!((d.text.as_str(), d.cursor), ("foo ", 4));
        let mut d = doc("foo bar", 0);
        d.delete_word_right();
        assert_eq!((d.text.as_str(), d.cursor), (" bar", 0));
    }

    #[test]
    fn up_down_line_preserve_column_and_report_edges() {
        let mut d = doc("abc\ndefgh", 6);
        assert!(d.move_up_line());
        assert_eq!(d.cursor, 2);
        assert!(!d.move_up_line());
        assert!(d.move_down_line());
        assert_eq!(d.cursor, 6);
        assert!(!d.move_down_line());
    }

    #[test]
    fn delete_to_line_end_and_start() {
        let mut d = doc("abcde", 2);
        d.delete_to_line_end();
        assert_eq!((d.text.as_str(), d.cursor), ("ab", 2));
        d.delete_to_line_start();
        assert_eq!((d.text.as_str(), d.cursor), ("", 0));
    }
}
