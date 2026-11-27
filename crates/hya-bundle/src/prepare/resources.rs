use std::collections::{BTreeMap, BTreeSet};

use hya_proto::AgentName;

use super::decode::{digest_bytes, resource_order, set_bundle_digest};
use super::extensions::validate_hook_local_id;
use super::parse::{normalize_source_path, read_text_reference, split_markdown};
use crate::error::BundleError;
use crate::model::{
    AgentRole, Bundle, PreparedAgent, PreparedJsRuntimeMode, PreparedNativeCapability,
    PreparedProcessExtension, PreparedResource, PreparedRuntime, PreparedRuntimeKind,
};
use crate::source::{
    SourceAgent, SourceAgentEntry, SourceAgentFile, SourceExtensions, SourceHookCommand,
    SourceHookFailureMode, SourceHookHandler, SourceHooksDescriptor, SourceMcpServer,
    SourceResource, SourceResources, SourceToolDescriptor, SourceToolDescriptorAlias,
    SourceToolDescriptorCollection,
};

const MAX_COMMAND_BYTES: usize = 256 * 1024;

pub(super) fn resolve_catalog_references(bundles: &mut [Bundle]) -> Result<(), BundleError> {
    for bundle in bundles.iter_mut() {
        let bundle_id = bundle.identity().id.clone();
        let mut local_resources: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut hook_resources = BTreeMap::new();
        for resource in bundle
            .tools()
            .iter()
            .chain(bundle.skills())
            .chain(bundle.commands())
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
            .chain(bundle.commands())
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

pub(super) struct PreparedResourceSets {
    pub tools: Vec<PreparedResource>,
    pub skills: Vec<PreparedResource>,
    pub commands: Vec<PreparedResource>,
    pub mcp: Vec<PreparedResource>,
    pub hooks: Vec<PreparedResource>,
    pub extensions: Vec<PreparedResource>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_resource_sets(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    resources: SourceResources,
    extensions: SourceExtensions,
    assets: Vec<SourceResource>,
    runtime: Option<&PreparedRuntime>,
    process: Option<&PreparedProcessExtension>,
    runtime_js_entry: Option<SourceResource>,
    runtime_native_library: Option<SourceResource>,
) -> Result<PreparedResourceSets, BundleError> {
    let tools = prepare_tool_resources(bundle_id, files, resources.tools)?;
    let skills = prepare_resources(bundle_id, "skill", files, resources.skills)?;
    let commands = prepare_command_resources(bundle_id, files, resources.commands)?;
    let mcp = prepare_mcp_resources(bundle_id, files, resources.mcp)?;
    let hooks = prepare_hook_resources(bundle_id, files, resources.hooks)?;
    let process_backed = process.is_some();
    let managed_js = runtime.is_some_and(|rt| {
        rt.kind == PreparedRuntimeKind::Js && rt.mode == Some(PreparedJsRuntimeMode::Managed)
    });
    let native_runtime = runtime.is_some_and(|rt| rt.kind == PreparedRuntimeKind::Native);
    let native_hooks_declared = runtime.is_some_and(|rt| {
        rt.kind == PreparedRuntimeKind::Native
            && rt.capabilities.contains(&PreparedNativeCapability::Hooks)
    });
    for hook in &hooks {
        validate_hook_local_id(bundle_id, &hook.local_id)?;
        if !process_backed
            && !managed_js
            && !native_hooks_declared
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
    let mut support_files = extensions.files;
    support_files.extend(assets);
    if let Some(mut entry) = runtime_js_entry {
        let already_listed = executable_extensions
            .iter()
            .chain(&support_files)
            .any(|res| normalize_source_path(bundle_id, &res.path).as_deref() == Ok(&entry.path));
        if !already_listed {
            let id_taken = executable_extensions
                .iter()
                .chain(&support_files)
                .any(|res| res.id == entry.id || res.aliases.contains(&entry.id));
            if id_taken {
                entry.id = "runtime-entry".to_string();
            }
            executable_extensions.push(entry);
        }
    }
    let native_binaries = prepare_binary_resources(bundle_id, files, extensions.rust, "extension")?;
    let mut library_sources = extensions.libraries;
    if let Some(lib) = runtime_native_library {
        library_sources.push(lib);
    }
    let native_libraries = prepare_binary_resources(bundle_id, files, library_sources, "library")?;
    let library_backed = !native_libraries.is_empty() || native_runtime;
    if process_backed {
        executable_extensions.extend(support_files);
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
        return Ok(PreparedResourceSets {
            tools,
            skills,
            commands,
            mcp,
            hooks,
            extensions,
        });
    }
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
    let runtime_entry_path = runtime.and_then(|rt| rt.entry.as_deref());
    for resource in tools.iter().chain(&hooks) {
        let is_tool = tools
            .iter()
            .any(|tool| tool.stable_id == resource.stable_id);
        if library_backed && (is_tool || native_hooks_declared) {
            continue;
        }
        if managed_js && is_descriptor_path(&resource.source_path) {
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
        if runtime_entry_path == Some(extension.source_path.as_str()) {
            continue;
        }
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
    Ok(PreparedResourceSets {
        tools,
        skills,
        commands,
        mcp,
        hooks,
        extensions,
    })
}

fn is_descriptor_path(path: &str) -> bool {
    [".json", ".yaml", ".yml"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
}

fn parse_descriptor_value(
    bundle_id: &str,
    path: &str,
    content: &str,
    kind_label: &str,
) -> Result<serde_json::Value, BundleError> {
    if path.ends_with(".json") {
        serde_json::from_str(content)
    } else {
        serde_norway::from_str(content).map_err(|error| {
            serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error.to_string(),
            ))
        })
    }
    .map_err(|error| BundleError::InvalidManifest {
        source_name: bundle_id.to_string(),
        detail: format!("{kind_label} descriptor `{path}` failed to parse: {error}"),
    })
}

fn prepare_tool_resources(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    resources: Vec<SourceResource>,
) -> Result<Vec<PreparedResource>, BundleError> {
    let prepared = prepare_resources(bundle_id, "tool", files, resources)?;
    for resource in &prepared {
        if !is_descriptor_path(&resource.source_path) {
            continue;
        }
        let value =
            parse_descriptor_value(bundle_id, &resource.source_path, &resource.content, "tool")?;
        let Some(object) = value.as_object() else {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "tool descriptor `{}` must be a JSON/YAML object",
                    resource.source_path
                ),
            });
        };
        // Legacy `{}` placeholder used by v1 process/native declarations.
        if object.is_empty() {
            continue;
        }
        if object.contains_key("tools") {
            let collection: SourceToolDescriptorCollection = serde_json::from_value(value)
                .map_err(|error| BundleError::InvalidManifest {
                    source_name: bundle_id.to_string(),
                    detail: format!(
                        "tool descriptor `{}` is invalid: {error}",
                        resource.source_path
                    ),
                })?;
            if let Some(version) = collection.schema_version
                && version != 1
            {
                return Err(BundleError::InvalidManifest {
                    source_name: bundle_id.to_string(),
                    detail: format!(
                        "tool descriptor `{}` has unsupported schema_version `{version}`",
                        resource.source_path
                    ),
                });
            }
            if collection.tools.is_empty() {
                return Err(BundleError::InvalidManifest {
                    source_name: bundle_id.to_string(),
                    detail: format!(
                        "tool descriptor `{}` must declare at least one tool",
                        resource.source_path
                    ),
                });
            }
            let mut matched = false;
            for tool in &collection.tools {
                validate_tool_descriptor(bundle_id, &resource.source_path, tool)?;
                if tool.name == resource.local_id {
                    matched = true;
                }
            }
            if !matched {
                return Err(BundleError::InvalidManifest {
                    source_name: bundle_id.to_string(),
                    detail: format!(
                        "tool descriptor `{}` does not contain an entry named `{}`",
                        resource.source_path, resource.local_id
                    ),
                });
            }
        } else {
            let descriptor: SourceToolDescriptor =
                serde_json::from_value(value).map_err(|error| BundleError::InvalidManifest {
                    source_name: bundle_id.to_string(),
                    detail: format!(
                        "tool descriptor `{}` is invalid: {error}",
                        resource.source_path
                    ),
                })?;
            validate_tool_descriptor(bundle_id, &resource.source_path, &descriptor)?;
            if descriptor.name != resource.local_id {
                return Err(BundleError::InvalidManifest {
                    source_name: bundle_id.to_string(),
                    detail: format!(
                        "tool descriptor `{}` declares name `{}` but manifest resource id is `{}`",
                        resource.source_path, descriptor.name, resource.local_id
                    ),
                });
            }
        }
    }
    Ok(prepared)
}

fn validate_tool_descriptor(
    bundle_id: &str,
    path: &str,
    descriptor: &SourceToolDescriptor,
) -> Result<(), BundleError> {
    let invalid = |detail: String| BundleError::InvalidManifest {
        source_name: bundle_id.to_string(),
        detail: format!("tool descriptor `{path}`: {detail}"),
    };
    if let Some(version) = descriptor.schema_version
        && version != 1
    {
        return Err(invalid(format!("unsupported schema_version `{version}`")));
    }
    if descriptor.name.trim().is_empty() {
        return Err(invalid("`name` must not be empty".to_string()));
    }
    if descriptor.description.trim().is_empty() {
        return Err(invalid("`description` must not be empty".to_string()));
    }
    for alias in &descriptor.aliases {
        match alias {
            SourceToolDescriptorAlias::Name(name) => {
                if name.trim().is_empty() {
                    return Err(invalid("alias name must not be empty".to_string()));
                }
            }
            SourceToolDescriptorAlias::Detailed { name, visibility } => {
                if name.trim().is_empty() {
                    return Err(invalid("alias name must not be empty".to_string()));
                }
                if let Some(vis) = visibility
                    && !matches!(vis.as_str(), "public" | "hidden")
                {
                    return Err(invalid(format!(
                        "alias `{name}` has invalid visibility `{vis}`; expected `public` or `hidden`"
                    )));
                }
            }
        }
    }
    if let Some(parameters) = &descriptor.parameters
        && parameters.get("type").and_then(serde_json::Value::as_str) != Some("object")
    {
        return Err(invalid(
            "`parameters` must be a JSON Schema object with `\"type\": \"object\"`".to_string(),
        ));
    }
    if let Some(entrypoint) = &descriptor.entrypoint
        && let Some(js) = &entrypoint.js
    {
        let module_path = js.split('#').next().unwrap_or("");
        if module_path.is_empty() || normalize_source_path(bundle_id, module_path).is_err() {
            return Err(invalid(format!(
                "entrypoint.js `{js}` must begin with a valid relative path"
            )));
        }
    }
    Ok(())
}

fn prepare_hook_resources(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    resources: Vec<SourceResource>,
) -> Result<Vec<PreparedResource>, BundleError> {
    let prepared = prepare_resources(bundle_id, "hook", files, resources)?;
    for resource in &prepared {
        if !is_descriptor_path(&resource.source_path) {
            continue;
        }
        let value =
            parse_descriptor_value(bundle_id, &resource.source_path, &resource.content, "hook")?;
        let Some(object) = value.as_object() else {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "hook descriptor `{}` must be a JSON/YAML object",
                    resource.source_path
                ),
            });
        };
        // Legacy `{}` placeholder used by v1 process declarations.
        if object.is_empty() {
            continue;
        }
        let descriptor: SourceHooksDescriptor =
            serde_json::from_value(value).map_err(|error| BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "hook descriptor `{}` is invalid: {error}",
                    resource.source_path
                ),
            })?;
        let invalid = |detail: String| BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: format!("hook descriptor `{}`: {detail}", resource.source_path),
        };
        if let Some(version) = descriptor.schema_version
            && version != 1
        {
            return Err(invalid(format!("unsupported schema_version `{version}`")));
        }
        if descriptor.hooks.is_empty() {
            return Err(invalid(
                "`hooks` must contain at least one entry".to_string(),
            ));
        }
        let mut matched_event = false;
        for entry in &descriptor.hooks {
            validate_hook_local_id(bundle_id, &entry.event)?;
            if entry.event == resource.local_id {
                matched_event = true;
            }
            if let Some(matcher) = &entry.matcher
                && matcher.trim().is_empty()
            {
                return Err(invalid("`matcher` must not be blank".to_string()));
            }
            if entry.event == "tool.execute.before"
                && entry.failure_mode == Some(SourceHookFailureMode::FailOpen)
            {
                return Err(invalid(
                    "`tool.execute.before` hooks must be fail_closed".to_string(),
                ));
            }
            if let Some(timeout) = entry.timeout_ms
                && !(1..=3_600_000).contains(&timeout)
            {
                return Err(invalid(format!(
                    "timeout_ms must be 1..=3600000 (got {timeout})"
                )));
            }
            match &entry.handler {
                SourceHookHandler::Js { target } => {
                    let module_path = target.split('#').next().unwrap_or("");
                    if target.trim().is_empty()
                        || module_path.is_empty()
                        || normalize_source_path(bundle_id, module_path).is_err()
                    {
                        return Err(invalid(format!(
                            "js handler target `{target}` must begin with a valid relative path"
                        )));
                    }
                }
                SourceHookHandler::Rpc { method } => {
                    if method.trim().is_empty() {
                        return Err(invalid(
                            "rpc handler `method` must not be empty".to_string(),
                        ));
                    }
                }
                SourceHookHandler::Command { command } => match command {
                    SourceHookCommand::Shell(cmd) if cmd.trim().is_empty() => {
                        return Err(invalid(
                            "command handler `command` must not be empty".to_string(),
                        ));
                    }
                    SourceHookCommand::Argv(argv)
                        if argv.is_empty() || argv.iter().any(|arg| arg.trim().is_empty()) =>
                    {
                        return Err(invalid(
                            "command handler `command` argv must be non-empty without blank entries"
                                .to_string(),
                        ));
                    }
                    _ => {}
                },
                SourceHookHandler::Native { target } => {
                    if target.trim().is_empty() {
                        return Err(invalid(
                            "native handler `target` must not be empty".to_string(),
                        ));
                    }
                }
            }
        }
        if !matched_event {
            return Err(invalid(format!(
                "does not contain a hook entry for declared event `{}`",
                resource.local_id
            )));
        }
    }
    Ok(prepared)
}

