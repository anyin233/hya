use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use serde_json::{Value, json};

use crate::file_diff;
use crate::hashline::{
    EditPreview, HashlineError, HashlineMutation, HashlineRuntime, MAX_READ_BYTES,
    MutationBeginError, MutationText, MutationWriteError, PreparedEdit, append_bounded_notices,
    bound_output,
};
use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::lsp_post_edit;
use crate::permission::{Action, Resource};
use crate::read_media::{ReadFileKind, classify_file};
use crate::tool::{Tool, ToolCtx, ToolError, ToolResultPolicy};

/// Public Edit adapter backed by one registry-owned native runtime.
pub(crate) struct EditTool {
    runtime: Arc<HashlineRuntime>,
}

impl EditTool {
    /// Construct an Edit adapter using the registry's shared hashline runtime.
    ///
    /// # Parameters
    /// - `runtime`: Runtime shared with the matching Read adapter.
    ///
    /// # Returns
    /// An Edit tool that serializes mutations and shares Read recovery state.
    pub(crate) fn new(runtime: Arc<HashlineRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("edit"),
            description: "Apply strict hashline edits to a file.".to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path": { "type": "string" },
                    "edits": {
                        "type": "array",
                        "items": {
                            "anyOf": [
                                {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "op": { "type": "string", "enum": ["replace"] },
                                        "pos": { "type": "string" },
                                        "end": { "type": "string" },
                                        "lines": { "type": "array", "items": { "type": "string" } }
                                    },
                                    "required": ["op", "pos", "lines"]
                                },
                                {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "op": { "type": "string", "enum": ["append"] },
                                        "pos": { "type": "string" },
                                        "lines": { "type": "array", "items": { "type": "string" } }
                                    },
                                    "required": ["op", "lines"]
                                },
                                {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "op": { "type": "string", "enum": ["prepend"] },
                                        "pos": { "type": "string" },
                                        "lines": { "type": "array", "items": { "type": "string" } }
                                    },
                                    "required": ["op", "lines"]
                                },
                                {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "op": { "type": "string", "enum": ["replace_text"] },
                                        "oldText": { "type": "string" },
                                        "newText": { "type": "string" }
                                    },
                                    "required": ["op", "oldText", "newText"]
                                }
                            ]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
            output_schema: None,
        }
    }
    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::CodingWithDiff
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let request = self
            .runtime
            .parse_edit_request(input)
            .map_err(hashline_input_error)?;
        let workdir = normalize(&absolutize(&ctx.workdir));
        let requested_path = resolve_file(&workdir, request.path());

        assert_external_file(ctx, &workdir, &requested_path).await?;
        ctx.permission
            .assert(Action::Edit, Resource::Path(display_path(&requested_path)))
            .await?;

        let mut mutation = self
            .runtime
            .begin_mutation(&requested_path, ctx.session, &workdir, ctx.cancel.clone())
            .await
            .map_err(|error| match error {
                MutationBeginError::Io(error) => ToolError::Io(error),
                MutationBeginError::Cancelled => ToolError::Cancelled,
            })?;
        let metadata = tokio::fs::metadata(&requested_path).await;
        let metadata = match metadata {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ToolError::Other(format!(
                    "Edit target does not exist: {}. Use write to create a new file.",
                    display_path(&requested_path)
                )));
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.is_dir() {
            return Err(ToolError::Other(format!(
                "Cannot edit directory: {}",
                display_path(&requested_path)
            )));
        }
        match classify_file(&requested_path).await? {
            ReadFileKind::Text => {}
            ReadFileKind::Binary | ReadFileKind::Attachment(_) => {
                return Err(ToolError::Other(format!(
                    "Cannot edit binary or attachment file: {}",
                    display_path(&requested_path)
                )));
            }
        }
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        let loaded = mutation.load_current().await.map_err(ToolError::Io)?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let prepared = mutation.prepare(&request).map_err(hashline_input_error)?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if prepared.desired == prepared.original {
            return Ok(noop_result(&mutation, &prepared, &workdir));
        }

        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let write = match mutation.commit(&prepared.desired).await {
            Ok(write) => write,
            Err(MutationWriteError::Io(error)) => return Err(ToolError::Io(error)),
            Err(error @ MutationWriteError::Committed { .. }) => {
                return Err(reconcile_committed_stage(
                    &mut mutation,
                    prepared.payload_digest,
                    ctx.cancel.is_cancelled(),
                    "atomic write synchronization",
                    error.to_string(),
                )
                .await);
            }
        };

        if let Err(error) = ctx
            .formatter
            .format_file(&workdir, mutation.target_path())
            .await
        {
            return Err(reconcile_committed_stage(
                &mut mutation,
                prepared.payload_digest,
                ctx.cancel.is_cancelled(),
                "formatter failure",
                error.to_string(),
            )
            .await);
        }
        if ctx.cancel.is_cancelled() {
            return Err(
                reconcile_cancelled_after_commit(&mut mutation, prepared.payload_digest).await,
            );
        }
        if let Err(error) = mutation.restore_after_formatter().await {
            return Err(reconcile_committed_stage(
                &mut mutation,
                prepared.payload_digest,
                ctx.cancel.is_cancelled(),
                "BOM/line-ending restoration failure",
                error.to_string(),
            )
            .await);
        }
        if ctx.cancel.is_cancelled() {
            return Err(
                reconcile_cancelled_after_commit(&mut mutation, prepared.payload_digest).await,
            );
        }

        let final_text = match mutation.reload().await {
            Ok(final_text) => final_text,
            Err(error) => {
                return Err(reconcile_committed_stage(
                    &mut mutation,
                    prepared.payload_digest,
                    ctx.cancel.is_cancelled(),
                    "final reload failure",
                    error.to_string(),
                )
                .await);
            }
        };
        if ctx.cancel.is_cancelled() {
            return Err(
                reconcile_cancelled_after_commit(&mut mutation, prepared.payload_digest).await,
            );
        }
        let diagnostics =
            match lsp_post_edit::touch_and_diagnostics(&ctx.lsp, mutation.target_path()).await {
                Ok(diagnostics) => diagnostics,
                Err(error) => {
                    return Err(reconcile_committed_stage(
                        &mut mutation,
                        prepared.payload_digest,
                        ctx.cancel.is_cancelled(),
                        "LSP integration failure",
                        error.to_string(),
                    )
                    .await);
                }
            };
        if ctx.cancel.is_cancelled() {
            return Err(
                reconcile_cancelled_after_commit(&mut mutation, prepared.payload_digest).await,
            );
        }
        mutation.record_final(&final_text.text, prepared.payload_digest);
        let preview = match mutation.preview(&prepared.original, &final_text.text) {
            Ok(preview) => preview,
            Err(error) => {
                return Err(reconcile_after_commit(
                    &mut mutation,
                    prepared.payload_digest,
                    "final anchor formatting failure",
                    error.to_string(),
                )
                .await);
            }
        };
        Ok(success_result(EditSuccessContext {
            mutation: &mutation,
            loaded: &loaded,
            final_text: &final_text,
            prepared: &prepared,
            write: &write,
            preview,
            diagnostics,
            workdir: &workdir,
        }))
    }
}

