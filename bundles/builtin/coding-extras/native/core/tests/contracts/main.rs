//! Public coding-extras tool contracts.

#![allow(clippy::unwrap_used)]

mod ask_user;
mod common;
mod fs_tools;
mod patch_hooks;

fn coding_extras_registry() -> hya_tool::ToolRegistry {
    registry_with_planes(
        hya_tool::FormatterPlane::default(),
        hya_tool::LspPlane::default(),
    )
}

/// The coding-extras tools over a bundle state holding `formatter` and `lsp`,
/// the only planes they read.
fn registry_with_planes(
    formatter: hya_tool::FormatterPlane,
    lsp: hya_tool::LspPlane,
) -> hya_tool::ToolRegistry {
    let registry = hya_tool::ToolRegistry::default();
    let state = std::sync::Arc::new(hya_coding_extras_core::CodingExtrasState::with_planes(
        formatter, lsp,
    ));
    for tool in hya_coding_extras_core::coding_extras_tools(state) {
        registry.register(tool).unwrap();
    }
    registry
}
