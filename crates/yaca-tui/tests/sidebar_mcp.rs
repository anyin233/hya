#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use ratatui::style::Color;
use render_support::{buffer_text, find_rendered_text, render, render_buffer};
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

#[test]
fn context_rail_hides_mcp_expand_arrow_for_short_lists_like_opencode() {
    // Given: OpenCode hides the MCP expand arrow for one or two connectors.
    let mut state = AppState {
        mcp: vec![
            ConnectorView {
                name: "codegraph".to_string(),
                state: ConnectorState::Connected,
            },
            ConnectorView {
                name: "context7".to_string(),
                state: ConnectorState::Connected,
            },
        ],
        ..AppState::default()
    };

    // When: a short MCP list renders.
    let text = render(&mut state, 124, 28);

    // Then: the title stays plain instead of advertising an unavailable collapse control.
    assert!(text.contains("MCP"));
    assert!(!text.contains("▼ MCP"));
}

#[test]
fn context_rail_shows_mcp_expand_arrow_for_long_lists_like_opencode() {
    // Given: OpenCode allows MCP lists with more than two connectors to collapse.
    let mut state = AppState {
        mcp: vec![
            ConnectorView {
                name: "codegraph".to_string(),
                state: ConnectorState::Connected,
            },
            ConnectorView {
                name: "context7".to_string(),
                state: ConnectorState::Connected,
            },
            ConnectorView {
                name: "linear".to_string(),
                state: ConnectorState::NeedsAuth,
            },
        ],
        ..AppState::default()
    };

    // When: a long MCP list renders.
    let text = render(&mut state, 124, 28);

    // Then: the title advertises the expandable section.
    assert!(text.contains("▼ MCP"));
}

#[test]
fn context_rail_renders_opencode_mcp_error_states() {
    // Given: OpenCode distinguishes failed connectors from disabled connectors.
    let mut state = AppState {
        mcp: vec![ConnectorView {
            name: "broken".to_string(),
            state: ConnectorState::Failed("spawn failed".to_string()),
        }],
        ..AppState::default()
    };

    // When: MCP connector states render in the context rail.
    let buffer = render_buffer(&mut state, 124, 28);
    let text = buffer_text(&buffer, 124, 28);
    let broken = find_rendered_text(&buffer, 124, 28, "• broken spawn failed").unwrap();

    // Then: yaca uses OpenCode's visible labels and error markers for the states.
    assert!(text.contains("broken spawn failed"));
    assert_eq!(buffer[(broken.0, broken.1)].fg, Color::Rgb(224, 108, 117));
}
