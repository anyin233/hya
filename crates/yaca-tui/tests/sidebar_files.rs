#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use unicode_width::UnicodeWidthStr;
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

fn find_rendered_text(
    buffer: &Buffer,
    width: u16,
    height: u16,
    needle: &str,
) -> Option<(u16, u16)> {
    for y in 0..height {
        let mut row = String::new();
        for x in 0..width {
            row.push_str(buffer[(x, y)].symbol());
        }
        if let Some(x) = row.find(needle) {
            let display_x = UnicodeWidthStr::width(&row[..x]);
            return Some((u16::try_from(display_x).unwrap(), y));
        }
    }
    None
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
    let buffer = render_buffer(&mut state, 124, 28);
    let text = buffer_text(&buffer, 124, 28);

    // Then: changed files are visible as a first-class sidebar section.
    assert!(
        text.contains("Modified Files"),
        "context rail should show OpenCode's Modified Files section"
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

#[test]
fn context_rail_colors_modified_file_stats_like_opencode() {
    // Given: OpenCode renders additions and deletions with separate diff colors.
    let mut state = AppState {
        changed_files: vec![ChangedFileView {
            path: "crates/yaca-tui/src/widgets/sidebar.rs".to_string(),
            additions: Some(12),
            deletions: Some(3),
        }],
        ..AppState::default()
    };

    // When: the modified file row renders in the context rail.
    let buffer = render_buffer(&mut state, 124, 28);
    let added = find_rendered_text(&buffer, 124, 28, "+12").unwrap();
    let removed = find_rendered_text(&buffer, 124, 28, "-3").unwrap();

    // Then: addition and deletion counts use their semantic diff colors.
    assert_eq!(buffer[(added.0, added.1)].fg, Color::Rgb(127, 216, 143));
    assert_eq!(buffer[(removed.0, removed.1)].fg, Color::Rgb(224, 108, 117));
}

#[test]
fn context_rail_omits_zero_modified_file_stats_like_opencode() {
    // Given: OpenCode hides zero-value file diff counters.
    let mut state = AppState {
        changed_files: vec![ChangedFileView {
            path: "src/lib.rs".to_string(),
            additions: Some(8),
            deletions: Some(0),
        }],
        ..AppState::default()
    };

    // When: the modified file row renders in the context rail.
    let buffer = render_buffer(&mut state, 124, 28);
    let text = buffer_text(&buffer, 124, 28);

    // Then: only non-zero counters are shown.
    assert!(text.contains("src/lib.rs +8"));
    assert!(!text.contains("-0"));
}

#[test]
fn context_rail_marks_long_modified_file_lists_expandable_like_opencode() {
    // Given: OpenCode marks modified file lists longer than two entries expandable.
    let mut state = AppState {
        changed_files: vec![
            ChangedFileView {
                path: "a.rs".to_string(),
                additions: None,
                deletions: None,
            },
            ChangedFileView {
                path: "b.rs".to_string(),
                additions: None,
                deletions: None,
            },
            ChangedFileView {
                path: "c.rs".to_string(),
                additions: None,
                deletions: None,
            },
        ],
        ..AppState::default()
    };

    // When: the context rail renders the file section.
    let buffer = render_buffer(&mut state, 124, 28);
    let text = buffer_text(&buffer, 124, 28);

    // Then: the section title uses the same disclosure marker as OpenCode.
    assert!(text.contains("▼ Modified Files"));
}

#[test]
fn context_rail_renders_all_modified_files_when_expanded_like_opencode() {
    // Given: OpenCode's expanded Modified Files section has more than six entries.
    let mut state = AppState {
        changed_files: ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs", "g.rs"]
            .into_iter()
            .map(|path| ChangedFileView {
                path: path.to_string(),
                additions: None,
                deletions: None,
            })
            .collect(),
        ..AppState::default()
    };

    // When: the context rail renders with enough height for every entry.
    let buffer = render_buffer(&mut state, 124, 36);
    let text = buffer_text(&buffer, 124, 36);

    // Then: yaca mirrors OpenCode's open state by showing every file, not a synthetic counter.
    assert!(
        text.contains("g.rs"),
        "expanded file list should include the seventh file"
    );
    assert!(
        !text.contains("+1 more"),
        "expanded file list should not collapse visible entries"
    );
}
