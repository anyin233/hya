#[allow(dead_code)]
mod render_support;

use ratatui::buffer::Buffer;
use ratatui::style::Color;
use render_support::{render, render_buffer};
use yaca_tui::{AppState, ChangedFileView};

fn row_text(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

fn row_index(buffer: &Buffer, width: u16, height: u16, needle: &str) -> Option<u16> {
    (0..height).find(|&y| row_text(buffer, width, y).contains(needle))
}

fn symbol_x(buffer: &Buffer, width: u16, y: u16, symbol: &str) -> Option<u16> {
    (0..width).find(|&x| buffer[(x, y)].symbol() == symbol)
}

fn text_x(buffer: &Buffer, width: u16, y: u16, needle: &str) -> Option<u16> {
    row_text(buffer, width, y)
        .find(needle)
        .and_then(|x| u16::try_from(x).ok())
}

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

#[test]
fn context_rail_renders_agents_as_opencode_card() {
    // Given: the context rail has active agent metadata.
    let mut state = AppState {
        agent: "build".to_string(),
        team: vec![("sisyphus".to_string(), "ultraworker retry".to_string())],
        ..AppState::default()
    };

    // When: the wide OpenCode-style shell renders the sidebar.
    let width = 124;
    let height = 28;
    let buffer = render_buffer(&mut state, width, height);
    let Some(title_y) = row_index(&buffer, width, height, "│ Agents") else {
        panic!("agents card title row should be visible");
    };

    // Then: the Agents section is framed like the OpenCode sidebar card.
    assert!(
        title_y > 0 && title_y + 2 < height,
        "agents card title row should be visible"
    );
    assert!(row_text(&buffer, width, title_y - 1).contains("┌"));
    assert!(row_text(&buffer, width, title_y).contains("│ Agents"));
    assert!(row_text(&buffer, width, title_y + 1).contains("│ sisyphus - ultraworker retry"));
    assert!(row_text(&buffer, width, title_y + 2).contains("└"));
}

#[test]
fn context_rail_agents_card_styles_identity_and_status_separately() {
    // Given: the active agent has an OpenCode-style role/status suffix.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        team: vec![("sisyphus".to_string(), "ultraworker retry".to_string())],
        ..AppState::default()
    };

    // When: the wide OpenCode-style shell renders the Agents card.
    let width = 124;
    let height = 28;
    let buffer = render_buffer(&mut state, width, height);
    let Some(title_y) = row_index(&buffer, width, height, "│ Agents") else {
        panic!("agents card title row should be visible");
    };
    let agent_y = title_y + 1;
    let Some(agent_x) = text_x(&buffer, width, agent_y, "sisyphus") else {
        panic!("active agent name should be visible");
    };
    let Some(status_x) = text_x(&buffer, width, agent_y, "ultraworker retry") else {
        panic!("active agent status should be visible");
    };

    // Then: agent identity uses the semantic agent accent while status stays muted.
    assert_eq!(
        buffer[(agent_x, agent_y)].fg,
        Color::Rgb(0, 188, 212),
        "agent identity should use the TUI agent accent"
    );
    assert_eq!(
        buffer[(status_x, agent_y)].fg,
        Color::Rgb(128, 128, 128),
        "agent status should remain secondary metadata"
    );
}

#[test]
fn context_rail_does_not_render_clipped_agents_card_above_footer() {
    // Given: preceding sidebar sections consume nearly all body rows.
    let mut state = AppState {
        agent: "build".to_string(),
        changed_files: (0..6)
            .map(|index| ChangedFileView {
                path: format!("crates/yaca-tui/src/changed-file-{index}.rs"),
                additions: Some(1),
                deletions: Some(0),
            })
            .collect(),
        ..AppState::default()
    };

    // When: the footer is visible in a moderately tall OpenCode rail.
    let width = 124;
    let height = 36;
    let buffer = render_buffer(&mut state, width, height);

    // Then: the Agents card is never shown without its closing border.
    if let Some(title_y) = row_index(&buffer, width, height, "│ Agents") {
        assert!(
            row_text(&buffer, width, title_y + 2).contains("└"),
            "agents card should be complete when visible:\n{}",
            render(&mut state, width, height)
        );
    }
}

#[test]
fn context_rail_agents_card_keeps_cjk_status_inside_border() {
    // Given: the Agents card has a full-width status that must be clipped.
    let mut state = AppState {
        agent: "build".to_string(),
        team: vec![(
            "研究员".to_string(),
            "处理中处理中处理中处理中处理中处理中".to_string(),
        )],
        ..AppState::default()
    };

    // When: the wide OpenCode-style shell renders the sidebar.
    let width = 124;
    let height = 28;
    let buffer = render_buffer(&mut state, width, height);
    let Some(title_y) = row_index(&buffer, width, height, "│ Agents") else {
        panic!("agents card title row should be visible");
    };
    let top_y = title_y - 1;
    let agent_y = title_y + 1;
    let Some(left_x) = symbol_x(&buffer, width, top_y, "┌") else {
        panic!("agents card left border should be visible");
    };
    let Some(right_x) = symbol_x(&buffer, width, top_y, "┐") else {
        panic!("agents card right border should be visible");
    };

    // Then: full-width text stays inside the same card border columns.
    assert_eq!(agent_y, top_y + 2);
    assert_eq!(buffer[(left_x, agent_y)].symbol(), "│");
    assert_eq!(buffer[(right_x, agent_y)].symbol(), "│");
    assert!(
        row_text(&buffer, width, agent_y).contains('…'),
        "long CJK status should be ellipsized before the right border"
    );
}
