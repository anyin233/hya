use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

use super::sidebar_context::sidebar_lines;
use super::sidebar_footer::sidebar_footer_lines;
use crate::AppState;
use crate::theme::Theme;

pub fn render_sidebar(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.panel)),
        area,
    );
    let footer = sidebar_footer_lines(state, theme, area.width, area.height);
    let footer_height = u16::try_from(footer.len())
        .unwrap_or(u16::MAX)
        .min(area.height);
    let body_height = area.height.saturating_sub(footer_height);
    if body_height > 0 {
        let body_area = Rect {
            height: body_height,
            ..area
        };
        let body_lines = visible_body_lines(sidebar_lines(state, theme), body_height);
        frame.render_widget(
            Paragraph::new(body_lines).style(Style::default().fg(theme.text).bg(theme.panel)),
            body_area,
        );
    }
    if footer_height == 0 {
        return;
    }
    let footer_area = Rect {
        y: area.y + body_height,
        height: footer_height,
        ..area
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(theme.text).bg(theme.panel)),
        footer_area,
    );
}

fn visible_body_lines(mut lines: Vec<Line<'static>>, height: u16) -> Vec<Line<'static>> {
    let max_lines = usize::from(height);
    if lines.len() <= max_lines {
        return lines;
    }
    lines.truncate(max_lines);
    truncate_unclosed_card(&mut lines);
    lines
}

fn truncate_unclosed_card(lines: &mut Vec<Line<'static>>) {
    let mut last_open = None;
    let mut last_close = None;
    for (index, line) in lines.iter().enumerate() {
        if line_contains(line, '┌') {
            last_open = Some(index);
        }
        if line_contains(line, '└') {
            last_close = Some(index);
        }
    }
    if let Some(open) = last_open
        && last_close.is_none_or(|close| close < open)
    {
        lines.truncate(open);
    }
}

fn line_contains(line: &Line<'_>, target: char) -> bool {
    line.spans.iter().any(|span| span.content.contains(target))
}
