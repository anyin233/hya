use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::widgets::prompt_height;

pub struct AppLayout {
    pub status: Rect,
    pub timeline: Rect,
    pub sidebar: Option<Rect>,
    pub prompt: Rect,
    pub footer: Rect,
}

#[must_use]
pub fn app_layout(area: Rect, input: &str) -> AppLayout {
    let prompt_height = prompt_height(input, area.width);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(prompt_height),
            Constraint::Length(1),
        ])
        .split(area);

    let show_sidebar = rows[1].width >= 110;
    let body = if show_sidebar {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(38)])
            .split(rows[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100)])
            .split(rows[1])
    };

    let sidebar = if show_sidebar { Some(body[1]) } else { None };

    AppLayout {
        status: rows[0],
        timeline: body[0],
        sidebar,
        prompt: rows[2],
        footer: rows[3],
    }
}
