#[allow(dead_code)]
mod render_support;

use render_support::render;
use yaca_tui::AppState;

#[test]
fn context_rail_does_not_duplicate_reasoning_effort() {
    // Given: the active model has an OpenCode-style effort variant.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        reasoning_effort: Some("max".to_string()),
        ..AppState::default()
    };

    // When: the wide shell renders both the context rail and composer metadata.
    let text = render(&mut state, 124, 36);

    // Then: effort stays in the composer identity line instead of becoming a
    // duplicate Runtime section in the context rail.
    assert!(
        text.contains("Sisyphus · kimi-k2 · max"),
        "composer metadata should keep the effort visible:\n{text}"
    );
    assert!(
        !text.contains("Runtime") && !text.contains("think max"),
        "OpenCode context rail should not duplicate model effort:\n{text}"
    );
}
