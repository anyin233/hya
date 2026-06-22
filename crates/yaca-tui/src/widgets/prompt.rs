use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::AppState;
use crate::theme::Theme;

pub fn render_prompt(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.element)),
        area,
    );
    let rail = if state.running {
        theme.warning
    } else {
        theme.primary
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("▌ ", Style::default().fg(rail).bg(theme.element)),
            Span::styled(
                state.input.clone(),
                Style::default().fg(theme.text).bg(theme.element),
            ),
        ]),
        composer_metadata(state, theme),
    ];
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(theme.text).bg(theme.element)),
        area,
    );
}

#[must_use]
pub fn prompt_cursor(state: &AppState, area: Rect) -> Option<(u16, u16)> {
    if state.permission.is_some() || state.running {
        return None;
    }
    let typed = u16::try_from(UnicodeWidthStr::width(state.input.as_str())).unwrap_or(u16::MAX);
    let rightmost = area.x + area.width.saturating_sub(1);
    let cursor_x = (area.x + 2).saturating_add(typed).min(rightmost);
    Some((cursor_x, area.y))
}

pub fn render_footer(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let text = if state.scroll_back > 0 {
        format!(
            "scroll {} · End to return · Ctrl-C clear/interrupt",
            state.scroll_back
        )
    } else if state.exit_armed {
        "Ctrl-C again to exit · type to cancel".to_string()
    } else if state.yolo {
        "YOLO mode · Tab disables auto-allow · / commands · @ references".to_string()
    } else if state.goal.is_some() || state.loop_view.is_some() {
        runtime_footer_text(state)
    } else {
        "PgUp/PgDn scroll · Tab yolo · / commands · @ references · F2 model".to_string()
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

fn composer_metadata(state: &AppState, theme: &Theme) -> Line<'static> {
    let agent = if state.agent.is_empty() {
        "build"
    } else {
        state.agent.as_str()
    };
    let model = if state.model.is_empty() {
        "offline"
    } else {
        state.model.as_str()
    };
    let effort = state.reasoning_effort.as_deref().unwrap_or("off");
    let cost = state.cost_label.as_deref().unwrap_or("cost n/a");
    let mode = if state.yolo { "YOLO" } else { "manual" };
    Line::from(vec![
        Span::styled("  ", Style::default().bg(theme.element)),
        Span::styled(
            agent.to_string(),
            Style::default().fg(theme.info).bg(theme.element),
        ),
        Span::styled(" · ", Style::default().fg(theme.muted).bg(theme.element)),
        Span::styled(
            model.to_string(),
            Style::default().fg(theme.text).bg(theme.element),
        ),
        Span::styled(
            " · think ",
            Style::default().fg(theme.muted).bg(theme.element),
        ),
        Span::styled(
            effort.to_string(),
            Style::default().fg(theme.accent).bg(theme.element),
        ),
        Span::styled(" · ", Style::default().fg(theme.muted).bg(theme.element)),
        Span::styled(
            mode.to_string(),
            Style::default().fg(theme.warning).bg(theme.element),
        ),
        Span::styled("   ", Style::default().bg(theme.element)),
        Span::styled(
            cost.to_string(),
            Style::default().fg(theme.muted).bg(theme.element),
        ),
        Span::styled(
            "   ctrl+p commands",
            Style::default().fg(theme.muted).bg(theme.element),
        ),
    ])
}

fn runtime_footer_text(state: &AppState) -> String {
    let mut segments = Vec::new();
    if let Some(goal) = &state.goal {
        segments.push(format!("GOAL:{} turns {}", goal.condition, goal.turns));
    }
    if let Some(loop_view) = &state.loop_view {
        segments.push(format!(
            "LOOP:{} iter {}/{} score {}",
            loop_view.target, loop_view.iteration, loop_view.budget, loop_view.last_score
        ));
    }
    segments.push("ctrl+p commands".to_string());
    segments.join(" · ")
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::prompt_cursor;
    use crate::AppState;

    #[test]
    fn prompt_cursor_uses_display_columns_when_input_contains_cjk() {
        // Given: a composer input containing two full-width CJK glyphs.
        let state = AppState {
            input: "你好".to_string(),
            ..AppState::default()
        };
        let area = Rect::new(10, 20, 40, 2);

        // When: the prompt asks ratatui where to draw the terminal cursor.
        let cursor = prompt_cursor(&state, area);

        // Then: the cursor advances by four terminal columns, not two chars.
        assert_eq!(cursor, Some((16, 20)));
    }
}
