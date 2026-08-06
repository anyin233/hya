use std::collections::{BTreeMap, BTreeSet};

use hya_proto::AgentName;
use sha2::{Digest, Sha256};

use crate::error::BundleError;
use crate::model::{
    BundleOrigin, PreparedAgent, PreparedBundle, PreparedBundleIndex, PreparedCatalog,
    PreparedDocument, PreparedDocumentOwned, PreparedResource,
};
use crate::source::{
    BundleSource, ParsedSource, SourceAgent, SourceExtensions, SourceFile, SourceManifest,
    SourceResource, SourceResources,
};

const SOURCE_API_VERSION: &str = "hya.agent-bundle/v1";
const SOURCE_KIND: &str = "AgentBundle";
const PREPARED_FORMAT_VERSION: u32 = 1;

/// Validate and deterministically prepare repo-native built-in Bundle sources.
///
/// Marks every resulting bundle `origin = Builtin` and `immutable = true`.
/// Callers pass one [`BundleSource`] per built-in directory (or in-memory
/// equivalent). Failures leave no on-disk state — prepare is pure over the
/// source bytes.
pub fn prepare_builtins(sources: Vec<BundleSource>) -> Result<PreparedCatalog, BundleError> {
    prepare_sources(sources, BundleOrigin::Builtin, true)
}

/// Validate and deterministically prepare one installable package source.
///
/// Marks the resulting bundle(s) `origin = Installed` and `immutable = false`.
/// Used after a public `.hyabundle` has been expanded into a [`BundleSource`]
/// (or for directory install paths). Like [`prepare_builtins`], this is pure
/// over source bytes: a failed prepare does not leave staging directories behind
/// (staging is owned by [`crate::stage_package`]).
///
/// Returns a [`PreparedCatalog`] with canonical JSON `bytes` and matching
/// `digest` for durable registry storage.
pub fn prepare_package(source: BundleSource) -> Result<PreparedCatalog, BundleError> {
    prepare_sources(vec![source], BundleOrigin::Installed, false)
}

fn prepare_sources(
    sources: Vec<BundleSource>,
    origin: BundleOrigin,
    immutable: bool,
) -> Result<PreparedCatalog, BundleError> {
    let mut parsed = sources
        .into_iter()
        .map(parse_source)
        .collect::<Result<Vec<_>, _>>()?;
    parsed.sort_by(|left, right| left.manifest.identity.id.cmp(&right.manifest.identity.id));

    let mut bundle_ids = BTreeSet::new();
    let mut stable_agent_ids = BTreeSet::new();
    let mut bundles = Vec::with_capacity(parsed.len());
    for source in parsed {
        let bundle_id = source.manifest.identity.id.clone();
        if !bundle_ids.insert(bundle_id.clone()) {
            return Err(BundleError::DuplicateBundleId { bundle_id });
        }
        let bundle = prepare_bundle(source, &mut stable_agent_ids, origin, immutable)?;
        bundles.push(bundle);
    }
    resolve_catalog_references(&mut bundles)?;
    validate_prepared_references(&bundles)?;

    let index = build_index(&bundles);
    let bytes = serde_json::to_vec(&PreparedDocument {
        format_version: PREPARED_FORMAT_VERSION,
        bundles: &bundles,
        index: &index,
    })
    .map_err(|error| BundleError::PreparedEncode {
        detail: error.to_string(),
    })?;
    let digest = digest_bytes(&bytes);
    Ok(PreparedCatalog {
        bundles,
        index,
        bytes,
        digest,
    })
}

