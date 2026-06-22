use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

pub(super) fn push_reasoning_lines(
    text: &str,
    selected: bool,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    lines.push(Line::from(vec![
        Span::styled("   ", reasoning_style(theme.muted, selected, theme)),
        Span::styled("Thinking", reasoning_style(theme.warning, selected, theme)),
    ]));
    for segment in text.trim().split('\n') {
        lines.push(Line::from(vec![
            Span::styled("   ", reasoning_style(theme.muted, selected, theme)),
            Span::styled("▏ ", reasoning_style(theme.warning, selected, theme)),
            Span::styled(
                segment.to_string(),
                reasoning_style(theme.muted, selected, theme),
            ),
        ]));
    }
}

fn reasoning_style(fg: Color, selected: bool, theme: &Theme) -> Style {
    let bg = if selected {
        theme.block
    } else {
        theme.background
    };
    Style::default().fg(fg).bg(bg)
}
