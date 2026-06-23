use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};

use super::transcript_diff::{DiffDisplayLine, DiffLineKind, format_unified_diff};
use super::transcript_output::collapsed_tool_output;
use super::transcript_pending::pending_tool_label;
use super::transcript_tool_status::{
    inline_status_text, is_denied_error, status_color, tool_style,
};
use crate::theme::Theme;
use crate::tool_labels::{action_symbol, status_symbol};
use crate::view_model::ToolStatus;

const TOOL_INPUT_INLINE_MAX: usize = 48;

pub fn push_tool_lines(
    tool: (&str, &str, &str, &ToolStatus),
    width: u16,
    selected: bool,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    let (name, label, input, status) = tool;
    if let Some(label) = pending_tool_label(name, status) {
        lines.push(Line::from(vec![
            Span::styled("   ", tool_style(theme.muted, selected, theme, false)),
            Span::styled("~ ", tool_style(theme.text, selected, theme, false)),
            Span::styled(label, tool_style(theme.text, selected, theme, false)),
        ]));
        return;
    }

    let denied = is_denied_error(status);
    let color = status_color(status, theme);
    let symbol = if denied {
        action_symbol(name)
    } else {
        status_symbol(name, status)
    };
    let mut spans = vec![
        Span::styled("   ", tool_style(theme.muted, selected, theme, false)),
        Span::styled(
            format!("{symbol} "),
            tool_style(color, selected, theme, denied),
        ),
        Span::styled(
            format!("{label} "),
            tool_style(color, selected, theme, denied).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            input_label(input),
            tool_style(theme.muted, selected, theme, denied),
        ),
    ];
    if let Some(status) = inline_status_text(status) {
        spans.push(Span::styled(
            "· ",
            tool_style(theme.muted, selected, theme, false),
        ));
        spans.push(Span::styled(
            status,
            tool_style(color, selected, theme, false),
        ));
    }
    lines.push(Line::from(spans));

    if let ToolStatus::Error { message } = status
        && !denied
    {
        lines.push(Line::from(vec![
            Span::styled("   ", tool_style(theme.muted, selected, theme, false)),
            Span::styled("▏ ", tool_style(theme.error, selected, theme, false)),
            Span::styled(
                message.clone(),
                tool_style(theme.error, selected, theme, false),
            ),
        ]));
    }

    if let ToolStatus::Completed {
        output: Some(output),
        ..
    } = status
    {
        if let Some(diff_lines) = diff_output_lines(name, output) {
            if let Some(title) = diff_output_title(name, input) {
                push_output_line(&title, theme.muted, selected, theme, lines);
            }
            for segment in diff_lines {
                push_output_line(
                    &segment.text,
                    diff_kind_color(segment.kind, theme),
                    selected,
                    theme,
                    lines,
                );
            }
            return;
        }

        let preview = collapsed_tool_output(name, output, width);
        for segment in preview.lines() {
            push_output_line(
                segment,
                output_line_color(segment, theme),
                selected,
                theme,
                lines,
            );
        }
    }
}

fn diff_output_title(name: &str, input: &str) -> Option<String> {
    if input.is_empty() {
        return None;
    }
    match name {
        "edit" => Some(format!("# Edited {input}")),
        "write" => Some(format!("# Wrote {input}")),
        _ => None,
    }
}

fn push_output_line(
    text: &str,
    color: Color,
    selected: bool,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    lines.push(Line::from(vec![
        Span::styled("   ", tool_style(theme.muted, selected, theme, false)),
        Span::styled("▏ ", tool_style(color, selected, theme, false)),
        Span::styled(text.to_string(), tool_style(color, selected, theme, false)),
    ]));
}

fn diff_output_lines(name: &str, output: &str) -> Option<Vec<DiffDisplayLine>> {
    if matches!(name, "edit" | "write") {
        format_unified_diff(output)
    } else {
        None
    }
}

fn diff_kind_color(kind: DiffLineKind, theme: &Theme) -> Color {
    match kind {
        DiffLineKind::Hunk => theme.muted,
        DiffLineKind::Added => theme.success,
        DiffLineKind::Removed => theme.error,
        DiffLineKind::Context => theme.text,
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

fn output_line_color(line: &str, theme: &Theme) -> Color {
    if line.starts_with("@@") {
        theme.muted
    } else if line.starts_with('+') && !line.starts_with("+++") {
        theme.success
    } else if line.starts_with('-') && !line.starts_with("---") {
        theme.error
    } else {
        theme.text
    }
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
}
