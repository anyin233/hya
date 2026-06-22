#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default
)]

use ratatui::style::Color;
use yaca_proto::Role;
use yaca_tui::{AppState, DialogItem, DialogView, PermissionPrompt};

#[allow(dead_code)]
mod render_support;

use render_support::{
    buffer_text, find_rendered_text, render, render_buffer, rich_state, with_event_error,
    with_text_message, with_tool_error_message, with_tool_message, with_user_message,
};

#[test]
fn renders_chat_with_input_status_and_panels() {
    let mut state = rich_state();

    let text = render(&mut state, 120, 24);
    assert!(text.contains("HELLOTUI"), "assistant text must render");
    assert!(text.contains("fake"), "status must show model");
    assert!(text.contains("type here"), "input box must show typed text");
    assert!(text.contains("GOAL"), "goal indicator must render");
    assert!(text.contains("LOOP"), "loop indicator must render");
    assert!(text.contains("alice"), "team panel must render");
    assert!(text.contains("message"), "input box title must render");
}

#[test]
fn wide_layout_renders_sidebar_and_surface_labels() {
    let mut state = rich_state();
    let text = render(&mut state, 120, 36);
    assert!(
        text.contains("context"),
        "wide layout should show context sidebar"
    );
    assert!(text.contains("model fake"), "sidebar should show model");
    assert!(
        text.contains("session sess-1"),
        "sidebar should show session label"
    );
    assert!(text.contains("team"), "sidebar should summarize team");
}

#[test]
fn narrow_layout_hides_sidebar_without_hiding_prompt() {
    let mut state = rich_state();
    let text = render(&mut state, 80, 24);
    assert!(
        !text.contains("context"),
        "narrow layout should hide sidebar"
    );
    assert!(text.contains("type here"), "prompt must remain visible");
    assert!(text.contains("HELLOTUI"), "transcript must remain visible");
}

#[test]
fn timeline_renders_message_rails_and_tool_status() {
    let mut state = AppState {
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        ..AppState::default()
    };
    with_user_message(&mut state, "please inspect files");
    with_tool_message(&mut state, 20, "README.md", 12);
    let text = render(&mut state, 120, 30);
    assert!(text.contains("You"), "user label should render");
    assert!(text.contains("▏"), "timeline should use a left rail");
    assert!(
        text.contains("→ Read README.md"),
        "completed read tool should render as an OpenCode-style action row"
    );
    assert!(
        text.contains("completed ✓ 12ms"),
        "completed tool row should keep a readable status suffix"
    );
}

#[test]
fn system_turn_errors_render_with_error_rail_and_color() {
    let mut state = AppState {
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        ..AppState::default()
    };
    with_text_message(
        &mut state,
        30,
        Role::System,
        "turn error: http: 403 Forbidden\nquota exhausted",
    );

    let buffer = render_buffer(&mut state, 120, 24);
    let text = buffer_text(&buffer, 120, 24);
    assert!(
        text.contains("error turn error: http: 403 Forbidden"),
        "system errors should be promoted to an error row"
    );
    assert!(
        !text.contains("sys turn error"),
        "system errors should not be rendered as muted sys chatter"
    );
    let (x, y) = find_rendered_text(&buffer, 120, 24, "error turn error").unwrap();
    assert_eq!(
        buffer[(x, y)].fg,
        Color::Rgb(224, 108, 117),
        "error row label should use the theme error color"
    );
}

#[test]
fn event_errors_render_as_first_class_error_rows() {
    let mut state = AppState {
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        ..AppState::default()
    };
    // Given: the session receives a real protocol error event, not a synthetic
    // system text message injected by the controller.
    with_event_error(&mut state, 10, "provider", "quota exhausted");

    // When: the projected session is rendered.
    let buffer = render_buffer(&mut state, 120, 24);
    let text = buffer_text(&buffer, 120, 24);

    // Then: the error is a visible OpenCode-style error row.
    assert!(
        text.contains("error provider: quota exhausted"),
        "protocol errors should become readable timeline rows"
    );
    assert!(
        !text.contains("sys provider: quota exhausted"),
        "protocol errors should not be muted system chatter"
    );
    let (x, y) = find_rendered_text(&buffer, 120, 24, "error provider").unwrap();
    assert_eq!(
        buffer[(x, y)].fg,
        Color::Rgb(224, 108, 117),
        "protocol error rows should use the theme error color"
    );
}

