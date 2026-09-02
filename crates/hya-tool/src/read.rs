use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::hashline::{
    DEFAULT_READ_LIMIT, HashlineRuntime, MAX_READ_BYTES, ReadOptions, ReadResult, ReadRuntimeError,
};
use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::permission::{Action, Resource};
use crate::read_media::{ReadFileKind, attachment_value, classify_file};
use crate::tool::{Tool, ToolCtx, ToolError, ToolResultPolicy};

/// Accepted Read arguments, including the hidden legacy path spelling.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    /// Canonical model-facing path.
    #[serde(default)]
    path: Option<String>,
    /// Legacy compatibility path retained for captured sessions.
    #[serde(rename = "filePath", default)]
    file_path: Option<String>,
    /// One-based first line, with zero accepted only as a legacy spelling.
    #[serde(default)]
    offset: Option<usize>,
    /// Maximum number of visible lines.
    #[serde(default)]
    limit: Option<usize>,
    /// Return unanchored normalized text.
    #[serde(default)]
    raw: bool,
}

/// Public Read adapter backed by one registry-owned native runtime.
pub(crate) struct ReadTool {
    runtime: Arc<HashlineRuntime>,
}

impl ReadTool {
    /// Construct a Read adapter using the registry's shared hashline runtime.
    ///
    /// # Parameters
    /// - `runtime`: Runtime shared with the matching Edit adapter.
    ///
    /// # Returns
    /// A Read tool that keeps snapshots isolated inside the supplied runtime.
    pub(crate) fn new(runtime: Arc<HashlineRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("read"),
            description: "Read a file or directory's contents.".to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 1 },
                    "limit": { "type": "integer", "minimum": 1 },
                    "raw": { "type": "boolean" }
                },
                "required": ["path"]
            }),
            output_schema: None,
        }
    }
    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::Coding
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        check_cancel(ctx)?;
        reject_null_fields(&input)?;
        let input: ReadInput =
            serde_json::from_value(input).map_err(|error| ToolError::Input(error.to_string()))?;
        let (file_path, legacy_display) = read_path(&input)?;
        let offset = read_offset(&input);
        let limit = input.limit.unwrap_or(DEFAULT_READ_LIMIT);
        if limit == 0 {
            return Err(ToolError::Input(
                "limit must be a positive integer".to_string(),
            ));
        }

        let workdir = normalize(&absolutize(&ctx.workdir));
        let path = resolve_file(&workdir, file_path);
        check_cancel(ctx)?;
        let external_result = assert_external_path(ctx, &workdir, &path).await;
        check_cancel(ctx)?;
        external_result?;
        check_cancel(ctx)?;
        let permission_result = ctx
            .permission
            .assert(Action::Read, Resource::Path(display_path(&path)))
            .await;
        check_cancel(ctx)?;
        permission_result?;

        check_cancel(ctx)?;
        let meta = tokio::fs::metadata(&path).await;
        check_cancel(ctx)?;

        let meta = match meta {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                check_cancel(ctx)?;
                let result = missing_file(&path, &ctx.cancel).await;
                check_cancel(ctx)?;
                return result;
            }
            Err(error) => return Err(error.into()),
        };
        if meta.is_dir() {
            check_cancel(ctx)?;
            let result = read_directory(&path, &workdir, &input, &ctx.cancel).await;
            check_cancel(ctx)?;
            return result;
        }

        check_cancel(ctx)?;
        let file_kind_result = classify_file(&path).await;
        check_cancel(ctx)?;
        let file_kind = file_kind_result?;
        match file_kind {
            ReadFileKind::Text => {}
            ReadFileKind::Binary => {
                return Err(ToolError::Other(format!(
                    "Cannot read binary file: {}",
                    display_path(&path)
                )));
            }
            ReadFileKind::Attachment(mime) => {
                check_cancel(ctx)?;
                let attachment_result = attachment_value(&path, &workdir, &mime).await;
                check_cancel(ctx)?;
                return Ok(attachment_result?);
            }
        }

        check_cancel(ctx)?;
        let runtime_result = self
            .runtime
            .read_text(
                &path,
                ReadOptions {
                    session: ctx.session,
                    workdir: &workdir,
                    offset,
                    limit,
                    raw: input.raw,
                    cancel: ctx.cancel.clone(),
                },
            )
            .await
            .map_err(read_runtime_error);
        // A successful runtime call has committed its snapshot. Do not turn a later
        // cancellation into a failed Read.
        let result = match runtime_result {
            Ok(result) => result,
            Err(error) => {
                check_cancel(ctx)?;
                return Err(error);
            }
        };
        if result.total_lines < offset && !(result.total_lines == 0 && offset == 1) {
            return Err(ToolError::Input(format!(
                "[E_BAD_READ] Offset {offset} is out of range for this file ({} lines)",
                result.total_lines
            )));
        }
        Ok(file_value(&result, &workdir, input.raw, legacy_display))
    }
}

