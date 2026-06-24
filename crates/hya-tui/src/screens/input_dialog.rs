use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::render::draw::rgba_to_color;
use crate::theme::{selected_foreground, ResolvedTheme};

const WIDTH: u16 = 60;

pub const CONFIRM_OPTIONS: [&str; 2] = ["Confirm", "Cancel"];
pub(crate) const PROMPT_HEIGHT: u16 = 5;
pub(crate) const CONFIRM_HEIGHT: u16 = 6;

pub(crate) fn dialog_area(screen: Rect, height: u16) -> Rect {
    let width = WIDTH.min(screen.width.saturating_sub(2));
    Rect {
        x: screen.x + screen.width.saturating_sub(width) / 2,
        y: screen.y + screen.height / 4,
        width,
        height: height.min(screen.height.saturating_sub(screen.height / 4)),
    }
}

fn header(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    theme: &ResolvedTheme,
    panel: ratatui::style::Color,
) {
    let inner_x = area.x + 4;
    let inner_w = area.width.saturating_sub(8);
    frame.render_widget(
        Paragraph::new(title).style(
            Style::default()
                .fg(rgba_to_color(theme.text, theme.background))
                .bg(panel)
                .add_modifier(Modifier::BOLD),
        ),
        Rect {
            x: inner_x,
            y: area.y + 1,
            width: inner_w.saturating_sub(4),
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new("esc").alignment(Alignment::Right).style(
            Style::default()
                .fg(rgba_to_color(theme.text_muted, theme.background))
                .bg(panel),
        ),
        Rect {
            x: inner_x + inner_w.saturating_sub(4),
            y: area.y + 1,
            width: 4,
            height: 1,
        },
    );
}

pub fn draw_prompt(
    frame: &mut ratatui::Frame<'_>,
    title: &str,
    input: &str,
    theme: &ResolvedTheme,
) {
    let screen = frame.area();
    let bg = theme.background;
    let panel = rgba_to_color(theme.background_panel, bg);
    let area = dialog_area(screen, PROMPT_HEIGHT);
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(panel)), area);
    header(frame, area, title, theme, panel);

    let inner_x = area.x + 4;
    let inner_w = area.width.saturating_sub(8);
    frame.render_widget(
        Paragraph::new(format!("{input}▏"))
            .style(Style::default().fg(rgba_to_color(theme.text, bg)).bg(panel)),
        Rect {
            x: inner_x,
            y: area.y + 3,
            width: inner_w,
            height: 1,
        },
    );
}

pub fn draw_confirm(
    frame: &mut ratatui::Frame<'_>,
    title: &str,
    message: &str,
    selected: usize,
    theme: &ResolvedTheme,
) {
    let screen = frame.area();
    let bg = theme.background;
    let panel = rgba_to_color(theme.background_panel, bg);
    let area = dialog_area(screen, CONFIRM_HEIGHT);
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(panel)), area);
    header(frame, area, title, theme, panel);

    let inner_x = area.x + 4;
    let inner_w = area.width.saturating_sub(8);
    frame.render_widget(
        Paragraph::new(message).style(
            Style::default()
                .fg(rgba_to_color(theme.text_muted, bg))
                .bg(panel),
        ),
        Rect {
            x: inner_x,
            y: area.y + 3,
            width: inner_w,
            height: 1,
        },
    );

    let mut spans: Vec<Span> = Vec::new();
    for (index, label) in CONFIRM_OPTIONS.iter().enumerate() {
        let (fg, button_bg) = if index == selected {
            (
                rgba_to_color(selected_foreground(theme, Some(theme.primary)), bg),
                rgba_to_color(theme.primary, bg),
            )
        } else {
            (
                rgba_to_color(theme.text_muted, bg),
                rgba_to_color(theme.background_menu, bg),
            )
        };
        spans.push(Span::styled(
            format!(" {label} "),
            Style::default().fg(fg).bg(button_bg),
        ));
        spans.push(Span::styled("  ", Style::default().bg(panel)));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(panel)),
        Rect {
            x: inner_x,
            y: area.y + 5,
            width: inner_w,
            height: 1,
        },
    );
}

pub(crate) fn confirm_button_at(screen: Rect, column: u16, row: u16) -> Option<usize> {
    let area = dialog_area(screen, CONFIRM_HEIGHT);
    if row != area.y + 5 {
        return None;
    }
    let mut x = area.x + 4;
    for (index, label) in CONFIRM_OPTIONS.iter().enumerate() {
        let width = label.len() as u16 + 2;
        if column >= x && column < x + width {
            return Some(index);
        }
        x += width + 2;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{confirm_button_at, dialog_area, CONFIRM_HEIGHT};
    use ratatui::layout::Rect;

    #[test]
    fn confirm_button_at_maps_cells_to_each_button() {
        let screen = Rect::new(0, 0, 160, 48);
        let area = dialog_area(screen, CONFIRM_HEIGHT);
        let row = area.y + 5;
        assert_eq!(confirm_button_at(screen, area.x + 6, row), Some(0));
        assert_eq!(confirm_button_at(screen, area.x + 17, row), Some(1));
        assert_eq!(confirm_button_at(screen, area.x + 6, row - 1), None);
        assert_eq!(confirm_button_at(screen, area.x + 14, row), None);
        assert_eq!(confirm_button_at(screen, area.x, row), None);
    }
}
