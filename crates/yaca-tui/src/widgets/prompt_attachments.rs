use std::path::Path;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::AppState;
use crate::theme::Theme;

pub(super) fn attachment_badges(state: &AppState, theme: &Theme, width: u16) -> Line<'static> {
    let labels = state
        .attachments
        .iter()
        .map(|attachment| {
            let detail = attachment
                .source_path
                .as_deref()
                .and_then(|path| Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .unwrap_or(attachment.mime.as_str());
            format!("{} {}", attachment.placeholder, detail)
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let available = usize::from(width).saturating_sub(2);
    Line::from(vec![
        Span::styled("  ", Style::default().bg(theme.element)),
        Span::styled(
            ellipsize_width(&labels, available),
            Style::default().fg(theme.muted).bg(theme.element),
        ),
    ])
}

fn ellipsize_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut width = 0usize;
    let content_width = max_width.saturating_sub(1);
    for ch in text.chars() {
        let char_width = ch.width().unwrap_or(0);
        if width + char_width > content_width {
            break;
        }
        output.push(ch);
        width += char_width;
    }
    output.push('…');
    output
}
