use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

#[tokio::test]
async fn dummy_harness_switches_model_and_returns_fixed_response() {
    let mut harness = DummyHarness::new(vec!["alpha", "beta"]).await;

    harness.type_text("/model");
    harness.press(key(KeyCode::Enter)).await;
    harness.press(key(KeyCode::Down)).await;
    harness.press(key(KeyCode::Enter)).await;
    harness.type_text("hello");
    harness.press(key(KeyCode::Enter)).await;

    assert_eq!(harness.seen_models(), vec!["beta".to_string()]);
    assert!(harness.transcript().contains("dummy response"));
}
