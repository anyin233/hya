use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use yaca_proto::Role;

use super::error::is_system_error_text;
use crate::AppState;
use crate::theme::Theme;
use crate::view_model::{TimelinePart, ToolStatus, timeline_items};

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
    for part in parts {
        match part {
            TimelinePart::Text(text) => {
                for (idx, segment) in text.trim().split('\n').enumerate() {
                    let label = if idx == 0 { "yaca " } else { "     " };
                    lines.push(Line::from(vec![
                        Span::styled("   ", block_style(theme.muted, selected, theme)),
                        Span::styled(
                            label.to_string(),
                            block_style(theme.success, selected, theme)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            segment.to_string(),
                            block_style(theme.text, selected, theme),
                        ),
                    ]));
                }
            }
            TimelinePart::Reasoning(text) => {
                if !text.trim().is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled("   ", block_style(theme.muted, selected, theme)),
                        Span::styled("Thinking", block_style(theme.warning, selected, theme)),
                    ]));
                }
            }
            TimelinePart::Tool {
                name,
                input,
                status,
            } => {
                lines.push(Line::from(vec![
                    Span::styled("   ", block_style(theme.muted, selected, theme)),
                    Span::styled("⚙ ", block_style(theme.accent, selected, theme)),
                    Span::styled(
                        format!("{name} "),
                        block_style(theme.accent, selected, theme).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        if input.is_empty() {
                            String::new()
                        } else {
                            format!("{input} ")
                        },
                        block_style(theme.muted, selected, theme),
                    ),
                    Span::styled(
                        format!("tool {name} {}", status.label()),
                        block_style(status.color(theme), selected, theme),
                    ),
                    Span::styled(
                        status.suffix(),
                        block_style(status.color(theme), selected, theme),
                    ),
                ]));
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
                text.push_str(&format!("tool {name} {}", status.label()));
            }
        }
    }
    text
}

trait TimelineStatusExt {
    fn label(&self) -> &'static str;
    fn color(&self, theme: &Theme) -> Color;
    fn suffix(&self) -> String;
}

impl TimelineStatusExt for ToolStatus {
    fn label(&self) -> &'static str {
        match self {
            ToolStatus::Pending => "pending",
            ToolStatus::Running => "running",
            ToolStatus::Completed { .. } => "completed",
            ToolStatus::Error { .. } => "error",
        }
    }

    fn color(&self, theme: &Theme) -> Color {
        match self {
            ToolStatus::Pending | ToolStatus::Running => theme.warning,
            ToolStatus::Completed { .. } => theme.muted,
            ToolStatus::Error { .. } => theme.error,
        }
    }

    fn suffix(&self) -> String {
        match self {
            ToolStatus::Pending | ToolStatus::Running => " …".to_string(),
            ToolStatus::Completed { time_ms } => format!(" ✓ {time_ms}ms"),
            ToolStatus::Error { message } => format!(" ✗ {message}"),
        }
    }
}
