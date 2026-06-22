#[allow(dead_code)]
mod render_support;

use render_support::render;
use yaca_tui::AppState;

#[test]
fn default_footer_omits_legacy_navigation_hints() {
    // Given: the default OpenCode-style shell is idle.
    let mut state = AppState::default();

    // When: it renders at a width where the composer command hint is visible.
    let text = render(&mut state, 100, 20);

    // Then: command affordance lives in the composer metadata, not a legacy footer row.
    assert!(
        text.contains("ctrl+p commands"),
        "composer metadata should keep command affordance visible"
    );
    for hint in ["PgUp/PgDn scroll", "Tab yolo", "F2 model"] {
        assert!(
            !text.contains(hint),
            "default footer should omit legacy hint {hint:?}, got {text:?}"
        );
    }
}
