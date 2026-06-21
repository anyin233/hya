use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use yaca_proto::{SessionId, ToolName, ToolSchema};

use crate::apply_patch::ApplyPatchTool;
use crate::edit::EditTool;
use crate::interaction::{InteractionPlane, QuestionAnswer, QuestionKind};
use crate::invalid::InvalidTool;
use crate::lsp::{LspPlane, LspTool};
use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::permission::{Action, PermissionError, PermissionPlane, Resource, glob_match};
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
    pub session: Option<SessionId>,
    pub parent_session: Option<SessionId>,
    pub todo: TodoPlane,
    pub skills: SkillPlane,
    pub websearch: WebSearchPlane,
    pub lsp: LspPlane,
    pub workdir: PathBuf,
    pub cancel: CancellationToken,
}

const SEARCH_LIMIT: usize = 100;

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
            Arc::new(InvalidTool),
            Arc::new(ReadTool),
            Arc::new(WriteTool),
            Arc::new(EditTool),
            Arc::new(LsTool),
            Arc::new(GlobTool),
            Arc::new(FindTool),
            Arc::new(GrepTool),
            Arc::new(ShellTool),
            Arc::new(ApplyPatchTool),
            Arc::new(WebFetchTool),
            Arc::new(WebSearchTool),
            Arc::new(TodoWriteTool),
            Arc::new(QuestionTool),
            Arc::new(PlanExitTool),
            Arc::new(LspTool),
            Arc::new(SkillTool),
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
        if tokio::fs::metadata(&search)
            .await
            .map(|meta| meta.is_file())
            .unwrap_or(false)
        {
            return Err(ToolError::Other(format!(
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
        let mut files = if meta.as_ref().is_some_and(std::fs::Metadata::is_file) {
            vec![root.clone()]
        } else {
            let mut files = Vec::new();
            walk(&root, &mut files);
            files
        };
        files.sort();
        let mut rows = Vec::new();
        for f in files {
            if let Some(include) = &input.include
                && !matches_include(include, &f, &root)
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
