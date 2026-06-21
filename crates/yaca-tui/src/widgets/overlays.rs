use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::theme::Theme;
use crate::{DialogView, Picker, QuestionPrompt};

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

pub fn render_question(frame: &mut Frame, question: &QuestionPrompt, theme: &Theme) {
    let area = frame.area();
    let extra = u16::try_from(question.options.len()).unwrap_or(0);
    let height = (7u16.saturating_add(extra)).min(area.height).max(5);
    let width = area.width.saturating_sub(8).clamp(24, 76);
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
            ellipsize(&question.prompt, inner_width),
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for (idx, option) in question.options.iter().enumerate() {
        let style = if idx == question.selected {
            Style::default()
                .fg(theme.background)
                .bg(theme.primary)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        lines.push(Line::from(Span::styled(
            format!(" {} ", ellipsize(option, inner_width)),
            style,
        )));
    }
    if question.options.is_empty() || question.allow_custom {
        lines.push(Line::from(vec![
            Span::styled("> ", Style::default().fg(theme.primary)),
            Span::styled(question.input.clone(), Style::default().fg(theme.text)),
        ]));
    }
    let hint = if question.options.is_empty() {
        "type your answer · Enter confirm · Esc cancel"
    } else if question.allow_custom {
        "Up/Down select · type for custom · Enter confirm · Esc cancel"
    } else {
        "Up/Down select · Enter confirm · Esc cancel"
    };
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(theme.muted),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme.text).bg(theme.element))
            .block(
                Block::default()
                    .title("question")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border_active)),
            )
            .wrap(Wrap { trim: false }),
        rect,
    );
}

pub fn render_picker(frame: &mut Frame, picker: &Picker, theme: &Theme) {
    let area = frame.area();
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
    frame.render_widget(Clear, rect);
    let inner_width = usize::from(width).saturating_sub(6);
    let mut lines = vec![Line::from(Span::styled(
        "Up/Down select · Enter confirm · Esc cancel",
        Style::default().fg(theme.muted),
    ))];
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
            .block(
                Block::default()
                    .title(picker.title.clone())
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border_active)),
            )
            .wrap(Wrap { trim: false }),
        rect,
    );
}

pub(super) fn ellipsize(s: &str, max: usize) -> String {
    let cleaned = s.replace('\n', " ");
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let head: String = cleaned.chars().take(max).collect();
        format!("{head}…")
    }
}
