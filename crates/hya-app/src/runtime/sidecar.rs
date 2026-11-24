//! Catalog materialization and export validation for Bundle sidecars.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use hya_engine::{CoreError, TurnBinding};
use hya_plugin::PreparedPlugin;
use hya_plugin::messages::{HookName, HookRegistration};
use hya_tool::{ResolvedTool, ToolPermission};

mod activation;
pub use activation::{ActivationBindings, start_activation_sidecar};
/// Materialize the selected captured bundle extension closure into an activation directory.
pub fn materialize_bundle_sidecar_resources(
    binding: &TurnBinding,
    stable_agent_id: &str,
    activation_dir: &Path,
) -> Result<Vec<PathBuf>, CoreError> {
    let (bundle_id, _) = binding
        .bundle_catalog()
        .resolve_agent_entry(stable_agent_id)
        .ok_or_else(|| CoreError::AgentDefinitionMissing {
            agent_id: stable_agent_id.to_string(),
        })?;

    binding
        .has_selected_bundle_sidecar_capability(stable_agent_id)
        .map_err(CoreError::bundle)?;
    let policy = binding
        .agent_resource_policy(stable_agent_id)
        .map_err(CoreError::bundle)?;
    let catalog = binding.bundle_catalog();
    let mut resources = BTreeMap::new();
    let mut entrypoints = BTreeMap::new();

    for (kind, selected_ids) in [
        (
            hya_bundle::ExportKind::Tool,
            policy.selected_bundle_tool_ids(),
        ),
        (hya_bundle::ExportKind::Hook, policy.canonical_hook_ids()),
    ] {
        for stable_id in selected_ids {
            let (owner_bundle_id, resource) = catalog
                .resolve_resource_entry(bundle_id, kind, stable_id)
                .map_err(CoreError::bundle)?;
            let extensions = catalog
                .bundle_resources(owner_bundle_id, hya_bundle::ExportKind::Extension)
                .ok_or_else(|| {
                    CoreError::Invalid(format!(
                        "bundle extension catalog missing `{owner_bundle_id}`"
                    ))
                })?;
            let matches = extensions
                .iter()
                .filter(|extension| extension.source_path == resource.source_path)
                .collect::<Vec<_>>();
            let [extension] = matches.as_slice() else {
                return Err(CoreError::Invalid(format!(
                    "expected exactly one extension for bundle resource `{}`",
                    resource.stable_id
                )));
            };
            insert_materialized_resource(&mut resources, owner_bundle_id, resource)?;
            insert_materialized_resource(&mut resources, owner_bundle_id, extension)?;
            entrypoints
                .entry((owner_bundle_id.to_string(), extension.stable_id.clone()))
                .or_insert_with(|| extension.source_path.clone());
        }
    }

    let owner_ids = resources
        .keys()
        .map(|(owner_bundle_id, _)| owner_bundle_id.as_str())
        .collect::<BTreeSet<_>>();
    let owner_slots = (owner_ids.len() > 1).then(|| {
        owner_ids
            .iter()
            .enumerate()
            .map(|(index, owner_bundle_id)| {
                ((*owner_bundle_id).to_string(), format!("owner-{index:04}"))
            })
            .collect::<BTreeMap<_, _>>()
    });

    for ((owner_bundle_id, source_path), resource) in resources {
        let relative_path = if let Some(owner_slots) = &owner_slots {
            let Some(slot) = owner_slots.get(&owner_bundle_id) else {
                return Err(CoreError::Invalid(format!(
                    "missing materialization slot for bundle `{owner_bundle_id}`"
                )));
            };
            PathBuf::from(slot).join(&source_path)
        } else {
            PathBuf::from(&source_path)
        };
        let output_path = activation_dir.join(&relative_path);
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                CoreError::Invalid(format!(
                    "create bundle resource directory `{}`: {error}",
                    parent.display()
                ))
            })?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)
            .map_err(|error| {
                CoreError::Invalid(format!(
                    "create bundle resource `{}`: {error}",
                    output_path.display()
                ))
            })?;
        output
            .write_all(resource.content.as_bytes())
            .map_err(|error| {
                CoreError::Invalid(format!(
                    "write bundle resource `{}`: {error}",
                    output_path.display()
                ))
            })?;
    }

    let mut extension_paths = Vec::with_capacity(entrypoints.len());
    for ((owner_bundle_id, _), source_path) in entrypoints {
        let output_path = if let Some(owner_slots) = &owner_slots {
            let Some(slot) = owner_slots.get(&owner_bundle_id) else {
                return Err(CoreError::Invalid(format!(
                    "missing materialization slot for bundle `{owner_bundle_id}`"
                )));
            };
            activation_dir.join(slot).join(source_path)
        } else {
            activation_dir.join(source_path)
        };
        extension_paths.push(output_path);
    }
    Ok(extension_paths)
}

