//! Public coding-extras tool contracts.

#![allow(clippy::unwrap_used)]

mod ask_user;
mod common;
mod formatter;
mod fs_tools;
mod patch_hooks;

use hya_coding_extras_core::formatter::FormatterPlane;
use hya_coding_extras_core::lsp_service::LspPlane;

fn coding_extras_registry() -> hya_tool::ToolRegistry {
    registry_with_planes(FormatterPlane::default(), LspPlane::default())
}

/// The coding-extras tools over a bundle state holding `formatter` and `lsp`,
/// the only planes they read.
fn registry_with_planes(formatter: FormatterPlane, lsp: LspPlane) -> hya_tool::ToolRegistry {
    let registry = hya_tool::ToolRegistry::default();
    let state = std::sync::Arc::new(hya_coding_extras_core::CodingExtrasState::with_planes(
        formatter, lsp,
    ));
    for tool in hya_coding_extras_core::coding_extras_tools(state) {
        registry.register(tool).unwrap();
    }
    registry
}