/// Convert a private hashline failure into the typed input boundary.
///
/// # Parameters
/// - `error`: Native parser, anchor, or application failure.
///
/// # Returns
/// A [`ToolError::Input`] preserving the stable hashline diagnostic.
fn hashline_input_error(error: HashlineError) -> ToolError {
    ToolError::Input(error.diagnostic())
}

/// Build the no-op Edit result without exposing copied file contents.
///
/// # Parameters
/// - `mutation`: Locked target mutation.
/// - `prepared`: Validated no-op preparation.
/// - `workdir`: Normalized working directory.
///
/// # Returns
/// A bounded successful no-op result with its classification and locations.
fn noop_result(mutation: &HashlineMutation<'_>, prepared: &PreparedEdit, workdir: &Path) -> Value {
    let path = mutation.target_path();
    let warnings = prepared.warnings.clone();
    let (output, _) = append_bounded_notices(
        "No changes were made; requested edit already matches the file.".to_string(),
        &warnings,
    );
    json!({
        "created": false,
        "replaced": 0,
        "title": relative_title(path, workdir),
        "output": bound_output(output),
        "metadata": {
            "classification": "noop",
            "warnings": warnings,
            "diagnostics": {},
            "noopLocations": prepared.noop_locations,
            "diff": "",
            "filediff": {
                "file": display_path(path),
                "patch": "",
                "additions": 0,
                "deletions": 0
            }
        }
    })
}