fn insert_materialized_resource<'a>(
    resources: &mut BTreeMap<(String, String), &'a hya_bundle::PreparedResource>,
    owner_bundle_id: &str,
    resource: &'a hya_bundle::PreparedResource,
) -> Result<(), CoreError> {
    validate_materialized_resource_path(&resource.source_path)?;
    match resources.entry((owner_bundle_id.to_string(), resource.source_path.clone())) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(resource);
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            let existing = entry.get_mut();
            if existing.digest != resource.digest || existing.content != resource.content {
                return Err(CoreError::Invalid(format!(
                    "conflicting bundle resource path `{}`",
                    resource.source_path
                )));
            }
        }
    }
    Ok(())
}

fn validate_materialized_resource_path(source_path: &str) -> Result<(), CoreError> {
    let mut segments = source_path.split('/');
    let Some(first) = segments.next() else {
        return Err(CoreError::Invalid("empty bundle resource path".to_string()));
    };
    if first.is_empty()
        || first == "."
        || first == ".."
        || (first.len() == 2
            && first
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
            && first.as_bytes().get(1) == Some(&b':'))
    {
        return Err(CoreError::Invalid(format!(
            "unsafe bundle resource path `{source_path}`"
        )));
    }
    if source_path.contains('\\') || source_path.as_bytes().contains(&0) {
        return Err(CoreError::Invalid(format!(
            "unsafe bundle resource path `{source_path}`"
        )));
    }
    for segment in segments {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(CoreError::Invalid(format!(
                "unsafe bundle resource path `{source_path}`"
            )));
        }
    }
    if !Path::new(source_path)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(CoreError::Invalid(format!(
            "unsafe bundle resource path `{source_path}`"
        )));
    }
    Ok(())
}

