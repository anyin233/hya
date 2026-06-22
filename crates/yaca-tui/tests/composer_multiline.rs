#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use ratatui::buffer::Buffer;
use render_support::render_buffer;
use yaca_tui::AppState;

fn row_text(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

fn row_index(buffer: &Buffer, width: u16, height: u16, needle: &str) -> u16 {
    (0..height)
        .find(|&y| row_text(buffer, width, y).contains(needle))
        .unwrap()
}

#[test]
fn composer_body_expands_upward_for_multiline_input() {
    // Given: an OpenCode-style composer draft with multiple input rows.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        input: "first line\nsecond line\nthird line".to_string(),
        ..AppState::default()
    };

    // When: the shell renders the grounded composer.
    let width = 80;
    let height = 18;
    let buffer = render_buffer(&mut state, width, height);

    // Then: every input row is visible directly above the metadata row.
    let first = row_index(&buffer, width, height, "first line");
    let second = row_index(&buffer, width, height, "second line");
    let third = row_index(&buffer, width, height, "third line");
    let metadata = row_index(&buffer, width, height, "ctrl+p commands");
    assert_eq!(second, first + 1);
    assert_eq!(third, second + 1);
    assert_eq!(metadata, third + 1);
    assert_eq!(
        metadata,
        height - 1,
        "composer metadata should stay attached to the viewport bottom"
    );
}
