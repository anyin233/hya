use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
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
    let footer = sidebar_footer_lines(state, theme, area.width);
    let footer_height = u16::try_from(footer.len())
        .unwrap_or(u16::MAX)
        .min(area.height);
    let body_height = area.height.saturating_sub(footer_height);
    if body_height > 0 {
        let body_area = Rect {
            height: body_height,
            ..area
        };
        frame.render_widget(
            Paragraph::new(sidebar_lines(state, theme))
                .style(Style::default().fg(theme.text).bg(theme.panel)),
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
