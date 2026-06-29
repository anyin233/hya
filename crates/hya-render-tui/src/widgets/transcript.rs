use hya_proto::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

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
    for item in timeline_items(&state.projection) {
        match item.role {
            Role::User => user_lines(&item.parts, theme, &mut lines),
            Role::Assistant => assistant_lines(&item.parts, theme, &mut lines),
            Role::System => system_lines(&item.parts, theme, &mut lines),
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Ask hya anything. Type below and press Enter.",
            Style::default().fg(theme.muted),
        )));
    }
    lines
}

fn user_lines(parts: &[TimelinePart], theme: &Theme, lines: &mut Vec<Line<'static>>) {
    let text = text_from_parts(parts);
    for (idx, segment) in text.split('\n').enumerate() {
        let label = if idx == 0 { "You " } else { "    " };
        lines.push(Line::from(vec![
            Span::styled("│ ", Style::default().fg(theme.primary)),
            Span::styled(
                label.to_string(),
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(segment.to_string(), Style::default().fg(theme.text)),
        ]));
    }
    lines.push(Line::from(""));
}

fn assistant_lines(parts: &[TimelinePart], theme: &Theme, lines: &mut Vec<Line<'static>>) {
    for part in parts {
        match part {
            TimelinePart::Text(text) => {
                for (idx, segment) in text.trim().split('\n').enumerate() {
                    let label = if idx == 0 { "hya " } else { "     " };
                    lines.push(Line::from(vec![
                        Span::styled("  ", Style::default().fg(theme.muted)),
                        Span::styled(
                            label.to_string(),
                            Style::default()
                                .fg(theme.success)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(segment.to_string(), Style::default().fg(theme.text)),
                    ]));
                }
            }
            TimelinePart::Reasoning(text) => {
                if !text.trim().is_empty() {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled("Thinking", Style::default().fg(theme.warning)),
                    ]));
                }
            }
            TimelinePart::Tool {
                name,
                input,
                status,
            } => {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled("⚙ ", Style::default().fg(theme.accent)),
                    Span::styled(
                        format!("{name} "),
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        if input.is_empty() {
                            String::new()
                        } else {
                            format!("{input} ")
                        },
                        Style::default().fg(theme.muted),
                    ),
                    Span::styled(
                        format!("tool {name} {}", status.label()),
                        Style::default().fg(status.color(theme)),
                    ),
                    Span::styled(status.suffix(), Style::default().fg(status.color(theme))),
                ]));
            }
        }
    }
    lines.push(Line::from(""));
}

fn system_lines(parts: &[TimelinePart], theme: &Theme, lines: &mut Vec<Line<'static>>) {
    let text = text_from_parts(parts);
    let is_error = is_system_error_text(&text);
    for (idx, segment) in text.split('\n').enumerate() {
        if is_error {
            let label = if idx == 0 { "error " } else { "      " };
            lines.push(Line::from(vec![
                Span::styled(
                    label.to_string(),
                    Style::default()
                        .fg(theme.error)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(segment.to_string(), Style::default().fg(theme.error)),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("sys ", Style::default().fg(theme.muted)),
                Span::styled(segment.to_string(), Style::default().fg(theme.muted)),
            ]));
        }
    }
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
