//! Rendering layer: layout solving (`flex`), and (later waves) styled text, markdown,
//! scroll, and overlay composition over ratatui.

pub mod diff;
pub mod draw;
pub mod flex;
pub mod markdown;
mod markdown_highlight;
pub mod overlay;
pub mod scroll;
pub mod text;
pub(crate) mod transcript;

pub use flex::{layout, LayoutCache};
pub use markdown_highlight::highlight_code;
