//! Prepared installable bundle value types and immutable catalog documents.
//!
//! These types are what preparation emits and what runtime catalogs index.
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

/// Bundle payload kind in a prepared document.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PreparedBundleKind {
    /// One complete Bundle payload.
    Bundle,
}

impl PreparedBundleKind {
    /// Serialized bundle kind.
    pub const fn as_str(self) -> &'static str {
        "Bundle"
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
    /// Per-agent allow/deny/alias resource view.
    pub resource_view: ResourceView,
    /// Stable agent ids this agent is allowed to spawn.
    pub can_spawn: Vec<AgentName>,
    /// Hook resource refs selected for this agent (exact-path join at activation).
    pub hook_refs: Vec<String>,
}

/// Runtime channel topology represented by a declarative template.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelTemplateKind {
    /// One leader and its direct reports on the existing group broadcast pipe.
    Unit,
    /// The existing vertical direct-message pair between parent and child.
    ParentDm,
}

/// Participant topology role used by a channel template.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelParticipantRole {
    /// Leader of the unit that owns a group channel.
    UnitLeader,
    /// Direct reports in the unit that owns a group channel.
    DirectReports,
    /// Parent endpoint of a vertical DM.
    Parent,
    /// Child endpoint of a vertical DM.
    Child,
}

/// A local Agent definition or topology role selected by a channel template.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreparedChannelParticipant {
    /// One Agent declared in the same Bundle.
    Agent {
        /// Manifest-local Agent id.
        agent: String,
    },
    /// A runtime topology role.
    Role {
        /// Role resolved when the engine creates a concrete channel.
        role: ChannelParticipantRole,
    },
}

/// Capability ceiling declared by a channel template.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelCapability {
    /// Send ordinary mail through the channel.
    Send,
    /// Submit a terminal child report through the parent-child DM.
    Report,
    /// Surface unread channel mail as an in-turn steer.
    Steer,
    /// Deliver follow-up work after a member turn.
    FollowUp,
    /// Wake or deliver mail to resident members.
    ResidentMail,
}

/// Runtime hierarchy fence for a channel template.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelScope {
    /// A leader and its direct reports.
    Unit,
    /// One parent-child edge.
    Vertical,
}

/// Durable lifetime supported by the current event-sourced mailbox.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRetention {
    /// Retain events for the lifetime of the team session, including archive/revival.
    TeamSession,
}

/// Prepared policy/template used to mint runtime channels later.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedChannelTemplate {
    /// Stable bundle-local template id; never a runtime channel id.
    pub id: String,
    /// Supported runtime topology.
    pub kind: ChannelTemplateKind,
    /// Local Agent or topology-role selectors.
    pub participants: Vec<PreparedChannelParticipant>,
    /// Restrictive capability ceiling. Empty denies every optional capability.
    #[serde(default)]
    pub capabilities: Vec<ChannelCapability>,
    /// Hierarchy boundary enforced by the runtime.
    pub scope: ChannelScope,
    /// Event retention contract.
    pub retention: ChannelRetention,
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
    /// Raw native executable or library bytes in canonical standard Base64.
    /// Present for `extensions.rust` and `extensions.libraries`; `content` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_base64: Option<String>,
    /// Alternate local names that resolve to this resource inside the bundle.
    pub aliases: Vec<String>,
}

impl PreparedResource {
    /// Decode this resource's original source bytes for materialization.
    ///
    /// # Errors
    /// Returns a Base64 decode error if binary content is malformed.
    pub fn source_bytes(&self) -> Result<Vec<u8>, base64::DecodeError> {
        use base64::Engine as _;
        match &self.binary_base64 {
            Some(encoded) => base64::engine::general_purpose::STANDARD.decode(encoded),
            None => Ok(self.content.as_bytes().to_vec()),
        }
    }
}

/// A validated bundle self-check declaration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedCheck {
    /// Argv to execute without a shell.
    pub command: Vec<String>,
    /// Maximum runtime in seconds.
    pub timeout_secs: u64,
}

