//! Offline `--emit-bundle-manifest` contract between the Claude adapter and
//! `hya bundle install --claude`.
//!
//! The adapter prints exactly one JSON document on stdout:
//!
//! ```json
//! {
//!   "manifest": "kind: Plugin\n...",
//!   "files": [{ "path": "skills/review.md", "content": "…" }]
//! }
//! ```
//!
//! `manifest` is a complete hya `Plugin` or `AgentSetBundle` source manifest
//! (`bundle.yaml`)
//! whose `resources.*[].path` references the translated files; `files` carries
//! those translated file contents verbatim. The Rust side materializes both
//! into a [`hya_bundle::BundleSource`] and installs through the normal
//! prepare/package/registry path, so digests are computed by the canonical
//! preparer rather than duplicated here.

use serde::Deserialize;

/// Reserved namespace tokens, mirroring `hya-bundle`'s prepare policy.
pub const RESERVED_NAMESPACES: [&str; 4] = ["mcp", "harness", "builtin", "plugin"];

/// One translated file emitted by the adapter.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ManifestFile {
    /// `/`-separated path relative to the bundle source root.
    pub path: String,
    /// UTF-8 content of the translated file.
    pub content: String,
}

/// The complete `--emit-bundle-manifest` stdout envelope.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ManifestEmit {
    /// Complete standard bundle source manifest (YAML) referencing `files`.
    pub manifest: String,
    /// Translated resource files referenced by `manifest`.
    #[serde(default)]
    pub files: Vec<ManifestFile>,
}

/// Why a plugin name could not become a namespace token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceError {
    /// The sanitized name collapsed to an empty token.
    Empty,
    /// The sanitized name is a reserved namespace token.
    Reserved(String),
}

impl std::fmt::Display for NamespaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(formatter, "namespace is empty after sanitization"),
            Self::Reserved(namespace) => {
                write!(formatter, "namespace `{namespace}` is reserved")
            }
        }
    }
}

impl std::error::Error for NamespaceError {}

/// Sanitize a Claude Code plugin name into a hya namespace token.
///
/// Lowercases, maps every character outside `[a-z0-9_-]` to `-`, collapses
/// repeats, trims leading/trailing `-`, then validates against the same rules
/// as the `hya-bundle` preparer: non-empty, no `__`, and not reserved.
///
/// # Errors
///
/// Returns [`NamespaceError`] when sanitization collapses to an empty token,
/// produces the `__` separator, or lands on a reserved namespace.
pub fn sanitize_namespace(name: &str) -> Result<String, NamespaceError> {
    // The `__` tool-plane separator is not claimable; collapse it like any
    // other non-token punctuation instead of failing the install later.
    let collapsed = name.replace("__", "-");
    let mut token = String::with_capacity(collapsed.len());
    let mut last_dash = false;
    for character in collapsed.chars() {
        let mapped = if character.is_ascii_alphanumeric() || character == '_' {
            character.to_ascii_lowercase()
        } else {
            '-'
        };
        if mapped == '-' && last_dash {
            continue;
        }
        last_dash = mapped == '-';
        token.push(mapped);
    }
    while token.starts_with('-') {
        token.remove(0);
    }
    while token.ends_with('-') {
        token.pop();
    }
    if token.is_empty() {
        return Err(NamespaceError::Empty);
    }
    if RESERVED_NAMESPACES.contains(&token.as_str()) {
        return Err(NamespaceError::Reserved(token));
    }
    Ok(token)
}

/// Parse the adapter's `--emit-bundle-manifest` stdout envelope.
///
/// The adapter may emit trailing diagnostics-free whitespace only; any other
/// trailing content is a protocol violation.
///
/// # Errors
///
/// Returns a descriptive error when stdout is not exactly one envelope object.
pub fn parse_manifest_emit(stdout: &str) -> Result<ManifestEmit, String> {
    let trimmed = stdout.trim();
    serde_json::from_str(trimmed)
        .map_err(|error| format!("invalid --emit-bundle-manifest envelope: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{NamespaceError, parse_manifest_emit, sanitize_namespace};

    #[test]
    fn sanitizes_plugin_names_into_namespace_tokens() {
        assert_eq!(
            sanitize_namespace("Code Review").as_deref(),
            Ok("code-review")
        );
        assert_eq!(sanitize_namespace("My__Plugin").as_deref(), Ok("my-plugin"));
        assert_eq!(sanitize_namespace("  --v2--  ").as_deref(), Ok("v2"));
        assert_eq!(sanitize_namespace("proj.x+y").as_deref(), Ok("proj-x-y"));
    }

    #[test]
    fn sanitize_rejects_empty_and_reserved_tokens() {
        assert_eq!(sanitize_namespace("///"), Err(NamespaceError::Empty));
        assert_eq!(sanitize_namespace("工具"), Err(NamespaceError::Empty));
        assert_eq!(
            sanitize_namespace("mcp"),
            Err(NamespaceError::Reserved("mcp".to_string()))
        );
        // The `__` separator collapses into `-` instead of failing.
        assert_eq!(sanitize_namespace("my__keep").as_deref(), Ok("my-keep"));
    }

    #[test]
    fn parses_the_emit_envelope() {
        let emit = parse_manifest_emit(
            r#"{"manifest": "kind: AgentBundle\n", "files": [{"path": "skills/a.md", "content": "A"}]}"#,
        )
        .unwrap_or_else(|error| panic!("envelope must parse: {error}"));
        assert_eq!(emit.manifest, "kind: AgentBundle\n");
        assert_eq!(emit.files.len(), 1);
        assert_eq!(emit.files[0].path, "skills/a.md");
        assert_eq!(emit.files[0].content, "A");
    }

    #[test]
    fn parse_rejects_non_envelope_stdout() {
        assert!(parse_manifest_emit("").is_err());
        assert!(parse_manifest_emit("hello").is_err());
        assert!(parse_manifest_emit("{}").is_err(), "manifest is required");
        assert!(
            parse_manifest_emit("{\"manifest\": \"x\"} extra").is_err(),
            "trailing content after the envelope is a protocol violation"
        );
    }
}
