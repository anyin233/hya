#[allow(dead_code)]
mod render_support;

use render_support::render;
use yaca_tui::AppState;

#[test]
fn context_rail_shows_model_provider_without_reasoning_effort() {
    // Given: the active model has provider and effort metadata.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        model_provider_label: Some("anthropic".to_string()),
        reasoning_effort: Some("max".to_string()),
        ..AppState::default()
    };

    // When: the wide OpenCode-style context rail renders.
    let text = render(&mut state, 124, 36);

    // Then: sidebar core context exposes model/provider, while effort stays in the composer.
    assert!(
        text.contains("model kimi-k2 anthropic"),
        "context rail should show model/provider context:\n{text}"
    );
    assert!(
        !text.contains("model kimi-k2 anthropic max"),
        "context rail should not duplicate reasoning effort:\n{text}"
    );
    assert!(
        text.contains("Sisyphus · kimi-k2 anthropic · max"),
        "composer should keep the full model identity:\n{text}"
    );
}
