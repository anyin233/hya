#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default
)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::{AppState, GoalView, LoopView, draw};

pub fn render_buffer(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, state)).unwrap();
    terminal.backend().buffer().clone()
}

pub fn render(state: &mut AppState, width: u16, height: u16) -> String {
    let buffer = render_buffer(state, width, height);
    buffer_text(&buffer, width, height)
}

pub fn buffer_text(buffer: &Buffer, width: u16, height: u16) -> String {
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

pub fn find_rendered_text(
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

pub fn with_text_message(state: &mut AppState, base_seq: u64, role: Role, text: &str) {
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

pub fn with_assistant_message(state: &mut AppState, text: &str) {
    with_text_message(state, 1, Role::Assistant, text);
}

pub fn with_user_message(state: &mut AppState, text: &str) {
    with_text_message(state, 10, Role::User, text);
}

pub fn with_tool_message(state: &mut AppState, base_seq: u64, path: &str, time_ms: u64) {
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

pub fn with_tool_error_message(
    state: &mut AppState,
    base_seq: u64,
    path: &str,
    message_text: &str,
) {
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
            message_text: message_text.to_string(),
        },
    ));
}

pub fn with_event_error(state: &mut AppState, base_seq: u64, code: &str, message: &str) {
    let session = SessionId::new();
    state.apply(&env(
        base_seq,
        Event::SessionCreated {
            session,
            parent: None,
            agent: "build".into(),
            model: "fake".into(),
            workdir: "/tmp".into(),
        },
    ));
    state.apply(&env(
        base_seq + 1,
        Event::Error {
            session: Some(session),
            code: code.to_string(),
            message: message.to_string(),
        },
    ));
}

pub fn rich_state() -> AppState {
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
