#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use ratatui::buffer::Buffer;
use ratatui::style::Color;
use render_support::render_buffer;
use yaca_tui::{AppState, ContextView};

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
fn commands_and_usage_render_below_the_prompt_panel() {
    // Given: an OpenCode-style composer with identity, context, and billing metadata.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        reasoning_effort: Some("max".to_string()),
        cost_label: Some("$3.14".to_string()),
        context: ContextView {
            current_tokens: Some(187_750),
            context_window_tokens: Some(988_000),
            ..ContextView::default()
        },
        ..AppState::default()
    };

    // When: the grounded composer renders at a width that exposes usage metadata.
    let width = 120;
    let height = 16;
    let buffer = render_buffer(&mut state, width, height);

    // Then: agent/model identity stays inside the prompt panel and usage/commands
    // sit on the separate footer row below it, matching OpenCode's prompt split.
    let identity_y = row_index(&buffer, width, height, "Sisyphus · kimi-k2 · max");
    let footer_y = row_index(&buffer, width, height, "ctrl+p commands");
    let footer = row_text(&buffer, width, footer_y);
    assert_eq!(footer_y, identity_y + 1);
    assert!(
        footer.contains("187.8K (19%) · $3.14"),
        "footer should keep usage before billing, got {footer:?}"
    );
    assert!(
        !footer.contains("Sisyphus"),
        "footer should not duplicate prompt identity, got {footer:?}"
    );
    assert_eq!(
        buffer[(2, identity_y)].bg,
        Color::Rgb(30, 30, 30),
        "prompt identity row should keep the input panel background"
    );
    assert_eq!(
        buffer[(2, footer_y)].bg,
        Color::Rgb(10, 10, 10),
        "usage and command hints should render outside the input panel"
    );
}
