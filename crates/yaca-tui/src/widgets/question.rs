use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use super::overlays::ellipsize;
use crate::QuestionPrompt;
use crate::theme::Theme;

pub fn render_question(frame: &mut Frame, question: &QuestionPrompt, theme: &Theme) {
    let area = frame.area();
    let footer_height = u16::from(area.height > 1);
    let extra = u16::try_from(question.options.len()).unwrap_or(u16::MAX);
    let height = (7u16.saturating_add(extra)).min(area.height.saturating_sub(footer_height));
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
            "question",
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            ellipsize(&question.prompt, inner_width),
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
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
    if question.options.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("> ", Style::default().fg(theme.primary)),
            Span::styled(question.input.clone(), Style::default().fg(theme.text)),
        ]));
    } else if question.allow_custom && !question.input.is_empty() {
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
}

fn option_line(
    idx: usize,
    option: &str,
    selected: bool,
    inner_width: usize,
    theme: &Theme,
) -> Line<'static> {
    let active_style = Style::default()
        .fg(theme.background)
        .bg(theme.primary)
        .add_modifier(Modifier::BOLD);
    let number_style = if selected {
        active_style
    } else {
        Style::default().fg(theme.muted)
    };
    let option_style = if selected {
        active_style
    } else {
        Style::default().fg(theme.text)
    };
    let gap_style = if selected {
        active_style
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
    if question.options.is_empty() || (question.allow_custom && !question.input.is_empty()) {
        "enter save   esc cancel"
    } else {
        "↑↓ select   enter submit   esc dismiss"
    }
}
