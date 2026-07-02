pub use hya_tui_lib::render::draw::{layout_rect_to_ratatui, rect_to_ratatui, rgba_to_color};
use ratatui::style::{Modifier, Style};
use ratatui::text as rt;

use crate::contracts::Rgba;

use super::text::{Attrs, Line, Span, Text};

#[must_use]
pub fn style(fg: Option<Rgba>, bg: Option<Rgba>, attrs: Attrs, background: Rgba) -> Style {
    let mut style = Style::default();
    if let Some(fg) = fg {
        style = style.fg(rgba_to_color(fg, background));
    }
    if let Some(bg) = bg {
        style = style.bg(rgba_to_color(bg, background));
    }
    style.add_modifier(modifiers(attrs))
}

#[must_use]
pub fn text_to_ratatui(text: &Text, background: Rgba) -> rt::Text<'static> {
    rt::Text::from(
        text.0
            .iter()
            .map(|line| line_to_ratatui(line, background))
            .collect::<Vec<_>>(),
    )
}

fn line_to_ratatui(line: &Line, background: Rgba) -> rt::Line<'static> {
    rt::Line::from(
        line.0
            .iter()
            .map(|span| span_to_ratatui(span, background))
            .collect::<Vec<_>>(),
    )
}

fn span_to_ratatui(span: &Span, background: Rgba) -> rt::Span<'static> {
    rt::Span::styled(
        span.text.clone(),
        style(span.fg, span.bg, span.attrs, background),
    )
}

const fn modifiers(attrs: Attrs) -> Modifier {
    let mut out = Modifier::empty();
    if attrs.bold {
        out = out.union(Modifier::BOLD);
    }
    if attrs.italic {
        out = out.union(Modifier::ITALIC);
    }
    if attrs.underline {
        out = out.union(Modifier::UNDERLINED);
    }
    if attrs.dim {
        out = out.union(Modifier::DIM);
    }
    if attrs.strikethrough {
        out = out.union(Modifier::CROSSED_OUT);
    }
    out
}
