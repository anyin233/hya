//! Namespaced tool names (`namespace__local`).
//!
//! Provider tool-name charsets (OpenAI/Anthropic function names) allow only
//! `[a-zA-Z0-9_-]`, so namespaces ride on a double-underscore separator — the
//! same convention MCP tools already use model-facing (`mcp__server__tool`).
//! A namespace groups tools from one provider of functionality; the local
//! name may repeat across namespaces (`todo__read` vs `pluginx__read`) while
//! the full name stays globally unique inside a registry.

use std::sync::Arc;

use thiserror::Error;

use crate::ToolPermission;
use crate::tool::{DuplicateName, Tool, ToolRegistry};

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

/// A namespaced registration failed before the tool entered the registry.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum NamespacedRegisterError {
    /// The namespace/local tokens do not form a legal name.
    #[error(transparent)]
    Invalid(#[from] InvalidNamespacedName),
    /// The tool's own name disagrees with the composed canonical name, which
    /// would leave the advertised schema and the registry key out of sync.
    #[error("tool reports name `{actual}` but namespace registration composes `{expected}`")]
    NameMismatch {
        /// Composed canonical name from the namespace and local tokens.
        expected: String,
        /// Name the tool itself reports.
        actual: String,
    },
    /// The composed canonical name is already taken in this registry.
    #[error(transparent)]
    Duplicate(#[from] DuplicateName),
}

impl ToolRegistry {
    /// Register a tool under `namespace__local`, validating the tokens and
    /// that the tool's own name matches the composed canonical name.
    ///
    /// # Errors
    /// Returns [`NamespacedRegisterError`] for invalid tokens, a name
    /// mismatch, or a duplicate canonical name.
    pub fn register_namespaced(
        &self,
        namespace: &str,
        local_name: &str,
        tool: Arc<dyn Tool>,
    ) -> Result<(), NamespacedRegisterError> {
        self.register_namespaced_with_permission(namespace, local_name, tool, ToolPermission::Tool)
    }

    /// Namespaced registration with an explicit permission class.
    ///
    /// # Errors
    /// Returns [`NamespacedRegisterError`] for invalid tokens, a name
    /// mismatch, or a duplicate canonical name.
    pub fn register_namespaced_with_permission(
        &self,
        namespace: &str,
        local_name: &str,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
    ) -> Result<(), NamespacedRegisterError> {
        let expected = namespaced_name(namespace, local_name)?;
        let actual = tool.name();
        if actual != expected {
            return Err(NamespacedRegisterError::NameMismatch {
                expected,
                actual: actual.to_string(),
            });
        }
        self.register_with_permission(tool, permission)?;
        Ok(())
    }
}
