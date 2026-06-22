use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use yaca_proto::Role;

use super::error::is_system_error_text;
use super::transcript_tools::{push_tool_lines, status_label};
use crate::AppState;
use crate::theme::Theme;
use crate::view_model::{TimelinePart, timeline_items};

pub fn render_timeline(frame: &mut Frame, area: Rect, state: &mut AppState, theme: &Theme) {
    let lines = timeline_lines(state, theme);
    let inner_height = area.height.max(1);
    let inner_width = area.width.max(1);
    let total = lines.iter().fold(0u16, |acc, line| {
        let wrapped = u16::try_from(line.width())
            .unwrap_or(u16::MAX)
            .div_ceil(inner_width)
            .max(1);
        acc.saturating_add(wrapped)
    });
    let max_back = total.saturating_sub(inner_height);
    state.scroll_back = state.scroll_back.min(max_back);
    let top = max_back.saturating_sub(state.scroll_back);

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.base())
            .wrap(Wrap { trim: false })
            .scroll((top, 0)),
        area,
    );
}

fn timeline_lines(state: &AppState, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (idx, item) in timeline_items(&state.projection).iter().enumerate() {
        let selected = state.selected_message == Some(idx);
        match item.role {
            Role::User => user_lines(&item.parts, idx, selected, theme, &mut lines),
            Role::Assistant => assistant_lines(&item.parts, idx, selected, theme, &mut lines),
            Role::System => system_lines(&item.parts, idx, selected, theme, &mut lines),
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Ask yaca anything. Type below and press Enter.",
            Style::default().fg(theme.muted),
        )));
    }
    lines
}

fn user_lines(
    parts: &[TimelinePart],
    idx: usize,
    selected: bool,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    lines.push(message_header("You", idx, selected, theme, theme.primary));
    let text = text_from_parts(parts);
    for segment in text.split('\n') {
        lines.push(Line::from(vec![
            Span::styled("▏ ", block_style(theme.primary, selected, theme)),
            Span::styled("  ", block_style(theme.muted, selected, theme)),
            Span::styled(
                segment.to_string(),
                block_style(theme.text, selected, theme),
            ),
        ]));
    }
    lines.push(Line::from(""));
}

fn assistant_lines(
    parts: &[TimelinePart],
    idx: usize,
    selected: bool,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    lines.push(message_header("yaca", idx, selected, theme, theme.success));
    let mut has_visible_part = false;
    let mut previous_was_tool = false;
    for part in parts {
        match part {
            TimelinePart::Text(text) => {
                for segment in text.trim().split('\n') {
                    lines.push(Line::from(vec![
                        Span::styled("   ", block_style(theme.muted, selected, theme)),
                        Span::styled(
                            segment.to_string(),
                            block_style(theme.text, selected, theme),
                        ),
                    ]));
                }
                has_visible_part = true;
                previous_was_tool = false;
            }
            TimelinePart::Reasoning(text) => {
                if !text.trim().is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled("   ", block_style(theme.muted, selected, theme)),
                        Span::styled("Thinking", block_style(theme.warning, selected, theme)),
                    ]));
                    has_visible_part = true;
                    previous_was_tool = false;
                }
            }
            TimelinePart::Tool {
                name,
                input,
                status,
            } => {
                if has_visible_part && !previous_was_tool {
                    lines.push(Line::from(""));
                }
                push_tool_lines(name, input, status, selected, theme, lines);
                has_visible_part = true;
                previous_was_tool = true;
            }
        }
    }
    lines.push(Line::from(""));
}

fn system_lines(
    parts: &[TimelinePart],
    idx: usize,
    selected: bool,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    let text = text_from_parts(parts);
    let is_error = is_system_error_text(&text);
    let header = if is_error { "error" } else { "sys" };
    let header_color = if is_error { theme.error } else { theme.muted };
    lines.push(message_header(header, idx, selected, theme, header_color));
    for (idx, segment) in text.split('\n').enumerate() {
        if is_error {
            let label = if idx == 0 { "error " } else { "      " };
            lines.push(Line::from(vec![
                Span::styled(
                    label.to_string(),
                    block_style(theme.error, selected, theme).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    segment.to_string(),
                    block_style(theme.error, selected, theme),
                ),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("sys ", block_style(theme.muted, selected, theme)),
                Span::styled(
                    segment.to_string(),
                    block_style(theme.muted, selected, theme),
                ),
            ]));
        }
    }
}

fn message_header(
    label: &str,
    idx: usize,
    selected: bool,
    theme: &Theme,
    color: Color,
) -> Line<'static> {
    let marker = if selected { "▌ " } else { "  " };
    let label_style = block_style(color, selected, theme).add_modifier(Modifier::BOLD);
    let mut spans = vec![
        Span::styled(marker.to_string(), block_style(color, selected, theme)),
        Span::styled(format!("{label} #{}", idx + 1), label_style),
    ];
    if selected {
        spans.push(Span::styled(
            "  r revert · b branch",
            block_style(theme.muted, selected, theme),
        ));
    }
    Line::from(spans)
}

fn block_style(fg: Color, selected: bool, theme: &Theme) -> Style {
    let bg = if selected {
        theme.block
    } else {
        theme.background
    };
    Style::default().fg(fg).bg(bg)
}

fn text_from_parts(parts: &[TimelinePart]) -> String {
    let mut text = String::new();
    for part in parts {
        match part {
            TimelinePart::Text(value) => text.push_str(value),
            TimelinePart::Reasoning(_) => {}
            TimelinePart::Tool { name, status, .. } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("tool {name} {}", status_label(status)));
            }
        }
    }
    text
}
