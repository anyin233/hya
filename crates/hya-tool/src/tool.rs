use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ActorClaim, OperationId, SessionId, ToolCallId, ToolName, ToolSchema};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::agents::{AgentDef, ListAgentsTool};
use crate::apply_patch::ApplyPatchTool;
use crate::edit::EditTool;
use crate::formatter::FormatterPlane;
use crate::interaction::{InteractionPlane, QuestionAnswer, QuestionKind};
use crate::invalid::InvalidTool;
use crate::lsp::{LspPlane, LspTool};
use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::mailbox::{ChannelsTool, JoinTool, LeaveTool, MailboxPlane, RosterTool, SendTool};
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
use crate::write::WriteTool;

#[derive(Error, Debug)]
pub enum ToolError {
    #[error("input: {0}")]
    Input(String),
    #[error(transparent)]
    Permission(#[from] PermissionError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cancelled")]
    Cancelled,
    #[error("overloaded: {0}")]
    Overloaded(String),
    #[error("OPERATION_ID_CONFLICT")]
    OperationIdConflict,
    #[error("operation already handled")]
    OperationAlreadyHandled,
    #[error("UNKNOWN_AGENT_ID: `{agent_id}`")]
    UnknownAgentId { agent_id: String },
    #[error("AGENT_SPAWN_NOT_ALLOWED: `{caller}` cannot spawn `{agent_id}`")]
    AgentSpawnNotAllowed { caller: String, agent_id: String },
    #[error("UNSUPPORTED_INLINE_AGENT_FIELD: `{field}`")]
    UnsupportedInlineAgentField { field: &'static str },
    #[error("{0}")]
    Other(String),
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
#[error("duplicate tool name: {name}")]
pub struct DuplicateName {
    pub name: String,
}

pub struct ToolCtx {
    pub permission: PermissionPlane,
    pub interaction: InteractionPlane,
    pub spawner: SpawnerPlane,
    pub operation: ToolOperation,
    pub mailbox: MailboxPlane,
    pub session: Option<SessionId>,
    pub parent_session: Option<SessionId>,
    pub todo: TodoPlane,
    pub skills: SkillPlane,
    pub agents: Arc<[AgentDef]>,
    pub websearch: WebSearchPlane,
    pub lsp: LspPlane,
    pub formatter: FormatterPlane,
    pub workdir: PathBuf,
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
    #[must_use]
    pub fn from_tool_call(source_tool_call_id: ToolCallId) -> Self {
        Self {
            source_tool_call_id,
            operation_id: OperationId::from_tool_call(source_tool_call_id),
            actor_claim: None,
        }
    }

    #[must_use]
    pub const fn with_actor_claim(mut self, actor_claim: Option<ActorClaim>) -> Self {
        self.actor_claim = actor_claim;
        self
    }

    #[must_use]
    pub fn source_tool_call_id(self) -> ToolCallId {
        self.source_tool_call_id
    }

    #[must_use]
    pub fn operation_id(self) -> OperationId {
        self.operation_id
    }

    #[must_use]
    pub const fn actor_claim(self) -> Option<ActorClaim> {
        self.actor_claim
    }
}

const SEARCH_LIMIT: usize = 100;
const BUILTIN_DISPATCH_IDENTITY_DOMAIN_V1: &[u8] = b"hya.tool.builtin-dispatch/v1";

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolSchema;
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

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        self.inner.execute(ctx, input).await
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolPermission {
    ReadOnly,
    Task,
    Tool,
    Command,
    Mcp,
}

#[derive(Clone)]
pub struct ResolvedTool {
    pub tool: Arc<dyn Tool>,
    pub permission: ToolPermission,
}

impl ResolvedTool {
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

    #[must_use]
    pub fn builtins() -> Self {
        let registry = Self::empty();
        for tool in [
            Arc::new(InvalidTool) as Arc<dyn Tool>,
            Arc::new(ReadTool),
            Arc::new(WriteTool),
            Arc::new(EditTool),
            Arc::new(LsTool),
            Arc::new(GlobTool),
            Arc::new(FindTool),
            Arc::new(GrepTool),
            Arc::new(QuestionTool),
            Arc::new(LspTool),
            Arc::new(SkillTool),
            Arc::new(ListAgentsTool),
            Arc::new(AskUserTool),
            Arc::new(TaskTool),
            Arc::new(SendTool),
            Arc::new(RosterTool),
            Arc::new(ChannelsTool),
            Arc::new(JoinTool),
            Arc::new(LeaveTool),
        ] {
            registry.insert_builtin(tool);
        }
        let shell = Arc::new(ShellTool);
        registry.insert_builtin(shell.clone());
        registry.insert_named_builtin("bash", shell);
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

    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.resolve(name).map(|resolved| resolved.tool)
    }

    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<ResolvedTool> {
        let inner = self.read();
        inner
            .tools
            .get(name)
            .or_else(|| inner.aliases.get(name))
            .cloned()
    }

    pub fn remove(&self, name: &str) {
        let mut inner = self.write();
        if inner.tools.remove(name).is_some() {
            inner.dispatch_identities.remove(name);
        }
        inner
            .aliases
            .retain(|alias, resolved| alias != name && resolved.tool.name() != name);
    }

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

    fn insert_named_builtin(&self, name: &str, tool: Arc<dyn Tool>) {
        let mut inner = self.write();
        inner.tools.insert(
            name.to_string(),
            ResolvedTool {
                tool: Arc::new(NamedTool {
                    name: name.to_string(),
                    inner: tool,
                }),
                permission: builtin_permission(name),
            },
        );
        if let Some(identity) = builtin_dispatch_identity(name) {
            inner.dispatch_identities.insert(name.to_string(), identity);
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
    #[must_use]
    pub fn dispatch_identity_v1(&self, canonical: &str) -> Option<[u8; 32]> {
        self.inner.dispatch_identities.get(canonical).copied()
    }

    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<ResolvedTool> {
        self.inner
            .tools
            .get(name)
            .or_else(|| self.inner.aliases.get(name))
            .cloned()
    }

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

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
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

fn matches_include(include: &str, path: &Path, root: &Path) -> bool {
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    glob_match(include, &relative) || glob_match(include, &name)
}

async fn assert_external_directory(
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

#[derive(Deserialize)]
struct GlobInput {
    pattern: String,
    path: Option<String>,
}
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
                "pattern": {"type": "string", "description": "The glob pattern to match files against"},
                "path": {"type": "string", "description": "The directory to search in. If omitted, uses the working directory."}
            }),
            &["pattern"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: GlobInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        ctx.permission
            .assert(Action::Glob, Resource::Glob(input.pattern.clone()))
            .await?;
        let search = input.path.as_deref().map_or_else(
            || ctx.workdir.clone(),
            |path| resolve_file(&ctx.workdir, path),
        );
        let is_file = tokio::fs::metadata(&search)
            .await
            .is_ok_and(|meta| meta.is_file());
        if is_file {
            return Err(ToolError::Input(format!(
                "glob path must be a directory: {}",
                display_path(&search)
            )));
        }
        assert_external_directory(ctx, &search, true).await?;
        let mut files = Vec::new();
        walk(&search, &mut files);
        let mut rows = Vec::new();
        for f in files {
            let rel = f.strip_prefix(&search).unwrap_or(f.as_path());
            let rel_str = rel.to_string_lossy();
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if glob_match(&input.pattern, &rel_str) || glob_match(&input.pattern, &name) {
                rows.push(f);
            }
        }
        rows.sort();
        let total = rows.len();
        let truncated = total >= SEARCH_LIMIT;
        rows.truncate(SEARCH_LIMIT);
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
struct GrepInput {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
}
pub struct GrepTool;
#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "grep",
            "Search file contents with a regex pattern under a path.",
            json!({
                "pattern": {"type": "string", "description": "The regex pattern to search for in file contents"},
                "path": {"type": "string", "description": "The directory or file to search in. Defaults to the working directory."},
                "include": {"type": "string", "description": "File glob pattern to include in the search"}
            }),
            &["pattern"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: GrepInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        if input.pattern.is_empty() {
            return Err(ToolError::Input("pattern is required".to_string()));
        }
        let regex = Regex::new(&input.pattern).map_err(|e| ToolError::Input(e.to_string()))?;
        let root = input.path.as_deref().map_or_else(
            || ctx.workdir.clone(),
            |path| resolve_file(&ctx.workdir, path),
        );
        ctx.permission
            .assert(Action::Grep, Resource::Glob(input.pattern.clone()))
            .await?;
        let meta = tokio::fs::metadata(&root).await.ok();
        assert_external_directory(
            ctx,
            &root,
            meta.as_ref().is_some_and(std::fs::Metadata::is_dir),
        )
        .await?;
        let search_root = if meta.as_ref().is_some_and(std::fs::Metadata::is_file) {
            root.parent()
                .map_or_else(|| root.clone(), Path::to_path_buf)
        } else {
            root.clone()
        };
        let mut files = {
            let mut files = Vec::new();
            walk(&search_root, &mut files);
            files
        };
        files.sort();
        let mut rows = Vec::new();
        for f in files {
            if let Some(include) = &input.include
                && !matches_include(include, &f, &search_root)
            {
                continue;
            }
            let Ok(content) = tokio::fs::read_to_string(&f).await else {
                continue;
            };
            for (i, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    rows.push((f.clone(), i + 1, line.to_string()));
                    if rows.len() >= SEARCH_LIMIT {
                        break;
                    }
                }
            }
            if rows.len() >= SEARCH_LIMIT {
                break;
            }
        }
        let truncated = rows.len() >= SEARCH_LIMIT;
        if rows.is_empty() {
            return Ok(json!({
                "title": input.pattern,
                "metadata": { "matches": 0, "truncated": false },
                "output": "No files found",
                "matches": [],
                "total": 0,
            }));
        }
        let mut output = vec![format!(
            "Found {} matches{}",
            rows.len(),
            if truncated {
                " (more matches available)"
            } else {
                ""
            }
        )];
        let mut current = PathBuf::new();
        for (path, line, text) in &rows {
            if current != *path {
                if !current.as_os_str().is_empty() {
                    output.push(String::new());
                }
                current = path.clone();
                output.push(format!("{}:", display_path(path)));
            }
            output.push(format!("  Line {line}: {text}"));
        }
        if truncated {
            output.push(String::new());
            output.push(
                "(Results truncated. Consider using a more specific path or pattern.)".to_string(),
            );
        }
        let matches = rows
            .iter()
            .map(|(path, line, text)| {
                json!({
                    "file": display_path(path),
                    "line": line,
                    "text": text,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "title": input.pattern,
            "metadata": {
                "matches": rows.len(),
                "truncated": truncated,
            },
            "output": output.join("\n"),
            "matches": matches,
            "total": rows.len(),
        }))
    }
}

#[derive(Deserialize)]
struct LsInput {
    path: Option<String>,
}
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
        let root = input
            .path
            .clone()
            .map_or_else(|| ctx.workdir.clone(), PathBuf::from);
        ctx.permission
            .assert(Action::Glob, Resource::Glob(input.pattern.clone()))
            .await?;
        let mut files = Vec::new();
        walk(&root, &mut files);
        let mut rows: Vec<(String, u64)> = Vec::new();
        for f in &files {
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
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let results: Vec<Value> = rows
            .into_iter()
            .map(|(path, size)| json!({ "path": path, "size": size }))
            .collect();
        Ok(json!({ "results": results }))
    }
}

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
