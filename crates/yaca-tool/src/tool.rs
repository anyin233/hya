use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use yaca_proto::{SessionId, ToolName, ToolSchema};

use crate::interaction::{InteractionPlane, QuestionAnswer, QuestionKind};
use crate::permission::{Action, PermissionError, PermissionPlane, Resource, glob_match};
use crate::spawn::{SpawnMember, SpawnerPlane};

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
    pub parent_session: Option<SessionId>,
    pub workdir: PathBuf,
    pub cancel: CancellationToken,
}

const MAX_OUTPUT_BYTES: usize = 16 * 1024;
const MAX_LIST_ITEMS: usize = 500;

fn truncate(s: &str) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        return s.to_string();
    }
    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…[truncated {} bytes]", &s[..end], s.len() - end)
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolSchema;
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    #[must_use]
    pub fn builtins() -> Self {
        let list: Vec<Arc<dyn Tool>> = vec![
            Arc::new(ReadTool),
            Arc::new(WriteTool),
            Arc::new(EditTool),
            Arc::new(LsTool),
            Arc::new(GlobTool),
            Arc::new(FindTool),
            Arc::new(GrepTool),
            Arc::new(ShellTool),
            Arc::new(AskUserTool),
            Arc::new(TaskTool),
        ];
        let mut tools = HashMap::new();
        for t in list {
            tools.insert(t.name().to_string(), t);
        }
        Self { tools }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), DuplicateName> {
        let name = tool.name().to_string();
        if self.tools.contains_key(&name) {
            return Err(DuplicateName { name });
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    #[must_use]
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|t| t.schema()).collect()
    }
}

fn obj_schema(name: &str, description: &str, props: Value, required: &[&str]) -> ToolSchema {
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

#[derive(Deserialize)]
struct ReadInput {
    path: String,
}
pub struct ReadTool;
#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "read",
            "Read a file's contents.",
            json!({"path": {"type": "string"}}),
            &["path"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: ReadInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        ctx.permission
            .assert(Action::Read, Resource::Path(input.path.clone()))
            .await?;
        let content = tokio::fs::read_to_string(&input.path).await?;
        Ok(json!({ "content": truncate(&content) }))
    }
}

#[derive(Deserialize)]
struct WriteInput {
    path: String,
    content: String,
}
pub struct WriteTool;
#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "write",
            "Write content to a file (creating parent dirs).",
            json!({"path": {"type": "string"}, "content": {"type": "string"}}),
            &["path", "content"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: WriteInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        ctx.permission
            .assert(Action::Edit, Resource::Path(input.path.clone()))
            .await?;
        if let Some(parent) = Path::new(&input.path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&input.path, input.content.as_bytes()).await?;
        Ok(json!({ "ok": true, "bytes": input.content.len() }))
    }
}

#[derive(Deserialize)]
struct EditInput {
    path: String,
    old: String,
    new: String,
    #[serde(default)]
    replace_all: bool,
}
pub struct EditTool;
#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "edit",
            "Replace `old` with `new` in a file. Errors if `old` is missing or matches more than once, unless `replace_all` is set.",
            json!({"path": {"type": "string"}, "old": {"type": "string"}, "new": {"type": "string"}, "replace_all": {"type": "boolean"}}),
            &["path", "old", "new"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: EditInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        ctx.permission
            .assert(Action::Edit, Resource::Path(input.path.clone()))
            .await?;
        let content = tokio::fs::read_to_string(&input.path).await?;
        let count = content.matches(&input.old).count();
        if count == 0 {
            return Err(ToolError::Other("old string not found".to_string()));
        }
        if count > 1 && !input.replace_all {
            return Err(ToolError::Other(format!(
                "old string is ambiguous ({count} matches); add surrounding context or set replace_all=true"
            )));
        }
        let updated = if input.replace_all {
            content.replace(&input.old, &input.new)
        } else {
            content.replacen(&input.old, &input.new, 1)
        };
        tokio::fs::write(&input.path, updated).await?;
        Ok(json!({ "replaced": count }))
    }
}

#[derive(Deserialize)]
struct GlobInput {
    pattern: String,
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
            "List files under the working dir matching a `*` glob pattern.",
            json!({"pattern": {"type": "string"}}),
            &["pattern"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: GlobInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        ctx.permission
            .assert(Action::Glob, Resource::Glob(input.pattern.clone()))
            .await?;
        let mut files = Vec::new();
        walk(&ctx.workdir, &mut files);
        let mut matches = Vec::new();
        for f in files {
            let rel = f.strip_prefix(&ctx.workdir).unwrap_or(f.as_path());
            let rel_str = rel.to_string_lossy();
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if glob_match(&input.pattern, &rel_str) || glob_match(&input.pattern, &name) {
                matches.push(rel_str.into_owned());
            }
        }
        matches.sort();
        let total = matches.len();
        matches.truncate(MAX_LIST_ITEMS);
        Ok(json!({ "paths": matches, "total": total }))
    }
}

