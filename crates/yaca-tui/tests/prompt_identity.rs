#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
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
        metadata_row.contains("Sisyphus · kimi-k2 anthropic · max"),
        "composer identity should show provider between model and effort, got {metadata_row:?}"
    );
}

#[test]
fn composer_identity_styles_effort_like_opencode_variant() {
    // Given: the active model has an OpenCode-style effort variant.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        reasoning_effort: Some("max".to_string()),
        ..AppState::default()
    };

    // When: the prompt identity row renders.
    let buffer = render_buffer(&mut state, 120, 16);
    let metadata_row = find_row(&buffer, 120, 16, "max");
    let effort_x = u16::try_from(metadata_row.find("max").unwrap()).unwrap();
    let effort_y = (0..16)
        .find(|&y| rendered_row(&buffer, 120, y).contains("max"))
        .unwrap();
    let cell = &buffer[(effort_x, effort_y)];

    // Then: effort matches OpenCode's warning-colored bold model variant style.
    assert_eq!(cell.fg, Color::Rgb(245, 167, 66));
    assert!(
        cell.modifier.contains(Modifier::BOLD),
        "effort variant should be bold, got {:?}",
        cell.modifier
    );
}

#[test]
fn composer_identity_omits_empty_team_status_suffix() {
    // Given: the active agent has a team entry without a role/status label.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        team: vec![("sisyphus".to_string(), String::new())],
        ..AppState::default()
    };

    // When: the prompt identity row renders.
    let buffer = render_buffer(&mut state, 120, 16);
    let metadata_row = find_row(&buffer, 120, 16, "Sisyphus");

    // Then: it keeps the bare agent name instead of a dangling separator.
    assert!(
        metadata_row.contains("Sisyphus · kimi-k2"),
        "composer identity should omit empty team status, got {metadata_row:?}"
    );
    assert!(
        !metadata_row.contains("Sisyphus - "),
        "composer identity should not render a dangling team separator, got {metadata_row:?}"
    );
}
