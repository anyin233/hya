#![allow(clippy::expect_used)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_tui::AppState;

use super::{Controller, TuiEffect};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

fn ctrl(code: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
}

#[test]
fn ctrl_p_enter_dispatches_selected_command() {
    // Given
    let mut controller =
        Controller::with_models(AppState::default(), vec!["alpha".into(), "beta".into()]);

    // When
    assert_eq!(controller.handle_key(ctrl('p')), TuiEffect::None);
    let effect = controller.handle_key(key(KeyCode::Enter));

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(
        controller
            .app
            .dialog
            .as_ref()
            .expect("model dialog after palette dispatch")
            .title,
        "select model"
    );
}

#[test]
fn ctrl_p_can_dispatch_non_dialog_commands_from_selection() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    assert_eq!(controller.handle_key(ctrl('p')), TuiEffect::None);
    assert_eq!(controller.handle_key(key(KeyCode::Down)), TuiEffect::None);
    assert_eq!(controller.handle_key(key(KeyCode::Down)), TuiEffect::None);
    let effect = controller.handle_key(key(KeyCode::Enter));

    // Then
    assert_eq!(effect, TuiEffect::NewSession);
    assert!(controller.app.dialog.is_none());
}