#[derive(Deserialize)]
struct GrepInput {
    pattern: String,
    path: Option<String>,
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
            "Find lines containing a substring under a path (default: working dir).",
            json!({"pattern": {"type": "string"}, "path": {"type": "string"}}),
            &["pattern"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: GrepInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let root = input
            .path
            .clone()
            .map_or_else(|| ctx.workdir.clone(), PathBuf::from);
        ctx.permission
            .assert(
                Action::Grep,
                Resource::Path(root.to_string_lossy().into_owned()),
            )
            .await?;
        let mut files = Vec::new();
        walk(&root, &mut files);
        let mut matches = Vec::new();
        for f in files {
            let Ok(content) = tokio::fs::read_to_string(&f).await else {
                continue;
            };
            for (i, line) in content.lines().enumerate() {
                if line.contains(&input.pattern) {
                    matches.push(json!({
                        "file": f.to_string_lossy().into_owned(),
                        "line": i + 1,
                        "text": line,
                    }));
                }
            }
        }
        let total = matches.len();
        matches.truncate(MAX_LIST_ITEMS);
        Ok(json!({ "matches": matches, "total": total }))
    }
}

#[derive(Deserialize)]
struct ShellInput {
    command: String,
}
pub struct ShellTool;
#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "shell",
            "Run a shell command in the working dir.",
            json!({"command": {"type": "string"}}),
            &["command"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let input: ShellInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        ctx.permission
            .assert(Action::Bash, Resource::Command(input.command.clone()))
            .await?;
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&input.command)
            .current_dir(&ctx.workdir)
            .output()
            .await?;
        Ok(json!({
            "stdout": truncate(&String::from_utf8_lossy(&output.stdout)),
            "stderr": truncate(&String::from_utf8_lossy(&output.stderr)),
            "exit_code": output.status.code(),
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
            Ok(QuestionAnswer::FreeText(text)) => Ok(json!({ "answer": text })),
            Ok(QuestionAnswer::Cancelled) | Err(_) => {
                Ok(json!({ "answer": "", "cancelled": true }))
            }
        }
    }
}

pub struct TaskTool;

#[derive(Deserialize)]
struct TaskMemberInput {
    #[serde(default)]
    description: String,
    prompt: String,
    subagent_type: String,
}

#[derive(Deserialize)]
struct TaskInput {
    #[serde(default)]
    description: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    subagent_type: String,
    #[serde(default)]
    members: Vec<TaskMemberInput>,
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "task",
            "Dispatch subagents to work in parallel and return their evidence. Give a single {description, prompt, subagent_type} or a members[] list. subagent_type is one of quick|deep|ultrabrain|writing.",
            json!({
                "description": { "type": "string" },
                "prompt": { "type": "string" },
                "subagent_type": { "type": "string", "enum": ["quick", "deep", "ultrabrain", "writing"] },
                "members": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": { "type": "string" },
                            "prompt": { "type": "string" },
                            "subagent_type": { "type": "string", "enum": ["quick", "deep", "ultrabrain", "writing"] }
                        },
                        "required": ["prompt", "subagent_type"]
                    }
                }
            }),
            &[],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if ctx.parent_session.is_some() {
            return Err(ToolError::Other(
                "the task tool is lead-only; a subagent cannot spawn more subagents".to_string(),
            ));
        }
        let input: TaskInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let mut members: Vec<SpawnMember> = input
            .members
            .into_iter()
            .map(|m| SpawnMember {
                description: m.description,
                prompt: m.prompt,
                subagent_type: m.subagent_type,
            })
            .collect();
        if members.is_empty() {
            if input.prompt.trim().is_empty() {
                return Err(ToolError::Input(
                    "provide a prompt + subagent_type, or a non-empty members list".to_string(),
                ));
            }
            members.push(SpawnMember {
                description: input.description,
                prompt: input.prompt,
                subagent_type: input.subagent_type,
            });
        }
        for member in &members {
            ctx.permission
                .assert(
                    Action::Task,
                    Resource::Subagent(member.subagent_type.clone()),
                )
                .await?;
        }
        let outcomes = ctx
            .spawner
            .spawn(members, ctx.cancel.clone())
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?;
        let members_json: Vec<Value> = outcomes
            .into_iter()
            .map(|o| {
                json!({
                    "member": o.member,
                    "session": o.session,
                    "status": o.status,
                    "summary": o.summary,
                })
            })
            .collect();
        Ok(json!({ "members": members_json }))
    }
}
