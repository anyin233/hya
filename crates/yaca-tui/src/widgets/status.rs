use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::AppState;
use crate::theme::Theme;

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
    if state.yolo {
        spans.push(Span::styled(
            "  YOLO",
            Style::default()
                .fg(theme.background)
                .bg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if state.exit_armed {
        spans.push(Span::styled(
            "  Ctrl-C again to exit",
            Style::default().fg(theme.warning),
        ));
    }
    Line::from(spans)
}
