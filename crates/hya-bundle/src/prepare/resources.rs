use std::collections::{BTreeMap, BTreeSet};

use hya_proto::AgentName;

use super::decode::{digest_bytes, resource_order, set_bundle_digest};
use super::extensions::validate_hook_local_id;
use super::parse::{normalize_source_path, read_text_reference};
use crate::error::BundleError;
use crate::model::{Bundle, PreparedAgent, PreparedResource};
use crate::source::{
    SourceAgent, SourceExtensions, SourceMcpServer, SourceResource, SourceResources,
};

pub(super) fn resolve_catalog_references(bundles: &mut [Bundle]) -> Result<(), BundleError> {
    for bundle in bundles.iter_mut() {
        let bundle_id = bundle.identity().id.clone();
        let mut local_resources: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut hook_resources = BTreeMap::new();
        for resource in bundle
            .tools()
            .iter()
            .chain(bundle.skills())
            .chain(bundle.mcp())
            .chain(bundle.hooks())
            .chain(bundle.extensions())
        {
            for name in std::iter::once(resource.local_id.as_str())
                .chain(resource.aliases.iter().map(String::as_str))
            {
                local_resources
                    .entry(name.to_string())
                    .or_default()
                    .insert(resource.stable_id.clone());
            }
        }
        for hook in bundle.hooks() {
            hook_resources.insert(hook.stable_id.clone(), hook.local_id.clone());
        }
        let resources = bundle
            .tools()
            .iter()
            .chain(bundle.skills())
            .chain(bundle.mcp())
            .chain(bundle.hooks())
            .chain(bundle.extensions())
            .map(|resource| resource.stable_id.clone())
            .collect::<BTreeSet<_>>();
        resolve_agents(
            &bundle_id,
            &mut bundle.agents,
            &resources,
            &local_resources,
            &hook_resources,
        )?;
        set_bundle_digest(bundle)?;
    }
    Ok(())
}

fn resolve_agents(
    bundle_id: &str,
    agents: &mut [PreparedAgent],
    resources: &BTreeSet<String>,
    local_resources: &BTreeMap<String, BTreeSet<String>>,
    hook_resources: &BTreeMap<String, String>,
) -> Result<(), BundleError> {
    for agent in agents {
        let mut resolved = agent.can_spawn.clone();
        resolved.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        resolved.dedup_by(|left, right| left.as_str() == right.as_str());
        agent.can_spawn = resolved;

        for target in agent.resource_view.aliases.values_mut() {
            *target = resolve_resource_reference(bundle_id, target, resources, local_resources)?;
        }
        agent.resource_view.allow = agent
            .resource_view
            .allow
            .iter()
            .map(|reference| {
                resolve_resource_reference(bundle_id, reference, resources, local_resources)
            })
            .collect::<Result<Vec<_>, _>>()?;
        agent.resource_view.allow.sort();
        agent.resource_view.allow.dedup();
        agent.resource_view.deny = agent
            .resource_view
            .deny
            .iter()
            .map(|reference| {
                resolve_resource_reference(bundle_id, reference, resources, local_resources)
            })
            .collect::<Result<Vec<_>, _>>()?;
        agent.resource_view.deny.sort();
        agent.resource_view.deny.dedup();

        let mut hook_refs = Vec::with_capacity(agent.hook_refs.len());
        for reference in &agent.hook_refs {
            let resolved = match resolve_resource_reference(
                bundle_id,
                reference,
                resources,
                local_resources,
            ) {
                Ok(resolved) => resolved,
                Err(BundleError::UnknownResourceReference { .. }) => {
                    return Err(BundleError::UnknownResourceReference {
                        bundle_id: bundle_id.to_string(),
                        kind: "hook".to_string(),
                        reference: reference.clone(),
                    });
                }
                Err(error) => return Err(error),
            };
            let Some(local_id) = hook_resources.get(&resolved) else {
                return Err(BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: "hook".to_string(),
                    reference: reference.clone(),
                });
            };
            validate_hook_local_id(bundle_id, local_id)?;
            hook_refs.push(resolved);
        }
        hook_refs.sort();
        if let Some(duplicate) = hook_refs
            .windows(2)
            .find_map(|window| (window[0] == window[1]).then_some(window[0].clone()))
        {
            return Err(BundleError::AliasCollision {
                bundle_id: bundle_id.to_string(),
                name: duplicate,
            });
        }
        agent.hook_refs = hook_refs;
    }
    Ok(())
}

