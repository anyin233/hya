use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use yaca_proto::ToolSchema;

use crate::edit_replace;
use crate::file_diff;
use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::lsp_post_edit;
use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError, obj_schema};
use crate::utf8_bom;

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
            let (incoming_has_bom, new) = utf8_bom::split(&input.new);
            tokio::fs::write(&path, utf8_bom::encode(new, incoming_has_bom)).await?;
            let formatted = ctx
                .formatter
                .format_file(&workdir, &path)
                .await
                .map_err(|error| ToolError::Other(error.to_string()))?;
            if formatted {
                utf8_bom::sync_file(&path, incoming_has_bom).await?;
            }
            let diagnostics = lsp_post_edit::touch_and_diagnostics(&ctx.lsp, &path).await?;
            return Ok(success_result(
                true,
                0,
                &path,
                &workdir,
                "",
                new,
                diagnostics,
            ));
        }
        let (source_has_bom, content) = utf8_bom::read_text(&path).await?;
        let replacement =
            edit_replace::replace(&content, &input.old, &input.new, input.replace_all)?;
        let (incoming_has_bom, updated) = utf8_bom::split(&replacement.content);
        let desired_bom = source_has_bom || incoming_has_bom;
        tokio::fs::write(&path, utf8_bom::encode(updated, desired_bom)).await?;
        let formatted = ctx
            .formatter
            .format_file(&workdir, &path)
            .await
            .map_err(|error| ToolError::Other(error.to_string()))?;
        if formatted {
            utf8_bom::sync_file(&path, desired_bom).await?;
        }
        let diagnostics = lsp_post_edit::touch_and_diagnostics(&ctx.lsp, &path).await?;
        Ok(success_result(
            false,
            replacement.replaced,
            &path,
            &workdir,
            &content,
            updated,
            diagnostics,
        ))
    }
}

fn success_result(
    created: bool,
    replaced: usize,
    path: &Path,
    workdir: &Path,
    content_old: &str,
    content_new: &str,
    diagnostics: Value,
) -> Value {
    let diff = file_diff::create(path, content_old, content_new);
    let filepath = display_path(path);
    let patch = diff.patch;
    let mut output = "Edit applied successfully.".to_string();
    lsp_post_edit::append_edit_diagnostics(&mut output, path, &diagnostics);
    json!({
        "created": created,
        "replaced": replaced,
        "title": relative_title(path, workdir),
        "output": output,
        "metadata": {
            "diagnostics": diagnostics,
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

fn relative_title(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .map_or_else(|_| display_path(path), display_path)
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
