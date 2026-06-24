use crate::contracts::{Key, KeyEvent};

pub(crate) fn key_events_match(expected: &KeyEvent, actual: &KeyEvent) -> bool {
    let expected = ComparableKeyEvent::from(*expected);
    let actual = ComparableKeyEvent::from(*actual);
    expected.ctrl == actual.ctrl
        && expected.alt == actual.alt
        && expected.meta == actual.meta
        && expected.key == actual.key
        && (expected.shift == actual.shift || expected.ignores_shift())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ComparableKeyEvent {
    key: Key,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
}

impl ComparableKeyEvent {
    fn ignores_shift(self) -> bool {
        matches!(self.key, Key::Char(ch) if !ch.is_ascii_alphanumeric())
    }
}

impl From<KeyEvent> for ComparableKeyEvent {
    fn from(event: KeyEvent) -> Self {
        let mut shift = event.shift;
        let key = match event.key {
            Key::Char(ch) => {
                shift |= ch.is_ascii_uppercase();
                Key::Char(ch.to_ascii_lowercase())
            }
            Key::BackTab => {
                shift = true;
                Key::Tab
            }
            Key::Enter => Key::Enter,
            Key::Esc => Key::Esc,
            Key::Backspace => Key::Backspace,
            Key::Tab => Key::Tab,
            Key::Up => Key::Up,
            Key::Down => Key::Down,
            Key::Left => Key::Left,
            Key::Right => Key::Right,
            Key::Home => Key::Home,
            Key::End => Key::End,
            Key::PageUp => Key::PageUp,
            Key::PageDown => Key::PageDown,
            Key::Delete => Key::Delete,
            Key::Insert => Key::Insert,
            Key::F(value) => Key::F(value),
        };
        Self {
            key,
            ctrl: event.ctrl,
            alt: event.alt,
            shift,
            meta: event.meta,
        }
    }
}
