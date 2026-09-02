use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::hashline::{
    HashlineMutation, HashlineRuntime, MAX_READ_BYTES, MutationBeginError, MutationText,
    MutationWriteError, append_bounded_notices, bound_output,
};
use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::lsp_post_edit;
use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError, ToolResultPolicy};
use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};

/// Closed model-facing Write arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    /// Lexical path selected by the caller.
    path: String,
    /// Complete replacement text.
    content: String,
}

/// Whole-file writer backed by the registry-owned hashline runtime.
pub(crate) struct WriteTool {
    runtime: Arc<HashlineRuntime>,
}

impl WriteTool {
    /// Construct a Write adapter using the registry's shared hashline runtime.
    ///
    /// # Parameters
    /// - `runtime`: Runtime shared with Read, Edit, and Grep adapters.
    ///
    /// # Returns
    /// A writer that serializes target mutations and records final snapshots.
    pub(crate) fn new(runtime: Arc<HashlineRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("write"),
            description: "Write content to a file (creating parent dirs).".to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
            output_schema: None,
        }
    }

    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::Coding
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        check_cancel(ctx)?;
        let input: WriteInput =
            serde_json::from_value(input).map_err(|error| ToolError::Input(error.to_string()))?;
        let workdir = normalize(&absolutize(&ctx.workdir));
        let requested_path = resolve_file(&workdir, &input.path);

        // Keep the lexical permission boundary before any symlink resolution or I/O.
        assert_external_file(ctx, &workdir, &requested_path).await?;
        ctx.permission
            .assert(Action::Edit, Resource::Path(display_path(&requested_path)))
            .await?;
        check_cancel(ctx)?;

        let mut mutation = self
            .runtime
            .begin_write(&requested_path, ctx.session, &workdir, ctx.cancel.clone())
            .await
            .map_err(map_mutation_begin_error)?;
        let loaded = mutation
            .load_current_or_empty()
            .await
            .map_err(ToolError::Io)?;
        check_cancel(ctx)?;

        let (content, stripped) = strip_hashline_display_prefixes(&input.content);
        check_cancel(ctx)?;
        let write = match mutation.commit_write(&content).await {
            Ok(write) => write,
            Err(MutationWriteError::Io(error)) => return Err(ToolError::Io(error)),
            Err(error @ MutationWriteError::Committed { .. }) => {
                return Err(reconcile_after_commit(
                    &mut mutation,
                    "atomic write synchronization",
                    error.to_string(),
                )
                .await);
            }
        };

        // Cancellation after the first replacement is a committed mutation and must
        // retain the actual final snapshot instead of reporting an untouched file.
        if ctx.cancel.is_cancelled() {
            return Err(reconcile_cancelled_after_commit(&mut mutation).await);
        }
        if let Err(error) = ctx
            .formatter
            .format_file(&workdir, mutation.target_path())
            .await
        {
            return Err(reconcile_after_commit(
                &mut mutation,
                "formatter failure",
                error.to_string(),
            )
            .await);
        }
        if ctx.cancel.is_cancelled() {
            return Err(reconcile_cancelled_after_commit(&mut mutation).await);
        }
        if let Err(error) = mutation.restore_after_formatter().await {
            return Err(reconcile_after_commit(
                &mut mutation,
                "BOM/line-ending restoration failure",
                error.to_string(),
            )
            .await);
        }
        if ctx.cancel.is_cancelled() {
            return Err(reconcile_cancelled_after_commit(&mut mutation).await);
        }

        let final_text = match mutation.reload().await {
            Ok(final_text) => final_text,
            Err(error) => {
                return Err(reconcile_after_commit(
                    &mut mutation,
                    "final reload failure",
                    error.to_string(),
                )
                .await);
            }
        };
        if ctx.cancel.is_cancelled() {
            return Err(reconcile_cancelled_after_commit(&mut mutation).await);
        }

        let (executable, chmod_warning) =
            match executable_state(mutation.target_path(), &final_text.text).await {
                Ok(state) => state,
                Err(error) => {
                    return Err(reconcile_after_commit(
                        &mut mutation,
                        "executable metadata",
                        error.to_string(),
                    )
                    .await);
                }
            };
        if ctx.cancel.is_cancelled() {
            return Err(reconcile_cancelled_after_commit(&mut mutation).await);
        }

        let diagnostics =
            match lsp_post_edit::touch_and_diagnostics(&ctx.lsp, mutation.target_path()).await {
                Ok(diagnostics) => diagnostics,
                Err(error) => {
                    return Err(reconcile_after_commit(
                        &mut mutation,
                        "LSP integration failure",
                        error.to_string(),
                    )
                    .await);
                }
            };
        if ctx.cancel.is_cancelled() {
            return Err(reconcile_cancelled_after_commit(&mut mutation).await);
        }

        mutation.record_write(&final_text);
        Ok(success_result(WriteSuccessContext {
            write: &write,
            loaded: &loaded,
            final_text: &final_text,
            executable,
            chmod_warning,
            stripped,
            diagnostics,
            workdir: &workdir,
        }))
    }
}

