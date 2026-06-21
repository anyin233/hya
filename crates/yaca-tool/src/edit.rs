use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};
use yaca_proto::ToolSchema;

use crate::edit_replace;
use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError, obj_schema};

const UTF8_BOM: char = '\u{feff}';
const UTF8_BOM_BYTES: &[u8; 3] = b"\xEF\xBB\xBF";

#[derive(Deserialize)]
struct EditInput {
    #[serde(alias = "filePath")]
    path: String,
    #[serde(alias = "oldString")]
    old: String,
    #[serde(alias = "newString")]
    new: String,
    #[serde(default, alias = "replaceAll")]
    replace_all: bool,
}

pub(crate) struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "edit",
            "Replace `old` with `new` in a file. Errors if `old` is missing or matches more than once, unless `replace_all` is set.",
            json!({
                "filePath": {"type": "string"},
                "oldString": {"type": "string"},
                "newString": {"type": "string"},
                "replaceAll": {"type": "boolean"},
                "path": {"type": "string"},
                "old": {"type": "string"},
                "new": {"type": "string"},
                "replace_all": {"type": "boolean"}
            }),
            &["filePath", "oldString", "newString"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: EditInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        if input.old == input.new {
            return Err(ToolError::Other(
                "No changes to apply: oldString and newString are identical.".to_string(),
            ));
        }
        let workdir = normalize(&absolutize(&ctx.workdir));
        let path = resolve_file(&workdir, &input.path);
        assert_external_file(ctx, &workdir, &path).await?;
        ctx.permission
            .assert(Action::Edit, Resource::Path(display_path(&path)))
            .await?;
        if input.old.is_empty() {
            if path.exists() {
                return Err(ToolError::Other(
                    "oldString cannot be empty when editing an existing file. Provide the exact text to replace, or use write for an intentional full-file replacement."
                        .to_string(),
                ));
            }
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let (incoming_has_bom, new) = split_bom(&input.new);
            tokio::fs::write(&path, encode_with_bom(new, incoming_has_bom)).await?;
            return Ok(success_result(true, 0, &path, &workdir, "", new));
        }
        let (source_has_bom, content) = read_utf8_text(&path).await?;
        let replacement =
            edit_replace::replace(&content, &input.old, &input.new, input.replace_all)?;
        let (incoming_has_bom, updated) = split_bom(&replacement.content);
        tokio::fs::write(
            &path,
            encode_with_bom(updated, source_has_bom || incoming_has_bom),
        )
        .await?;
        Ok(success_result(
            false,
            replacement.replaced,
            &path,
            &workdir,
            &content,
            updated,
        ))
    }
}

struct FileDiff {
    patch: String,
    additions: usize,
    deletions: usize,
}

fn success_result(
    created: bool,
    replaced: usize,
    path: &Path,
    workdir: &Path,
    content_old: &str,
    content_new: &str,
) -> Value {
    let diff = file_diff(path, content_old, content_new);
    let filepath = display_path(path);
    let patch = diff.patch;
    json!({
        "created": created,
        "replaced": replaced,
        "title": relative_title(path, workdir),
        "output": "Edit applied successfully.",
        "metadata": {
            "diagnostics": {},
            "diff": patch.clone(),
            "filediff": {
                "file": filepath,
                "patch": patch,
                "additions": diff.additions,
                "deletions": diff.deletions,
            },
        },
    })
}

fn file_diff(path: &Path, content_old: &str, content_new: &str) -> FileDiff {
    let diff = TextDiff::from_lines(content_old, content_new);
    let mut additions = 0;
    let mut deletions = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => deletions += change_line_count(change.value()),
            ChangeTag::Insert => additions += change_line_count(change.value()),
            ChangeTag::Equal => {}
        }
    }
    let path = display_path(path);
    FileDiff {
        patch: diff.unified_diff().header(&path, &path).to_string(),
        additions,
        deletions,
    }
}

fn change_line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.lines().count().max(1)
}

fn relative_title(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .map_or_else(|_| display_path(path), display_path)
}

async fn read_utf8_text(path: &Path) -> Result<(bool, String), ToolError> {
    let bytes = tokio::fs::read(path).await?;
    let source_has_bom = bytes.starts_with(UTF8_BOM_BYTES);
    let bytes = bytes.strip_prefix(UTF8_BOM_BYTES).unwrap_or(&bytes);
    let text = std::str::from_utf8(bytes)
        .map_err(|err| ToolError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, err)))?;
    Ok((source_has_bom, text.to_string()))
}

fn split_bom(text: &str) -> (bool, &str) {
    if text.starts_with(UTF8_BOM) {
        return (true, &text[UTF8_BOM.len_utf8()..]);
    }
    (false, text)
}

fn encode_with_bom(text: &str, bom: bool) -> Vec<u8> {
    let extra = if bom { UTF8_BOM_BYTES.len() } else { 0 };
    let mut out = Vec::with_capacity(text.len() + extra);
    if bom {
        out.extend_from_slice(UTF8_BOM_BYTES);
    }
    out.extend_from_slice(text.as_bytes());
    out
}

async fn assert_external_file(ctx: &ToolCtx, workdir: &Path, path: &Path) -> Result<(), ToolError> {
    if path.starts_with(workdir) {
        return Ok(());
    }
    let parent = path
        .parent()
        .map_or_else(|| Path::new("/").to_path_buf(), Path::to_path_buf);
    ctx.permission
        .assert(
            Action::ExternalDirectory,
            Resource::Path(display_path(&parent.join("*"))),
        )
        .await?;
    Ok(())
}