/// One complete prepared bundle, including every executable and policy declaration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    /// Prepared-document format version (currently `4`).
    pub format_version: u32,
    /// Bundle identity block.
    pub identity: BundleIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Optional source self-check.
    pub check: Option<PreparedCheck>,
    /// Provider-facing namespace (declared or identity-derived).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Digest of this bundle's canonical content for integrity checks.
    pub digest: String,
    /// All Agents this bundle defines.
    pub agents: Vec<PreparedAgent>,
    /// Optional compiled Workflow.
    pub workflow: Option<PreparedWorkflow>,
    /// Declarative channel templates; concrete ids remain runtime-minted event facts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<PreparedChannelTemplate>,
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
    /// Optional bundle-owned frontend extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tui: Option<PreparedTuiExtension>,
    /// External URI schemes owned by this bundle.
    pub schemas: Vec<PreparedSchema>,
    /// Explicit provider process.
    pub process: Option<PreparedProcessExtension>,
    /// HTTP API declarations.
    pub apis: Vec<PreparedApi>,
    /// Permission modes owned by this bundle.
    pub permission_modes: Vec<PreparedPermissionMode>,
}

/// One compiled Workflow source retained in a prepared Bundle.
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

/// Validated bundle-owned TUI entrypoint and requested frontend capabilities.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedTuiExtension {
    /// Extension API generation (currently 1).
    pub api_version: u32,
    /// Canonical relative TS/JS path in extension resources.
    pub entry: String,
    /// Independent SDK contract version (major 1).
    pub sdk: String,
    /// Allowlisted capability names in strictly sorted order.
    pub permissions: Vec<String>,
}

/// Immutable accessors used by runtime catalogs.
impl Bundle {
    /// The bundle's identity.
    pub fn identity(&self) -> &BundleIdentity {
        &self.identity
    }
    /// Digest of every declaration and embedded resource.
    pub fn digest(&self) -> &str {
        &self.digest
    }
    /// Provider-facing namespace.
    pub fn namespace(&self) -> &str {
        self.namespace
            .as_deref()
            .unwrap_or_else(|| self.identity.id.rsplit('/').next().unwrap_or_default())
    }
    /// Source self-check.
    pub fn check(&self) -> Option<&PreparedCheck> {
        self.check.as_ref()
    }
    /// Published Agents.
    pub fn agents(&self) -> &[PreparedAgent] {
        &self.agents
    }
    /// Channel templates.
    pub fn channels(&self) -> &[PreparedChannelTemplate] {
        &self.channels
    }
    /// Compiled Workflow, if declared.
    pub fn workflow(&self) -> Option<&PreparedWorkflow> {
        self.workflow.as_ref()
    }
    /// Frontend extension, if declared.
    pub fn tui(&self) -> Option<&PreparedTuiExtension> {
        self.tui.as_ref()
    }
    /// Bundle kind exposed to catalogs.
    pub const fn kind(&self) -> PreparedBundleKind {
        PreparedBundleKind::Bundle
    }
    /// Resources of a selected family.
    pub fn resources(&self, kind: crate::ExportKind) -> &[PreparedResource] {
        match kind {
            crate::ExportKind::Tool => &self.tools,
            crate::ExportKind::Skill => &self.skills,
            crate::ExportKind::Mcp => &self.mcp,
            crate::ExportKind::Hook => &self.hooks,
            crate::ExportKind::Extension => &self.extensions,
        }
    }
    /// Prepared tools resources.
    pub fn tools(&self) -> &[PreparedResource] {
        &self.tools
    }
    /// Prepared skills resources.
    pub fn skills(&self) -> &[PreparedResource] {
        &self.skills
    }
    /// Prepared mcp resources.
    pub fn mcp(&self) -> &[PreparedResource] {
        &self.mcp
    }
    /// Prepared hooks resources.
    pub fn hooks(&self) -> &[PreparedResource] {
        &self.hooks
    }
    /// Prepared extensions resources.
    pub fn extensions(&self) -> &[PreparedResource] {
        &self.extensions
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

/// Derived per-bundle URI-scheme lookup row; never persisted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedBundleSchemas {
    /// Bundle identity id the schemas belong to.
    pub bundle_id: String,
    /// Declared schema extensions, sorted by scheme.
    pub schemas: Vec<PreparedSchema>,
}

/// One prepared HTTP endpoint a bundle process serves (manifest `apis:`).
///
/// Endpoints exist only for bundles with an explicit `extensions.process`;
/// the process answers `api/request` for each declared id.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedApi {
    /// Endpoint id, unique within the bundle.
    pub id: String,
    /// HTTP method.
    pub method: crate::api::ApiMethod,
    /// Mount scope.
    pub scope: crate::api::ApiScope,
    /// Canonical path template below the bundle mount (`/items/{id}`).
    pub path: String,
    /// Human-readable description (empty when not declared).
    pub description: String,
    /// Normalized path of the declared extension file holding the request
    /// body's JSON Schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_schema: Option<String>,
    /// Normalized path of the declared extension file holding the response
    /// body's JSON Schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<String>,
}

