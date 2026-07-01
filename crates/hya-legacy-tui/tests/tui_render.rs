#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default
)]

use hya_legacy_tui::{
    AppState, DialogItem, DialogView, GoalView, LoopView, PermissionPrompt, draw,
};
use hya_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
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

fn render_cursor_position(
    state: &mut AppState,
    width: u16,
    height: u16,
) -> ratatui::layout::Position {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, state)).unwrap();
    terminal.get_cursor_position().unwrap()
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

fn buffer_row_text(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut out = String::new();
    for x in 0..width {
        out.push_str(buffer[(x, y)].symbol());
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
    with_named_tool_message(state, base_seq, "read", json!({ "path": path }), time_ms);
}

fn with_named_tool_message(
    state: &mut AppState,
    base_seq: u64,
    tool_name: &str,
    input: serde_json::Value,
    time_ms: u64,
) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new(tool_name);
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
            input,
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
fn long_prompt_expands_and_wraps_at_80_columns() {
    let mut state = AppState {
        input: "alpha ".repeat(30),
        ..AppState::default()
    };
    let buffer = render_buffer(&mut state, 80, 24);
    let first = find_rendered_text(&buffer, 80, 24, "> alpha").expect("prompt first row renders");
    assert_eq!(
        first.1, 19,
        "three wrapped text rows plus border should raise prompt"
    );
    assert!(
        buffer_row_text(&buffer, 80, first.1 + 1).contains("alpha"),
        "wrapped prompt text should render below first row"
    );
}

#[test]
fn very_long_prompt_caps_visible_rows() {
    let mut state = AppState {
        input: "beta ".repeat(180),
        ..AppState::default()
    };
    let buffer = render_buffer(&mut state, 80, 30);
    let top = find_rendered_text(&buffer, 80, 30, "beta").expect("prompt tail renders");

    assert_eq!(
        top.1, 22,
        "six visible text rows plus border is the max prompt height"
    );
    assert!(
        buffer_row_text(&buffer, 80, top.1 + 5).contains("beta"),
        "tail viewport should fill the capped prompt rows"
    );
}

#[test]
fn wrapped_prompt_cursor_stays_inside_prompt_area() {
    let mut state = AppState {
        input: "gamma ".repeat(40),
        ..AppState::default()
    };
    let buffer = render_buffer(&mut state, 80, 24);
    let first = find_rendered_text(&buffer, 80, 24, "> gamma").expect("prompt first row renders");
    let cursor = render_cursor_position(&mut state, 80, 24);

    assert!(cursor.y > first.1, "cursor should move to the wrapped row");
    assert!(cursor.y < 23, "cursor should stay inside prompt border");
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

#[test]
fn running_tool_call_renders_pending_status_before_result() {
    let mut state = AppState::default();
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new("bash");
    state.apply(&env(
        1,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
        },
    ));
    state.apply(&env(
        2,
        Event::ToolInputStart {
            session,
            message,
            part,
            call,
            name: name.clone(),
        },
    ));
    state.apply(&env(
        3,
        Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name,
            input: json!({ "command": "cargo test" }),
        },
    ));

    let text = render(&mut state, 100, 12);
    assert!(text.contains("⚙ bash"));
    assert!(text.contains("tool bash running"));
    assert!(text.contains("cargo test"));
}

#[test]
fn tool_error_row_renders_status_message_and_error_color() {
    let mut state = AppState::default();
    with_tool_error_message(&mut state, 1, "README.md", "permission denied");

    let buffer = render_buffer(&mut state, 100, 12);
    let text = buffer_text(&buffer, 100, 12);
    assert!(text.contains("⚙ read"));
    assert!(text.contains("tool read error"));
    assert!(text.contains("permission denied"));
    let (x, y) = find_rendered_text(&buffer, 100, 12, "tool read error").unwrap();
    assert_eq!(buffer[(x, y)].fg, Color::Rgb(224, 108, 117));
}

#[test]
fn mcp_tool_call_renders_through_compact_tool_row() {
    let mut state = AppState::default();
    with_named_tool_message(
        &mut state,
        1,
        "mcp__filesystem__read_file",
        json!({ "path": "README.md" }),
        9,
    );

    let text = render(&mut state, 160, 12);
    assert!(text.contains("⚙ mcp__filesystem__read_file"));
    assert!(text.contains("tool mcp__filesystem__read_file completed"));
    assert!(text.contains("README.md"));
    assert!(text.contains("9ms"));
}

#[test]
fn subagent_permission_view_names_origin_session() {
    let mut state = AppState {
        permission: Some(PermissionPrompt {
            title: "bash · subagent abc12345".to_string(),
            detail: "cargo test -p hya-core".to_string(),
            selected: 0,
            reply: String::new(),
        }),
        ..AppState::default()
    };

    let text = render(&mut state, 100, 20);
    assert!(text.contains("permission required"));
    assert!(text.contains("bash · subagent abc12345"));
    assert!(text.contains("cargo test -p hya-core"));
}

#[test]
fn multiline_prompt_wraps_and_preserves_line_order() {
    let mut state = AppState {
        input: format!("first line\n{}", "second ".repeat(20)),
        ..AppState::default()
    };

    let buffer = render_buffer(&mut state, 80, 24);
    let text = buffer_text(&buffer, 80, 24);
    let first = find_rendered_text(&buffer, 80, 24, "> first line").expect("first line");
    let second = find_rendered_text(&buffer, 80, 24, "second").expect("second line");

    assert!(
        second.1 > first.1,
        "explicit newline should render below first line"
    );
    assert!(text.contains("second"));
}

#[test]
fn switch_states_are_visible_in_prompt_footer_and_sidebar() {
    let mut state = AppState {
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        yolo: true,
        running: true,
        reasoning_effort: Some("high".to_string()),
        ..AppState::default()
    };
    let text = render(&mut state, 120, 24);
    assert!(text.contains("YOLO"));
    assert!(text.contains("Tab yolo off") || text.contains("Tab disables auto-allow"));
    assert!(
        text.contains("● streaming"),
        "status bar should show streaming switch state"
    );
    assert!(
        text.contains("state streaming"),
        "sidebar should show streaming state"
    );
    assert!(
        text.contains("think:high"),
        "status bar should show reasoning effort"
    );
    assert!(
        text.contains("think high"),
        "sidebar should show reasoning effort"
    );
}

#[test]
fn sidebar_displays_attachments_and_reasoning_state() {
    let mut state = AppState {
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        reasoning_effort: Some("max".to_string()),
        attachments: vec![hya_legacy_tui::PromptAttachment {
            placeholder: "[Image #1]".to_string(),
            source_path: Some("/tmp/screenshot.png".to_string()),
            mime: "image/png".to_string(),
        }],
        ..AppState::default()
    };
    with_user_message(&mut state, "see image");

    let text = render(&mut state, 120, 24);
    assert!(text.contains("attachments 1"));
    assert!(text.contains("[Image #1]"));
    assert!(text.contains("think max"));
}
