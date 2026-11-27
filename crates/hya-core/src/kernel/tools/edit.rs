//! Standard `edit` tool for `hya-core`.
//!
//! Performs exact unique literal-text replacement with
//! [`hya_tool::PermissionPlane`], [`hya_tool::ProjectScope`], `local://`
//! handle support, and coding diff output capping (`CodingWithDiff`). Contains
//! zero hashline, `channel://`, LSP, or formatter logic.

use std::path::PathBuf;

use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::handle::{HandleError, HandleRef, SchemeRegistry};
use hya_tool::literal_span::{UniqueSpanError, find_unique_span};
use hya_tool::path::display_path;
use hya_tool::{
    Action, Resource, Tool, ToolCtx, ToolError, ToolResultPolicy, cap_tool_output_with_policy,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    assert_external_directory, check_cancel, closed_obj_schema, map_handle_error, relative_title,
};

pub(super) struct Edit;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
    old: String,
    new: String,
}

#[async_trait]
impl Tool for Edit {
    fn name(&self) -> &str {
        "edit"
    }

    fn schema(&self) -> ToolSchema {
        closed_obj_schema(
            "edit",
            "Replace the one exact occurrence of `old` in a file (or `local://` handle) with `new`.",
            json!({
                "path": {"type": "string", "description": "File path (relative to the working directory or absolute) or `local://<name>` handle"},
                "old": {"type": "string", "description": "Exact text to replace; must occur exactly once"},
                "new": {"type": "string", "description": "Replacement text"},
            }),
            &["path", "old", "new"],
        )
    }

    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::CodingWithDiff
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        check_cancel(ctx)?;
        let Args { path, old, new } = super::args(input)?;
        if path.is_empty() {
            return Err(ToolError::Input(
                "edit requires a non-empty `path` string".to_string(),
            ));
        }
        if old == new {
            return Err(ToolError::Input(
                "`old` and `new` are identical. The file was not changed".into(),
            ));
        }
        if old.is_empty() {
            return Err(ToolError::Input("`old` must be non-empty".into()));
        }

        let (file, is_handle) = resolve_edit_target(ctx, &path)?;
        if !is_handle {
            assert_external_directory(ctx, &file, false).await?;
        }
        check_cancel(ctx)?;
        ctx.permission
            .assert(Action::Edit, Resource::Path(display_path(&file)))
            .await?;
        check_cancel(ctx)?;

        let mut text = tokio::fs::read_to_string(&file).await?;
        let span = find_unique_span(&text, &old).map_err(|error| {
            let count = match error {
                UniqueSpanError::Empty => {
                    return ToolError::Input("`old` must be non-empty".into());
                }
                UniqueSpanError::Missing => 0,
                UniqueSpanError::Multiple { .. } => {
                    let count = text.matches(old.as_str()).count();
                    if count == 1 {
                        return ToolError::Input(format!(
                            "`old` has overlapping matches in {path}; it must occur exactly once"
                        ));
                    }
                    count
                }
            };
            ToolError::Input(format!(
                "`old` occurs {count} times in {path}; it must occur exactly once"
            ))
        })?;

        let start_line = text[..span.start].bytes().filter(|&b| b == b'\n').count() + 1;
        text.replace_range(span, &new);
        tokio::fs::write(&file, &text).await?;

        let title = if is_handle {
            path
        } else {
            relative_title(&file, &ctx.workdir)
        };
        let (patch, additions, deletions) = build_simple_diff(&title, start_line, &old, &new);
        Ok(cap_tool_output_with_policy(
            json!({
                "title": title,
                "output": format!("Replaced 1 occurrence in {title}."),
                "replaced": 1,
                "created": false,
                "metadata": {
                    "path": display_path(&file),
                    "bytes": text.len(),
                    "diff": patch,
                    "filediff": {
                        "file": display_path(&file),
                        "patch": patch,
                        "additions": additions,
                        "deletions": deletions,
                    },
                },
            }),
            ToolResultPolicy::CodingWithDiff,
        ))
    }
}

fn resolve_edit_target(ctx: &ToolCtx, requested: &str) -> Result<(PathBuf, bool), ToolError> {
    let Some((scheme, _)) = requested.split_once("://") else {
        return Ok((super::resolve(&ctx.workdir, requested), false));
    };
    if SchemeRegistry::is_internal_scheme(scheme) {
        let reference: HandleRef = requested.parse().map_err(map_handle_error)?;
        let target = ctx
            .handles()
            .write_target(&reference)
            .map_err(map_handle_error)?;
        return Ok((target, true));
    }
    if ctx.handles().has_uri_handler(scheme) {
        return Err(ToolError::Input(format!(
            "{scheme}:// does not support in-place `edit`; use `write` instead"
        )));
    }
    Err(map_handle_error(HandleError::UnknownScheme(
        scheme.to_string(),
    )))
}

fn build_simple_diff(
    title: &str,
    start_line: usize,
    old: &str,
    new: &str,
) -> (String, usize, usize) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let deletions = old_lines.len().max(usize::from(!old.is_empty()));
    let additions = new_lines.len().max(usize::from(!new.is_empty()));
    let mut patch = format!(
        "--- a/{title}\n+++ b/{title}\n@@ -{start_line},{deletions} +{start_line},{additions} @@\n"
    );
    for line in &old_lines {
        patch.push('-');
        patch.push_str(line);
        patch.push('\n');
    }
    for line in &new_lines {
        patch.push('+');
        patch.push_str(line);
        patch.push('\n');
    }
    (patch, additions, deletions)
}
