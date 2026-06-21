#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use yaca_tui::{AppState, PermissionPrompt};

#[test]
fn permission_panel_clears_prompt_and_footer_gutters_when_bottom_aligned() {
    // Given: an active permission prompt rendered over the bottom composer area.
    let mut state = AppState {
        input: "typed prompt".to_string(),
        permission: Some(PermissionPrompt {
            title: "task".to_string(),
            detail: "quick".to_string(),
            selected: 0,
            reply: String::new(),
        }),
        ..AppState::default()
    };

    // When: the TUI renders at the same size used by tmux visual QA.
    let text = render(&mut state, 100, 30);

    // Then: no prompt rail or footer hint leaks into the permission panel gutter.
    let reply_row = text
        .lines()
        .find(|row| row.contains("reply:"))
        .unwrap_or_else(|| panic!("permission reply row missing:\n{text}"));
    assert!(
        reply_row.starts_with("  │reply:"),
        "permission reply row should own its left gutter:\n{reply_row}"
    );

    let bottom_row = text
        .lines()
        .find(|row| row.contains('└'))
        .unwrap_or_else(|| panic!("permission bottom border missing:\n{text}"));
    assert!(
        bottom_row.starts_with("  └"),
        "permission bottom border should own its left gutter:\n{bottom_row}"
    );
}

fn render(state: &mut AppState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| yaca_tui::draw(frame, state)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut output = String::new();
    for y in 0..height {
        for x in 0..width {
            output.push_str(buffer[(x, y)].symbol());
        }
        output.push('\n');
    }
    output
}
