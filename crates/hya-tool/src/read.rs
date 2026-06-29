use std::path::Path;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::permission::{Action, Resource};
use crate::read_media::{ReadFileKind, attachment_value, classify_file};
use crate::read_text::read_file as read_text_file;
use crate::tool::{Tool, ToolCtx, ToolError};

const DEFAULT_READ_LIMIT: usize = 2000;

#[derive(Deserialize)]
struct ReadInput {
    #[serde(default, alias = "filePath")]
    path: Option<String>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

pub(crate) struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("read"),
            description: "Read a file or directory's contents.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "filePath": { "type": "string" },
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 0 }
                },
                "required": ["filePath"]
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: ReadInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let file_path = input
            .path
            .as_deref()
            .ok_or_else(|| ToolError::Input("missing field `filePath`".to_string()))?;
        let workdir = normalize(&absolutize(&ctx.workdir));
        let path = resolve_file(&workdir, file_path);
        let meta = tokio::fs::metadata(&path).await;
        let is_dir = matches!(&meta, Ok(meta) if meta.is_dir());
        assert_external_path(ctx, &workdir, &path, is_dir).await?;
        ctx.permission
            .assert(Action::Read, Resource::Path(display_path(&path)))
            .await?;

        let meta = match meta {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return missing_file(&path).await;
            }
            Err(err) => return Err(err.into()),
        };
        if meta.is_dir() {
            return read_directory(&path, &workdir, &input).await;
        }
        match classify_file(&path).await? {
            ReadFileKind::Text => {}
            ReadFileKind::Binary => {
                return Err(ToolError::Other(format!(
                    "Cannot read binary file: {}",
                    display_path(&path)
                )));
            }
            ReadFileKind::Attachment(mime) => {
                return Ok(attachment_value(&path, &workdir, &mime).await?);
            }
        }
        read_text_file(
            &path,
            &workdir,
            read_offset(&input),
            input.limit.unwrap_or(DEFAULT_READ_LIMIT),
        )
        .await
    }
}

async fn read_directory(
    path: &Path,
    workdir: &Path,
    input: &ReadInput,
) -> Result<Value, ToolError> {
    let mut entries = Vec::new();
    let mut dir = tokio::fs::read_dir(path).await?;
    while let Some(entry) = dir.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().await?.is_dir() {
            entries.push(format!("{name}/"));
        } else {
            entries.push(name);
        }
    }
    entries.sort_by(|a, b| match (a.ends_with('/'), b.ends_with('/')) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.cmp(b),
    });

    let offset = read_offset(input);
    let limit = input.limit.unwrap_or(DEFAULT_READ_LIMIT);
    let start = offset.saturating_sub(1);
    let sliced = entries
        .iter()
        .skip(start)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let truncated = start + sliced.len() < entries.len();
    let title = relative_title(path, workdir);
    let output = [
        format!("<path>{}</path>", display_path(path)),
        "<type>directory</type>".to_string(),
        "<entries>".to_string(),
        sliced.join("\n"),
        directory_footer(sliced.len(), entries.len(), offset, truncated),
        "</entries>".to_string(),
    ]
    .join("\n");

    Ok(json!({
        "title": title,
        "output": output,
        "content": sliced.join("\n"),
        "metadata": {
            "preview": sliced.iter().take(20).cloned().collect::<Vec<_>>().join("\n"),
            "truncated": truncated,
            "loaded": [],
            "display": {
                "type": "directory",
                "path": display_path(path),
                "entries": sliced,
                "offset": offset,
                "totalEntries": entries.len(),
                "truncated": truncated,
            },
        },
    }))
}

async fn missing_file(path: &Path) -> Result<Value, ToolError> {
    let suggestions = similar_paths(path).await;
    if suggestions.is_empty() {
        return Err(ToolError::Other(format!(
            "File not found: {}",
            display_path(path)
        )));
    }
    Err(ToolError::Other(format!(
        "File not found: {}\n\nDid you mean one of these?\n{}",
        display_path(path),
        suggestions.join("\n")
    )))
}

async fn similar_paths(path: &Path) -> Vec<String> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let base = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut suggestions = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
        return suggestions;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let name_lower = name.to_ascii_lowercase();
        if name_lower.contains(&base) || base.contains(&name_lower) {
            suggestions.push(display_path(&parent.join(name)));
        }
    }
    suggestions.sort();
    suggestions.truncate(3);
    suggestions
}

async fn assert_external_path(
    ctx: &ToolCtx,
    workdir: &Path,
    path: &Path,
    is_dir: bool,
) -> Result<(), ToolError> {
    if path.starts_with(workdir) {
        return Ok(());
    }
    let directory = if is_dir {
        path.to_path_buf()
    } else {
        path.parent()
            .map_or_else(|| Path::new("/").to_path_buf(), Path::to_path_buf)
    };
    ctx.permission
        .assert(
            Action::ExternalDirectory,
            Resource::Path(display_path(&directory.join("*"))),
        )
        .await?;
    Ok(())
}

fn read_offset(input: &ReadInput) -> usize {
    input.offset.unwrap_or(1).max(1)
}

fn relative_title(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .map_or_else(|_| display_path(path), display_path)
}

fn directory_footer(shown: usize, total: usize, offset: usize, truncated: bool) -> String {
    if truncated {
        format!(
            "\n(Showing {shown} of {total} entries. Use 'offset' parameter to read beyond entry {})",
            offset + shown
        )
    } else {
        format!("\n({total} entries)")
    }
}
