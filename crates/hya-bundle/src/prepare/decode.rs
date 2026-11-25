use std::collections::BTreeSet;

use hya_workflow::is_valid_identifier;
use sha2::{Digest, Sha256};

use super::extensions::{
    validate_hook_local_id, validate_native_binary_bindings, validate_prepared_api_rows,
    validate_prepared_permission_mode_rows, validate_prepared_process_rows,
    validate_prepared_schema_rows,
};
use super::parse::{normalize_source_path, validate_identity};
use super::resources::validate_resource_views;
use super::workflow::{is_canonical_workflow_path, validate_prepared_workflow};
use super::{PREPARED_FORMAT_VERSION, channels_are_canonical, validate_tui};
use crate::error::BundleError;
use crate::model::{
    Bundle, PreparedAgent, PreparedBundleApis, PreparedBundleIndex, PreparedBundlePermissionModes,
    PreparedBundleProcess, PreparedBundleSchemas, PreparedCatalog, PreparedResource,
};

impl PreparedCatalog {
    /// Decode canonical prepared bytes and verify catalog, payload, content,
    /// closure, and index integrity before publication.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an outer digest mismatch, unsupported prepared
    /// format, malformed payload, non-canonical vectors, or any integrity
    /// failure. Older prepared formats require reinstalling; no migration is performed.
    pub fn decode(bytes: &[u8], expected_digest: &str) -> Result<Self, BundleError> {
        let actual_digest = digest_bytes(bytes);
        if actual_digest != expected_digest {
            return Err(BundleError::PreparedDigestMismatch {
                expected: expected_digest.to_string(),
                actual: actual_digest,
            });
        }
        // Check the envelope before decoding fields removed from older payloads.
        #[derive(serde::Deserialize)]
        struct Format {
            format_version: u32,
        }
        let format: Format =
            serde_json::from_slice(bytes).map_err(|error| BundleError::PreparedDecode {
                detail: error.to_string(),
            })?;
        if format.format_version != PREPARED_FORMAT_VERSION {
            return Err(BundleError::UnsupportedPreparedFormat {
                found: format.format_version,
            });
        }
        let bundle: Bundle =
            serde_json::from_slice(bytes).map_err(|error| BundleError::PreparedDecode {
                detail: error.to_string(),
            })?;
        let document = catalog_from_bundle(bundle, bytes.to_vec(), expected_digest.to_string());
        for bundle in document.bundles() {
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
        validate_prepared_references(document.bundles())?;
        validate_prepared_schema_rows(document.bundles(), &document.schemas)?;
        validate_prepared_process_rows(document.bundles(), &document.process_extensions)?;
        validate_native_binary_bindings(document.bundles(), &document.process_extensions)?;
        validate_prepared_api_rows(
            document.bundles(),
            &document.process_extensions,
            &document.apis,
        )?;
        validate_prepared_permission_mode_rows(
            document.bundles(),
            &document.process_extensions,
            &document.permission_modes,
        )?;
        Ok(document)
    }
}

fn catalog_from_bundle(bundle: Bundle, bytes: Vec<u8>, digest: String) -> PreparedCatalog {
    let id = bundle.identity.id.clone();
    let schemas = if bundle.schemas.is_empty() {
        vec![]
    } else {
        vec![PreparedBundleSchemas {
            bundle_id: id.clone(),
            schemas: bundle.schemas.clone(),
        }]
    };
    let process_extensions = bundle
        .process
        .clone()
        .map(|process| PreparedBundleProcess {
            bundle_id: id.clone(),
            process,
        })
        .into_iter()
        .collect();
    let apis = if bundle.apis.is_empty() {
        vec![]
    } else {
        vec![PreparedBundleApis {
            bundle_id: id.clone(),
            apis: bundle.apis.clone(),
        }]
    };
    let permission_modes = if bundle.permission_modes.is_empty() {
        vec![]
    } else {
        vec![PreparedBundlePermissionModes {
            bundle_id: id,
            modes: bundle.permission_modes.clone(),
        }]
    };
    let index = build_index(std::slice::from_ref(&bundle));
    PreparedCatalog {
        bundle: std::sync::Arc::new(bundle),
        index,
        schemas,
        process_extensions,
        apis,
        permission_modes,
        bytes,
        digest,
    }
}

fn prepared_bundle_is_canonical(bundle: &Bundle) -> bool {
    let common = validate_identity(&bundle.identity().id, &bundle.identity().version).is_ok()
        && resources_are_canonical(bundle, "tool", bundle.tools())
        && resources_are_canonical(bundle, "skill", bundle.skills())
        && resources_are_canonical(bundle, "mcp", bundle.mcp())
        && resources_are_canonical(bundle, "hook", bundle.hooks())
        && resources_are_canonical(bundle, "extension", bundle.extensions())
        && bundle.agents().iter().all(agent_is_canonical)
        && bundle.tui().is_none_or(|tui| {
            validate_tui(tui, bundle.extensions()).is_ok()
                && is_strictly_sorted(tui.permissions.iter().map(String::as_str))
        });
    if !common {
        return false;
    }
    bundle.format_version == PREPARED_FORMAT_VERSION
        && is_strictly_sorted(bundle.agents.iter().map(|agent| agent.id.as_str()))
        && channels_are_canonical(
            &bundle.channels,
            &bundle
                .agents
                .iter()
                .map(|agent| agent.id.as_str())
                .collect(),
        )
        && bundle.workflow.as_ref().is_none_or(|workflow| {
            is_valid_identifier(&workflow.id)
                && is_canonical_workflow_path(&workflow.source_path)
                && is_hex_digest(&workflow.source_digest)
                && is_hex_digest(&workflow.compiler_revision)
        })
}

fn agent_is_canonical(agent: &PreparedAgent) -> bool {
    is_strictly_sorted(agent.can_spawn.iter().map(|target| target.as_str()))
        && is_strictly_sorted(agent.advertise.iter().map(String::as_str))
        && is_strictly_sorted(agent.hook_refs.iter().map(String::as_str))
        && is_strictly_sorted(agent.resource_view.allow.iter().map(String::as_str))
        && is_strictly_sorted(agent.resource_view.deny.iter().map(String::as_str))
}

fn validate_prepared_references(bundles: &[Bundle]) -> Result<(), BundleError> {
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
        if let Some(workflow) = &bundle.workflow {
            validate_prepared_workflow(bundle, workflow)?;
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

pub(super) fn resource_order(
    left: &PreparedResource,
    right: &PreparedResource,
) -> std::cmp::Ordering {
    left.local_id.cmp(&right.local_id)
}

fn resources_are_canonical(bundle: &Bundle, kind: &str, resources: &[PreparedResource]) -> bool {
    let mut names = BTreeSet::new();
    resources
        .windows(2)
        .all(|pair| resource_order(&pair[0], &pair[1]).is_lt())
        && resources.iter().all(|resource| {
            (resource.stable_id
                == format!(
                    "bundle:{}/{kind}/{}",
                    bundle.identity().id,
                    resource.local_id
                )
                || (kind == "extension"
                    && resource.binary_base64.is_some()
                    && resource.stable_id
                        == format!(
                            "bundle:{}/library/{}",
                            bundle.identity().id,
                            resource.local_id
                        )))
                && is_strictly_sorted(resource.aliases.iter().map(String::as_str))
                && names.insert(resource.local_id.as_str())
                && resource
                    .aliases
                    .iter()
                    .all(|alias| names.insert(alias.as_str()))
        })
}

fn validate_prepared_content_digests(bundle: &Bundle) -> Result<(), BundleError> {
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
        if resource.binary_base64.is_some() && !bundle.extensions().contains(resource) {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
        let bytes = resource
            .source_bytes()
            .map_err(|_| BundleError::NonCanonicalPreparedCatalog)?;
        if resource.binary_base64.is_some()
            && (!resource.content.is_empty()
                || base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)
                    != resource.binary_base64.as_deref().unwrap_or_default())
        {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
        if resource.digest != digest_bytes(&bytes) {
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

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn build_index(bundles: &[Bundle]) -> Vec<PreparedBundleIndex> {
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

fn prepared_bundle_digest(bundle: &Bundle) -> Result<String, BundleError> {
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

pub(super) fn set_bundle_digest(bundle: &mut Bundle) -> Result<(), BundleError> {
    let digest = prepared_bundle_digest(bundle)?;
    bundle.digest = digest;
    Ok(())
}

pub(super) fn digest_bytes(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes).as_slice())
}

pub(super) fn is_strictly_sorted<'a>(values: impl Iterator<Item = &'a str>) -> bool {
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
