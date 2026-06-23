#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use yaca_tui::{AppState, PermissionPrompt, PermissionPromptStage, draw};

use render_support::render_buffer;

fn cursor_position(state: &mut AppState, width: u16, height: u16) -> Position {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    terminal.backend().cursor_position()
}

fn find_text_cell(buffer: &Buffer, width: u16, height: u16, needle: &str) -> Option<(u16, u16)> {
    for y in 0..height {
        for x in 0..width {
            let mut matched = true;
            for (offset, ch) in needle.chars().enumerate() {
                let x = x.saturating_add(u16::try_from(offset).unwrap());
                let mut cell = [0; 4];
                if x >= width || buffer[(x, y)].symbol() != ch.encode_utf8(&mut cell) {
                    matched = false;
                    break;
                }
            }
            if matched {
                return Some((x, y));
            }
        }
    }
    None
}

#[test]
fn reject_feedback_places_terminal_cursor_after_reply() {
    // Given: the reject feedback editor contains typed text.
    let mut state = AppState {
        permission: Some(PermissionPrompt {
            title: "bash".to_string(),
            detail: "rm -rf /tmp/x".to_string(),
            selected: 0,
            reply: "no thanks".to_string(),
            stage: PermissionPromptStage::Reject,
        }),
        ..AppState::default()
    };
    let width = 100;
    let height = 20;

    // When: the permission overlay renders.
    let buffer = render_buffer(&mut state, width, height);
    let (x, y) = find_text_cell(&buffer, width, height, "no thanks").unwrap();
    let cursor = cursor_position(&mut state, width, height);

    // Then: the live terminal cursor follows the typed reject feedback.
    assert_eq!(
        cursor,
        Position {
            x: x + "no thanks".len() as u16,
            y
        }
    );
}
