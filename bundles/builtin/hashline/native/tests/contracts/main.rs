//! Public hashline contracts.

#![allow(clippy::unwrap_used)]

mod common;
mod edit_hashline;
mod grep;
mod read_write;

fn hashline_registry() -> hya_tool::ToolRegistry {
    let registry = hya_tool::ToolRegistry::default();
    for tool in hya_hashline::hashline_tools() {
        registry.register(tool).unwrap();
    }
    registry
}
