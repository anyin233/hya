use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ActorClaim, OperationId, SessionId, ToolCallId, ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::agents::AgentDef;
use crate::base_tools::{AliasVisibility, tool_bundle_presets};
use crate::formatter::FormatterPlane;
use crate::handle::{ArtifactPlane, HandleRouter};
use crate::interaction::InteractionPlane;
use crate::lsp_plane::LspPlane;
use crate::mailbox::MailboxPlane;
use crate::permission::{Invocation, Mode, PermissionError, PermissionPlane};
use crate::skill::SkillPlane;
use crate::spawn::SpawnerPlane;
use crate::todo::TodoPlane;
use crate::websearch::WebSearchPlane;
use crate::workflow_plane::WorkflowPlane;

/// Failure returned from tool execution and mapped to wire `error.type` strings by the engine.
#[derive(Error, Debug)]
pub enum ToolError {
    /// Caller-supplied arguments failed validation or schema checks.
    #[error("input: {0}")]
    Input(String),
    /// Permission plane denied the call (or ask channel was unavailable).
    #[error(transparent)]
    Permission(#[from] PermissionError),
    /// Filesystem or process I/O failure during the tool body.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON parse/serialize failure for tool input or output.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Cooperative cancellation via [`ToolCtx::cancel`].
    #[error("cancelled")]
    Cancelled,
    /// Transient capacity pressure (for example spawn admission overloaded).
    #[error("overloaded: {0}")]
    Overloaded(String),
    /// The same operation id is already in flight under a conflicting claim.
    #[error("OPERATION_ID_CONFLICT")]
    OperationIdConflict,
    /// The operation id was already completed; a second handle is rejected.
    #[error("operation already handled")]
    OperationAlreadyHandled,
    /// App-owned Workflow control rejected a structured command.
    #[error("{code}: {message}")]
    WorkflowControl {
        /// Machine-stable control code.
        code: String,
        /// Bounded diagnostic.
        message: String,
    },
    /// Requested subagent type is not in the caller's authorized roster.
    #[error("UNKNOWN_AGENT_ID: `{agent_id}`")]
    UnknownAgentId {
        /// Agent id that was requested.
        agent_id: String,
    },
    /// Caller is not allowed to spawn the named agent (`can_spawn` / roster).
    #[error("AGENT_SPAWN_NOT_ALLOWED: `{caller}` cannot spawn `{agent_id}`")]
    AgentSpawnNotAllowed {
        /// Calling agent id.
        caller: String,
        /// Target agent id that was refused.
        agent_id: String,
    },
    /// Inline agent overlay used a field the runtime does not support.
    #[error("UNSUPPORTED_INLINE_AGENT_FIELD: `{field}`")]
    UnsupportedInlineAgentField {
        /// Unsupported field name.
        field: &'static str,
    },
    /// Catch-all message mapped to wire type `unknown`.
    #[error("{0}")]
    Other(String),
}

/// Registry rejected a registration because the name (or alias) is already taken.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
#[error("duplicate tool name: {name}")]
pub struct DuplicateName {
    /// Conflicting tool or alias name.
    pub name: String,
}

/// Per-call runtime context passed to every [`Tool::execute`].
///
/// Planes are session-scoped services; tools assert permissions and call planes
/// without holding the session engine itself.
#[derive(Clone)]
pub struct ToolCtx {
    /// Call-scoped permission plane (after invocation authorization).
    pub permission: PermissionPlane,
    /// Channel for human questions (`question` / `ask_user`).
    pub interaction: InteractionPlane,
    /// Subagent spawn plane for the `task` tool.
    pub spawner: SpawnerPlane,
    /// User-authored workflow plane for the `workflow` tool (disconnected
    /// unless a workflow host is wired; see `workflow_plane`).
    pub workflows: WorkflowPlane,
    /// Persisted operation identity for this tool call.
    pub operation: ToolOperation,
    /// Team mailbox plane (disconnected outside a running team).
    pub mailbox: MailboxPlane,
    /// Subagent lifecycle plane for `report`/`archive`/`wait` (ADR-0015).
    pub lifecycle: crate::lifecycle::LifecyclePlane,
    /// Active session id when the tool runs inside a session.
    pub session: Option<SessionId>,
    /// Parent session id for nested/subagent turns, when applicable.
    pub parent_session: Option<SessionId>,
    /// In-memory todo plane for `todowrite`.
    pub todo: TodoPlane,
    /// Skill catalog plane for the `skill` tool.
    pub skills: SkillPlane,
    /// Post-processing chain applied when an `artifact://` handle is retrieved.
    pub artifacts: ArtifactPlane,
    /// Immutable caller-reachable agent roster for spawn authorization and listing.
    pub agents: Arc<[AgentDef]>,
    /// Configured web-search plane.
    pub websearch: WebSearchPlane,
    /// Language-server plane for `lsp` and post-edit diagnostics.
    pub lsp: LspPlane,
    /// External formatter plane for write/edit/patch post-processing.
    pub formatter: FormatterPlane,
    /// Session working directory used for path resolution.
    pub workdir: PathBuf,
    /// Workspace roots of the session (ADR-0024), resolved fresh at each
    /// turn start: a Project session's roots in order (the workdir prepended
    /// when it lies inside none of them), otherwise just `[workdir]`. Ordered
    /// and deduplicated; never empty for an engine-built context. Carried
    /// only for now: path permission checks still resolve against `workdir`.
    pub roots: Vec<PathBuf>,
    /// Cancellation token for cooperative abort.
    pub cancel: CancellationToken,
}

impl ToolCtx {
    /// Internal resource URLs reachable from this call.
    ///
    /// Built from the call's own `workdir` and skill catalog rather than stored
    /// alongside them, so a router can never resolve against a different session
    /// than the context it came from.
    ///
    /// This is the agent's own namespace — spilled output, skill bodies, scratch
    /// payloads. It has no bearing on how `read`, `write`, `grep`, or `bash`
    /// treat an ordinary filesystem path.
    #[must_use]
    pub fn handles(&self) -> HandleRouter {
        HandleRouter::new(&self.workdir)
            .with_skills(self.skills.clone())
            .with_artifacts(self.artifacts.store(&self.workdir))
    }
}

/// Immutable identity of the persisted tool invocation and its admission operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolOperation {
    source_tool_call_id: ToolCallId,
    operation_id: OperationId,
    actor_claim: Option<ActorClaim>,
}

