mod decode;
mod extensions;
mod parse;
mod resources;
#[cfg(test)]
mod tests;
mod workflow;

use std::collections::BTreeSet;

use hya_workflow::is_valid_identifier;

use self::decode::{digest_bytes, is_strictly_sorted, set_bundle_digest};
use self::extensions::{
    ResolvedRuntime, resolve_runtime, validate_declared_apis, validate_declared_permission_modes,
    validate_declared_schemas, validate_overrides,
};
use self::parse::{parse_source, resolve_namespace, split_markdown, validate_identity};
use self::resources::{
    PreparedResourceSets, prepare_agent, prepare_resource_sets, resolve_catalog_references,
    resolve_source_agent_entry, validate_resource_views,
};
use self::workflow::prepare_workflow;
use crate::error::BundleError;
use crate::model::{
    Bundle, ChannelParticipantRole, ChannelScope, ChannelTemplateKind, PreparedCatalog,
    PreparedChannelParticipant, PreparedChannelTemplate, PreparedCheck, PreparedResource,
    PreparedTuiExtension,
};
use crate::source::{BundleSource, ParsedSource, SourceTuiExtension};
pub(crate) use extensions::validate_hook_local_id;
pub(crate) use parse::normalize_source_path;

const PREPARED_FORMAT_VERSION: u32 = 4;

fn reject_removed_exposure(
    bundle_id: &str,
    files: &std::collections::BTreeMap<String, Vec<u8>>,
    assets: &[crate::source::SourceResource],
) -> Result<(), BundleError> {
    if let Some(path) = files
        .keys()
        .find(|path| *path == "exposure.yaml" || path.ends_with("/exposure.yaml"))
    {
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: format!(
                "`{path}` is not allowed: `exposure.yaml` was removed in hyabundle schema_version 2; declare tools under `resources.tools` instead"
            ),
        });
    }
    for asset in assets {
        let path_is_exposure = normalize_source_path(bundle_id, &asset.path)
            .ok()
            .is_some_and(|path| path == "exposure.yaml" || path.ends_with("/exposure.yaml"));
        if asset.id == "exposure"
            || asset.aliases.iter().any(|alias| alias == "exposure")
            || path_is_exposure
        {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: "`assets` entry `exposure` / `exposure.yaml` was removed in hyabundle schema_version 2; declare tools under `resources.tools` instead".to_string(),
            });
        }
    }
    Ok(())
}

/// Read manifest identity before preparing resources or starting providers.
pub fn read_bundle_identity(root: &std::path::Path) -> Result<(String, String), BundleError> {
    let yaml = root.join("bundle.yaml");
    let path = if yaml.exists() {
        yaml
    } else {
        root.join("bundle.hya.md")
    };
    let text = std::fs::read_to_string(&path).map_err(|error| BundleError::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    })?;
    let manifest_text = if path.extension().is_some_and(|ext| ext == "md") {
        split_markdown(&text)
            .ok_or_else(|| BundleError::InvalidManifest {
                source_name: path.display().to_string(),
                detail: "bundle.hya.md requires YAML frontmatter".into(),
            })?
            .0
    } else {
        &text
    };
    // Shadowing depends only on identity claims. Invalid declarations in an
    // ignored source must neither start a provider nor produce a refresh error.
    let invalid = |detail: String| BundleError::InvalidManifest {
        source_name: path.display().to_string(),
        detail,
    };
    let manifest: serde_json::Value =
        serde_norway::from_str(manifest_text).map_err(|error| invalid(error.to_string()))?;
    let kind = manifest["kind"]
        .as_str()
        .ok_or_else(|| invalid("kind must be a string".into()))?;
    if kind != "Bundle" {
        return Err(BundleError::WrongKind {
            source_name: path.display().to_string(),
            found: kind.to_string(),
        });
    }
    let id = manifest["identity"]["id"]
        .as_str()
        .ok_or_else(|| invalid("identity.id must be a string".into()))?;
    let namespace = match manifest.get("namespace") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) => Some(value.clone()),
        Some(_) => return Err(invalid("namespace must be a string".into())),
    };
    let namespace = resolve_namespace(id, id, &namespace)?;
    Ok((id.to_string(), namespace))
}
fn prepared_check(check: &crate::source::SourceCheck) -> PreparedCheck {
    PreparedCheck {
        command: check.command.clone(),
        timeout_secs: check.timeout_secs,
    }
}

/// Validate and deterministically prepare one installable package source.
///
/// Source parsing and Workflow compilation are build/install-time operations.
/// Runtime callers should decode the returned immutable prepared bytes instead
/// of reading source directories.
///
/// # Errors
///
/// Returns a typed error when the source, Workflow graph, Agent closure,
/// resources, or canonical prepared representation is invalid.
pub fn prepare_package(source: BundleSource) -> Result<PreparedCatalog, BundleError> {
    let source = parse_source(source)?;
    let mut bundle = prepare_bundle(source, &mut BTreeSet::new())?;
    resolve_catalog_references(std::slice::from_mut(&mut bundle))?;
    let bytes = serde_json::to_vec(&bundle).map_err(|error| BundleError::PreparedEncode {
        detail: error.to_string(),
    })?;
    let digest = digest_bytes(&bytes);
    // Decode through the same boundary validation as installed packages.
    PreparedCatalog::decode(&bytes, &digest)
}

