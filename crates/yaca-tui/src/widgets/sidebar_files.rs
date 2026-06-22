use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::sidebar_context::push_section;
use crate::AppState;
use crate::theme::Theme;

const FILE_ROW_WIDTH: usize = 34;
const EXPANDABLE_FILE_COUNT: usize = 2;

pub(super) fn push_files(lines: &mut Vec<Line<'static>>, state: &AppState, theme: &Theme) {
    if state.changed_files.is_empty() {
        return;
    }
    lines.push(Line::from(""));
    let title = if state.changed_files.len() > EXPANDABLE_FILE_COUNT {
        "▼ Modified Files"
    } else {
        "Modified Files"
    };
    push_section(lines, title, theme.info, theme);
    for file in &state.changed_files {
        lines.push(file_line(file, theme));
    }
}

fn file_line(file: &crate::ChangedFileView, theme: &Theme) -> Line<'static> {
    let path_width = FILE_ROW_WIDTH.saturating_sub(stats_width(file));
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(
            ellipsize_tail(&file.path, path_width),
            Style::default().fg(theme.muted),
        ),
    ];
    if let Some(additions) = additions(file) {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("+{additions}"),
            Style::default().fg(theme.success),
        ));
    }
    if let Some(deletions) = deletions(file) {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("-{deletions}"),
            Style::default().fg(theme.error),
        ));
    }
    Line::from(spans)
}

fn stats_width(file: &crate::ChangedFileView) -> usize {
    let additions = additions(file)
        .map(|count| 1 + UnicodeWidthStr::width(format!("+{count}").as_str()))
        .unwrap_or_default();
    let deletions = deletions(file)
        .map(|count| 1 + UnicodeWidthStr::width(format!("-{count}").as_str()))
        .unwrap_or_default();
    additions + deletions
}

fn additions(file: &crate::ChangedFileView) -> Option<u32> {
    file.additions.filter(|&count| count > 0)
}

fn deletions(file: &crate::ChangedFileView) -> Option<u32> {
    file.deletions.filter(|&count| count > 0)
}

fn ellipsize_tail(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let tail_width = max_width - 1;
    let mut tail = Vec::new();
    let mut width = 0usize;
    for ch in text.chars().rev() {
        let char_width = ch.width().unwrap_or(0);
        if width + char_width > tail_width {
            break;
        }
        width += char_width;
        tail.push(ch);
    }
    tail.reverse();
    format!("…{}", tail.into_iter().collect::<String>())
}
