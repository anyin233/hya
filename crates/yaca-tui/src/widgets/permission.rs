use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::overlays::ellipsize;
use crate::PermissionPrompt;
use crate::theme::Theme;

pub fn render_permission(frame: &mut Frame, prompt: &PermissionPrompt, theme: &Theme) {
    let area = frame.area();
    let height = 9u16.min(area.height);
    let width = area.width.saturating_sub(4).max(12);
    let y = area.y + area.height.saturating_sub(height);
    let clear_rect = Rect {
        x: area.x,
        y,
        width: area.width,
        height,
    };
    let rect = Rect {
        x: area.x + 2,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, clear_rect);

    let inner_width = usize::from(width).saturating_sub(4);
    let option_spans: Vec<Span> = prompt
        .options()
        .iter()
        .enumerate()
        .flat_map(|(idx, label)| {
            let style = if idx == prompt.selected {
                Style::default()
                    .fg(theme.background)
                    .bg(theme.primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            vec![Span::styled(format!(" {label} "), style), Span::raw(" ")]
        })
        .collect();
    let lines = vec![
        Line::from(Span::styled(
            format!("{} wants to run:", prompt.title),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(ellipsize(&prompt.detail, inner_width)),
        Line::from(""),
        Line::from(option_spans),
        Line::from(vec![
            Span::styled("reply: ", Style::default().fg(theme.muted)),
            Span::styled(prompt.reply.clone(), Style::default().fg(theme.text)),
            Span::styled("█", Style::default().fg(theme.primary)),
        ]),
        Line::from(Span::styled(
            "←/→ select · type a reply · Enter confirm · Esc deny",
            Style::default().fg(theme.muted),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme.text).bg(theme.element))
            .block(
                Block::default()
                    .title("permission required")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.warning)),
            )
            .wrap(Wrap { trim: false }),
        rect,
    );
}