/// Return a cancellation error when a Write call has been cancelled.
fn check_cancel(ctx: &ToolCtx) -> Result<(), ToolError> {
    if ctx.cancel.is_cancelled() {
        Err(ToolError::Cancelled)
    } else {
        Ok(())
    }
}

/// Map a mutation-start failure to the stable tool error boundary.
fn map_mutation_begin_error(error: MutationBeginError) -> ToolError {
    match error {
        MutationBeginError::Io(error) => ToolError::Io(error),
        MutationBeginError::Cancelled => ToolError::Cancelled,
    }
}

/// Inputs collected to build one successful Write result envelope.
///
/// The context borrows mutation facts and owns metadata that must move into
/// the result value.
struct WriteSuccessContext<'a> {
    /// Atomic-write facts for the target.
    write: &'a crate::hashline::WriteResult,
    /// Pre-write normalized text and warnings.
    loaded: &'a MutationText,
    /// Reloaded post-formatter normalized text and warnings.
    final_text: &'a MutationText,
    /// Whether the final target has execute bits.
    executable: bool,
    /// Non-fatal warning from shebang chmod handling.
    chmod_warning: Option<String>,
    /// Whether copied hashline display prefixes were stripped.
    stripped: bool,
    /// LSP diagnostics gathered after the write.
    diagnostics: Value,
    /// Normalized working directory used for the result title.
    workdir: &'a Path,
}

/// Build a successful bounded Write envelope from final post-formatter facts.
///
/// # Parameters
/// - `context`: Borrowed write facts and owned result metadata.
///
/// # Returns
/// A bounded successful Write result envelope.
fn success_result(context: WriteSuccessContext<'_>) -> Value {
    let WriteSuccessContext {
        write,
        loaded,
        final_text,
        executable,
        chmod_warning,
        stripped,
        diagnostics,
        workdir,
    } = context;
    let mut warnings = loaded.warnings.clone();
    warnings.extend(final_text.warnings.iter().cloned());
    if stripped {
        warnings.push("Hashline display prefixes were stripped before writing.".to_string());
    }
    if let Some(warning) = chmod_warning {
        warnings.push(warning);
    }
    let warnings = dedup_warnings(warnings);

    let (display_text, truncated) = bounded_display_text(&final_text.text);
    let total_lines = visible_line_count(&final_text.text);
    let line_end = visible_line_count(&display_text);
    let preview = display_text.lines().take(20).collect::<Vec<_>>().join("\n");

    let mut output = "Wrote file successfully.".to_string();
    output.push_str("\n\n<content>\n");
    output.push_str(&display_text);
    output.push_str("\n</content>");
    lsp_post_edit::append_write_diagnostics(&mut output, &write.path, &diagnostics);
    let (output, _) = append_bounded_notices(output, &warnings);
    let output = bound_output(output);
    let path = display_path(&write.path);
    json!({
        "ok": true,
        "bytes": final_text.bytes,
        "title": relative_title(&write.path, workdir),
        "output": output,
        "content": display_text.clone(),
        "metadata": {
            "path": path,
            "bytes": final_text.bytes,
            "existed": !write.created,
            "created": write.created,
            "hardLink": final_text.hard_link,
            "executable": executable,
            "warnings": warnings,
            "diagnostics": diagnostics,
            "preview": preview,
            "truncated": truncated,
            "display": {
                "type": "file",
                "path": display_path(&write.path),
                "text": display_text,
                "lineStart": 1,
                "lineEnd": line_end,
                "totalLines": total_lines,
                "truncated": truncated
            }
        }
    })
}

