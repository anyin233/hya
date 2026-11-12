use std::collections::{BTreeMap, BTreeSet};

use hya_proto::AgentName;
use hya_workflow::{WorkflowSource, compile};
use sha2::{Digest, Sha256};

use crate::error::BundleError;
use crate::model::{
    Bundle, ChannelParticipantRole, ChannelScope, ChannelTemplateKind, PreparedAgent, PreparedApi,
    PreparedBundleApis, PreparedBundleIndex, PreparedBundlePermissionModes, PreparedBundleProcess,
    PreparedBundleSchemas, PreparedCatalog, PreparedChannelParticipant, PreparedChannelTemplate,
    PreparedCheck, PreparedPermissionMode, PreparedProcessExtension, PreparedResource,
    PreparedSchema, PreparedTuiExtension, PreparedWorkflow,
};
use crate::source::{
    BundleSource, ParsedSource, SourceAgent, SourceExtensions, SourceFile, SourceManifest,
    SourceMcpServer, SourceResource, SourceResources, SourceTuiExtension,
};

const PREPARED_FORMAT_VERSION: u32 = 4;

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
        validate_prepared_schema_rows(&document.bundles, &document.schemas)?;
        validate_prepared_process_rows(&document.bundles, &document.process_extensions)?;
        validate_native_binary_bindings(&document.bundles, &document.process_extensions)?;
        validate_prepared_api_rows(
            &document.bundles,
            &document.process_extensions,
            &document.apis,
        )?;
        validate_prepared_permission_mode_rows(
            &document.bundles,
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
    let bundles = vec![bundle];
    let index = build_index(&bundles);
    PreparedCatalog {
        bundles,
        index,
        schemas,
        process_extensions,
        apis,
        permission_modes,
        bytes,
        digest,
    }
}

fn resolve_version_reference(
    manifest: &mut SourceManifest,
    source_name: &str,
) -> Result<(), BundleError> {
    let (version_ref, identity) = (&mut manifest.version_ref, &mut manifest.identity);
    let Some(version_ref) = version_ref.take() else {
        return Ok(());
    };
    if version_ref != "backend" {
        return Err(BundleError::InvalidManifest {
            source_name: source_name.to_string(),
            detail: format!("unsupported version_ref `{version_ref}`; expected `backend`"),
        });
    }
    if identity.version != "0.0.0" {
        return Err(BundleError::InvalidManifest {
            source_name: source_name.to_string(),
            detail: "version_ref requires identity.version 0.0.0".to_string(),
        });
    }
    identity.version = hya_version::BACKEND_VERSION.to_string();
    Ok(())
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
            valid_workflow_identifier(&workflow.id)
                && is_canonical_workflow_path(&workflow.source_path)
                && is_hex_digest(&workflow.source_digest)
                && is_hex_digest(&workflow.compiler_revision)
        })
}

