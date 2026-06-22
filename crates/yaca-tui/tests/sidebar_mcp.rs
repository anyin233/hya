#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use render_support::render;
use yaca_tui::{AppState, ConnectorState, ConnectorView};

#[test]
fn context_rail_omits_empty_mcp_section_like_opencode() {
    // Given: the shell has no MCP connectors.
    let mut state = AppState::default();

    // When: the OpenCode-style context rail renders.
    let text = render(&mut state, 124, 28);

    // Then: OpenCode omits the MCP section instead of showing an empty placeholder.
    assert!(!text.contains("MCP"));
    assert!(!text.contains("none configured"));
}

#[test]
fn context_rail_shows_mcp_section_when_connectors_exist() {
    // Given: the shell knows about at least one MCP connector.
    let mut state = AppState {
        mcp: vec![ConnectorView {
            name: "codegraph".to_string(),
            state: ConnectorState::Connected,
        }],
        ..AppState::default()
    };

    // When: the OpenCode-style context rail renders.
    let text = render(&mut state, 124, 28);

    // Then: non-empty MCP state remains visible.
    assert!(text.contains("MCP"));
    assert!(text.contains("codegraph Connected"));
}