fn prepare_command_resources(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    resources: Vec<SourceResource>,
) -> Result<Vec<PreparedResource>, BundleError> {
    let prepared = prepare_resources(bundle_id, "command", files, resources)?;
    for resource in &prepared {
        if resource.content.trim().is_empty() {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!("command `{}` must not be empty", resource.source_path),
            });
        }
        if resource.content.len() > MAX_COMMAND_BYTES {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "command `{}` exceeds maximum size of {MAX_COMMAND_BYTES} bytes",
                    resource.source_path
                ),
            });
        }
        if let Some((frontmatter, _body)) = split_markdown(&resource.content) {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct CommandFrontmatter {
                #[allow(dead_code)]
                #[serde(default)]
                name: Option<String>,
                #[allow(dead_code)]
                #[serde(default)]
                description: Option<String>,
                #[allow(dead_code)]
                #[serde(default)]
                hints: Option<serde_json::Value>,
                #[allow(dead_code)]
                #[serde(default)]
                subtask: Option<bool>,
            }
            let parsed: CommandFrontmatter =
                serde_norway::from_str(frontmatter).map_err(|error| {
                    BundleError::InvalidManifest {
                        source_name: bundle_id.to_string(),
                        detail: format!(
                            "command `{}` has invalid YAML frontmatter: {error}",
                            resource.source_path
                        ),
                    }
                })?;
            if let Some(hints) = parsed.hints {
                let valid_hints = hints.is_string()
                    || hints
                        .as_array()
                        .is_some_and(|items| items.iter().all(serde_json::Value::is_string));
                if !valid_hints {
                    return Err(BundleError::InvalidManifest {
                        source_name: bundle_id.to_string(),
                        detail: format!(
                            "command `{}` frontmatter `hints` must be a string or list of strings",
                            resource.source_path
                        ),
                    });
                }
            }
        }
    }
    Ok(prepared)
}

