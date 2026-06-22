use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::prompt_attachments::attachment_badges;
use super::prompt_metadata::composer_metadata;
use crate::AppState;
use crate::theme::Theme;

const PROMPT_GUTTER_WIDTH: u16 = 2;
const MAX_INPUT_ROWS: usize = 6;

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
    let input_lines = visible_input_lines(&state.input, area.width);
    let mut lines = input_lines
        .into_iter()
        .map(|text| {
            Line::from(vec![
                Span::styled("▌ ", Style::default().fg(rail).bg(theme.element)),
                Span::styled(text, Style::default().fg(theme.text).bg(theme.element)),
            ])
        })
        .collect::<Vec<_>>();
    lines.push(composer_metadata(state, theme, area.width, policy_width));
    if !state.attachments.is_empty() {
        lines.push(attachment_badges(state, theme, area.width));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(theme.text).bg(theme.element)),
        area,
    );
}

#[must_use]
pub fn prompt_height(state: &AppState, width: u16) -> u16 {
    let input_rows = u16::try_from(visible_input_lines(&state.input, width).len()).unwrap_or(6);
    let attachment_rows = u16::from(!state.attachments.is_empty());
    input_rows + 1 + attachment_rows
}

#[must_use]
pub fn prompt_cursor(state: &AppState, area: Rect) -> Option<(u16, u16)> {
    if state.permission.is_some() || state.running {
        return None;
    }
    let input_lines = visible_input_lines(&state.input, area.width);
    let last_line = input_lines.last().map_or("", String::as_str);
    let typed = u16::try_from(UnicodeWidthStr::width(last_line)).unwrap_or(u16::MAX);
    let cursor_y = area
        .y
        .saturating_add(u16::try_from(input_lines.len().saturating_sub(1)).unwrap_or(0));
    let rightmost = area.x + area.width.saturating_sub(1);
    let cursor_x = (area.x + PROMPT_GUTTER_WIDTH)
        .saturating_add(typed)
        .min(rightmost);
    Some((cursor_x, cursor_y))
}

fn visible_input_lines(input: &str, width: u16) -> Vec<String> {
    let mut lines = wrapped_input_lines(input, width);
    let keep_from = lines.len().saturating_sub(MAX_INPUT_ROWS);
    lines.drain(..keep_from);
    lines
}

fn wrapped_input_lines(input: &str, width: u16) -> Vec<String> {
    let content_width = usize::from(width.saturating_sub(PROMPT_GUTTER_WIDTH).max(1));
    let mut lines = Vec::new();
    if input.is_empty() {
        lines.push(String::new());
        return lines;
    }
    for segment in input.split('\n') {
        push_wrapped_segment(segment, content_width, &mut lines);
    }
    lines
}

fn push_wrapped_segment(segment: &str, width: usize, lines: &mut Vec<String>) {
    if segment.is_empty() {
        lines.push(String::new());
        return;
    }
    let mut current = String::new();
    let mut current_width = 0usize;
    for ch in segment.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width > 0 && current_width.saturating_add(ch_width) > width {
            lines.push(current);
            current = String::new();
            current_width = 0;
        }
        current.push(ch);
        current_width = current_width.saturating_add(ch_width);
    }
    lines.push(current);
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

    #[test]
    fn prompt_cursor_tracks_the_last_multiline_input_row() {
        // Given: a composer input containing an explicit newline.
        let state = AppState {
            input: "first\nsecond".to_string(),
            ..AppState::default()
        };
        let area = Rect::new(10, 20, 40, 3);

        // When: the prompt asks ratatui where to draw the terminal cursor.
        let cursor = prompt_cursor(&state, area);

        // Then: the cursor sits after the final row instead of the first row.
        assert_eq!(cursor, Some((18, 21)));
    }
}