impl ToolOperation {
    /// Derive operation identity from a provider tool-call id (no actor claim yet).
    #[must_use]
    pub fn from_tool_call(source_tool_call_id: ToolCallId) -> Self {
        Self {
            source_tool_call_id,
            operation_id: OperationId::from_tool_call(source_tool_call_id),
            actor_claim: None,
        }
    }

    /// Attach the optional actor claim used by resident/subagent fences.
    #[must_use]
    pub const fn with_actor_claim(mut self, actor_claim: Option<ActorClaim>) -> Self {
        self.actor_claim = actor_claim;
        self
    }

    /// Provider-facing tool-call id that originated this operation.
    #[must_use]
    pub fn source_tool_call_id(self) -> ToolCallId {
        self.source_tool_call_id
    }

    /// Stable operation id used for admission and conflict detection.
    #[must_use]
    pub fn operation_id(self) -> OperationId {
        self.operation_id
    }

    /// Actor claim, when this call runs under a fenced resident or subagent.
    #[must_use]
    pub const fn actor_claim(self) -> Option<ActorClaim> {
        self.actor_claim
    }
}

const BUILTIN_DISPATCH_IDENTITY_DOMAIN_V1: &[u8] = b"hya.tool.builtin-dispatch/v1";

/// Policy used when bounding a successful tool result before persistence.
///
/// The default keeps the historical 5,000-character tail behavior. Coding
/// tools opt into shape-aware bounds so their presentation envelope remains
/// structured while large fields are capped independently.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ToolResultPolicy {
    /// Apply the legacy display-text cap to arbitrary JSON values.
    #[default]
    Default,
    /// Preserve a bounded coding-tool presentation envelope.
    Coding,
    /// Preserve a coding envelope and an independently bounded edit diff.
    CodingWithDiff,
}

