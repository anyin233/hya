use std::collections::BTreeMap;

use crate::error::BundleError;
use crate::source::{BundleSource, ParsedSource, SourceFile, SourceManifest};

fn resolve_version_reference(
    manifest: &mut SourceManifest,
    source_name: &str,
) -> Result<(), BundleError> {
    let (version_ref, identity) = (&mut manifest.version_ref, &mut manifest.identity);
    let Some(version_ref) = version_ref.take() else {
        return Ok(());
    };
    if version_ref != "hya" && version_ref != "backend" {
        return Err(BundleError::InvalidManifest {
            source_name: source_name.to_string(),
            detail: format!("unsupported version_ref `{version_ref}`; expected `hya` or `backend`"),
        });
    }
    if identity.version != "0.0.0" {
        return Err(BundleError::InvalidManifest {
            source_name: source_name.to_string(),
            detail: "version_ref requires identity.version 0.0.0".to_string(),
        });
    }
    identity.version = hya_version::VERSION.to_string();
    Ok(())
}

pub(super) fn parse_source(source: BundleSource) -> Result<ParsedSource, BundleError> {
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

    if let Some(version) = manifest.schema_version
        && version != 1
        && version != 2
    {
        return Err(BundleError::InvalidManifest {
            source_name: name,
            detail: format!("unsupported schema_version `{version}`; expected 1 or 2"),
        });
    }

    resolve_version_reference(&mut manifest, &name)?;

    let markdown_prompt = markdown_prompt.filter(|body| !body.is_empty());
    let single_inline_without_prompt = matches!(
        manifest.agents.as_slice(),
        [agent] if agent.path.is_none() && agent.prompt.is_none()
    );
    if markdown_prompt.is_some() && !single_inline_without_prompt {
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

pub(super) fn split_markdown(content: &str) -> Option<(&str, &str)> {
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
pub(super) fn resolve_namespace(
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

pub(super) fn validate_identity(bundle_id: &str, version: &str) -> Result<(), BundleError> {
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

pub(super) fn read_text_reference(
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
