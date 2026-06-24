use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use crate::render::draw::rgba_to_color;
use crate::theme::ResolvedTheme;
use crate::widgets::dialog_select::DialogSelect;

const DIALOG_WIDTH: u16 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogRow {
    Header(usize),
    Item(usize),
}

pub struct DialogGeometry {
    pub area: Rect,
    pub list_area: Rect,
    pub rows: Vec<DialogRow>,
}

#[must_use]
pub fn geometry(screen: Rect, palette: &DialogSelect<String>) -> DialogGeometry {
    let width = DIALOG_WIDTH.min(screen.width.saturating_sub(2));
    let x = screen.x + screen.width.saturating_sub(width) / 2;
    let top = screen.y + screen.height / 4;
    let items = palette.filtered_items();
    let grouped = palette.filter().trim().is_empty();

    let mut rows: Vec<DialogRow> = Vec::new();
    let mut current: Option<&str> = None;
    for (index, item) in items.iter().enumerate() {
        if grouped && item.category.as_deref() != current {
            current = item.category.as_deref();
            if current.is_some() {
                rows.push(DialogRow::Header(index));
            }
        }
        rows.push(DialogRow::Item(index));
    }

    let available = screen
        .height
        .saturating_sub(top - screen.y)
        .saturating_sub(6);
    let list_height = (rows.len() as u16).clamp(1, available.max(1));
    let height = list_height + 5;
    let area = Rect {
        x,
        y: top,
        width,
        height: height.min(screen.height.saturating_sub(top - screen.y)),
    };
    let list_area = Rect {
        x: area.x + 1,
        y: area.y + 4,
        width: area.width.saturating_sub(2),
        height: list_height,
    };
    DialogGeometry {
        area,
        list_area,
        rows,
    }
}

pub fn draw(
    frame: &mut ratatui::Frame<'_>,
    palette: &DialogSelect<String>,
    title: &str,
    theme: &ResolvedTheme,
) {
    let screen = frame.area();
    let bg = theme.background;
    let panel = rgba_to_color(theme.background_panel, bg);
    let geo = geometry(screen, palette);
    let area = geo.area;

    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(panel)), area);

    let inner_x = area.x + 4;
    let inner_w = area.width.saturating_sub(8);
    frame.render_widget(
        Paragraph::new(title).style(
            Style::default()
                .fg(rgba_to_color(theme.text, bg))
                .bg(panel)
                .add_modifier(Modifier::BOLD),
        ),
        Rect {
            x: inner_x,
            y: area.y + 1,
            width: inner_w.saturating_sub(4),
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new("esc").alignment(Alignment::Right).style(
            Style::default()
                .fg(rgba_to_color(theme.text_muted, bg))
                .bg(panel),
        ),
        Rect {
            x: inner_x + inner_w.saturating_sub(4),
            y: area.y + 1,
            width: 4,
            height: 1,
        },
    );

    let (filter_text, filter_color) = if palette.filter().is_empty() {
        ("Search".to_owned(), theme.text_muted)
    } else {
        (palette.filter().to_owned(), theme.text)
    };
    frame.render_widget(
        Paragraph::new(filter_text).style(
            Style::default()
                .fg(rgba_to_color(filter_color, bg))
                .bg(panel),
        ),
        Rect {
            x: inner_x,
            y: area.y + 2,
            width: inner_w,
            height: 1,
        },
    );

    let list_area = geo.list_area;
    let items = palette.filtered_items();
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("  No results found").style(
                Style::default()
                    .fg(rgba_to_color(theme.text_muted, bg))
                    .bg(panel),
            ),
            list_area,
        );
        return;
    }

    let inner_width = list_area.width as usize;
    let mut list_rows: Vec<ListItem> = Vec::new();
    let mut display_selected = 0usize;
    for row in &geo.rows {
        match *row {
            DialogRow::Header(index) => {
                let category = items[index].category.as_deref().unwrap_or_default();
                list_rows.push(ListItem::new(ratatui::text::Line::from(
                    ratatui::text::Span::styled(
                        format!("  {category}"),
                        Style::default()
                            .fg(rgba_to_color(theme.accent, bg))
                            .add_modifier(Modifier::BOLD),
                    ),
                )));
            }
            DialogRow::Item(index) => {
                if index == palette.selected_index() {
                    display_selected = list_rows.len();
                }
                let item = items[index];
                let row_item = match (&item.description, &item.footer) {
                    (_, Some(footer)) => {
                        let title = match &item.description {
                            Some(detail) => format!("  {} {detail}", item.title),
                            None => format!("  {}", item.title),
                        };
                        let pad = inner_width
                            .saturating_sub(title.chars().count() + footer.chars().count() + 1);
                        ListItem::new(ratatui::text::Line::from(vec![
                            ratatui::text::Span::raw(title),
                            ratatui::text::Span::raw(" ".repeat(pad)),
                            ratatui::text::Span::styled(
                                footer.clone(),
                                Style::default().fg(rgba_to_color(theme.text_muted, bg)),
                            ),
                        ]))
                    }
                    (Some(detail), None) => {
                        let title = format!("  {}", item.title);
                        let pad = inner_width
                            .saturating_sub(title.chars().count() + detail.chars().count() + 1);
                        ListItem::new(ratatui::text::Line::from(vec![
                            ratatui::text::Span::raw(title),
                            ratatui::text::Span::raw(" ".repeat(pad)),
                            ratatui::text::Span::styled(
                                detail.clone(),
                                Style::default().fg(rgba_to_color(theme.text_muted, bg)),
                            ),
                        ]))
                    }
                    (None, None) => ListItem::new(format!("  {}", item.title)),
                };
                list_rows.push(row_item);
            }
        }
    }
    let list = List::new(list_rows)
        .style(Style::default().fg(rgba_to_color(theme.text, bg)).bg(panel))
        .highlight_style(
            Style::default()
                .fg(rgba_to_color(theme.selected_list_item_text, bg))
                .bg(rgba_to_color(theme.primary, bg)),
        );
    let mut state = ListState::default();
    state.select(Some(display_selected));
    frame.render_stateful_widget(list, list_area, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::dialog_select::DialogSelectItem;

    fn two_category_select() -> DialogSelect<String> {
        DialogSelect::new(vec![
            DialogSelectItem::new("New session".to_owned(), "a".to_owned())
                .with_category("Session"),
            DialogSelectItem::new("Switch model".to_owned(), "b".to_owned()).with_category("Model"),
        ])
    }

    #[test]
    fn geometry_interleaves_category_headers_with_items_when_unfiltered() {
        let geo = geometry(Rect::new(0, 0, 80, 40), &two_category_select());
        assert_eq!(
            geo.rows,
            vec![
                DialogRow::Header(0),
                DialogRow::Item(0),
                DialogRow::Header(1),
                DialogRow::Item(1),
            ],
        );
    }

    #[test]
    fn geometry_drops_headers_when_filtering() {
        let mut select = two_category_select();
        select.set_filter("session");
        let geo = geometry(Rect::new(0, 0, 80, 40), &select);
        assert!(geo.rows.iter().all(|row| matches!(row, DialogRow::Item(_))));
    }
}
