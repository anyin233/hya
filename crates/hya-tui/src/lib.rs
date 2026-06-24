//! `hya_tui` — the TUI app: runtime, state, render, theme, keymap, screens, prompt, widgets.
//!
//! W0 lands the FROZEN CONTRACTS (`contracts`) and the prompt display-offset parity layer
//! (`prompt::display`). Subsequent waves fill in app loop, state, render/flex, theme, keymap,
//! screens, dialogs, and plugins (see PLAN.md).

pub mod app;
pub mod contracts;
pub mod keymap;
pub mod prompt;
pub mod render;
pub mod screens;
pub mod state;
pub mod theme;
pub mod tui;
pub mod widgets;

pub use contracts::{
    Align, BindingId, Extmark, ExtmarkKind, FlexDirection, FlexSpec, Justify, Key, KeyChord,
    KeyEvent, LayoutResult, ManagedTextareaInputLayer, NodeId, PromptDoc, PromptPart, Rect,
    RenderNode, Rgba, SizeHint, Wrap,
};