/// Reconcile the authoritative final text after a committed integration failure.
async fn reconcile_after_commit(
    mutation: &mut HashlineMutation<'_>,
    context: &str,
    error: String,
) -> ToolError {
    let path = mutation.target_path().to_path_buf();
    match mutation.reload().await {
        Ok(final_text) => {
            mutation.record_write(&final_text);
            ToolError::Other(format!(
                "File changed at {}; {context}: {}",
                display_path(&path),
                bound_output(error),
            ))
        }
        Err(reload_error) => ToolError::Other(format!(
            "File changed at {}; {context}: {}; final reload failed: {}",
            display_path(&path),
            bound_output(error),
            reload_error,
        )),
    }
}

/// Reconcile the committed target before returning a cancellation result.
async fn reconcile_cancelled_after_commit(mutation: &mut HashlineMutation<'_>) -> ToolError {
    let path = mutation.target_path().to_path_buf();
    match mutation.reload().await {
        Ok(final_text) => {
            mutation.record_write(&final_text);
            ToolError::Cancelled
        }
        Err(error) => ToolError::Other(format!(
            "File changed at {}; cancellation observed after commit; final reload failed: {error}",
            display_path(&path)
        )),
    }
}

/// Convert a chmod failure into a bounded, content-free warning.
fn chmod_failure_warning(error: &std::io::Error) -> String {
    format!(
        "Could not mark shebang file executable: chmod failed ({}).",
        bound_output(error.to_string())
    )
}
/// Map a chmod failure to a nonfatal executable-state warning.
fn map_chmod_failure(error: std::io::Error) -> (bool, Option<String>) {
    (false, Some(chmod_failure_warning(&error)))
}

/// Return actual execute-bit state and attempt `chmod a+x` for a shebang file.
async fn executable_state(
    path: &Path,
    content: &str,
) -> Result<(bool, Option<String>), std::io::Error> {
    let metadata = tokio::fs::metadata(path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let current_mode = metadata.permissions().mode();
        let chmod_warning = if content.starts_with("#!") && current_mode & 0o111 != 0o111 {
            let mut permissions = metadata.permissions();
            permissions.set_mode(current_mode | 0o111);
            match tokio::fs::set_permissions(path, permissions).await {
                Ok(()) => None,
                Err(error) => map_chmod_failure(error).1,
            }
        } else {
            None
        };
        let final_mode = tokio::fs::metadata(path).await?.permissions().mode();
        Ok((final_mode & 0o111 != 0, chmod_warning))
    }
    #[cfg(not(unix))]
    {
        let warning = content.starts_with("#!").then(|| {
            "Could not mark shebang file executable: chmod is unavailable on this platform."
                .to_string()
        });
        let _ = metadata;
        Ok((false, warning))
    }
}

/// Strip copied hashline headers and rows only when every row is unambiguous.
fn strip_hashline_display_prefixes(content: &str) -> (String, bool) {
    let trailing_newline = content.ends_with('\n');
    let mut rows = content.split('\n').collect::<Vec<_>>();
    if trailing_newline {
        rows.pop();
    }
    if rows.is_empty() {
        return (content.to_string(), false);
    }

    let mut first_row = 0usize;
    if is_hashline_header(rows[0]) {
        first_row = 1;
    }
    if first_row == rows.len() {
        return (content.to_string(), false);
    }

    let mut previous_line: Option<usize> = None;
    let mut stripped_rows = Vec::with_capacity(rows.len() - first_row);
    for row in &rows[first_row..] {
        let Some((line_number, payload)) = parse_hashline_row(row) else {
            return (content.to_string(), false);
        };
        if previous_line.is_some_and(|previous| previous.checked_add(1) != Some(line_number)) {
            return (content.to_string(), false);
        }
        previous_line = Some(line_number);
        stripped_rows.push(payload);
    }

    let mut stripped = stripped_rows.join("\n");
    if trailing_newline {
        stripped.push('\n');
    }
    (stripped, true)
}

