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
    let new_index = controller
        .app
        .dialog
        .as_ref()
        .expect("command palette after ctrl-p")
        .items
        .iter()
        .position(|item| item.label == "/new" && !item.detail.starts_with("Suggested ·"))
        .expect("regular /new command in palette");
    for _ in 0..new_index {
        assert_eq!(controller.handle_key(key(KeyCode::Down)), TuiEffect::None);
    }
    let effect = controller.handle_key(key(KeyCode::Enter));

    // Then
    assert_eq!(effect, TuiEffect::NewSession);
    assert!(controller.app.dialog.is_none());
}

#[test]
fn ctrl_p_palette_prepends_suggested_commands_with_category() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    assert_eq!(controller.handle_key(ctrl('p')), TuiEffect::None);

    // Then
    let dialog = controller
        .app
        .dialog
        .as_ref()
        .expect("command palette after ctrl-p");
    assert_eq!(dialog.items[0].label, "/model");
    assert!(
        dialog.items[0].detail.starts_with("Suggested ·"),
        "first palette row should be an OpenCode-style Suggested command: {:?}",
        dialog.items[0]
    );
    let regular_model = dialog
        .items
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, item)| item.label == "/model")
        .expect("regular /model command remains in full command list");
    assert!(
        regular_model.1.detail.starts_with("Agent ·"),
        "regular command should keep its own category: {:?}",
        regular_model.1
    );
}
