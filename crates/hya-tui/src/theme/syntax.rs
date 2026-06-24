use crate::contracts::Rgba;

use super::ResolvedTheme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxStyle {
    pub foreground: Option<Rgba>,
    pub background: Option<Rgba>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxRule {
    pub scope: &'static [&'static str],
    pub style: SyntaxStyle,
}

#[must_use]
pub fn get_syntax_rules(theme: &ResolvedTheme) -> Vec<SyntaxRule> {
    vec![
        rule(&["default"], fg(theme.text)),
        rule(&["prompt"], fg(theme.accent)),
        rule(
            &["comment", "comment.documentation"],
            fg_italic(theme.syntax_comment),
        ),
        rule(&["string", "symbol"], fg(theme.syntax_string)),
        rule(
            &["number", "boolean", "constant", "float"],
            fg(theme.syntax_number),
        ),
        rule(&["keyword"], fg_italic(theme.syntax_keyword)),
        rule(&["keyword.type"], fg_bold_italic(theme.syntax_type)),
        rule(
            &["keyword.function", "function.method"],
            fg(theme.syntax_function),
        ),
        rule(
            &["operator", "keyword.operator", "punctuation.delimiter"],
            fg(theme.syntax_operator),
        ),
        rule(
            &["variable", "variable.parameter", "function.call"],
            fg(theme.syntax_variable),
        ),
        rule(&["type", "module", "class"], fg(theme.syntax_type)),
        rule(
            &["punctuation", "punctuation.bracket"],
            fg(theme.syntax_punctuation),
        ),
        rule(&["markup.heading"], fg_bold(theme.markdown_heading)),
        rule(
            &["markup.bold", "markup.strong"],
            fg_bold(theme.markdown_strong),
        ),
        rule(
            &["markup.italic", "markup.quote"],
            fg_italic(theme.markdown_emph),
        ),
        rule(&["markup.raw", "markup.raw.block"], fg(theme.markdown_code)),
        rule(
            &["markup.link", "markup.link.url"],
            fg_underline(theme.markdown_link),
        ),
        rule(&["diff.plus"], fg_bg(theme.diff_added, theme.diff_added_bg)),
        rule(
            &["diff.minus"],
            fg_bg(theme.diff_removed, theme.diff_removed_bg),
        ),
        rule(
            &["diff.delta"],
            fg_bg(theme.diff_context, theme.diff_context_bg),
        ),
        rule(&["error"], fg_bold(theme.error)),
        rule(&["warning"], fg_bold(theme.warning)),
        rule(&["info"], fg(theme.info)),
    ]
}

fn rule(scope: &'static [&'static str], style: SyntaxStyle) -> SyntaxRule {
    SyntaxRule { scope, style }
}

fn fg(foreground: Rgba) -> SyntaxStyle {
    SyntaxStyle {
        foreground: Some(foreground),
        background: None,
        bold: false,
        italic: false,
        underline: false,
    }
}

fn fg_bg(foreground: Rgba, background: Rgba) -> SyntaxStyle {
    SyntaxStyle {
        background: Some(background),
        ..fg(foreground)
    }
}

fn fg_bold(foreground: Rgba) -> SyntaxStyle {
    SyntaxStyle {
        bold: true,
        ..fg(foreground)
    }
}

fn fg_italic(foreground: Rgba) -> SyntaxStyle {
    SyntaxStyle {
        italic: true,
        ..fg(foreground)
    }
}

fn fg_bold_italic(foreground: Rgba) -> SyntaxStyle {
    SyntaxStyle {
        bold: true,
        italic: true,
        ..fg(foreground)
    }
}

fn fg_underline(foreground: Rgba) -> SyntaxStyle {
    SyntaxStyle {
        underline: true,
        ..fg(foreground)
    }
}