#[test]
fn sidebar_summarizes_transcript_tools_and_errors() {
    let mut state = AppState {
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        ..AppState::default()
    };
    with_user_message(&mut state, "inspect README");
    with_text_message(&mut state, 20, Role::Assistant, "checking");
    with_tool_error_message(&mut state, 40, "README.md", "permission denied");
    with_text_message(
        &mut state,
        50,
        Role::System,
        "turn error: http: 403 Forbidden",
    );

    let text = render(&mut state, 120, 36);
    assert!(
        text.contains("transcript"),
        "sidebar should include a transcript section"
    );
    assert!(
        text.contains("messages 4"),
        "sidebar should count transcript messages"
    );
    assert!(text.contains("tools 1"), "sidebar should count tool calls");
    assert!(
        text.contains("errors 2"),
        "sidebar should count tool and system errors"
    );
}

#[test]
fn permission_panel_renders_options_and_reply() {
    let mut state = AppState::default();
    state.permission = Some(PermissionPrompt {
        title: "bash".to_string(),
        detail: "rm -rf /tmp/x".to_string(),
        selected: 2,
        reply: "use ls instead".to_string(),
    });
    let text = render(&mut state, 100, 20);
    assert!(text.contains("permission required"), "panel title renders");
    assert!(text.contains("rm -rf /tmp/x"), "command detail renders");
    assert!(text.contains("Allow once"), "allow-once option renders");
    assert!(
        text.contains("Allow all bash"),
        "allow-all option uses the action"
    );
    assert!(text.contains("Deny"), "deny option renders");
    assert!(text.contains("use ls instead"), "reply text renders");
}

#[test]
fn list_dialog_renders_selected_item_and_hints() {
    let mut state = AppState {
        dialog: Some(DialogView {
            title: "select model".to_string(),
            subtitle: "next turn uses the selected model".to_string(),
            items: vec![
                DialogItem {
                    label: "model-a".to_string(),
                    detail: "available".to_string(),
                },
                DialogItem {
                    label: "model-b".to_string(),
                    detail: "current".to_string(),
                },
            ],
            selected: 1,
        }),
        ..AppState::default()
    };

    let text = render(&mut state, 100, 24);
    assert!(text.contains("select model"), "dialog title renders");
    assert!(
        text.contains("next turn uses the selected model"),
        "dialog subtitle renders"
    );
    assert!(
        text.contains("> model-b"),
        "selected row renders with marker"
    );
    assert!(text.contains("Esc"), "dialog hint mentions cancel");
    assert!(text.contains("Enter"), "dialog hint mentions submit");
}

#[test]
fn default_state_renders_banner_and_hint() {
    let text = render(&mut AppState::default(), 80, 20);
    assert!(text.contains("yaca"), "status banner must render");
    assert!(text.contains("Ask yaca"), "empty-state hint must render");
}

#[test]
fn yolo_and_exit_armed_states_are_visible() {
    let mut state = AppState {
        yolo: true,
        exit_armed: true,
        ..AppState::default()
    };

    let text = render(&mut state, 100, 20);
    assert!(text.contains("YOLO"), "yolo mode should be visible");
    assert!(
        text.contains("Ctrl-C again"),
        "armed exit hint should be visible"
    );
}

#[test]
fn scroll_back_saturates() {
    let mut state = AppState::default();
    state.scroll_down(5);
    assert_eq!(state.scroll_back, 0);
    state.scroll_up(3);
    assert_eq!(state.scroll_back, 3);
    state.scroll_down(10);
    assert_eq!(state.scroll_back, 0);
}

#[test]
fn tool_call_renders_as_one_compact_line() {
    let mut state = AppState::default();
    with_tool_message(&mut state, 1, "Cargo.toml", 7);

    let text = render(&mut state, 100, 12);
    assert!(text.contains("→ Read Cargo.toml"), "tool action renders");
    assert!(text.contains("Cargo.toml"), "brief input renders");
    assert!(text.contains("7ms"), "completion time renders");
    assert_eq!(text.matches("→ Read").count(), 1, "exactly one tool line");
}
