#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default
)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use yaca_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};
use yaca_tui::{AppState, GoalView, LoopView, PermissionPrompt, Picker, QuestionPrompt, draw};

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

fn with_assistant_message(state: &mut AppState, text: &str) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
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
        Event::TextStart {
            session,
            message,
            part,
        },
    ));
    state.apply(&env(
        3,
        Event::TextDelta {
            session,
            message,
            part,
            delta: text.to_string(),
        },
    ));
}

#[test]
fn renders_chat_with_input_status_and_panels() {
    let mut state = AppState::default();
    state.model = "fake".to_string();
    state.session_label = "sess-1".to_string();
    state.input = "type here".to_string();
    with_assistant_message(&mut state, "HELLOTUI");
    state.goal = Some(GoalView {
        condition: "tests pass".to_string(),
        turns: 2,
        last_reason: "not yet".to_string(),
    });
    state.loop_view = Some(LoopView {
        target: "improve".to_string(),
        iteration: 1,
        budget: 5,
        last_score: 60,
    });
    state.team = vec![("alice".to_string(), "active".to_string())];

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
fn status_shows_yolo_when_enabled() {
    let mut state = AppState::default();
    state.model = "fake".to_string();
    state.session_label = "s".to_string();
    state.yolo = true;
    let text = render(&mut state, 120, 24);
    assert!(
        text.contains("YOLO"),
        "status must show YOLO pill when enabled"
    );
}

#[test]
fn question_overlay_renders_options() {
    let mut state = AppState::default();
    state.question = Some(QuestionPrompt {
        prompt: "pick a color".to_string(),
        options: vec!["red".to_string(), "green".to_string()],
        selected: 1,
        input: String::new(),
        allow_custom: false,
    });
    let text = render(&mut state, 100, 20);
    assert!(text.contains("question"), "panel title renders");
    assert!(text.contains("pick a color"), "prompt renders");
    assert!(text.contains("green"), "option renders");
}

#[test]
fn permission_overlay_renders_options_and_detail() {
    let mut state = AppState::default();
    state.permission = Some(PermissionPrompt {
        title: "bash".to_string(),
        detail: "rm -rf /tmp/x".to_string(),
        selected: 2,
    });
    let text = render(&mut state, 100, 20);
    assert!(text.contains("permission required"), "panel title renders");
    assert!(text.contains("rm -rf /tmp/x"), "command detail renders");
    assert!(text.contains("Allow once"), "allow-once option renders");
    assert!(
        text.contains("permission required"),
        "overlay title renders"
    );
    assert!(text.contains("rm -rf /tmp/x"), "command detail renders");
    assert!(text.contains("Allow once"), "allow-once option renders");
    assert!(text.contains("Allow all bash"), "allow-all uses the action");
    assert!(text.contains("Deny"), "deny option renders");
}

#[test]
fn session_picker_renders_entries() {
    let mut state = AppState::default();
    state.picker = Some(Picker {
        title: "sessions".to_string(),
        entries: vec![
            "ses_aaa (3 events)".to_string(),
            "ses_bbb (1 events)".to_string(),
        ],
        selected: 1,
    });
    let text = render(&mut state, 100, 20);
    assert!(text.contains("sessions"), "picker title renders");
    assert!(text.contains("ses_aaa"), "first entry renders");
    assert!(text.contains("ses_bbb"), "second entry renders");
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

#[test]
fn tool_call_renders_as_one_compact_line() {
    use serde_json::json;
    use yaca_proto::{ToolCallId, ToolName};

    let mut state = AppState::default();
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
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
            name: ToolName::new("read"),
        },
    ));
    state.apply(&env(
        3,
        Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name: ToolName::new("read"),
            input: json!({ "path": "Cargo.toml" }),
        },
    ));
    state.apply(&env(
        4,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output: json!({ "ok": true }),
            time_ms: 7,
        },
    ));

    let text = render(&mut state, 100, 12);
    assert!(text.contains("⚙ read"), "tool name renders");
    assert!(text.contains("Cargo.toml"), "brief input renders");
    assert!(text.contains("7ms"), "completion time renders");
    assert_eq!(text.matches('⚙').count(), 1, "exactly one tool line");
}
