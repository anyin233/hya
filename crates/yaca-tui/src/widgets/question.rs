use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::overlays::ellipsize;
use crate::QuestionPrompt;
use crate::theme::Theme;

pub fn render_question(frame: &mut Frame, question: &QuestionPrompt, theme: &Theme) {
    let area = frame.area();
    let footer_height = u16::from(area.height > 1);
    let extra = u16::try_from(question.options.len()).unwrap_or(u16::MAX);
    let height = (6u16.saturating_add(extra)).min(area.height.saturating_sub(footer_height));
    if height == 0 {
        return;
    }
    let width = area.width.saturating_sub(4).max(12);
    let y = area.y + area.height.saturating_sub(height + footer_height);
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

    let inner_width = usize::from(width).saturating_sub(6);
    let mut lines = vec![
        Line::from(Span::styled(
            ellipsize(&question.prompt, inner_width),
            Style::default().fg(theme.text),
        )),
        Line::from(""),
    ];
    for (idx, option) in question.options.iter().enumerate() {
        lines.push(option_line(
            idx,
            option,
            idx == question.selected,
            inner_width,
            theme,
        ));
    }
    if question.allow_custom && !question.options.is_empty() {
        let custom_idx = question.options.len();
        lines.push(option_line(
            custom_idx,
            "Type your own answer",
            question.selected == custom_idx,
            inner_width,
            theme,
        ));
    }
    let mut cursor = None;
    if question.options.is_empty() {
        cursor = Some(question_cursor_position(
            question,
            rect.x.saturating_add(2),
            rect.y
                .saturating_add(u16::try_from(lines.len()).unwrap_or(u16::MAX)),
            inner_width.saturating_sub(2),
        ));
        lines.push(Line::from(vec![
            Span::styled("> ", Style::default().fg(theme.primary)),
            Span::styled(question.input.clone(), Style::default().fg(theme.text)),
        ]));
    } else if question.allow_custom
        && (question.selected == question.options.len() || !question.input.is_empty())
    {
        cursor = Some(question_cursor_position(
            question,
            rect.x.saturating_add(3),
            rect.y
                .saturating_add(u16::try_from(lines.len()).unwrap_or(u16::MAX)),
            inner_width.saturating_sub(3),
        ));
        lines.push(Line::from(Span::styled(
            format!(
                "   {}",
                ellipsize(&question.input, inner_width.saturating_sub(3))
            ),
            Style::default().fg(theme.muted),
        )));
    }
    lines.push(Line::from(Span::styled(
        question_hint(question),
        Style::default().fg(theme.muted),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme.text).bg(theme.element))
            .wrap(Wrap { trim: false }),
        rect,
    );
    if let Some(cursor) = cursor {
        frame.set_cursor_position(cursor);
    }
}

fn option_line(
    idx: usize,
    option: &str,
    selected: bool,
    inner_width: usize,
    theme: &Theme,
) -> Line<'static> {
    let number_style = if selected {
        Style::default().fg(theme.muted).bg(theme.element)
    } else {
        Style::default().fg(theme.muted)
    };
    let option_style = if selected {
        Style::default().fg(theme.info).bg(theme.element)
    } else {
        Style::default().fg(theme.text)
    };
    let gap_style = if selected {
        Style::default().bg(theme.element)
    } else {
        Style::default()
    };
    let prefix = format!("{}.", idx + 1);
    Line::from(vec![
        Span::styled(prefix.clone(), number_style),
        Span::styled(" ", gap_style),
        Span::styled(
            ellipsize(option, inner_width.saturating_sub(prefix.len() + 1)),
            option_style,
        ),
    ])
}

const fn question_hint(question: &QuestionPrompt) -> &'static str {
    if question.options.is_empty() {
        "enter submit   esc dismiss"
    } else {
        "↑↓ select   enter submit   esc dismiss"
    }
}

fn question_cursor_position(
    question: &QuestionPrompt,
    x: u16,
    y: u16,
    max_width: usize,
) -> Position {
    let prefix = input_cursor_prefix(&question.input, question.input_cursor);
    let column = UnicodeWidthStr::width(prefix).min(max_width);
    Position {
        x: x.saturating_add(u16::try_from(column).unwrap_or(u16::MAX)),
        y,
    }
}

fn input_cursor_prefix(input: &str, cursor: Option<usize>) -> &str {
    let mut idx = cursor.unwrap_or(input.len()).min(input.len());
    while !input.is_char_boundary(idx) {
        idx = idx.saturating_sub(1);
    }
    &input[..idx]
}
