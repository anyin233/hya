use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Paragraph};
use serde_json::Value;

use crate::render::draw::{rgba_to_color, text_to_ratatui};
use crate::render::text::{Attrs, Line, Span, Text};
use crate::theme::ResolvedTheme;

use super::model::{multiple, questions, single};
use super::state::QuestionState;

pub(super) fn draw_footer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    request: &Value,
    state: &QuestionState,
    theme: &ResolvedTheme,
) {
    let bg = theme.background;
    let element = rgba_to_color(theme.background_element, bg);
    let footer_area = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(1),
        width: area.width,
        height: 1,
    };
    frame.render_widget(
        Block::default().style(Style::default().bg(element)),
        footer_area,
    );
    let mut spans = Vec::new();
    if !single(request) {
        spans.extend(hint("⇆", "tab  ", theme));
    }
    if !state.confirm(request) && !state.editing() {
        spans.extend(hint("↑↓", "select  ", theme));
    }
    spans.extend(hint(
        "enter",
        &format!("{}  ", enter_hint(request, state)),
        theme,
    ));
    spans.extend(hint("esc", esc_hint(state), theme));
    frame.render_widget(
        Paragraph::new(text_to_ratatui(&Text(vec![Line(spans)]), bg))
            .style(Style::default().bg(element)),
        footer_area,
    );
}

fn enter_hint(request: &Value, state: &QuestionState) -> &'static str {
    if state.confirm(request) {
        "submit"
    } else if state.editing() {
        "save"
    } else if questions(request).get(state.tab()).is_some_and(multiple) {
        "toggle"
    } else if single(request) {
        "submit"
    } else {
        "confirm"
    }
}

fn esc_hint(state: &QuestionState) -> &'static str {
    if state.editing() {
        "cancel"
    } else {
        "dismiss"
    }
}

fn hint(key: &str, value: &str, theme: &ResolvedTheme) -> Vec<Span> {
    vec![
        Span::styled(key, Some(theme.text), None, Attrs::default()),
        Span::styled(
            format!(" {value}"),
            Some(theme.text_muted),
            None,
            Attrs::default(),
        ),
    ]
}
