#![deny(missing_docs)]

//! Reusable ratatui-friendly terminal UI primitives for Hya and other TUIs.

pub mod component;
pub mod contracts;
pub mod layer;
pub mod render;

pub use component::{Component, ComponentError, ComponentKind, ComponentLayout};
pub use contracts::{
    Align, FlexDirection, FlexSpec, Justify, LayoutResult, NodeId, Rect, RenderNode, Rgba,
    SizeHint, Wrap,
};
pub use layer::{LayerError, LayerId, LayeredRect, layered_rect, validate_no_overlap};
pub use render::flex::{LayoutCache, layout};
