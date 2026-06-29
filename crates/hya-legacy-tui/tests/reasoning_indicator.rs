#![allow(clippy::unwrap_used)]

use hya_legacy_tui::{AppState, draw};
use hya_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;

fn render_buffer(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, state)).unwrap();
    terminal.backend().buffer().clone()
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

fn with_started_reasoning(state: &mut AppState) {
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
        Event::ReasoningStart {
            session,
            message,
            part,
        },
    ));
}

#[test]
fn thinking_indicator_renders_when_reasoning_starts_before_text() {
    // Given: an assistant message has started reasoning before visible text streams.
    let mut state = AppState {
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        ..AppState::default()
    };
    with_started_reasoning(&mut state);

    // When: the TUI renders the active transcript.
    let buffer = render_buffer(&mut state, 120, 24);
    let text = buffer_text(&buffer, 120, 24);

    // Then: the user sees progress instead of the empty-state hint.
    assert!(
        text.contains("Thinking"),
        "reasoning start should show a visible progress indicator before text streams"
    );
    assert!(
        !text.contains("Ask hya"),
        "active reasoning should replace the empty-state hint"
    );
    let (x, y) = find_rendered_text(&buffer, 120, 24, "Thinking").unwrap();
    assert_eq!(
        buffer[(x, y)].fg,
        Color::Rgb(157, 124, 216),
        "thinking indicator should use the DESIGN.md accent token"
    );
}
