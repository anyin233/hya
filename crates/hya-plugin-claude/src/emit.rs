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
    use super::parse_manifest_emit;

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
