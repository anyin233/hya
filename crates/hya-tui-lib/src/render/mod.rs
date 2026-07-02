//! Render helpers for layout, overlays, and ratatui adapters.

/// Ratatui 0.29 adapter helpers.
pub mod draw;
/// Flex layout solving.
pub mod flex;
/// Simple overlay geometry helpers.
pub mod overlay;

/// Re-export of the flex layout cache.
pub use flex::LayoutCache;
/// Re-export of the flex layout solver.
pub use flex::layout;
