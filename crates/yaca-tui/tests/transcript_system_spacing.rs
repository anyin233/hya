#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use yaca_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};
use yaca_tui::{AppState, draw};

fn render(state: &mut AppState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, state)).unwrap();
    buffer_text(terminal.backend().buffer(), width, height)
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

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

fn with_text_message(state: &mut AppState, seq: u64, role: Role, text: &str) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    state.apply(&env(
        seq,
        Event::MessageStarted {
            session,
            message,
            role,
        },
    ));
    state.apply(&env(
        seq + 1,
        Event::TextStart {
            session,
            message,
            part,
        },
    ));
    state.apply(&env(
        seq + 2,
        Event::TextDelta {
            session,
            message,
            part,
            delta: text.to_string(),
        },
    ));
}

#[test]
fn system_blocks_leave_blank_row_before_following_messages() {
    let mut state = AppState::default();

    // Given: a system transcript block followed by an assistant block.
    with_text_message(&mut state, 1, Role::System, "system note");
    with_text_message(&mut state, 10, Role::Assistant, "assistant reply");

    // When: the transcript renders in the OpenCode-style streaming area.
    let text = render(&mut state, 100, 16);
    let lines: Vec<&str> = text.lines().collect();
    let system_line = lines
        .iter()
        .position(|line| line.contains("sys system note"))
        .unwrap();

    // Then: the system block has the same trailing blank row as other blocks.
    assert!(
        lines
            .get(system_line + 1)
            .is_some_and(|line| line.trim().is_empty()),
        "system transcript blocks should not run into the next message"
    );
}
