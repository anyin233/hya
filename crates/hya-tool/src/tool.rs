use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ActorClaim, OperationId, SessionId, ToolCallId, ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::agents::{AgentDef, ListAgentsTool};
use crate::apply_patch::ApplyPatchTool;
use crate::edit::EditTool;
use crate::formatter::FormatterPlane;
pub use crate::grep::GrepTool;
use crate::hashline::HashlineRuntime;
use crate::interaction::{InteractionPlane, QuestionAnswer, QuestionKind};
use crate::invalid::InvalidTool;
use crate::lsp::{LspPlane, LspTool};
use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::mailbox::{
    AnnounceTool, ChannelsTool, JoinTool, LeaveTool, MailboxPlane, RosterTool, SendTool,
};
use crate::permission::{
    Action, Invocation, Mode, PermissionError, PermissionPlane, Resource, glob_match,
};
use crate::plan::PlanExitTool;
use crate::question::QuestionTool;
use crate::read::ReadTool;
use crate::shell::ShellTool;
use crate::skill::{SkillPlane, SkillTool};
use crate::spawn::SpawnerPlane;
use crate::task::TaskTool;
use crate::todo::{TodoPlane, TodoWriteTool};
use crate::webfetch::WebFetchTool;
use crate::websearch::{WebSearchPlane, WebSearchTool};
use crate::workflow_plane::{WorkflowPlane, WorkflowTool};
use crate::write::WriteTool;

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
    /// Active session id when the tool runs inside a session.
    pub session: Option<SessionId>,
    /// Parent session id for nested/subagent turns, when applicable.
    pub parent_session: Option<SessionId>,
    /// In-memory todo plane for `todowrite`.
    pub todo: TodoPlane,
    /// Skill catalog plane for the `skill` tool.
    pub skills: SkillPlane,
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
    /// Cancellation token for cooperative abort.
    pub cancel: CancellationToken,
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

const SEARCH_LIMIT: usize = 100;
const MAX_GLOB_BYTES: usize = 4096;
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

struct NamedTool {
    name: String,
    inner: Arc<dyn Tool>,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

