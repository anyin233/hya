//! Namespaced tool names (`namespace__local`).
//!
//! Provider tool-name charsets (OpenAI/Anthropic function names) allow only
//! `[a-zA-Z0-9_-]`, so namespaces ride on a double-underscore separator — the
//! same convention MCP tools already use model-facing (`mcp__server__tool`).
//! A namespace groups tools from one provider of functionality; the local
//! name may repeat across namespaces (`todo__read` vs `pluginx__read`) while
//! the full name stays globally unique inside a registry.

use thiserror::Error;

/// Separator between namespace and local tool name.
pub const NAMESPACE_SEPARATOR: &str = "__";

/// A namespace or local token was not a legal namespaced-name component.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
#[error(
    "invalid namespaced tool name `{namespace}__{local}`: tokens must be non-empty, use only [a-zA-Z0-9_-], and not contain `{NAMESPACE_SEPARATOR}`"
)]
pub struct InvalidNamespacedName {
    /// Namespace token as provided.
    pub namespace: String,
    /// Local name token as provided.
    pub local: String,
}

/// Compose the canonical model-facing name for a namespaced tool.
///
/// # Errors
/// Returns [`InvalidNamespacedName`] when either token is empty, contains the
/// [`NAMESPACE_SEPARATOR`], or uses characters outside `[a-zA-Z0-9_-]`.
pub fn namespaced_name(namespace: &str, local: &str) -> Result<String, InvalidNamespacedName> {
    if !valid_token(namespace) || !valid_token(local) {
        return Err(InvalidNamespacedName {
            namespace: namespace.to_string(),
            local: local.to_string(),
        });
    }
    Ok(format!("{namespace}{NAMESPACE_SEPARATOR}{local}"))
}

/// Return the namespace segment of a namespaced name (first-segment rule).
///
/// Plain names and degenerate spellings (`__x`, `x__`) map to `None`.
/// MCP-style nested names (`mcp__server__tool`) resolve to `Some("mcp")`.
#[must_use]
pub fn namespace_of(name: &str) -> Option<&str> {
    let (namespace, local) = name.split_once(NAMESPACE_SEPARATOR)?;
    if namespace.is_empty() || local.is_empty() {
        return None;
    }
    Some(namespace)
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && !token.contains(NAMESPACE_SEPARATOR)
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
