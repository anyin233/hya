//! Theme definitions for the yaca TUI.
//!
//! The default palette is a port of opencode's default dark theme
//! (`packages/tui/src/theme/assets/opencode.json`).

use ratatui::style::Color;

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub background: Color,
    pub background_panel: Color,
    pub background_element: Color,
    pub border: Color,
    pub border_active: Color,
    pub border_subtle: Color,
    pub text: Color,
    pub text_muted: Color,
    pub primary: Color,
    pub secondary: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
}

impl Theme {
    /// opencode's default dark palette.
    #[must_use]
    pub fn opencode_dark() -> Self {
        Self {
            background: Self::hex(0x0a, 0x0a, 0x0a),
            background_panel: Self::hex(0x14, 0x14, 0x14),
            background_element: Self::hex(0x1e, 0x1e, 0x1e),
            border: Self::hex(0x48, 0x48, 0x48),
            border_active: Self::hex(0x60, 0x60, 0x60),
            border_subtle: Self::hex(0x3c, 0x3c, 0x3c),
            text: Self::hex(0xee, 0xee, 0xee),
            text_muted: Self::hex(0x80, 0x80, 0x80),
            primary: Self::hex(0xfa, 0xb2, 0x83),
            secondary: Self::hex(0x5c, 0x9c, 0xf5),
            accent: Self::hex(0x9d, 0x7c, 0xd8),
            success: Self::hex(0x7f, 0xd8, 0x8f),
            warning: Self::hex(0xf5, 0xa7, 0x42),
            error: Self::hex(0xe0, 0x6c, 0x75),
            info: Self::hex(0x56, 0xb6, 0xc2),
        }
    }

    const fn hex(r: u8, g: u8, b: u8) -> Color {
        Color::Rgb(r, g, b)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::opencode_dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_is_opencode_dark() {
        assert_eq!(Theme::default(), Theme::opencode_dark());
    }
}
