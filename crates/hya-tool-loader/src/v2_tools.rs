//! Registration and descriptor metadata helpers for ABI v2 native tool bundles.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

use hya_bundle::{Bundle, first_party_bundle, first_party_bundle_optional};
use hya_runtime_abi::{HostRuntime, NativeRegistrations};
use hya_tool::{AliasVisibility, Tool, ToolPermission, ToolRegistry};
use serde::Deserialize;
use serde_json::Value;

use crate::v2::load_v2;

/// First-party native ABI v2 bundles that export canonical builtin tools via
/// structured `resources.tools` descriptors.
pub const NATIVE_V2_TOOL_BUNDLES: [&str; 2] = ["hya/plan", "hya/workflow"];

#[derive(Deserialize)]
#[serde(untagged)]
enum DescriptorAlias {
    Name(String),
    Detailed {
        name: String,
        #[serde(default)]
        visibility: Option<String>,
    },
}

#[derive(Deserialize)]
struct ToolDescriptor {
    name: String,
    #[serde(default)]
    permission: Option<String>,
    #[serde(default)]
    aliases: Vec<DescriptorAlias>,
    #[serde(default)]
    prompt_hint: Option<String>,
}

#[derive(Deserialize)]
struct ToolDescriptorCollection {
    tools: Vec<ToolDescriptor>,
}

/// Parsed metadata for one tool declared in an ABI v2 bundle's `resources.tools`.
#[derive(Clone, Debug)]
struct V2ToolMetadata {
    name: String,
    permission: ToolPermission,
    aliases: Vec<(String, AliasVisibility)>,
    prompt_hint: Option<String>,
}

fn parse_permission(raw: Option<&str>) -> Result<ToolPermission, String> {
    match raw.unwrap_or("tool") {
        "read_only" => Ok(ToolPermission::ReadOnly),
        "tool" | "interactive" => Ok(ToolPermission::Tool),
        "command" => Ok(ToolPermission::Command),
        "task" => Ok(ToolPermission::Task),
        other => Err(format!("unsupported tool permission `{other}`")),
    }
}

#[allow(
    deprecated,
    reason = "Support descriptor public alias visibility parsed by hya-bundle"
)]
fn parse_aliases(
    descriptor_aliases: &[DescriptorAlias],
    resource_aliases: &[String],
) -> Result<Vec<(String, AliasVisibility)>, String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for entry in descriptor_aliases {
        let (name, vis) = match entry {
            DescriptorAlias::Name(name) => (name.clone(), AliasVisibility::Hidden),
            DescriptorAlias::Detailed { name, visibility } => {
                let vis = match visibility.as_deref().unwrap_or("hidden") {
                    "hidden" => AliasVisibility::Hidden,
                    "public" => AliasVisibility::Public,
                    other => return Err(format!("invalid alias visibility `{other}`")),
                };
                (name.clone(), vis)
            }
        };
        if seen.insert(name.clone()) {
            out.push((name, vis));
        }
    }
    for alias in resource_aliases {
        if seen.insert(alias.clone()) {
            out.push((alias.clone(), AliasVisibility::Hidden));
        }
    }
    Ok(out)
}

fn parse_bundle_tool_metadata(bundle: &Bundle) -> Result<Vec<V2ToolMetadata>, String> {
    let mut out = Vec::with_capacity(bundle.tools().len());
    for resource in bundle.tools() {
        let value: Value = if resource.source_path.ends_with(".json") {
            serde_json::from_str(&resource.content)
                .map_err(|e| format!("{}: {e}", resource.source_path))?
        } else {
            serde_norway::from_str(&resource.content)
                .map_err(|e| format!("{}: {e}", resource.source_path))?
        };
        let descriptor = if value.get("tools").is_some() {
            let collection: ToolDescriptorCollection = serde_json::from_value(value)
                .map_err(|e| format!("{}: {e}", resource.source_path))?;
            collection
                .tools
                .into_iter()
                .find(|entry| entry.name == resource.local_id)
                .ok_or_else(|| {
                    format!(
                        "{}: missing descriptor entry for `{}`",
                        resource.source_path, resource.local_id
                    )
                })?
        } else {
            serde_json::from_value(value).map_err(|e| format!("{}: {e}", resource.source_path))?
        };
        if descriptor.name != resource.local_id {
            return Err(format!(
                "{}: descriptor name `{}` differs from resource id `{}`",
                resource.source_path, descriptor.name, resource.local_id
            ));
        }
        let permission = parse_permission(descriptor.permission.as_deref())?;
        let aliases = parse_aliases(&descriptor.aliases, &resource.aliases)?;
        out.push(V2ToolMetadata {
            name: descriptor.name,
            permission,
            aliases,
            prompt_hint: descriptor.prompt_hint,
        });
    }
    Ok(out)
}

/// Register every tool exported by an ABI v2 first-party bundle into `registry`,
/// using the bundle's prepared `resources.tools` descriptors for permissions
/// and aliases.
///
/// # Errors
/// Returns a diagnostic string when `identity` cannot be prepared, its
/// exported tools disagree with its declared `resources.tools`, or a reserved
/// kernel tool is encountered.
pub fn register_v2_bundle_tools(
    registry: &ToolRegistry,
    identity: &str,
    regs: &NativeRegistrations,
) -> Result<(), String> {
    let catalog = first_party_bundle(identity).map_err(|e| format!("{identity}: {e}"))?;
    let [bundle] = catalog.bundles() else {
        return Err(format!("{identity}: expected a single bundle"));
    };
    register_bundle_tools(registry, bundle, bundle.identity().id.as_str(), regs)
}

