#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use yaca_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};
use yaca_tui::{AppState, draw};

fn render_buffer(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    terminal.backend().buffer().clone()
}

fn rendered_row(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

fn with_text_message(state: &mut AppState, role: Role, text: &str) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    state.apply(&env(
        1,
        Event::MessageStarted {
            session,
            message,
            role,
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
fn main_stream_starts_at_top_without_status_banner() {
    // Given: a narrow OpenCode-style shell with one assistant block.
    let mut state = AppState {
        agent: "build".to_string(),
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        ..AppState::default()
    };
    with_text_message(&mut state, Role::Assistant, "top stream");

    // When: the shell renders without the wide context rail.
    let buffer = render_buffer(&mut state, 80, 16);
    let first_row = rendered_row(&buffer, 80, 0);

    // Then: the stream owns the first row instead of an extra status banner.
    assert!(
        first_row.contains("yaca #1"),
        "first row should be the selected transcript stream, got {first_row:?}"
    );
    assert!(
        !first_row.contains(" · build · fake · sess-1"),
        "OpenCode shell should not reserve a top status banner"
    );
}