fn resolve_resource_reference(
    bundle_id: &str,
    reference: &str,
    resources: &BTreeSet<String>,
    local_resources: &BTreeMap<String, BTreeSet<String>>,
) -> Result<String, BundleError> {
    if reference.starts_with("harness:") {
        let valid = ["tool", "skill", "mcp"]
            .iter()
            .any(|kind| reference.starts_with(&format!("harness:{kind}/")));
        if valid {
            return Ok(reference.to_string());
        }
    } else if reference.starts_with("bundle:") {
        if resources.contains(reference) && reference.starts_with(&format!("bundle:{bundle_id}/")) {
            return Ok(reference.to_string());
        }
    } else if let Some(candidates) = local_resources.get(reference) {
        if candidates.len() > 1 {
            return Err(BundleError::AliasCollision {
                bundle_id: bundle_id.to_string(),
                name: reference.to_string(),
            });
        }
        if let Some(candidate) = candidates.iter().next() {
            return Ok(candidate.clone());
        }
    }
    Err(BundleError::UnknownResourceReference {
        bundle_id: bundle_id.to_string(),
        kind: "resource".to_string(),
        reference: reference.to_string(),
    })
}

type PreparedResourceSets = (
    Vec<PreparedResource>,
    Vec<PreparedResource>,
    Vec<PreparedResource>,
    Vec<PreparedResource>,
    Vec<PreparedResource>,
);

pub(super) fn prepare_resource_sets(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    resources: SourceResources,
    extensions: SourceExtensions,
) -> Result<PreparedResourceSets, BundleError> {
    let tools = prepare_resources(bundle_id, "tool", files, resources.tools)?;
    let skills = prepare_resources(bundle_id, "skill", files, resources.skills)?;
    let mcp = prepare_mcp_resources(bundle_id, files, resources.mcp)?;
    let hooks = prepare_resources(bundle_id, "hook", files, resources.hooks)?;
    let process_backed = extensions.process.is_some();
    for hook in &hooks {
        validate_hook_local_id(bundle_id, &hook.local_id)?;
        if !process_backed
            && !matches!(
                hook.local_id.as_str(),
                "event" | "tool.execute.before" | "tool.execute.after"
            )
        {
            return Err(BundleError::UnsupportedBundleFeature {
                bundle_id: bundle_id.to_string(),
                feature: format!("hook:{}", hook.local_id),
            });
        }
    }
    let mut executable_extensions = extensions.js;
    let native_binaries = prepare_binary_resources(bundle_id, files, extensions.rust, "extension")?;
    let native_libraries =
        prepare_binary_resources(bundle_id, files, extensions.libraries, "library")?;
    let library_backed = !native_libraries.is_empty();
    if process_backed {
        executable_extensions.extend(extensions.files);
        let mut extensions =
            prepare_resources(bundle_id, "extension", files, executable_extensions)?;
        extensions.extend(native_binaries);
        extensions.extend(native_libraries);
        extensions.sort_by(resource_order);
        let mut names = BTreeSet::new();
        for extension in &extensions {
            for name in std::iter::once(&extension.local_id).chain(&extension.aliases) {
                if !names.insert(name.as_str()) {
                    return Err(BundleError::NamespaceCollision {
                        bundle_id: bundle_id.to_string(),
                        name: name.clone(),
                    });
                }
            }
        }
        return Ok((tools, skills, mcp, hooks, extensions));
    }
    let support_files = extensions.files;
    let mut extensions = prepare_resources(bundle_id, "extension", files, executable_extensions)?;
    let extension_path_counts = extensions
        .iter()
        .fold(BTreeMap::new(), |mut counts, resource| {
            *counts.entry(resource.source_path.as_str()).or_default() += 1;
            counts
        });
    let selected_extension_paths = tools
        .iter()
        .chain(&hooks)
        .map(|resource| resource.source_path.as_str())
        .collect::<BTreeSet<_>>();
    for resource in tools.iter().chain(&hooks) {
        if library_backed
            && tools
                .iter()
                .any(|tool| tool.stable_id == resource.stable_id)
        {
            continue;
        }
        match extension_path_counts
            .get(resource.source_path.as_str())
            .copied()
            .unwrap_or(0)
        {
            0 => {
                return Err(BundleError::UnsupportedBundleFeature {
                    bundle_id: bundle_id.to_string(),
                    feature: format!("unmatched executable resource:{}", resource.stable_id),
                });
            }
            1 => {}
            _ => {
                return Err(BundleError::UnsupportedBundleFeature {
                    bundle_id: bundle_id.to_string(),
                    feature: format!("ambiguous executable resource:{}", resource.stable_id),
                });
            }
        }
    }
    for extension in &extensions {
        if !selected_extension_paths.contains(extension.source_path.as_str()) {
            return Err(BundleError::UnsupportedBundleFeature {
                bundle_id: bundle_id.to_string(),
                feature: format!("unreachable extension:{}", extension.stable_id),
            });
        }
    }
    let support = prepare_resources(bundle_id, "extension", files, support_files)?;
    for resource in support {
        if extensions
            .iter()
            .any(|entry| entry.stable_id == resource.stable_id)
        {
            return Err(BundleError::NamespaceCollision {
                bundle_id: bundle_id.to_string(),
                name: resource.stable_id,
            });
        }
        extensions.push(resource);
    }
    extensions.extend(native_libraries);
    extensions.sort_by(resource_order);
    let mut names = BTreeSet::new();
    for extension in &extensions {
        for name in std::iter::once(&extension.local_id).chain(&extension.aliases) {
            if !names.insert(name.as_str()) {
                return Err(BundleError::NamespaceCollision {
                    bundle_id: bundle_id.to_string(),
                    name: name.clone(),
                });
            }
        }
    }
    Ok((tools, skills, mcp, hooks, extensions))
}

