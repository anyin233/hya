use std::collections::BTreeSet;

use super::decode::is_strictly_sorted;
use super::parse::normalize_source_path;
use crate::error::BundleError;
use crate::model::{
    Bundle, PreparedApi, PreparedBundleApis, PreparedBundlePermissionModes, PreparedBundleProcess,
    PreparedBundleSchemas, PreparedJsRuntimeMode, PreparedNativeCapability, PreparedPermissionMode,
    PreparedProcessExtension, PreparedProcessKind, PreparedResource, PreparedRuntime,
    PreparedRuntimeKind, PreparedSchema,
};
use crate::source::{
    SourceExtensions, SourceJsRuntimeMode, SourceNativeCapability, SourceResource, SourceRuntime,
    SourceRuntimeKind,
};

/// Reserved internal handle schemes a manifest can never claim.
const INTERNAL_SCHEMES: [&str; 3] = ["artifact", "skill", "local"];

/// Validate one declared schema extension against the bundle's prepared tools.
///
/// The scheme must be a `[a-zA-Z0-9_-]` token of at least two characters
/// without the `__` separator and must not name an internal handle family; the
/// tool reference must be a tool the bundle actually declares. Sorting is
/// enforced so the prepared document is deterministic.
pub(super) fn validate_declared_schemas(
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
pub(super) fn validate_prepared_schema_rows(
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
pub(super) fn validate_declared_apis(
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
pub(super) fn validate_prepared_api_rows(
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
pub(super) fn validate_declared_permission_modes(
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
pub(super) fn validate_prepared_permission_mode_rows(
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
pub(super) fn validate_prepared_process_rows(
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
            || validate_process(&row.process.command, row.process.timeout_ms).is_err()
        {
            return Err(BundleError::NonCanonicalPreparedCatalog);
        }
    }
    Ok(())
}

pub(super) fn validate_native_binary_bindings(
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

/// Validate process argv and the optional per-request timeout.
fn validate_process(command: &[String], timeout_ms: Option<u64>) -> Result<(), String> {
    if command.is_empty() {
        return Err("extensions.process: command must not be empty".to_string());
    }
    if let Some(arg) = command.iter().find(|arg| arg.trim().is_empty()) {
        return Err(format!(
            "extensions.process: command arguments must not be blank (got {arg:?})"
        ));
    }
    if let Some(timeout) = timeout_ms
        && !(1000..=3_600_000).contains(&timeout)
    {
        return Err(format!(
            "extensions.process.timeout_ms must be 1000..=3600000 (got {timeout})"
        ));
    }
    Ok(())
}

/// Native executable files require a Rust process extension.
pub(super) fn validate_unsupported(
    bundle_id: &str,
    extensions: &SourceExtensions,
) -> Result<(), BundleError> {
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

/// Resolved runtime preparation outputs derived from top-level `runtime:`
/// and/or legacy `extensions.process`.
pub(super) struct ResolvedRuntime {
    pub prepared: Option<PreparedRuntime>,
    pub process: Option<PreparedProcessExtension>,
    pub js_entry_resource: Option<SourceResource>,
    pub native_library_resource: Option<SourceResource>,
}

/// Validate and lift the optional top-level `runtime:` block together with
/// legacy `extensions.{process, libraries, rust}` conflict checks.
pub(super) fn resolve_runtime(
    bundle_id: &str,
    runtime: Option<&SourceRuntime>,
    extensions: &SourceExtensions,
) -> Result<ResolvedRuntime, BundleError> {
    let invalid = |detail: String| BundleError::InvalidManifest {
        source_name: bundle_id.to_string(),
        detail,
    };
    let Some(runtime) = runtime else {
        let process = declared_process_extension(bundle_id, extensions)?;
        return Ok(ResolvedRuntime {
            prepared: None,
            process,
            js_entry_resource: None,
            native_library_resource: None,
        });
    };
    if extensions.process.is_some() {
        return Err(invalid(
            "runtime: cannot declare both `runtime` and legacy `extensions.process`".to_string(),
        ));
    }
    if !extensions.libraries.is_empty() {
        return Err(invalid(
            "runtime: cannot declare both `runtime` and legacy `extensions.libraries`".to_string(),
        ));
    }
    if !extensions.rust.is_empty() {
        return Err(invalid(
            "runtime: cannot declare both `runtime` and legacy `extensions.rust`".to_string(),
        ));
    }

    match runtime.kind {
        SourceRuntimeKind::Static => {
            if runtime.mode.is_some()
                || runtime.entry.is_some()
                || !runtime.command.is_empty()
                || runtime.timeout_ms.is_some()
                || runtime.abi_version.is_some()
                || runtime.crate_dir.is_some()
                || runtime.library.is_some()
                || !runtime.capabilities.is_empty()
            {
                return Err(invalid(
                    "runtime: `kind: static` must not set js or native runtime fields".to_string(),
                ));
            }
            Ok(ResolvedRuntime {
                prepared: Some(PreparedRuntime {
                    kind: PreparedRuntimeKind::Static,
                    mode: None,
                    entry: None,
                    abi_version: None,
                    crate_dir: None,
                    library: None,
                    capabilities: Vec::new(),
                }),
                process: None,
                js_entry_resource: None,
                native_library_resource: None,
            })
        }
        SourceRuntimeKind::Js => {
            if runtime.abi_version.is_some()
                || runtime.crate_dir.is_some()
                || runtime.library.is_some()
                || !runtime.capabilities.is_empty()
            {
                return Err(invalid(
                    "runtime: `kind: js` must not set native fields (`abi_version`, `crate_dir`, `library`, `capabilities`)"
                        .to_string(),
                ));
            }
            if let Some(timeout) = runtime.timeout_ms
                && !(1000..=3_600_000).contains(&timeout)
            {
                return Err(invalid(format!(
                    "runtime.timeout_ms must be 1000..=3600000 (got {timeout})"
                )));
            }
            let entry = runtime
                .entry
                .as_deref()
                .map(|path| normalize_js_entry(bundle_id, path))
                .transpose()?;
            let mode = match runtime.mode {
                Some(mode) => mode,
                None if !runtime.command.is_empty() && entry.is_none() => {
                    SourceJsRuntimeMode::Process
                }
                None if entry.is_some() && runtime.command.is_empty() => {
                    SourceJsRuntimeMode::Managed
                }
                None => {
                    return Err(invalid(
                        "runtime: `kind: js` requires `mode` (`managed`, `process`, or `claude`)"
                            .to_string(),
                    ));
                }
            };
            match mode {
                SourceJsRuntimeMode::Managed => {
                    if !runtime.command.is_empty() {
                        return Err(invalid(
                            "runtime: `kind: js` with `mode: managed` must not set `command`"
                                .to_string(),
                        ));
                    }
                    let Some(entry_path) = entry else {
                        return Err(invalid(
                            "runtime: `kind: js` with `mode: managed` requires `entry`".to_string(),
                        ));
                    };
                    let js_entry_resource = Some(SourceResource {
                        id: "runtime".to_string(),
                        path: entry_path.clone(),
                        aliases: Vec::new(),
                    });
                    Ok(ResolvedRuntime {
                        prepared: Some(PreparedRuntime {
                            kind: PreparedRuntimeKind::Js,
                            mode: Some(PreparedJsRuntimeMode::Managed),
                            entry: Some(entry_path),
                            abi_version: None,
                            crate_dir: None,
                            library: None,
                            capabilities: Vec::new(),
                        }),
                        process: None,
                        js_entry_resource,
                        native_library_resource: None,
                    })
                }
                SourceJsRuntimeMode::Process => {
                    if let Err(detail) = validate_process(&runtime.command, runtime.timeout_ms) {
                        return Err(invalid(detail.replace("extensions.process", "runtime")));
                    }
                    let js_entry_resource = entry.as_ref().map(|path| SourceResource {
                        id: "runtime".to_string(),
                        path: path.clone(),
                        aliases: Vec::new(),
                    });
                    Ok(ResolvedRuntime {
                        prepared: Some(PreparedRuntime {
                            kind: PreparedRuntimeKind::Js,
                            mode: Some(PreparedJsRuntimeMode::Process),
                            entry,
                            abi_version: None,
                            crate_dir: None,
                            library: None,
                            capabilities: Vec::new(),
                        }),
                        process: Some(PreparedProcessExtension {
                            kind: PreparedProcessKind::Bun,
                            command: runtime.command.clone(),
                            timeout_ms: runtime.timeout_ms,
                        }),
                        js_entry_resource,
                        native_library_resource: None,
                    })
                }
                SourceJsRuntimeMode::Claude => {
                    if entry.is_some() {
                        return Err(invalid(
                            "runtime: `kind: js` with `mode: claude` must not set `entry`"
                                .to_string(),
                        ));
                    }
                    if let Err(detail) = validate_process(&runtime.command, runtime.timeout_ms) {
                        return Err(invalid(detail.replace("extensions.process", "runtime")));
                    }
                    Ok(ResolvedRuntime {
                        prepared: Some(PreparedRuntime {
                            kind: PreparedRuntimeKind::Js,
                            mode: Some(PreparedJsRuntimeMode::Claude),
                            entry: None,
                            abi_version: None,
                            crate_dir: None,
                            library: None,
                            capabilities: Vec::new(),
                        }),
                        process: Some(PreparedProcessExtension {
                            kind: PreparedProcessKind::Claude,
                            command: runtime.command.clone(),
                            timeout_ms: runtime.timeout_ms,
                        }),
                        js_entry_resource: None,
                        native_library_resource: None,
                    })
                }
            }
        }
        SourceRuntimeKind::Native => {
            if runtime.mode.is_some()
                || runtime.entry.is_some()
                || !runtime.command.is_empty()
                || runtime.timeout_ms.is_some()
            {
                return Err(invalid(
                    "runtime: `kind: native` must not set JS fields (`mode`, `entry`, `command`, `timeout_ms`)"
                        .to_string(),
                ));
            }
            let abi_version = runtime.abi_version.unwrap_or(1);
            if abi_version != 1 && abi_version != 2 {
                return Err(invalid(format!(
                    "runtime.abi_version must be 1 or 2 (got {abi_version})"
                )));
            }
            let crate_dir = runtime
                .crate_dir
                .as_deref()
                .map(|path| normalize_source_path(bundle_id, path))
                .transpose()?;
            let library = runtime
                .library
                .as_deref()
                .map(|path| normalize_source_path(bundle_id, path))
                .transpose()?;
            let mut seen = BTreeSet::new();
            let mut capabilities = Vec::with_capacity(runtime.capabilities.len());
            for capability in &runtime.capabilities {
                let prepared = match capability {
                    SourceNativeCapability::Tools => PreparedNativeCapability::Tools,
                    SourceNativeCapability::Hooks => PreparedNativeCapability::Hooks,
                    SourceNativeCapability::Rewriters => PreparedNativeCapability::Rewriters,
                };
                if !seen.insert(prepared) {
                    return Err(invalid(format!(
                        "runtime.capabilities: duplicate capability `{}`",
                        prepared.as_str()
                    )));
                }
                capabilities.push(prepared);
            }
            capabilities.sort();
            let native_library_resource = library.as_ref().map(|path| SourceResource {
                id: "runtime".to_string(),
                path: path.clone(),
                aliases: Vec::new(),
            });
            Ok(ResolvedRuntime {
                prepared: Some(PreparedRuntime {
                    kind: PreparedRuntimeKind::Native,
                    mode: None,
                    entry: None,
                    abi_version: Some(abi_version),
                    crate_dir,
                    library,
                    capabilities,
                }),
                process: None,
                js_entry_resource: None,
                native_library_resource,
            })
        }
    }
}

fn normalize_js_entry(bundle_id: &str, path: &str) -> Result<String, BundleError> {
    let normalized = normalize_source_path(bundle_id, path)?;
    if ![".ts", ".js", ".mjs", ".cjs"]
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
    {
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: format!("runtime.entry `{path}` must end with .ts, .js, .mjs, or .cjs"),
        });
    }
    Ok(normalized)
}

/// Validate a decoded bundle's optional `PreparedRuntime` for canonical invariants.
pub(super) fn validate_prepared_runtime(bundle: &Bundle) -> bool {
    let Some(runtime) = bundle.runtime() else {
        return true;
    };
    match runtime.kind {
        PreparedRuntimeKind::Static => {
            runtime.mode.is_none()
                && runtime.entry.is_none()
                && runtime.abi_version.is_none()
                && runtime.crate_dir.is_none()
                && runtime.library.is_none()
                && runtime.capabilities.is_empty()
                && bundle.process.is_none()
        }
        PreparedRuntimeKind::Js => {
            if runtime.abi_version.is_some()
                || runtime.crate_dir.is_some()
                || runtime.library.is_some()
                || !runtime.capabilities.is_empty()
            {
                return false;
            }
            let entry_ok = runtime.entry.as_deref().is_none_or(|entry| {
                normalize_js_entry(&bundle.identity().id, entry).as_deref() == Ok(entry)
                    && bundle
                        .extensions()
                        .iter()
                        .any(|res| res.source_path == entry && res.binary_base64.is_none())
            });
            if !entry_ok {
                return false;
            }
            match runtime.mode {
                Some(PreparedJsRuntimeMode::Managed) => {
                    runtime.entry.is_some() && bundle.process.is_none()
                }
                Some(PreparedJsRuntimeMode::Process) => bundle
                    .process
                    .as_ref()
                    .is_some_and(|proc| proc.kind == PreparedProcessKind::Bun),
                Some(PreparedJsRuntimeMode::Claude) => {
                    runtime.entry.is_none()
                        && bundle
                            .process
                            .as_ref()
                            .is_some_and(|proc| proc.kind == PreparedProcessKind::Claude)
                }
                None => false,
            }
        }
        PreparedRuntimeKind::Native => {
            if runtime.mode.is_some()
                || runtime.entry.is_some()
                || bundle.process.is_some()
                || !matches!(runtime.abi_version, Some(1 | 2))
            {
                return false;
            }
            if !runtime
                .capabilities
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            {
                return false;
            }
            if let Some(crate_dir) = &runtime.crate_dir
                && normalize_source_path(&bundle.identity().id, crate_dir).as_deref()
                    != Ok(crate_dir.as_str())
            {
                return false;
            }
            if let Some(library) = &runtime.library {
                if normalize_source_path(&bundle.identity().id, library).as_deref()
                    != Ok(library.as_str())
                {
                    return false;
                }
                if !bundle.extensions().iter().any(|res| {
                    res.source_path == *library
                        && res.binary_base64.is_some()
                        && res.stable_id.contains("/library/")
                }) {
                    return false;
                }
            }
            true
        }
    }
}

/// Validate and lift the manifest's optional `extensions.process` declaration.
pub(super) fn declared_process_extension(
    bundle_id: &str,
    extensions: &SourceExtensions,
) -> Result<Option<PreparedProcessExtension>, BundleError> {
    let Some(process) = &extensions.process else {
        return Ok(None);
    };
    if let Err(detail) = validate_process(&process.command, process.timeout_ms) {
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
        timeout_ms: process.timeout_ms,
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
