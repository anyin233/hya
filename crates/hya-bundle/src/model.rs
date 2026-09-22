//! Prepared installable bundle value types and immutable catalog documents.
//!
//! These types are what preparation emits and what runtime catalogs index.
//! Source-only shapes live in `source.rs` and are not re-exported.

use std::collections::BTreeMap;

use hya_proto::AgentName;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

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

/// Bundle payload kind in a prepared document.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PreparedBundleKind {
    /// A singular AgentBundle payload.
    AgentBundle,
    /// A closed set of Agents without a Workflow.
    AgentSetBundle,
    /// A WorkflowBundle payload containing one Workflow and its Agent closure.
    WorkflowBundle,
}

impl PreparedBundleKind {
    /// Return the exact serialized payload kind tag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentBundle => "AgentBundle",
            Self::AgentSetBundle => "AgentSetBundle",
            Self::WorkflowBundle => "WorkflowBundle",
        }
    }
}

impl std::fmt::Display for PreparedBundleKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Bundle identity block from a manifest (`identity:`).
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

/// One prepared Agent: id, prompt material, spawn graph, and resource view.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedAgent {
    /// Stable agent id. Also addressable as `bundle:{bundle_id}/agent/{id}`.
    pub id: AgentName,
    /// Optional human description for selectors.
    pub description: Option<String>,
    /// Main vs subagent selector role.
    pub role: AgentRole,
    /// Optional UI color hint.
    pub color: Option<String>,
    /// Resolved prompt text when embedded; `None` when no prompt was declared.
    pub prompt: Option<String>,
    /// Relative path the prompt was loaded from, when applicable.
    pub prompt_source: Option<String>,
    /// SHA-256 (hex) of the embedded prompt bytes for integrity checks.
    pub prompt_digest: Option<String>,
    /// Model/category/reasoning overrides.
    pub model_policy: ModelPolicy,
    /// Optional workdir override for turns of this agent.
    pub workdir: Option<String>,
    /// Transient vs resident when Harness spawns this entry.
    pub spawn_lifecycle: SpawnLifecycle,
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

/// Fully prepared singular AgentBundle: exactly one Agent plus its resources.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedAgentBundle {
    /// Prepared-document format version (currently `2`).
    pub format_version: u32,
    /// Bundle identity block.
    pub identity: BundleIdentity,
    /// Provider-facing namespace (declared or identity-derived). Skipped when
    /// absent so prepared documents written before namespaces stay decodable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Digest of this bundle's canonical content for integrity checks.
    pub digest: String,
    /// The single Agent this bundle defines.
    pub agent: PreparedAgent,
    /// Prepared tool resources.
    pub tools: Vec<PreparedResource>,
    /// Prepared Skill resources.
    pub skills: Vec<PreparedResource>,
    /// Prepared MCP declarations (`resources.mcp`); each entry's content is a
    /// validated `McpServerConfig`-shaped JSON file. Runtime spawning of these
    /// servers lands in a later phase.
    pub mcp: Vec<PreparedResource>,
    /// Prepared hook resources.
    pub hooks: Vec<PreparedResource>,
    /// Prepared JS/Rust extension entrypoints.
    pub extensions: Vec<PreparedResource>,
}

/// Fully prepared AgentSetBundle: multiple Agents sharing one resource plane.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedAgentSetBundle {
    /// Prepared-document format version (currently `2`).
    pub format_version: u32,
    /// Bundle identity block.
    pub identity: BundleIdentity,
    /// Provider-facing namespace (declared or identity-derived).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Digest of this bundle's canonical content for integrity checks.
    pub digest: String,
    /// All Agents this bundle defines.
    pub agents: Vec<PreparedAgent>,
    /// Prepared tool resources.
    pub tools: Vec<PreparedResource>,
    /// Prepared Skill resources.
    pub skills: Vec<PreparedResource>,
    /// Prepared MCP declarations.
    pub mcp: Vec<PreparedResource>,
    /// Prepared hook resources.
    pub hooks: Vec<PreparedResource>,
    /// Prepared JS/Rust extension entrypoints.
    pub extensions: Vec<PreparedResource>,
}

/// One compiled Workflow source retained in a prepared WorkflowBundle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedWorkflow {
    /// Manifest-local Workflow identifier.
    pub id: String,
    /// Canonical source path inside the bundle.
    pub source_path: String,
    /// Complete UTF-8 Workflow Markdown source.
    pub source: String,
    /// SHA-256 (hex) digest of the complete source bytes.
    pub source_digest: String,
    /// Hex digest of the normalized `hya-workflow` compiler revision.
    pub compiler_revision: String,
}