/// Derived per-bundle API lookup row; never persisted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedBundleApis {
    /// Bundle identity id the endpoints belong to.
    pub bundle_id: String,
    /// Declared endpoints, sorted by id.
    pub apis: Vec<PreparedApi>,
}

/// One prepared session permission mode (manifest `permission_modes:`).
///
/// Modes exist only for bundles with an explicit `extensions.process` that
/// declares the `permission.approve` hook; the runtime publishes them as
/// `<bundle-id>/<id>`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedPermissionMode {
    /// Mode id, unique within the bundle.
    pub id: String,
    /// Short human-readable name.
    pub title: String,
    /// Longer description (empty when not declared).
    pub description: String,
}

/// Derived per-bundle permission-mode lookup row; never persisted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedBundlePermissionModes {
    /// Bundle identity id the modes belong to.
    pub bundle_id: String,
    /// Declared modes, sorted by id.
    pub modes: Vec<PreparedPermissionMode>,
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
/// starts it. Validated at prepare time; the application starts it before
/// publishing a new immutable runtime generation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedProcessExtension {
    /// Runtime kind that executes the command.
    pub kind: PreparedProcessKind,
    /// Argv to spawn (`command[0]` is the program).
    pub command: Vec<String>,
}

/// Derived per-bundle process lookup row; never persisted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedBundleProcess {
    /// Bundle identity id the declaration belongs to.
    pub bundle_id: String,
    /// The declared process extension.
    pub process: PreparedProcessExtension,
}

/// Derived catalog index row; never persisted.
#[derive(Clone, Debug, Eq, PartialEq)]
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

/// One decoded Bundle with derived runtime lookups and its canonical stored bytes.
#[derive(Debug)]
pub struct PreparedCatalog {
    pub(crate) bundles: Vec<Bundle>,
    pub(crate) index: Vec<PreparedBundleIndex>,
    pub(crate) schemas: Vec<PreparedBundleSchemas>,
    pub(crate) process_extensions: Vec<PreparedBundleProcess>,
    pub(crate) apis: Vec<PreparedBundleApis>,
    pub(crate) permission_modes: Vec<PreparedBundlePermissionModes>,
    pub(crate) bytes: Vec<u8>,
    pub(crate) digest: String,
}

impl PreparedCatalog {
    /// Prepared installable payloads in catalog order.
    #[must_use]
    pub fn bundles(&self) -> &[Bundle] {
        &self.bundles
    }

    /// Compact index reconstructed from the prepared Bundle on load.
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

    /// Per-bundle HTTP endpoint declarations, sorted by bundle id.
    #[must_use]
    pub fn apis(&self) -> &[PreparedBundleApis] {
        &self.apis
    }

    /// The endpoints one bundle declares, or an empty slice.
    #[must_use]
    pub fn bundle_apis(&self, bundle_id: &str) -> &[PreparedApi] {
        self.apis
            .iter()
            .find(|row| row.bundle_id == bundle_id)
            .map(|row| row.apis.as_slice())
            .unwrap_or(&[])
    }

    /// Per-bundle permission-mode declarations, sorted by bundle id.
    #[must_use]
    pub fn permission_modes(&self) -> &[PreparedBundlePermissionModes] {
        &self.permission_modes
    }

    /// The permission modes one bundle declares, or an empty slice.
    #[must_use]
    pub fn bundle_permission_modes(&self, bundle_id: &str) -> &[PreparedPermissionMode] {
        self.permission_modes
            .iter()
            .find(|row| row.bundle_id == bundle_id)
            .map(|row| row.modes.as_slice())
            .unwrap_or(&[])
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