/// Resolve canonical and legacy path spellings without trimming filenames.
///
/// # Parameters
/// - `input`: Parsed Read arguments containing the two optional path fields.
///
/// # Returns
/// The selected path and whether a sole legacy spelling was used.
fn read_path(input: &ReadInput) -> Result<(&str, bool), ToolError> {
    let canonical = input.path.as_deref();
    let legacy = input.file_path.as_deref();
    match (canonical, legacy) {
        (Some(path), Some(file_path)) if !path.is_empty() && !file_path.is_empty() => {
            if path != file_path {
                return Err(ToolError::Input(
                    "Read request contains conflicting `filePath` and `path` values.".to_string(),
                ));
            }
            Ok((path, false))
        }
        (Some(path), _) if !path.is_empty() => Ok((path, false)),
        (_, Some(file_path)) if !file_path.is_empty() => Ok((file_path, true)),
        _ => Err(ToolError::Input(
            "Read request requires a non-empty `path` string.".to_string(),
        )),
    }
}

/// Reject explicit JSON nulls that do not satisfy the published Read field types.
///
/// # Parameters
/// - `input`: Raw Read argument value before Serde optional-field normalization.
///
/// # Returns
/// A typed input failure for a null known field, otherwise success.
fn reject_null_fields(input: &Value) -> Result<(), ToolError> {
    let Some(object) = input.as_object() else {
        return Ok(());
    };
    for field in ["path", "filePath", "offset", "limit", "raw"] {
        if object.get(field).is_some_and(Value::is_null) {
            return Err(ToolError::Input(format!("{field} must not be null")));
        }
    }
    Ok(())
}

/// Convert a runtime Read failure into the stable tool error boundary.
///
/// # Parameters
/// - `error`: Native Read runtime failure.
///
/// # Returns
/// A typed input error for hashline failures, cancellation for cancelled work,
/// or an I/O error for filesystem failures.
fn read_runtime_error(error: ReadRuntimeError) -> ToolError {
    match error {
        ReadRuntimeError::Io(error) => ToolError::Io(error),
        ReadRuntimeError::Hashline(error) => ToolError::Input(error.diagnostic()),
        ReadRuntimeError::Cancelled => ToolError::Cancelled,
    }
}

/// Bounded file facts used to keep every Read view synchronized after fitting.
struct FittedRead {
    /// Model-facing output, including wrappers and continuation notices.
    output: String,
    /// Complete source rows represented by the output.
    content: String,
    /// One-based final represented line, or zero for an empty file.
    line_end: usize,
    /// Continuation offset when another complete row can be requested.
    next_offset: Option<usize>,
    /// Whether rows or notices were omitted from the complete result.
    truncated: bool,
}
/// Borrowed rendering data shared by each bounded Read candidate.
struct RenderParts<'a> {
    /// Content-free diagnostic used when no complete row fits.
    diagnostic: &'a str,
    /// Continuation notice reserved inside the byte budget.
    continuation: &'a str,
    /// Warning block reserved inside the byte budget.
    warnings: &'a str,
    /// Whether the byte budget caused or forced truncation.
    byte_limited: bool,
}
/// Bounded directory facts shared by output, content, and display metadata.
struct FittedDirectory {
    /// Model-facing directory wrapper and footer.
    output: String,
    /// Sorted entries represented by every directory view.
    entries: Vec<String>,
    /// Whether entries remain after this page.
    truncated: bool,
    /// One-based offset for the next represented page.
    next_offset: Option<usize>,
}