/// Fully prepared WorkflowBundle: one Workflow and its exact Agent closure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedWorkflowBundle {
    /// Prepared-document format version (currently `2`).
    pub format_version: u32,
    /// Bundle identity block.
    pub identity: BundleIdentity,
    /// Provider-facing namespace (declared or identity-derived). Skipped when
    /// absent so prepared documents written before namespaces stay decodable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Digest of this bundle's canonical content for integrity checks.
    pub digest: String,
    /// The one compiled Workflow this bundle defines.
    pub workflow: PreparedWorkflow,
    /// All packaged Agents reachable from the Workflow's stages and spawn graph.
    pub agents: Vec<PreparedAgent>,
    /// Prepared tool resources.
    pub tools: Vec<PreparedResource>,
    /// Prepared Skill resources.
    pub skills: Vec<PreparedResource>,
    /// Prepared MCP declarations (`resources.mcp`); each entry's content is a
    /// validated `McpServerConfig`-shaped JSON file. Runtime spawning of these
    /// servers lands in a later phase.
    pub mcp: Vec<PreparedResource>,
    /// Prepared hook resources.
    pub hooks: Vec<PreparedResource>,
    /// Prepared JS/Rust extension entrypoints.
    pub extensions: Vec<PreparedResource>,
}

/// Closed prepared payload union for one installable bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreparedInstallableBundle {
    /// Singular AgentBundle payload.
    Agent(Box<PreparedAgentBundle>),
    /// AgentSetBundle payload with multiple Agents and no Workflow.
    AgentSet(Box<PreparedAgentSetBundle>),
    /// WorkflowBundle payload with one Workflow and Agent closure.
    Workflow(Box<PreparedWorkflowBundle>),
}

impl PreparedInstallableBundle {
    /// Resolved provider-facing namespace: the declared value, or the
    /// identity name segment when the prepared document predates namespaces.
    #[must_use]
    pub fn namespace(&self) -> &str {
        let (declared, id) = match self {
            Self::Agent(bundle) => (bundle.namespace.as_deref(), bundle.identity.id.as_str()),
            Self::AgentSet(bundle) => (bundle.namespace.as_deref(), bundle.identity.id.as_str()),
            Self::Workflow(bundle) => (bundle.namespace.as_deref(), bundle.identity.id.as_str()),
        };
        declared.unwrap_or_else(|| id.rsplit('/').next().unwrap_or_default())
    }
}

impl Serialize for PreparedInstallableBundle {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct Tagged<'a, T> {
            kind: &'static str,
            #[serde(flatten)]
            bundle: &'a T,
        }

        match self {
            Self::Agent(bundle) => Tagged {
                kind: PreparedBundleKind::AgentBundle.as_str(),
                bundle,
            }
            .serialize(serializer),
            Self::AgentSet(bundle) => Tagged {
                kind: PreparedBundleKind::AgentSetBundle.as_str(),
                bundle,
            }
            .serialize(serializer),
            Self::Workflow(bundle) => Tagged {
                kind: PreparedBundleKind::WorkflowBundle.as_str(),
                bundle,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for PreparedInstallableBundle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut object = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        let kind = object
            .remove("kind")
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| de::Error::custom("prepared bundle is missing string `kind`"))?;
        let value = serde_json::Value::Object(object);
        match kind.as_str() {
            "AgentBundle" => serde_json::from_value::<PreparedAgentBundle>(value)
                .map(Box::new)
                .map(Self::Agent)
                .map_err(de::Error::custom),
            "AgentSetBundle" => serde_json::from_value::<PreparedAgentSetBundle>(value)
                .map(Box::new)
                .map(Self::AgentSet)
                .map_err(de::Error::custom),
            "WorkflowBundle" => serde_json::from_value::<PreparedWorkflowBundle>(value)
                .map(Box::new)
                .map(Self::Workflow)
                .map_err(de::Error::custom),
            other => Err(de::Error::custom(format!(
                "unsupported prepared bundle kind `{other}`"
            ))),
        }
    }
}

impl PreparedInstallableBundle {
    /// Return the closed payload kind.
    #[must_use]
    pub const fn kind(&self) -> PreparedBundleKind {
        match self {
            Self::Agent(_) => PreparedBundleKind::AgentBundle,
            Self::AgentSet(_) => PreparedBundleKind::AgentSetBundle,
            Self::Workflow(_) => PreparedBundleKind::WorkflowBundle,
        }
    }

    /// Return this payload's bundle identity.
    #[must_use]
    pub fn identity(&self) -> &BundleIdentity {
        match self {
            Self::Agent(bundle) => &bundle.identity,
            Self::AgentSet(bundle) => &bundle.identity,
            Self::Workflow(bundle) => &bundle.identity,
        }
    }

