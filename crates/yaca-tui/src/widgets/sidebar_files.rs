use ratatui::text::Line;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::sidebar_context::{meta, push_section};
use crate::AppState;
use crate::theme::Theme;

const FILE_ROW_WIDTH: usize = 34;
const MAX_VISIBLE_FILES: usize = 6;

pub(super) fn push_files(lines: &mut Vec<Line<'static>>, state: &AppState, theme: &Theme) {
    if state.changed_files.is_empty() {
        return;
    }
    lines.push(Line::from(""));
    push_section(lines, "Files", theme.info, theme);
    for file in state.changed_files.iter().take(MAX_VISIBLE_FILES) {
        lines.push(meta(file_label(file), theme.text));
    }
    let hidden = state.changed_files.len().saturating_sub(MAX_VISIBLE_FILES);
    if hidden > 0 {
        lines.push(meta(format!("+{hidden} more"), theme.muted));
    }
}

fn file_label(file: &crate::ChangedFileView) -> String {
    let stats = match (file.additions, file.deletions) {
        (Some(additions), Some(deletions)) => format!(" +{additions} -{deletions}"),
        (Some(additions), None) => format!(" +{additions}"),
        (None, Some(deletions)) => format!(" -{deletions}"),
        (None, None) => String::new(),
    };
    let path_width = FILE_ROW_WIDTH.saturating_sub(UnicodeWidthStr::width(stats.as_str()));
    format!("{}{}", ellipsize_tail(&file.path, path_width), stats)
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
