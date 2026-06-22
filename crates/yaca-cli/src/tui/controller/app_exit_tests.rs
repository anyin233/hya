use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_tui::AppState;

use super::{Controller, TuiEffect};

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
fn ctrl_d_exits_immediately_when_input_is_empty() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    let effect = controller.handle_key(ctrl('d'));

    // Then
    assert_eq!(effect, TuiEffect::Exit);
    assert_eq!(controller.app.input, "");
    assert!(!controller.app.exit_armed);
}

#[test]
fn ctrl_d_exits_from_open_dialog_without_mutating_dialog_state() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "draft prompt".to_string(),
        ..AppState::default()
    });
    assert_eq!(controller.handle_key(ctrl('p')), TuiEffect::None);
    assert!(controller.app.dialog.is_some());

    // When
    let effect = controller.handle_key(ctrl('d'));

    // Then
    assert_eq!(effect, TuiEffect::Exit);
    assert_eq!(controller.app.input, "draft prompt");
    assert!(controller.app.dialog.is_some());
}

#[test]
fn ctrl_d_exits_even_when_leader_is_armed() {
    // Given
    let mut controller = Controller::new(AppState::default());
    assert_eq!(controller.handle_key(ctrl('x')), TuiEffect::None);

    // When
    let effect = controller.handle_key(ctrl('d'));

    // Then
    assert_eq!(effect, TuiEffect::Exit);
    assert_eq!(controller.app.input, "");
}

#[test]
fn ctrl_d_does_not_arm_ctrl_c_double_exit() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    assert_eq!(controller.handle_key(ctrl('d')), TuiEffect::Exit);
    let first_ctrl_c = controller.handle_key(ctrl('c'));

    // Then
    assert_eq!(first_ctrl_c, TuiEffect::None);
    assert!(controller.app.exit_armed);
}

#[test]
fn ctrl_alt_d_keeps_message_scroll_behavior() {
    // Given
    let mut controller = Controller::new(AppState {
        scroll_back: 5,
        ..AppState::default()
    });

    // When
    let effect = controller.handle_key(ctrl_alt('d'));

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(controller.app.scroll_back, 0);
}