/// Prepare `resources.mcp` declarations: each entry names a JSON file that
/// must parse into hya's `McpServerConfig` shape (stdio command or remote
/// url). The bytes are retained verbatim; runtime spawning lands later.
fn prepare_mcp_resources(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    resources: Vec<SourceResource>,
) -> Result<Vec<PreparedResource>, BundleError> {
    let prepared = prepare_resources(bundle_id, "mcp", files, resources)?;
    for resource in &prepared {
        let server: SourceMcpServer =
            serde_json::from_str(resource.content.as_str()).map_err(|error| {
                BundleError::InvalidManifest {
                    source_name: bundle_id.to_string(),
                    detail: format!(
                        "MCP declaration `{}` must be a JSON object with `command`/`env`/`url`/\
                         `transport`/`enabled`/`timeout_ms` fields: {error}",
                        resource.source_path
                    ),
                }
            })?;
        let has_command = !server.command.is_empty();
        let has_url = server.url.as_deref().is_some_and(|url| !url.is_empty());
        if !has_command && !has_url {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "MCP declaration `{}` must declare a non-empty `command` argv or `url`",
                    resource.source_path
                ),
            });
        }
        if has_command && server.command.iter().any(|arg| arg.trim().is_empty()) {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "MCP declaration `{}` command arguments must not be blank",
                    resource.source_path
                ),
            });
        }
    }
    Ok(prepared)
}

pub(super) fn validate_resource_views(
    bundle_id: &str,
    agent: &PreparedAgent,
    tools: &[PreparedResource],
    skills: &[PreparedResource],
) -> Result<(), BundleError> {
    let occupied = tools
        .iter()
        .chain(skills)
        .flat_map(|resource| {
            std::iter::once(resource.local_id.as_str())
                .chain(resource.aliases.iter().map(String::as_str))
        })
        .collect::<BTreeSet<_>>();
    if let Some(alias) = agent
        .resource_view
        .aliases
        .keys()
        .find(|alias| occupied.contains(alias.as_str()))
    {
        return Err(BundleError::AliasCollision {
            bundle_id: bundle_id.to_string(),
            name: alias.clone(),
        });
    }
    Ok(())
}