fn prepare_bundle(
    source: ParsedSource,
    stable_agent_ids: &mut BTreeSet<String>,
) -> Result<Bundle, BundleError> {
    let ParsedSource {
        files,
        manifest,
        markdown_prompt,
    } = source;
    let check = manifest.check.as_ref().map(prepared_check);
    let bundle_id = manifest.identity.id.clone();
    validate_identity(&bundle_id, &manifest.identity.version)?;
    reject_removed_exposure(&bundle_id, &files, &manifest.assets)?;
    let namespace = Some(resolve_namespace(
        &bundle_id,
        &manifest.identity.id,
        &manifest.namespace,
    )?);
    let ResolvedRuntime {
        prepared: runtime,
        process,
        js_entry_resource,
        native_library_resource,
    } = resolve_runtime(&bundle_id, manifest.runtime.as_ref())?;
    let PreparedResourceSets {
        tools,
        skills,
        commands,
        mcp,
        hooks,
        extensions,
    } = prepare_resource_sets(
        &bundle_id,
        &files,
        manifest.resources,
        manifest.assets,
        manifest.overrides.as_deref(),
        runtime.as_ref(),
        process.as_ref(),
        js_entry_resource,
        native_library_resource,
    )?;
    validate_overrides(&bundle_id, manifest.overrides.as_deref(), &tools)?;
    let tui = prepare_tui(&bundle_id, manifest.tui.as_ref(), &extensions)?;
    let schemas = validate_declared_schemas(&bundle_id, &manifest.schemas, &tools)?;
    let apis = validate_declared_apis(&bundle_id, &manifest.apis, process.as_ref(), &extensions)?;
    let permission_modes = validate_declared_permission_modes(
        &bundle_id,
        &manifest.permission_modes,
        process.as_ref(),
        &hooks,
    )?;
    let mut source_agents = manifest
        .agents
        .into_iter()
        .map(|entry| resolve_source_agent_entry(&bundle_id, &files, entry))
        .collect::<Result<Vec<_>, _>>()?;
    source_agents.sort_by(|(left, _), (right, _)| left.id.cmp(&right.id));
    let mut agents = Vec::with_capacity(source_agents.len());
    let mut local_agent_ids = BTreeSet::new();
    for (source_agent, file_prompt) in source_agents {
        if manifest.workflow.is_some()
            && let Some(prompt) = &source_agent.prompt
            && !normalize_source_path(&bundle_id, prompt)?.starts_with("prompts/")
        {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.clone(),
                detail: "Workflow Agent prompt paths must be under `prompts/`".to_string(),
            });
        }
        if source_agent.harness_access.is_some() {
            return Err(BundleError::RemovedManifestKey {
                source_name: bundle_id.clone(),
                key: "harness_access".to_string(),
                guidance: "the tool plane is host-controlled".to_string(),
            });
        }
        if !local_agent_ids.insert(source_agent.id.clone()) {
            return Err(BundleError::NamespaceCollision {
                bundle_id: bundle_id.clone(),
                name: source_agent.id,
            });
        }
        let prompt_override = match (&file_prompt, markdown_prompt.as_deref()) {
            (Some((body, path)), None) => Some((body.as_str(), path.as_str())),
            (None, Some(body)) => Some((body, "bundle.hya.md")),
            (None, None) => None,
            (Some(_), Some(_)) => {
                return Err(BundleError::InvalidManifest {
                    source_name: bundle_id.clone(),
                    detail: "Markdown bundle body cannot combine with standalone agent files"
                        .to_string(),
                });
            }
        };
        agents.push(prepare_agent(
            &bundle_id,
            &files,
            prompt_override,
            source_agent,
            stable_agent_ids,
        )?);
    }
    agents.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    for agent in &agents {
        validate_resource_views(&bundle_id, agent, &tools, &skills)?;
    }
    let channels = prepare_channel_templates(&bundle_id, manifest.channels, &local_agent_ids)?;
    let workflow = manifest
        .workflow
        .map(|workflow| prepare_workflow(&bundle_id, &files, workflow, &agents))
        .transpose()?;
    let mut bundle = Bundle {
        format_version: PREPARED_FORMAT_VERSION,
        check,
        identity: manifest.identity,
        namespace,
        overrides: manifest.overrides,
        digest: String::new(),
        tui,
        agents,
        channels,
        tools,
        skills,
        commands,
        mcp,
        hooks,
        extensions,
        schemas: schemas.clone(),
        process: process.clone(),
        runtime,
        apis: apis.clone(),
        permission_modes: permission_modes.clone(),
        workflow,
    };
    set_bundle_digest(&mut bundle)?;
    Ok(bundle)
}

