use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::AppState;
use crate::theme::Theme;

const MAX_TEXTAREA_ROWS: u16 = 6;
const PROMPT_PREFIX: &str = "> ";

#[derive(Clone, Copy)]
enum PromptStyle {
    Prefix,
    Text,
}

#[derive(Clone)]
struct PromptPart {
    text: String,
    style: PromptStyle,
}

pub fn render_prompt(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let title = if state.yolo {
        " message — YOLO · Enter send · Tab yolo off · Ctrl-C clear/interrupt · F2 model "
    } else {
        " message — Enter send · Tab yolo · / or @ popup · Ctrl-C clear/interrupt · F2 model "
    };
    let text_width = prompt_text_width(area.width);
    let wrapped = wrapped_prompt(&state.input, text_width);
    let rows = visible_rows(&wrapped);
    let cursor_row = total_rows(&wrapped).saturating_sub(1);
    let start = viewport_start(total_rows(&wrapped), cursor_row, rows);
    let lines = visible_prompt_lines(&wrapped, start, rows, theme);
    let widget = Paragraph::new(lines)
        .style(Style::default().fg(theme.text).bg(theme.panel))
        .block(
            Block::default()
                .title(title)
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
pub fn prompt_height(input: &str, area_width: u16) -> u16 {
    visible_rows(&wrapped_prompt(input, prompt_text_width(area_width))) + 2
}

#[must_use]
pub fn prompt_cursor(state: &AppState, area: Rect) -> Option<(u16, u16)> {
    if state.permission.is_some() || state.running {
        return None;
    }
    let text_width = prompt_text_width(area.width);
    let wrapped = wrapped_prompt(&state.input, text_width);
    let rows = visible_rows(&wrapped);
    let cursor_row = total_rows(&wrapped).saturating_sub(1);
    let start = viewport_start(total_rows(&wrapped), cursor_row, rows);
    let row = cursor_row.saturating_sub(start).min(rows.saturating_sub(1));
    let col = u16::try_from(wrapped.last().map_or(0, |line| line_width(line)))
        .unwrap_or(u16::MAX)
        .min(text_width.saturating_sub(1));
    let rightmost = area.x.saturating_add(area.width.saturating_sub(2));
    let bottom = area.y.saturating_add(area.height.saturating_sub(2));
    Some((
        area.x.saturating_add(1).saturating_add(col).min(rightmost),
        area.y.saturating_add(1).saturating_add(row).min(bottom),
    ))
}

fn prompt_text_width(area_width: u16) -> u16 {
    area_width.saturating_sub(2).max(1)
}

fn total_rows(lines: &[Vec<PromptPart>]) -> u16 {
    u16::try_from(lines.len()).unwrap_or(u16::MAX).max(1)
}

fn visible_rows(lines: &[Vec<PromptPart>]) -> u16 {
    total_rows(lines).min(MAX_TEXTAREA_ROWS)
}

fn viewport_start(total_rows: u16, cursor_row: u16, visible_rows: u16) -> u16 {
    if total_rows <= visible_rows {
        return 0;
    }
    cursor_row
        .saturating_add(1)
        .saturating_sub(visible_rows)
        .min(total_rows - visible_rows)
}

fn visible_prompt_lines(
    lines: &[Vec<PromptPart>],
    start: u16,
    rows: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    lines
        .iter()
        .skip(usize::from(start))
        .take(usize::from(rows))
        .map(|line| prompt_line(line, theme))
        .collect()
}

fn prompt_line(parts: &[PromptPart], theme: &Theme) -> Line<'static> {
    Line::from(
        parts
            .iter()
            .map(|part| {
                let color = match part.style {
                    PromptStyle::Prefix => theme.primary,
                    PromptStyle::Text => theme.text,
                };
                Span::styled(part.text.clone(), Style::default().fg(color))
            })
            .collect::<Vec<_>>(),
    )
}

fn wrapped_prompt(input: &str, text_width: u16) -> Vec<Vec<PromptPart>> {
    let target = usize::from(text_width.max(1));
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut width = 0usize;
    for (grapheme, style) in PROMPT_PREFIX
        .graphemes(true)
        .map(|grapheme| (grapheme, PromptStyle::Prefix))
        .chain(
            input
                .graphemes(true)
                .map(|grapheme| (grapheme, PromptStyle::Text)),
        )
    {
        if grapheme == "\n" {
            lines.push(std::mem::take(&mut line));
            width = 0;
            continue;
        }
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width > 0 && width + grapheme_width > target {
            lines.push(std::mem::take(&mut line));
            width = 0;
        }
        line.push(PromptPart {
            text: grapheme.to_string(),
            style,
        });
        width += grapheme_width;
    }
    if lines.is_empty() || !line.is_empty() || input.ends_with('\n') {
        lines.push(line);
    }
    lines
}

fn line_width(line: &[PromptPart]) -> usize {
    line.iter()
        .map(|part| UnicodeWidthStr::width(part.text.as_str()))
        .sum()
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

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::*;
    use crate::AppState;

    #[test]
    fn prompt_height_wraps_long_input_and_caps_visible_rows() {
        assert_eq!(prompt_height("short", 12), 3);
        assert!(prompt_height("alpha beta gamma", 12) > 3);
        assert_eq!(prompt_height(&"x".repeat(200), 12), 8);
        assert_eq!(prompt_height("one\ntwo\nthree", 12), 5);
    }

    #[test]
    fn prompt_cursor_tracks_wrapped_viewport_bottom() {
        let area = Rect::new(5, 7, 8, 8);
        let state = AppState {
            input: "x".repeat(40),
            ..AppState::default()
        };

        assert_eq!(prompt_cursor(&state, area), Some((11, 13)));
    }
}
