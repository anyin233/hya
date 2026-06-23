use ratatui::style::{Color, Style};

pub struct Theme {
    pub background: Color,
    pub panel: Color,
    pub element: Color,
    pub block: Color,
    pub text: Color,
    pub muted: Color,
    pub primary: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
    pub agent: Color,
}

impl Theme {
    #[must_use]
    pub const fn yaca_dark() -> Self {
        Self {
            background: Color::Rgb(10, 10, 10),
            panel: Color::Rgb(20, 20, 20),
            element: Color::Rgb(30, 30, 30),
            block: Color::Rgb(24, 48, 58),
            text: Color::Rgb(238, 238, 238),
            muted: Color::Rgb(128, 128, 128),
            primary: Color::Rgb(250, 178, 131),
            accent: Color::Rgb(157, 124, 216),
            success: Color::Rgb(127, 216, 143),
            warning: Color::Rgb(245, 167, 66),
            error: Color::Rgb(224, 108, 117),
            info: Color::Rgb(86, 182, 194),
            agent: Color::Rgb(0, 188, 212),
        }
    }

    #[must_use]
    pub fn base(&self) -> Style {
        Style::default().fg(self.text).bg(self.background)
    }
}
