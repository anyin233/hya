use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use crate::theme::Theme;
use crate::{KeyBindingGroup, KeyBindingItem, KeyBindingsView};

const MAX_GROUPS: usize = 4;
const MAX_ITEMS_PER_GROUP: usize = 4;

pub fn render_keybindings(frame: &mut Frame, area: Rect, view: &KeyBindingsView, theme: &Theme) {
    let width = area.width.saturating_sub(8).clamp(32, 84);
    let height = keybindings_height(view).min(area.height).max(1);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height),
        width,
        height,
    };
    frame.render_widget(
        Clear,
        Rect {
            x: area.x,
            y: rect.y,
            width: area.width,
            height: rect.height,
        },
    );
    frame.render_widget(
        Paragraph::new(keybindings_lines(view, theme))
            .style(Style::default().fg(theme.text).bg(theme.element))
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn keybindings_height(view: &KeyBindingsView) -> u16 {
    let rows = view
        .groups
        .iter()
        .take(MAX_GROUPS)
        .map(|group| 1 + group.items.len().min(MAX_ITEMS_PER_GROUP))
        .sum::<usize>();
    u16::try_from(rows.saturating_add(3)).unwrap_or(u16::MAX)
}

fn keybindings_lines(view: &KeyBindingsView, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        view.title.clone(),
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
    ))];
    for group in view.groups.iter().take(MAX_GROUPS) {
        push_group_lines(&mut lines, group, theme);
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("esc ", Style::default().fg(theme.text)),
        Span::styled("dismiss", Style::default().fg(theme.muted)),
        Span::styled("   ctrl+p ", Style::default().fg(theme.text)),
        Span::styled("commands", Style::default().fg(theme.muted)),
    ]));
    lines
}

fn push_group_lines(lines: &mut Vec<Line<'static>>, group: &KeyBindingGroup, theme: &Theme) {
    lines.push(Line::from(Span::styled(
        group.label.clone(),
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD),
    )));
    lines.extend(
        group
            .items
            .iter()
            .take(MAX_ITEMS_PER_GROUP)
            .map(|item| item_line(item, theme)),
    );
}

fn item_line(item: &KeyBindingItem, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("  ", Style::default().fg(theme.muted)),
        Span::styled(item.label.clone(), Style::default().fg(theme.muted)),
        Span::styled(" ", Style::default().fg(theme.muted)),
        Span::styled(
            item.key.clone(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ])
}
