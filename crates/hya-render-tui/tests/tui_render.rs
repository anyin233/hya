#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default
)]

use hya_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use hya_render_tui::{
    AppState, DialogItem, DialogView, GoalView, LoopView, PermissionPrompt, draw,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use serde_json::json;

fn render_buffer(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, state)).unwrap();
    terminal.backend().buffer().clone()
}

fn render(state: &mut AppState, width: u16, height: u16) -> String {
    let buffer = render_buffer(state, width, height);
    buffer_text(&buffer, width, height)
}

fn buffer_text(buffer: &Buffer, width: u16, height: u16) -> String {
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn find_rendered_text(
    buffer: &Buffer,
    width: u16,
    height: u16,
    needle: &str,
) -> Option<(u16, u16)> {
    for y in 0..height {
        let mut row = String::new();
        for x in 0..width {
            row.push_str(buffer[(x, y)].symbol());
        }
        if let Some(x) = row.find(needle) {
            return Some((u16::try_from(x).unwrap(), y));
        }
    }
    None
}

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

fn with_text_message(state: &mut AppState, base_seq: u64, role: Role, text: &str) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    state.apply(&env(
        base_seq,
        Event::MessageStarted {
            session,
            message,
            role,
        },
    ));
    state.apply(&env(
        base_seq + 1,
        Event::TextStart {
            session,
            message,
            part,
        },
    ));
    state.apply(&env(
        base_seq + 2,
        Event::TextDelta {
            session,
            message,
            part,
            delta: text.to_string(),
        },
    ));
}

fn with_assistant_message(state: &mut AppState, text: &str) {
    with_text_message(state, 1, Role::Assistant, text);
}

fn with_user_message(state: &mut AppState, text: &str) {
    with_text_message(state, 10, Role::User, text);
}

fn with_tool_message(state: &mut AppState, base_seq: u64, path: &str, time_ms: u64) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new("read");
    state.apply(&env(
        base_seq,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
        },
    ));
    state.apply(&env(
        base_seq + 1,
        Event::ToolInputStart {
            session,
            message,
            part,
            call,
            name: name.clone(),
        },
    ));
    state.apply(&env(
        base_seq + 2,
        Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name,
            input: json!({ "path": path }),
        },
    ));
    state.apply(&env(
        base_seq + 3,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output: json!({ "ok": true }),
            time_ms,
        },
    ));
}

fn with_tool_error_message(state: &mut AppState, base_seq: u64, path: &str, message_text: &str) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new("read");
    state.apply(&env(
        base_seq,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
        },
    ));
    state.apply(&env(
        base_seq + 1,
        Event::ToolInputStart {
            session,
            message,
            part,
            call,
            name: name.clone(),
        },
    ));
    state.apply(&env(
        base_seq + 2,
        Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name,
            input: json!({ "path": path }),
        },
    ));
    state.apply(&env(
        base_seq + 3,
        Event::ToolError {
            session,
            message,
            part,
            call,
            value: None,
            message_text: message_text.to_string(),
        },
    ));
}

fn rich_state() -> AppState {
    let mut state = AppState {
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        input: "type here".to_string(),
        goal: Some(GoalView {
            condition: "tests pass".to_string(),
            turns: 2,
            last_reason: "not yet".to_string(),
        }),
        loop_view: Some(LoopView {
            target: "improve".to_string(),
            iteration: 1,
            budget: 5,
            last_score: 60,
        }),
        team: vec![("alice".to_string(), "active".to_string())],
        ..AppState::default()
    };
    with_assistant_message(&mut state, "HELLOTUI");
    state
}

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
    assert!(text.contains("│"), "timeline should use a left rail");
    assert!(
        text.contains("tool read completed"),
        "completed tool should render as a compact status row"
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
    assert!(text.contains("hya"), "status banner must render");
    assert!(text.contains("Ask hya"), "empty-state hint must render");
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
    assert!(text.contains("⚙ read"), "tool name renders");
    assert!(text.contains("Cargo.toml"), "brief input renders");
    assert!(text.contains("7ms"), "completion time renders");
    assert_eq!(text.matches('⚙').count(), 1, "exactly one tool line");
}
