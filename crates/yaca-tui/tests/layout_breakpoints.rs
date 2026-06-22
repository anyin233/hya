#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use yaca_proto::Role;
use yaca_tui::{AppState, draw};

use render_support::with_text_message;

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
    let (x, y) = find_text(&buffer, width, 18, "GUI sess-1").unwrap();

    // Then: the sidebar occupies the same fixed 42-column rail as OpenCode.
    assert_eq!(
        x,
        width - 42 + 2,
        "OpenCode reserves a 42-column context rail with a two-column title gutter"
    );
    assert_eq!(
        y, 1,
        "OpenCode applies one-row top padding before the context rail title"
    );
}

#[test]
fn composer_rail_uses_opencode_two_column_main_gutter() {
    // Given: a narrow shell where OpenCode still applies main-column padding.
    let mut state = AppState {
        input: "type here".to_string(),
        ..AppState::default()
    };

    // When: the grounded composer renders.
    let buffer = render_buffer(&mut state, 80, 16);
    let (x, y) = find_text(&buffer, 80, 16, "▌").unwrap();

    // Then: the composer rail starts after the OpenCode two-column main gutter.
    assert_eq!(
        x, 2,
        "OpenCode pads the main column by two cells before the composer rail"
    );
    assert_eq!(
        buffer[(x, y - 1)].bg,
        Color::Rgb(30, 30, 30),
        "OpenCode applies one-row input-surface padding above the composer text"
    );
}

#[test]
fn selected_block_surface_starts_after_opencode_main_gutter() {
    // Given: a selected transcript block in the main stream.
    let mut state = AppState {
        selected_message: Some(0),
        ..AppState::default()
    };
    with_text_message(&mut state, 1, Role::Assistant, "selected assistant block");

    // When: the transcript renders without the sidebar.
    let buffer = render_buffer(&mut state, 80, 18);
    let (_x, y) = find_text(&buffer, 80, 18, "selected assistant block").unwrap();

    // Then: the selectable surface fills the padded main column, not the terminal gutter.
    assert_eq!(
        buffer[(0, y)].bg,
        Color::Reset,
        "left terminal gutter should remain outside the selected message block"
    );
    assert_eq!(
        buffer[(1, y)].bg,
        Color::Reset,
        "second terminal gutter cell should remain outside the selected message block"
    );
    assert_eq!(
        buffer[(2, y)].bg,
        Color::Rgb(24, 48, 58),
        "selected block should begin at the OpenCode main content edge"
    );
}
