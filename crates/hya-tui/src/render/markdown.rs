use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::contracts::Rgba;
use crate::theme::ResolvedTheme;

use super::markdown_highlight::highlight_code;
use super::text::{append_span, Attrs, Line, Span, Text};

#[must_use]
pub fn parse(markdown: &str, theme: &ResolvedTheme) -> Text {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let mut renderer = Renderer::new(theme);
    for event in Parser::new_ext(markdown, options) {
        renderer.event(event);
    }
    renderer.finish()
}

#[derive(Debug, Clone, Copy)]
struct TextStyle {
    fg: Rgba,
    bg: Option<Rgba>,
    attrs: Attrs,
}

#[derive(Debug, Clone, Copy)]
struct ListState {
    next: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
enum AttrDelta {
    Plain,
    Bold,
    Italic,
    Underline,
    Strikethrough,
}

struct Renderer<'a> {
    theme: &'a ResolvedTheme,
    lines: Vec<Line>,
    current: Vec<Span>,
    styles: Vec<TextStyle>,
    lists: Vec<ListState>,
    code_language: Option<String>,
}

impl<'a> Renderer<'a> {
    fn new(theme: &'a ResolvedTheme) -> Self {
        Self {
            theme,
            lines: Vec::new(),
            current: Vec::new(),
            styles: vec![TextStyle {
                fg: theme.markdown_text,
                bg: None,
                attrs: Attrs::default(),
            }],
            lists: Vec::new(),
            code_language: None,
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => self.inline_code(&code),
            Event::SoftBreak | Event::HardBreak => self.finish_line(),
            Event::Rule => {
                self.push_text(
                    "---",
                    self.theme.markdown_horizontal_rule,
                    None,
                    Attrs::default(),
                );
                self.finish_line();
            }
            Event::Html(html) | Event::InlineHtml(html) => self.text(&html),
            Event::TaskListMarker(checked) => self.text(if checked { "[x] " } else { "[ ] " }),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { .. } => {
                self.push_style(self.theme.markdown_heading, None, AttrDelta::Bold)
            }
            Tag::Emphasis => self.push_style(self.theme.markdown_emph, None, AttrDelta::Italic),
            Tag::Strong => self.push_style(self.theme.markdown_strong, None, AttrDelta::Bold),
            Tag::Strikethrough => self.push_style(
                self.current_style().fg,
                self.current_style().bg,
                AttrDelta::Strikethrough,
            ),
            Tag::Link { .. } => {
                self.push_style(self.theme.markdown_link_text, None, AttrDelta::Underline)
            }
            Tag::BlockQuote(_) => {
                self.push_style(self.theme.markdown_block_quote, None, AttrDelta::Italic);
                self.push_text(
                    "> ",
                    self.theme.markdown_block_quote,
                    None,
                    Attrs::default(),
                );
            }
            Tag::List(next) => self.lists.push(ListState { next }),
            Tag::Item => self.item_prefix(),
            Tag::CodeBlock(kind) => self.code_language = Some(language_from_code_block(kind)),
            Tag::Image { .. } => {
                self.push_style(self.theme.markdown_image_text, None, AttrDelta::Plain)
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.pop_style();
                self.finish_block();
            }
            TagEnd::Paragraph | TagEnd::Item => self.finish_block(),
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Link
            | TagEnd::Image => {
                self.pop_style();
            }
            TagEnd::CodeBlock => {
                self.code_language = None;
                self.finish_block();
            }
            TagEnd::List(_) => {
                self.lists.pop();
                self.finish_block();
            }
            TagEnd::BlockQuote(_) => {
                self.pop_style();
                self.finish_block();
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if let Some(language) = self.code_language.clone() {
            self.code_text(&language, text);
            return;
        }
        let style = self.current_style();
        for (line_index, part) in text.split('\n').enumerate() {
            if line_index > 0 {
                self.finish_line();
            }
            self.push_text(part, style.fg, style.bg, style.attrs);
        }
    }

    fn inline_code(&mut self, code: &str) {
        self.push_text(
            code,
            self.theme.markdown_code,
            Some(self.theme.markdown_code_block),
            Attrs::default(),
        );
    }

    fn code_text(&mut self, language: &str, text: &str) {
        for line in highlight_code(language, text, self.theme) {
            if !self.current.is_empty() {
                self.finish_line();
            }
            self.current = line.0;
            self.finish_line();
        }
    }

    fn item_prefix(&mut self) {
        let (prefix, color) = match self.lists.last_mut().and_then(|list| list.next.as_mut()) {
            Some(next) => {
                let prefix = format!("{next}. ");
                *next += 1;
                (prefix, self.theme.markdown_list_enumeration)
            }
            None => ("- ".to_owned(), self.theme.markdown_list_item),
        };
        self.push_text(&prefix, color, None, Attrs::default());
    }

    fn push_style(&mut self, fg: Rgba, bg: Option<Rgba>, delta: AttrDelta) {
        let mut attrs = self.current_style().attrs;
        match delta {
            AttrDelta::Plain => {}
            AttrDelta::Bold => attrs.bold = true,
            AttrDelta::Italic => attrs.italic = true,
            AttrDelta::Underline => attrs.underline = true,
            AttrDelta::Strikethrough => attrs.strikethrough = true,
        }
        self.styles.push(TextStyle { fg, bg, attrs });
    }

    fn pop_style(&mut self) {
        if self.styles.len() > 1 {
            self.styles.pop();
        }
    }

    fn current_style(&self) -> TextStyle {
        match self.styles.last().copied() {
            Some(style) => style,
            None => TextStyle {
                fg: self.theme.markdown_text,
                bg: None,
                attrs: Attrs::default(),
            },
        }
    }

    fn push_text(&mut self, text: &str, fg: Rgba, bg: Option<Rgba>, attrs: Attrs) {
        append_span(&mut self.current, Span::styled(text, Some(fg), bg, attrs));
    }

    fn finish_line(&mut self) {
        self.lines.push(Line(std::mem::take(&mut self.current)));
    }

    fn finish_block(&mut self) {
        if !self.current.is_empty() {
            self.finish_line();
        }
    }

    fn finish(mut self) -> Text {
        self.finish_block();
        Text(self.lines)
    }
}

fn language_from_code_block(kind: CodeBlockKind<'_>) -> String {
    match kind {
        CodeBlockKind::Fenced(info) => match info.split_whitespace().next() {
            Some(language) => language.to_owned(),
            None => "text".to_owned(),
        },
        CodeBlockKind::Indented => "text".to_owned(),
    }
}