/// Model-callable capability: stable name, advertised schema, and async execution.
///
/// Implementations must not assume a particular UI. They assert permissions on
/// [`ToolCtx::permission`], use planes for side effects, and return JSON values
/// the engine will optionally pass through [`crate::cap_tool_output`].
#[async_trait]
pub trait Tool: Send + Sync {
    /// Canonical tool name as registered and advertised to the model.
    fn name(&self) -> &str;
    /// JSON Schema describing required and optional arguments.
    fn schema(&self) -> ToolSchema;
    /// Result bounding policy used after successful execution.
    ///
    /// The default keeps external tools source-compatible and preserves the
    /// historical arbitrary-value cap. Built-in coding adapters override this
    /// method explicitly rather than relying on their name.
    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::Default
    }
    /// Run the tool body with validated (or raw) `input` and the call context.
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError>;
}

/// A tool exposed under a different provider-facing name than its inner
/// implementation. Used for qualified contributed names
/// (`{namespace}__{local}`) and legacy alias spellings; `execute` always
/// delegates to the inner tool.
pub struct NamedTool {
    name: String,
    inner: Arc<dyn Tool>,
}

impl NamedTool {
    /// Wrap `inner` so it is registered and advertised as `name`.
    pub fn new(name: impl Into<String>, inner: Arc<dyn Tool>) -> Self {
        Self {
            name: name.into(),
            inner,
        }
    }
}

#[async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> ToolSchema {
        let mut schema = self.inner.schema();
        schema.name = ToolName::new(self.name.clone());
        schema
    }

    fn result_policy(&self) -> ToolResultPolicy {
        self.inner.result_policy()
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        self.inner.execute(ctx, input).await
    }
}

#[cfg(test)]
mod result_policy_tests {
    use super::*;

    struct ExplicitPolicyTool;

    #[async_trait]
    impl Tool for ExplicitPolicyTool {
        fn name(&self) -> &str {
            "wrapped"
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new("wrapped"),
                description: String::new(),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: None,
            }
        }

        fn result_policy(&self) -> ToolResultPolicy {
            ToolResultPolicy::CodingWithDiff
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
            Ok(Value::Null)
        }
    }

    struct DefaultPolicyTool;

    #[async_trait]
    impl Tool for DefaultPolicyTool {
        fn name(&self) -> &str {
            "read"
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new("read"),
                description: String::new(),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: None,
            }
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn external_tool_gets_legacy_policy_by_default() {
        assert_eq!(DefaultPolicyTool.result_policy(), ToolResultPolicy::Default);
    }

    #[test]
    fn named_tool_forwards_inner_policy() {
        let named = NamedTool {
            name: "alias".to_string(),
            inner: Arc::new(ExplicitPolicyTool),
        };
        assert_eq!(named.name(), "alias");
        assert_eq!(named.result_policy(), ToolResultPolicy::CodingWithDiff);
    }
}

/// Mutable tool catalog used to assemble a complete runtime candidate.
///
/// A live session engine consumes an immutable [`ToolRegistrySnapshot`] instead.
/// Mutating this builder after snapshotting does not alter an effective runtime
/// view.
pub struct ToolRegistry {
    inner: std::sync::RwLock<ToolRegistryInner>,
}

#[derive(Clone, Default)]
struct ToolRegistryInner {
    tools: HashMap<String, ResolvedTool>,
    aliases: HashMap<String, ResolvedTool>,
    dispatch_identities: HashMap<String, [u8; 32]>,
    advertised_aliases: BTreeSet<String>,
    bundle_origins: HashMap<String, &'static str>,
}