impl PreparedCatalog {
    /// Decode canonical prepared bytes and verify both the catalog digest and
    /// every embedded bundle/index digest relationship.
    pub fn decode(bytes: &[u8], expected_digest: &str) -> Result<Self, BundleError> {
        let actual_digest = digest_bytes(bytes);
        if actual_digest != expected_digest {
            return Err(BundleError::PreparedDigestMismatch {
                expected: expected_digest.to_string(),
                actual: actual_digest,
            });
        }
        let document: PreparedDocumentOwned =
            serde_json::from_slice(bytes).map_err(|error| BundleError::PreparedDecode {
                detail: error.to_string(),
            })?;
        if document.format_version != PREPARED_FORMAT_VERSION
            || !is_strictly_sorted(
                document
                    .bundles
                    .iter()
                    .map(|bundle| bundle.identity.id.as_str()),
            )
        {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
        for bundle in &document.bundles {
            if !prepared_bundle_is_canonical(bundle) {
                return Err(BundleError::NonCanonicalPreparedCatalog);
            }
            validate_prepared_content_digests(bundle)?;
            if prepared_bundle_digest(bundle)? != bundle.digest {
                return Err(BundleError::PreparedBundleDigestMismatch {
                    bundle_id: bundle.identity.id.clone(),
                });
            }
        }
        validate_prepared_references(&document.bundles)?;
        let expected_index = build_index(&document.bundles);
        if expected_index != document.index {
            return Err(BundleError::PreparedIndexMismatch);
        }
        Ok(Self {
            bundles: document.bundles,
            index: document.index,
            bytes: bytes.to_vec(),
            digest: expected_digest.to_string(),
        })
    }
}

fn prepared_bundle_is_canonical(bundle: &PreparedBundle) -> bool {
    bundle.format_version == PREPARED_FORMAT_VERSION
        && matches!(
            (bundle.origin, bundle.immutable),
            (BundleOrigin::Builtin, true) | (BundleOrigin::Installed, false)
        )
        && validate_identity(&bundle.identity.id, &bundle.identity.version).is_ok()
        && is_strictly_sorted(bundle.agents.iter().map(|agent| agent.stable_id.as_str()))
        && bundle.agents.iter().all(|agent| {
            is_strictly_sorted(agent.can_spawn.iter().map(|target| target.as_str()))
                && is_strictly_sorted(agent.hook_refs.iter().map(String::as_str))
                && is_strictly_sorted(agent.resource_view.allow.iter().map(String::as_str))
                && is_strictly_sorted(agent.resource_view.deny.iter().map(String::as_str))
        })
        && bundle.mcp.is_empty()
        && resources_are_canonical(&bundle.identity.id, "tool", &bundle.tools)
        && resources_are_canonical(&bundle.identity.id, "skill", &bundle.skills)
        && resources_are_canonical(&bundle.identity.id, "mcp", &bundle.mcp)
        && resources_are_canonical(&bundle.identity.id, "hook", &bundle.hooks)
        && resources_are_canonical(&bundle.identity.id, "extension", &bundle.extensions)
}

fn validate_prepared_references(bundles: &[PreparedBundle]) -> Result<(), BundleError> {
    let mut stable_agents = BTreeSet::new();
    let mut agent_references = BTreeSet::new();
    let mut resources = BTreeSet::new();
    for bundle in bundles {
        for agent in &bundle.agents {
            if !stable_agents.insert(agent.stable_id.as_str()) {
                return Err(BundleError::DuplicateStableAgentId {
                    stable_id: agent.stable_id.as_str().to_string(),
                });
            }
            for reference in [
                agent.stable_id.as_str().to_string(),
                format!("bundle:{}/agent/{}", bundle.identity.id, agent.local_id),
            ] {
                if !agent_references.insert(reference.clone()) {
                    return Err(BundleError::NamespaceCollision {
                        bundle_id: bundle.identity.id.clone(),
                        name: reference,
                    });
                }
            }
        }
        for hook in &bundle.hooks {
            validate_hook_local_id(&bundle.identity.id, &hook.local_id)?;
        }
        for resource in bundle
            .tools
            .iter()
            .chain(&bundle.skills)
            .chain(&bundle.mcp)
            .chain(&bundle.hooks)
            .chain(&bundle.extensions)
        {
            if !resources.insert(resource.stable_id.as_str()) {
                return Err(BundleError::NamespaceCollision {
                    bundle_id: bundle.identity.id.clone(),
                    name: resource.stable_id.clone(),
                });
            }
        }
    }

    for bundle in bundles {
        for agent in &bundle.agents {
            for reference in &agent.can_spawn {
                if !stable_agents.contains(reference.as_str()) {
                    return Err(BundleError::UnknownAgentReference {
                        bundle_id: bundle.identity.id.clone(),
                        agent_id: agent.stable_id.as_str().to_string(),
                        reference: reference.as_str().to_string(),
                    });
                }
            }
            for reference in agent
                .resource_view
                .aliases
                .values()
                .chain(&agent.resource_view.allow)
                .chain(&agent.resource_view.deny)
            {
                validate_prepared_resource_reference(&bundle.identity.id, reference, &resources)?;
            }
            for reference in &agent.hook_refs {
                validate_prepared_resource_reference(&bundle.identity.id, reference, &resources)?;
                if !reference.contains("/hook/") {
                    return Err(BundleError::UnknownResourceReference {
                        bundle_id: bundle.identity.id.clone(),
                        kind: "hook".to_string(),
                        reference: reference.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn validate_prepared_resource_reference(
    bundle_id: &str,
    reference: &str,
    resources: &BTreeSet<&str>,
) -> Result<(), BundleError> {
    let harness_reference = ["tool", "skill", "mcp"]
        .iter()
        .any(|kind| reference.starts_with(&format!("harness:{kind}/")));
    if harness_reference || resources.contains(reference) {
        return Ok(());
    }
    Err(BundleError::UnknownResourceReference {
        bundle_id: bundle_id.to_string(),
        kind: "resource".to_string(),
        reference: reference.to_string(),
    })
}

fn resources_are_canonical(bundle_id: &str, kind: &str, resources: &[PreparedResource]) -> bool {
    is_strictly_sorted(resources.iter().map(|resource| resource.local_id.as_str()))
        && resources.iter().all(|resource| {
            resource.stable_id == format!("bundle:{bundle_id}/{kind}/{}", resource.local_id)
                && is_strictly_sorted(resource.aliases.iter().map(String::as_str))
        })
}

fn validate_prepared_content_digests(bundle: &PreparedBundle) -> Result<(), BundleError> {
    for agent in &bundle.agents {
        match (
            agent.prompt.as_deref(),
            agent.prompt_source.as_deref(),
            agent.prompt_digest.as_deref(),
        ) {
            (Some(prompt), Some(source), Some(digest)) => {
                if normalize_source_path(&bundle.identity.id, source).as_deref() != Ok(source) {
                    return Err(BundleError::NonCanonicalPreparedCatalog);
                }
                if digest != digest_bytes(prompt.as_bytes()) {
                    return Err(BundleError::PreparedContentDigestMismatch {
                        bundle_id: bundle.identity.id.clone(),
                        source_path: source.to_string(),
                    });
                }
            }
            (None, None, None) => {}
            _ => return Err(BundleError::NonCanonicalPreparedCatalog),
        }
    }
    for resource in bundle
        .tools
        .iter()
        .chain(&bundle.skills)
        .chain(&bundle.mcp)
        .chain(&bundle.hooks)
        .chain(&bundle.extensions)
    {
        if normalize_source_path(&bundle.identity.id, &resource.source_path).as_deref()
            != Ok(resource.source_path.as_str())
        {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
        if resource.digest != digest_bytes(resource.content.as_bytes()) {
            return Err(BundleError::PreparedContentDigestMismatch {
                bundle_id: bundle.identity.id.clone(),
                source_path: resource.source_path.clone(),
            });
        }
    }
    Ok(())
}

fn resolve_catalog_references(bundles: &mut [PreparedBundle]) -> Result<(), BundleError> {
    let mut agents = BTreeMap::new();
    let mut resources = BTreeSet::new();
    let mut hook_resources = BTreeMap::new();
    let mut local_resources: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for bundle in bundles.iter() {
        for hook in &bundle.hooks {
            hook_resources.insert(hook.stable_id.clone(), hook.local_id.clone());
        }
        for agent in &bundle.agents {
            for reference in [
                agent.stable_id.as_str().to_string(),
                format!("bundle:{}/agent/{}", bundle.identity.id, agent.local_id),
            ] {
                if agents
                    .insert(reference.clone(), agent.stable_id.clone())
                    .is_some()
                {
                    return Err(BundleError::NamespaceCollision {
                        bundle_id: bundle.identity.id.clone(),
                        name: reference,
                    });
                }
            }
        }
        for resource in bundle
            .tools
            .iter()
            .chain(&bundle.skills)
            .chain(&bundle.mcp)
            .chain(&bundle.hooks)
            .chain(&bundle.extensions)
        {
            resources.insert(resource.stable_id.clone());
            for name in std::iter::once(resource.local_id.as_str())
                .chain(resource.aliases.iter().map(String::as_str))
            {
                local_resources
                    .entry((bundle.identity.id.clone(), name.to_string()))
                    .or_default()
                    .insert(resource.stable_id.clone());
            }
        }
    }
    for bundle in bundles.iter_mut() {
        for agent in &mut bundle.agents {
            let mut resolved = Vec::with_capacity(agent.can_spawn.len());
            for reference in &agent.can_spawn {
                let Some(target) = agents.get(reference.as_str()) else {
                    return Err(BundleError::UnknownAgentReference {
                        bundle_id: bundle.identity.id.clone(),
                        agent_id: agent.stable_id.as_str().to_string(),
                        reference: reference.as_str().to_string(),
                    });
                };
                resolved.push(target.clone());
            }
            resolved.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            resolved.dedup_by(|left, right| left.as_str() == right.as_str());
            agent.can_spawn = resolved;

            for target in agent.resource_view.aliases.values_mut() {
                *target = resolve_resource_reference(
                    &bundle.identity.id,
                    target,
                    &resources,
                    &local_resources,
                )?;
            }
            agent.resource_view.allow = agent
                .resource_view
                .allow
                .iter()
                .map(|reference| {
                    resolve_resource_reference(
                        &bundle.identity.id,
                        reference,
                        &resources,
                        &local_resources,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            agent.resource_view.allow.sort();
            agent.resource_view.allow.dedup();
            agent.resource_view.deny = agent
                .resource_view
                .deny
                .iter()
                .map(|reference| {
                    resolve_resource_reference(
                        &bundle.identity.id,
                        reference,
                        &resources,
                        &local_resources,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            agent.resource_view.deny.sort();
            agent.resource_view.deny.dedup();
            let mut hook_refs = Vec::with_capacity(agent.hook_refs.len());
            for reference in &agent.hook_refs {
                let resolved = match resolve_resource_reference(
                    &bundle.identity.id,
                    reference,
                    &resources,
                    &local_resources,
                ) {
                    Ok(resolved) => resolved,
                    Err(BundleError::UnknownResourceReference { .. }) => {
                        return Err(BundleError::UnknownResourceReference {
                            bundle_id: bundle.identity.id.clone(),
                            kind: "hook".to_string(),
                            reference: reference.clone(),
                        });
                    }
                    Err(error) => return Err(error),
                };
                let Some(local_id) = hook_resources.get(&resolved) else {
                    return Err(BundleError::UnknownResourceReference {
                        bundle_id: bundle.identity.id.clone(),
                        kind: "hook".to_string(),
                        reference: reference.clone(),
                    });
                };
                validate_hook_local_id(&bundle.identity.id, local_id)?;
                hook_refs.push(resolved);
            }
            hook_refs.sort();
            if let Some(duplicate) = hook_refs
                .windows(2)
                .find_map(|window| (window[0] == window[1]).then_some(window[0].clone()))
            {
                return Err(BundleError::AliasCollision {
                    bundle_id: bundle.identity.id.clone(),
                    name: duplicate,
                });
            }
            agent.hook_refs = hook_refs;
        }
        bundle.digest = prepared_bundle_digest(bundle)?;
    }
    Ok(())
}

fn resolve_resource_reference(
    bundle_id: &str,
    reference: &str,
    resources: &BTreeSet<String>,
    local_resources: &BTreeMap<(String, String), BTreeSet<String>>,
) -> Result<String, BundleError> {
    if reference.starts_with("harness:") {
        let valid = ["tool", "skill", "mcp"]
            .iter()
            .any(|kind| reference.starts_with(&format!("harness:{kind}/")));
        if valid {
            return Ok(reference.to_string());
        }
    } else if reference.starts_with("bundle:") {
        if resources.contains(reference) {
            return Ok(reference.to_string());
        }
    } else if let Some(candidates) =
        local_resources.get(&(bundle_id.to_string(), reference.to_string()))
    {
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

fn parse_source(source: BundleSource) -> Result<ParsedSource, BundleError> {
    let (name, source_files) = source.into_parts();
    let files = collect_files(&name, source_files)?;
    let yaml = files.get("bundle.yaml");
    let markdown = files.get("bundle.hya.md");
    let (manifest_bytes, markdown_prompt) = match (yaml, markdown) {
        (Some(_), Some(_)) => {
            return Err(BundleError::InvalidManifest {
                source_name: name,
                detail: "source contains both bundle.yaml and bundle.hya.md".to_string(),
            });
        }
        (Some(bytes), None) => (bytes.as_slice(), None),
        (None, Some(bytes)) => {
            let text =
                std::str::from_utf8(bytes).map_err(|error| BundleError::InvalidManifest {
                    source_name: name.clone(),
                    detail: format!("bundle.hya.md is not UTF-8: {error}"),
                })?;
            let (frontmatter, body) =
                split_markdown(text).ok_or_else(|| BundleError::InvalidManifest {
                    source_name: name.clone(),
                    detail: "bundle.hya.md requires YAML frontmatter".to_string(),
                })?;
            (frontmatter.as_bytes(), Some(body.trim().to_string()))
        }
        (None, None) => {
            return Err(BundleError::UnsupportedSource { source_name: name });
        }
    };

    let manifest: SourceManifest =
        serde_norway::from_slice(manifest_bytes).map_err(|error| BundleError::InvalidManifest {
            source_name: name.clone(),
            detail: error.to_string(),
        })?;
    if manifest.api_version != SOURCE_API_VERSION {
        return Err(BundleError::WrongApiVersion {
            source_name: name,
            found: manifest.api_version,
        });
    }
    if manifest.kind != SOURCE_KIND {
        return Err(BundleError::WrongKind {
            source_name: name,
            found: manifest.kind,
        });
    }
    let markdown_prompt = if markdown_prompt.as_deref() == Some("")
        && !manifest.agents.is_empty()
        && manifest.agents.iter().all(|agent| agent.prompt.is_some())
    {
        None
    } else {
        markdown_prompt
    };
    if markdown_prompt.is_some()
        && (manifest.agents.len() != 1 || manifest.agents[0].prompt.is_some())
    {
        return Err(BundleError::InvalidManifest {
            source_name: name,
            detail: "bundle.hya.md requires exactly one agent and uses its body as prompt"
                .to_string(),
        });
    }
    Ok(ParsedSource {
        files,
        manifest,
        markdown_prompt,
    })
}

fn collect_files(
    source_name: &str,
    files: Vec<SourceFile>,
) -> Result<BTreeMap<String, Vec<u8>>, BundleError> {
    let mut sorted = BTreeMap::new();
    for file in files {
        let (path, bytes) = file.into_parts();
        let path = normalize_source_path(source_name, &path)?;
        if sorted.insert(path.clone(), bytes).is_some() {
            return Err(BundleError::DuplicateSourcePath {
                source_name: source_name.to_string(),
                path,
            });
        }
    }
    Ok(sorted)
}

fn normalize_source_path(source_name: &str, path: &str) -> Result<String, BundleError> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return Err(BundleError::InvalidSourcePath {
            source_name: source_name.to_string(),
            path: path.to_string(),
        });
    }
    let mut normalized = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(BundleError::InvalidSourcePath {
                    source_name: source_name.to_string(),
                    path: path.to_string(),
                });
            }
            value => normalized.push(value),
        }
    }
    if normalized.is_empty() {
        return Err(BundleError::InvalidSourcePath {
            source_name: source_name.to_string(),
            path: path.to_string(),
        });
    }
    Ok(normalized.join("/"))
}

fn split_markdown(content: &str) -> Option<(&str, &str)> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;
    let (frontmatter, body) = rest.split_once("\n---")?;
    Some((
        frontmatter.strip_suffix('\r').unwrap_or(frontmatter),
        body.strip_prefix("\r\n")
            .or_else(|| body.strip_prefix('\n'))
            .unwrap_or(body),
    ))
}

fn prepare_bundle(
    source: ParsedSource,
    stable_agent_ids: &mut BTreeSet<String>,
    origin: BundleOrigin,
    immutable: bool,
) -> Result<PreparedBundle, BundleError> {
    let bundle_id = source.manifest.identity.id.clone();
    validate_identity(&bundle_id, &source.manifest.identity.version)?;
    validate_unsupported(
        &bundle_id,
        &source.manifest.resources,
        &source.manifest.extensions,
    )?;

    let tools = prepare_resources(
        &bundle_id,
        "tool",
        &source.files,
        source.manifest.resources.tools,
    )?;
    let skills = prepare_resources(
        &bundle_id,
        "skill",
        &source.files,
        source.manifest.resources.skills,
    )?;
    let hooks = prepare_resources(
        &bundle_id,
        "hook",
        &source.files,
        source.manifest.resources.hooks,
    )?;
    for hook in &hooks {
        validate_hook_local_id(&bundle_id, &hook.local_id)?;
    }
    let extensions = prepare_resources(
        &bundle_id,
        "extension",
        &source.files,
        source.manifest.extensions.js,
    )?;
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
        match extension_path_counts
            .get(resource.source_path.as_str())
            .copied()
            .unwrap_or(0)
        {
            0 => {
                return Err(BundleError::UnsupportedBundleFeature {
                    bundle_id: bundle_id.clone(),
                    feature: format!("unmatched executable resource:{}", resource.stable_id),
                });
            }
            1 => {}
            _ => {
                return Err(BundleError::UnsupportedBundleFeature {
                    bundle_id: bundle_id.clone(),
                    feature: format!("ambiguous executable resource:{}", resource.stable_id),
                });
            }
        }
    }
    for extension in &extensions {
        if !selected_extension_paths.contains(extension.source_path.as_str()) {
            return Err(BundleError::UnsupportedBundleFeature {
                bundle_id: bundle_id.clone(),
                feature: format!("unreachable extension:{}", extension.stable_id),
            });
        }
    }
    let mut local_agent_ids = BTreeSet::new();
    let mut agents = source
        .manifest
        .agents
        .into_iter()
        .map(|agent| {
            prepare_agent(
                &bundle_id,
                &source.files,
                source.markdown_prompt.as_deref(),
                agent,
                &mut local_agent_ids,
                stable_agent_ids,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    agents.sort_by(|left, right| left.stable_id.as_str().cmp(right.stable_id.as_str()));
    validate_resource_views(&bundle_id, &agents, &tools, &skills)?;
    let mut bundle = PreparedBundle {
        format_version: PREPARED_FORMAT_VERSION,
        identity: source.manifest.identity,
        origin,
        immutable,
        digest: String::new(),
        agents,
        tools,
        skills,
        mcp: Vec::new(),
        hooks,
        extensions,
    };
    bundle.digest = prepared_bundle_digest(&bundle)?;
    Ok(bundle)
}

fn validate_resource_views(
    bundle_id: &str,
    agents: &[PreparedAgent],
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
    for agent in agents {
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
    }
    Ok(())
}

fn validate_identity(bundle_id: &str, version: &str) -> Result<(), BundleError> {
    let valid_id = bundle_id.contains('/')
        && bundle_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'));
    if !valid_id {
        return Err(BundleError::InvalidIdentity {
            bundle_id: bundle_id.to_string(),
            value: bundle_id.to_string(),
        });
    }
    if version.trim().is_empty() {
        return Err(BundleError::InvalidIdentity {
            bundle_id: bundle_id.to_string(),
            value: version.to_string(),
        });
    }
    Ok(())
}

fn validate_unsupported(
    bundle_id: &str,
    resources: &SourceResources,
    extensions: &SourceExtensions,
) -> Result<(), BundleError> {
    let unsupported = [
        (!resources.mcp.is_empty(), "resources.mcp"),
        (!extensions.rust.is_empty(), "extensions.rust"),
    ];
    if let Some((_, feature)) = unsupported.into_iter().find(|(present, _)| *present) {
        return Err(BundleError::UnsupportedBundleFeature {
            bundle_id: bundle_id.to_string(),
            feature: feature.to_string(),
        });
    }
    Ok(())
}

pub(crate) fn validate_hook_local_id(bundle_id: &str, local_id: &str) -> Result<(), BundleError> {
    if matches!(
        local_id,
        "event" | "tool.execute.before" | "tool.execute.after"
    ) {
        return Ok(());
    }
    Err(BundleError::UnsupportedBundleFeature {
        bundle_id: bundle_id.to_string(),
        feature: format!("hook:{local_id}"),
    })
}

fn prepare_agent(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    markdown_prompt: Option<&str>,
    mut source: SourceAgent,
    local_ids: &mut BTreeSet<String>,
    stable_ids: &mut BTreeSet<String>,
) -> Result<PreparedAgent, BundleError> {
    if source.resource_profile.is_some() {
        return Err(BundleError::UnsupportedBundleFeature {
            bundle_id: bundle_id.to_string(),
            feature: "agents[].resource_profile".to_string(),
        });
    }
    if !local_ids.insert(source.local_id.clone()) {
        return Err(BundleError::DuplicateLocalAgentId {
            bundle_id: bundle_id.to_string(),
            local_id: source.local_id,
        });
    }
    if !stable_ids.insert(source.stable_id.clone()) {
        return Err(BundleError::DuplicateStableAgentId {
            stable_id: source.stable_id,
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
                detail: "Markdown agent cannot also name a prompt resource".to_string(),
            });
        }
    };
    let prompt_digest = prompt.as_deref().map(|text| digest_bytes(text.as_bytes()));
    source.can_spawn.sort();
    source.can_spawn.dedup();
    source.hook_refs.sort();
    source.resource_view.allow.sort();
    source.resource_view.allow.dedup();
    source.resource_view.deny.sort();
    source.resource_view.deny.dedup();
    Ok(PreparedAgent {
        local_id: source.local_id,
        stable_id: AgentName::new(source.stable_id),
        description: source.description,
        role: source.role,
        color: source.color,
        prompt,
        prompt_source,
        prompt_digest,
        model_policy: source.model_policy,
        workdir: source.workdir,
        spawn_lifecycle: source.spawn_lifecycle,
        harness_access: source.harness_access,
        resource_view: source.resource_view,
        can_spawn: source.can_spawn.into_iter().map(AgentName::new).collect(),
        hook_refs: source.hook_refs,
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
            aliases: resource.aliases,
        });
    }
    prepared.sort_by(|left, right| left.local_id.cmp(&right.local_id));
    Ok(prepared)
}

fn read_text_reference(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    path: &str,
) -> Result<String, BundleError> {
    let Some(bytes) = files.get(path) else {
        return Err(BundleError::MissingReference {
            bundle_id: bundle_id.to_string(),
            path: path.to_string(),
        });
    };
    let text = std::str::from_utf8(bytes).map_err(|error| BundleError::InvalidManifest {
        source_name: bundle_id.to_string(),
        detail: format!("prompt `{path}` is not UTF-8: {error}"),
    })?;
    Ok(text.trim_end().to_string())
}

fn build_index(bundles: &[PreparedBundle]) -> Vec<PreparedBundleIndex> {
    bundles
        .iter()
        .map(|bundle| PreparedBundleIndex {
            bundle_id: bundle.identity.id.clone(),
            version: bundle.identity.version.clone(),
            digest: bundle.digest.clone(),
            stable_agent_ids: bundle
                .agents
                .iter()
                .map(|agent| agent.stable_id.clone())
                .collect(),
        })
        .collect()
}

fn prepared_bundle_digest(bundle: &PreparedBundle) -> Result<String, BundleError> {
    let mut value = serde_json::to_value(bundle).map_err(|error| BundleError::PreparedEncode {
        detail: error.to_string(),
    })?;
    let Some(fields) = value.as_object_mut() else {
        return Err(BundleError::PreparedEncode {
            detail: "prepared bundle did not encode as an object".to_string(),
        });
    };
    fields.remove("digest");
    let bytes = serde_json::to_vec(&value).map_err(|error| BundleError::PreparedEncode {
        detail: error.to_string(),
    })?;
    Ok(digest_bytes(&bytes))
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes).as_slice())
}

fn is_strictly_sorted<'a>(values: impl Iterator<Item = &'a str>) -> bool {
    let mut previous = None;
    for value in values {
        if previous.is_some_and(|previous| previous >= value) {
            return false;
        }
        previous = Some(value);
    }
    true
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}