fn agent_is_canonical(agent: &PreparedAgent) -> bool {
    is_strictly_sorted(agent.can_spawn.iter().map(|target| target.as_str()))
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

/// Reserved internal handle schemes a manifest can never claim.
const INTERNAL_SCHEMES: [&str; 3] = ["artifact", "skill", "local"];

/// Validate one declared schema extension against the bundle's prepared tools.
///
/// The scheme must be a `[a-zA-Z0-9_-]` token of at least two characters
/// without the `__` separator and must not name an internal handle family; the
/// tool reference must be a tool the bundle actually declares. Sorting is
/// enforced so the prepared document is deterministic.
fn validate_declared_schemas(
    bundle_id: &str,
    schemas: &[crate::source::SourceSchema],
    tools: &[PreparedResource],
) -> Result<Vec<PreparedSchema>, BundleError> {
    let invalid = |detail: String| BundleError::InvalidManifest {
        source_name: bundle_id.to_string(),
        detail,
    };
    let tool_ids: BTreeSet<&str> = tools.iter().map(|tool| tool.local_id.as_str()).collect();
    let mut seen = BTreeSet::new();
    let mut prepared = Vec::with_capacity(schemas.len());
    for schema in schemas {
        if INTERNAL_SCHEMES.contains(&schema.scheme.as_str()) {
            return Err(invalid(format!(
                "schemas: scheme `{}` is reserved for internal handles and cannot be declared",
                schema.scheme
            )));
        }
        if !is_valid_scheme_token(&schema.scheme) {
            return Err(invalid(format!(
                "schemas: scheme `{}` must be a `[a-zA-Z0-9_-]` token of at least two \
                 characters without `__`",
                schema.scheme
            )));
        }
        if !seen.insert(schema.scheme.as_str()) {
            return Err(invalid(format!(
                "schemas: scheme `{}` is declared more than once",
                schema.scheme
            )));
        }
        if !tool_ids.contains(schema.tool.as_str()) {
            return Err(invalid(format!(
                "schemas: scheme `{}` references tool `{}` which the bundle does not declare",
                schema.scheme, schema.tool
            )));
        }
        prepared.push(PreparedSchema {
            scheme: schema.scheme.clone(),
            tool: schema.tool.clone(),
            writable: schema.writable,
        });
    }
    prepared.sort_by(|left, right| left.scheme.cmp(&right.scheme));
    Ok(prepared)
}

/// Whether `scheme` is a publishable token: at least two characters of
/// `[a-zA-Z0-9_-]` and no `__` separator.
fn is_valid_scheme_token(scheme: &str) -> bool {
    scheme.len() >= 2
        && !scheme.contains("__")
        && scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Validate the document-level schema section of a decoded prepared catalog:
/// rows strictly sorted by bundle id, every row naming a bundle in the
/// document, and every row's declarations valid against that bundle's tools.
fn validate_prepared_schema_rows(
    bundles: &[Bundle],
    rows: &[PreparedBundleSchemas],
) -> Result<(), BundleError> {
    if !is_strictly_sorted(rows.iter().map(|row| row.bundle_id.as_str())) {
        return Err(BundleError::NonCanonicalPreparedCatalog);
    }
    for row in rows {
        let Some(bundle) = bundles
            .iter()
            .find(|bundle| bundle.identity().id == row.bundle_id)
        else {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        };
        if !is_strictly_sorted(row.schemas.iter().map(|schema| schema.scheme.as_str())) {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
        let prepared = validate_declared_schemas(
            &row.bundle_id,
            &row.schemas
                .iter()
                .map(|schema| crate::source::SourceSchema {
                    scheme: schema.scheme.clone(),
                    tool: schema.tool.clone(),
                    writable: schema.writable,
                })
                .collect::<Vec<_>>(),
            bundle.tools(),
        )?;
        if prepared != row.schemas {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
    }
    Ok(())
}

/// Maximum UTF-8 byte length of an endpoint id.
const MAX_API_ID_BYTES: usize = 64;

/// Maximum UTF-8 byte length of an endpoint description.
const MAX_API_DESCRIPTION_BYTES: usize = 1024;

/// Maximum number of endpoints one bundle may declare.
const MAX_APIS_PER_BUNDLE: usize = 64;

/// Validate the manifest's `apis:` declarations and return them sorted by id.
///
/// Endpoints are answered by the bundle's explicit `extensions.process` over
/// the plugin `api/request` request, so declaring any without one is
/// rejected. Ids are `[A-Za-z0-9._-]` tokens of at most 64 bytes that start
/// with an alphanumeric character and are unique within the bundle; paths
/// follow the [`crate::api`] template grammar; two endpoints with the same
/// method and scope may not have overlapping templates (every concrete path
/// resolves to at most one endpoint). Schema paths must name a declared
/// text extension file (`extensions.files`/`extensions.js`) whose content is
/// a JSON object or boolean, so they are packaged and covered by the digest.
fn validate_declared_apis(
    bundle_id: &str,
    apis: &[crate::source::SourceApi],
    process: Option<&PreparedProcessExtension>,
    extensions: &[PreparedResource],
) -> Result<Vec<PreparedApi>, BundleError> {
    let invalid = |detail: String| BundleError::InvalidManifest {
        source_name: bundle_id.to_string(),
        detail,
    };
    if apis.is_empty() {
        return Ok(Vec::new());
    }
    if process.is_none() {
        return Err(invalid(
            "apis: a bundle may declare API endpoints only with an explicit \
             `extensions.process` that serves them"
                .to_string(),
        ));
    }
    if apis.len() > MAX_APIS_PER_BUNDLE {
        return Err(invalid(format!(
            "apis: at most {MAX_APIS_PER_BUNDLE} endpoints may be declared"
        )));
    }
    let mut seen = BTreeSet::new();
    let mut prepared = Vec::with_capacity(apis.len());
    let mut templates: Vec<(&crate::source::SourceApi, crate::api::ApiPathTemplate)> =
        Vec::with_capacity(apis.len());
    for api in apis {
        if !is_valid_api_id(&api.id) {
            return Err(invalid(format!(
                "apis: id `{}` must be a `[A-Za-z0-9._-]` token of at most \
                 {MAX_API_ID_BYTES} bytes starting with a letter or digit",
                api.id
            )));
        }
        if !seen.insert(api.id.as_str()) {
            return Err(invalid(format!(
                "apis: id `{}` is declared more than once",
                api.id
            )));
        }
        if api.description.len() > MAX_API_DESCRIPTION_BYTES
            || api.description.chars().any(char::is_control)
        {
            return Err(invalid(format!(
                "apis: description of `{}` must be at most {MAX_API_DESCRIPTION_BYTES} bytes \
                 without control characters",
                api.id
            )));
        }
        let template = crate::api::ApiPathTemplate::parse(&api.path).map_err(|reason| {
            invalid(format!(
                "apis: path `{}` of `{}` {reason}",
                api.path, api.id
            ))
        })?;
        if let Some((other, _)) = templates.iter().find(|(other, other_template)| {
            other.method == api.method
                && other.scope == api.scope
                && other_template.overlaps(&template)
        }) {
            return Err(invalid(format!(
                "apis: `{}` ({} {} {}) overlaps `{}` ({} {} {}): some request path \
                 would match both",
                api.id,
                api.method,
                api.scope,
                api.path,
                other.id,
                other.method,
                other.scope,
                other.path
            )));
        }
        let request_schema = api
            .request_schema
            .as_deref()
            .map(|path| {
                validate_api_schema_file(bundle_id, &api.id, "request_schema", path, extensions)
            })
            .transpose()?;
        let response_schema = api
            .response_schema
            .as_deref()
            .map(|path| {
                validate_api_schema_file(bundle_id, &api.id, "response_schema", path, extensions)
            })
            .transpose()?;
        templates.push((api, template));
        prepared.push(PreparedApi {
            id: api.id.clone(),
            method: api.method,
            scope: api.scope,
            path: api.path.clone(),
            description: api.description.clone(),
            request_schema,
            response_schema,
        });
    }
    prepared.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(prepared)
}

/// Resolve one `request_schema`/`response_schema` path to a declared text
/// extension file whose content parses as a JSON Schema root (an object or a
/// boolean). Returns the normalized path.
fn validate_api_schema_file(
    bundle_id: &str,
    api_id: &str,
    field: &str,
    path: &str,
    extensions: &[PreparedResource],
) -> Result<String, BundleError> {
    let invalid = |detail: String| BundleError::InvalidManifest {
        source_name: bundle_id.to_string(),
        detail,
    };
    let normalized = normalize_source_path(bundle_id, path).map_err(|_| {
        invalid(format!(
            "apis: {field} `{path}` of `{api_id}` is not a valid path"
        ))
    })?;
    let resource = extensions
        .iter()
        .find(|resource| resource.source_path == normalized && resource.binary_base64.is_none())
        .ok_or_else(|| {
            invalid(format!(
                "apis: {field} `{path}` of `{api_id}` must name a file declared under \
                 `extensions.files` (or `extensions.js`)"
            ))
        })?;
    match serde_json::from_str::<serde_json::Value>(&resource.content) {
        Ok(serde_json::Value::Object(_) | serde_json::Value::Bool(_)) => Ok(normalized),
        Ok(_) | Err(_) => Err(invalid(format!(
            "apis: {field} `{path}` of `{api_id}` must be a JSON Schema document \
             (a JSON object or boolean)"
        ))),
    }
}

/// Whether `id` is a publishable endpoint id.
fn is_valid_api_id(id: &str) -> bool {
    id.len() <= MAX_API_ID_BYTES
        && id
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Validate the document-level `apis` section of a decoded prepared catalog:
/// rows strictly sorted by bundle id, every row naming a bundle in the
/// document that also has an `extensions_process` row, and every row's
/// declarations canonical (valid, non-overlapping, strictly sorted by id,
/// schema files resolving to the bundle's declared extension files).
fn validate_prepared_api_rows(
    bundles: &[Bundle],
    processes: &[PreparedBundleProcess],
    rows: &[PreparedBundleApis],
) -> Result<(), BundleError> {
    if !is_strictly_sorted(rows.iter().map(|row| row.bundle_id.as_str())) {
        return Err(BundleError::NonCanonicalPreparedCatalog);
    }
    for row in rows {
        let bundle = bundles
            .iter()
            .find(|bundle| bundle.identity().id == row.bundle_id)
            .ok_or(BundleError::NonCanonicalPreparedCatalog)?;
        if row.apis.is_empty() || !is_strictly_sorted(row.apis.iter().map(|api| api.id.as_str())) {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
        let process = processes
            .iter()
            .find(|process| process.bundle_id == row.bundle_id)
            .map(|process| &process.process);
        let prepared = validate_declared_apis(
            &row.bundle_id,
            &row.apis
                .iter()
                .map(|api| crate::source::SourceApi {
                    id: api.id.clone(),
                    method: api.method,
                    scope: api.scope,
                    path: api.path.clone(),
                    description: api.description.clone(),
                    request_schema: api.request_schema.clone(),
                    response_schema: api.response_schema.clone(),
                })
                .collect::<Vec<_>>(),
            process,
            bundle.extensions(),
        )
        .map_err(|_| BundleError::NonCanonicalPreparedCatalog)?;
        if prepared != row.apis {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
    }
    Ok(())
}

/// Maximum UTF-8 byte length of a permission mode id.
const MAX_PERMISSION_MODE_ID_BYTES: usize = 64;

/// Maximum UTF-8 byte length of a permission mode title.
const MAX_PERMISSION_MODE_TITLE_BYTES: usize = 128;

/// Maximum UTF-8 byte length of a permission mode description.
const MAX_PERMISSION_MODE_DESCRIPTION_BYTES: usize = 1024;

/// Maximum number of permission modes one bundle may declare.
const MAX_PERMISSION_MODES_PER_BUNDLE: usize = 16;

/// Built-in mode names a bundle may not reuse as a mode id.
const RESERVED_PERMISSION_MODE_IDS: [&str; 2] = ["manual", "yolo"];

/// Validate the manifest's `permission_modes:` declarations and return them
/// sorted by id.
///
/// A mode's asks are answered by the bundle's explicit `extensions.process`
/// through the `permission.approve` hook, so declaring any mode without that
/// process or without a `permission.approve` hook resource is rejected. Ids
/// are `[A-Za-z0-9._-]` tokens of at most 64 bytes that start with an
/// alphanumeric character, are unique within the bundle, and are not the
/// built-in `manual`/`yolo`; titles are 1–128 bytes and descriptions at most
/// 1024 bytes, both without control characters; at most 16 modes.
fn validate_declared_permission_modes(
    bundle_id: &str,
    modes: &[crate::source::SourcePermissionMode],
    process: Option<&PreparedProcessExtension>,
    hooks: &[PreparedResource],
) -> Result<Vec<PreparedPermissionMode>, BundleError> {
    let invalid = |detail: String| BundleError::InvalidManifest {
        source_name: bundle_id.to_string(),
        detail,
    };
    if modes.is_empty() {
        return Ok(Vec::new());
    }
    if process.is_none() {
        return Err(invalid(
            "permission_modes: a bundle may declare permission modes only with an explicit \
             `extensions.process` that approves them"
                .to_string(),
        ));
    }
    if !hooks
        .iter()
        .any(|hook| hook.local_id == "permission.approve")
    {
        return Err(invalid(
            "permission_modes: a bundle that declares permission modes must declare the \
             `permission.approve` hook under `resources.hooks`"
                .to_string(),
        ));
    }
    if modes.len() > MAX_PERMISSION_MODES_PER_BUNDLE {
        return Err(invalid(format!(
            "permission_modes: at most {MAX_PERMISSION_MODES_PER_BUNDLE} modes may be declared"
        )));
    }
    let mut seen = BTreeSet::new();
    let mut prepared = Vec::with_capacity(modes.len());
    for mode in modes {
        if !is_valid_permission_mode_id(&mode.id) {
            return Err(invalid(format!(
                "permission_modes: id `{}` must be a `[A-Za-z0-9._-]` token of at most \
                 {MAX_PERMISSION_MODE_ID_BYTES} bytes starting with a letter or digit",
                mode.id
            )));
        }
        if RESERVED_PERMISSION_MODE_IDS.contains(&mode.id.as_str()) {
            return Err(invalid(format!(
                "permission_modes: id `{}` is reserved for a built-in mode",
                mode.id
            )));
        }
        if !seen.insert(mode.id.as_str()) {
            return Err(invalid(format!(
                "permission_modes: id `{}` is declared more than once",
                mode.id
            )));
        }
        if mode.title.trim().is_empty()
            || mode.title.len() > MAX_PERMISSION_MODE_TITLE_BYTES
            || mode.title.chars().any(char::is_control)
        {
            return Err(invalid(format!(
                "permission_modes: title of `{}` must be 1 to {MAX_PERMISSION_MODE_TITLE_BYTES} \
                 bytes without control characters",
                mode.id
            )));
        }
        if mode.description.len() > MAX_PERMISSION_MODE_DESCRIPTION_BYTES
            || mode.description.chars().any(char::is_control)
        {
            return Err(invalid(format!(
                "permission_modes: description of `{}` must be at most \
                 {MAX_PERMISSION_MODE_DESCRIPTION_BYTES} bytes without control characters",
                mode.id
            )));
        }
        prepared.push(PreparedPermissionMode {
            id: mode.id.clone(),
            title: mode.title.clone(),
            description: mode.description.clone(),
        });
    }
    prepared.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(prepared)
}

/// Whether `id` is a publishable permission mode id (no `/`, so
/// `<bundle-id>/<id>` splits unambiguously at the last `/`).
fn is_valid_permission_mode_id(id: &str) -> bool {
    id.len() <= MAX_PERMISSION_MODE_ID_BYTES
        && id
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Validate the document-level `permission_modes` section of a decoded
/// prepared catalog: rows strictly sorted by bundle id, every row naming a
/// bundle in the document, and every row's declarations canonical (valid,
/// strictly sorted by id, backed by the bundle's process and hook).
fn validate_prepared_permission_mode_rows(
    bundles: &[Bundle],
    processes: &[PreparedBundleProcess],
    rows: &[PreparedBundlePermissionModes],
) -> Result<(), BundleError> {
    if !is_strictly_sorted(rows.iter().map(|row| row.bundle_id.as_str())) {
        return Err(BundleError::NonCanonicalPreparedCatalog);
    }
    for row in rows {
        let bundle = bundles
            .iter()
            .find(|bundle| bundle.identity().id == row.bundle_id)
            .ok_or(BundleError::NonCanonicalPreparedCatalog)?;
        if row.modes.is_empty()
            || !is_strictly_sorted(row.modes.iter().map(|mode| mode.id.as_str()))
        {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
        let process = processes
            .iter()
            .find(|process| process.bundle_id == row.bundle_id)
            .map(|process| &process.process);
        let prepared = validate_declared_permission_modes(
            &row.bundle_id,
            &row.modes
                .iter()
                .map(|mode| crate::source::SourcePermissionMode {
                    id: mode.id.clone(),
                    title: mode.title.clone(),
                    description: mode.description.clone(),
                })
                .collect::<Vec<_>>(),
            process,
            bundle.hooks(),
        )
        .map_err(|_| BundleError::NonCanonicalPreparedCatalog)?;
        if prepared != row.modes {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
    }
    Ok(())
}

/// Validate the document-level `extensions.process` section of a decoded
/// prepared catalog: rows strictly sorted by bundle id, every row naming a
/// bundle in the document, and every declaration carrying a usable command.
fn validate_prepared_process_rows(
    bundles: &[Bundle],
    rows: &[PreparedBundleProcess],
) -> Result<(), BundleError> {
    if !is_strictly_sorted(rows.iter().map(|row| row.bundle_id.as_str())) {
        return Err(BundleError::NonCanonicalPreparedCatalog);
    }
    for row in rows {
        if !bundles
            .iter()
            .any(|bundle| bundle.identity().id == row.bundle_id)
            || validate_process_command(&row.process.command).is_err()
        {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
    }
    Ok(())
}

fn validate_native_binary_bindings(
    bundles: &[Bundle],
    rows: &[PreparedBundleProcess],
) -> Result<(), BundleError> {
    for bundle in bundles {
        let binaries = bundle
            .extensions()
            .iter()
            .filter(|resource| {
                resource.binary_base64.is_some() && !resource.stable_id.contains("/library/")
            })
            .collect::<Vec<_>>();
        if binaries.is_empty() {
            continue;
        }
        let Some(process) = rows
            .iter()
            .find(|row| row.bundle_id == bundle.identity().id)
            .map(|row| &row.process)
        else {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        };
        if process.kind != crate::model::PreparedProcessKind::Rust {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
        let first = process
            .command
            .first()
            .map(String::as_str)
            .unwrap_or_default();
        if !binaries.iter().any(|binary| {
            first == binary.source_path
                || first == format!("${{BUNDLE_ROOT}}/{}", binary.source_path)
        }) {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
    }
    Ok(())
}

/// Whether `command` is a usable argv: non-empty with no blank arguments.
fn validate_process_command(command: &[String]) -> Result<(), String> {
    if command.is_empty() {
        return Err("extensions.process: command must not be empty".to_string());
    }
    if let Some(arg) = command.iter().find(|arg| arg.trim().is_empty()) {
        return Err(format!(
            "extensions.process: command arguments must not be blank (got {arg:?})"
        ));
    }
    Ok(())
}

fn resources_are_canonical(bundle: &Bundle, kind: &str, resources: &[PreparedResource]) -> bool {
    let mut names = BTreeSet::new();
    is_strictly_sorted(resources.iter().map(|resource| resource.local_id.as_str()))
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

fn resolve_catalog_references(bundles: &mut [Bundle]) -> Result<(), BundleError> {
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

fn parse_source(source: BundleSource) -> Result<ParsedSource, BundleError> {
    let (name, source_files) = source.into_parts();
    let files = collect_files(&name, source_files)?;
    let yaml = files.get("bundle.yaml");
    let markdown = files.get("bundle.hya.md");
    let (mut manifest, markdown_prompt) = match (yaml, markdown) {
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
            (
                parse_yaml_manifest(&name, frontmatter.as_bytes())?,
                Some(body.trim().to_string()),
            )
        }
        (None, None) => {
            return Err(BundleError::UnsupportedSource { source_name: name });
        }
    };

    resolve_version_reference(&mut manifest, &name)?;

    let markdown_prompt = markdown_prompt.filter(|body| !body.is_empty());
    if markdown_prompt.is_some()
        && (manifest.agents.len() != 1 || manifest.agents[0].prompt.is_some())
    {
        return Err(BundleError::InvalidManifest {
            source_name: name,
            detail: "a Markdown body requires exactly one agent without an explicit prompt".into(),
        });
    }
    if let Some(check) = &manifest.check {
        check.validate(&name)?;
    }
    Ok(ParsedSource {
        files,
        manifest,
        markdown_prompt,
    })
}

fn parse_yaml_manifest(name: &str, bytes: &[u8]) -> Result<SourceManifest, BundleError> {
    #[derive(serde::Deserialize)]
    struct Kind {
        kind: String,
    }
    let kind: Kind =
        serde_norway::from_slice(bytes).map_err(|error| BundleError::InvalidManifest {
            source_name: name.to_string(),
            detail: error.to_string(),
        })?;
    if kind.kind != "Bundle" {
        return Err(BundleError::WrongKind {
            source_name: name.to_string(),
            found: kind.kind,
        });
    }
    let manifest: SourceManifest =
        serde_norway::from_slice(bytes).map_err(|error| BundleError::InvalidManifest {
            source_name: name.to_string(),
            detail: error.to_string(),
        })?;
    debug_assert_eq!(manifest.kind, "Bundle");
    Ok(manifest)
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

/// Reserved namespace tokens that contributed sources may not claim.
const RESERVED_NAMESPACES: [&str; 4] = ["mcp", "harness", "builtin", "plugin"];

/// Validate a namespace token: the same charset as tool-plane tokens
/// (`[a-zA-Z0-9_-]`, non-empty, no `__` separator) and not reserved.
fn validate_namespace_token(namespace: &str) -> Result<(), String> {
    if namespace.is_empty() {
        return Err("namespace must not be empty".to_string());
    }
    if namespace.contains("__") {
        return Err("namespace must not contain the `__` separator".to_string());
    }
    if !namespace
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err("namespace may only contain ASCII letters, digits, `_`, and `-`".to_string());
    }
    if RESERVED_NAMESPACES.contains(&namespace) {
        return Err(format!(
            "namespace `{namespace}` is reserved; pick another namespace"
        ));
    }
    Ok(())
}

/// Resolve the bundle's provider-facing namespace: the declared value, or the
/// identity name segment (the part after the sole `/`). Fails with
/// [`BundleError::InvalidNamespace`] when the result is not a valid token.
fn resolve_namespace(
    source_name: &str,
    identity_id: &str,
    declared: &Option<String>,
) -> Result<String, BundleError> {
    let invalid = |namespace: &str, guidance: String| {
        Err(BundleError::InvalidNamespace {
            source_name: source_name.to_string(),
            namespace: namespace.to_string(),
            guidance,
        })
    };
    let namespace = match declared {
        // An explicit empty declaration means "use the identity default".
        Some(value) if value.is_empty() => identity_id
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string(),
        Some(value) => value.clone(),
        None => identity_id
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string(),
    };
    if let Err(detail) = validate_namespace_token(&namespace) {
        let guidance = if declared.as_deref().is_none_or(str::is_empty) {
            format!("{detail}; declare an explicit `namespace:` in the manifest")
        } else {
            detail
        };
        return invalid(&namespace, guidance);
    }
    Ok(namespace)
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
    let namespace = Some(resolve_namespace(
        &bundle_id,
        &manifest.identity.id,
        &manifest.namespace,
    )?);
    validate_unsupported(&bundle_id, &manifest.extensions)?;
    let process = declared_process_extension(&bundle_id, &manifest.extensions)?;
    let (tools, skills, mcp, hooks, extensions) =
        prepare_resource_sets(&bundle_id, &files, manifest.resources, manifest.extensions)?;
    let tui = prepare_tui(&bundle_id, manifest.tui.as_ref(), &extensions)?;
    let schemas = validate_declared_schemas(&bundle_id, &manifest.schemas, &tools)?;
    let apis = validate_declared_apis(&bundle_id, &manifest.apis, process.as_ref(), &extensions)?;
    let permission_modes = validate_declared_permission_modes(
        &bundle_id,
        &manifest.permission_modes,
        process.as_ref(),
        &hooks,
    )?;
    let mut source_agents = manifest.agents;
    source_agents.sort_by(|left, right| left.id.cmp(&right.id));
    let mut agents = Vec::with_capacity(source_agents.len());
    let mut local_agent_ids = BTreeSet::new();
    for source_agent in source_agents {
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
        agents.push(prepare_agent(
            &bundle_id,
            &files,
            markdown_prompt.as_deref(),
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
        digest: String::new(),
        tui,
        agents,
        channels,
        tools,
        skills,
        mcp,
        hooks,
        extensions,
        schemas: schemas.clone(),
        process: process.clone(),
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
                && valid_workflow_identifier(&channel.id)
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

fn prepare_workflow(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    workflow: crate::source::SourceWorkflow,
    agents: &[PreparedAgent],
) -> Result<PreparedWorkflow, BundleError> {
    let workflow_path = normalize_source_path(bundle_id, &workflow.path)?;
    if !is_canonical_workflow_path(&workflow_path) {
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: "Workflow source path must match `workflows/*.hya.md`".to_string(),
        });
    }
    let workflow_bytes =
        files
            .get(&workflow_path)
            .ok_or_else(|| BundleError::MissingReference {
                bundle_id: bundle_id.to_string(),
                path: workflow_path.clone(),
            })?;
    let workflow_source =
        std::str::from_utf8(workflow_bytes).map_err(|error| BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: format!("Workflow source `{workflow_path}` is not UTF-8: {error}"),
        })?;
    let compiled =
        compile(WorkflowSource::new(&workflow_path, workflow_source)).map_err(|error| {
            BundleError::WorkflowCompile {
                bundle_id: bundle_id.to_string(),
                source_path: workflow_path.clone(),
                detail: error.to_string(),
            }
        })?;
    if workflow.id != compiled.definition().name() {
        return Err(BundleError::WorkflowIdMismatch {
            bundle_id: bundle_id.to_string(),
            manifest_id: workflow.id,
            compiled_id: compiled.definition().name().to_string(),
        });
    }
    validate_workflow_agent_closure(bundle_id, &compiled, agents)?;
    Ok(PreparedWorkflow {
        id: compiled.definition().name().to_string(),
        source_path: workflow_path,
        source: workflow_source.to_string(),
        source_digest: digest_bytes(workflow_bytes),
        compiler_revision: compiled.revision().to_string(),
    })
}

type PreparedResourceSets = (
    Vec<PreparedResource>,
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
        extensions.sort_by(|left, right| left.stable_id.cmp(&right.stable_id));
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
    extensions.sort_by(|left, right| left.local_id.cmp(&right.local_id));
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

fn validate_prepared_workflow(
    bundle: &Bundle,
    workflow: &PreparedWorkflow,
) -> Result<(), BundleError> {
    let compiled =
        compile(WorkflowSource::new(&workflow.source_path, &workflow.source)).map_err(|error| {
            BundleError::WorkflowCompile {
                bundle_id: bundle.identity.id.clone(),
                source_path: workflow.source_path.clone(),
                detail: error.to_string(),
            }
        })?;
    if workflow.id != compiled.definition().name() {
        return Err(BundleError::WorkflowIdMismatch {
            bundle_id: bundle.identity.id.clone(),
            manifest_id: workflow.id.clone(),
            compiled_id: compiled.definition().name().to_string(),
        });
    }
    if workflow.compiler_revision != compiled.revision().to_string() {
        return Err(BundleError::PreparedContentDigestMismatch {
            bundle_id: bundle.identity.id.clone(),
            source_path: workflow.source_path.clone(),
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

/// Native executable files require a Rust process extension.
fn validate_unsupported(bundle_id: &str, extensions: &SourceExtensions) -> Result<(), BundleError> {
    if !extensions.rust.is_empty()
        && !extensions
            .process
            .as_ref()
            .is_some_and(|process| matches!(process.kind, crate::source::SourceProcessKind::Rust))
    {
        return Err(BundleError::UnsupportedBundleFeature {
            bundle_id: bundle_id.to_string(),
            feature: "extensions.rust".to_string(),
        });
    }
    Ok(())
}

/// Validate and lift the manifest's optional `extensions.process` declaration.
fn declared_process_extension(
    bundle_id: &str,
    extensions: &SourceExtensions,
) -> Result<Option<PreparedProcessExtension>, BundleError> {
    let Some(process) = &extensions.process else {
        return Ok(None);
    };
    if let Err(detail) = validate_process_command(&process.command) {
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail,
        });
    }
    Ok(Some(PreparedProcessExtension {
        kind: match process.kind {
            crate::source::SourceProcessKind::Rust => crate::model::PreparedProcessKind::Rust,
            crate::source::SourceProcessKind::Bun => crate::model::PreparedProcessKind::Bun,
            crate::source::SourceProcessKind::Claude => crate::model::PreparedProcessKind::Claude,
        },
        command: process.command.clone(),
    }))
}

pub(crate) fn validate_hook_local_id(bundle_id: &str, local_id: &str) -> Result<(), BundleError> {
    if matches!(
        local_id,
        "event"
            | "tool.execute.before"
            | "tool.execute.after"
            | "command.execute.before"
            | "experimental.text.complete"
            | "message.user.before"
            | "chat.params"
            | "permission.ask"
            | "permission.approve"
            | "goal.evaluate"
            | "loop.verifier"
            | "loop.planner"
            | "loop.should_stop"
            | "compaction.before"
            | "compaction.after"
            | "session.start"
            | "session.end"
            | "agent.spawn"
            | "model.fallback"
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

fn set_bundle_digest(bundle: &mut Bundle) -> Result<(), BundleError> {
    let digest = prepared_bundle_digest(bundle)?;
    bundle.digest = digest;
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
    if tui.api_version != 1 {
        return Err("tui.api_version must be 1".to_string());
    }
    let parts = tui.sdk.split('.').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len())
        || parts[0] != "1"
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
        })
    {
        return Err("tui.sdk must be major.minor[.patch] with major 1".to_string());
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
