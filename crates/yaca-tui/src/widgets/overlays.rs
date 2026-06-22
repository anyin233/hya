use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;
use crate::{DialogView, Picker};

pub fn render_dialog(frame: &mut Frame, dialog: &DialogView, theme: &Theme) {
    let area = frame.area();
    let width = area.width.saturating_sub(8).clamp(24, 76);
    let item_rows = u16::try_from(dialog.items.len())
        .unwrap_or(u16::MAX)
        .min(10);
    let height = item_rows.saturating_add(6).min(area.height).max(8);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);

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
    for (idx, item) in dialog.items.iter().enumerate().take(usize::from(item_rows)) {
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
        let detail = if item.detail.is_empty() {
            String::new()
        } else {
            format!("  {}", ellipsize(&item.detail, inner_width / 2))
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
    frame.render_widget(Clear, rect);
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