/// Immutable, lock-free tool view retained by an admitted turn.
#[derive(Clone)]
pub struct ToolRegistrySnapshot {
    inner: Arc<ToolRegistryInner>,
}

/// Invocation-level permission class attached at registration time.
///
/// Drives the default mode for [`ResolvedTool::invocation`]: read-only and task
/// default to allow, general tools and MCP default to ask, and commands build a
/// dual tool+command subject from the `command` field.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermission {
    /// Local discovery and read tools that default to allow under `default`.
    ReadOnly,
    /// Subagent launch (`task`); defaults to allow at the invocation layer.
    Task,
    /// Mutating or network tools that default to ask.
    Tool,
    /// Shell tools that also subject-match the full command string.
    Command,
    /// MCP-bridged tools; subject is the namespaced MCP name.
    Mcp,
}

/// A registered tool together with its invocation permission class.
#[derive(Clone)]
pub struct ResolvedTool {
    /// Shared tool implementation.
    pub tool: Arc<dyn Tool>,
    /// How the engine builds the pre-execution [`Invocation`].
    pub permission: ToolPermission,
}

impl ResolvedTool {
    /// Build the invocation subject(s) used by [`PermissionPlane::authorize`].
    ///
    /// # Errors
    /// Returns [`ToolError::Input`] when a command tool is missing a string `command`.
    pub fn invocation(&self, input: &Value) -> Result<Invocation, ToolError> {
        let name = self.tool.name();
        match self.permission {
            ToolPermission::ReadOnly | ToolPermission::Task => {
                Ok(Invocation::tool(name, Mode::Allow))
            }
            ToolPermission::Tool => Ok(Invocation::tool(name, Mode::Ask)),
            ToolPermission::Command => input
                .get("command")
                .and_then(Value::as_str)
                .map(|command| Invocation::command(name, command))
                .ok_or_else(|| ToolError::Input("command must be a string".to_string())),
            ToolPermission::Mcp => Ok(Invocation::mcp(name)),
        }
    }
}

