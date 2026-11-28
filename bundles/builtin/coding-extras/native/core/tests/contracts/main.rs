//! Public coding-extras tool contracts.

#![allow(clippy::unwrap_used)]

mod ask_user;
mod common;
mod fs_tools;
mod patch_hooks;

fn coding_extras_registry() -> hya_tool::ToolRegistry {
    let registry = hya_tool::ToolRegistry::default();
    let state = std::sync::Arc::new(hya_coding_extras_core::CodingExtrasState::new());
    for tool in hya_coding_extras_core::coding_extras_tools(state) {
        registry.register(tool).unwrap();
    }
    registry
}
