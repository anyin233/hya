use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_tui::AppState;

// The controller does not know the rendered viewport height, so page and
// half-page transcript aliases share the current controller scroll quantum.
const PAGE_SCROLL_LINES: u16 = 5;

pub fn handle_message_scroll_key(app: &mut AppState, key: &KeyEvent) -> bool {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }

    match key.code {
        KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.scroll_up(PAGE_SCROLL_LINES);
            true
        }
        KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.scroll_down(PAGE_SCROLL_LINES);
            true
        }
        KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.scroll_up(1);
            true
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.scroll_down(1);
            true
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.scroll_up(PAGE_SCROLL_LINES);
            true
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.scroll_down(PAGE_SCROLL_LINES);
            true
        }
        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.scroll_back = 0;
            true
        }
        KeyCode::Char('g') => {
            app.scroll_back = u16::MAX;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl(code: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
    }

    fn ctrl_alt(code: char) -> KeyEvent {
        KeyEvent::new(
            KeyCode::Char(code),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        )
    }

    #[test]
    fn message_page_scroll_aliases_match_page_quantum() {
        let mut app = AppState::default();

        assert!(handle_message_scroll_key(&mut app, &ctrl_alt('b')));
        assert_eq!(app.scroll_back, PAGE_SCROLL_LINES);
        assert!(handle_message_scroll_key(&mut app, &ctrl_alt('f')));
        assert_eq!(app.scroll_back, 0);
    }

    #[test]
    fn message_line_scroll_aliases_move_one_line() {
        let mut app = AppState::default();

        assert!(handle_message_scroll_key(&mut app, &ctrl_alt('y')));
        assert_eq!(app.scroll_back, 1);
        assert!(handle_message_scroll_key(&mut app, &ctrl_alt('e')));
        assert_eq!(app.scroll_back, 0);
    }

    #[test]
    fn message_half_page_aliases_use_page_quantum() {
        let mut app = AppState::default();

        assert!(handle_message_scroll_key(&mut app, &ctrl_alt('u')));
        assert_eq!(app.scroll_back, PAGE_SCROLL_LINES);
        assert!(handle_message_scroll_key(&mut app, &ctrl_alt('d')));
        assert_eq!(app.scroll_back, 0);
    }

    #[test]
    fn message_first_last_aliases_match_home_end() {
        let mut app = AppState {
            scroll_back: 12,
            ..AppState::default()
        };

        assert!(handle_message_scroll_key(&mut app, &ctrl('g')));
        assert_eq!(app.scroll_back, u16::MAX);
        assert!(handle_message_scroll_key(&mut app, &ctrl_alt('g')));
        assert_eq!(app.scroll_back, 0);
    }

    #[test]
    fn unhandled_keys_return_false_without_scrolling() {
        let mut app = AppState::default();

        assert!(!handle_message_scroll_key(&mut app, &ctrl('x')));
        assert_eq!(app.scroll_back, 0);
    }
}
