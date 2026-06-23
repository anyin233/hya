#![allow(clippy::expect_used)]

use std::time::{Duration, Instant};

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
fn ctrl_x_opens_non_modal_keybindings_preview() {
    // Given
    let mut controller =
        Controller::with_models(AppState::default(), vec!["alpha".into(), "beta".into()]);

    // When
    let arm_effect = controller.handle_key(ctrl('x'));

    // Then
    assert_eq!(arm_effect, TuiEffect::None);
    assert!(controller.app.dialog.is_none());
    let preview = controller
        .app
        .keybindings
        .as_ref()
        .expect("which-key preview");
    assert_eq!(preview.title, "Key Bindings");
    assert!(
        preview
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .any(|item| item.key == "b" && item.label == "Toggle sidebar")
    );
    assert!(
        preview
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .any(|item| item.key == "m" && item.label == "Select model")
    );
    assert!(
        preview
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .any(|item| item.key == "↓" && item.label == "Status")
    );
    assert!(
        !preview
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .any(|item| item.label == "View subagents")
    );

    // When: the pending leader sequence receives the second key.
    let model_effect = controller.handle_key(key(KeyCode::Char('m')));

    // Then: the preview was non-modal, so the leader dispatch still runs.
    assert_eq!(model_effect, TuiEffect::None);
    assert!(controller.app.keybindings.is_none());
    assert_eq!(
        controller.app.dialog.as_ref().expect("model dialog").title,
        "select model"
    );
}

#[test]
fn ctrl_x_then_b_toggles_sidebar_without_typing() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    assert_eq!(controller.handle_key(ctrl('x')), TuiEffect::None);
    let hide_effect = controller.handle_key(key(KeyCode::Char('b')));

    // Then
    assert_eq!(hide_effect, TuiEffect::None);
    assert!(controller.app.sidebar_hidden);
    assert_eq!(controller.app.input, "");
    assert!(controller.app.keybindings.is_none());

    // When
    assert_eq!(controller.handle_key(ctrl('x')), TuiEffect::None);
    let show_effect = controller.handle_key(key(KeyCode::Char('b')));

    // Then
    assert_eq!(show_effect, TuiEffect::None);
    assert!(!controller.app.sidebar_hidden);
    assert_eq!(controller.app.input, "");
}

#[test]
fn ctrl_x_preview_labels_down_as_view_subagents_when_active_subagents_exist() {
    // Given
    let mut controller = Controller::new(AppState {
        running: true,
        team: vec![("explore".to_string(), "running".to_string())],
        ..AppState::default()
    });

    // When
    let arm_effect = controller.handle_key(ctrl('x'));

    // Then
    assert_eq!(arm_effect, TuiEffect::None);
    let preview = controller
        .app
        .keybindings
        .as_ref()
        .expect("which-key preview");
    assert!(
        preview
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .any(|item| item.key == "↓" && item.label == "View subagents")
    );
}

#[test]
fn leader_preview_expires_without_consuming_next_text_key() {
    // Given
    let mut controller = Controller::new(AppState::default());
    let start = Instant::now();
    assert_eq!(controller.handle_key_at(ctrl('x'), start), TuiEffect::None);
    assert!(controller.app.keybindings.is_some());

    // When
    let effect = controller.handle_key_at(
        key(KeyCode::Char('z')),
        start + Duration::from_millis(2_001),
    );

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert!(controller.app.keybindings.is_none());
    assert_eq!(controller.app.input, "z");
}

#[test]
fn leader_preview_expires_on_idle_tick() {
    // Given
    let mut controller = Controller::new(AppState::default());
    let start = Instant::now();
    assert_eq!(controller.handle_key_at(ctrl('x'), start), TuiEffect::None);
    assert!(controller.app.keybindings.is_some());
    assert_eq!(
        controller.leader_keybindings_timeout_at(start),
        Some(Duration::from_millis(2_000))
    );

    // When
    let too_early = controller.expire_leader_keybindings_at(start + Duration::from_millis(1_999));

    // Then
    assert!(!too_early);
    assert!(controller.app.keybindings.is_some());

    // When
    let expired = controller.expire_leader_keybindings_at(start + Duration::from_millis(2_001));

    // Then
    assert!(expired);
    assert!(controller.app.keybindings.is_none());
}
