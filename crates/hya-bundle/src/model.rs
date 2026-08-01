use std::collections::BTreeMap;

use hya_proto::AgentName;
use serde::{Deserialize, Serialize};

/// Source role. It controls selector visibility only.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Main,
    Subagent,
}

impl AgentRole {
    /// Compat/TUI selector mode for this role.
    ///
    /// `main` → `"primary"`, `subagent` → `"subagent"`. Role remains the sole
    /// selector rule; callers must not invent a parallel mode source.
    #[must_use]
    pub const fn selector_mode(self) -> &'static str {
        match self {
            Self::Main => "primary",
            Self::Subagent => "subagent",
        }
    }
}

/// Lifecycle used only when Harness spawns the catalog entry.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnLifecycle {
    #[default]
    Transient,
    Resident,
}

/// Which Harness-owned resources enter the candidate view.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessAccess {
    None,
    Basic,
    Full,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleIdentity {
    pub id: String,
    pub version: String,
    pub publisher: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleOrigin {
    Builtin,
    Installed,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPolicy {
    pub model: Option<String>,
    pub category: Option<String>,
    pub reasoning: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceView {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub aliases: BTreeMap<String, String>,
    pub namespace: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedAgent {
    pub local_id: String,
    pub stable_id: AgentName,
    pub description: Option<String>,
    pub role: AgentRole,
    pub color: Option<String>,
    pub prompt: Option<String>,
    pub prompt_source: Option<String>,
    pub prompt_digest: Option<String>,
    pub model_policy: ModelPolicy,
    pub workdir: Option<String>,
    pub spawn_lifecycle: SpawnLifecycle,
    pub harness_access: HarnessAccess,
    pub resource_view: ResourceView,
    pub can_spawn: Vec<AgentName>,
    pub hook_refs: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedResource {
    pub local_id: String,
    pub stable_id: String,
    pub source_path: String,
    pub digest: String,
    pub content: String,
    pub aliases: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedBundle {
    pub format_version: u32,
    pub identity: BundleIdentity,
    pub origin: BundleOrigin,
    pub immutable: bool,
    pub digest: String,
    pub agents: Vec<PreparedAgent>,
    pub tools: Vec<PreparedResource>,
    pub skills: Vec<PreparedResource>,
    pub mcp: Vec<PreparedResource>,
    pub hooks: Vec<PreparedResource>,
    pub extensions: Vec<PreparedResource>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedBundleIndex {
    pub bundle_id: String,
    pub version: String,
    pub digest: String,
    pub stable_agent_ids: Vec<AgentName>,
}

#[derive(Debug)]
pub struct PreparedCatalog {
    pub(crate) bundles: Vec<PreparedBundle>,
    pub(crate) index: Vec<PreparedBundleIndex>,
    pub(crate) bytes: Vec<u8>,
    pub(crate) digest: String,
}

impl PreparedCatalog {
    #[must_use]
    pub fn bundles(&self) -> &[PreparedBundle] {
        &self.bundles
    }

    #[must_use]
    pub fn index(&self) -> &[PreparedBundleIndex] {
        &self.index
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

#[derive(Serialize)]
pub(crate) struct PreparedDocument<'a> {
    pub format_version: u32,
    pub bundles: &'a [PreparedBundle],
    pub index: &'a [PreparedBundleIndex],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedDocumentOwned {
    pub format_version: u32,
    pub bundles: Vec<PreparedBundle>,
    pub index: Vec<PreparedBundleIndex>,
}
