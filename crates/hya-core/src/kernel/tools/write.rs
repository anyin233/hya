//! Standard `write` tool for `hya-core`.
//!
//! Creates or overwrites workspace files, `local://` scratch handles, or
//! writable custom URI schemes registered on [`hya_tool::handle::HandleRouter`].
//! Enforces [`hya_tool::PermissionPlane`], [`hya_tool::ProjectScope`], and
//! coding output caps with zero hashline, `channel://`, LSP, or formatter logic.

use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::handle::{HandleError, HandleRef, SchemeRegistry, WriteHandleOutcome};
use hya_tool::path::display_path;
use hya_tool::{
    Action, Resource, Tool, ToolCtx, ToolError, ToolResultPolicy, cap_tool_output_with_policy,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    assert_external_directory, check_cancel, closed_obj_schema, map_handle_error, relative_title,
};

pub(super) struct Write;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for Write {
    fn name(&self) -> &str {
        "write"
    }

    fn schema(&self) -> ToolSchema {
        closed_obj_schema(
            "write",
            concat!(
                "Create or overwrite a file, creating parent directories.\n\n",
                "`path` also accepts `local://<name>` for agent scratch payloads or any registered ",
                "writable custom URI scheme. `artifact://` and `skill://` are read-only."
            ),
            json!({
                "path": {
                    "type": "string",
                    "description": "File path (relative to the working directory or absolute), `local://<name>`, or writable handle URI"
                },
                "content": {
                    "type": "string",
                    "description": "Full new file content"
                },
            }),
            &["path", "content"],
        )
    }

    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::Coding
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        check_cancel(ctx)?;
        let Args { path, content } = super::args(input)?;
        if path.is_empty() {
            return Err(ToolError::Input(
                "write requires a non-empty `path` string".to_string(),
            ));
        }

        // Route URI handles (`local://`, read-only `artifact://`/`skill://`, and
        // dynamic `UriSchemaHandler` schemes) generically through `HandleRouter`.
        if let Some((scheme, _)) = path.split_once("://") {
            let router = ctx.handles();
            if SchemeRegistry::is_internal_scheme(scheme) {
                let reference: HandleRef = path.parse().map_err(map_handle_error)?;
                let target = router.write_target(&reference).map_err(map_handle_error)?;
                check_cancel(ctx)?;
                ctx.permission
                    .assert(Action::Edit, Resource::Path(display_path(&target)))
                    .await?;
                check_cancel(ctx)?;
                let existed = tokio::fs::try_exists(&target).await.unwrap_or(false);
                if let Some(parent) = target.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&target, content.as_bytes()).await?;
                return Ok(cap_tool_output_with_policy(
                    json!({
                        "title": path,
                        "output": format!("Wrote {} bytes to {path}.", content.len()),
                        "ok": true,
                        "bytes": content.len(),
                        "created": !existed,
                        "metadata": {
                            "path": display_path(&target),
                            "bytes": content.len(),
                            "existed": existed,
                            "created": !existed,
                        },
                    }),
                    ToolResultPolicy::Coding,
                ));
            }

            let Some(handler) = router.uri_handler(scheme) else {
                return Err(map_handle_error(HandleError::UnknownScheme(
                    scheme.to_string(),
                )));
            };
            if !handler.writable() {
                return Err(map_handle_error(HandleError::SchemeNotWritable(
                    scheme.to_string(),
                )));
            }
            ctx.permission
                .assert(Action::Edit, Resource::Path(path.clone()))
                .await?;
            check_cancel(ctx)?;
            return match router.write_uri(ctx.session, &path, &content).await {
                Ok(WriteHandleOutcome::Handled(reply)) => {
                    let output = reply
                        .get("output")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Wrote {} bytes to {path}.", content.len()));
                    Ok(cap_tool_output_with_policy(
                        json!({
                            "title": path,
                            "output": output,
                            "ok": true,
                            "bytes": content.len(),
                            "metadata": {
                                "path": path,
                                "bytes": content.len(),
                            },
                        }),
                        ToolResultPolicy::Coding,
                    ))
                }
                Ok(WriteHandleOutcome::LocalPath(_)) => unreachable!("checked non-internal scheme"),
                Err(error) => Err(map_handle_error(error)),
            };
        }

        let resolved = super::resolve(&ctx.workdir, &path);
        assert_external_directory(ctx, &resolved, false).await?;
        check_cancel(ctx)?;
        ctx.permission
            .assert(Action::Edit, Resource::Path(display_path(&resolved)))
            .await?;
        check_cancel(ctx)?;

        let existed = tokio::fs::try_exists(&resolved).await.unwrap_or(false);
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&resolved, content.as_bytes()).await?;
        let title = relative_title(&resolved, &ctx.workdir);
        Ok(cap_tool_output_with_policy(
            json!({
                "title": title,
                "output": format!("Wrote {} bytes to {title}.", content.len()),
                "ok": true,
                "bytes": content.len(),
                "created": !existed,
                "metadata": {
                    "path": display_path(&resolved),
                    "bytes": content.len(),
                    "existed": existed,
                    "created": !existed,
                },
            }),
            ToolResultPolicy::Coding,
        ))
    }
}
