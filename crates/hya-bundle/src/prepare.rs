use std::collections::{BTreeMap, BTreeSet};

use hya_proto::AgentName;
use hya_workflow::{WorkflowSource, compile};
use sha2::{Digest, Sha256};

use crate::error::BundleError;
use crate::model::{
    BundleIdentity, PreparedAgent, PreparedAgentBundle, PreparedBundleIndex, PreparedCatalog,
    PreparedDocument, PreparedDocumentOwned, PreparedInstallableBundle, PreparedResource,
    PreparedWorkflow, PreparedWorkflowBundle,
};
use crate::source::{
    BundleSource, ParsedSource, SourceAgent, SourceAgentManifest, SourceExtensions, SourceFile,
    SourceManifest, SourceResource, SourceResources, SourceWorkflowManifest,
};

const AGENT_SOURCE_KIND: &str = "AgentBundle";
const WORKFLOW_SOURCE_KIND: &str = "WorkflowBundle";
const PREPARED_FORMAT_VERSION: u32 = 2;

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
    prepare_sources(vec![source])
}

fn prepare_sources(sources: Vec<BundleSource>) -> Result<PreparedCatalog, BundleError> {
    let mut parsed = sources
        .into_iter()
        .map(parse_source)
        .collect::<Result<Vec<_>, _>>()?;
    parsed.sort_by(|left, right| {
        manifest_identity(&left.manifest)
            .id
            .cmp(&manifest_identity(&right.manifest).id)
    });

    let mut bundle_ids = BTreeSet::new();
    let mut stable_agent_ids = BTreeSet::new();
    let mut bundles = Vec::with_capacity(parsed.len());
    for source in parsed {
        let bundle_id = manifest_identity(&source.manifest).id.clone();
        if !bundle_ids.insert(bundle_id.clone()) {
            return Err(BundleError::DuplicateBundleId { bundle_id });
        }
        bundles.push(prepare_bundle(source, &mut stable_agent_ids)?);
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
    /// Decode canonical prepared bytes and verify catalog, payload, content,
    /// closure, and index integrity before publication.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an outer digest mismatch, unsupported prepared
    /// format, malformed payload, non-canonical vectors, or any integrity
    /// failure. Prepared format v1 is intentionally not upgraded.
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
                    .map(|bundle| bundle.identity().id.as_str()),
            )
        {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
        for bundle in &document.bundles {
            if !prepared_bundle_is_canonical(bundle) {
                return Err(BundleError::NonCanonicalPreparedCatalog);
            }
            validate_prepared_content_digests(bundle)?;
            if prepared_bundle_digest(bundle)? != bundle.digest() {
                return Err(BundleError::PreparedBundleDigestMismatch {
                    bundle_id: bundle.identity().id.clone(),
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

fn manifest_identity(manifest: &SourceManifest) -> &BundleIdentity {
    match manifest {
        SourceManifest::Agent(manifest) => &manifest.identity,
        SourceManifest::Workflow(manifest) => &manifest.identity,
    }
}

fn prepared_bundle_is_canonical(bundle: &PreparedInstallableBundle) -> bool {
    let common = validate_identity(&bundle.identity().id, &bundle.identity().version).is_ok()
        && resources_are_canonical(bundle, "tool", bundle.tools())
        && resources_are_canonical(bundle, "skill", bundle.skills())
        && resources_are_canonical(bundle, "mcp", bundle.mcp())
        && resources_are_canonical(bundle, "hook", bundle.hooks())
        && resources_are_canonical(bundle, "extension", bundle.extensions())
        && bundle.mcp().is_empty()
        && bundle.agents().iter().all(agent_is_canonical);
    if !common {
        return false;
    }
    match bundle {
        PreparedInstallableBundle::Agent(bundle) => {
            bundle.format_version == PREPARED_FORMAT_VERSION
        }
        PreparedInstallableBundle::Workflow(bundle) => {
            bundle.format_version == PREPARED_FORMAT_VERSION
                && is_strictly_sorted(bundle.agents.iter().map(|agent| agent.id.as_str()))
                && valid_workflow_identifier(&bundle.workflow.id)
                && is_canonical_workflow_path(&bundle.workflow.source_path)
                && is_hex_digest(&bundle.workflow.source_digest)
                && is_hex_digest(&bundle.workflow.compiler_revision)
        }
    }
}

fn agent_is_canonical(agent: &PreparedAgent) -> bool {
    is_strictly_sorted(agent.can_spawn.iter().map(|target| target.as_str()))
        && is_strictly_sorted(agent.hook_refs.iter().map(String::as_str))
        && is_strictly_sorted(agent.resource_view.allow.iter().map(String::as_str))
        && is_strictly_sorted(agent.resource_view.deny.iter().map(String::as_str))
}

fn validate_prepared_references(bundles: &[PreparedInstallableBundle]) -> Result<(), BundleError> {
    let mut stable_agents = BTreeSet::new();
    let mut agent_references = BTreeSet::new();
    let mut resource_references = BTreeSet::new();
    let mut bundle_ids = BTreeSet::new();
    for bundle in bundles {
        if !bundle_ids.insert(bundle.identity().id.as_str()) {
            return Err(BundleError::DuplicateBundleId {
                bundle_id: bundle.identity().id.clone(),
            });
        }
        if !bundle.mcp().is_empty() {
            return Err(BundleError::UnsupportedBundleFeature {
                bundle_id: bundle.identity().id.clone(),
                feature: "resources.mcp".to_string(),
            });
        }
        for agent in bundle.agents() {
            if !stable_agents.insert(agent.id.as_str()) {
                return Err(BundleError::DuplicateStableAgentId {
                    stable_id: agent.id.as_str().to_string(),
                });
            }
            for reference in [
                agent.id.as_str().to_string(),
                format!("bundle:{}/agent/{}", bundle.identity().id, agent.id),
            ] {
                if !agent_references.insert(reference.clone()) {
                    return Err(BundleError::NamespaceCollision {
                        bundle_id: bundle.identity().id.clone(),
                        name: reference,
                    });
                }
            }
        }
        for resource in bundle
            .tools()
            .iter()
            .chain(bundle.skills())
            .chain(bundle.mcp())
            .chain(bundle.hooks())
            .chain(bundle.extensions())
        {
            if !resource_references.insert(resource.stable_id.as_str()) {
                return Err(BundleError::NamespaceCollision {
                    bundle_id: bundle.identity().id.clone(),
                    name: resource.stable_id.clone(),
                });
            }
        }
        for hook in bundle.hooks() {
            validate_hook_local_id(&bundle.identity().id, &hook.local_id)?;
        }
        let local_resources = bundle
            .tools()
            .iter()
            .chain(bundle.skills())
            .chain(bundle.mcp())
            .chain(bundle.hooks())
            .chain(bundle.extensions())
            .map(|resource| resource.stable_id.as_str())
            .collect::<BTreeSet<_>>();
        for agent in bundle.agents() {
            for reference in agent
                .resource_view
                .aliases
                .values()
                .chain(&agent.resource_view.allow)
                .chain(&agent.resource_view.deny)
            {
                validate_prepared_resource_reference(
                    &bundle.identity().id,
                    reference,
                    &local_resources,
                )?;
            }
            for reference in &agent.hook_refs {
                validate_prepared_resource_reference(
                    &bundle.identity().id,
                    reference,
                    &local_resources,
                )?;
                if !reference.contains("/hook/") {
                    return Err(BundleError::UnknownResourceReference {
                        bundle_id: bundle.identity().id.clone(),
                        kind: "hook".to_string(),
                        reference: reference.clone(),
                    });
                }
            }
            validate_resource_views(
                &bundle.identity().id,
                agent,
                bundle.tools(),
                bundle.skills(),
            )?;
        }
        if let PreparedInstallableBundle::Workflow(workflow) = bundle {
            validate_prepared_workflow(workflow)?;
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

fn resources_are_canonical(
    bundle: &PreparedInstallableBundle,
    kind: &str,
    resources: &[PreparedResource],
) -> bool {
    let mut names = BTreeSet::new();
    is_strictly_sorted(resources.iter().map(|resource| resource.local_id.as_str()))
        && resources.iter().all(|resource| {
            resource.stable_id
                == format!(
                    "bundle:{}/{kind}/{}",
                    bundle.identity().id,
                    resource.local_id
                )
                && is_strictly_sorted(resource.aliases.iter().map(String::as_str))
                && names.insert(resource.local_id.as_str())
                && resource
                    .aliases
                    .iter()
                    .all(|alias| names.insert(alias.as_str()))
        })
}

fn validate_prepared_content_digests(
    bundle: &PreparedInstallableBundle,
) -> Result<(), BundleError> {
    for agent in bundle.agents() {
        validate_prepared_agent_content(&bundle.identity().id, agent)?;
    }
    for resource in bundle
        .tools()
        .iter()
        .chain(bundle.skills())
        .chain(bundle.mcp())
        .chain(bundle.hooks())
        .chain(bundle.extensions())
    {
        if normalize_source_path(&bundle.identity().id, &resource.source_path).as_deref()
            != Ok(resource.source_path.as_str())
        {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
        if resource.digest != digest_bytes(resource.content.as_bytes()) {
            return Err(BundleError::PreparedContentDigestMismatch {
                bundle_id: bundle.identity().id.clone(),
                source_path: resource.source_path.clone(),
            });
        }
    }
    if let Some(workflow) = bundle.workflow() {
        if normalize_source_path(&bundle.identity().id, &workflow.source_path).as_deref()
            != Ok(workflow.source_path.as_str())
            || !is_canonical_workflow_path(&workflow.source_path)
        {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
        if workflow.source_digest != digest_bytes(workflow.source.as_bytes()) {
            return Err(BundleError::PreparedContentDigestMismatch {
                bundle_id: bundle.identity().id.clone(),
                source_path: workflow.source_path.clone(),
            });
        }
    }
    Ok(())
}

fn validate_prepared_agent_content(
    bundle_id: &str,
    agent: &PreparedAgent,
) -> Result<(), BundleError> {
    match (
        agent.prompt.as_deref(),
        agent.prompt_source.as_deref(),
        agent.prompt_digest.as_deref(),
    ) {
        (Some(prompt), Some(source), Some(digest)) => {
            if normalize_source_path(bundle_id, source).as_deref() != Ok(source) {
                return Err(BundleError::NonCanonicalPreparedCatalog);
            }
            if digest != digest_bytes(prompt.as_bytes()) {
                return Err(BundleError::PreparedContentDigestMismatch {
                    bundle_id: bundle_id.to_string(),
                    source_path: source.to_string(),
                });
            }
        }
        (None, None, None) => {}
        _ => return Err(BundleError::NonCanonicalPreparedCatalog),
    }
    Ok(())
}

fn resolve_catalog_references(
    bundles: &mut [PreparedInstallableBundle],
) -> Result<(), BundleError> {
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
        match bundle {
            PreparedInstallableBundle::Agent(bundle) => resolve_agents(
                &bundle_id,
                std::slice::from_mut(&mut bundle.agent),
                &resources,
                &local_resources,
                &hook_resources,
            )?,
            PreparedInstallableBundle::Workflow(bundle) => resolve_agents(
                &bundle_id,
                &mut bundle.agents,
                &resources,
                &local_resources,
                &hook_resources,
            )?,
        }
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

fn parse_source(source: BundleSource) -> Result<ParsedSource, BundleError> {
    let (name, source_files) = source.into_parts();
    let files = collect_files(&name, source_files)?;
    let yaml = files.get("bundle.yaml");
    let markdown = files.get("bundle.hya.md");
    let (manifest, markdown_prompt) =
        match (yaml, markdown) {
            (Some(_), Some(_)) => {
                return Err(BundleError::InvalidManifest {
                    source_name: name,
                    detail: "source contains both bundle.yaml and bundle.hya.md".to_string(),
                });
            }
            (Some(bytes), None) => (parse_yaml_manifest(&name, bytes)?, None),
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
                let kind = serde_norway::from_str::<crate::source::SourceKind>(frontmatter)
                    .map_err(|error| BundleError::InvalidManifest {
                        source_name: name.clone(),
                        detail: error.to_string(),
                    })?;
                if kind.kind != AGENT_SOURCE_KIND {
                    return Err(BundleError::InvalidManifest {
                        source_name: name,
                        detail: "WorkflowBundle sources must use explicit bundle.yaml".to_string(),
                    });
                }
                let manifest = serde_norway::from_str::<SourceAgentManifest>(frontmatter).map_err(
                    |error| BundleError::InvalidManifest {
                        source_name: name.clone(),
                        detail: error.to_string(),
                    },
                )?;
                (
                    SourceManifest::Agent(Box::new(manifest)),
                    Some(body.trim().to_string()),
                )
            }
            (None, None) => {
                return Err(BundleError::UnsupportedSource { source_name: name });
            }
        };

    if let SourceManifest::Agent(manifest) = &manifest {
        for (present, key, guidance) in [
            (
                manifest.api_version.is_some(),
                "api_version",
                "delete it; the AgentBundle manifest is no longer versioned",
            ),
            (
                manifest.agents.is_some(),
                "agents",
                "a bundle defines exactly one agent: replace the `agents:` list with a single `agent:` map",
            ),
            (
                manifest.agent.harness_access.is_some(),
                "harness_access",
                "the tool plane is host-controlled: a bundle agent always gets the internal public tools plus its own bundle resources",
            ),
        ] {
            if present {
                return Err(BundleError::RemovedManifestKey {
                    source_name: name,
                    key: key.to_string(),
                    guidance: guidance.to_string(),
                });
            }
        }
        if manifest.kind != AGENT_SOURCE_KIND {
            return Err(BundleError::WrongKind {
                source_name: name,
                found: manifest.kind.clone(),
            });
        }
    } else if let SourceManifest::Workflow(manifest) = &manifest
        && manifest.kind != WORKFLOW_SOURCE_KIND
    {
        return Err(BundleError::WrongKind {
            source_name: name,
            found: manifest.kind.clone(),
        });
    }

    let markdown_prompt = match (&manifest, markdown_prompt) {
        (SourceManifest::Agent(manifest), Some(body))
            if body.is_empty() && manifest.agent.prompt.is_some() =>
        {
            None
        }
        (_, prompt) => prompt,
    };
    if let (SourceManifest::Agent(manifest), Some(_)) = (&manifest, markdown_prompt.as_ref())
        && manifest.agent.prompt.is_some()
    {
        return Err(BundleError::InvalidManifest {
            source_name: name,
            detail: "bundle.hya.md uses its body as the agent prompt, so the agent must not also name a prompt resource".to_string(),
        });
    }
    Ok(ParsedSource {
        files,
        manifest,
        markdown_prompt,
    })
}

fn parse_yaml_manifest(name: &str, bytes: &[u8]) -> Result<SourceManifest, BundleError> {
    let kind = serde_norway::from_slice::<crate::source::SourceKind>(bytes).map_err(|error| {
        BundleError::InvalidManifest {
            source_name: name.to_string(),
            detail: error.to_string(),
        }
    })?;
    match kind.kind.as_str() {
        AGENT_SOURCE_KIND => serde_norway::from_slice::<SourceAgentManifest>(bytes)
            .map(Box::new)
            .map(SourceManifest::Agent)
            .map_err(|error| BundleError::InvalidManifest {
                source_name: name.to_string(),
                detail: error.to_string(),
            }),
        WORKFLOW_SOURCE_KIND => serde_norway::from_slice::<SourceWorkflowManifest>(bytes)
            .map(Box::new)
            .map(SourceManifest::Workflow)
            .map_err(|error| BundleError::InvalidManifest {
                source_name: name.to_string(),
                detail: error.to_string(),
            }),
        found => Err(BundleError::WrongKind {
            source_name: name.to_string(),
            found: found.to_string(),
        }),
    }
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

pub(crate) fn normalize_source_path(source_name: &str, path: &str) -> Result<String, BundleError> {
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
) -> Result<PreparedInstallableBundle, BundleError> {
    match source.manifest {
        SourceManifest::Agent(manifest) => prepare_agent_bundle(
            source.files,
            source.markdown_prompt,
            *manifest,
            stable_agent_ids,
        ),
        SourceManifest::Workflow(manifest) => {
            prepare_workflow_bundle(source.files, *manifest, stable_agent_ids)
        }
    }
}

fn prepare_agent_bundle(
    files: BTreeMap<String, Vec<u8>>,
    markdown_prompt: Option<String>,
    manifest: SourceAgentManifest,
    stable_agent_ids: &mut BTreeSet<String>,
) -> Result<PreparedInstallableBundle, BundleError> {
    let bundle_id = manifest.identity.id.clone();
    validate_identity(&bundle_id, &manifest.identity.version)?;
    validate_unsupported(&bundle_id, &manifest.resources, &manifest.extensions)?;
    let (tools, skills, hooks, extensions) =
        prepare_resource_sets(&bundle_id, &files, manifest.resources, manifest.extensions)?;
    let agent = prepare_agent(
        &bundle_id,
        &files,
        markdown_prompt.as_deref(),
        manifest.agent,
        stable_agent_ids,
    )?;
    validate_resource_views(&bundle_id, &agent, &tools, &skills)?;
    let mut bundle = PreparedInstallableBundle::Agent(Box::new(PreparedAgentBundle {
        format_version: PREPARED_FORMAT_VERSION,
        identity: manifest.identity,
        digest: String::new(),
        agent,
        tools,
        skills,
        mcp: Vec::new(),
        hooks,
        extensions,
    }));
    set_bundle_digest(&mut bundle)?;
    Ok(bundle)
}

fn prepare_workflow_bundle(
    files: BTreeMap<String, Vec<u8>>,
    manifest: SourceWorkflowManifest,
    stable_agent_ids: &mut BTreeSet<String>,
) -> Result<PreparedInstallableBundle, BundleError> {
    let bundle_id = manifest.identity.id.clone();
    validate_identity(&bundle_id, &manifest.identity.version)?;
    validate_unsupported(&bundle_id, &manifest.resources, &manifest.extensions)?;
    let workflow_path = normalize_source_path(&bundle_id, &manifest.workflow.path)?;
    if !is_canonical_workflow_path(&workflow_path) {
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.clone(),
            detail: "Workflow source path must match `workflows/*.hya.md`".to_string(),
        });
    }
    let workflow_bytes =
        files
            .get(&workflow_path)
            .ok_or_else(|| BundleError::MissingReference {
                bundle_id: bundle_id.clone(),
                path: workflow_path.clone(),
            })?;
    let workflow_source =
        std::str::from_utf8(workflow_bytes).map_err(|error| BundleError::InvalidManifest {
            source_name: bundle_id.clone(),
            detail: format!("Workflow source `{workflow_path}` is not UTF-8: {error}"),
        })?;
    let compiled =
        compile(WorkflowSource::new(&workflow_path, workflow_source)).map_err(|error| {
            BundleError::WorkflowCompile {
                bundle_id: bundle_id.clone(),
                source_path: workflow_path.clone(),
                detail: error.to_string(),
            }
        })?;
    if manifest.workflow.id != compiled.definition().name() {
        return Err(BundleError::WorkflowIdMismatch {
            bundle_id,
            manifest_id: manifest.workflow.id,
            compiled_id: compiled.definition().name().to_string(),
        });
    }
    let (tools, skills, hooks, extensions) = prepare_resource_sets(
        &manifest.identity.id,
        &files,
        manifest.resources,
        manifest.extensions,
    )?;
    let mut source_agents = manifest.agents;
    for source_agent in &source_agents {
        if let Some(prompt) = &source_agent.prompt {
            let prompt_path = normalize_source_path(&manifest.identity.id, prompt)?;
            if !is_canonical_prompt_path(&prompt_path) {
                return Err(BundleError::InvalidManifest {
                    source_name: manifest.identity.id.clone(),
                    detail: "WorkflowBundle Agent prompt paths must be under `prompts/`"
                        .to_string(),
                });
            }
        }
    }
    source_agents.sort_by(|left, right| left.id.cmp(&right.id));
    let mut agents = Vec::with_capacity(source_agents.len());
    let mut local_agent_ids = BTreeSet::new();
    for source_agent in source_agents {
        if !local_agent_ids.insert(source_agent.id.clone()) {
            return Err(BundleError::NamespaceCollision {
                bundle_id: manifest.identity.id.clone(),
                name: source_agent.id,
            });
        }
        agents.push(prepare_agent(
            &manifest.identity.id,
            &files,
            None,
            source_agent,
            stable_agent_ids,
        )?);
    }
    agents.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    validate_workflow_agent_closure(&manifest.identity.id, &compiled, &agents)?;
    for agent in &agents {
        validate_resource_views(&manifest.identity.id, agent, &tools, &skills)?;
    }
    let mut bundle = PreparedInstallableBundle::Workflow(Box::new(PreparedWorkflowBundle {
        format_version: PREPARED_FORMAT_VERSION,
        identity: manifest.identity,
        digest: String::new(),
        workflow: PreparedWorkflow {
            id: compiled.definition().name().to_string(),
            source_path: workflow_path,
            source: workflow_source.to_string(),
            source_digest: digest_bytes(workflow_bytes),
            compiler_revision: compiled.revision().to_string(),
        },
        agents,
        tools,
        skills,
        mcp: Vec::new(),
        hooks,
        extensions,
    }));
    set_bundle_digest(&mut bundle)?;
    Ok(bundle)
}

/// Prepared resource vectors in tool, Skill, hook, and extension order.
type PreparedResourceSets = (
    Vec<PreparedResource>,
    Vec<PreparedResource>,
    Vec<PreparedResource>,
    Vec<PreparedResource>,
);

fn prepare_resource_sets(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    resources: SourceResources,
    extensions: SourceExtensions,
) -> Result<PreparedResourceSets, BundleError> {
    let tools = prepare_resources(bundle_id, "tool", files, resources.tools)?;
    let skills = prepare_resources(bundle_id, "skill", files, resources.skills)?;
    let hooks = prepare_resources(bundle_id, "hook", files, resources.hooks)?;
    for hook in &hooks {
        validate_hook_local_id(bundle_id, &hook.local_id)?;
    }
    let extensions = prepare_resources(bundle_id, "extension", files, extensions.js)?;
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
    Ok((tools, skills, hooks, extensions))
}

fn validate_workflow_agent_closure(
    bundle_id: &str,
    workflow: &hya_workflow::CompiledWorkflow,
    agents: &[PreparedAgent],
) -> Result<(), BundleError> {
    let available = agents
        .iter()
        .map(|agent| (agent.id.as_str(), agent))
        .collect::<BTreeMap<_, _>>();
    let mut required = BTreeSet::new();
    let mut queue = Vec::new();
    for stage in workflow.plan().stages() {
        require_workflow_agent(
            bundle_id,
            stage.agent(),
            &format!("stage:{}", stage.id()),
            &available,
            &mut required,
            &mut queue,
        )?;
        if let Some(verify) = stage.verify() {
            require_workflow_agent(
                bundle_id,
                verify.agent(),
                &format!("verifier:{}", stage.id()),
                &available,
                &mut required,
                &mut queue,
            )?;
        }
    }
    while let Some(agent_id) = queue.pop() {
        let Some(agent) = available.get(agent_id.as_str()) else {
            continue;
        };
        for target in &agent.can_spawn {
            let target_id = target.as_str();
            require_workflow_agent(
                bundle_id,
                target_id,
                &format!("agent:{agent_id}"),
                &available,
                &mut required,
                &mut queue,
            )?;
        }
    }
    for agent in agents {
        if !required.contains(agent.id.as_str()) {
            return Err(BundleError::WorkflowAgentUnreachable {
                bundle_id: bundle_id.to_string(),
                agent_id: agent.id.as_str().to_string(),
            });
        }
    }
    Ok(())
}

fn require_workflow_agent<'a>(
    bundle_id: &str,
    agent_id: &str,
    reference: &str,
    available: &BTreeMap<&'a str, &'a PreparedAgent>,
    required: &mut BTreeSet<String>,
    queue: &mut Vec<String>,
) -> Result<(), BundleError> {
    if !available.contains_key(agent_id) {
        return Err(BundleError::WorkflowAgentMissing {
            bundle_id: bundle_id.to_string(),
            agent_id: agent_id.to_string(),
            reference: reference.to_string(),
        });
    }
    if required.insert(agent_id.to_string()) {
        queue.push(agent_id.to_string());
    }
    Ok(())
}

fn validate_prepared_workflow(bundle: &PreparedWorkflowBundle) -> Result<(), BundleError> {
    let compiled = compile(WorkflowSource::new(
        &bundle.workflow.source_path,
        &bundle.workflow.source,
    ))
    .map_err(|error| BundleError::WorkflowCompile {
        bundle_id: bundle.identity.id.clone(),
        source_path: bundle.workflow.source_path.clone(),
        detail: error.to_string(),
    })?;
    if bundle.workflow.id != compiled.definition().name() {
        return Err(BundleError::WorkflowIdMismatch {
            bundle_id: bundle.identity.id.clone(),
            manifest_id: bundle.workflow.id.clone(),
            compiled_id: compiled.definition().name().to_string(),
        });
    }
    if bundle.workflow.compiler_revision != compiled.revision().to_string() {
        return Err(BundleError::PreparedContentDigestMismatch {
            bundle_id: bundle.identity.id.clone(),
            source_path: bundle.workflow.source_path.clone(),
        });
    }
    validate_workflow_agent_closure(&bundle.identity.id, &compiled, &bundle.agents)
}

fn is_canonical_workflow_path(path: &str) -> bool {
    let Some(name) = path.strip_prefix("workflows/") else {
        return false;
    };
    !name.is_empty() && !name.contains('/') && name.ends_with(".hya.md")
}
fn is_canonical_prompt_path(path: &str) -> bool {
    path.strip_prefix("prompts/")
        .is_some_and(|name| !name.is_empty() && !name.split('/').any(|part| part.is_empty()))
}

fn valid_workflow_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn validate_resource_views(
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
    stable_agent_ids: &mut BTreeSet<String>,
) -> Result<PreparedAgent, BundleError> {
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
        spawn_lifecycle: source.spawn_lifecycle,
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

fn build_index(bundles: &[PreparedInstallableBundle]) -> Vec<PreparedBundleIndex> {
    bundles
        .iter()
        .map(|bundle| PreparedBundleIndex {
            bundle_id: bundle.identity().id.clone(),
            version: bundle.identity().version.clone(),
            digest: bundle.digest().to_string(),
            agent_ids: bundle
                .agents()
                .iter()
                .map(|agent| agent.id.clone())
                .collect(),
            workflow_ids: bundle
                .workflow()
                .map(|workflow| vec![workflow.id.clone()])
                .unwrap_or_default(),
        })
        .collect()
}

fn prepared_bundle_digest(bundle: &PreparedInstallableBundle) -> Result<String, BundleError> {
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

fn set_bundle_digest(bundle: &mut PreparedInstallableBundle) -> Result<(), BundleError> {
    let digest = prepared_bundle_digest(bundle)?;
    match bundle {
        PreparedInstallableBundle::Agent(bundle) => bundle.digest = digest,
        PreparedInstallableBundle::Workflow(bundle) => bundle.digest = digest,
    }
    Ok(())
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