fn prepare_channel_templates(
    bundle_id: &str,
    mut channels: Vec<PreparedChannelTemplate>,
    local_agent_ids: &BTreeSet<String>,
) -> Result<Vec<PreparedChannelTemplate>, BundleError> {
    for channel in &mut channels {
        channel.participants.sort();
        channel.capabilities.sort();
    }
    channels.sort_by(|left, right| left.id.cmp(&right.id));
    if !channels_are_canonical(
        &channels,
        &local_agent_ids.iter().map(String::as_str).collect(),
    ) {
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: "invalid, duplicate, or unsupported channel template policy".to_string(),
        });
    }
    Ok(channels)
}

fn channels_are_canonical(
    channels: &[PreparedChannelTemplate],
    local_agent_ids: &BTreeSet<&str>,
) -> bool {
    let mut kinds = BTreeSet::new();
    is_strictly_sorted(channels.iter().map(|channel| channel.id.as_str()))
        && channels.iter().all(|channel| {
            kinds.insert(channel.kind)
                && is_valid_identifier(&channel.id)
                && !channel.participants.is_empty()
                && is_strictly_ordered(&channel.participants)
                && is_strictly_ordered(&channel.capabilities)
                && channel
                    .participants
                    .iter()
                    .all(|participant| match participant {
                        PreparedChannelParticipant::Agent { agent } => {
                            local_agent_ids.contains(agent.as_str())
                        }
                        PreparedChannelParticipant::Role { role } => match channel.kind {
                            ChannelTemplateKind::Unit => matches!(
                                role,
                                ChannelParticipantRole::UnitLeader
                                    | ChannelParticipantRole::DirectReports
                            ),
                            ChannelTemplateKind::ParentDm => {
                                matches!(
                                    role,
                                    ChannelParticipantRole::Parent | ChannelParticipantRole::Child
                                )
                            }
                        },
                    })
                && matches!(
                    (channel.kind, channel.scope),
                    (ChannelTemplateKind::Unit, ChannelScope::Unit)
                        | (ChannelTemplateKind::ParentDm, ChannelScope::Vertical)
                )
        })
}

fn is_strictly_ordered<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn prepare_tui(
    bundle_id: &str,
    source: Option<&SourceTuiExtension>,
    extensions: &[PreparedResource],
) -> Result<Option<PreparedTuiExtension>, BundleError> {
    let Some(source) = source else {
        return Ok(None);
    };
    let mut tui = PreparedTuiExtension {
        api_version: source.api_version,
        entry: normalize_source_path(bundle_id, &source.entry)?,
        sdk: source.sdk.clone(),
        permissions: source.permissions.clone(),
    };
    validate_tui(&tui, extensions).map_err(|detail| BundleError::InvalidManifest {
        source_name: bundle_id.to_string(),
        detail,
    })?;
    tui.permissions.sort();
    Ok(Some(tui))
}

fn validate_tui(tui: &PreparedTuiExtension, extensions: &[PreparedResource]) -> Result<(), String> {
    if tui.api_version != 2 {
        return Err("tui.api_version must be 2".to_string());
    }
    let parts = tui.sdk.split('.').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len())
        || parts[0] != "2"
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
        })
    {
        return Err("tui.sdk must be major.minor[.patch] with major 2".to_string());
    }
    if normalize_source_path("tui", &tui.entry).as_deref() != Ok(tui.entry.as_str())
        || tui.entry.contains(':')
        || ![".ts", ".js", ".mjs"]
            .iter()
            .any(|suffix| tui.entry.ends_with(suffix))
        || !extensions
            .iter()
            .any(|resource| resource.source_path == tui.entry && resource.binary_base64.is_none())
    {
        return Err("tui.entry must name a canonical TS/JS extension resource".to_string());
    }
    const PERMISSIONS: &[&str] = &[
        "fs.read",
        "tui.panel",
        "tui.render",
        "tui.status_item",
        "tui.session.read",
        "tui.transcript.read",
        "tui.transcript.embed",
        "tui.workspace.read",
        "workspace.git.read",
        "tui.action",
        "tui.sessions.read",
        "tui.projects.read",
        "tui.todos.read",
        "tui.status.read",
        "tui.keys",
        "tui.session.control",
        "tui.project.control",
        "tui.bundle_api",
        "tui.extension_state.read",
    ];
    let mut seen = BTreeSet::new();
    for permission in &tui.permissions {
        if permission.len() > 64
            || !PERMISSIONS.contains(&permission.as_str())
            || !seen.insert(permission)
        {
            return Err(format!(
                "tui.permissions: unknown or duplicate permission `{permission}`"
            ));
        }
    }
    Ok(())
}