/// Prepare `resources.mcp` declarations: each entry names a JSON file that
/// must parse into hya's `McpServerConfig` shape (stdio command or remote
/// url with optional headers). The bytes are retained verbatim.
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
                         `headers`/`transport`/`enabled`/`timeout_ms` fields: {error}",
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
        if has_command && has_url {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "MCP declaration `{}` must declare either `command` or `url`, not both",
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
        if has_url {
            let url = server.url.as_deref().unwrap_or_default();
            if !(url.starts_with("http://") || url.starts_with("https://"))
                || url.chars().any(char::is_whitespace)
            {
                return Err(BundleError::InvalidManifest {
                    source_name: bundle_id.to_string(),
                    detail: format!(
                        "MCP declaration `{}` url must be a valid http:// or https:// URL",
                        resource.source_path
                    ),
                });
            }
        }
        if server.headers.is_some() && !has_url {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "MCP declaration `{}` `headers` is only valid with a remote `url`",
                    resource.source_path
                ),
            });
        }
        if let Some(transport) = server.transport.as_deref() {
            let valid = if has_command {
                transport == "stdio"
            } else {
                matches!(transport, "http" | "sse" | "streamable_http")
            };
            if !valid {
                return Err(BundleError::InvalidManifest {
                    source_name: bundle_id.to_string(),
                    detail: format!(
                        "MCP declaration `{}` has incompatible transport `{transport}`",
                        resource.source_path
                    ),
                });
            }
        }
        if server.timeout_ms == Some(0) {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "MCP declaration `{}` timeout_ms must be greater than 0",
                    resource.source_path
                ),
            });
        }
    }
    Ok(prepared)
}

