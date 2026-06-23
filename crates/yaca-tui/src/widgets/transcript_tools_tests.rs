use super::transcript_tools::push_tool_lines;
use crate::theme::Theme;
use crate::view_model::ToolStatus;

#[test]
fn shell_tool_status_fits_an_eighty_column_transcript_budget_with_long_input() {
    // Given: a long shell command rendered as a completed compact tool row.
    let theme = Theme::yaca_dark();
    let mut lines = Vec::new();

    // When: the row is converted into ratatui spans.
    push_tool_lines(
        (
            "shell",
            "Shell",
            r#"{"cmd":"printf line one && printf line two"}"#,
            &ToolStatus::Completed {
                time_ms: 9,
                output: None,
                exit_code: None,
            },
        ),
        80,
        false,
        &theme,
        &mut lines,
    );

    // Then: the status row leaves room for terminal glyph-width differences at 80 columns.
    let width = match lines.first() {
        Some(line) => line.width(),
        None => panic!("tool row missing"),
    };
    assert!(
        width <= 76,
        "tool status row should fit the narrow transcript budget, got width {width}"
    );
}

#[test]
fn tool_status_ellipsizes_cjk_input_by_display_width() {
    // Given: a wide-character command rendered in the narrow transcript budget.
    let theme = Theme::yaca_dark();
    let mut lines = Vec::new();

    // When: the row is converted into ratatui spans.
    push_tool_lines(
        (
            "shell",
            "Shell",
            "测试路径".repeat(14).as_str(),
            &ToolStatus::Completed {
                time_ms: 9,
                output: None,
                exit_code: None,
            },
        ),
        80,
        false,
        &theme,
        &mut lines,
    );

    // Then: wide characters are counted as two terminal cells before truncation.
    let line = match lines.first() {
        Some(line) => line,
        None => panic!("tool row missing"),
    };
    assert!(
        line.width() <= 76,
        "CJK tool status row should fit the narrow transcript budget, got width {}",
        line.width()
    );
    assert!(
        line.spans.iter().any(|span| span.content.contains('…')),
        "truncated CJK input should keep an ellipsis marker"
    );
}

#[test]
fn tool_status_ellipsizes_input_to_actual_line_width() {
    // Given: a long tool label and running status competing with input text.
    let theme = Theme::yaca_dark();
    let mut lines = Vec::new();

    // When: the row is rendered with a narrow OpenCode-style transcript budget.
    push_tool_lines(
        (
            "parallel_search",
            "Parallel Web Search",
            "opencode tui transcript block selection branch revert narrow viewport",
            &ToolStatus::Running,
        ),
        52,
        false,
        &theme,
        &mut lines,
    );

    // Then: input truncation uses the actual remaining row width, not a fixed budget.
    let line = match lines.first() {
        Some(line) => line,
        None => panic!("tool row missing"),
    };
    assert!(
        line.width() <= 48,
        "tool row should fit the visible transcript budget, got width {}",
        line.width()
    );
    assert!(
        line.spans.iter().any(|span| span.content.contains('…')),
        "narrow rows should keep an ellipsis marker on the input"
    );
}
