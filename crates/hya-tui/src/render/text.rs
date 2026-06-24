use crate::contracts::Rgba;
use crate::prompt::display::prompt_offset_width;

use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Attrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub strikethrough: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub attrs: Attrs,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line(pub Vec<Span>);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text(pub Vec<Line>);

impl Span {
    #[must_use]
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            fg: None,
            bg: None,
            attrs: Attrs::default(),
        }
    }

    #[must_use]
    pub fn styled(
        text: impl Into<String>,
        fg: Option<Rgba>,
        bg: Option<Rgba>,
        attrs: Attrs,
    ) -> Self {
        Self {
            text: text.into(),
            fg,
            bg,
            attrs,
        }
    }

    #[must_use]
    pub fn width(&self) -> usize {
        prompt_offset_width(&self.text)
    }
}

impl Line {
    #[must_use]
    pub fn width(&self) -> usize {
        self.0.iter().map(Span::width).sum()
    }

    #[must_use]
    pub fn wrap(&self, target_width: usize) -> Vec<Self> {
        wrap_tokens(tokenize(self), target_width.max(1))
    }
}

impl Text {
    #[must_use]
    pub fn wrap(&self, target_width: usize) -> Self {
        Self(
            self.0
                .iter()
                .flat_map(|line| line.wrap(target_width))
                .collect(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Word,
    Space,
    Newline,
}

#[derive(Debug, Clone)]
struct Token {
    spans: Vec<Span>,
    width: usize,
    kind: TokenKind,
}

fn tokenize(line: &Line) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut current: Option<Token> = None;
    for span in &line.0 {
        for grapheme in span.text.graphemes(true) {
            let kind = token_kind(grapheme);
            if matches!(&current, Some(token) if token.kind != kind) {
                if let Some(token) = current.take() {
                    tokens.push(token);
                }
            }
            append_grapheme(&mut current, span, grapheme, kind);
            if matches!(kind, TokenKind::Newline) {
                if let Some(token) = current.take() {
                    tokens.push(token);
                }
            }
        }
    }
    if let Some(token) = current {
        tokens.push(token);
    }
    tokens
}

fn token_kind(grapheme: &str) -> TokenKind {
    if grapheme == "\n" {
        return TokenKind::Newline;
    }
    if grapheme.chars().all(char::is_whitespace) {
        return TokenKind::Space;
    }
    TokenKind::Word
}

fn append_grapheme(current: &mut Option<Token>, span: &Span, grapheme: &str, kind: TokenKind) {
    let fragment = Span::styled(grapheme, span.fg, span.bg, span.attrs);
    let token = current.get_or_insert_with(|| Token {
        spans: Vec::new(),
        width: 0,
        kind,
    });
    token.width += prompt_offset_width(grapheme);
    append_span(&mut token.spans, fragment);
}

fn wrap_tokens(tokens: Vec<Token>, target_width: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut current = Vec::new();
    let mut current_width = 0usize;
    let mut trailing_newline = false;
    let mut pending_space = Token {
        spans: Vec::new(),
        width: 0,
        kind: TokenKind::Space,
    };
    for token in tokens {
        match token.kind {
            TokenKind::Newline => {
                push_line(&mut lines, &mut current, &mut current_width);
                trailing_newline = true;
            }
            TokenKind::Space if current_width > 0 => {
                merge_token(&mut pending_space, token);
                trailing_newline = false;
            }
            TokenKind::Space => trailing_newline = false,
            TokenKind::Word => {
                append_word(
                    &mut lines,
                    &mut current,
                    &mut current_width,
                    &mut pending_space,
                    token,
                    target_width,
                );
                trailing_newline = false;
            }
        }
    }
    if trailing_newline || !current.is_empty() || lines.is_empty() {
        lines.push(Line(current));
    }
    lines
}

fn append_word(
    lines: &mut Vec<Line>,
    current: &mut Vec<Span>,
    current_width: &mut usize,
    pending_space: &mut Token,
    token: Token,
    target_width: usize,
) {
    if token.width > target_width {
        push_line_if_needed(lines, current, current_width);
        pending_space.spans.clear();
        pending_space.width = 0;
        append_broken_word(lines, current, current_width, token, target_width);
        return;
    }
    if *current_width > 0 && *current_width + pending_space.width + token.width > target_width {
        push_line(lines, current, current_width);
    } else {
        append_token(current, pending_space);
        *current_width += pending_space.width;
    }
    append_token(current, &token);
    *current_width += token.width;
    pending_space.spans.clear();
    pending_space.width = 0;
}

fn append_broken_word(
    lines: &mut Vec<Line>,
    current: &mut Vec<Span>,
    current_width: &mut usize,
    token: Token,
    target_width: usize,
) {
    for span in token.spans {
        for grapheme in span.text.graphemes(true) {
            let width = prompt_offset_width(grapheme);
            if *current_width > 0 && *current_width + width > target_width {
                push_line(lines, current, current_width);
            }
            append_span(
                current,
                Span::styled(grapheme, span.fg, span.bg, span.attrs),
            );
            *current_width += width;
        }
    }
}

fn append_token(spans: &mut Vec<Span>, token: &Token) {
    for span in &token.spans {
        append_span(spans, span.clone());
    }
}

fn merge_token(target: &mut Token, token: Token) {
    target.width += token.width;
    for span in token.spans {
        append_span(&mut target.spans, span);
    }
}

fn push_line(lines: &mut Vec<Line>, current: &mut Vec<Span>, current_width: &mut usize) {
    lines.push(Line(std::mem::take(current)));
    *current_width = 0;
}

fn push_line_if_needed(lines: &mut Vec<Line>, current: &mut Vec<Span>, current_width: &mut usize) {
    if !current.is_empty() {
        push_line(lines, current, current_width);
    }
}

pub(crate) fn append_span(spans: &mut Vec<Span>, span: Span) {
    if span.text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.fg == span.fg && last.bg == span.bg && last.attrs == span.attrs {
            last.text.push_str(&span.text);
            return;
        }
    }
    spans.push(span);
}
