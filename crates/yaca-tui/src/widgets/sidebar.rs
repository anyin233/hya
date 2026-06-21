use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use yaca_proto::Role;

use super::error::is_system_error_text;
use crate::AppState;
use crate::theme::Theme;
use crate::view_model::{TimelinePart, ToolStatus, timeline_items};

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
    let stats = transcript_stats(state);
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

    if stats.messages > 0 || stats.attachments > 0 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "transcript",
            Style::default().fg(theme.info),
        )));
        lines.push(Line::from(vec![
            Span::styled("messages ", Style::default().fg(theme.muted)),
            Span::styled(stats.messages.to_string(), Style::default().fg(theme.text)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("tools ", Style::default().fg(theme.muted)),
            Span::styled(stats.tools.to_string(), Style::default().fg(theme.text)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("errors ", Style::default().fg(theme.muted)),
            Span::styled(
                stats.errors.to_string(),
                Style::default().fg(if stats.errors > 0 {
                    theme.error
                } else {
                    theme.text
                }),
            ),
        ]));
        if stats.attachments > 0 {
            lines.push(Line::from(vec![
                Span::styled("attachments ", Style::default().fg(theme.muted)),
                Span::styled(
                    stats.attachments.to_string(),
                    Style::default().fg(theme.text),
                ),
            ]));
            for attachment in state.attachments.iter().take(3) {
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default().fg(theme.muted)),
                    Span::styled(
                        attachment.placeholder.clone(),
                        Style::default().fg(theme.text),
                    ),
                ]));
            }
        }
    }

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
    if state.yolo {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("mode ", Style::default().fg(theme.muted)),
            Span::styled("YOLO", Style::default().fg(theme.warning)),
        ]));
    }
    if let Some(effort) = &state.reasoning_effort {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("think ", Style::default().fg(theme.muted)),
            Span::styled(effort.clone(), Style::default().fg(theme.accent)),
        ]));
    }
    lines
}

#[derive(Default)]
struct TranscriptStats {
    messages: usize,
    tools: usize,
    errors: usize,
    attachments: usize,
}

fn transcript_stats(state: &AppState) -> TranscriptStats {
    let mut stats = TranscriptStats {
        attachments: state.attachments.len(),
        ..TranscriptStats::default()
    };
    for item in timeline_items(&state.projection) {
        stats.messages += 1;
        let mut system_text = String::new();
        for part in item.parts {
            match part {
                TimelinePart::Text(text) => {
                    if item.role == Role::System {
                        system_text.push_str(&text);
                    }
                }
                TimelinePart::Reasoning(_) => {}
                TimelinePart::Tool { status, .. } => {
                    stats.tools += 1;
                    if matches!(status, ToolStatus::Error { .. }) {
                        stats.errors += 1;
                    }
                }
            }
        }
        if item.role == Role::System && is_system_error_text(&system_text) {
            stats.errors += 1;
        }
    }
    stats
}
