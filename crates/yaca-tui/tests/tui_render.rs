#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default
)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use yaca_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};
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
