#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use yaca_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};
use yaca_tui::AppState;

use render_support::{find_rendered_text, render_buffer};

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

fn with_reasoning_message(state: &mut AppState, reasoning: &str, answer: &str) {
    let session = SessionId::new();
    let message = MessageId::new();
    let reasoning_part = PartId::new();
    let text_part = PartId::new();
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
        Event::ReasoningStart {
            session,
            message,
            part: reasoning_part,
        },
    ));
    state.apply(&env(
        3,
        Event::ReasoningDelta {
            session,
            message,
            part: reasoning_part,
            delta: reasoning.to_string(),
        },
    ));
    state.apply(&env(
        4,
        Event::TextStart {
            session,
            message,
            part: text_part,
        },
    ));
    state.apply(&env(
        5,
        Event::TextDelta {
            session,
            message,
            part: text_part,
            delta: answer.to_string(),
        },
    ));
}

#[test]
fn assistant_reasoning_renders_as_readable_thinking_block() {
    let mut state = AppState::default();
    with_reasoning_message(
        &mut state,
        "Need inspect the config before answering.",
        "Done.",
    );

    let buffer = render_buffer(&mut state, 80, 18);
    let (_label_x, label_y) = find_rendered_text(&buffer, 80, 18, "Thinking").unwrap();
    let (_text_x, text_y) = find_rendered_text(&buffer, 80, 18, "Need inspect the config").unwrap();
    let (_answer_x, answer_y) = find_rendered_text(&buffer, 80, 18, "Done.").unwrap();

    assert!(
        text_y > label_y,
        "reasoning text should sit below the thinking label"
    );
    assert!(
        answer_y > text_y,
        "assistant answer should render after the reasoning block"
    );
}