impl ToolRegistry {
    fn empty() -> Self {
        Self {
            inner: std::sync::RwLock::new(ToolRegistryInner::default()),
        }
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, ToolRegistryInner> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, ToolRegistryInner> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Install the full set of canonical builtins (and their hidden aliases)
    /// from all five trusted tool families.
    #[must_use]
    pub fn builtins() -> Self {
        Self::from_tool_families(&crate::base_tools::TOOL_FAMILIES)
    }

    /// Install the builtins of a subset of the trusted tool families.
    ///
    /// A name two loaded families both export resolves to the entry that
    /// declares `overrides: <the other family>` in its exposure policy (for
    /// example channel-tools' mail-aware `wait` over extended-tools' `wait`);
    /// when the overriding family is not loaded, the overridden tool is
    /// installed. Load order never decides.
    ///
    /// # Panics
    ///
    /// Panics on an unknown family identity, or when a family's native
    /// library cannot be loaded or disagrees with its bundle policy.
    #[must_use]
    pub fn from_tool_families(identities: &[&str]) -> Self {
        const STEMS: [(&str, &str); 5] = [
            ("hya_base_tools", "hya/base-tools"),
            ("hya_extended_tools", "hya/extended-tools"),
            ("hya_channel_tools", "hya/channel-tools"),
            ("hya_network_tools", "hya/network-tools"),
            ("hya_todo_tools", "hya/todo-tools"),
        ];
        for identity in identities {
            assert!(
                STEMS.iter().any(|(_, known)| known == identity),
                "unknown tool family {identity}"
            );
        }
        let registry = Self::empty();
        let mut implementations: HashMap<(&'static str, String), Arc<dyn Tool>> = HashMap::new();
        for (stem, identity) in STEMS {
            if !identities.contains(&identity) {
                continue;
            }
            let tools = crate::native_bundle::load_family(stem)
                .unwrap_or_else(|error| panic!("load {identity}: {error}"));
            let names = tools
                .iter()
                .map(|tool| tool.name().to_string())
                .collect::<BTreeSet<_>>();
            let expected = tool_bundle_presets()
                .iter()
                .find(|preset| preset.identity() == identity)
                .map(|preset| {
                    preset
                        .tools()
                        .iter()
                        .map(|tool| tool.name().to_string())
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            assert_eq!(
                names, expected,
                "{identity} library tool set differs from its bundle policy"
            );
            assert_eq!(
                names.len(),
                tools.len(),
                "{identity} library declared duplicate tools"
            );
            implementations.extend(
                tools
                    .into_iter()
                    .map(|tool| ((identity, tool.name().to_string()), tool)),
            );
        }
        let loaded = tool_bundle_presets()
            .iter()
            .filter(|preset| identities.contains(&preset.identity()))
            .collect::<Vec<_>>();
        for preset in &loaded {
            let identity = STEMS
                .iter()
                .map(|(_, identity)| *identity)
                .find(|identity| *identity == preset.identity())
                .unwrap_or_else(|| panic!("unknown tool family {}", preset.identity()));
            for exposure in preset.tools() {
                let Some(tool) = implementations.remove(&(identity, exposure.name().to_string()))
                else {
                    panic!(
                        "{} declares `{}` without a Rust implementation",
                        preset.identity(),
                        exposure.name()
                    );
                };
                if !exposure.exposed() {
                    continue;
                }
                let overridden = loaded.iter().any(|other| {
                    other
                        .tool(exposure.name())
                        .is_some_and(|winner| winner.overrides() == Some(identity))
                });
                if overridden {
                    continue;
                }
                registry.insert_preset_builtin(tool, exposure, Some(identity));
            }
        }
        assert!(
            implementations.is_empty(),
            "Rust builtin implementations are missing from tool-family presets: {:?}",
            implementations.keys().collect::<BTreeSet<_>>()
        );
        for preset in &loaded {
            for scheme in preset.schemes() {
                assert!(
                    registry.get(scheme.tool()).is_some(),
                    "{} scheme `{}` names missing tool `{}`",
                    preset.identity(),
                    scheme.scheme(),
                    scheme.tool()
                );
            }
        }
        registry
    }

    /// Freeze the current builder contents into an immutable runtime view.
    #[must_use]
    pub fn snapshot(&self) -> ToolRegistrySnapshot {
        ToolRegistrySnapshot {
            inner: Arc::new(self.read().clone()),
        }
    }

    /// Start an offline candidate builder from an immutable runtime view.
    #[must_use]
    pub fn from_snapshot(snapshot: &ToolRegistrySnapshot) -> Self {
        Self {
            inner: std::sync::RwLock::new((*snapshot.inner).clone()),
        }
    }

    /// Compare a candidate with a frozen view by names, permission classes,
    /// aliases, and executor identity.
    #[must_use]
    pub fn logically_matches(&self, snapshot: &ToolRegistrySnapshot) -> bool {
        let candidate = self.read();
        maps_match(&candidate.tools, &snapshot.inner.tools)
            && maps_match(&candidate.aliases, &snapshot.inner.aliases)
            && candidate.dispatch_identities == snapshot.inner.dispatch_identities
            && candidate.bundle_origins == snapshot.inner.bundle_origins
    }

    /// Register a tool on this candidate builder through a shared reference.
    pub fn register(&self, tool: Arc<dyn Tool>) -> Result<(), DuplicateName> {
        self.register_with_permission(tool, ToolPermission::Tool)
    }

    /// Register a candidate tool with an explicit permission class.
    pub fn register_with_permission(
        &self,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
    ) -> Result<(), DuplicateName> {
        self.register_with_permission_and_aliases(tool, permission, &[])
    }

    /// Register one canonical tool plus aliases after validating the entire
    /// name set. Candidate builders use this before an immutable publication.
    pub fn register_with_permission_and_aliases(
        &self,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
        aliases: &[String],
    ) -> Result<(), DuplicateName> {
        self.register_with_permission_and_aliases_and_identity(tool, permission, aliases, None)
    }

    /// Register a tool with aliases and an explicit dispatch-identity digest.
    ///
    /// # Errors
    /// Returns [`DuplicateName`] when the canonical name or any alias collides.
    pub fn register_with_permission_and_aliases_and_dispatch_identity(
        &self,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
        aliases: &[String],
        identity: [u8; 32],
    ) -> Result<(), DuplicateName> {
        self.register_with_permission_and_aliases_and_identity(
            tool,
            permission,
            aliases,
            Some(identity),
        )
    }

    fn register_with_permission_and_aliases_and_identity(
        &self,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
        aliases: &[String],
        identity: Option<[u8; 32]>,
    ) -> Result<(), DuplicateName> {
        let name = tool.name().to_string();
        let mut inner = self.write();
        if inner.tools.contains_key(&name) || inner.aliases.contains_key(&name) {
            return Err(DuplicateName { name });
        }
        let mut pending = std::collections::BTreeSet::new();
        for alias in aliases {
            if alias == &name
                || !pending.insert(alias.as_str())
                || inner.tools.contains_key(alias)
                || inner.aliases.contains_key(alias)
            {
                return Err(DuplicateName {
                    name: alias.clone(),
                });
            }
        }
        let resolved = ResolvedTool { tool, permission };
        inner.tools.insert(name.clone(), resolved.clone());
        for alias in aliases {
            inner.aliases.insert(alias.clone(), resolved.clone());
        }
        if let Some(identity) = identity {
            inner.dispatch_identities.insert(name, identity);
        }
        Ok(())
    }

    /// Look up the tool implementation by canonical name or hidden alias.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.resolve(name).map(|resolved| resolved.tool)
    }

    /// Bundle identity that supplies a dynamically loaded builtin tool.
    #[must_use]
    pub fn builtin_bundle_origin(&self, canonical: &str) -> Option<&'static str> {
        self.read().bundle_origins.get(canonical).copied()
    }

    /// Resolve name or alias to the full [`ResolvedTool`] (tool + permission class).
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<ResolvedTool> {
        let inner = self.read();
        inner
            .tools
            .get(name)
            .or_else(|| inner.aliases.get(name))
            .cloned()
    }

    /// Remove a canonical tool and every alias that pointed at it.
    pub fn remove(&self, name: &str) {
        let mut inner = self.write();
        if inner.tools.remove(name).is_some() {
            inner.dispatch_identities.remove(name);
            inner.bundle_origins.remove(name);
        }
        inner
            .aliases
            .retain(|alias, resolved| alias != name && resolved.tool.name() != name);
    }

    /// Collect advertised schemas for every **canonical** tool (aliases excluded).
    #[must_use]
    pub fn schemas(&self) -> Vec<ToolSchema> {
        let inner = self.read();
        let mut schemas = inner
            .tools
            .values()
            .map(|resolved| resolved.tool.schema())
            .collect::<Vec<_>>();
        schemas.extend(inner.advertised_aliases.iter().filter_map(|alias| {
            inner.aliases.get(alias).map(|resolved| {
                let mut schema = resolved.tool.schema();
                schema.name = ToolName::new(alias.clone());
                schema
            })
        }));
        schemas
    }

    /// Install one built-in tool, failing loudly when the fixed builtin list
    /// ever carries a duplicate name: a silent overwrite would shadow an
    /// earlier tool with no diagnostic, so the registry treats it as an
    /// invariant violation instead.
    fn insert_preset_builtin(
        &self,
        tool: Arc<dyn Tool>,
        exposure: &crate::base_tools::BaseToolExposure,
        origin: Option<&'static str>,
    ) {
        let name = tool.name().to_string();
        assert_eq!(
            name,
            exposure.name(),
            "preset and implementation names differ"
        );
        let mut inner = self.write();
        let replaced = inner.tools.insert(
            name.clone(),
            ResolvedTool {
                tool: Arc::clone(&tool),
                permission: exposure.permission(),
            },
        );
        assert!(
            replaced.is_none(),
            "duplicate built-in tool name `{name}` in the fixed builtin list"
        );
        if let Some(identity) = builtin_dispatch_identity(&name) {
            inner.dispatch_identities.insert(name.clone(), identity);
        }
        if let Some(origin) = origin {
            inner.bundle_origins.insert(name.clone(), origin);
        }
        for alias in exposure.aliases() {
            let replaced = inner.aliases.insert(
                alias.name().to_string(),
                ResolvedTool {
                    tool: Arc::clone(&tool),
                    permission: exposure.permission(),
                },
            );
            assert!(
                replaced.is_none() && !inner.tools.contains_key(alias.name()),
                "duplicate tool-family preset alias `{}`",
                alias.name()
            );
            if alias.visibility() == AliasVisibility::Public {
                inner.advertised_aliases.insert(alias.name().to_string());
            }
        }
    }
}

fn maps_match(left: &HashMap<String, ResolvedTool>, right: &HashMap<String, ResolvedTool>) -> bool {
    left.len() == right.len()
        && left.iter().all(|(name, left)| {
            right.get(name).is_some_and(|right| {
                left.permission == right.permission && Arc::ptr_eq(&left.tool, &right.tool)
            })
        })
}

fn builtin_dispatch_identity(canonical: &str) -> Option<[u8; 32]> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(BUILTIN_DISPATCH_IDENTITY_DOMAIN_V1);
    append_length_prefixed(&mut bytes, env!("CARGO_PKG_VERSION").as_bytes())?;
    append_length_prefixed(&mut bytes, canonical.as_bytes())?;
    Some(Sha256::digest(bytes).into())
}

fn append_length_prefixed(bytes: &mut Vec<u8>, value: &[u8]) -> Option<()> {
    let length = u64::try_from(value.len()).ok()?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value);
    Some(())
}

