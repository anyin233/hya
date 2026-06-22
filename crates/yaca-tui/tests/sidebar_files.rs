#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use yaca_tui::{AppState, ChangedFileView, draw};

fn render_buffer(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    terminal.backend().buffer().clone()
}

fn buffer_text(buffer: &Buffer, width: u16, height: u16) -> String {
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn context_rail_shows_modified_files() {
    // Given: the shell knows the git worktree has modified files.
    let mut state = AppState {
        changed_files: vec![
            ChangedFileView {
                path: "crates/yaca-tui/src/widgets/sidebar.rs".to_string(),
                additions: Some(12),
                deletions: Some(3),
            },
            ChangedFileView {
                path: "README.md".to_string(),
                additions: None,
                deletions: None,
            },
        ],
        ..AppState::default()
    };

    // When: the OpenCode-style context rail renders.
    let buffer = render_buffer(&mut state, 120, 28);
    let text = buffer_text(&buffer, 120, 28);

    // Then: changed files are visible as a first-class sidebar section.
    assert!(
        text.contains("Files"),
        "context rail should show a Files section"
    );
    assert!(
        text.contains("sidebar.rs"),
        "context rail should preserve the modified file tail"
    );
    assert!(
        text.contains("+12 -3"),
        "context rail should show file additions and deletions"
    );
    assert!(
        text.contains("README.md"),
        "context rail should include files without numstat data"
    );
}