/// Parse one configured-width hashline row and return its source payload.
fn parse_hashline_row(row: &str) -> Option<(usize, &str)> {
    let mut row = row.trim_start();
    if let Some(rest) = row.strip_prefix(">>>") {
        row = rest.trim_start();
    } else if let Some(rest) = row.strip_prefix(">>") {
        row = rest.trim_start();
    }
    if row
        .chars()
        .next()
        .is_some_and(|character| matches!(character, '+' | '-' | '*'))
    {
        row = row[1..].trim_start();
    }
    let hash_start = row.find('#')?;
    let line_part = row[..hash_start].trim();
    if line_part.is_empty() || !line_part.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let line_number = line_part.parse::<usize>().ok()?;
    if line_number == 0 {
        return None;
    }
    let hash_and_payload = &row[hash_start + 1..];
    let colon = hash_and_payload.find(':')?;
    let hash = &hash_and_payload[..colon];
    if hash.len() != 2 || !hash.bytes().all(|byte| b"ZPMQVRWSNKTXJBYH".contains(&byte)) {
        return None;
    }
    Some((line_number, &hash_and_payload[colon + 1..]))
}

/// Recognize one optional hashline section header without accepting metadata.
fn is_hashline_header(row: &str) -> bool {
    let row = row.trim();
    let Some(header) = row
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return false;
    };
    let Some((path, tag)) = header.rsplit_once('#') else {
        return false;
    };
    !path.is_empty() && tag.len() == 4 && tag.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Bound final display text at a valid UTF-8 boundary.
fn bounded_display_text(text: &str) -> (String, bool) {
    if text.len() <= MAX_READ_BYTES {
        return (text.to_owned(), false);
    }
    let mut end = MAX_READ_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// Count visible lines while excluding a terminal newline sentinel.
fn visible_line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let count = text.bytes().filter(|byte| *byte == b'\n').count() + 1;
    if text.ends_with('\n') {
        count.saturating_sub(1)
    } else {
        count
    }
}

/// Deduplicate warnings while retaining a fixed bounded list.
fn dedup_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    for warning in warnings {
        if !result.iter().any(|existing| existing == &warning) {
            result.push(warning);
        }
        if result.len() >= 32 {
            break;
        }
    }
    result
}

/// Require external-directory permission before mutating an outside path.
async fn assert_external_file(ctx: &ToolCtx, workdir: &Path, path: &Path) -> Result<(), ToolError> {
    if path.starts_with(workdir) {
        return Ok(());
    }
    let parent = path
        .parent()
        .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
    ctx.permission
        .assert(
            Action::ExternalDirectory,
            Resource::Path(display_path(&parent.join("*"))),
        )
        .await?;
    Ok(())
}

/// Compute a relative Write title, falling back to an absolute display path.
fn relative_title(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .map_or_else(|_| display_path(path), display_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Verify missing executable metadata maps to a bounded chmod warning.
    #[tokio::test]
    async fn missing_target_chmod_failure_is_nonfatal_and_content_free() {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!(
                "hya-write-missing-chmod-{}-{id}",
                std::process::id()
            ))
            .join("target");
        assert!(!path.exists());

        let (executable, warning) = executable_state(&path, "#!/bin/sh\nsecret\n")
            .await
            .unwrap_or_else(map_chmod_failure);
        let warning = match warning {
            Some(warning) => warning,
            None => panic!("missing target must produce a chmod warning"),
        };
        assert!(!executable);
        assert!(warning.contains("chmod"));
        assert!(warning.len() <= MAX_READ_BYTES);
        assert!(!warning.contains("secret"));
    }
}