/// Register `regs.tools` under `bundle`'s tool descriptors; the library must
/// export exactly the declared tools.
///
/// # Errors
/// The exported and declared tools differ, a descriptor is malformed, or a
/// tool claims a kernel-exclusive name.
pub fn register_bundle_tools(
    registry: &ToolRegistry,
    bundle: &Bundle,
    origin: &'static str,
    regs: &NativeRegistrations,
) -> Result<(), String> {
    let identity = bundle.identity().id.as_str();
    let metadata = parse_bundle_tool_metadata(bundle)?;
    let expected: BTreeSet<&str> = metadata.iter().map(|m| m.name.as_str()).collect();
    let actual: BTreeSet<&str> = regs.tools.iter().map(|t| t.name()).collect();
    if actual != expected {
        return Err(format!(
            "{identity} native tools differ from declared resources.tools: {actual:?} vs {expected:?}"
        ));
    }
    if actual.len() != regs.tools.len() {
        return Err(format!("{identity} library declared duplicate tools"));
    }
    let mut by_name: HashMap<&str, Arc<dyn Tool>> = regs
        .tools
        .iter()
        .map(|tool| (tool.name(), Arc::clone(tool)))
        .collect();
    for meta in &metadata {
        let Some(tool) = by_name.remove(meta.name.as_str()) else {
            return Err(format!(
                "{identity} declares `{}` without a Rust implementation",
                meta.name
            ));
        };
        if hya_tool::is_reserved_kernel_tool(tool.name()) {
            return Err(format!(
                "{identity} cannot register kernel-exclusive tool `{}`",
                tool.name()
            ));
        }
        registry.register_builtin(
            tool,
            meta.permission,
            meta.aliases
                .iter()
                .map(|(alias, visibility)| (alias.as_str(), *visibility)),
            origin,
        );
    }
    Ok(())
}

/// Load an installed (not first-party) native ABI v2 bundle from its prepared
/// form: stage its packaged library, load it, and register its tools.
///
/// # Errors
/// The bundle carries no library, the library fails the ABI check or
/// registration, or its tools disagree with its descriptors.
#[cfg(unix)]
pub fn load_installed_native_bundle(
    registry: &ToolRegistry,
    bundle: &Bundle,
    runtime: Arc<dyn HostRuntime>,
) -> Result<NativeRegistrations, String> {
    let identity = bundle.identity().id.as_str();
    let library_path = bundle
        .runtime()
        .and_then(|runtime| runtime.library.as_deref())
        .ok_or_else(|| format!("{identity}: native runtime declares no `library`"))?;
    let library = bundle
        .extensions()
        .iter()
        .find(|resource| resource.source_path == library_path && resource.binary_base64.is_some())
        .ok_or_else(|| format!("{identity}: package carries no library at {library_path}"))?;
    let payload = library
        .source_bytes()
        .map_err(|error| format!("{identity}: {error}"))?;
    let filename = std::path::Path::new(library_path)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| format!("{identity}: invalid library path {library_path}"))?;
    let path = crate::native_bundle::stage_library(&payload, filename)?;
    let regs = crate::v2::load_v2_from_path(identity, &path, runtime)
        .map_err(|error| format!("load {identity}: {error}"))?;
    // An installed bundle's tools stay registered for the process lifetime.
    let origin: &'static str = Box::leak(identity.to_string().into_boxed_str());
    register_bundle_tools(registry, bundle, origin, &regs)?;
    Ok(regs)
}

/// Load a first-party ABI v2 native tool bundle and register its tools into
/// `registry`, returning the full [`NativeRegistrations`] (including hooks,
/// rewriters, services, etc.).
///
/// # Errors
/// Returns a diagnostic string if dynamic loading, ABI v2 verification, or
/// tool registration fails.
pub fn load_and_register_v2_bundle(
    registry: &ToolRegistry,
    identity: &str,
    runtime: Arc<dyn HostRuntime>,
) -> Result<NativeRegistrations, String> {
    let regs = load_v2(identity, runtime).map_err(|error| format!("load {identity}: {error}"))?;
    register_v2_bundle_tools(registry, identity, &regs)?;
    Ok(regs)
}

static V2_PROMPTS: OnceLock<BTreeMap<String, String>> = OnceLock::new();

/// Look up a `<tools>` prompt hint declared in any ABI v2 native tool bundle
/// (`tools/*.json` `prompt_hint`). Returns `None` cleanly when the bundle is absent.
#[must_use]
pub fn v2_tool_prompt(name: &str) -> Option<&'static str> {
    let prompts = V2_PROMPTS.get_or_init(|| {
        let mut map = BTreeMap::new();
        for identity in NATIVE_V2_TOOL_BUNDLES {
            let Ok(Some(catalog)) = first_party_bundle_optional(identity) else {
                continue;
            };
            let Some(bundle) = catalog.bundles().first() else {
                continue;
            };
            if let Ok(entries) = parse_bundle_tool_metadata(bundle) {
                for entry in entries {
                    if let Some(hint) = entry.prompt_hint {
                        map.insert(entry.name, hint);
                    }
                }
            }
        }
        map
    });
    prompts.get(name).map(String::as_str)
}
