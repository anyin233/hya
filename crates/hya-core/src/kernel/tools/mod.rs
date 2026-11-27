//! The kernel's seven standard tools: `read`, `write`, `edit`, `bash`, `grep`,
//! `todo`, and `rewrite`.
//!
//! `read`, `write`, `edit`, `bash`, and `grep` are complete standard coding
//! primitives wired with [`hya_tool::PermissionPlane`],
//! [`hya_tool::ProjectScope`], coding output caps, and generic
//! [`hya_tool::handle::HandleRouter`] dispatch (`artifact://`, `skill://`,
//! `local://`, plus dynamic [`hya_runtime_abi::UriSchemaHandler`] extensions).
//! They contain zero hashline, `channel://`, LSP, or formatter logic.
//!
//! `todo` and `rewrite` are permanently kernel-exclusive and cannot be
//! overridden, shadowed, or aliased.

mod bash;
mod edit;
mod grep;
mod read;
pub(super) mod rewrite;
pub(super) mod todo;
mod write;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hya_proto::ToolSchema;
use hya_tool::handle::HandleError;
use hya_tool::path::{absolutize, display_path, normalize, resolve_file};
use hya_tool::tool::obj_schema;
use hya_tool::{ProjectScope, Tool, ToolCtx, ToolError};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

pub use hya_proto::context::TODO_TOOL as TODO;
pub use hya_runtime_abi::{UriSchemaHandleAdapter, adapt_uri_schema_handler};
pub use hya_tool::{RESERVED_KERNEL_TOOLS as RESERVED, is_reserved_kernel_tool as is_reserved};

/// Name of the tool whose calls the kernel loop handles itself.
pub const REWRITE: &str = "rewrite";

/// Maximum raw UTF-8 bytes retained in a standard coding tool's model-facing output.
pub(super) const MAX_CODING_OUTPUT_BYTES: usize = 50 * 1024;

/// The seven kernel tools, in the order the system prompt lists them.
#[must_use]
pub fn builtin() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(read::Read),
        Arc::new(write::Write),
        Arc::new(edit::Edit),
        Arc::new(bash::Bash),
        Arc::new(grep::Grep),
        Arc::new(todo::Todo),
        Arc::new(rewrite::Rewrite),
    ]
}

/// Schema of the `rewrite` tool, for a tool set the kernel loop intercepts it in.
pub fn rewrite_schema() -> ToolSchema {
    rewrite::Rewrite.schema()
}

/// Schema of the `todo` tool for full-mode requests.
pub fn todo_schema() -> ToolSchema {
    todo::Todo.schema()
}

pub(super) fn args<T: DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|error| ToolError::Input(error.to_string()))
}

pub(super) fn workdir_normalized(workdir: &Path) -> PathBuf {
    normalize(&absolutize(workdir))
}

pub(super) fn resolve(workdir: &Path, path: &str) -> PathBuf {
    let base = workdir_normalized(workdir);
    resolve_file(&base, path)
}

pub(super) fn relative_title(path: &Path, workdir: &Path) -> String {
    let base = workdir_normalized(workdir);
    path.strip_prefix(&base)
        .map_or_else(|_| display_path(path), display_path)
}

pub(super) fn check_cancel(ctx: &ToolCtx) -> Result<(), ToolError> {
    if ctx.cancel.is_cancelled() {
        Err(ToolError::Cancelled)
    } else {
        Ok(())
    }
}

/// Require `ExternalDirectory` permission when `target` lies outside every
/// root in [`ProjectScope`] (ADR-0026).
pub(super) async fn assert_external_directory(
    ctx: &ToolCtx,
    target: &Path,
    is_directory: bool,
) -> Result<(), ToolError> {
    ProjectScope::for_ctx(ctx)
        .authorize(&ctx.permission, target, |scope| {
            if is_directory {
                scope.outside_directory_pattern(target)
            } else {
                scope.outside_dir_pattern(target)
            }
        })
        .await?;
    Ok(())
}

/// Authorize an external search target using the kind-blind parent pattern so
/// callers cannot probe path existence outside `ProjectScope` before approval.
pub(super) async fn assert_external_search_target(
    ctx: &ToolCtx,
    target: &Path,
) -> Result<(), ToolError> {
    assert_external_directory(ctx, target, false).await
}

/// Build a closed JSON object schema (`additionalProperties: false`).
pub(super) fn closed_obj_schema(
    name: &str,
    description: &str,
    props: Value,
    required: &[&str],
) -> ToolSchema {
    let mut schema = obj_schema(name, description, props, required);
    schema.input_schema["additionalProperties"] = json!(false);
    schema
}

/// Truncate text at a valid UTF-8 boundary not exceeding `max_bytes`.
pub(super) fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Map a [`HandleError`] into a stable [`ToolError`].
pub(super) fn map_handle_error(error: HandleError) -> ToolError {
    match error {
        HandleError::NotFound(handle) => ToolError::Other(format!("{handle} does not exist")),
        HandleError::PermissionDenied(msg) => ToolError::Other(format!("permission denied: {msg}")),
        HandleError::Io(io_err) => ToolError::Io(io_err),
        HandleError::Handler(msg) => ToolError::Other(msg),
        other => ToolError::Input(other.to_string()),
    }
}
