#![allow(clippy::expect_used)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::harness::DummyHarness;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

fn ctrl_up() -> KeyEvent {
    KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL)
}

fn ctrl_n() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)
}

#[tokio::test]
async fn branch_key_forks_to_selected_user_block_when_a_block_is_selected() {
    let mut harness = DummyHarness::new(vec!["dummy"]).await;
    harness.type_text("hello");
    harness.press(key(KeyCode::Enter)).await;
    assert!(harness.transcript().contains("dummy response"));

    harness.press(ctrl_up()).await;
    harness.press(ctrl_up()).await;
    harness.press(key(KeyCode::Char('b'))).await;

    let transcript = harness.transcript();
    assert!(transcript.contains("hello"));
    assert!(!transcript.contains("dummy response"));
}

#[tokio::test]
async fn branch_key_clears_previous_session_team_state() {
    let mut harness = DummyHarness::new(vec!["dummy"]).await;
    harness.type_text("hello");
    harness.press(key(KeyCode::Enter)).await;
    harness.set_team(vec![("review".to_string(), "running".to_string())]);

    harness.press(ctrl_up()).await;
    harness.press(ctrl_up()).await;
    harness.press(key(KeyCode::Char('b'))).await;

    assert!(harness.team().is_empty());
}

#[tokio::test]
async fn new_session_key_clears_previous_session_team_state() {
    let mut harness = DummyHarness::new(vec!["dummy"]).await;
    harness.set_team(vec![("review".to_string(), "running".to_string())]);

    harness.press(ctrl_n()).await;

    assert!(harness.team().is_empty());
}

#[tokio::test]
async fn revert_key_restores_selected_user_block_to_the_composer() {
    // Given: a completed turn with the original user prompt selected.
    let mut harness = DummyHarness::new(vec!["dummy"]).await;
    harness.type_text("hello");
    harness.press(key(KeyCode::Enter)).await;
    assert!(harness.transcript().contains("dummy response"));

    // When: the selected block is reverted.
    harness.press(ctrl_up()).await;
    harness.press(ctrl_up()).await;
    harness.press(key(KeyCode::Char('r'))).await;

    // Then: the completed turn is removed and the prompt is editable again.
    let transcript = harness.transcript();
    assert!(!transcript.contains("hello"));
    assert!(!transcript.contains("dummy response"));
    assert_eq!(harness.input(), "hello");
}
