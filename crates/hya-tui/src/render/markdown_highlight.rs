use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color, FontStyle, Style, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::SyntaxSet;

use crate::contracts::Rgba;
use crate::theme::ResolvedTheme;

use super::text::{append_span, Attrs, Line, Span};

pub fn highlight_code(language: &str, source: &str, theme: &ResolvedTheme) -> Vec<Line> {
    let syntax_set = SyntaxSet::load_defaults_newlines();
    let syntect_theme = build_theme(theme);
    let syntax = match syntax_set.find_syntax_by_token(language) {
        Some(syntax) => syntax,
        None => syntax_set.find_syntax_plain_text(),
    };
    let mut highlighter = HighlightLines::new(syntax, &syntect_theme);
    let mut lines = Vec::new();
    let mut parts = source.split('\n').peekable();
    while let Some(part) = parts.next() {
        if !part.is_empty() || parts.peek().is_none() {
            lines.push(highlight_line(part, &mut highlighter, &syntax_set, theme));
        }
        if parts.peek().is_some() && part.is_empty() {
            lines.push(Line::default());
        }
    }
    lines
}

fn highlight_line(
    line: &str,
    highlighter: &mut HighlightLines<'_>,
    syntax_set: &SyntaxSet,
    theme: &ResolvedTheme,
) -> Line {
    let mut spans = Vec::new();
    match highlighter.highlight_line(line, syntax_set) {
        Ok(regions) => {
            for (style, text) in regions {
                append_span(
                    &mut spans,
                    Span::styled(
                        text,
                        Some(rgba_from_color(style.foreground)),
                        Some(theme.markdown_code_block),
                        attrs_from_style(style),
                    ),
                );
            }
        }
        Err(_) => append_span(
            &mut spans,
            Span::styled(
                line,
                Some(theme.markdown_code),
                Some(theme.markdown_code_block),
                Attrs::default(),
            ),
        ),
    }
    Line(spans)
}

fn attrs_from_style(style: Style) -> Attrs {
    Attrs {
        bold: style.font_style.contains(FontStyle::BOLD),
        italic: style.font_style.contains(FontStyle::ITALIC),
        underline: style.font_style.contains(FontStyle::UNDERLINE),
        dim: false,
        strikethrough: false,
    }
}

fn build_theme(theme: &ResolvedTheme) -> Theme {
    let mut scopes = Vec::new();
    push_scope(
        &mut scopes,
        "comment",
        theme.syntax_comment,
        FontStyle::ITALIC,
    );
    push_scope(
        &mut scopes,
        "string",
        theme.syntax_string,
        FontStyle::empty(),
    );
    push_scope(
        &mut scopes,
        "constant",
        theme.syntax_number,
        FontStyle::empty(),
    );
    push_scope(
        &mut scopes,
        "keyword",
        theme.syntax_keyword,
        FontStyle::ITALIC,
    );
    push_scope(
        &mut scopes,
        "storage",
        theme.syntax_keyword,
        FontStyle::ITALIC,
    );
    push_scope(
        &mut scopes,
        "entity.name.function",
        theme.syntax_function,
        FontStyle::empty(),
    );
    push_scope(
        &mut scopes,
        "support.function",
        theme.syntax_function,
        FontStyle::empty(),
    );
    push_scope(
        &mut scopes,
        "variable",
        theme.syntax_variable,
        FontStyle::empty(),
    );
    push_scope(
        &mut scopes,
        "support.type",
        theme.syntax_type,
        FontStyle::empty(),
    );
    push_scope(
        &mut scopes,
        "entity.name.type",
        theme.syntax_type,
        FontStyle::empty(),
    );
    push_scope(
        &mut scopes,
        "keyword.operator",
        theme.syntax_operator,
        FontStyle::empty(),
    );
    push_scope(
        &mut scopes,
        "punctuation",
        theme.syntax_punctuation,
        FontStyle::empty(),
    );
    Theme {
        name: Some("hya".to_owned()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(color_from_rgba(theme.markdown_code)),
            background: Some(color_from_rgba(theme.markdown_code_block)),
            ..ThemeSettings::default()
        },
        scopes,
    }
}

fn push_scope(
    scopes: &mut Vec<ThemeItem>,
    selector: &str,
    foreground: Rgba,
    font_style: FontStyle,
) {
    if let Ok(scope) = selector.parse() {
        scopes.push(ThemeItem {
            scope,
            style: StyleModifier {
                foreground: Some(color_from_rgba(foreground)),
                background: None,
                font_style: Some(font_style),
            },
        });
    }
}

const fn color_from_rgba(color: Rgba) -> Color {
    Color {
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    }
}

const fn rgba_from_color(color: Color) -> Rgba {
    Rgba::new(color.r, color.g, color.b, color.a)
}
