use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;
use crate::{DialogItem, DialogView, Picker};

pub fn render_dialog(frame: &mut Frame, dialog: &DialogView, theme: &Theme) {
    let area = frame.area();
    let width = area.width.saturating_sub(8).clamp(24, 76);
    let item_rows = u16::try_from(dialog.items.len())
        .unwrap_or(u16::MAX)
        .min(10);
    let category_rows = dialog_category_rows(dialog.items.iter().take(usize::from(item_rows)));
    let height = item_rows
        .saturating_add(category_rows)
        .saturating_add(6)
        .min(area.height)
        .max(8);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    clear_overlay_band(frame, area, rect);

    let inner_width = usize::from(width).saturating_sub(6);
    let mut lines = vec![
        Line::from(Span::styled(
            dialog.title.clone(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            dialog.subtitle.clone(),
            Style::default().fg(theme.muted),
        )),
        Line::from(""),
    ];
    let mut last_category: Option<&str> = None;
    for (idx, item) in dialog.items.iter().enumerate().take(usize::from(item_rows)) {
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
        let selected = idx == dialog.selected;
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
        "Agent" | "Context" | "Custom" | "MCP" | "Permissions" | "Session" | "Suggested" | "System"
    )
}

pub fn render_picker(frame: &mut Frame, picker: &Picker, theme: &Theme) {
    let area = frame.area();
    let item_rows = u16::try_from(picker.entries.len())
        .unwrap_or(u16::MAX)
        .min(10);
    let height = item_rows.saturating_add(5).min(area.height).max(6);
    let width = area.width.saturating_sub(8).clamp(24, 76);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    clear_overlay_band(frame, area, rect);
    let inner_width = usize::from(width).saturating_sub(6);
    let mut lines = vec![
        Line::from(Span::styled(
            picker.title.clone(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "↑↓ select   enter confirm   esc dismiss",
            Style::default().fg(theme.muted),
        )),
    ];
    lines.push(Line::from(""));
    for (idx, label) in picker
        .entries
        .iter()
        .enumerate()
        .take(usize::from(item_rows))
    {
        let selected = idx == picker.selected;
        let marker = if selected { "> " } else { "  " };
        let style = if selected {
            Style::default()
                .fg(theme.background)
                .bg(theme.primary)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        lines.push(Line::from(vec![
            Span::styled(marker.to_string(), Style::default().fg(theme.primary)),
            Span::styled(ellipsize(label, inner_width), style),
        ]));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme.text).bg(theme.element))
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn clear_overlay_band(frame: &mut Frame, area: Rect, rect: Rect) {
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

pub(super) fn ellipsize(s: &str, max: usize) -> String {
    let cleaned = s.replace('\n', " ");
    if UnicodeWidthStr::width(cleaned.as_str()) <= max {
        return cleaned;
    }
    if max == 0 {
        return String::new();
    }

    let limit = max.saturating_sub(1);
    let mut width = 0;
    let mut head = String::new();
    for ch in cleaned.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > limit {
            break;
        }
        head.push(ch);
        width += ch_width;
    }
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::ellipsize;

    #[test]
    fn ellipsize_limits_display_width_for_cjk_text() {
        // Given: a wide-character permission/detail label and a narrow terminal budget.
        let text = "权限请求需要读取文件";

        // When: the overlay ellipsizes it for a seven-cell slot.
        let rendered = ellipsize(text, 7);

        // Then: the visible result fits the cell budget and still marks truncation.
        assert!(
            UnicodeWidthStr::width(rendered.as_str()) <= 7,
            "ellipsized CJK text should fit the display-cell budget: {rendered}"
        );
        assert!(
            rendered.ends_with('…'),
            "truncated CJK text should keep an ellipsis marker: {rendered}"
        );
    }
}
