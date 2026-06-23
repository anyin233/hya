#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use render_support::render;
use yaca_tui::AppState;

#[test]
fn selected_block_runtime_strip_names_copy_action() {
    // Given: a transcript block is selected and the composer is ready for block commands.
    let mut state = AppState {
        selected_message: Some(0),
        ..AppState::default()
    };

    // When: the runtime strip renders selected-block actions.
    let text = render(&mut state, 120, 16);
    let row = text
        .lines()
        .find(|row| row.contains("enter actions"))
        .unwrap();

    // Then: the visible affordance includes the existing copy action.
    assert!(
        row.contains("copy"),
        "selected block actions should advertise copy, got {row:?}"
    );
    assert!(
        row.contains("revert") && row.contains("branch"),
        "selected block actions should keep revert and branch, got {row:?}"
    );
}
