#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use yaca_tui::{AppState, draw};

fn render_buffer(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    terminal.backend().buffer().clone()
}

fn row_text(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

fn find_text(buffer: &Buffer, width: u16, height: u16, needle: &str) -> Option<(u16, u16)> {
    for y in 0..height {
        let row = row_text(buffer, width, y);
        if let Some(x) = row.find(needle) {
            return Some((u16::try_from(x).unwrap(), y));
        }
    }
    None
}

#[test]
fn sidebar_stays_hidden_at_opencode_boundary_width() {
    // Given: OpenCode treats exactly 120 columns as non-wide.
    let mut state = AppState {
        session_label: "sess-1".to_string(),
        input: "type here".to_string(),
        ..AppState::default()
    };

    // When: the shell renders at the boundary width.
    let buffer = render_buffer(&mut state, 120, 18);
    let first_row = row_text(&buffer, 120, 0);

    // Then: no context rail is present at 120 columns.
    assert!(
        !first_row.contains("GUI sess-1"),
        "OpenCode only shows the sidebar when width is greater than 120 columns, got {first_row:?}"
    );
    assert_eq!(find_text(&buffer, 120, 18, "ContextPilot"), None);
}

#[test]
fn sidebar_uses_opencode_forty_two_column_width_when_wide() {
    // Given: a terminal just wider than OpenCode's auto-sidebar breakpoint.
    let width = 121;
    let mut state = AppState {
        session_label: "sess-1".to_string(),
        input: "type here".to_string(),
        ..AppState::default()
    };

    // When: the wide shell renders.
    let buffer = render_buffer(&mut state, width, 18);
    let (x, _y) = find_text(&buffer, width, 18, "GUI sess-1").unwrap();

    // Then: the sidebar occupies the same fixed 42-column rail as OpenCode.
    assert_eq!(
        x,
        width - 42 + 2,
        "OpenCode reserves a 42-column context rail with a two-column title gutter"
    );
}
