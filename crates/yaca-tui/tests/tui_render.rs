#![allow(clippy::unwrap_used, clippy::expect_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use yaca_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};
use yaca_tui::{AppState, GoalView, LoopView, draw};

fn render(state: &AppState, width: u16, height: u16) -> String {
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

#[test]
fn renders_messages_goal_loop_and_permission() {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();

    let mut state = AppState::default();
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
            delta: "HELLOTUI".to_string(),
        },
    ));
    state.goal = Some(GoalView {
        condition: "tests pass".to_string(),
        turns: 2,
        last_reason: "not yet".to_string(),
    });
    state.loop_view = Some(LoopView {
        target: "improve docs".to_string(),
        iteration: 1,
        budget: 5,
        last_score: 60,
    });
    state.team = vec![("alice".to_string(), "active".to_string())];
    state.pending_permission = Some("bash: rm file".to_string());

    let text = render(&state, 100, 24);
    assert!(text.contains("HELLOTUI"), "message text must render");
    assert!(text.contains("GOAL"), "goal bar must render");
    assert!(text.contains("LOOP"), "loop bar must render");
    assert!(
        text.contains("PERMISSION REQUEST"),
        "permission modal must render"
    );
    assert!(text.contains("alice"), "team panel must render");
}

#[test]
fn renders_default_state_without_panicking() {
    let state = AppState::default();
    let text = render(&state, 80, 20);
    assert!(text.contains("yaca"));
}
