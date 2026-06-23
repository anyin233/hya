use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::Picker;
use crate::theme::Theme;

use super::{clear_overlay_band, ellipsize};

pub fn render_picker(frame: &mut Frame, area: Rect, picker: &Picker, theme: &Theme) {
    let item_rows = u16::try_from(picker.entries.len())
        .unwrap_or(u16::MAX)
        .min(10);
    let height = item_rows.saturating_add(5).min(area.height).max(6);
    let width = area.width.saturating_sub(8).clamp(24, 76);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    clear_overlay_band(frame, area, rect);
    let inner_width = usize::from(width).saturating_sub(6);
    let mut lines = vec![
        Line::from(Span::styled(
            picker.title.clone(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "↑↓ select   enter confirm   esc dismiss",
            Style::default().fg(theme.muted),
        )),
    ];
    lines.push(Line::from(""));
    for (idx, label) in picker
        .entries
        .iter()
        .enumerate()
        .take(usize::from(item_rows))
    {
        let selected = idx == picker.selected;
        let marker = if selected { "> " } else { "  " };
        let style = if selected {
            Style::default()
                .fg(theme.background)
                .bg(theme.primary)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        lines.push(Line::from(vec![
            Span::styled(marker.to_string(), Style::default().fg(theme.primary)),
            Span::styled(ellipsize(label, inner_width), style),
        ]));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme.text).bg(theme.element))
            .wrap(Wrap { trim: false }),
        rect,
    );
}