/// Inputs collected to build one successful Edit result envelope.
///
/// The context borrows mutation facts and owns metadata that must move into
/// the result value.
struct EditSuccessContext<'borrow, 'runtime> {
    /// Locked target mutation used for the target path and final preview.
    mutation: &'borrow HashlineMutation<'runtime>,
    /// Pre-edit normalized text and warnings.
    loaded: &'borrow MutationText,
    /// Reloaded post-format normalized text and warnings.
    final_text: &'borrow MutationText,
    /// Validated edit request and its classification.
    prepared: &'borrow PreparedEdit,
    /// Atomic-write facts for the target.
    write: &'borrow crate::hashline::WriteResult,
    /// Optional bounded preview of the changed region.
    preview: Option<EditPreview>,
    /// LSP diagnostics gathered after the mutation.
    diagnostics: Value,
    /// Normalized working directory used for the result title.
    workdir: &'borrow Path,
}

/// Build a bounded successful Edit envelope from final post-format bytes.
///
/// # Parameters
/// - `context`: Borrowed mutation facts and owned result metadata.
///
/// # Returns
/// A bounded successful Edit result envelope.
fn success_result(context: EditSuccessContext<'_, '_>) -> Value {
    let EditSuccessContext {
        mutation,
        loaded,
        final_text,
        prepared,
        write,
        preview,
        diagnostics,
        workdir,
    } = context;
    let mut warnings = prepared.warnings.clone();
    warnings.extend(final_text.warnings.iter().cloned());
    warnings = dedup_warnings(warnings);
    let mut output = preview.as_ref().map_or_else(
        || "Edit applied successfully. Re-read the file to get fresh anchors.".to_string(),
        |preview| format!("Edit applied successfully.\n\n{}", preview.output),
    );
    if prepared.recovered && !output.contains("Recovered stale anchors") {
        output = format!("Recovered stale anchors.\n\n{output}");
    }
    lsp_post_edit::append_edit_diagnostics(&mut output, mutation.target_path(), &diagnostics);
    let (output, _) = append_bounded_notices(output, &warnings);
    let output = bound_output(output);

    let diff = file_diff::create(mutation.target_path(), &loaded.text, &final_text.text);
    let patch = bound_output(diff.patch);
    let total_lines = final_text.text.lines().count();
    let fallback_fits = final_text.text.len() <= MAX_READ_BYTES;
    let display = preview.as_ref().map_or_else(
        || {
            json!({
                "type": "file",
                "path": display_path(mutation.target_path()),
                "text": if fallback_fits { final_text.text.as_str() } else { "" },
                "lineStart": 1,
                "lineEnd": if fallback_fits { total_lines } else { 0 },
                "totalLines": total_lines,
                "truncated": !fallback_fits
            })
        },
        |preview: &EditPreview| {
            json!({
                "type": "file",
                "path": display_path(mutation.target_path()),
                "text": preview.content,
                "lineStart": preview.line_start,
                "lineEnd": preview.line_end,
                "totalLines": preview.total_lines,
                "truncated": false
            })
        },
    );
    json!({
        "created": write.created,
        "replaced": prepared.noop_locations.len().max(1),
        "title": relative_title(mutation.target_path(), workdir),
        "output": output,
        "metadata": {
            "classification": "applied",
            "recovered": prepared.recovered,
            "warnings": warnings,
            "diagnostics": diagnostics,
            "diff": patch,
            "filediff": {
                "file": display_path(mutation.target_path()),
                "patch": patch,
                "additions": diff.additions,
                "deletions": diff.deletions
            },
            "display": display,
            "bytes": final_text.bytes,
            "hardLink": final_text.hard_link
        }
    })
}

