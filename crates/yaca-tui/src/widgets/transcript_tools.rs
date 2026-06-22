use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;
use crate::view_model::ToolStatus;

const TOOL_INPUT_INLINE_MAX: usize = 32;

pub fn push_tool_lines(
    name: &str,
    input: &str,
    status: &ToolStatus,
    selected: bool,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    let color = status_color(status, theme);
    lines.push(Line::from(vec![
        Span::styled("   ", tool_style(theme.muted, selected, theme)),
        Span::styled(
            format!("{} ", status_symbol(name, status)),
            tool_style(color, selected, theme),
        ),
        Span::styled(
            format!("{} ", action_label(name)),
            tool_style(color, selected, theme).add_modifier(Modifier::BOLD),
        ),
        Span::styled(input_label(input), tool_style(theme.muted, selected, theme)),
        Span::styled("· ", tool_style(theme.muted, selected, theme)),
        Span::styled(status_label(status), tool_style(color, selected, theme)),
        Span::styled(status_suffix(status), tool_style(color, selected, theme)),
    ]));

    if let ToolStatus::Error { message } = status {
        lines.push(Line::from(vec![
            Span::styled("   ", tool_style(theme.muted, selected, theme)),
            Span::styled("▏ ", tool_style(theme.error, selected, theme)),
            Span::styled(message.clone(), tool_style(theme.error, selected, theme)),
        ]));
    }

    if let ToolStatus::Completed {
        output: Some(output),
        ..
    } = status
    {
        for segment in output.lines() {
            lines.push(Line::from(vec![
                Span::styled("   ", tool_style(theme.muted, selected, theme)),
                Span::styled("▏ ", tool_style(theme.muted, selected, theme)),
                Span::styled(segment.to_string(), tool_style(theme.text, selected, theme)),
            ]));
        }
    }
}

fn input_label(input: &str) -> String {
    if input.is_empty() {
        String::new()
    } else {
        format!("{} ", ellipsize_input(input))
    }
}

fn ellipsize_input(input: &str) -> String {
    let cleaned = input.replace('\n', " ");
    if cleaned.chars().count() <= TOOL_INPUT_INLINE_MAX {
        cleaned
    } else {
        let head: String = cleaned.chars().take(TOOL_INPUT_INLINE_MAX).collect();
        format!("{head}…")
    }
}

fn status_symbol(name: &str, status: &ToolStatus) -> &'static str {
    match status {
        ToolStatus::Error { .. } => "×",
        ToolStatus::Pending | ToolStatus::Running | ToolStatus::Completed { .. } => {
            if matches!(name, "edit" | "write") {
                "←"
            } else {
                "→"
            }
        }
    }
}

fn action_label(name: &str) -> String {
    match name {
        "bash" | "shell" => "Shell".to_string(),
        "read" => "Read".to_string(),
        "edit" => "Edit".to_string(),
        "write" => "Write".to_string(),
        "grep" => "Grep".to_string(),
        "glob" => "Glob".to_string(),
        other => title_case_ascii(other),
    }
}

fn title_case_ascii(input: &str) -> String {
    let mut chars = input.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

pub fn status_label(status: &ToolStatus) -> &'static str {
    match status {
        ToolStatus::Pending => "pending",
        ToolStatus::Running => "running",
        ToolStatus::Completed { .. } => "completed",
        ToolStatus::Error { .. } => "error",
    }
}

fn status_color(status: &ToolStatus, theme: &Theme) -> Color {
    match status {
        ToolStatus::Pending | ToolStatus::Running => theme.warning,
        ToolStatus::Completed { .. } => theme.muted,
        ToolStatus::Error { .. } => theme.error,
    }
}

fn status_suffix(status: &ToolStatus) -> String {
    match status {
        ToolStatus::Pending | ToolStatus::Running => " …".to_string(),
        ToolStatus::Completed { time_ms, .. } => format!(" ✓ {time_ms}ms"),
        ToolStatus::Error { .. } => " ✗".to_string(),
    }
}

fn tool_style(fg: Color, selected: bool, theme: &Theme) -> Style {
    let bg = if selected {
        theme.block
    } else {
        theme.background
    };
    Style::default().fg(fg).bg(bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_tool_status_fits_an_eighty_column_transcript_budget_with_long_input() {
        // Given: a long shell command rendered as a completed compact tool row.
        let theme = Theme::yaca_dark();
        let mut lines = Vec::new();

        // When: the row is converted into ratatui spans.
        push_tool_lines(
            "shell",
            r#"{"cmd":"printf line one && printf line two"}"#,
            &ToolStatus::Completed {
                time_ms: 9,
                output: None,
            },
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
}
