#![allow(clippy::expect_used)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_proto::{
    MessageId, MessageProjection, PartId, PartProjection, Projection, Role, SessionProjection,
};
use yaca_tui::AppState;

use super::super::block_action::{SelectedBlockAction, SelectedBlockActionKind};
use super::{Controller, TuiEffect};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

#[test]
fn enter_opens_message_actions_dialog_for_selected_block() {
    let mut controller = Controller::new(AppState {
        selected_message: Some(2),
        ..AppState::default()
    });

    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);

    let dialog = controller.app.dialog.as_ref().expect("message dialog");
    assert_eq!(dialog.title, "Message Actions");
    assert_eq!(dialog.subtitle, "selected message operations");
    assert_eq!(dialog.items[0].label, "Revert");
    assert_eq!(dialog.items[0].detail, "undo messages and file changes");
    assert_eq!(dialog.items[1].label, "Copy");
    assert_eq!(dialog.items[1].detail, "message text to clipboard");
    assert_eq!(dialog.items[2].label, "Fork");
    assert_eq!(dialog.items[2].detail, "create a new session");
}

#[test]
fn message_actions_dialog_dispatches_revert_and_fork() {
    let mut controller = Controller::new(AppState {
        selected_message: Some(3),
        ..AppState::default()
    });

    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
    assert_eq!(
        controller.handle_key(key(KeyCode::Enter)),
        TuiEffect::SelectedBlock(SelectedBlockAction {
            kind: SelectedBlockActionKind::Revert,
            message_index: 3,
        })
    );

    controller.app.selected_message = Some(3);
    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
    assert_eq!(controller.handle_key(key(KeyCode::Down)), TuiEffect::None);
    assert_eq!(controller.handle_key(key(KeyCode::Down)), TuiEffect::None);
    assert_eq!(
        controller.handle_key(key(KeyCode::Enter)),
        TuiEffect::SelectedBlock(SelectedBlockAction {
            kind: SelectedBlockActionKind::Branch,
            message_index: 3,
        })
    );
}

#[test]
fn message_actions_dialog_dispatches_copy_with_selected_text() {
    let mut controller = Controller::new(AppState {
        selected_message: Some(0),
        projection: Projection {
            session: SessionProjection {
                messages: vec![MessageProjection {
                    id: MessageId::new(),
                    role: Role::User,
                    started_millis: None,
                    completed_millis: None,
                    finish: None,
                    parts: vec![PartProjection::Text {
                        id: PartId::new(),
                        text: "copy this prompt".to_string(),
                    }],
                }],
                ..SessionProjection::default()
            },
            last_seq: 0,
        },
        ..AppState::default()
    });

    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
    assert_eq!(controller.handle_key(key(KeyCode::Down)), TuiEffect::None);
    assert_eq!(
        controller.handle_key(key(KeyCode::Enter)),
        TuiEffect::CopyMessage("copy this prompt".to_string())
    );
}
