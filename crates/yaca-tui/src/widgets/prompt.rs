use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::prompt_attachments::attachment_badges;
use super::prompt_metadata::composer_metadata;
use crate::AppState;
use crate::theme::Theme;

pub fn render_prompt(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    policy_width: u16,
) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.element)),
        area,
    );
    let rail = if state.running {
        theme.warning
    } else {
        theme.primary
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("▌ ", Style::default().fg(rail).bg(theme.element)),
            Span::styled(
                state.input.clone(),
                Style::default().fg(theme.text).bg(theme.element),
            ),
        ]),
        composer_metadata(state, theme, area.width, policy_width),
    ];
    if !state.attachments.is_empty() {
        lines.push(attachment_badges(state, theme, area.width));
    }
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
        String::new()
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