pub(super) fn prepare_agent(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    markdown_prompt: Option<&str>,
    mut source: SourceAgent,
    stable_agent_ids: &mut BTreeSet<String>,
) -> Result<PreparedAgent, BundleError> {
    if source.spawn_lifecycle.is_some() {
        return Err(BundleError::RemovedManifestKey {
            source_name: bundle_id.to_string(),
            key: "spawn_lifecycle".to_string(),
            guidance: format!(
                "delete it from agent `{}`; every subagent is a resident actor (spawned \
                 non-blocking, woken by mail, archived by its report or its parent's \
                 `archive`)",
                source.id
            ),
        });
    }
    if source.resource_profile.is_some() {
        return Err(BundleError::UnsupportedBundleFeature {
            bundle_id: bundle_id.to_string(),
            feature: "agent.resource_profile".to_string(),
        });
    }
    if !stable_agent_ids.insert(source.id.clone()) {
        return Err(BundleError::DuplicateStableAgentId {
            stable_id: source.id,
        });
    }
    let (prompt, prompt_source) = match (source.prompt.take(), markdown_prompt) {
        (Some(path), None) => {
            let path = normalize_source_path(bundle_id, &path)?;
            (
                Some(read_text_reference(bundle_id, files, &path)?),
                Some(path),
            )
        }
        (None, Some(body)) => (Some(body.to_string()), Some("bundle.hya.md".to_string())),
        (None, None) => (None, None),
        (Some(_), Some(_)) => {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: "Markdown Agent cannot also name a prompt resource".to_string(),
            });
        }
    };
    let prompt_digest = prompt.as_deref().map(|text| digest_bytes(text.as_bytes()));
    let mut advertise = source
        .advertise
        .into_iter()
        .map(|value| match value {
            serde_json::Value::String(name) => Ok(name),
            _ => Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "agent `{}` advertise entries must be tool name strings",
                    source.id
                ),
            }),
        })
        .collect::<Result<Vec<_>, BundleError>>()?;
    advertise.sort();
    advertise.dedup();
    source.can_spawn.sort();
    source.can_spawn.dedup();
    source.hook_refs.sort();
    source.resource_view.allow.sort();
    source.resource_view.allow.dedup();
    source.resource_view.deny.sort();
    source.resource_view.deny.dedup();
    Ok(PreparedAgent {
        id: AgentName::new(source.id),
        description: source.description,
        role: source.role,
        color: source.color,
        prompt,
        prompt_source,
        prompt_digest,
        model_policy: source.model_policy,
        workdir: source.workdir,
        resource_view: source.resource_view,
        can_spawn: source.can_spawn.into_iter().map(AgentName::new).collect(),
        private: source.private,
        hook_refs: source.hook_refs,
        advertise,
        handoff: source.handoff,
    })
}

fn prepare_resources(
    bundle_id: &str,
    kind: &str,
    files: &BTreeMap<String, Vec<u8>>,
    resources: Vec<SourceResource>,
) -> Result<Vec<PreparedResource>, BundleError> {
    let mut canonical = BTreeSet::new();
    let mut aliases = BTreeSet::new();
    let mut prepared = Vec::with_capacity(resources.len());
    for mut resource in resources {
        if !canonical.insert(resource.id.clone()) {
            return Err(BundleError::NamespaceCollision {
                bundle_id: bundle_id.to_string(),
                name: resource.id,
            });
        }
        resource.aliases.sort();
        resource.aliases.dedup();
        for alias in &resource.aliases {
            if canonical.contains(alias) || !aliases.insert(alias.clone()) {
                return Err(BundleError::AliasCollision {
                    bundle_id: bundle_id.to_string(),
                    name: alias.clone(),
                });
            }
        }
        if aliases.contains(&resource.id) {
            return Err(BundleError::NamespaceCollision {
                bundle_id: bundle_id.to_string(),
                name: resource.id,
            });
        }
        let path = normalize_source_path(bundle_id, &resource.path)?;
        let Some(bytes) = files.get(&path) else {
            return Err(BundleError::MissingReference {
                bundle_id: bundle_id.to_string(),
                path,
            });
        };
        let content = std::str::from_utf8(bytes)
            .map_err(|error| BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!("resource `{path}` is not UTF-8: {error}"),
            })?
            .to_string();
        let local_id = resource.id;
        prepared.push(PreparedResource {
            stable_id: format!("bundle:{bundle_id}/{kind}/{local_id}"),
            local_id,
            source_path: path,
            digest: digest_bytes(bytes),
            content,
            binary_base64: None,
            aliases: resource.aliases,
        });
    }
    prepared.sort_by(|left, right| left.local_id.cmp(&right.local_id));
    Ok(prepared)
}

fn prepare_binary_resources(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    resources: Vec<SourceResource>,
    kind: &str,
) -> Result<Vec<PreparedResource>, BundleError> {
    use base64::Engine as _;
    let mut prepared = Vec::with_capacity(resources.len());
    let mut ids = BTreeSet::new();
    for resource in resources {
        if !ids.insert(resource.id.clone()) || !resource.aliases.is_empty() {
            return Err(BundleError::NamespaceCollision {
                bundle_id: bundle_id.to_string(),
                name: resource.id,
            });
        }
        let path = normalize_source_path(bundle_id, &resource.path)?;
        let bytes = files
            .get(&path)
            .ok_or_else(|| BundleError::MissingReference {
                bundle_id: bundle_id.to_string(),
                path: path.clone(),
            })?;
        prepared.push(PreparedResource {
            stable_id: format!("bundle:{bundle_id}/{kind}/{}", resource.id),
            local_id: resource.id,
            source_path: path,
            digest: digest_bytes(bytes),
            content: String::new(),
            binary_base64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
            aliases: Vec::new(),
        });
    }
    prepared.sort_by(|left, right| left.local_id.cmp(&right.local_id));
    Ok(prepared)
}
