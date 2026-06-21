use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use yaca_proto::{ToolName, ToolSchema};

use crate::permission::{Action, PermissionError, PermissionPlane, Resource, glob_match};

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

pub struct ToolCtx {
    pub permission: PermissionPlane,
    pub workdir: PathBuf,
    pub cancel: CancellationToken,
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
        ];
        let mut tools = HashMap::new();
        for t in list {
            tools.insert(t.name().to_string(), t);
        }
        Self { tools }
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
        Ok(json!({ "content": content }))
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
        Ok(json!({ "paths": matches }))
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
        Ok(json!({ "matches": matches }))
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
            "stdout": String::from_utf8_lossy(&output.stdout).into_owned(),
            "stderr": String::from_utf8_lossy(&output.stderr).into_owned(),
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