impl ToolRegistrySnapshot {
    /// Domain-separated dispatch identity for a canonical tool, when recorded.
    #[must_use]
    pub fn dispatch_identity_v1(&self, canonical: &str) -> Option<[u8; 32]> {
        self.inner.dispatch_identities.get(canonical).copied()
    }

    /// Resolve a canonical name or hidden alias without locking.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<ResolvedTool> {
        self.inner
            .tools
            .get(name)
            .or_else(|| self.inner.aliases.get(name))
            .cloned()
    }

    /// Advertised schemas for canonical tools only (aliases are not listed).
    #[must_use]
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.inner
            .tools
            .values()
            .map(|resolved| resolved.tool.schema())
            .collect()
    }

    /// Canonical effective tools, excluding alternate registry aliases.
    #[must_use]
    pub fn canonical_tools(&self) -> Vec<(String, ResolvedTool)> {
        self.inner
            .tools
            .iter()
            .map(|(name, resolved)| (name.clone(), resolved.clone()))
            .collect()
    }

    /// Alias spellings whose resolved tool name matches `canonical`.
    /// Narrow seam used by resource-view compilation to project candidate
    /// effective aliases; there is no bulk public alias dump.
    #[must_use]
    pub fn aliases_for_canonical(&self, canonical: &str) -> Vec<String> {
        let mut names = self
            .inner
            .aliases
            .iter()
            .filter(|(_, resolved)| resolved.tool.name() == canonical)
            .map(|(alias, _)| alias.clone())
            .collect::<Vec<_>>();
        names.sort();
        names
    }
}

/// Construct an object-input tool schema from property and required-key sets.
#[must_use]
pub fn obj_schema(name: &str, description: &str, props: Value, required: &[&str]) -> ToolSchema {
    ToolSchema {
        name: ToolName::new(name),
        description: description.to_string(),
        input_schema: json!({ "type": "object", "properties": props, "required": required }),
        output_schema: None,
    }
}
