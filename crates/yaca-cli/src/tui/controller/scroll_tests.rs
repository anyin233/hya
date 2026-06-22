#![allow(clippy::expect_used)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_proto::{MessageId, MessageProjection, Role};
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

fn ctrl_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn message_projection(role: Role) -> MessageProjection {
    MessageProjection {
        id: MessageId::new(),
        role,
        started_millis: None,
        completed_millis: None,
        finish: None,
        parts: Vec::new(),
    }
}

#[test]
fn opencode_scroll_aliases_apply_to_controller() {
    let mut controller = Controller::new(AppState::default());

    assert_eq!(controller.handle_key(ctrl_alt('b')), TuiEffect::None);
    assert_eq!(controller.app.scroll_back, 5);
    assert_eq!(controller.handle_key(ctrl_alt('f')), TuiEffect::None);
    assert_eq!(controller.app.scroll_back, 0);
    assert_eq!(controller.handle_key(ctrl('g')), TuiEffect::None);
    assert_eq!(controller.app.scroll_back, u16::MAX);
    assert_eq!(controller.handle_key(ctrl_alt('g')), TuiEffect::None);
    assert_eq!(controller.app.scroll_back, 0);
}

#[test]
fn ctrl_up_down_still_select_transcript_messages() {
    let mut controller = Controller::new(AppState::default());
    controller.app.projection.session.messages = vec![
        message_projection(Role::User),
        message_projection(Role::Assistant),
        message_projection(Role::System),
    ];

    assert_eq!(
        controller.handle_key(ctrl_key(KeyCode::Down)),
        TuiEffect::None
    );
    assert_eq!(controller.app.selected_message, Some(0));
    assert_eq!(
        controller.handle_key(ctrl_key(KeyCode::Down)),
        TuiEffect::None
    );
    assert_eq!(controller.app.selected_message, Some(1));

    controller.app.selected_message = None;
    assert_eq!(
        controller.handle_key(ctrl_key(KeyCode::Up)),
        TuiEffect::None
    );
    assert_eq!(controller.app.selected_message, Some(2));
    assert_eq!(
        controller.handle_key(ctrl_key(KeyCode::Up)),
        TuiEffect::None
    );
    assert_eq!(controller.app.selected_message, Some(1));
}
