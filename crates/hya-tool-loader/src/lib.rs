//! Host-side discovery and loading of first-party tool families and their
//! exposure policies. Native libraries use the ABI in `hya-tool`.

mod base_tools;
mod native_bundle;

#[allow(
    deprecated,
    reason = "Preserve the deprecated BaseToolScheme re-export for embedders"
)]
pub use base_tools::{
    BaseToolAlias, BaseToolExposure, BaseToolScheme, BaseToolsPreset, base_tools_preset,
    file_operations_rule, tool_bundle_presets, tool_prompt,
};
pub use hya_tool::{AliasVisibility, native_bundle::Registrations};
pub use native_bundle::{load, loaded_library_sources};

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use hya_tool::{Tool, ToolRegistry};

use base_tools::TOOL_FAMILIES;

/// Install the full set of canonical builtins (and their hidden aliases)
/// from all four trusted tool families.
#[must_use]
pub fn builtins() -> ToolRegistry {
    from_tool_families(&TOOL_FAMILIES)
}

/// Install the builtins of a subset of the trusted tool families.
///
/// A name two loaded families both export resolves to the entry that
/// declares `overrides: <the other family>` in its exposure policy (for
/// example channel-tools' mail-aware `wait` over extended-tools' `wait`);
/// when the overriding family is not loaded, the overridden tool is
/// installed. Load order never decides.
///
/// # Panics
///
/// Panics on an unknown family identity, or when a family's native
/// library cannot be loaded or disagrees with its bundle policy.
#[must_use]
pub fn from_tool_families(identities: &[&str]) -> ToolRegistry {
    for identity in identities {
        assert!(
            TOOL_FAMILIES.contains(identity),
            "unknown tool family {identity}"
        );
    }
    // Prepare every selected family before loading any native image, so
    // the first image does not serialize its package ahead of the others.
    let _ = tool_bundle_presets();
    let registry = ToolRegistry::default();
    let mut implementations: HashMap<(&'static str, String), Arc<dyn Tool>> = HashMap::new();
    for identity in TOOL_FAMILIES {
        if !identities.contains(&identity) {
            continue;
        }
        let tools = load(identity)
            .unwrap_or_else(|error| panic!("load {identity}: {error}"))
            .tools;
        let names = tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect::<BTreeSet<_>>();
        let expected = tool_bundle_presets()
            .iter()
            .find(|preset| preset.identity() == identity)
            .map(|preset| {
                preset
                    .tools()
                    .iter()
                    .map(|tool| tool.name().to_string())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        assert_eq!(
            names, expected,
            "{identity} library tool set differs from its bundle policy"
        );
        assert_eq!(
            names.len(),
            tools.len(),
            "{identity} library declared duplicate tools"
        );
        implementations.extend(
            tools
                .into_iter()
                .map(|tool| ((identity, tool.name().to_string()), tool)),
        );
    }
    let loaded = tool_bundle_presets()
        .iter()
        .filter(|preset| identities.contains(&preset.identity()))
        .collect::<Vec<_>>();
    for preset in &loaded {
        let identity = TOOL_FAMILIES
            .into_iter()
            .find(|identity| *identity == preset.identity())
            .unwrap_or_else(|| panic!("unknown tool family {}", preset.identity()));
        for exposure in preset.tools() {
            let Some(tool) = implementations.remove(&(identity, exposure.name().to_string()))
            else {
                panic!(
                    "{} declares `{}` without a Rust implementation",
                    preset.identity(),
                    exposure.name()
                );
            };
            if !exposure.exposed() {
                continue;
            }
            let overridden = loaded.iter().any(|other| {
                other
                    .tool(exposure.name())
                    .is_some_and(|winner| winner.overrides() == Some(identity))
            });
            if overridden {
                continue;
            }
            assert_eq!(
                tool.name(),
                exposure.name(),
                "preset and implementation names differ"
            );
            registry.register_builtin(
                tool,
                exposure.permission(),
                exposure
                    .aliases()
                    .iter()
                    .map(|alias| (alias.name(), alias.visibility())),
                identity,
            );
        }
    }
    assert!(
        implementations.is_empty(),
        "Rust builtin implementations are missing from tool-family presets: {:?}",
        implementations.keys().collect::<BTreeSet<_>>()
    );
    for preset in &loaded {
        for scheme in preset.schemes() {
            assert!(
                registry.get(scheme.tool()).is_some(),
                "{} scheme `{}` names missing tool `{}`",
                preset.identity(),
                scheme.scheme(),
                scheme.tool()
            );
        }
    }
    registry
}
