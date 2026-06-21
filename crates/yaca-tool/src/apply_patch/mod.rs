mod apply;
mod parse;

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use yaca_proto::{ToolName, ToolSchema};

use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError};

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
            description: "Apply an OpenCode-style patch envelope to files.".to_string(),
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
        for hunk in hunks {
            let summary = apply::apply_hunk(&ctx.workdir, hunk).await?;
            summaries.push(summary);
        }

        let output = format!(
            "Success. Updated the following files:\n{}",
            summaries
                .iter()
                .map(apply::FileSummary::line)
                .collect::<Vec<_>>()
                .join("\n")
        );
        let files: Vec<Value> = summaries
            .into_iter()
            .map(|summary| {
                json!({
                    "path": summary.path,
                    "action": summary.action.as_str(),
                    "additions": summary.additions,
                    "deletions": summary.deletions,
                })
            })
            .collect();
        Ok(json!({ "ok": true, "output": output, "files": files }))
    }
}

fn resolve_workdir_path(workdir: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    let raw_path = Path::new(raw);
    if raw_path.is_absolute() {
        return Err(ToolError::Input(
            "apply_patch paths must be relative to the working directory".to_string(),
        ));
    }

    let mut normalized = PathBuf::new();
    for component in raw_path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                return Err(ToolError::Input(
                    "apply_patch paths must not escape the working directory".to_string(),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ToolError::Input(
                    "apply_patch paths must be relative to the working directory".to_string(),
                ));
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err(ToolError::Input("apply_patch path is empty".to_string()));
    }
    Ok(workdir.join(normalized))
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
