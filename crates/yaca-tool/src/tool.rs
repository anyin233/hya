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
            Arc::new(GlobTool),
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
            "Replace the first occurrence of `old` with `new` in a file.",
            json!({"path": {"type": "string"}, "old": {"type": "string"}, "new": {"type": "string"}}),
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
        if !content.contains(&input.old) {
            return Err(ToolError::Other("old string not found".to_string()));
        }
        let updated = content.replacen(&input.old, &input.new, 1);
        tokio::fs::write(&input.path, updated).await?;
        Ok(json!({ "replaced": true }))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_small_output() {
        assert_eq!(truncate("hello"), "hello");
    }

    #[test]
    fn truncate_caps_large_output_with_marker() {
        let big = "x".repeat(MAX_OUTPUT_BYTES + 5000);
        let out = truncate(&big);
        assert!(out.len() < big.len());
        assert!(out.contains("truncated"));
    }

    #[test]
    fn truncate_never_splits_a_multibyte_char() {
        let big = "€".repeat(10_000);
        let out = truncate(&big);
        assert!(out.contains("truncated"), "must truncate");
    }
}
