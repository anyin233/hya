#[allow(dead_code)]
mod render_support;

use render_support::render;
use yaca_tui::AppState;

#[test]
fn default_state_omits_empty_transcript_banner() {
    // Given: the default OpenCode-style shell has no transcript messages.
    let mut state = AppState::default();

    // When: the shell renders its main streaming region.
    let text = render(&mut state, 80, 20);

    // Then: the transcript surface stays blank instead of showing product help text.
    assert!(
        !text.contains("Ask yaca"),
        "empty transcript should be blank"
    );
}