/// Validate the complete module tool declaration and bind the selected agent view.
pub fn bind_bundle_sidecar_tools(
    binding: &TurnBinding,
    stable_agent_id: &str,
    plugin: &PreparedPlugin,
) -> Result<Arc<[ResolvedTool]>, CoreError> {
    let (bundle_id, _) = binding
        .bundle_catalog()
        .resolve_agent_entry(stable_agent_id)
        .ok_or_else(|| CoreError::AgentDefinitionMissing {
            agent_id: stable_agent_id.to_string(),
        })?;
    let policy = binding
        .agent_resource_policy(stable_agent_id)
        .map_err(CoreError::bundle)?;
    let selected = policy
        .selected_bundle_tool_ids()
        .iter()
        .map(|stable_id| {
            binding
                .bundle_catalog()
                .resolve_resource_entry(bundle_id, hya_bundle::ExportKind::Tool, stable_id)
                .map(|(_, resource)| resource)
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(CoreError::bundle)?;
    let expected =
        bundle_sidecar_module_resources(binding, stable_agent_id, hya_bundle::ExportKind::Tool)?;
    let declarations = &plugin.contributions().tools;
    if expected.len() != declarations.len() {
        return Err(CoreError::Invalid(format!(
            "bundle sidecar tool declaration count mismatch: expected {}, got {}",
            expected.len(),
            declarations.len()
        )));
    }

    let mut bound = Vec::with_capacity(expected.len());
    for resource in &expected {
        declarations
            .iter()
            .find(|declaration| declaration.name == resource.local_id)
            .ok_or_else(|| {
                CoreError::Invalid(format!(
                    "missing bundle sidecar tool declaration `{}`",
                    resource.local_id
                ))
            })?;
    }
    for resource in selected {
        bound.push(ResolvedTool {
            tool: plugin
                .tool(&resource.local_id, &resource.stable_id)
                .ok_or_else(|| {
                    CoreError::Invalid(format!(
                        "missing bundle sidecar tool `{}`",
                        resource.local_id
                    ))
                })?,
            permission: ToolPermission::Tool,
        });
    }
    Ok(Arc::from(bound))
}

/// The complete declaration of the loaded modules, independent of an agent's view.
fn bundle_sidecar_module_resources<'a>(
    binding: &'a TurnBinding,
    stable_agent_id: &str,
    kind: hya_bundle::ExportKind,
) -> Result<Vec<&'a hya_bundle::PreparedResource>, CoreError> {
    let policy = binding
        .agent_resource_policy(stable_agent_id)
        .map_err(CoreError::bundle)?;
    let (bundle_id, _) = binding
        .bundle_catalog()
        .resolve_agent_entry(stable_agent_id)
        .ok_or_else(|| CoreError::AgentDefinitionMissing {
            agent_id: stable_agent_id.to_string(),
        })?;
    let mut paths = BTreeSet::new();
    for (kind, ids) in [
        (
            hya_bundle::ExportKind::Tool,
            policy.selected_bundle_tool_ids(),
        ),
        (hya_bundle::ExportKind::Hook, policy.canonical_hook_ids()),
    ] {
        for id in ids {
            let (_, resource) = binding
                .bundle_catalog()
                .resolve_resource_entry(bundle_id, kind, id)
                .map_err(CoreError::bundle)?;
            paths.insert(resource.source_path.as_str());
        }
    }
    Ok(binding
        .bundle_catalog()
        .bundle_resources(bundle_id, kind)
        .into_iter()
        .flatten()
        .filter(|resource| paths.contains(resource.source_path.as_str()))
        .collect())
}

fn declared_bundle_sidecar_hooks(
    registrations: &[HookRegistration],
) -> Result<BTreeSet<HookName>, CoreError> {
    let mut seen = BTreeSet::new();
    for registration in registrations {
        if !matches!(
            registration.name,
            HookName::ToolExecuteBefore | HookName::ToolExecuteAfter | HookName::Event
        ) {
            return Err(CoreError::Invalid(format!(
                "unsupported Bundle sidecar hook declaration `{}`",
                registration.name.as_str()
            )));
        }
        if !seen.insert(registration.name) {
            return Err(CoreError::Invalid(format!(
                "duplicate Bundle sidecar hook declaration `{}`",
                registration.name.as_str()
            )));
        }
    }
    Ok(seen)
}

/// Require the complete selected-module hook declaration before publishing its view.
pub fn validate_bundle_sidecar_hooks(
    binding: &TurnBinding,
    stable_agent_id: &str,
    registrations: &[HookRegistration],
) -> Result<(), CoreError> {
    let actual = declared_bundle_sidecar_hooks(registrations)?;
    let mut expected = BTreeSet::new();
    for resource in
        bundle_sidecar_module_resources(binding, stable_agent_id, hya_bundle::ExportKind::Hook)?
    {
        let hook = match resource.local_id.as_str() {
            "event" => HookName::Event,
            "tool.execute.before" => HookName::ToolExecuteBefore,
            "tool.execute.after" => HookName::ToolExecuteAfter,
            _ => {
                return Err(CoreError::Invalid(format!(
                    "unsupported Bundle sidecar hook resource `{}`",
                    resource.local_id
                )));
            }
        };
        expected.insert(hook);
    }
    if actual != expected {
        return Err(CoreError::Invalid(format!(
            "Bundle sidecar hook declaration set mismatch: expected {expected:?}, got {actual:?}"
        )));
    }
    Ok(())
}
