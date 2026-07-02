//! Ratatui 0.29 adapter helpers for `hya-tui-lib` layout primitives.

use ratatui::style::Color;

use crate::contracts::{LayoutResult, NodeId, Rect, Rgba};

/// Converts an RGBA color into a ratatui color by flattening it over
/// `background` first.
#[must_use]
pub fn rgba_to_color(color: Rgba, background: Rgba) -> Color {
    let flattened = color.over(background);
    Color::Rgb(flattened.r, flattened.g, flattened.b)
}

/// Converts a library rectangle into a ratatui 0.29 layout rectangle.
#[must_use]
pub const fn rect_to_ratatui(rect: Rect) -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

/// Looks up `id` in `layout` and converts the solved rectangle into a ratatui
/// rectangle when present.
#[must_use]
pub fn layout_rect_to_ratatui(layout: &LayoutResult, id: NodeId) -> Option<ratatui::layout::Rect> {
    layout.get(id).map(rect_to_ratatui)
}
