use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_tui::AppState;

use super::{Controller, TuiEffect};

fn ctrl(code: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

#[test]
fn ctrl_u_deletes_to_current_line_start() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "first line\nsecond word".to_string(),
        exit_armed: true,
        ..AppState::default()
    });

    // When
    let effect = controller.handle_key(ctrl('u'));

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(controller.app.input, "first line\n");
    assert!(!controller.app.exit_armed);
}

#[test]
fn ctrl_w_deletes_previous_word_from_input_end() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "alpha beta   ".to_string(),
        ..AppState::default()
    });

    // When
    let effect = controller.handle_key(ctrl('w'));

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(controller.app.input, "alpha ");
}

#[test]
fn input_edit_shortcuts_refresh_completion_popup() {
    // Given
    let mut controller = Controller::new(AppState::default());
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('/'))),
        TuiEffect::None
    );
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('m'))),
        TuiEffect::None
    );
    assert!(controller.app.dialog.is_some());

    // When
    assert_eq!(controller.handle_key(ctrl('u')), TuiEffect::None);

    // Then
    assert_eq!(controller.app.input, "");
    assert!(controller.app.dialog.is_none());
}
