mod apply;
mod parse;

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::lsp_post_edit;
use crate::utf8_bom;
use hya_tool::{Action, ProjectScope, Resource};
use hya_tool::{Tool, ToolCtx, ToolError};

pub(crate) struct ApplyPatchTool;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApplyPatchInput {
    #[serde(alias = "patch")]
    patch_text: String,
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("apply_patch"),
            description: "Apply an Compat-style patch envelope to files.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "patchText": {
                        "type": "string",
                        "description": "The full patch text that describes all changes to be made"
                    }
                },
                "required": ["patchText"]
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: ApplyPatchInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let hunks = parse::parse_patch(&input.patch_text).map_err(ToolError::Input)?;
        if hunks.is_empty() {
            return Err(ToolError::Other("patch rejected: empty patch".to_string()));
        }

        // Every target must lie inside a Project root (ADR-0026), judged after
        // symlink resolution, before any permission ask or file I/O.
        let scope = ProjectScope::for_ctx(ctx);
        for hunk in &hunks {
            ensure_in_project(&scope, &ctx.workdir, hunk.path())?;
            if let Some(move_path) = hunk.move_path() {
                ensure_in_project(&scope, &ctx.workdir, move_path)?;
            }
        }
        for hunk in &hunks {
            let path = resolve_workdir_path(&ctx.workdir, hunk.path())?;
            ctx.permission
                .assert(Action::Edit, Resource::Path(display_path(&path)))
                .await?;
            if let Some(move_path) = hunk.move_path() {
                let move_path = resolve_workdir_path(&ctx.workdir, move_path)?;
                ctx.permission
                    .assert(Action::Edit, Resource::Path(display_path(&move_path)))
                    .await?;
            }
        }

        let mut summaries = Vec::with_capacity(hunks.len());
        let mut lsp_paths = Vec::new();
        for hunk in hunks {
            let summary = apply::apply_hunk(&ctx.workdir, hunk).await?;
            if !matches!(summary.action, apply::FileAction::Delete) {
                let path = resolve_workdir_path(&ctx.workdir, &summary.path)?;
                let formatted = ctx
                    .formatter
                    .format_file(&ctx.workdir, &path)
                    .await
                    .map_err(|error| ToolError::Other(error.to_string()))?;
                if formatted {
                    utf8_bom::sync_file(&path, summary.bom).await?;
                }
                lsp_paths.push(path);
            }
            summaries.push(summary);
        }
        let diagnostics = lsp_post_edit::touch_many_and_diagnostics(ctx, &lsp_paths).await?;

        let mut output = format!(
            "Success. Updated the following files:\n{}",
            summaries
                .iter()
                .map(apply::FileSummary::line)
                .collect::<Vec<_>>()
                .join("\n")
        );
        for path in &lsp_paths {
            let relative = path.strip_prefix(&ctx.workdir).unwrap_or(path);
            lsp_post_edit::append_patch_diagnostics(
                &mut output,
                path,
                &display_path(relative),
                &diagnostics,
            );
        }
        let diff = summaries.iter().fold(String::new(), |mut out, summary| {
            out.push_str(&summary.patch);
            if !summary.patch.ends_with('\n') {
                out.push('\n');
            }
            out
        });
        let mut files = Vec::with_capacity(summaries.len());
        let mut metadata_files = Vec::with_capacity(summaries.len());
        for summary in &summaries {
            let source = resolve_workdir_path(&ctx.workdir, &summary.source_path)?;
            let target = resolve_workdir_path(&ctx.workdir, &summary.path)?;
            let relative = target.strip_prefix(&ctx.workdir).unwrap_or(&target);
            files.push(json!({
                "path": summary.path.clone(),
                "action": summary.action.as_str(),
                "additions": summary.additions,
                "deletions": summary.deletions,
            }));
            let mut metadata_file = json!({
                "filePath": display_path(&source),
                "relativePath": display_path(relative),
                "type": summary.action.compat_type(),
                "patch": summary.patch.clone(),
                "additions": summary.additions,
                "deletions": summary.deletions,
            });
            if matches!(summary.action, apply::FileAction::Move) {
                metadata_file["movePath"] = json!(display_path(&target));
            }
            metadata_files.push(metadata_file);
        }
        Ok(json!({
            "ok": true,
            "title": output,
            "output": output,
            "files": files,
            "metadata": {
                "diff": diff,
                "files": metadata_files,
                "diagnostics": diagnostics,
            },
        }))
    }
}

/// Resolve a patch path against the workdir.
///
/// Relative paths resolve against the workdir; absolute paths are kept. A
/// `..` component is rejected so the checked and the written path agree.
fn resolve_workdir_path(workdir: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    let mut normalized = PathBuf::new();
    let mut named = false;
    for component in Path::new(raw).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                named = true;
                normalized.push(part);
            }
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
            Component::ParentDir => {
                return Err(ToolError::Input(
                    "apply_patch paths must not contain `..`".to_string(),
                ));
            }
        }
    }
    if !named {
        return Err(ToolError::Input("apply_patch path is empty".to_string()));
    }
    Ok(workdir.join(normalized))
}

/// Reject a patch path that lies outside every Project root.
fn ensure_in_project(scope: &ProjectScope, workdir: &Path, raw: &str) -> Result<(), ToolError> {
    let path = resolve_workdir_path(workdir, raw)?;
    if scope.contains(&path) {
        Ok(())
    } else {
        Err(ToolError::Input(format!(
            "apply_patch path is outside the Project roots: {raw}"
        )))
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