/// Return a cancellation error when a call has been cancelled.
///
/// # Parameters
/// - `ctx`: Tool context carrying the call cancellation token.
///
/// # Returns
/// `Ok(())` when the call is active, otherwise [`ToolError::Cancelled`].
fn check_cancel(ctx: &ToolCtx) -> Result<(), ToolError> {
    if ctx.cancel.is_cancelled() {
        Err(ToolError::Cancelled)
    } else {
        Ok(())
    }
}

/// Return a cancellation error for a helper that receives only a token.
///
/// # Parameters
/// - `cancel`: Call cancellation token to inspect.
///
/// # Returns
/// `Ok(())` when the token is active, otherwise [`ToolError::Cancelled`].
fn check_token(cancel: &CancellationToken) -> Result<(), ToolError> {
    if cancel.is_cancelled() {
        Err(ToolError::Cancelled)
    } else {
        Ok(())
    }
}

/// Format bounded warning metadata for inclusion in a Read output.
///
/// # Parameters
/// - `warnings`: Already bounded warning strings from the runtime.
///
/// # Returns
/// A warning block whose byte length is reserved by the fitting pass.
fn warning_notice(warnings: &[String]) -> String {
    if warnings.is_empty() {
        return String::new();
    }
    format!(
        "\n\n<hashline_warnings>\n{}\n</hashline_warnings>",
        warnings
            .iter()
            .map(|warning| format!("- {warning}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Build the explicit continuation notice for a fitted text slice.
///
/// # Parameters
/// - `line_start`: One-based first represented line.
/// - `line_end`: One-based last represented line.
/// - `total_lines`: Number of visible lines in the complete file.
/// - `byte_limited`: Whether the byte budget, rather than only the line limit,
///   caused truncation.
///
/// # Returns
/// A content-free continuation notice with a stable next offset.
fn continuation_notice(
    line_start: usize,
    line_end: usize,
    total_lines: usize,
    byte_limited: bool,
) -> String {
    let reason = if byte_limited {
        format!(" ({} byte limit)", MAX_READ_BYTES)
    } else {
        String::new()
    };
    format!(
        "[Showing lines {line_start}-{line_end} of {total_lines}{reason}. Use offset={} to continue.]",
        line_end.saturating_add(1)
    )
}

/// Remove the runtime continuation notice before adapter-level fitting.
///
/// # Parameters
/// - `output`: Runtime-formatted Read output.
///
/// # Returns
/// The complete-row body without a previously generated continuation suffix.
fn strip_continuation_notice(output: &str) -> &str {
    output
        .rfind("\n\n[Showing lines ")
        .map_or(output, |index| &output[..index])
}

/// Identify one complete hashline row without inspecting its source content.
///
/// # Parameters
/// - `row`: Candidate rendered output line.
///
/// # Returns
/// `true` only for a decimal line number followed by a hash and colon.
fn is_hashline_row(row: &str) -> bool {
    let Some(hash) = row.find('#') else {
        return false;
    };
    let Some(colon) = row[hash + 1..].find(':') else {
        return false;
    };
    let number = row[..hash].trim();
    !number.is_empty() && number.parse::<usize>().is_ok() && colon > 0
}

/// Truncate a string only at a valid UTF-8 boundary.
///
/// # Parameters
/// - `value`: Candidate string to bound.
/// - `max_bytes`: Maximum byte length for the returned prefix.
///
/// # Returns
/// A UTF-8-safe prefix no longer than `max_bytes` bytes.
fn truncate_utf8_prefix(value: &str, max_bytes: usize) -> String {
    let mut end = value.len().min(max_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
/// Limit one legacy numbered Read line to the historical character budget.
///
/// # Parameters
/// - `line`: Complete source line from the normalized Read content.
///
/// # Returns
/// The original line or a 2,000-character prefix with the legacy notice.
fn legacy_line(line: &str) -> String {
    const LEGACY_LINE_LIMIT: usize = 2000;
    for (character_count, (index, _)) in line.char_indices().enumerate() {
        if character_count == LEGACY_LINE_LIMIT {
            return format!("{}... (line truncated to 2000 chars)", &line[..index]);
        }
    }
    line.to_owned()
}

/// Build the legacy numbered content retained in the public content field.
///
/// # Parameters
/// - `result`: Native Read facts containing the selected source lines.
/// - `count`: Number of complete lines represented by the fitted output.
///
/// # Returns
/// Numbered-display-independent content with historical per-line truncation.
fn legacy_content(result: &ReadResult, count: usize) -> String {
    result
        .content
        .split('\n')
        .take(count)
        .map(legacy_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build numbered legacy rows for the old sole-`filePath` wrapper.
///
/// # Parameters
/// - `result`: Native Read facts containing the selected source lines.
/// - `count`: Number of complete lines represented by the fitted output.
///
/// # Returns
/// Numbered rows with complete UTF-8 boundaries and bounded line content.
fn legacy_numbered(result: &ReadResult, count: usize) -> String {
    result
        .content
        .split('\n')
        .take(count)
        .enumerate()
        .map(|(index, line)| format!("{}: {}", result.line_start + index, legacy_line(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render one fitted file body with the requested wire wrapper.
///
/// # Parameters
/// - `result`: Native Read facts used for path, line, and legacy footer data.
/// - `rows`: Complete rendered rows available for selection.
/// - `count`: Number of rows to retain from the front of `rows`.
/// - `parts`: Bounded diagnostic, continuation, and warning rendering data.
/// - `raw`: Whether to omit the XML file wrapper.
/// - `legacy_display`: Whether to include the numbered compatibility display.
///
/// # Returns
/// One output string whose row boundaries are preserved.
fn render_file_output(
    result: &ReadResult,
    rows: &[&str],
    count: usize,
    parts: RenderParts<'_>,
    raw: bool,
    legacy_display: bool,
) -> String {
    if raw {
        let body = if count == 0 {
            parts.diagnostic.to_owned()
        } else {
            rows[..count].join("\n")
        };
        let mut output = body;
        if !parts.continuation.is_empty() {
            output.push_str("\n\n");
            output.push_str(parts.continuation);
        }
        output.push_str(parts.warnings);
        return output;
    }

    if legacy_display {
        let line_end = if count == 0 {
            result.line_end
        } else {
            result.line_start + count - 1
        };
        let numbered = legacy_numbered(result, count);
        let body = if count == 0 {
            parts.diagnostic.to_owned()
        } else {
            numbered
        };
        let showing = result.truncated || count < rows.len();
        let footer = if result.first_line_exceeds_limit || (showing && parts.byte_limited) {
            format!(
                "(Output capped at 50 KB. Showing lines {}-{line_end}. Use offset={} to continue.)",
                result.line_start,
                line_end.saturating_add(1)
            )
        } else if showing {
            format!(
                "(Showing lines {}-{line_end} of {}. Use offset={} to continue.)",
                result.line_start,
                result.total_lines,
                line_end.saturating_add(1)
            )
        } else {
            format!("(End of file - total {} lines)", result.total_lines)
        };
        let mut output = format!(
            "<path>{}</path>\n<type>file</type>\n<content>\n{}\n\n{}\n</content>",
            display_path(&result.path),
            body,
            footer
        );
        output.push_str(parts.warnings);
        return output;
    }

    let body = if count == 0 {
        parts.diagnostic.to_owned()
    } else {
        rows[..count].join("\n")
    };
    let mut output = format!(
        "<path>{}</path>\n<type>file</type>\n<content>\n{}",
        display_path(&result.path),
        body
    );
    if !parts.continuation.is_empty() {
        output.push_str("\n\n");
        output.push_str(parts.continuation);
    }
    output.push_str("\n</content>");
    output.push_str(parts.warnings);
    output
}

/// Fit an oversized raw first line while retaining a valid UTF-8 prefix.
///
/// # Parameters
/// - `result`: Native Read facts for line and continuation metadata.
/// - `row`: First raw line that cannot fit with required notices.
/// - `warnings`: Bounded warning block reserved by the fitting pass.
///
/// # Returns
/// A truncated raw result with an explicit continuation notice and no partial
/// UTF-8 code point.
fn fit_raw_prefix(result: &ReadResult, row: &str, warnings: &str) -> FittedRead {
    let notice = continuation_notice(
        result.line_start,
        result.line_start,
        result.total_lines,
        true,
    );
    let available = MAX_READ_BYTES
        .saturating_sub(warnings.len())
        .saturating_sub(notice.len())
        .saturating_sub(2);
    let content = truncate_utf8_prefix(row, available);
    let mut output = content.clone();
    if !content.is_empty() {
        output.push_str("\n\n");
    }
    output.push_str(&notice);
    output.push_str(warnings);
    FittedRead {
        output,
        content,
        line_end: result.line_start,
        next_offset: Some(result.line_start.saturating_add(1)),
        truncated: true,
    }
}

/// Bounded output, content, and synchronized line metadata.
fn fit_file_output(result: &ReadResult, raw: bool, legacy_display: bool) -> FittedRead {
    let body = strip_continuation_notice(&result.output);
    let rows = if result.total_lines == 0 {
        Vec::new()
    } else if raw {
        body.split('\n').collect::<Vec<_>>()
    } else {
        body.split('\n')
            .filter(|row| is_hashline_row(row))
            .collect::<Vec<_>>()
    };
    let warnings = warning_notice(&result.warnings);

    if raw && !rows.is_empty() {
        let requires_notice = result.truncated
            || rows[0].len() > MAX_READ_BYTES
            || rows[0].len().saturating_add(warnings.len()) > MAX_READ_BYTES;
        if requires_notice {
            let notice = continuation_notice(
                result.line_start,
                result.line_start,
                result.total_lines,
                true,
            );
            let available = MAX_READ_BYTES
                .saturating_sub(warnings.len())
                .saturating_sub(notice.len())
                .saturating_sub(2);
            if rows[0].len() > available {
                return fit_raw_prefix(result, rows[0], &warnings);
            }
        }
    }
    let diagnostic = if rows.is_empty() {
        body.to_owned()
    } else {
        format!(
            "[Line {} cannot fit within {} bytes after Read wrappers. Hashline output requires full lines; no content was returned.]",
            result.line_start, MAX_READ_BYTES
        )
    };
    let mut count = rows.len();
    let mut byte_limited = result.output.contains("byte limit");
    loop {
        let removed_rows = count < rows.len();
        let truncated = result.truncated || removed_rows;
        let line_end = if count == 0 {
            if rows.is_empty() {
                result.line_end
            } else {
                result.line_start
            }
        } else {
            result.line_start + count - 1
        };
        let continuation =
            if !legacy_display && truncated && !result.first_line_exceeds_limit && count > 0 {
                continuation_notice(
                    result.line_start,
                    line_end,
                    result.total_lines,
                    byte_limited || removed_rows,
                )
            } else {
                String::new()
            };
        let output = render_file_output(
            result,
            &rows,
            count,
            RenderParts {
                diagnostic: &diagnostic,
                continuation: &continuation,
                warnings: &warnings,
                byte_limited: byte_limited || removed_rows,
            },
            raw,
            legacy_display,
        );
        if output.len() <= MAX_READ_BYTES {
            let content = if count == 0 {
                String::new()
            } else if legacy_display {
                legacy_content(result, count)
            } else {
                result
                    .content
                    .split('\n')
                    .take(count)
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let next_offset = if result.first_line_exceeds_limit {
                result.next_offset
            } else if count == 0 && removed_rows {
                None
            } else if truncated {
                Some(line_end.saturating_add(1))
            } else {
                result.next_offset
            };
            return FittedRead {
                output,
                content,
                line_end,
                next_offset,
                truncated,
            };
        }
        if count == 0 {
            break;
        }
        count -= 1;
        byte_limited = true;
    }

    let diagnostic = format!(
        "[Read output exceeded {} bytes; no complete row fits with its required wrapper.]",
        MAX_READ_BYTES
    );
    let output = render_file_output(
        result,
        &[],
        0,
        RenderParts {
            diagnostic: &diagnostic,
            continuation: "",
            warnings: &warnings,
            byte_limited: true,
        },
        raw,
        legacy_display,
    );
    FittedRead {
        output,
        content: String::new(),
        line_end: result.line_start,
        next_offset: None,
        truncated: true,
    }
}

/// Build the bounded file result envelope from fitted native Read facts.
///
/// # Parameters
/// - `result`: Native Read facts and model output.
/// - `workdir`: Normalized working directory for the relative title.
/// - `raw`: Whether model output must remain unanchored.
/// - `legacy_display`: Whether the sole legacy path spelling requested the old wrapper.
///
/// # Returns
/// A bounded JSON file result with synchronized content and display metadata.
fn file_value(result: &ReadResult, workdir: &Path, raw: bool, legacy_display: bool) -> Value {
    let fitted = fit_file_output(result, raw, legacy_display);
    let content = fitted.content;
    let warnings = result.warnings.clone();
    let truncated = fitted.truncated;
    json!({
        "title": relative_title(&result.path, workdir),
        "output": fitted.output,
        "content": content.clone(),
        "metadata": {
            "preview": content.lines().take(20).collect::<Vec<_>>().join("\n"),
            "truncated": truncated,
            "loaded": [],
            "nextOffset": fitted.next_offset,
            "warnings": warnings,
            "display": {
                "type": "file",
                "path": display_path(&result.path),
                "text": content,
                "lineStart": result.line_start,
                "lineEnd": fitted.line_end,
                "totalLines": result.total_lines,
                "truncated": truncated,
            },
        },
    })
}

/// Fit one sorted directory page while keeping every public view identical.
///
/// # Parameters
/// - `path`: Directory path used by the model-facing wrapper.
/// - `entries`: Complete sorted directory entry list.
/// - `offset`: One-based requested entry offset.
/// - `limit`: Maximum requested entries for this page.
///
/// # Returns
/// A bounded page whose output, content, display entries, truncation flag, and
/// continuation offset all describe the same represented entries.
fn fit_directory_page(
    path: &Path,
    entries: &[String],
    offset: usize,
    limit: usize,
) -> FittedDirectory {
    let start = offset.saturating_sub(1);
    let mut represented = Vec::new();
    for entry in entries.iter().skip(start).take(limit) {
        represented.push(entry.clone());
        let truncated = start.saturating_add(represented.len()) < entries.len();
        let content = represented.join("\n");
        let output = render_directory_output(path, &represented, entries.len(), offset, truncated);
        if !directory_views_fit(&output, &content, &represented) {
            represented.pop();
            break;
        }
    }

    let truncated = start.saturating_add(represented.len()) < entries.len();
    let output = render_directory_output(path, &represented, entries.len(), offset, truncated);
    let next_offset = truncated.then_some(offset.saturating_add(represented.len()));
    FittedDirectory {
        output,
        entries: represented,
        truncated,
        next_offset,
    }
}

/// Render the bounded directory wrapper and footer for one page.
///
/// # Parameters
/// - `path`: Directory path shown in the wrapper.
/// - `entries`: Entries represented in this page.
/// - `total`: Total sorted entries in the directory.
/// - `offset`: One-based requested offset.
/// - `truncated`: Whether another page remains.
///
/// # Returns
/// The model-facing directory output for exactly `entries`.
fn render_directory_output(
    path: &Path,
    entries: &[String],
    total: usize,
    offset: usize,
    truncated: bool,
) -> String {
    [
        format!("<path>{}</path>", display_path(path)),
        "<type>directory</type>".to_string(),
        "<entries>".to_string(),
        entries.join("\n"),
        directory_footer(entries.len(), total, offset, truncated),
        "</entries>".to_string(),
    ]
    .join("\n")
}

/// Check all serialized views against the Read byte budget.
///
/// # Parameters
/// - `output`: Model-facing directory wrapper.
/// - `content`: Plain content representation of the same entries.
/// - `entries`: Structured display entry representation.
///
/// # Returns
/// `true` when all three views fit their downstream coding caps.
fn directory_views_fit(output: &str, content: &str, entries: &[String]) -> bool {
    serde_json::to_vec(output).is_ok_and(|serialized| serialized.len() <= MAX_READ_BYTES)
        && serde_json::to_vec(content).is_ok_and(|serialized| serialized.len() <= MAX_READ_BYTES)
        && serde_json::to_vec(entries).is_ok_and(|serialized| serialized.len() <= MAX_READ_BYTES)
}

/// Read and format a directory using the existing sorted-entry contract.
///
/// # Parameters
/// - `path`: Absolute directory path.
/// - `workdir`: Normalized working directory for the title.
/// - `input`: Read controls for offset and limit.
/// - `cancel`: Call cancellation token checked around directory I/O.
///
/// # Returns
/// A directory result with bounded display output and entry metadata.
async fn read_directory(
    path: &Path,
    workdir: &Path,
    input: &ReadInput,
    cancel: &CancellationToken,
) -> Result<Value, ToolError> {
    check_token(cancel)?;
    let mut entries = Vec::new();
    let directory_result = tokio::fs::read_dir(path).await;
    check_token(cancel)?;
    let mut dir = directory_result?;
    loop {
        check_token(cancel)?;
        let entry_result = dir.next_entry().await;
        check_token(cancel)?;
        let Some(entry) = entry_result? else {
            break;
        };
        check_token(cancel)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        check_token(cancel)?;
        let file_type_result = entry.file_type().await;
        check_token(cancel)?;
        let file_type = file_type_result?;
        if file_type.is_dir() {
            entries.push(format!("{name}/"));
        } else {
            entries.push(name);
        }
    }
    entries.sort_by(
        |left, right| match (left.ends_with('/'), right.ends_with('/')) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => left.cmp(right),
        },
    );

    let offset = read_offset(input);
    let limit = input.limit.unwrap_or(DEFAULT_READ_LIMIT);
    let fitted = fit_directory_page(path, &entries, offset, limit);
    let content = fitted.entries.join("\n");
    let preview = fitted
        .entries
        .iter()
        .take(20)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let title = relative_title(path, workdir);

    Ok(json!({
        "title": title,
        "output": fitted.output,
        "content": content,
        "metadata": {
            "preview": preview,
            "truncated": fitted.truncated,
            "loaded": [],
            "nextOffset": fitted.next_offset,
            "display": {
                "type": "directory",
                "path": display_path(path),
                "entries": fitted.entries,
                "offset": offset,
                "totalEntries": entries.len(),
                "truncated": fitted.truncated,
            },
        },
    }))
}

/// Return a bounded missing-file diagnostic with at most three nearby names.
///
/// # Parameters
/// - `path`: Absolute path that was not found.
/// - `cancel`: Call cancellation token checked around suggestion I/O.
///
/// # Returns
/// A typed tool error describing the missing target without file contents.
async fn missing_file(path: &Path, cancel: &CancellationToken) -> Result<Value, ToolError> {
    check_token(cancel)?;
    let suggestions = similar_paths(path, cancel).await?;
    check_token(cancel)?;
    if suggestions.is_empty() {
        return Err(ToolError::Other(format!(
            "File not found: {}",
            display_path(path)
        )));
    }
    Err(ToolError::Other(format!(
        "File not found: {}\n\nDid you mean one of these?\n{}",
        display_path(path),
        suggestions.join("\n")
    )))
}

/// Find up to three nearby path names for a missing target.
///
/// # Parameters
/// - `path`: Absolute missing path whose parent is searched.
/// - `cancel`: Call cancellation token checked around suggestion I/O.
///
/// # Returns
/// Sorted content-free path suggestions, or cancellation when requested.
async fn similar_paths(path: &Path, cancel: &CancellationToken) -> Result<Vec<String>, ToolError> {
    let Some(parent) = path.parent() else {
        return Ok(Vec::new());
    };
    let base = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut suggestions = Vec::new();
    check_token(cancel)?;
    let directory_result = tokio::fs::read_dir(parent).await;
    check_token(cancel)?;
    let Ok(mut entries) = directory_result else {
        return Ok(suggestions);
    };
    check_token(cancel)?;
    loop {
        check_token(cancel)?;
        let entry_result = entries.next_entry().await;
        check_token(cancel)?;
        let entry = match entry_result {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(_) => break,
        };
        check_token(cancel)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let name_lower = name.to_ascii_lowercase();
        if name_lower.contains(&base) || base.contains(&name_lower) {
            suggestions.push(display_path(&parent.join(name)));
        }
    }
    suggestions.sort();
    suggestions.truncate(3);
    Ok(suggestions)
}

/// Require external-directory permission before reading an outside path.
///
/// # Parameters
/// - `ctx`: Tool context carrying the permission plane.
/// - `workdir`: Normalized session working directory.
/// - `path`: Lexically resolved requested path.
///
/// The permission resource is based on the lexical parent for both files and directories.
///
/// # Returns
/// Success when the path is inside the workdir or external access is allowed.
async fn assert_external_path(ctx: &ToolCtx, workdir: &Path, path: &Path) -> Result<(), ToolError> {
    if path.starts_with(workdir) {
        return Ok(());
    }
    let directory = path
        .parent()
        .map_or_else(|| Path::new("/").to_path_buf(), Path::to_path_buf);
    ctx.permission
        .assert(
            Action::ExternalDirectory,
            Resource::Path(display_path(&directory.join("*"))),
        )
        .await?;
    Ok(())
}

/// Normalize the hidden zero-offset compatibility spelling to line one.
///
/// # Parameters
/// - `input`: Parsed Read arguments.
///
/// # Returns
/// A positive one-based line offset.
fn read_offset(input: &ReadInput) -> usize {
    input.offset.unwrap_or(1).max(1)
}

/// Compute a relative title, falling back to an absolute display path.
///
/// # Parameters
/// - `path`: Display path.
/// - `workdir`: Normalized working directory.
///
/// # Returns
/// A slash-normalized relative title when possible.
fn relative_title(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .map_or_else(|_| display_path(path), display_path)
}

/// Render the existing directory footer for offset paging.
///
/// # Parameters
/// - `shown`: Number of entries in this result.
/// - `total`: Total entries in the directory.
/// - `offset`: One-based requested offset.
/// - `truncated`: Whether more entries remain.
///
/// # Returns
/// A bounded continuation or end-of-directory message.
fn directory_footer(shown: usize, total: usize, offset: usize, truncated: bool) -> String {
    if truncated {
        format!(
            "\n(Showing {shown} of {total} entries. Use 'offset' parameter to read beyond entry {})",
            offset.saturating_add(shown)
        )
    } else {
        format!("\n({total} entries)")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn long_directory_page_keeps_output_and_page_metadata_in_sync() {
        let entries = (0..5000)
            .map(|index| format!("entry-{index:04}"))
            .collect::<Vec<_>>();
        let fitted = fit_directory_page(Path::new("/workspace"), &entries, 1, entries.len());
        let content = fitted.entries.join("\n");

        assert!(fitted.output.len() <= MAX_READ_BYTES);
        assert!(fitted.entries.len() < entries.len());
        assert!(fitted.truncated);
        assert_eq!(fitted.next_offset, Some(fitted.entries.len() + 1));
        assert_eq!(
            content.split('\n').collect::<Vec<_>>(),
            fitted
                .entries
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        for entry in &fitted.entries {
            assert!(fitted.output.contains(entry));
        }

        let next = fit_directory_page(
            Path::new("/workspace"),
            &entries,
            fitted.next_offset.unwrap_or(1),
            entries.len(),
        );
        assert_eq!(next.entries.first(), entries.get(fitted.entries.len()));
    }

    #[test]
    fn escape_heavy_directory_page_fits_every_serialized_view() {
        let path = format!("/{}", "\"".repeat(4000));
        let entries = (0..200)
            .map(|index| format!("entry-{index:04}-{}", "x".repeat(220)))
            .collect::<Vec<_>>();
        let fitted = fit_directory_page(Path::new(&path), &entries, 1, entries.len());
        let content = fitted.entries.join("\n");

        assert!(serde_json::to_vec(&fitted.output).unwrap().len() <= MAX_READ_BYTES);
        assert!(serde_json::to_vec(&content).unwrap().len() <= MAX_READ_BYTES);
        assert!(serde_json::to_vec(&fitted.entries).unwrap().len() <= MAX_READ_BYTES);
        assert_eq!(fitted.next_offset, Some(fitted.entries.len() + 1));
    }
}