    /// Return this payload's canonical content digest.
    #[must_use]
    pub fn digest(&self) -> &str {
        match self {
            Self::Agent(bundle) => &bundle.digest,
            Self::AgentSet(bundle) => &bundle.digest,
            Self::Workflow(bundle) => &bundle.digest,
        }
    }

    /// Return every Agent published by this payload.
    #[must_use]
    pub fn agents(&self) -> &[PreparedAgent] {
        match self {
            Self::Agent(bundle) => std::slice::from_ref(&bundle.agent),
            Self::AgentSet(bundle) => &bundle.agents,
            Self::Workflow(bundle) => &bundle.agents,
        }
    }

    /// Return this payload's resources of one exported kind.
    #[must_use]
    pub fn resources(&self, kind: crate::ExportKind) -> &[PreparedResource] {
        match kind {
            crate::ExportKind::Tool => self.tools(),
            crate::ExportKind::Skill => self.skills(),
            crate::ExportKind::Mcp => self.mcp(),
            crate::ExportKind::Hook => self.hooks(),
            crate::ExportKind::Extension => self.extensions(),
        }
    }

    /// Return prepared tool resources.
    #[must_use]
    pub fn tools(&self) -> &[PreparedResource] {
        match self {
            Self::Agent(bundle) => &bundle.tools,
            Self::AgentSet(bundle) => &bundle.tools,
            Self::Workflow(bundle) => &bundle.tools,
        }
    }

    /// Return prepared Skill resources.
    #[must_use]
    pub fn skills(&self) -> &[PreparedResource] {
        match self {
            Self::Agent(bundle) => &bundle.skills,
            Self::AgentSet(bundle) => &bundle.skills,
            Self::Workflow(bundle) => &bundle.skills,
        }
    }

    /// Return prepared MCP resources.
    #[must_use]
    pub fn mcp(&self) -> &[PreparedResource] {
        match self {
            Self::Agent(bundle) => &bundle.mcp,
            Self::AgentSet(bundle) => &bundle.mcp,
            Self::Workflow(bundle) => &bundle.mcp,
        }
    }

    /// Return prepared hook resources.
    #[must_use]
    pub fn hooks(&self) -> &[PreparedResource] {
        match self {
            Self::Agent(bundle) => &bundle.hooks,
            Self::AgentSet(bundle) => &bundle.hooks,
            Self::Workflow(bundle) => &bundle.hooks,
        }
    }

    /// Return prepared JS/Rust extension resources.
    #[must_use]
    pub fn extensions(&self) -> &[PreparedResource] {
        match self {
            Self::Agent(bundle) => &bundle.extensions,
            Self::AgentSet(bundle) => &bundle.extensions,
            Self::Workflow(bundle) => &bundle.extensions,
        }
    }

    /// Return the Workflow metadata for a WorkflowBundle, if this is one.
    #[must_use]
    pub fn workflow(&self) -> Option<&PreparedWorkflow> {
        match self {
            Self::Agent(_) | Self::AgentSet(_) => None,
            Self::Workflow(bundle) => Some(&bundle.workflow),
        }
    }

    /// Return the singular AgentBundle payload, if this is one.
    #[must_use]
    pub fn agent_bundle(&self) -> Option<&PreparedAgentBundle> {
        match self {
            Self::Agent(bundle) => Some(bundle),
            Self::AgentSet(_) | Self::Workflow(_) => None,
        }
    }

    /// Return the AgentSetBundle payload, if this is one.
    #[must_use]
    pub fn agent_set_bundle(&self) -> Option<&PreparedAgentSetBundle> {
        match self {
            Self::AgentSet(bundle) => Some(bundle),
            Self::Agent(_) | Self::Workflow(_) => None,
        }
    }

    /// Return the WorkflowBundle payload, if this is one.
    #[must_use]
    pub fn workflow_bundle(&self) -> Option<&PreparedWorkflowBundle> {
        match self {
            Self::Agent(_) | Self::AgentSet(_) => None,
            Self::Workflow(bundle) => Some(bundle),
        }
    }
}

/// One prepared external URI-scheme extension declared by a bundle.
///
/// `tool` is the bundle-local tool id (e.g. `query` for `bundle:<id>/tool/query`)
/// that serves the scheme; the runtime resolves it to that tool's canonical
/// qualified name when the source publishes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedSchema {
    /// Scheme text as it appears before `://` (e.g. `db`).
    pub scheme: String,
    /// Bundle-local id of the tool that serves the scheme.
    pub tool: String,
    /// Whether the scheme accepts writes in addition to reads.
    pub writable: bool,
}