    /// Install the full set of canonical builtins (and their hidden aliases).
    #[must_use]
    pub fn builtins() -> Self {
        let registry = Self::empty();
        let hashline_runtime = Arc::new(HashlineRuntime::new());
        for tool in [
            Arc::new(InvalidTool) as Arc<dyn Tool>,
            Arc::new(ReadTool::new(Arc::clone(&hashline_runtime))),
            Arc::new(WriteTool::new(Arc::clone(&hashline_runtime))),
            Arc::new(EditTool::new(Arc::clone(&hashline_runtime))),
            Arc::new(LsTool),
            Arc::new(GlobTool),
            Arc::new(FindTool),
            Arc::new(GrepTool::with_runtime(Arc::clone(&hashline_runtime))),
            Arc::new(QuestionTool),
            Arc::new(LspTool),
            Arc::new(SkillTool),
            Arc::new(ListAgentsTool),
            Arc::new(AskUserTool),
            Arc::new(TaskTool),
            Arc::new(WorkflowTool),
            Arc::new(SendTool),
            Arc::new(AnnounceTool),
            Arc::new(RosterTool),
            Arc::new(ChannelsTool),
            Arc::new(JoinTool),
            Arc::new(LeaveTool),
        ] {
            registry.insert_builtin(tool);
        }
        registry.insert_aliased_builtin("bash", "shell", Arc::new(ShellTool));
        registry.insert_aliased_builtin("apply_patch", "patch", Arc::new(ApplyPatchTool));
        registry.insert_aliased_builtin("webfetch", "fetch", Arc::new(WebFetchTool));
        registry.insert_aliased_builtin("websearch", "search", Arc::new(WebSearchTool));
        registry.insert_aliased_builtin("todowrite", "todo", Arc::new(TodoWriteTool));
        registry.insert_aliased_builtin("plan_exit", "plan", Arc::new(PlanExitTool));
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
        }
        inner
            .aliases
            .retain(|alias, resolved| alias != name && resolved.tool.name() != name);
    }

    /// Collect advertised schemas for every **canonical** tool (aliases excluded).
    #[must_use]
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.read()
            .tools
            .values()
            .map(|resolved| resolved.tool.schema())
            .collect()
    }

    fn insert_builtin(&self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        let mut inner = self.write();
        inner.tools.insert(
            name.clone(),
            ResolvedTool {
                tool,
                permission: builtin_permission(&name),
            },
        );
        if let Some(identity) = builtin_dispatch_identity(&name) {
            inner.dispatch_identities.insert(name.clone(), identity);
        }
    }

    fn insert_aliased_builtin(&self, canonical: &str, legacy: &str, tool: Arc<dyn Tool>) {
        let permission = builtin_permission(canonical);
        let mut inner = self.write();
        inner.tools.insert(
            canonical.to_string(),
            ResolvedTool {
                tool: Arc::new(NamedTool {
                    name: canonical.to_string(),
                    inner: tool.clone(),
                }),
                permission,
            },
        );
        inner
            .aliases
            .insert(legacy.to_string(), ResolvedTool { tool, permission });
        if let Some(identity) = builtin_dispatch_identity(canonical) {
            inner
                .dispatch_identities
                .insert(canonical.to_string(), identity);
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

fn builtin_permission(name: &str) -> ToolPermission {
    match name {
        "read" | "ls" | "glob" | "find" | "grep" | "lsp" | "skill" | "list_agents" | "roster"
        | "channels" => ToolPermission::ReadOnly,
        "task" => ToolPermission::Task,
        "shell" | "bash" => ToolPermission::Command,
        _ => ToolPermission::Tool,
    }
}

pub(crate) fn obj_schema(
    name: &str,
    description: &str,
    props: Value,
    required: &[&str],
) -> ToolSchema {
    ToolSchema {
        name: ToolName::new(name),
        description: description.to_string(),
        input_schema: json!({ "type": "object", "properties": props, "required": required }),
        output_schema: None,
    }
}

/// Collect files recursively while checking the call cancellation token.
fn walk(dir: &Path, out: &mut Vec<PathBuf>, cancel: &CancellationToken) -> Result<(), ToolError> {
    if cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out, cancel)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn relative_title(path: &Path, workdir: &Path) -> String {
    let relative = path.strip_prefix(workdir).unwrap_or(path);
    let title = relative.to_string_lossy().replace('\\', "/");
    if title.is_empty() {
        ".".to_string()
    } else {
        title
    }
}

pub(crate) async fn assert_external_directory(
    ctx: &ToolCtx,
    target: &Path,
    is_directory: bool,
) -> Result<(), ToolError> {
    let target = normalize(&absolutize(target));
    let workdir = normalize(&absolutize(&ctx.workdir));
    if target.starts_with(&workdir) {
        return Ok(());
    }
    let parent = if is_directory {
        target
    } else {
        target
            .parent()
            .map_or_else(|| PathBuf::from("/"), Path::to_path_buf)
    };
    let pattern = display_path(&parent.join("*"));
    ctx.permission
        .assert(Action::ExternalDirectory, Resource::Path(pattern))
        .await?;
    Ok(())
}

/// Authorize an external Grep or Glob target with one lexical, kind-blind resource.
pub(crate) async fn assert_external_directory_lexical(
    ctx: &ToolCtx,
    target: &Path,
) -> Result<(), ToolError> {
    let target = normalize(&absolutize(target));
    let workdir = normalize(&absolutize(&ctx.workdir));
    if target.starts_with(&workdir) {
        return Ok(());
    }
    let parent = target
        .parent()
        .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
    let pattern = display_path(&parent.join("*"));
    ctx.permission
        .assert(Action::ExternalDirectory, Resource::Path(pattern))
        .await?;
    Ok(())
}

/// Reject a caller-provided Glob pattern that exceeds the native matcher bound.
fn validate_glob_pattern(pattern: &str) -> Result<(), ToolError> {
    if pattern.len() > MAX_GLOB_BYTES {
        return Err(ToolError::Input(format!(
            "glob pattern exceeds {MAX_GLOB_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Traverse files while retaining only the lexically first bounded Glob results.
///
/// # Parameters
/// - `directory`: Directory currently being visited.
/// - `root`: Search root used to build relative match candidates.
/// - `pattern`: Validated caller glob pattern.
/// - `matches`: Bounded ordered set of the first matching paths.
/// - `total`: Saturating count of all matching files observed.
/// - `cancel`: Call token checked before each directory entry and recursion.
///
/// # Returns
/// Success after the branch is exhausted, or typed cancellation.
fn collect_glob_matches(
    directory: &Path,
    root: &Path,
    pattern: &str,
    matches: &mut BTreeSet<PathBuf>,
    total: &mut usize,
    cancel: &CancellationToken,
) -> Result<(), ToolError> {
    if cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(());
    };
    for entry in entries {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_glob_matches(&path, root, pattern, matches, total, cancel)?;
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(path.as_path());
        let relative = relative.to_string_lossy().replace('\\', "/");
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if crate::grep::wildcard_match(pattern, &relative)
            || crate::grep::wildcard_match(pattern, &name)
        {
            *total = total.saturating_add(1);
            matches.insert(path);
            if matches.len() > SEARCH_LIMIT
                && let Some(last) = matches.iter().next_back().cloned()
            {
                matches.remove(&last);
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobInput {
    pattern: String,
    path: Option<String>,
}
/// Recursive path matcher with a 100-row cap (`SEARCH_LIMIT`).
pub struct GlobTool;
#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "glob",
            "List files under a directory matching a glob pattern.",
            json!({
                "pattern": {"type": "string", "maxLength": MAX_GLOB_BYTES, "description": "The glob pattern to match files against"},
                "path": {"type": "string", "description": "The directory to search in. If omitted, uses the working directory."}
            }),
            &["pattern"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if input.get("path").is_some_and(Value::is_null) {
            return Err(ToolError::Input("path must not be null".to_string()));
        }
        let input: GlobInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        validate_glob_pattern(&input.pattern)?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        ctx.permission
            .assert(Action::Glob, Resource::Glob(input.pattern.clone()))
            .await?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let search = input.path.as_deref().map_or_else(
            || ctx.workdir.clone(),
            |path| resolve_file(&ctx.workdir, path),
        );
        assert_external_directory_lexical(ctx, &search).await?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let is_file = tokio::fs::metadata(&search)
            .await
            .is_ok_and(|meta| meta.is_file());
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if is_file {
            return Err(ToolError::Input(format!(
                "glob path must be a directory: {}",
                display_path(&search)
            )));
        }
        let mut bounded = BTreeSet::new();
        let mut total = 0usize;
        collect_glob_matches(
            &search,
            &search,
            &input.pattern,
            &mut bounded,
            &mut total,
            &ctx.cancel,
        )?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let rows = bounded.into_iter().collect::<Vec<_>>();
        let truncated = total > SEARCH_LIMIT;
        let output_rows = rows
            .iter()
            .map(|path| display_path(path))
            .collect::<Vec<_>>();
        let mut output = if output_rows.is_empty() {
            "No files found".to_string()
        } else {
            output_rows.join("\n")
        };
        if truncated {
            output.push_str(
                "\n\n(Results are truncated: showing first 100 results. Consider using a more specific path or pattern.)",
            );
        }
        let legacy_paths = rows
            .iter()
            .map(|path| {
                path.strip_prefix(&ctx.workdir)
                    .unwrap_or(path.as_path())
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "title": relative_title(&search, &ctx.workdir),
            "metadata": {
                "count": output_rows.len(),
                "truncated": truncated,
            },
            "output": output,
            "paths": legacy_paths,
            "total": total,
        }))
    }
}

#[derive(Deserialize)]
struct LsInput {
    path: Option<String>,
}
/// Lists immediate directory entries (name, type, size) without recursion.
pub struct LsTool;
#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "ls",
            "List the immediate entries of a directory (name, type, size).",
            json!({"path": {"type": "string"}}),
            &[],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: LsInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let dir = input
            .path
            .clone()
            .map_or_else(|| ctx.workdir.clone(), PathBuf::from);
        ctx.permission
            .assert(
                Action::Read,
                Resource::Path(dir.to_string_lossy().into_owned()),
            )
            .await?;
        let mut rows: Vec<(String, &'static str, u64)> = Vec::new();
        let mut rd = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            let meta = entry.metadata().await?;
            let kind = if meta.is_dir() {
                "dir"
            } else if meta.is_file() {
                "file"
            } else {
                "other"
            };
            rows.push((
                entry.file_name().to_string_lossy().into_owned(),
                kind,
                meta.len(),
            ));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let entries: Vec<Value> = rows
            .into_iter()
            .map(|(name, kind, size)| json!({ "name": name, "type": kind, "size": size }))
            .collect();
        Ok(json!({ "entries": entries }))
    }
}

#[derive(Deserialize)]
struct FindInput {
    pattern: String,
    path: Option<String>,
}
/// Compatibility path finder: recursive `*` matching with sizes and no result-row cap.
pub struct FindTool;
#[async_trait]
impl Tool for FindTool {
    fn name(&self) -> &str {
        "find"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "find",
            "Recursively find files whose relative path or name matches a `*` glob, with size metadata.",
            json!({"pattern": {"type": "string"}, "path": {"type": "string"}}),
            &["pattern"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: FindInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let root = input.path.as_deref().map_or_else(
            || ctx.workdir.clone(),
            |path| resolve_file(&ctx.workdir, path),
        );
        ctx.permission
            .assert(Action::Glob, Resource::Glob(input.pattern.clone()))
            .await?;
        assert_external_directory(ctx, &root, true).await?;
        let mut files = Vec::new();
        walk(&root, &mut files, &ctx.cancel)?;
        let mut rows: Vec<(String, u64)> = Vec::new();
        for f in &files {
            if ctx.cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let rel = f.strip_prefix(&root).unwrap_or(f.as_path());
            let rel_str = rel.to_string_lossy();
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if glob_match(&input.pattern, &rel_str) || glob_match(&input.pattern, &name) {
                let size = tokio::fs::metadata(f).await.map(|m| m.len()).unwrap_or(0);
                rows.push((f.to_string_lossy().into_owned(), size));
            }
        }
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let results: Vec<Value> = rows
            .into_iter()
            .map(|(path, size)| json!({ "path": path, "size": size }))
            .collect();
        Ok(json!({ "results": results }))
    }
}

/// Single free-text or select prompt via [`InteractionPlane`] (cancellation is soft).
pub struct AskUserTool;

#[derive(Deserialize)]
struct AskUserInput {
    question: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    allow_custom: bool,
    #[serde(default)]
    default: Option<String>,
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "ask_user",
            "Ask the human operator a question and wait for their answer. Use kind=\"select\" with options for a choice, or kind=\"text\" for free-form input.",
            json!({
                "question": { "type": "string" },
                "kind": { "type": "string", "enum": ["text", "select"] },
                "options": { "type": "array", "items": { "type": "string" } },
                "allow_custom": { "type": "boolean" },
                "default": { "type": "string" }
            }),
            &["question"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: AskUserInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let kind = if input.kind == "select" {
            if input.options.is_empty() {
                return Err(ToolError::Input(
                    "kind=select requires a non-empty options list".to_string(),
                ));
            }
            QuestionKind::Select {
                options: input.options.clone(),
                allow_custom: input.allow_custom,
            }
        } else {
            QuestionKind::FreeText {
                default: input.default.clone(),
            }
        };
        match ctx.interaction.ask(input.question, kind).await {
            Ok(QuestionAnswer::Selected(i)) => Ok(json!({
                "answer": input.options.get(i).cloned().unwrap_or_default(),
                "selected_index": i,
            })),
            Ok(QuestionAnswer::SelectedMany(indices)) => Ok(json!({
                "answer": indices
                    .iter()
                    .filter_map(|index| input.options.get(*index).cloned())
                    .collect::<Vec<_>>(),
                "selected_indices": indices,
            })),
            Ok(QuestionAnswer::FreeText(text)) => Ok(json!({ "answer": text })),
            Ok(QuestionAnswer::Cancelled) | Err(_) => {
                Ok(json!({ "answer": "", "cancelled": true }))
            }
        }
    }
}
