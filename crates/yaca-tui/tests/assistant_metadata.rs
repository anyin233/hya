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
}
