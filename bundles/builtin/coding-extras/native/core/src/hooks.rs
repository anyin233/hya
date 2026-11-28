//! Native ABI v2 hooks registered by `hya/coding-extras`:
//!
//! - [`PostEditHook`] (`tool.execute.after`): runs configured code formatters
//!   (with UTF-8 BOM preservation) and collects LSP diagnostics after `write`
//!   and `edit` tool executions — firing identically on both standard
//!   `hya-bare` file tools and `hya/hashline` file tools.
//! - [`AskUserVisibilityHook`] (`tool.visibility`): hides `ask_user` from
//!   model-facing schemas when no interactive user receiver is connected.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hya_runtime_abi::{
    HookPoint, HostError, HostRuntime, NativeHook, NativeToolAfterInput, NativeToolAfterOutcome,
    NativeToolResult, ToolVisibilityInput, ToolVisibilityOutcome,
};
use hya_tool::path::display_path;
use serde_json::{Value, json};

use crate::lsp_post_edit;
use crate::service::CodingExtrasState;

/// Hook identifier for the post-edit formatter + LSP diagnostics hook.
pub const POST_EDIT_HOOK_ID: &str = "coding-extras.post-edit";

/// Hook identifier for `ask_user` interactive-availability filtering.
pub const ASK_USER_VISIBILITY_HOOK_ID: &str = "coding-extras.ask-user-visibility";

/// Post-edit `tool.execute.after` hook that formats edited files and appends
/// LSP error diagnostics for both standard `hya-bare` and `hya/hashline`
/// `write`/`edit` tool results.
pub struct PostEditHook {
    state: Arc<CodingExtrasState>,
    runtime: Option<Arc<dyn HostRuntime>>,
}

impl PostEditHook {
    /// Construct a post-edit hook backed by `state` and an optional [`HostRuntime`].
    #[must_use]
    pub fn new(state: Arc<CodingExtrasState>, runtime: Option<Arc<dyn HostRuntime>>) -> Self {
        Self { state, runtime }
    }

    async fn resolve_workdir(
        &self,
        session: hya_proto::SessionId,
        file: &Path,
        output: &Value,
    ) -> PathBuf {
        if let Some(runtime) = &self.runtime
            && let Ok(Some(meta)) = runtime.session_store().session_metadata(session).await
            && let Some(workdir) = meta.workdir.filter(|dir| !dir.trim().is_empty())
        {
            return PathBuf::from(workdir);
        }
        if let Some(title) = output.get("title").and_then(Value::as_str)
            && !title.is_empty()
            && !title.contains("://")
        {
            let title_path = Path::new(title);
            if !title_path.is_absolute()
                && let Some(stripped) = strip_path_suffix(file, title_path)
            {
                return stripped;
            }
        }
        file.parent().unwrap_or(file).to_path_buf()
    }
}

