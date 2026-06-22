use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Paragraph};

use super::sidebar_context::sidebar_lines;
use crate::AppState;
use crate::theme::Theme;

pub fn render_sidebar(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.panel)),
        area,
    );
    frame.render_widget(
        Paragraph::new(sidebar_lines(state, theme))
            .style(Style::default().fg(theme.text).bg(theme.panel)),
        area,
    );
}
