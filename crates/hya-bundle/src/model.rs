//! Prepared AgentBundle value types: agents, resources, catalogs, and origin.
//!
//! These types are what prepare emits and what the runtime catalog indexes.
//! Source-only shapes live in `source.rs` and are not re-exported.

use std::collections::BTreeMap;

use hya_proto::AgentName;
use serde::{Deserialize, Serialize};

/// Source role. It controls selector visibility only.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Shown as a primary/main agent in selectors (`selector_mode` → `"primary"`).
    Main,
    /// Shown as a subagent-only entry (`selector_mode` → `"subagent"`).
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
    /// Blocking one-shot subagent (default when omitted in source).
    #[default]
    Transient,
    /// Long-lived mail-woken resident actor.
    Resident,
}

/// Which Harness-owned resources enter the candidate view.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessAccess {
    /// No Harness builtins/tools in the candidate view.
    None,
    /// Restricted built-in set (basic coding tools).
    Basic,
    /// Full Harness tool surface allowed by the agent resource view.
    Full,
}

/// Bundle identity block from the manifest (`identity:`).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleIdentity {
    /// Stable bundle id (catalog key and `bundle:{id}/…` namespace root).
    pub id: String,
    /// Semver-like version string for install/replace conflict checks.
    pub version: String,
    /// Publisher label for display and audit.
    pub publisher: String,
}

/// Whether a prepared bundle came from built-ins or an installable package.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleOrigin {
    /// Shipped with the binary / prepare_builtins path; treated as immutable.
    Builtin,
    /// User-installed package; may be replaced or uninstalled.
    Installed,
}

/// Optional model routing overrides for a prepared agent.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPolicy {
    /// Explicit model ref when set; otherwise the runtime default applies.
    pub model: Option<String>,
    /// Optional model category hint for routing.
    pub category: Option<String>,
    /// Optional reasoning effort string (provider-specific).
    pub reasoning: Option<String>,
}

/// Allow/deny/alias view of tools and resources available to one agent.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceView {
    /// Resource names or patterns explicitly allowed.
    pub allow: Vec<String>,
    /// Resource names or patterns explicitly denied (wins over allow where both match).
    pub deny: Vec<String>,
    /// Alias spelling → canonical resource name within this view.
    pub aliases: BTreeMap<String, String>,
    /// Optional namespace prefix for qualified names.
    pub namespace: Option<String>,
}

/// One agent after prepare: stable ids, prompt material, spawn graph, resource view.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedAgent {
    /// Bundle-local agent id (unique within the bundle).
    pub local_id: String,
    /// Global stable agent id used for spawn graphs and session binding.
    pub stable_id: AgentName,
    /// Optional human description for selectors.
    pub description: Option<String>,
    /// Main vs subagent selector role.
    pub role: AgentRole,
    /// Optional UI color hint.
    pub color: Option<String>,
    /// Resolved prompt text when embedded; `None` when only a file source was recorded.
    pub prompt: Option<String>,
    /// Relative path the prompt was loaded from, when applicable.
    pub prompt_source: Option<String>,
    /// SHA-256 (hex) of the prompt bytes for integrity checks.
    pub prompt_digest: Option<String>,
    /// Model/category/reasoning overrides.
    pub model_policy: ModelPolicy,
    /// Optional workdir override for turns of this agent.
    pub workdir: Option<String>,
    /// Transient vs resident when Harness spawns this entry.
    pub spawn_lifecycle: SpawnLifecycle,
    /// How much of the Harness tool plane is visible.
    pub harness_access: HarnessAccess,
    /// Per-agent allow/deny/alias resource view.
    pub resource_view: ResourceView,
    /// Stable agent ids this agent is allowed to spawn.
    pub can_spawn: Vec<AgentName>,
    /// Hook resource refs selected for this agent (exact-path join at activation).
    pub hook_refs: Vec<String>,
}

/// One tool/skill/mcp/hook/extension resource with embedded content and digests.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedResource {
    /// Bundle-local resource id.
    pub local_id: String,
    /// Globally qualified stable id (`bundle:{bundle}/kind/{local}`).
    pub stable_id: String,
    /// Normalized source path inside the package/source tree.
    pub source_path: String,
    /// Content digest covering the embedded bytes.
    pub digest: String,
    /// File contents as UTF-8 text (JSON/YAML/JS source as appropriate).
    pub content: String,
    /// Alternate local names that resolve to this resource inside the bundle.
    pub aliases: Vec<String>,
}

/// Fully prepared single AgentBundle: agents, resources, digests, origin flags.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedBundle {
    /// Prepared-document format version (currently `1`).
    pub format_version: u32,
    /// Bundle identity block.
    pub identity: BundleIdentity,
    /// Builtin vs installed provenance.
    pub origin: BundleOrigin,
    /// When true, uninstall/replace of this bundle id is rejected.
    pub immutable: bool,
    /// Digest of this bundle's canonical content for integrity checks.
    pub digest: String,
    /// Prepared agents in deterministic order.
    pub agents: Vec<PreparedAgent>,
    /// Prepared tool resources.
    pub tools: Vec<PreparedResource>,
    /// Prepared skill resources.
    pub skills: Vec<PreparedResource>,
    /// Prepared MCP resource declarations (catalog may still reject non-empty).
    pub mcp: Vec<PreparedResource>,
    /// Prepared hook resources.
    pub hooks: Vec<PreparedResource>,
    /// Prepared JS/Rust extension entrypoints.
    pub extensions: Vec<PreparedResource>,
}

/// Compact index row for one bundle in a prepared catalog (id, version, agents).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedBundleIndex {
    /// Bundle identity id.
    pub bundle_id: String,
    /// Bundle version string.
    pub version: String,
    /// Bundle content digest.
    pub digest: String,
    /// Stable agent ids exported by this bundle.
    pub stable_agent_ids: Vec<AgentName>,
}

/// Canonical prepared multi-bundle document: decoded bundles, index, raw bytes, digest.
///
/// Build via [`crate::prepare_builtins`] / [`crate::prepare_package`], or
/// [`PreparedCatalog::decode`]. Accessors expose slices without cloning.
#[derive(Debug)]
pub struct PreparedCatalog {
    pub(crate) bundles: Vec<PreparedBundle>,
    pub(crate) index: Vec<PreparedBundleIndex>,
    pub(crate) bytes: Vec<u8>,
    pub(crate) digest: String,
}

impl PreparedCatalog {
    /// Prepared bundles in catalog order.
    #[must_use]
    pub fn bundles(&self) -> &[PreparedBundle] {
        &self.bundles
    }

    /// Compact index parallel to the prepared document.
    #[must_use]
    pub fn index(&self) -> &[PreparedBundleIndex] {
        &self.index
    }

    /// Canonical JSON bytes of the prepared document (what the registry stores).
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// SHA-256 hex digest of [`Self::bytes`].
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
