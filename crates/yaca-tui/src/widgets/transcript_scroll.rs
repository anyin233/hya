use std::ops::Range;

use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RowRange {
    pub(super) start: u16,
    pub(super) end: u16,
}

pub(super) fn wrapped_rows(lines: &[Line<'_>], width: u16) -> u16 {
    paragraph_line_count(lines, width)
}

pub(super) fn wrapped_row_range(lines: &[Line<'_>], range: Range<usize>, width: u16) -> RowRange {
    let start = range.start.min(lines.len());
    let end = range.end.min(lines.len());
    RowRange {
        start: wrapped_rows(&lines[..start], width),
        end: wrapped_rows(&lines[..end], width),
    }
}

pub(super) fn top_with_selection_visible(
    current_top: u16,
    viewport_height: u16,
    max_top: u16,
    selected: Option<RowRange>,
) -> u16 {
    let Some(selected) = selected else {
        return current_top.min(max_top);
    };
    let viewport_end = current_top.saturating_add(viewport_height.max(1));
    let selected_height = selected.end.saturating_sub(selected.start);
    let top = if selected_height > viewport_height.max(1) || selected.start < current_top {
        selected.start
    } else if selected.end > viewport_end {
        selected.end.saturating_sub(viewport_height.max(1))
    } else {
        current_top
    };
    top.min(max_top)
}

fn paragraph_line_count(lines: &[Line<'_>], width: u16) -> u16 {
    let paragraph = Paragraph::new(lines.to_vec()).wrap(Wrap { trim: false });
    u16::try_from(paragraph.line_count(width.max(1)))
        .unwrap_or(u16::MAX)
        .max(1)
}

#[cfg(test)]
mod tests {
    use ratatui::text::Line;

    use super::*;

    #[test]
    fn wrapped_row_range_counts_rows_before_and_inside_range() {
        // Given
        let lines = vec![Line::from("abcd"), Line::from("efghij"), Line::from("k")];

        // When
        let range = wrapped_row_range(&lines, 1..3, 3);

        // Then
        assert_eq!(range, RowRange { start: 2, end: 5 });
    }

    #[test]
    fn top_with_selection_visible_moves_above_or_below_viewport_only_when_needed() {
        assert_eq!(
            top_with_selection_visible(10, 5, 20, Some(RowRange { start: 2, end: 4 })),
            2
        );
        assert_eq!(
            top_with_selection_visible(0, 5, 20, Some(RowRange { start: 9, end: 12 })),
            7
        );
        assert_eq!(
            top_with_selection_visible(5, 5, 20, Some(RowRange { start: 6, end: 8 })),
            5
        );
    }

    #[test]
    fn wrapped_rows_match_paragraph_word_wrapping() {
        // Given
        let lines = vec![Line::from("AAAAAA AAAAAA AAAAAA")];

        // When
        let rows = wrapped_rows(&lines, 10);

        // Then
        assert_eq!(rows, 3);
    }

    #[test]
    fn tall_selected_blocks_scroll_to_their_start() {
        assert_eq!(
            top_with_selection_visible(0, 5, 20, Some(RowRange { start: 9, end: 18 })),
            9
        );
    }
}