/// Resolve one manifest `agents:` entry (inline declaration or standalone
/// `agents/<id>.md` Markdown frontmatter file) into a normalized `SourceAgent`
/// plus an optional `(body, source_path)` when defined by a Markdown file.
pub(super) fn resolve_source_agent_entry(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    entry: SourceAgentEntry,
) -> Result<(SourceAgent, Option<(String, String)>), BundleError> {
    if let Some(ref_path) = entry.path {
        let has_inline_fields = entry.id.is_some()
            || entry.description.is_some()
            || entry.role.is_some()
            || entry.color.is_some()
            || entry.prompt.is_some()
            || entry.model_policy != crate::model::ModelPolicy::default()
            || entry.workdir.is_some()
            || entry.spawn_lifecycle.is_some()
            || entry.resource_profile.is_some()
            || entry.resource_view != crate::model::ResourceView::default()
            || !entry.can_spawn.is_empty()
            || entry.private
            || !entry.advertise.is_empty()
            || entry.handoff != crate::model::HandoffPolicy::Model
            || !entry.hook_refs.is_empty()
            || entry.harness_access.is_some();
        if has_inline_fields {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "agent file reference `{ref_path}` must not also declare inline agent fields"
                ),
            });
        }
        let path = normalize_source_path(bundle_id, &ref_path)?;
        let Some(bytes) = files.get(&path) else {
            return Err(BundleError::MissingReference {
                bundle_id: bundle_id.to_string(),
                path,
            });
        };
        let text = std::str::from_utf8(bytes).map_err(|error| BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: format!("agent file `{path}` is not UTF-8: {error}"),
        })?;
        let (frontmatter, body) =
            split_markdown(text).ok_or_else(|| BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!("agent file `{path}` requires YAML frontmatter"),
            })?;
        let file_agent: SourceAgentFile =
            serde_norway::from_str(frontmatter).map_err(|error| BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!("agent file `{path}` has invalid frontmatter: {error}"),
            })?;
        if file_agent.prompt.is_some() {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "agent file `{path}` cannot declare `prompt`; put prompt text in the Markdown body"
                ),
            });
        }
        let id = match (file_agent.id, file_agent.name) {
            (Some(id), Some(name)) if id != name => {
                return Err(BundleError::InvalidManifest {
                    source_name: bundle_id.to_string(),
                    detail: format!(
                        "agent file `{path}` declares conflicting `id` (`{id}`) and `name` (`{name}`)"
                    ),
                });
            }
            (Some(id), _) | (None, Some(id)) => id,
            (None, None) => {
                let filename = path.rsplit('/').next().unwrap_or("");
                let stem = filename.strip_suffix(".md").unwrap_or(filename);
                if stem.trim().is_empty() {
                    return Err(BundleError::InvalidManifest {
                        source_name: bundle_id.to_string(),
                        detail: format!("agent file `{path}` requires an `id` or `name`"),
                    });
                }
                stem.to_string()
            }
        };
        let agent = SourceAgent {
            id,
            description: file_agent.description,
            role: file_agent.role.unwrap_or(AgentRole::Subagent),
            color: file_agent.color,
            prompt: None,
            model_policy: file_agent.model_policy,
            workdir: file_agent.workdir,
            spawn_lifecycle: file_agent.spawn_lifecycle,
            resource_profile: file_agent.resource_profile,
            resource_view: file_agent.resource_view,
            can_spawn: file_agent.can_spawn,
            private: file_agent.private,
            advertise: file_agent.advertise,
            handoff: file_agent.handoff,
            hook_refs: file_agent.hook_refs,
            harness_access: file_agent.harness_access,
        };
        return Ok((agent, Some((body.trim().to_string(), path))));
    }
    let id = entry.id.ok_or_else(|| BundleError::InvalidManifest {
        source_name: bundle_id.to_string(),
        detail: "inline agent declaration requires `id` (or `path` for an agent file)".to_string(),
    })?;
    let role = entry.role.ok_or_else(|| BundleError::InvalidManifest {
        source_name: bundle_id.to_string(),
        detail: format!("inline agent `{id}` requires `role`"),
    })?;
    Ok((
        SourceAgent {
            id,
            description: entry.description,
            role,
            color: entry.color,
            prompt: entry.prompt,
            model_policy: entry.model_policy,
            workdir: entry.workdir,
            spawn_lifecycle: entry.spawn_lifecycle,
            resource_profile: entry.resource_profile,
            resource_view: entry.resource_view,
            can_spawn: entry.can_spawn,
            private: entry.private,
            advertise: entry.advertise,
            handoff: entry.handoff,
            hook_refs: entry.hook_refs,
            harness_access: entry.harness_access,
        },
        None,
    ))
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
    markdown_prompt: Option<(&str, &str)>,
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
        (None, Some((body, source_path))) => {
            (Some(body.to_string()), Some(source_path.to_string()))
        }
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
