#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use ratatui::style::{Color, Modifier};
use yaca_tui::AppState;

use render_support::{buffer_text, find_rendered_text, render_buffer, with_tool_error_message};

#[test]
fn tool_error_detail_renders_below_failed_tool_row() {
    let mut state = AppState::default();
    with_tool_error_message(&mut state, 1, "README.md", "permission denied");

    let buffer = render_buffer(&mut state, 120, 24);
    let text = buffer_text(&buffer, 120, 24);
    assert!(text.contains("× Read README.md"));
    assert!(text.contains("permission denied"));
    assert!(
        text.lines()
            .find(|line| line.contains("× Read README.md"))
            .is_some_and(|line| !line.contains("error ✗")),
        "failed tool action row should not append a generic error suffix:\n{text}"
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

#[test]
fn denied_tool_error_renders_struck_through_without_detail() {
    // Given: OpenCode classifies rejected permission errors as denied tool rows.
    let mut state = AppState::default();
    with_tool_error_message(
        &mut state,
        1,
        "README.md",
        "rejected permission: user dismissed",
    );

    // When: the transcript renders the denied tool row.
    let buffer = render_buffer(&mut state, 120, 24);
    let text = buffer_text(&buffer, 120, 24);
    let (status_x, status_y) = find_rendered_text(&buffer, 120, 24, "→ Read README.md").unwrap();

    // Then: the denied row is crossed out and does not expand as a failed error detail.
    assert!(
        buffer[(status_x, status_y)]
            .modifier
            .contains(Modifier::CROSSED_OUT),
        "denied tool icon should be struck through:\n{text}"
    );
    assert!(
        buffer[(status_x + 2, status_y)]
            .modifier
            .contains(Modifier::CROSSED_OUT),
        "denied tool label should be struck through:\n{text}"
    );
    assert!(
        !text.contains("rejected permission"),
        "denied tool row should not render rejected permission as a failed error detail:\n{text}"
    );
}
