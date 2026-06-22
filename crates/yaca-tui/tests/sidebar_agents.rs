#[allow(dead_code)]
mod render_support;

use render_support::render;
use yaca_tui::AppState;

#[test]
fn context_rail_omits_empty_agent_status_suffix() {
    // Given: the context rail has a team member without a status label.
    let mut state = AppState {
        agent: "build".to_string(),
        team: vec![("sisyphus".to_string(), String::new())],
        ..AppState::default()
    };

    // When: the wide OpenCode-style shell renders the sidebar.
    let text = render(&mut state, 124, 24);
    let agent_row = text
        .lines()
        .find(|row| row.contains("sisyphus"))
        .unwrap_or_default();

    // Then: it keeps the bare agent label instead of a dangling separator.
    assert!(
        agent_row.contains("sisyphus"),
        "agent row should include the member name, got {agent_row:?}"
    );
    assert!(
        !agent_row.contains("sisyphus -"),
        "agent row should omit empty status suffix, got {agent_row:?}"
    );
}
