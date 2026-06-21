use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use yaca_proto::Role;

use crate::theme::Theme;
use crate::view_model::{TimelinePart, ToolStatus, timeline_items};
use crate::{AppState, PermissionPrompt};

pub fn render_status(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    frame.render_widget(
        Paragraph::new(status_line(state, theme)).style(theme.base()),
        area,
    );
}

fn status_line(state: &AppState, theme: &Theme) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            "yaca",
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · {} · {}", state.model, state.session_label),
            Style::default().fg(theme.muted),
        ),
        Span::styled(
            if state.running {
                "  ● streaming".to_string()
            } else {
                "  ○ idle".to_string()
            },
            Style::default().fg(if state.running {
                theme.warning
            } else {
                theme.muted
            }),
        ),
    ];
    if let Some(goal) = &state.goal {
        spans.push(Span::styled(
            format!("  GOAL:{} turns {}", goal.condition, goal.turns),
            Style::default().fg(theme.accent),
        ));
    }
    if let Some(loop_view) = &state.loop_view {
        spans.push(Span::styled(
            format!(
                "  LOOP:{} iter {}/{} score {}",
                loop_view.target, loop_view.iteration, loop_view.budget, loop_view.last_score
            ),
            Style::default().fg(theme.info),
        ));
    }
    Line::from(spans)
}

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
            "Ask yaca anything. Type below and press Enter.",
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
                    let label = if idx == 0 { "yaca " } else { "     " };
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
    for segment in text.split('\n') {
        lines.push(Line::from(vec![
            Span::styled("sys ", Style::default().fg(theme.muted)),
            Span::styled(segment.to_string(), Style::default().fg(theme.muted)),
        ]));
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

pub fn render_sidebar(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    frame.render_widget(
        Paragraph::new(sidebar_lines(state, theme))
            .style(Style::default().fg(theme.text).bg(theme.panel))
            .block(
                Block::default()
                    .title(" context ")
                    .borders(Borders::LEFT)
                    .border_style(Style::default().fg(theme.border_subtle)),
            ),
        area,
    );
}

fn sidebar_lines(state: &AppState, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("model ", Style::default().fg(theme.muted)),
            Span::styled(state.model.clone(), Style::default().fg(theme.text)),
        ]),
        Line::from(vec![
            Span::styled("session ", Style::default().fg(theme.muted)),
            Span::styled(state.session_label.clone(), Style::default().fg(theme.text)),
        ]),
        Line::from(vec![
            Span::styled("state ", Style::default().fg(theme.muted)),
            Span::styled(
                if state.running { "streaming" } else { "idle" },
                Style::default().fg(if state.running {
                    theme.warning
                } else {
                    theme.success
                }),
            ),
        ]),
    ];

    if let Some(goal) = &state.goal {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "goal",
            Style::default().fg(theme.accent),
        )));
        lines.push(Line::from(format!(
            "{} · turns {}",
            goal.condition, goal.turns
        )));
    }
    if let Some(loop_view) = &state.loop_view {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "loop",
            Style::default().fg(theme.info),
        )));
        lines.push(Line::from(format!(
            "{} · {}/{} · score {}",
            loop_view.target, loop_view.iteration, loop_view.budget, loop_view.last_score
        )));
    }
    if !state.team.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "team",
            Style::default().fg(theme.primary),
        )));
        for (member, status) in &state.team {
            lines.push(Line::from(format!("{member}: {status}")));
        }
    }
    if let Some(permission) = &state.permission {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "permission",
            Style::default().fg(theme.warning),
        )));
        lines.push(Line::from(permission.title.clone()));
        lines.push(Line::from(permission.detail.clone()));
    }
    lines
}

pub fn render_prompt(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let widget = Paragraph::new(Line::from(vec![
        Span::styled("> ", Style::default().fg(theme.primary)),
        Span::styled(state.input.clone(), Style::default().fg(theme.text)),
    ]))
    .style(Style::default().fg(theme.text).bg(theme.panel))
    .block(
        Block::default()
            .title(" message — Enter: send · Ctrl-C: quit · PgUp/PgDn: scroll ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if state.running {
                theme.border_active
            } else {
                theme.border_subtle
            })),
    );
    frame.render_widget(widget, area);
}

#[must_use]
pub fn prompt_cursor(state: &AppState, area: Rect) -> Option<(u16, u16)> {
    if state.permission.is_some() || state.running {
        return None;
    }
    let typed = u16::try_from(state.input.chars().count()).unwrap_or(u16::MAX);
    let rightmost = area.x + area.width.saturating_sub(2);
    let cursor_x = (area.x + 3).saturating_add(typed).min(rightmost);
    Some((cursor_x, area.y + 1))
}

pub fn render_permission(frame: &mut Frame, prompt: &PermissionPrompt, theme: &Theme) {
    let area = frame.area();
    let height = 9u16.min(area.height);
    let width = area.width.saturating_sub(4).max(12);
    let rect = Rect {
        x: area.x + 2,
        y: area.height.saturating_sub(height),
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let inner_width = usize::from(width).saturating_sub(4);
    let option_spans: Vec<Span> = prompt
        .options()
        .iter()
        .enumerate()
        .flat_map(|(idx, label)| {
            let style = if idx == prompt.selected {
                Style::default()
                    .fg(theme.background)
                    .bg(theme.primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            vec![Span::styled(format!(" {label} "), style), Span::raw(" ")]
        })
        .collect();
    let lines = vec![
        Line::from(Span::styled(
            format!("{} wants to run:", prompt.title),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(ellipsize(&prompt.detail, inner_width)),
        Line::from(""),
        Line::from(option_spans),
        Line::from(vec![
            Span::styled("reply: ", Style::default().fg(theme.muted)),
            Span::styled(prompt.reply.clone(), Style::default().fg(theme.text)),
            Span::styled("█", Style::default().fg(theme.primary)),
        ]),
        Line::from(Span::styled(
            "←/→ select · type a reply · Enter confirm · Esc deny",
            Style::default().fg(theme.muted),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme.text).bg(theme.element))
            .block(
                Block::default()
                    .title("permission required")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.warning)),
            )
            .wrap(Wrap { trim: false }),
        rect,
    );
}

pub fn render_footer(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let text = if state.scroll_back > 0 {
        format!(
            "scroll {} · PgDn to return · Ctrl-C quit",
            state.scroll_back
        )
    } else {
        "PgUp/PgDn scroll · Enter send · Ctrl-C quit".to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(theme.muted),
        )))
        .style(theme.base()),
        area,
    );
}

trait TimelineStatusExt {
    fn label(&self) -> &'static str;
    fn color(&self, theme: &Theme) -> ratatui::style::Color;
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

    fn color(&self, theme: &Theme) -> ratatui::style::Color {
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

fn ellipsize(s: &str, max: usize) -> String {
    let cleaned = s.replace('\n', " ");
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let head: String = cleaned.chars().take(max).collect();
        format!("{head}…")
    }
}
