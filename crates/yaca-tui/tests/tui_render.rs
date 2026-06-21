#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default
)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::{AppState, GoalView, LoopView, draw};

fn render(state: &mut AppState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, state)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
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

fn with_tool_message(state: &mut AppState) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new("read");
    state.apply(&env(
        20,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
        },
    ));
    state.apply(&env(
        21,
        Event::ToolInputStart {
            session,
            message,
            part,
            call,
            name: name.clone(),
        },
    ));
    state.apply(&env(
        22,
        Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name,
            input: json!({ "path": "README.md" }),
        },
    ));
    state.apply(&env(
        23,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output: json!({ "ok": true }),
            time_ms: 12,
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
    with_tool_message(&mut state);
    let text = render(&mut state, 120, 30);
    assert!(text.contains("You"), "user label should render");
    assert!(text.contains("│"), "timeline should use a left rail");
    assert!(
        text.contains("tool read completed"),
        "completed tool should render as a compact status row"
    );
}

#[test]
fn permission_takes_over_input_area() {
    let mut state = AppState::default();
    state.pending_permission = Some("bash: rm file".to_string());
    let text = render(&mut state, 100, 20);
    assert!(
        text.contains("PERMISSION REQUEST"),
        "permission prompt must render"
    );
}

#[test]
fn default_state_renders_banner_and_hint() {
    let text = render(&mut AppState::default(), 80, 20);
    assert!(text.contains("yaca"), "status banner must render");
    assert!(text.contains("Ask yaca"), "empty-state hint must render");
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