/// Per-bundle schema list in a prepared catalog document.
///
/// Prepared bundles carry their `schemas:` declarations as a document-level
/// section parallel to the index (skipped entirely when no bundle declares
/// any), so documents written before the section stay byte-identical and
/// decodable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedBundleSchemas {
    /// Bundle identity id the schemas belong to.
    pub bundle_id: String,
    /// Declared schema extensions, sorted by scheme.
    pub schemas: Vec<PreparedSchema>,
}

/// Runtime kind that executes a bundle's declared process extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparedProcessKind {
    /// Native Rust plugin process.
    Rust,
    /// Bun/JavaScript sidecar process.
    Bun,
    /// Claude Code compatible adapter process.
    Claude,
}

impl PreparedProcessKind {
    /// Exact manifest spelling of this kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Bun => "bun",
            Self::Claude => "claude",
        }
    }
}

/// One declared out-of-process extension: the runtime kind and the argv that
/// starts it. Declared and validated at prepare time; spawning is wired in a
/// later phase.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedProcessExtension {
    /// Runtime kind that executes the command.
    pub kind: PreparedProcessKind,
    /// Argv to spawn (`command[0]` is the program).
    pub command: Vec<String>,
}

/// Per-bundle process-extension row in a prepared catalog document.
///
/// Like the `schemas:` section this is a document-level list parallel to the
/// index (skipped entirely when no bundle declares one) so documents written
/// before the section stay byte-identical and decodable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedBundleProcess {
    /// Bundle identity id the declaration belongs to.
    pub bundle_id: String,
    /// The declared process extension.
    pub process: PreparedProcessExtension,
}

/// Compact index row for one bundle in a prepared catalog.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedBundleIndex {
    /// Bundle identity id.
    pub bundle_id: String,
    /// Bundle version string.
    pub version: String,
    /// Bundle content digest.
    pub digest: String,
    /// Stable ids of all Agents this bundle exports.
    pub agent_ids: Vec<AgentName>,
    /// Local ids of all Workflows this bundle exports (zero or one).
    pub workflow_ids: Vec<String>,
}

/// Canonical prepared multi-bundle document: decoded payloads, index, raw bytes, and digest.
#[derive(Debug)]
pub struct PreparedCatalog {
    pub(crate) bundles: Vec<PreparedInstallableBundle>,
    pub(crate) index: Vec<PreparedBundleIndex>,
    pub(crate) schemas: Vec<PreparedBundleSchemas>,
    pub(crate) process_extensions: Vec<PreparedBundleProcess>,
    pub(crate) bytes: Vec<u8>,
    pub(crate) digest: String,
}

impl PreparedCatalog {
    /// Prepared installable payloads in catalog order.
    #[must_use]
    pub fn bundles(&self) -> &[PreparedInstallableBundle] {
        &self.bundles
    }

    /// Compact index parallel to the prepared document.
    #[must_use]
    pub fn index(&self) -> &[PreparedBundleIndex] {
        &self.index
    }

    /// Per-bundle schema-extension declarations, sorted by bundle id.
    #[must_use]
    pub fn schemas(&self) -> &[PreparedBundleSchemas] {
        &self.schemas
    }

    /// The schema extensions one bundle declares, or an empty slice.
    #[must_use]
    pub fn bundle_schemas(&self, bundle_id: &str) -> &[PreparedSchema] {
        self.schemas
            .iter()
            .find(|row| row.bundle_id == bundle_id)
            .map(|row| row.schemas.as_slice())
            .unwrap_or(&[])
    }

    /// Per-bundle process-extension declarations, sorted by bundle id.
    #[must_use]
    pub fn process_extensions(&self) -> &[PreparedBundleProcess] {
        &self.process_extensions
    }

    /// The process extension one bundle declares, if any.
    #[must_use]
    pub fn bundle_process(&self, bundle_id: &str) -> Option<&PreparedProcessExtension> {
        self.process_extensions
            .iter()
            .find(|row| row.bundle_id == bundle_id)
            .map(|row| &row.process)
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
    pub bundles: &'a [PreparedInstallableBundle],
    pub index: &'a [PreparedBundleIndex],
    /// Per-bundle schema declarations; skipped when no bundle declares any so
    /// documents written before the section keep their exact byte layout.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub schemas: Vec<PreparedBundleSchemas>,
    /// Per-bundle `extensions.process` declarations; skipped when no bundle
    /// declares any, for the same byte-layout reason.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions_process: Vec<PreparedBundleProcess>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedDocumentOwned {
    pub format_version: u32,
    pub bundles: Vec<PreparedInstallableBundle>,
    pub index: Vec<PreparedBundleIndex>,
    #[serde(default)]
    pub schemas: Vec<PreparedBundleSchemas>,
    #[serde(default)]
    pub extensions_process: Vec<PreparedBundleProcess>,
}
