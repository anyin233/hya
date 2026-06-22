#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use ratatui::buffer::Buffer;
use render_support::render_buffer;
use yaca_tui::{AppState, ContextView};

fn row_text(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

#[test]
fn composer_metadata_hides_context_and_cost_below_compact_width() {
    // Given: an OpenCode-style composer with context and billing metadata.
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

    // When: the footer renders just below OpenCode's compact metadata breakpoint.
    let buffer = render_buffer(&mut state, 79, 16);
    let metadata_row = row_text(&buffer, 79, 13);

    // Then: high-priority agent identity and command affordance remain, while
    // lower-priority model/context/cost hints follow OpenCode's width policy.
    assert!(
        metadata_row.contains("sisyphus"),
        "agent identity should remain visible, got {metadata_row:?}"
    );
    assert!(
        !metadata_row.contains("kimi-k2"),
        "model identity should be hidden below OpenCode's model breakpoint, got {metadata_row:?}"
    );
    assert!(
        metadata_row.contains("ctrl+p commands"),
        "command hint should remain visible above the command breakpoint, got {metadata_row:?}"
    );
    assert!(
        !metadata_row.contains("187.8K"),
        "context usage should be hidden below compact width, got {metadata_row:?}"
    );
    assert!(
        !metadata_row.contains("$3.14"),
        "billing should be hidden below compact width, got {metadata_row:?}"
    );
}

#[test]
fn composer_metadata_uses_bare_effort_without_manual_mode() {
    // Given: the active model has an OpenCode-style effort variant.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        reasoning_effort: Some("max".to_string()),
        ..AppState::default()
    };

    // When: the composer metadata row renders at the model breakpoint.
    let buffer = render_buffer(&mut state, 120, 16);
    let metadata_row = row_text(&buffer, 120, 13);

    // Then: the effort reads like OpenCode's model variant label, not a prose mode.
    assert!(
        metadata_row.contains("sisyphus · kimi-k2 · max"),
        "composer metadata should show bare effort after model, got {metadata_row:?}"
    );
    assert!(
        !metadata_row.contains("think max"),
        "composer metadata should not prefix effort with 'think', got {metadata_row:?}"
    );
    assert!(
        !metadata_row.contains("manual"),
        "composer metadata should not show manual mode in the OpenCode statusline, got {metadata_row:?}"
    );
}