/// Reconcile one failed committed stage with cancellation taking typed precedence.
///
/// # Parameters
/// - `mutation`: Locked target whose authoritative bytes must be recorded.
/// - `payload_digest`: Normalized request digest used by the duplicate guard.
/// - `cancelled`: Whether cancellation was observed after the committed await.
/// - `context`: Integration stage that failed.
/// - `error`: Stage failure text passed to the bounded reconciliation path.
///
/// # Returns
/// A typed cancellation after successful reconciliation when `cancelled` is
/// true; otherwise the contextual committed-stage failure.
async fn reconcile_committed_stage(
    mutation: &mut HashlineMutation<'_>,
    payload_digest: [u8; 32],
    cancelled: bool,
    context: &str,
    error: String,
) -> ToolError {
    if cancelled {
        reconcile_cancelled_after_commit(mutation, payload_digest).await
    } else {
        reconcile_after_commit(mutation, payload_digest, context, error).await
    }
}

/// Reconcile state after a mutation has committed but integration failed.
///
/// # Parameters
/// - `mutation`: Locked target whose final bytes must be reloaded.
/// - `payload_digest`: Normalized request digest for the guard state.
/// - `context`: Integration stage that failed.
/// - `error`: Stage failure text to bound before it enters tool state.
///
/// # Returns
/// A contextual tool error stating that the target changed, with no contents.
async fn reconcile_after_commit(
    mutation: &mut HashlineMutation<'_>,
    payload_digest: [u8; 32],
    context: &str,
    error: String,
) -> ToolError {
    let path = mutation.target_path().to_path_buf();
    let error = bound_error_detail(&error);
    match mutation.reload_and_record(payload_digest).await {
        Ok(_) => ToolError::Other(format!(
            "File changed at {}; {context}: {error}",
            display_path(&path)
        )),
        Err(reload_error) => {
            let reload_error = bound_error_detail(&reload_error.to_string());
            ToolError::Other(format!(
                "File changed at {}; {context}: {error}; final reload failed: {reload_error}",
                display_path(&path)
            ))
        }
    }
}

/// Reconcile final state after cancellation is observed after the first commit.
///
/// # Parameters
/// - `mutation`: Locked target whose committed bytes must be reloaded.
/// - `payload_digest`: Normalized request digest for the guard state.
///
/// # Returns
/// [`ToolError::Cancelled`] after successful reconciliation, or a contextual
/// error when the authoritative final bytes cannot be reloaded.
async fn reconcile_cancelled_after_commit(
    mutation: &mut HashlineMutation<'_>,
    payload_digest: [u8; 32],
) -> ToolError {
    let path = mutation.target_path().to_path_buf();
    match mutation.reload_and_record(payload_digest).await {
        Ok(_) => ToolError::Cancelled,
        Err(error) => {
            let error = bound_error_detail(&error.to_string());
            ToolError::Other(format!(
                "File changed at {}; cancellation observed after commit; final reload failed: {error}",
                display_path(&path)
            ))
        }
    }
}

/// Bound one integration error at a UTF-8 boundary before it enters ToolError state.
///
/// # Parameters
/// - `value`: Formatter, filesystem, or LSP error text from a committed stage.
///
/// # Returns
/// At most 8 KiB with a content-free marker when input bytes were omitted.
fn bound_error_detail(value: &str) -> String {
    const MAX_ERROR_BYTES: usize = 8 * 1024;
    const MARKER: &str = " ... [truncated]";
    if value.len() <= MAX_ERROR_BYTES {
        return value.to_owned();
    }
    let mut keep = MAX_ERROR_BYTES.saturating_sub(MARKER.len());
    while keep > 0 && !value.is_char_boundary(keep) {
        keep -= 1;
    }
    let mut bounded = String::with_capacity(MAX_ERROR_BYTES);
    bounded.push_str(&value[..keep]);
    bounded.push_str(MARKER);
    bounded
}

/// Remove duplicate warning strings while keeping a fixed warning budget.
///
/// # Parameters
/// - `warnings`: Warning strings gathered from apply and final reload stages.
///
/// # Returns
/// Deterministically ordered, bounded warning strings.
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
///
/// # Parameters
/// - `ctx`: Tool context carrying the permission plane.
/// - `workdir`: Normalized session working directory.
/// - `path`: Lexically resolved requested path.
///
/// # Returns
/// Success when the path is inside the workdir or external access is allowed.
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

/// Compute a relative Edit title, falling back to an absolute display path.
///
/// # Parameters
/// - `path`: Resolved target path.
/// - `workdir`: Normalized working directory.
///
/// # Returns
/// A slash-normalized relative title when possible.
fn relative_title(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .map_or_else(|_| display_path(path), display_path)
}
