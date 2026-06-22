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

fn rendered_row(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

fn find_row(buffer: &Buffer, width: u16, height: u16, needle: &str) -> String {
    (0..height)
        .map(|y| rendered_row(buffer, width, y))
        .find(|row| row.contains(needle))
        .unwrap()
}

#[test]
fn composer_identity_shows_provider_between_model_and_effort() {
    // Given: the active prompt identity has agent, model, provider, and effort.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        model_provider_label: Some("anthropic".to_string()),
        reasoning_effort: Some("max".to_string()),
        ..AppState::default()
    };

    // When: the prompt identity row renders at OpenCode's model breakpoint.
    let buffer = render_buffer(&mut state, 120, 16);
    let metadata_row = find_row(&buffer, 120, 16, "max");

    // Then: the provider label appears after the model and before effort.
    assert!(
        metadata_row.contains("sisyphus · kimi-k2 anthropic · max"),
        "composer identity should show provider between model and effort, got {metadata_row:?}"
    );
}
