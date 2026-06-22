#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use yaca_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};
use yaca_tui::{AppState, draw};

#[test]
fn selected_message_surface_extends_to_the_padded_timeline_edge() {
    // Given: a selected assistant message with short text on a narrow shell.
    let mut state = AppState {
        selected_message: Some(0),
        ..AppState::default()
    };
    with_text_message(&mut state, 1, Role::Assistant, "selected assistant block");

    // When: the OpenCode-style transcript is rendered without the sidebar.
    let width = 80;
    let height = 18;
    let buffer = render_buffer(&mut state, width, height);

    // Then: the selected block surface fills the padded content row, not the terminal gutter.
    let (_x, y) = find_rendered_text(&buffer, width, height, "selected assistant block").unwrap();
    assert_eq!(
        buffer[(width - 3, y)].bg,
        Color::Rgb(24, 48, 58),
        "selected block rows should paint through the padded main content width"
    );
    assert_eq!(
        buffer[(width - 1, y)].bg,
        Color::Reset,
        "right terminal gutter should remain outside the selected message block"
    );
}

#[test]
fn user_message_renders_as_an_opencode_panel_block_when_not_selected() {
    // Given: a normal user message on a narrow OpenCode-style shell.
    let mut state = AppState::default();
    with_text_message(&mut state, 1, Role::User, "panel-backed user prompt");

    // When: the transcript is rendered without selecting the message.
    let width = 80;
    let height = 18;
    let buffer = render_buffer(&mut state, width, height);

    // Then: the user prompt is enclosed by panel-backed padding rows.
    let (_x, y) = find_rendered_text(&buffer, width, height, "panel-backed user prompt").unwrap();
    assert_eq!(
        buffer[(2, y - 1)].bg,
        Color::Rgb(20, 20, 20),
        "OpenCode user blocks keep a top panel padding row"
    );
    assert_eq!(
        buffer[(width - 3, y)].bg,
        Color::Rgb(20, 20, 20),
        "OpenCode user blocks paint through the padded main content width"
    );
    assert_eq!(
        buffer[(2, y + 1)].bg,
        Color::Rgb(20, 20, 20),
        "OpenCode user blocks keep a bottom panel padding row"
    );
    assert_eq!(
        buffer[(width - 1, y)].bg,
        Color::Reset,
        "right terminal gutter should remain outside the user message block"
    );
}

fn render_buffer(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    terminal.backend().buffer().clone()
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

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}
