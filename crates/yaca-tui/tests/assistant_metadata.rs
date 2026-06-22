#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use yaca_proto::Role;
use yaca_tui::AppState;

use render_support::{find_rendered_text, render_buffer, with_text_message};

fn row_text(buffer: &ratatui::buffer::Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

fn assistant_metadata_rows(
    buffer: &ratatui::buffer::Buffer,
    width: u16,
    height: u16,
    needle: &str,
) -> Vec<String> {
    let mut rows = Vec::new();
    for y in 0..height {
        let row = row_text(buffer, width, y);
        if row.starts_with("   ") && row.contains(needle) {
            rows.push(row);
        }
    }
    rows
}

#[test]
fn assistant_block_renders_message_metadata_footer() {
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        ..AppState::default()
    };
    with_text_message(&mut state, 1, Role::Assistant, "metadata ready");

    let buffer = render_buffer(&mut state, 80, 16);
    let (_x, text_y) = find_rendered_text(&buffer, 80, 16, "metadata ready").unwrap();
    let metadata_row = row_text(&buffer, 80, text_y + 1);

    assert!(
        metadata_row.contains("sisyphus · kimi-k2 · completed"),
        "assistant metadata footer should sit under the message text, got {metadata_row:?}"
    );
    assert!(
        metadata_row.contains("▣ sisyphus"),
        "assistant metadata footer should use the OpenCode turn marker, got {metadata_row:?}"
    );
}

#[test]
fn assistant_metadata_footer_includes_active_agent_role() {
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        team: vec![("sisyphus".to_string(), "ultraworker retry".to_string())],
        ..AppState::default()
    };
    with_text_message(&mut state, 1, Role::Assistant, "metadata with role");

    let buffer = render_buffer(&mut state, 100, 16);
    let (_x, text_y) = find_rendered_text(&buffer, 100, 16, "metadata with role").unwrap();
    let metadata_row = row_text(&buffer, 100, text_y + 1);

    assert!(
        metadata_row.contains("sisyphus - ultraworker retry · kimi-k2 · completed"),
        "assistant metadata should include the active agent role, got {metadata_row:?}"
    );
}

#[test]
fn only_latest_assistant_block_reports_streaming_when_turn_is_running() {
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        running: true,
        ..AppState::default()
    };
    with_text_message(&mut state, 1, Role::Assistant, "first response");
    with_text_message(&mut state, 10, Role::Assistant, "second response");

    let buffer = render_buffer(&mut state, 100, 20);
    let metadata_rows = assistant_metadata_rows(&buffer, 100, 20, "sisyphus · kimi-k2 ·");

    assert_eq!(
        metadata_rows.len(),
        2,
        "expected one metadata footer per assistant block, got {metadata_rows:?}"
    );
    assert!(
        metadata_rows[0].contains("sisyphus · kimi-k2 · completed"),
        "older assistant block should remain completed, got {:?}",
        metadata_rows[0]
    );
    assert!(
        metadata_rows[1].contains("sisyphus · kimi-k2 · streaming"),
        "latest assistant block should report streaming, got {:?}",
        metadata_rows[1]
    );
}

#[test]
fn prior_assistant_block_stays_completed_while_new_user_turn_waits() {
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        running: true,
        ..AppState::default()
    };
    with_text_message(&mut state, 1, Role::Assistant, "previous response");
    with_text_message(&mut state, 10, Role::User, "new prompt");

    let buffer = render_buffer(&mut state, 100, 20);
    let metadata_rows = assistant_metadata_rows(&buffer, 100, 20, "sisyphus · kimi-k2 ·");

    assert_eq!(
        metadata_rows.len(),
        1,
        "expected only the existing assistant footer, got {metadata_rows:?}"
    );
    assert!(
        metadata_rows[0].contains("sisyphus · kimi-k2 · completed"),
        "previous assistant block should remain completed while waiting, got {:?}",
        metadata_rows[0]
    );
}
