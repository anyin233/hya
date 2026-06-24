use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Clear, Paragraph};
use serde_json::Value;

use crate::contracts::Rgba;
use crate::render::draw::{rgba_to_color, text_to_ratatui};
use crate::render::text::{Attrs, Line, Span, Text};
use crate::theme::{selected_foreground, ResolvedTheme};

use super::footer::draw_footer;
use super::model::{custom_enabled, multiple, options, questions, single, text_field};
use super::state::QuestionState;

const MAX_HEIGHT: u16 = 20;

pub fn draw(
    frame: &mut ratatui::Frame<'_>,
    request: &Value,
    state: &QuestionState,
    theme: &ResolvedTheme,
) {
    let screen = frame.area();
    let bg = theme.background;
    let panel = rgba_to_color(theme.background_panel, bg);
    let lines = render_lines(request, state, theme);
    let height = (lines.len() as u16 + 2).min(MAX_HEIGHT).min(screen.height);
    let area = Rect {
        x: screen.x,
        y: screen.y + screen.height.saturating_sub(height),
        width: screen.width,
        height,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(panel)), area);
    let content_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(2),
    };
    frame.render_widget(
        Paragraph::new(text_to_ratatui(&Text(lines), bg))
            .style(Style::default().fg(rgba_to_color(theme.text, bg)).bg(panel)),
        content_area,
    );
    draw_footer(frame, area, request, state, theme);
}

#[must_use]
pub fn render_lines(request: &Value, state: &QuestionState, theme: &ResolvedTheme) -> Vec<Line> {
    let mut lines = vec![Line(vec![bar(theme.accent)])];
    if !single(request) {
        lines.push(tab_line(request, state, theme));
        lines.push(Line(vec![bar(theme.accent)]));
    }
    if state.confirm(request) {
        lines.extend(review_lines(request, state, theme));
        return lines;
    }
    lines.extend(question_lines(request, state, theme));
    lines
}

fn question_lines(request: &Value, state: &QuestionState, theme: &ResolvedTheme) -> Vec<Line> {
    let Some(question) = questions(request).get(state.tab()) else {
        return Vec::new();
    };
    let suffix = if multiple(question) {
        " (select all that apply)"
    } else {
        ""
    };
    let mut lines = vec![Line(vec![
        bar(theme.accent),
        pad(2),
        Span::styled(
            format!("{}{}", text_field(question, "question"), suffix),
            Some(theme.text),
            None,
            Attrs::default(),
        ),
    ])];
    for (index, option) in options(question).iter().enumerate() {
        let label = text_field(option, "label");
        lines.push(option_line(
            index,
            &label,
            state.is_picked(&label),
            state.selected() == index,
            question,
            theme,
        ));
        let description = text_field(option, "description");
        if !description.is_empty() {
            lines.push(description_line(&description, theme));
        }
    }
    if custom_enabled(question) {
        let index = options(question).len();
        lines.push(option_line(
            index,
            "Type your own answer",
            state.custom_picked(),
            state.selected() == index,
            question,
            theme,
        ));
        if state.editing() {
            lines.push(description_line(&format!("{}▏", state.edit()), theme));
        } else if !state.custom_input().is_empty() {
            lines.push(description_line(state.custom_input(), theme));
        }
    }
    lines
}

fn option_line(
    index: usize,
    label: &str,
    picked: bool,
    active: bool,
    question: &Value,
    theme: &ResolvedTheme,
) -> Line {
    let prefix = if multiple(question) {
        format!("{}.[{}] ", index + 1, if picked { "✓" } else { " " })
    } else {
        format!("{}. ", index + 1)
    };
    let color = if active {
        theme.secondary
    } else if picked {
        theme.success
    } else {
        theme.text
    };
    let bg = active.then_some(theme.background_element);
    Line(vec![
        bar(theme.accent),
        pad(2),
        Span::styled(prefix, Some(theme.text_muted), bg, Attrs::default()),
        Span::styled(label.to_owned(), Some(color), bg, Attrs::default()),
        Span::styled(
            if !multiple(question) && picked {
                " ✓"
            } else {
                ""
            },
            Some(theme.success),
            bg,
            Attrs::default(),
        ),
    ])
}

fn review_lines(request: &Value, state: &QuestionState, theme: &ResolvedTheme) -> Vec<Line> {
    let mut lines = vec![Line(vec![
        bar(theme.accent),
        pad(2),
        Span::styled("Review", Some(theme.text), None, Attrs::default()),
    ])];
    for (index, question) in questions(request).iter().enumerate() {
        let answer = state.answer_at(index).map(|answers| answers.join(", "));
        lines.push(Line(vec![
            bar(theme.accent),
            pad(2),
            Span::styled(
                format!("{}: ", text_field(question, "header")),
                Some(theme.text_muted),
                None,
                Attrs::default(),
            ),
            Span::styled(
                answer.as_deref().unwrap_or("(not answered)").to_owned(),
                Some(if answer.is_some() {
                    theme.text
                } else {
                    theme.error
                }),
                None,
                Attrs::default(),
            ),
        ]));
    }
    lines
}

fn tab_line(request: &Value, state: &QuestionState, theme: &ResolvedTheme) -> Line {
    let mut spans = vec![bar(theme.accent), pad(2)];
    for (index, question) in questions(request).iter().enumerate() {
        spans.push(tab_span(
            text_field(question, "header"),
            state.tab() == index,
            state.answer_at(index).is_some(),
            theme,
        ));
        spans.push(pad(1));
    }
    spans.push(tab_span(
        "Confirm".to_owned(),
        state.confirm(request),
        false,
        theme,
    ));
    Line(spans)
}

fn tab_span(label: String, active: bool, answered: bool, theme: &ResolvedTheme) -> Span {
    let fg = if active {
        selected_foreground(theme, Some(theme.accent))
    } else if answered {
        theme.text
    } else {
        theme.text_muted
    };
    Span::styled(
        format!(" {label} "),
        Some(fg),
        active.then_some(theme.accent),
        Attrs::default(),
    )
}

fn description_line(text: &str, theme: &ResolvedTheme) -> Line {
    Line(vec![
        bar(theme.accent),
        pad(5),
        Span::styled(
            text.to_owned(),
            Some(theme.text_muted),
            None,
            Attrs::default(),
        ),
    ])
}

fn bar(color: Rgba) -> Span {
    Span::styled("┃", Some(color), None, Attrs::default())
}

fn pad(n: usize) -> Span {
    Span::styled(" ".repeat(n), None, None, Attrs::default())
}
