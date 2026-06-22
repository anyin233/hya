#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use yaca_tui::{AppState, PermissionPrompt};

#[test]
fn permission_panel_uses_opencode_left_rail_without_box_border() {
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

    // Then: the permission prompt uses OpenCode's warning rail instead of a boxed dialog.
    assert!(
        text.contains("▏ △ Permission required"),
        "permission panel should start with a warning rail header:\n{text}"
    );
    assert!(
        !text.contains('┌') && !text.contains('└') && !text.contains('─') && !text.contains('│'),
        "permission panel should not render box border glyphs:\n{text}"
    );
    let reply_row = text
        .lines()
        .find(|row| row.contains("reply:"))
        .unwrap_or_else(|| panic!("permission reply row missing:\n{text}"));
    assert!(
        reply_row.starts_with("  ▏ reply:"),
        "permission reply row should own its left gutter:\n{reply_row}"
    );

    assert!(
        text.lines().any(|row| row.starts_with("  ▏ ←/→ select")),
        "permission hint row should stay inside the left rail:\n{text}"
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
