//! Compat workspace-adapter descriptors exposed to clients.

use serde::{Deserialize, Serialize};

/// One discovered workspace adapter (Compat project/workspace surface).
#[deprecated(
    note = "Workspace adapter contributions are unsupported and retained for compatibility"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAdapterInfo {
    /// Adapter kind discriminator on the wire (`type` JSON key).
    #[serde(rename = "type")]
    pub r#type: String,
    /// Human-readable adapter name for listing UIs.
    pub name: String,
    /// Optional longer description; empty when the adapter does not supply one.
    #[serde(default)]
    pub description: String,
}
