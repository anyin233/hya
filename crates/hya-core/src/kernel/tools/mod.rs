//! The kernel's tools: `bash`, `read`, `write`, `edit`, `rewrite`, and `todo`.
//!
//! Plain implementations: no permission checks, output truncation, timeouts,
//! or path boundaries. Relative paths resolve against `ToolCtx::workdir`.

mod bash;
mod edit;
mod read;
pub(super) mod rewrite;
pub(super) mod todo;
mod write;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hya_proto::{ToolName, ToolSchema};
use hya_tool::{Tool, ToolError};
use serde::de::DeserializeOwned;

/// Name of the tool whose calls the kernel loop handles itself.
pub const REWRITE: &str = "rewrite";

pub use hya_proto::context::TODO_TOOL as TODO;

/// The six kernel tools, in the order the system prompt lists them.
#[must_use]
pub fn builtin() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(read::Read),
        Arc::new(bash::Bash),
        Arc::new(edit::Edit),
        Arc::new(write::Write),
        Arc::new(rewrite::Rewrite),
        Arc::new(todo::Todo),
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

fn schema(name: &str, description: &str, input_schema: serde_json::Value) -> ToolSchema {
    ToolSchema {
        name: ToolName::new(name),
        description: description.to_string(),
        input_schema,
        output_schema: None,
    }
}

pub(super) fn args<T: DeserializeOwned>(input: serde_json::Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|error| ToolError::Input(error.to_string()))
}

fn resolve(workdir: &Path, path: &str) -> PathBuf {
    workdir.join(path)
}
