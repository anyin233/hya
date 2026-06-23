use ratatui::layout::{Constraint, Direction, Layout, Rect};

const SIDEBAR_BREAKPOINT: u16 = 120;
const SIDEBAR_WIDTH: u16 = 42;
const MAIN_HORIZONTAL_PADDING: u16 = 2;

pub struct AppLayout {
    pub timeline: Rect,
    pub runtime_status: Rect,
    pub sidebar: Option<Rect>,
    pub prompt: Rect,
    pub footer: Rect,
}

#[must_use]
pub fn app_layout(
    area: Rect,
    prompt_height: u16,
    footer_height: u16,
    sidebar_hidden: bool,
) -> AppLayout {
    let show_sidebar = area.width > SIDEBAR_BREAKPOINT && !sidebar_hidden;
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
    let main = main_content_area(columns[0]);

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
pub const fn main_width(area: Rect, sidebar_hidden: bool) -> u16 {
    let width = if area.width > SIDEBAR_BREAKPOINT && !sidebar_hidden {
        area.width.saturating_sub(SIDEBAR_WIDTH)
    } else {
        area.width
    };
    let inset = main_padding(width);
    width.saturating_sub(inset.saturating_mul(2))
}

const fn main_content_area(area: Rect) -> Rect {
    let inset = main_padding(area.width);
    Rect {
        x: area.x.saturating_add(inset),
        y: area.y,
        width: area.width.saturating_sub(inset.saturating_mul(2)),
        height: area.height,
    }
}

const fn main_padding(width: u16) -> u16 {
    let max_inset = width.saturating_sub(1) / 2;
    if MAIN_HORIZONTAL_PADDING < max_inset {
        MAIN_HORIZONTAL_PADDING
    } else {
        max_inset
    }
}
