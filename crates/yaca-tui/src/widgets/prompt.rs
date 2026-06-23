use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::footer_text::footer_left_text;
use super::prompt_attachments::attachment_badges;
use super::prompt_metadata::{composer_footer_metadata, composer_identity_metadata};
use crate::AppState;
use crate::theme::Theme;

const PROMPT_GUTTER_WIDTH: u16 = 2;
const MAX_INPUT_ROWS: usize = 6;
const FIRST_PROMPT_PLACEHOLDER: &str = r#"Ask anything... "Fix a TODO in the codebase""#;

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
    let input_lines = visible_input_lines(&state.input, state.input_cursor, area.width);
    let mut lines = vec![Line::from("")];
    let show_placeholder =
        !state.running && state.input.is_empty() && state.projection.session.messages.is_empty();
    lines.extend(input_lines.into_iter().enumerate().map(|(idx, text)| {
        let placeholder = show_placeholder && idx == 0;
        let text = if placeholder {
            FIRST_PROMPT_PLACEHOLDER.to_string()
        } else {
            text
        };
        let text_style = if placeholder {
            Style::default().fg(theme.muted).bg(theme.element)
        } else {
            Style::default().fg(theme.text).bg(theme.element)
        };
        Line::from(vec![
            Span::styled("▌ ", Style::default().fg(rail).bg(theme.element)),
            Span::styled(text, text_style),
        ])
    }));
    lines.push(composer_identity_metadata(
        state,
        theme,
        area.width,
        policy_width,
    ));
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
    let input_rows =
        u16::try_from(visible_input_lines(&state.input, state.input_cursor, width).len())
            .unwrap_or(6);
    let attachment_rows = u16::from(!state.attachments.is_empty());
    input_rows + 2 + attachment_rows
}

#[must_use]
pub fn prompt_cursor(state: &AppState, area: Rect) -> Option<(u16, u16)> {
    if state.permission.is_some() || state.running {
        return None;
    }
    let input_window = visible_input_window(&state.input, state.input_cursor, area.width);
    let cursor_prefix = input_cursor_prefix(&state.input, state.input_cursor);
    let cursor_lines = wrapped_input_lines(cursor_prefix, area.width);
    let cursor_row = cursor_lines.len().saturating_sub(1);
    let cursor_line = cursor_lines.last().map_or("", String::as_str);
    let typed = u16::try_from(UnicodeWidthStr::width(cursor_line)).unwrap_or(u16::MAX);
    let cursor_y = area
        .y
        .saturating_add(1)
        .saturating_add(u16::try_from(cursor_row.saturating_sub(input_window.start)).unwrap_or(0));
    let rightmost = area.x + area.width.saturating_sub(1);
    let cursor_x = (area.x + PROMPT_GUTTER_WIDTH)
        .saturating_add(typed)
        .min(rightmost);
    Some((cursor_x, cursor_y))
}

fn input_cursor_prefix(input: &str, cursor: Option<usize>) -> &str {
    let mut idx = cursor.unwrap_or(input.len()).min(input.len());
    while !input.is_char_boundary(idx) {
        idx = idx.saturating_sub(1);
    }
    &input[..idx]
}

struct VisibleInputWindow {
    lines: Vec<String>,
    start: usize,
}

fn visible_input_lines(input: &str, cursor: Option<usize>, width: u16) -> Vec<String> {
    visible_input_window(input, cursor, width).lines
}

fn visible_input_window(input: &str, cursor: Option<usize>, width: u16) -> VisibleInputWindow {
    let mut lines = wrapped_input_lines(input, width);
    let cursor_row = input_cursor_row(input, cursor, width);
    let tail_start = lines.len().saturating_sub(MAX_INPUT_ROWS);
    let start = if cursor.is_some() && cursor_row < tail_start {
        cursor_row
    } else {
        tail_start
    };
    lines.drain(..start);
    lines.truncate(MAX_INPUT_ROWS);
    VisibleInputWindow { lines, start }
}

fn input_cursor_row(input: &str, cursor: Option<usize>, width: u16) -> usize {
    let cursor_prefix = input_cursor_prefix(input, cursor);
    wrapped_input_lines(cursor_prefix, width)
        .len()
        .saturating_sub(1)
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

pub fn render_footer(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    policy_width: u16,
) {
    frame.render_widget(
        Paragraph::new(composer_footer_metadata(
            state,
            theme,
            area.width,
            policy_width,
            footer_left_text(state, policy_width),
        ))
        .style(theme.base()),
        area,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::{prompt_cursor, visible_input_lines};
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
        assert_eq!(cursor, Some((16, 21)));
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
        assert_eq!(cursor, Some((18, 22)));
    }

    #[test]
    fn prompt_cursor_uses_explicit_input_cursor() {
        // Given: the composer cursor is in the middle of mixed-width input.
        let state = AppState {
            input: "ab你好".to_string(),
            input_cursor: Some(2),
            ..AppState::default()
        };
        let area = Rect::new(10, 20, 40, 2);

        // When: the prompt asks ratatui where to draw the terminal cursor.
        let cursor = prompt_cursor(&state, area);

        // Then: the cursor lands after "ab", not at the end of the input.
        assert_eq!(cursor, Some((14, 21)));
    }

    #[test]
    fn prompt_visible_rows_follow_explicit_cursor_above_tail_window() {
        // Given: the input is taller than the six-row composer viewport.
        let state = AppState {
            input: "zero\none\ntwo\nthree\nfour\nfive\nsix\nseven".to_string(),
            input_cursor: Some(0),
            ..AppState::default()
        };
        let area = Rect::new(10, 20, 40, 6);

        // When: the prompt computes visible rows and the terminal cursor.
        let rows = visible_input_lines(&state.input, state.input_cursor, area.width);
        let cursor = prompt_cursor(&state, area);

        // Then: the row containing the logical cursor is visible at the top.
        assert_eq!(rows.first().map(String::as_str), Some("zero"));
        assert_eq!(cursor, Some((12, 21)));
    }
}
