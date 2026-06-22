#![allow(dead_code)]

mod render_support;

use render_support::{render, with_tool_message, with_user_message};
use yaca_tui::AppState;

#[test]
fn timeline_renders_message_rails_and_compact_tool_actions() {
    // Given: a user turn followed by a successful read tool action.
    let mut state = AppState {
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        ..AppState::default()
    };
    with_user_message(&mut state, "please inspect files");
    with_tool_message(&mut state, 20, "README.md", 12);

    // When: the transcript renders at a wide OpenCode-style layout.
    let text = render(&mut state, 120, 30);

    // Then: the block keeps role/rail structure while successful tools stay compact.
    assert!(text.contains("You"), "user label should render");
    assert!(text.contains("▏"), "timeline should use a left rail");
    assert!(
        text.contains("→ Read README.md"),
        "completed read tool should render as an OpenCode-style action row"
    );
    assert!(
        !text.contains("completed ✓ 12ms"),
        "successful OpenCode-style tool rows should omit generic completed/time suffixes"
    );
}

#[test]
fn successful_tool_call_renders_as_one_compact_action_line() {
    // Given: a successful read tool call.
    let mut state = AppState::default();
    with_tool_message(&mut state, 1, "Cargo.toml", 7);

    // When: the transcript is rendered.
    let text = render(&mut state, 100, 12);

    // Then: the action is one compact OpenCode-style row with no generic timing suffix.
    assert!(text.contains("→ Read Cargo.toml"), "tool action renders");
    assert!(text.contains("Cargo.toml"), "brief input renders");
    assert!(
        !text.contains("7ms"),
        "successful compact tool row should omit generic completion timing"
    );
    assert_eq!(text.matches("→ Read").count(), 1, "exactly one tool line");
}
