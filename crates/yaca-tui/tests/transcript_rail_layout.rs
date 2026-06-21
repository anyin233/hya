#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use yaca_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};
use yaca_tui::AppState;

#[test]
fn user_transcript_rails_do_not_use_box_drawing_border_glyphs() {
    // Given: a normal user transcript block without any modal overlay.
    let mut state = AppState::default();
    with_text_message(&mut state, "read the file");

    // When: the transcript renders.
    let text = render(&mut state, 100, 20);

    // Then: the rail remains visible without being mistaken for a box border.
    let user_row = text
        .lines()
        .find(|row| row.contains("read the file"))
        .unwrap_or_else(|| panic!("user transcript row missing:\n{text}"));
    assert!(
        !user_row.contains('│'),
        "borderless transcript rails should not trip frame alignment checks:\n{user_row}"
    );
    assert!(
        user_row.contains("▏"),
        "the transcript should still render a visible tonal rail:\n{user_row}"
    );
}

fn render(state: &mut AppState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| yaca_tui::draw(frame, state)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut output = String::new();
    for y in 0..height {
        for x in 0..width {
            output.push_str(buffer[(x, y)].symbol());
        }
        output.push('\n');
    }
    output
}

fn with_text_message(state: &mut AppState, text: &str) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    state.apply(&env(
        1,
        Event::MessageStarted {
            session,
            message,
            role: Role::User,
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

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}