#[async_trait]
impl NativeHook for PostEditHook {
    fn id(&self) -> &str {
        POST_EDIT_HOOK_ID
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::ToolExecuteAfter]
    }

    async fn tool_execute_after(
        &self,
        input: NativeToolAfterInput,
    ) -> Result<NativeToolAfterOutcome, HostError> {
        if !matches!(input.tool.as_str(), "write" | "edit") {
            return Ok(NativeToolAfterOutcome::Continue {
                result: input.result,
            });
        }
        let NativeToolResult::Ok {
            mut output,
            time_ms,
        } = input.result
        else {
            return Ok(NativeToolAfterOutcome::Continue {
                result: input.result,
            });
        };
        if output
            .pointer("/metadata/classification")
            .and_then(Value::as_str)
            == Some("noop")
        {
            return Ok(NativeToolAfterOutcome::Continue {
                result: NativeToolResult::Ok { output, time_ms },
            });
        }
        let Some(raw_path) = extract_edited_path(&output) else {
            return Ok(NativeToolAfterOutcome::Continue {
                result: NativeToolResult::Ok { output, time_ms },
            });
        };
        if raw_path.is_empty() || raw_path.contains("://") {
            return Ok(NativeToolAfterOutcome::Continue {
                result: NativeToolResult::Ok { output, time_ms },
            });
        }
        let file = PathBuf::from(&raw_path);
        if !tokio::fs::metadata(&file)
            .await
            .is_ok_and(|meta| meta.is_file())
        {
            return Ok(NativeToolAfterOutcome::Continue {
                result: NativeToolResult::Ok { output, time_ms },
            });
        }

        if !self.state.formatter().is_configured() && !self.state.lsp().is_configured() {
            return Ok(NativeToolAfterOutcome::Continue {
                result: NativeToolResult::Ok { output, time_ms },
            });
        }

        let workdir = self.resolve_workdir(input.session, &file, &output).await;
        let post_edit =
            lsp_post_edit::format_and_diagnose(&self.state, &workdir, std::slice::from_ref(&file))
                .await;
        for error in post_edit.formatter_failures {
            tracing::warn!(path = %file.display(), %error, "post-edit formatter failed");
            append_warning(&mut output, format!("Formatter failed: {error}"));
        }
        match post_edit.diagnostics {
            None => {}
            Some(Ok(diagnostics)) => {
                if let Some(metadata) = ensure_metadata_object(&mut output) {
                    metadata.insert("diagnostics".to_string(), diagnostics.clone());
                }
                let relative = file.strip_prefix(&workdir).unwrap_or(&file);
                let label = display_path(relative);
                let mut diag_block = String::new();
                lsp_post_edit::append_patch_diagnostics(
                    &mut diag_block,
                    &file,
                    &label,
                    &diagnostics,
                );
                if !diag_block.is_empty()
                    && let Some(text) = output.get("output").and_then(Value::as_str)
                {
                    let updated = format!("{text}{diag_block}");
                    if let Some(object) = output.as_object_mut() {
                        object.insert("output".to_string(), Value::String(updated));
                    }
                }
            }
            Some(Err(error)) => {
                tracing::warn!(
                    path = %file.display(),
                    %error,
                    "post-edit LSP diagnostics failed"
                );
            }
        }

        Ok(NativeToolAfterOutcome::Continue {
            result: NativeToolResult::Ok { output, time_ms },
        })
    }
}

/// `tool.visibility` hook hiding `ask_user` when no interactive human receiver
/// is connected to the turn.
pub struct AskUserVisibilityHook;

#[async_trait]
impl NativeHook for AskUserVisibilityHook {
    fn id(&self) -> &str {
        ASK_USER_VISIBILITY_HOOK_ID
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::ToolVisibility]
    }

    async fn tool_visibility(
        &self,
        input: ToolVisibilityInput,
    ) -> Result<ToolVisibilityOutcome, HostError> {
        if !input.interactive && input.candidate_tools.iter().any(|t| t == "ask_user") {
            return Ok(ToolVisibilityOutcome::Hide {
                tools: vec!["ask_user".to_string()],
            });
        }
        Ok(ToolVisibilityOutcome::Preserve)
    }
}

fn extract_edited_path(output: &Value) -> Option<String> {
    output
        .pointer("/metadata/path")
        .or_else(|| output.pointer("/metadata/filediff/file"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn strip_path_suffix(full: &Path, suffix: &Path) -> Option<PathBuf> {
    let full_components: Vec<_> = full.components().collect();
    let suffix_components: Vec<_> = suffix.components().collect();
    if suffix_components.is_empty() || suffix_components.len() >= full_components.len() {
        return None;
    }
    let split = full_components.len() - suffix_components.len();
    if full_components[split..] == suffix_components[..] {
        let mut base = PathBuf::new();
        for component in &full_components[..split] {
            base.push(component.as_os_str());
        }
        Some(base)
    } else {
        None
    }
}

fn ensure_metadata_object(output: &mut Value) -> Option<&mut serde_json::Map<String, Value>> {
    let object = output.as_object_mut()?;
    let entry = object
        .entry("metadata".to_string())
        .or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    entry.as_object_mut()
}

fn append_warning(output: &mut Value, warning: String) {
    if let Some(metadata) = ensure_metadata_object(output) {
        let warnings = metadata
            .entry("warnings".to_string())
            .or_insert_with(|| json!([]));
        if let Some(list) = warnings.as_array_mut() {
            list.push(Value::String(warning));
        }
    }
}
