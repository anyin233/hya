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

#[test]
fn empty_first_composer_shows_opencode_placeholder() {
    // Given: a new shell has no prompt text and no transcript yet.
    let mut state = AppState::default();

    // When: the grounded composer renders.
    let width = 100;
    let height = 16;
    let buffer = render_buffer(&mut state, width, height);
    let prompt_y = row_index(&buffer, width, height, "Ask anything");
    let prompt_row = row_text(&buffer, width, prompt_y);

    // Then: it mirrors OpenCode's first-prompt placeholder.
    assert!(
        prompt_row.contains(r#"Ask anything... "Fix a TODO in the codebase""#),
        "empty composer should show the OpenCode placeholder, got {prompt_row:?}"
    );
    let placeholder_x = u16::try_from(prompt_row.find("Ask anything").unwrap()).unwrap();
    assert_eq!(
        buffer[(placeholder_x, prompt_y)].fg,
        Color::Rgb(128, 128, 128),
        "placeholder should use muted text"
    );
}

#[test]
fn running_first_turn_hides_empty_prompt_placeholder() {
    // Given: the first prompt was submitted and the turn is in flight before projection catches up.
    let mut state = AppState {
        running: true,
        ..AppState::default()
    };

    // When: the grounded composer renders with an empty input buffer.
    let width = 100;
    let height = 16;
    let buffer = render_buffer(&mut state, width, height);
    let mut text = String::new();
    for y in 0..height {
        text.push_str(&row_text(&buffer, width, y));
        text.push('\n');
    }

    // Then: it does not re-show the first-prompt placeholder while the turn is active.
    assert!(
        !text.contains("Ask anything"),
        "running first turn should hide the empty prompt placeholder:\n{text}"
    );
}
