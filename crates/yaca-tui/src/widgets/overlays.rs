use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use std::ops::Range;

use crate::theme::Theme;
use crate::{DialogItem, DialogView};

const MAX_VISIBLE_ITEMS: usize = 10;

mod picker;
mod text;

pub use picker::render_picker;
pub(super) use text::ellipsize;

pub fn render_dialog(frame: &mut Frame, area: Rect, dialog: &DialogView, theme: &Theme) {
    let width = area.width.saturating_sub(8).clamp(24, 76);
    let visible_range = dialog_visible_range(dialog, area.height);
    let visible_start = visible_range.start;
    let category_rows = dialog_category_rows(dialog.items[visible_range.clone()].iter());
    let subtitle_rows = if dialog_subtitle(dialog).is_some() {
        1
    } else {
        0
    };
    let item_rows = u16::try_from(visible_range.len()).unwrap_or(u16::MAX);
    let empty_rows = if dialog.items.is_empty() { 2 } else { 0 };
    let content_height = item_rows
        .saturating_add(category_rows)
        .saturating_add(empty_rows)
        .saturating_add(subtitle_rows)
        .saturating_add(4);
    let height = content_height.min(area.height).max(1);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    clear_overlay_band(frame, area, rect);

    let inner_width = usize::from(width).saturating_sub(6);
    let mut lines = vec![Line::from(Span::styled(
        dialog.title.clone(),
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
    ))];
    if let Some(subtitle) = dialog_subtitle(dialog) {
        lines.push(Line::from(Span::styled(
            subtitle.to_string(),
            Style::default().fg(theme.muted),
        )));
    }
    lines.push(Line::from(""));
    if dialog.items.is_empty() {
        push_empty_dialog_lines(&mut lines, dialog, theme);
    } else {
        push_dialog_item_lines(
            &mut lines,
            dialog,
            visible_range,
            visible_start,
            inner_width,
            theme,
        );
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑↓/tab select   enter confirm   esc dismiss",
        Style::default().fg(theme.muted),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme.text).bg(theme.element))
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn push_empty_dialog_lines(lines: &mut Vec<Line<'static>>, dialog: &DialogView, theme: &Theme) {
    let (label, detail) = dialog_empty_state(dialog);
    lines.push(Line::from(Span::styled(
        label,
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        detail,
        Style::default().fg(theme.muted),
    )));
}

fn dialog_empty_state(dialog: &DialogView) -> (&'static str, &'static str) {
    if dialog.title == "Skills" {
        (
            "No skills found",
            "Add SKILL.md under .yaca/skills or ~/.config/yaca/skills",
        )
    } else {
        ("No items found", "Try a different query or command")
    }
}

fn dialog_subtitle(dialog: &DialogView) -> Option<&str> {
    if matches!(
        (dialog.title.as_str(), dialog.subtitle.as_str()),
        ("commands", "select a slash command") | ("references", "select a file or reference")
    ) {
        None
    } else {
        Some(dialog.subtitle.as_str())
    }
}

fn push_dialog_item_lines(
    lines: &mut Vec<Line<'static>>,
    dialog: &DialogView,
    visible_range: Range<usize>,
    visible_start: usize,
    inner_width: usize,
    theme: &Theme,
) {
    let mut last_category: Option<&str> = None;
    for (idx, item) in dialog.items[visible_range].iter().enumerate() {
        let (category, detail_text) = split_category_detail(&item.detail);
        if category.is_some() && category != last_category {
            lines.push(Line::from(Span::styled(
                format!("  {}", category.unwrap_or_default()),
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        last_category = category;
        push_dialog_item_line(
            lines,
            item,
            detail_text,
            idx + visible_start == dialog.selected,
            inner_width,
            theme,
        );
    }
}

fn push_dialog_item_line(
    lines: &mut Vec<Line<'static>>,
    item: &DialogItem,
    detail_text: &str,
    selected: bool,
    inner_width: usize,
    theme: &Theme,
) {
    let marker = if selected { "> " } else { "  " };
    let style = if selected {
        Style::default()
            .fg(theme.background)
            .bg(theme.primary)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text)
    };
    let detail = if detail_text.is_empty() {
        String::new()
    } else {
        format!("  {}", ellipsize(detail_text, inner_width / 2))
    };
    lines.push(Line::from(vec![
        Span::styled(marker.to_string(), Style::default().fg(theme.primary)),
        Span::styled(item.label.clone(), style),
        Span::styled(detail, Style::default().fg(theme.muted)),
    ]));
}

fn dialog_visible_range(dialog: &DialogView, available_height: u16) -> Range<usize> {
    let mut item_limit = dialog.items.len().min(MAX_VISIBLE_ITEMS);
    loop {
        let range = visible_item_range(dialog.items.len(), dialog.selected, item_limit);
        let category_rows = dialog_category_rows(dialog.items[range.clone()].iter());
        let subtitle_rows = if dialog_subtitle(dialog).is_some() {
            1
        } else {
            0
        };
        let height = u16::try_from(range.len())
            .unwrap_or(u16::MAX)
            .saturating_add(category_rows)
            .saturating_add(subtitle_rows)
            .saturating_add(4);
        if height <= available_height || item_limit == 0 {
            return range;
        }
        item_limit -= 1;
    }
}

fn visible_item_range(len: usize, selected: usize, limit: usize) -> Range<usize> {
    if len == 0 || limit == 0 {
        return 0..0;
    }
    let count = len.min(limit);
    let selected = selected.min(len - 1);
    let start = selected.saturating_add(1).saturating_sub(count);
    let start = start.min(len - count);
    start..start + count
}

fn dialog_category_rows<'a>(items: impl Iterator<Item = &'a DialogItem>) -> u16 {
    let mut rows = 0_u16;
    let mut last_category: Option<&str> = None;
    for item in items {
        let (category, _) = split_category_detail(&item.detail);
        if category.is_some() && category != last_category {
            rows = rows.saturating_add(1);
        }
        last_category = category;
    }
    rows
}

fn split_category_detail(detail: &str) -> (Option<&str>, &str) {
    let Some((category, rest)) = detail.split_once(" · ") else {
        return (None, detail);
    };
    if is_dialog_category(category) {
        (Some(category), rest)
    } else {
        (None, detail)
    }
}

fn is_dialog_category(label: &str) -> bool {
    matches!(
        label,
        "Agent"
            | "Context"
            | "Custom"
            | "MCP"
            | "Permissions"
            | "Prompt"
            | "Session"
            | "Skills"
            | "Suggested"
            | "System"
    )
}

pub(super) fn clear_overlay_band(frame: &mut Frame, area: Rect, rect: Rect) {
    frame.render_widget(
        Clear,
        Rect {
            x: area.x,
            y: rect.y,
            width: area.width,
            height: rect.height,
        },
    );
}
