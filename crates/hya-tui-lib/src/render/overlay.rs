//! Overlay geometry helpers that stay independent of ratatui.

use crate::contracts::Rect;

/// The requested size of a centered modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModalSize {
    /// The requested modal width in cells.
    pub width: u16,
    /// The requested modal height in cells.
    pub height: u16,
}

/// Returns a rectangle of `width` by `height` centered inside `area`, clamped
/// so the result never extends beyond `area`.
#[must_use]
pub const fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let modal_width = if width > area.width {
        area.width
    } else {
        width
    };
    let modal_height = if height > area.height {
        area.height
    } else {
        height
    };

    Rect {
        x: area.x.saturating_add((area.width - modal_width) / 2),
        y: area.y.saturating_add((area.height - modal_height) / 2),
        width: modal_width,
        height: modal_height,
    }
}

/// Returns a centered rectangle for `size` inside `area`.
#[must_use]
pub const fn centered_modal(size: ModalSize, area: Rect) -> Rect {
    centered_rect(size.width, size.height, area)
}

/// Returns `area` inset by `margin` on every side, clamping oversized margins
/// down to an empty rectangle centered within the original area.
#[must_use]
pub const fn inset(area: Rect, margin: u16) -> Rect {
    let doubled = margin.saturating_mul(2);
    let horizontal = if doubled > area.width {
        area.width
    } else {
        doubled
    };
    let vertical = if doubled > area.height {
        area.height
    } else {
        doubled
    };
    let left = if margin > area.width / 2 {
        area.width / 2
    } else {
        margin
    };
    let top = if margin > area.height / 2 {
        area.height / 2
    } else {
        margin
    };

    Rect {
        x: area.x.saturating_add(left),
        y: area.y.saturating_add(top),
        width: area.width - horizontal,
        height: area.height - vertical,
    }
}
