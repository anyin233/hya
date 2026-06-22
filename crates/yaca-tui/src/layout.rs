use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct AppLayout {
    pub status: Rect,
    pub timeline: Rect,
    pub sidebar: Option<Rect>,
    pub prompt: Rect,
    pub footer: Rect,
}

#[must_use]
pub fn app_layout(area: Rect) -> AppLayout {
    let show_sidebar = area.width >= 110;
    let columns = if show_sidebar {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(38)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100)])
            .split(area)
    };
    let main = columns[0];

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(main);

    let sidebar = if show_sidebar { Some(columns[1]) } else { None };

    AppLayout {
        status: rows[0],
        timeline: rows[1],
        sidebar,
        prompt: rows[2],
        footer: rows[3],
    }
}
