use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::theme::Theme;
use crate::{DialogView, PermissionPrompt};

pub fn render_permission(frame: &mut Frame, prompt: &PermissionPrompt, theme: &Theme) {
    let area = frame.area();
    let height = 9u16.min(area.height);
    let width = area.width.saturating_sub(4).max(12);
    let rect = Rect {
        x: area.x + 2,
        y: area.height.saturating_sub(height),
        width,
        height,
    };
    frame.render_widget(Clear, rect);
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

pub fn render_dialog(frame: &mut Frame, dialog: &DialogView, theme: &Theme) {
    let area = frame.area();
    let width = area.width.saturating_sub(8).clamp(24, 76);
    let item_rows = u16::try_from(dialog.items.len())
        .unwrap_or(u16::MAX)
        .min(10);
    let height = item_rows.saturating_add(6).min(area.height).max(8);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);

    let inner_width = usize::from(width).saturating_sub(6);
    let mut lines = vec![
        Line::from(Span::styled(
            dialog.subtitle.clone(),
            Style::default().fg(theme.muted),
        )),
        Line::from(""),
    ];
    for (idx, item) in dialog.items.iter().enumerate().take(usize::from(item_rows)) {
        let selected = idx == dialog.selected;
        let marker = if selected { "> " } else { "  " };
        let style = if selected {
            Style::default()
                .fg(theme.background)
                .bg(theme.primary)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        let detail = if item.detail.is_empty() {
            String::new()
        } else {
            format!("  {}", ellipsize(&item.detail, inner_width / 2))
        };
        lines.push(Line::from(vec![
            Span::styled(marker.to_string(), Style::default().fg(theme.primary)),
            Span::styled(item.label.clone(), style),
            Span::styled(detail, Style::default().fg(theme.muted)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Up/Down or Tab select · Enter confirm · Esc cancel",
        Style::default().fg(theme.muted),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme.text).bg(theme.element))
            .block(
                Block::default()
                    .title(dialog.title.clone())
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border_active)),
            )
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn ellipsize(s: &str, max: usize) -> String {
    let cleaned = s.replace('\n', " ");
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let head: String = cleaned.chars().take(max).collect();
        format!("{head}…")
    }
}
