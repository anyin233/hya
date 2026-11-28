//! Host-side discovery and loading of first-party tool families and native
//! bundles across both transitional ABI v1 and lockstep ABI v2.

mod base_tools;
mod native_bundle;
mod v2;

pub use base_tools::{
    BaseToolAlias, BaseToolExposure, BaseToolsPreset, base_tools_preset, file_operations_rule,
    is_protected_tool_name, tool_bundle_presets, tool_prompt,
};
pub use hya_runtime_abi::{
    AbiError, HostRuntime, NativeAbiV2, NativePluginContext, NativeRegistrations,
};
pub use hya_tool::{AliasVisibility, native_bundle::Registrations};
pub use native_bundle::{load, loaded_library_sources};
pub use v2::{LoadedNativeBundles, NativeBundleLoader, load_v2, load_v2_from_path};

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

/// Non-panicking selective loader for a subset of the trusted v1 tool families.
///
/// # Errors
///
/// Returns a diagnostic string when `identities` contains an unknown family,
/// when a family's native library cannot be loaded, or when its exported tools
/// disagree with its bundle policy.
pub fn try_from_tool_families(identities: &[&str]) -> Result<ToolRegistry, String> {
    if identities.is_empty() {
        return Ok(ToolRegistry::default());
    }
    for identity in identities {
        if !TOOL_FAMILIES.contains(identity) {
            return Err(format!("unknown tool family {identity}"));
        }
    }
    let _ = tool_bundle_presets();
    let registry = ToolRegistry::default();
    let mut implementations: HashMap<(&'static str, String), Arc<dyn Tool>> = HashMap::new();
    for identity in TOOL_FAMILIES {
        if !identities.contains(&identity) {
            continue;
        }
        let tools = load(identity)
            .map_err(|error| format!("load {identity}: {error}"))?
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
        if names != expected {
            return Err(format!(
                "{identity} library tool set differs from its bundle policy: {names:?} vs {expected:?}"
            ));
        }
        if names.len() != tools.len() {
            return Err(format!("{identity} library declared duplicate tools"));
        }
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
        let Some(identity) = TOOL_FAMILIES
            .into_iter()
            .find(|identity| *identity == preset.identity())
        else {
            return Err(format!("unknown tool family {}", preset.identity()));
        };
        for exposure in preset.tools() {
            let Some(tool) = implementations.remove(&(identity, exposure.name().to_string()))
            else {
                return Err(format!(
                    "{} declares `{}` without a Rust implementation",
                    preset.identity(),
                    exposure.name()
                ));
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
            if tool.name() != exposure.name() {
                return Err(format!(
                    "preset and implementation names differ: {} vs {}",
                    exposure.name(),
                    tool.name()
                ));
            }
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
    if !implementations.is_empty() {
        return Err(format!(
            "Rust builtin implementations are missing from tool-family presets: {:?}",
            implementations.keys().collect::<BTreeSet<_>>()
        ));
    }
    Ok(registry)
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
    try_from_tool_families(identities).unwrap_or_else(|error| panic!("{error}"))
}
