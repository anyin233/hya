#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use ratatui::style::Color;
use yaca_tui::AppState;

use render_support::{buffer_text, find_rendered_text, render_buffer, with_tool_error_message};

#[test]
fn tool_error_detail_renders_below_failed_tool_row() {
    let mut state = AppState::default();
    with_tool_error_message(&mut state, 1, "README.md", "permission denied");

    let buffer = render_buffer(&mut state, 120, 24);
    let text = buffer_text(&buffer, 120, 24);
    assert!(text.contains("× Read README.md"));
    assert!(text.contains("error ✗"));
    assert!(text.contains("permission denied"));
    assert!(
        !text.contains("error ✗ permission denied"),
        "error detail should not be collapsed into the inline status row"
    );

    // Given: a failed tool row rendered in the assistant transcript.
    let (status_x, status_y) = find_rendered_text(&buffer, 120, 24, "× Read README.md").unwrap();

    // When: the tool carries an error message.
    let (detail_x, detail_y) = find_rendered_text(&buffer, 120, 24, "permission denied").unwrap();

    // Then: the detail is a separate OpenCode-style error row under the tool.
    assert!(
        detail_y > status_y,
        "tool error detail should render below the failed tool row"
    );
    assert_eq!(buffer[(status_x, status_y)].fg, Color::Rgb(224, 108, 117));
    assert_eq!(buffer[(detail_x, detail_y)].fg, Color::Rgb(224, 108, 117));
}
