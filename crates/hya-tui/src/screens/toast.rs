use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::render::draw::rgba_to_color;
use crate::theme::ResolvedTheme;

pub fn draw(frame: &mut ratatui::Frame<'_>, message: &str, theme: &ResolvedTheme) {
    if message.is_empty() {
        return;
    }
    let bg = theme.background;
    let area = frame.area();
    let width = (message.chars().count() as u16 + 4).clamp(8, area.width);
    let toast_area = Rect {
        x: area.x + area.width.saturating_sub(width),
        y: area.y,
        width,
        height: 3.min(area.height),
    };
    frame.render_widget(Clear, toast_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(rgba_to_color(theme.accent, bg)))
        .style(Style::default().bg(rgba_to_color(theme.background_panel, bg)));
    let paragraph = Paragraph::new(format!(" {message}"))
        .block(block)
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(rgba_to_color(theme.text, bg)));
    frame.render_widget(paragraph, toast_area);
}
