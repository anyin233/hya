use ratatui::layout::{Constraint, Direction, Layout, Rect};

const SIDEBAR_BREAKPOINT: u16 = 120;
const SIDEBAR_WIDTH: u16 = 42;

pub struct AppLayout {
    pub timeline: Rect,
    pub runtime_status: Rect,
    pub sidebar: Option<Rect>,
    pub prompt: Rect,
    pub footer: Rect,
}

#[must_use]
pub fn app_layout(area: Rect, prompt_height: u16, footer_height: u16) -> AppLayout {
    let show_sidebar = area.width > SIDEBAR_BREAKPOINT;
    let columns = if show_sidebar {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(SIDEBAR_WIDTH)])
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
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(prompt_height),
            Constraint::Length(footer_height),
        ])
        .split(main);

    let sidebar = if show_sidebar { Some(columns[1]) } else { None };

    AppLayout {
        timeline: rows[0],
        runtime_status: rows[1],
        sidebar,
        prompt: rows[2],
        footer: rows[3],
    }
}

#[must_use]
pub const fn main_width(area: Rect) -> u16 {
    if area.width > SIDEBAR_BREAKPOINT {
        area.width.saturating_sub(SIDEBAR_WIDTH)
    } else {
        area.width
    }
}
