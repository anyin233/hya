//! Global tool-output size guard.
//!
//! Ordinary results retain the historical display-text cap. Coding tools use a
//! separate allowlisted envelope cap so titles, model output, and presentation
//! metadata survive the trip through the durable event log without permitting
//! hostile fields to grow without bound.

use crate::tool::ToolResultPolicy;
use serde_json::{Map, Value, json};

/// Maximum characters of ordinary tool output kept for the model (last N chars).
pub const MAX_TOOL_OUTPUT_CHARS: usize = 5000;

/// Maximum raw UTF-8 bytes retained in a coding tool's model-facing output.
pub const MAX_CODING_OUTPUT_BYTES: usize = 50 * 1024;
/// Maximum raw UTF-8 bytes retained in a coding tool title.
pub const MAX_CODING_TITLE_BYTES: usize = 512;
/// Maximum raw UTF-8 bytes retained in one coding display text field.
pub const MAX_CODING_DISPLAY_BYTES: usize = 50 * 1024;
/// Maximum raw UTF-8 bytes retained in one Edit diff or patch field.
pub const MAX_CODING_DIFF_BYTES: usize = 50 * 1024;
/// Maximum serialized bytes retained by one coding attachment or its container.
pub const MAX_CODING_ATTACHMENT_BYTES: usize = 50 * 1024;
/// Maximum serialized bytes retained for an artifact path that must stay atomic.
pub(crate) const MAX_CODING_OUTPUT_PATH_BYTES: usize = 32 * 1024;
/// Maximum attachment entries retained after validation.
const MAX_CODING_ATTACHMENT_ENTRIES: usize = 16;
/// Maximum serialized bytes retained for one path-keyed diagnostic identity.
const MAX_CODING_DIAGNOSTIC_PATH_BYTES: usize = 4 * 1024;
/// Maximum warning or diagnostic rows retained in one metadata field.
pub const MAX_CODING_DIAGNOSTIC_ROWS: usize = 16;
/// Maximum raw UTF-8 bytes retained in one warning or diagnostic row.
pub const MAX_CODING_DIAGNOSTIC_ROW_BYTES: usize = 2 * 1024;
/// Maximum Grep display groups retained in one result.
pub const MAX_CODING_GREP_GROUPS: usize = 100;
/// Maximum Grep display rows retained across all groups.
pub const MAX_CODING_GREP_ROWS: usize = 2000;
/// Maximum serialized bytes retained by Grep display rows and groups.
pub const MAX_CODING_GREP_DISPLAY_BYTES: usize = 50 * 1024;
/// Maximum serialized JSON bytes retained by a coding result envelope.
pub const MAX_CODING_ENVELOPE_BYTES: usize = 256 * 1024;

const MAX_GENERIC_METADATA_ITEMS: usize = 128;
const MAX_METADATA_KEY_BYTES: usize = 128;
const MAX_SMALL_METADATA_STRING_BYTES: usize = 2 * 1024;

/// Facts reported in the retained metadata when a safety bound removed data.
#[derive(Default)]
struct TruncationFacts {
    output: bool,
    title: bool,
    display: bool,
    attachments: bool,
    diff: bool,
    diagnostics: bool,
    warnings: bool,
    rows: bool,
    groups: bool,
    metadata: bool,
    unknown: bool,
    envelope: bool,
}

impl TruncationFacts {
    /// Return whether any field was removed or shortened.
    fn any(&self) -> bool {
        self.output
            || self.title
            || self.display
            || self.attachments
            || self.diff
            || self.diagnostics
            || self.warnings
            || self.rows
            || self.groups
            || self.metadata
            || self.unknown
            || self.envelope
    }

    /// Mark a dropped or atomically bounded attachment and its display payload.
    fn mark_attachment(&mut self) {
        self.attachments = true;
        self.display = true;
    }

    /// Mark the category associated with a recursively bounded value.
    fn mark(&mut self, kind: TruncationKind) {
        match kind {
            TruncationKind::Output => self.output = true,
            TruncationKind::Title => self.title = true,
            TruncationKind::Display => self.display = true,
            TruncationKind::Diff => self.diff = true,
            TruncationKind::Diagnostics => self.diagnostics = true,
            TruncationKind::Warnings => self.warnings = true,
            TruncationKind::Metadata => self.metadata = true,
        }
    }

    /// Merge truncation facts collected while bounding one nested value.
    fn merge(&mut self, other: &Self) {
        self.output |= other.output;
        self.title |= other.title;
        self.display |= other.display;
        self.attachments |= other.attachments;
        self.diff |= other.diff;
        self.diagnostics |= other.diagnostics;
        self.warnings |= other.warnings;
        self.rows |= other.rows;
        self.groups |= other.groups;
        self.metadata |= other.metadata;
        self.unknown |= other.unknown;
        self.envelope |= other.envelope;
    }
}

#[derive(Clone, Copy)]
enum TruncationKind {
    Output,
    Title,
    Display,
    Diff,
    Diagnostics,
    Warnings,
    Metadata,
}

/// Cap a successful tool `output` value using the historical arbitrary-value policy.
///
/// Under the limit the original [`Value`] is returned unchanged (shape preserved).
/// Over the limit the result becomes a string notice plus the **last**
/// [`MAX_TOOL_OUTPUT_CHARS`] characters of the display text.
#[must_use]
pub fn cap_tool_output(output: Value) -> Value {
    let text = value_as_display_text(&output);
    let n = text.chars().count();
    if n <= MAX_TOOL_OUTPUT_CHARS {
        return output;
    }
    let kept = last_n_chars(&text, MAX_TOOL_OUTPUT_CHARS);
    Value::String(format!(
        "[tool output truncated: original {n} chars; showing last {MAX_TOOL_OUTPUT_CHARS} chars]\n{kept}"
    ))
}

/// Cap a successful result according to the executing tool's policy.
///
/// Coding policies retain the approved `{title, output, metadata}` envelope
/// and bounded presentation fields. A non-object coding result is treated as
/// an ordinary result because it is not an approved presentation envelope.
#[must_use]
pub fn cap_tool_output_with_policy(output: Value, policy: ToolResultPolicy) -> Value {
    match policy {
        ToolResultPolicy::Default => cap_tool_output(output),
        ToolResultPolicy::Coding => cap_coding_result(output, false),
        ToolResultPolicy::CodingWithDiff => cap_coding_result(output, true),
    }
}

/// Bound one coding envelope while retaining semantic fields needed by the TUI.
fn cap_coding_result(output: Value, include_diff: bool) -> Value {
    let Value::Object(mut fields) = output else {
        return cap_malformed_coding_result(output);
    };

    let mut facts = TruncationFacts::default();
    let title = bound_title(
        fields
            .remove("title")
            .unwrap_or_else(|| Value::String(String::new())),
        &mut facts,
    );
    let model_output = bound_model_output(
        fields
            .remove("output")
            .unwrap_or_else(|| Value::String(String::new())),
        &mut facts,
    );
    let metadata = fields
        .remove("metadata")
        .unwrap_or_else(|| Value::Object(Map::new()));
    let metadata = bound_metadata(metadata, include_diff, &mut facts);

    let mut envelope = Map::new();
    envelope.insert("title".to_string(), title);
    envelope.insert("output".to_string(), model_output);
    for (key, value) in fields {
        if is_allowed_top_level_key(&key) {
            envelope.insert(key.clone(), bound_top_level(&key, value, &mut facts));
        } else {
            facts.unknown = true;
            facts.metadata = true;
        }
    }
    envelope.insert("metadata".to_string(), metadata);
    synchronize_read_views(&mut envelope, &mut facts);
    if let Some(metadata) = envelope.get_mut("metadata") {
        add_truncation_facts(metadata, &facts);
    }
    enforce_envelope_bound(&mut envelope, &mut facts);
    Value::Object(envelope)
}
/// Synchronize Read content, display metadata, offsets, and wrapper notices.
fn synchronize_read_views(envelope: &mut Map<String, Value>, facts: &mut TruncationFacts) {
    let Some(Value::String(content)) = envelope.get("content") else {
        return;
    };
    let content = content.clone();
    let Some(Value::Object(metadata)) = envelope.get("metadata") else {
        return;
    };
    if !metadata.contains_key("nextOffset") {
        return;
    }
    let Some(Value::Object(display)) = metadata.get("display") else {
        return;
    };
    if display.get("type").and_then(Value::as_str) != Some("file") {
        return;
    }
    let line_start = display
        .get("lineStart")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1);
    let total_lines = display
        .get("totalLines")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
    let content_count = visible_content_lines(&content);
    let content_end = if content_count == 0 {
        line_start.saturating_sub(1)
    } else {
        line_start.saturating_add(content_count).saturating_sub(1)
    };
    let output = envelope
        .get("output")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let output_end = output.as_deref().and_then(last_read_row_line);
    let mut line_end = output_end.map_or(content_end, |end| content_end.min(end));
    let initial_keep_count = if line_end >= line_start {
        line_end.saturating_sub(line_start).saturating_add(1)
    } else {
        0
    };
    let initial_content_changed = initial_keep_count < content_count;
    let output_rows_mismatch = output_end.is_some_and(|end| end != content_end);
    let had_next_offset = metadata.get("nextOffset").and_then(Value::as_u64).is_some();
    let retained_continuation = output.as_deref().is_some_and(|text| {
        [
            "\n\n[Showing lines ",
            "\n\n(Showing lines ",
            "\n\n(Output capped at ",
        ]
        .iter()
        .any(|marker| text.contains(marker))
    });
    let raw_needs_notice = output_end.is_none()
        && output
            .as_deref()
            .is_some_and(|text| !text.contains("<content>\n"))
        && total_lines > line_end
        && (!had_next_offset || !retained_continuation);
    if output_rows_mismatch || raw_needs_notice {
        facts.output = true;
        facts.display = true;
    }
    let rewritten_output = if initial_content_changed || output_rows_mismatch {
        output.and_then(|output| {
            rewrite_read_continuation(&output, line_start, line_end, total_lines).map(
                |(rewritten, retained_end)| {
                    line_end = retained_end;
                    rewritten
                },
            )
        })
    } else if raw_needs_notice {
        rewrite_raw_read_continuation(&content, line_start, line_end, total_lines).map(
            |(rewritten, retained_end)| {
                line_end = retained_end;
                rewritten
            },
        )
    } else {
        None
    };
    let keep_count = if line_end >= line_start {
        line_end.saturating_sub(line_start).saturating_add(1)
    } else {
        0
    };
    let synchronized_content = take_complete_content_lines(&content, keep_count);
    let content_changed = synchronized_content != content;
    let existing_truncated = display
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let truncated = existing_truncated
        || content_changed
        || output_rows_mismatch
        || (total_lines > 0 && line_end < total_lines);
    let next_offset = (truncated && total_lines > line_end).then(|| line_end.saturating_add(1));

    if content_changed {
        envelope.insert(
            "content".to_string(),
            Value::String(synchronized_content.clone()),
        );
        facts.display = true;
    }
    if let Some(Value::Object(metadata)) = envelope.get_mut("metadata") {
        metadata.insert(
            "nextOffset".to_string(),
            next_offset.map_or(Value::Null, |offset| json!(offset)),
        );
        if let Some(Value::Object(display)) = metadata.get_mut("display") {
            display.insert(
                "text".to_string(),
                Value::String(synchronized_content.clone()),
            );
            display.insert("lineEnd".to_string(), json!(line_end));
            if truncated {
                display.insert("truncated".to_string(), Value::Bool(true));
            }
        }
        if truncated {
            metadata.insert("truncated".to_string(), Value::Bool(true));
        }
    }
    if envelope.contains_key("nextOffset") {
        envelope.insert(
            "nextOffset".to_string(),
            next_offset.map_or(Value::Null, |offset| json!(offset)),
        );
    }
    if let Some(output) = rewritten_output {
        envelope.insert("output".to_string(), Value::String(output));
    }
}

/// Count non-sentinel content lines in a normalized coding preview.
fn visible_content_lines(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content
            .split('\n')
            .count()
            .saturating_sub(usize::from(content.ends_with('\n')))
    }
}

/// Keep a prefix of complete content lines without a trailing delimiter.
fn take_complete_content_lines(content: &str, count: usize) -> String {
    if count >= visible_content_lines(content) {
        return content.to_owned();
    }
    content
        .split('\n')
        .take(count)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Return the last complete numbered row in a canonical or legacy Read wrapper.
fn last_read_row_line(output: &str) -> Option<usize> {
    const OPEN: &str = "<content>\n";
    const CLOSE: &str = "\n</content>";
    let body_start = output.find(OPEN)?.saturating_add(OPEN.len());
    let close_start = output[body_start..].find(CLOSE)?.saturating_add(body_start);
    output[body_start..close_start]
        .lines()
        .filter_map(|row| {
            if let Some(hash) = row.find('#')
                && row[hash + 1..].find(':').is_some_and(|colon| colon > 0)
                && let Ok(line) = row[..hash].trim().parse::<usize>()
            {
                return Some(line);
            }
            let (line, _) = row.split_once(':')?;
            line.trim().parse::<usize>().ok()
        })
        .next_back()
}

/// Rewrite a retained Read wrapper to a complete-row prefix that fits with its
/// synchronized continuation notice.
fn rewrite_read_continuation(
    output: &str,
    line_start: usize,
    line_end: usize,
    total_lines: usize,
) -> Option<(String, usize)> {
    const OPEN: &str = "<content>\n";
    const CLOSE: &str = "\n</content>";
    let body_start = output.find(OPEN)?.saturating_add(OPEN.len());
    let close_start = output[body_start..].find(CLOSE)?.saturating_add(body_start);
    let body = &output[body_start..close_start];
    let marker = [
        "\n\n[Showing lines ",
        "\n\n(Showing lines ",
        "\n\n(Output capped at ",
    ]
    .iter()
    .filter_map(|marker| body.rfind(marker).map(|index| (index, *marker)))
    .max_by_key(|(index, _)| *index);
    let inferred_legacy = body.lines().next().is_some_and(|row| {
        !row.contains('#')
            && row
                .split_once(':')
                .is_some_and(|(line, _)| line.trim().parse::<usize>().is_ok())
    });
    let (rows_end, legacy, byte_limited) = marker.map_or(
        (body.len(), inferred_legacy, true),
        |(marker_start, marker)| {
            (
                marker_start,
                marker.starts_with("\n\n("),
                marker.contains("Output capped") || body[marker_start + 2..].contains("byte limit"),
            )
        },
    );
    let rows = body[..rows_end].lines().collect::<Vec<_>>();
    let requested_count = if line_end >= line_start {
        line_end.saturating_sub(line_start).saturating_add(1)
    } else {
        0
    }
    .min(rows.len());

    for count in (0..=requested_count).rev() {
        let retained_end = if count == 0 {
            line_start.saturating_sub(1)
        } else {
            line_start.saturating_add(count).saturating_sub(1)
        };
        let mut rewritten = String::with_capacity(output.len());
        rewritten.push_str(&output[..body_start]);
        rewritten.push_str(&rows[..count].join("\n"));
        if retained_end < total_lines {
            if count > 0 {
                rewritten.push_str("\n\n");
            }
            let reason = if byte_limited {
                format!(" ({} byte limit)", MAX_CODING_OUTPUT_BYTES)
            } else {
                String::new()
            };
            let next_offset = retained_end.saturating_add(1);
            if legacy {
                rewritten.push_str(&format!(
                    "(Showing lines {line_start}-{retained_end} of {total_lines}{reason}. Use offset={next_offset} to continue.)"
                ));
            } else {
                rewritten.push_str(&format!(
                    "[Showing lines {line_start}-{retained_end} of {total_lines}{reason}. Use offset={next_offset} to continue.]"
                ));
            }
        }
        rewritten.push_str(&output[close_start..]);
        if serialized_string_len(&rewritten) <= MAX_CODING_OUTPUT_BYTES {
            return Some((rewritten, retained_end));
        }
    }

    None
}

/// Fit a capped raw Read prefix with a complete continuation notice.
fn rewrite_raw_read_continuation(
    content: &str,
    line_start: usize,
    line_end: usize,
    total_lines: usize,
) -> Option<(String, usize)> {
    let rows = content.split('\n').collect::<Vec<_>>();
    let requested_count = if line_end >= line_start {
        line_end.saturating_sub(line_start).saturating_add(1)
    } else {
        0
    }
    .min(visible_content_lines(content));
    for count in (0..=requested_count).rev() {
        let retained_end = if count == 0 {
            line_start.saturating_sub(1)
        } else {
            line_start.saturating_add(count).saturating_sub(1)
        };
        let next_offset = retained_end.saturating_add(1);
        let notice = format!(
            "[Showing lines {line_start}-{retained_end} of {total_lines} ({} byte limit). Use offset={next_offset} to continue.]",
            MAX_CODING_OUTPUT_BYTES
        );
        let mut rewritten = rows[..count].join("\n");
        if !rewritten.is_empty() {
            rewritten.push_str("\n\n");
        }
        rewritten.push_str(&notice);
        if serialized_string_len(&rewritten) <= MAX_CODING_OUTPUT_BYTES {
            return Some((rewritten, retained_end));
        }
    }
    None
}

/// Bound a malformed coding result without serializing its complete input first.
///
/// # Parameters
/// - `output`: Non-object JSON returned by a coding tool.
///
/// # Returns
/// The historical bounded string behavior for strings, or a recursively bounded
/// JSON value whose serialized size does not exceed the coding output budget.
fn cap_malformed_coding_result(output: Value) -> Value {
    match output {
        Value::String(text) => cap_tool_output(Value::String(text)),
        other => {
            let mut facts = TruncationFacts::default();
            let bounded = bound_json_value(
                other,
                MAX_CODING_OUTPUT_BYTES,
                8,
                MAX_GENERIC_METADATA_ITEMS,
                TruncationKind::Output,
                &mut facts,
            );
            if serialized_len(&bounded) <= MAX_CODING_OUTPUT_BYTES {
                bounded
            } else {
                Value::Null
            }
        }
    }
}

/// Bound the title while keeping its type stable for provider/TUI consumers.
fn bound_title(value: Value, facts: &mut TruncationFacts) -> Value {
    match value {
        Value::String(text) => Value::String(bound_string(
            text,
            MAX_CODING_TITLE_BYTES,
            MAX_CODING_TITLE_BYTES,
            TruncationKind::Title,
            facts,
        )),
        other => bound_json_value(
            other,
            MAX_CODING_TITLE_BYTES,
            4,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Title,
            facts,
        ),
    }
}

/// Bound the model-facing output, preserving a string whenever the adapter returned one.
fn bound_model_output(value: Value, facts: &mut TruncationFacts) -> Value {
    match value {
        Value::String(text) => {
            if serialized_string_len(&text) <= MAX_CODING_OUTPUT_BYTES {
                return Value::String(text);
            }
            if let Some(text) = bound_read_output(&text, MAX_CODING_OUTPUT_BYTES, facts) {
                Value::String(text)
            } else {
                Value::String(bound_line_string(
                    text,
                    MAX_CODING_OUTPUT_BYTES,
                    MAX_CODING_OUTPUT_BYTES,
                    TruncationKind::Output,
                    facts,
                ))
            }
        }
        other => bound_json_value(
            other,
            MAX_CODING_OUTPUT_BYTES,
            8,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Output,
            facts,
        ),
    }
}

/// Bound a Read file wrapper by complete rows while retaining its footer.
fn bound_read_output(
    text: &str,
    max_serialized_bytes: usize,
    facts: &mut TruncationFacts,
) -> Option<String> {
    const OPEN: &str = "<content>\n";
    const CLOSE: &str = "\n</content>";
    let body_start = text.find(OPEN)?.saturating_add(OPEN.len());
    let close_start = text[body_start..].find(CLOSE)?.saturating_add(body_start);
    let close_end = close_start.saturating_add(CLOSE.len());
    let prefix = &text[..body_start];
    let body = &text[body_start..close_start];
    let warning_suffix = &text[close_end..];
    let continuation_start = [
        "\n\n[Showing lines ",
        "\n\n(Showing lines ",
        "\n\n(Output capped at ",
    ]
    .iter()
    .filter_map(|marker| body.rfind(marker))
    .max();
    let (rows_text, continuation) = continuation_start.map_or((body, None), |start| {
        (&body[..start], Some(body[start + 2..].to_owned()))
    });
    let rows = if rows_text.is_empty() {
        Vec::new()
    } else {
        rows_text.split('\n').collect::<Vec<_>>()
    };

    let prefix_bytes = serialized_string_len(prefix).saturating_sub(2);
    let close_bytes = serialized_string_len(CLOSE).saturating_sub(2);
    let continuation_bytes = continuation.as_deref().map_or_else(
        || {
            let digits = usize::MAX.to_string().len();
            let number = "9".repeat(digits);
            let worst_case = format!(
                "\n\n[Showing lines {number}-{number} of {number} ({} byte limit). Use offset={number} to continue.]",
                max_serialized_bytes
            );
            serialized_string_len(&worst_case).saturating_sub(2)
        },
        |notice| {
            serialized_string_len("\n\n")
                .saturating_sub(2)
                .saturating_add(serialized_string_len(notice).saturating_sub(2))
        },
    );
    let warning_bytes = serialized_string_len(warning_suffix).saturating_sub(2);
    let fixed_without_warning = 2usize
        .saturating_add(prefix_bytes)
        .saturating_add(close_bytes)
        .saturating_add(continuation_bytes);
    if fixed_without_warning > max_serialized_bytes {
        return None;
    }
    let warning_budget = max_serialized_bytes.saturating_sub(fixed_without_warning);
    let warning = if warning_bytes > warning_budget {
        let mut warning = warning_suffix.to_owned();
        let mut warning_facts = TruncationFacts::default();
        warning = bound_string(
            warning,
            warning_budget,
            warning_budget,
            TruncationKind::Output,
            &mut warning_facts,
        );
        if warning_facts.any() {
            facts.mark(TruncationKind::Output);
        }
        warning
    } else {
        warning_suffix.to_owned()
    };
    let warning_bytes = serialized_string_len(&warning).saturating_sub(2);
    let mut used = 2usize
        .saturating_add(prefix_bytes)
        .saturating_add(close_bytes)
        .saturating_add(continuation_bytes)
        .saturating_add(warning_bytes);
    let mut count = 0usize;
    for row in &rows {
        let separator = usize::from(count > 0);
        let row_bytes = serialized_string_len(row).saturating_sub(2);
        let required = separator
            .saturating_mul(serialized_string_len("\n").saturating_sub(2))
            .saturating_add(row_bytes);
        if used.saturating_add(required) > max_serialized_bytes {
            break;
        }
        used = used.saturating_add(required);
        count += 1;
    }
    let mut output = String::with_capacity(text.len().min(max_serialized_bytes));
    output.push_str(prefix);
    if count > 0 {
        output.push_str(&rows[..count].join("\n"));
    }
    if let Some(notice) = continuation {
        output.push_str("\n\n");
        output.push_str(&notice);
    }
    output.push_str(CLOSE);
    output.push_str(&warning);
    if count < rows.len() || output != text {
        facts.mark(TruncationKind::Output);
    }
    Some(output)
}

/// Bound a string to complete newline-delimited rows where possible.
fn bound_line_string(
    text: String,
    max_raw_bytes: usize,
    max_serialized_bytes: usize,
    kind: TruncationKind,
    facts: &mut TruncationFacts,
) -> String {
    let original_len = text.len();
    let has_newline = text.contains('\n');
    let mut bounded = bound_string(text, max_raw_bytes, max_serialized_bytes, kind, facts);
    if bounded.len() < original_len {
        if let Some(end) = bounded.rfind('\n') {
            bounded.truncate(end);
        } else if has_newline {
            // A first row that cannot fit is omitted rather than split.
            bounded.clear();
        }
        facts.mark(kind);
    }
    bounded
}

/// Return whether a top-level coding field has a known presentation meaning.
fn is_allowed_top_level_key(key: &str) -> bool {
    matches!(
        key,
        "content"
            | "stdout"
            | "stderr"
            | "exit_code"
            | "exit"
            | "ok"
            | "bytes"
            | "created"
            | "replaced"
            | "paths"
            | "matches"
            | "total"
            | "files"
            | "attachments"
            | "command"
            | "status"
            | "timed_out"
            | "timeout"
    )
}

/// Bound an allowlisted top-level semantic field without retaining arbitrary keys.
fn bound_top_level(key: &str, value: Value, facts: &mut TruncationFacts) -> Value {
    match key {
        "content" | "stdout" | "stderr" => bound_display_value(value, facts),
        "attachments" => bound_attachments(value, facts),
        "paths" | "matches" | "files" => bound_json_value(
            value,
            MAX_CODING_GREP_DISPLAY_BYTES,
            8,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Display,
            facts,
        ),
        "command" => match value {
            Value::String(text) => Value::String(bound_string(
                text,
                MAX_CODING_DISPLAY_BYTES,
                MAX_CODING_DISPLAY_BYTES,
                TruncationKind::Display,
                facts,
            )),
            other => bound_json_value(
                other,
                MAX_CODING_DISPLAY_BYTES,
                4,
                MAX_GENERIC_METADATA_ITEMS,
                TruncationKind::Display,
                facts,
            ),
        },
        _ => bound_json_value(
            value,
            MAX_SMALL_METADATA_STRING_BYTES,
            4,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Metadata,
            facts,
        ),
    }
}
/// Bound the attachment container without ever shortening payload strings.
///
/// # Parameters
/// - `value`: Adapter-produced attachment array.
/// - `facts`: Truncation facts shared by the enclosing coding envelope.
///
/// # Returns
/// A validated array whose entries and serialized container fit the explicit
/// attachment budget; oversized entries are omitted atomically.
fn bound_attachments(value: Value, facts: &mut TruncationFacts) -> Value {
    let Value::Array(values) = value else {
        facts.mark_attachment();
        return Value::Array(Vec::new());
    };

    let mut bounded = Vec::new();
    let mut used = 2usize;
    for value in values {
        if bounded.len() >= MAX_CODING_ATTACHMENT_ENTRIES {
            facts.mark_attachment();
            break;
        }
        let Some(value) = bound_attachment_entry(value, facts) else {
            continue;
        };
        let size = serialized_attachment_object_len(&value);
        let separator = usize::from(!bounded.is_empty());
        if used.saturating_add(separator).saturating_add(size) > MAX_CODING_ATTACHMENT_BYTES {
            facts.mark_attachment();
            continue;
        }
        used = used.saturating_add(separator).saturating_add(size);
        bounded.push(value);
    }
    Value::Array(bounded)
}

/// Validate and bound one attachment while preserving its payload atomically.
///
/// # Parameters
/// - `value`: Candidate attachment object.
/// - `facts`: Truncation facts shared by the enclosing coding envelope.
///
/// # Returns
/// A bounded attachment object, or `None` when its payload or shape is unsafe.
fn bound_attachment_entry(value: Value, facts: &mut TruncationFacts) -> Option<Value> {
    let Value::Object(fields) = value else {
        facts.mark_attachment();
        return None;
    };

    let mut bounded = Map::new();
    let mut has_url = false;
    for (key, value) in fields {
        match key.as_str() {
            "type" | "mime" => {
                let Value::String(text) = value else {
                    facts.mark_attachment();
                    continue;
                };
                bounded.insert(
                    key,
                    Value::String(bound_string(
                        text,
                        MAX_SMALL_METADATA_STRING_BYTES,
                        MAX_SMALL_METADATA_STRING_BYTES,
                        TruncationKind::Metadata,
                        facts,
                    )),
                );
            }
            "url" => {
                let Value::String(text) = value else {
                    facts.mark_attachment();
                    continue;
                };
                if serialized_string_len(&text) > MAX_CODING_ATTACHMENT_BYTES {
                    facts.mark_attachment();
                    return None;
                }
                has_url = true;
                bounded.insert(key, Value::String(text));
            }
            _ => {
                facts.unknown = true;
                facts.metadata = true;
                facts.mark_attachment();
            }
        }
    }
    if !has_url {
        facts.mark_attachment();
        return None;
    }
    if serialized_attachment_object_len(&Value::Object(bounded.clone()))
        > MAX_CODING_ATTACHMENT_BYTES
    {
        facts.mark_attachment();
        return None;
    }
    Some(Value::Object(bounded))
}

/// Calculate an attachment object's serialized size without serializing payloads.
///
/// # Parameters
/// - `value`: Bounded attachment object containing only string fields.
///
/// # Returns
/// The serialized JSON width, saturating on arithmetic overflow.
fn serialized_attachment_object_len(value: &Value) -> usize {
    let Value::Object(fields) = value else {
        return usize::MAX;
    };
    let mut size = 2usize;
    for (index, (key, value)) in fields.iter().enumerate() {
        let separator = usize::from(index > 0);
        let value_size = match value {
            Value::String(text) => serialized_string_len(text),
            other => serialized_len(other),
        };
        size = size
            .saturating_add(separator)
            .saturating_add(serialized_string_len(key))
            .saturating_add(1)
            .saturating_add(value_size);
    }
    size
}

/// Return whether a metadata key belongs to the coding presentation allowlist.
fn is_allowed_metadata_key(key: &str, include_diff: bool) -> bool {
    matches!(
        key,
        "preview"
            | "truncated"
            | "loaded"
            | "nextOffset"
            | "warnings"
            | "display"
            | "classification"
            | "recovered"
            | "diagnostics"
            | "diff"
            | "filediff"
            | "bytes"
            | "hardLink"
            | "noopLocations"
            | "count"
            | "matches"
            | "files"
            | "total"
            | "exit"
            | "output"
            | "outputPath"
            | "filepath"
            | "existed"
            | "created"
            | "executable"
            | "timedOut"
            | "durationMs"
            | "timeoutSeconds"
            | "timeoutClamped"
            | "pty"
            | "cwd"
            | "entries"
            | "groups"
            | "rows"
            | "path"
            | "type"
            | "lineStart"
            | "lineEnd"
            | "totalLines"
            | "offset"
            | "totalEntries"
            | "additions"
            | "deletions"
            | "patch"
            | "text"
            | "isMatch"
            | "outputTruncated"
            | "titleTruncated"
            | "displayTruncated"
            | "diffTruncated"
            | "attachmentsTruncated"
            | "diagnosticsTruncated"
            | "warningsTruncated"
            | "rowsTruncated"
            | "groupsTruncated"
            | "metadataTruncated"
            | "unknownFieldsDropped"
            | "envelopeTruncated"
    ) && (include_diff || !matches!(key, "diff" | "patch"))
}

/// Bound the metadata object and discard keys outside the presentation contract.
fn bound_metadata(value: Value, include_diff: bool, facts: &mut TruncationFacts) -> Value {
    let Value::Object(fields) = value else {
        facts.metadata = true;
        return Value::Object(Map::new());
    };
    let mut bounded = Map::new();
    for (key, value) in fields {
        if !is_allowed_metadata_key(&key, include_diff) {
            facts.unknown = true;
            facts.metadata = true;
            continue;
        }
        let Some(value) = bound_metadata_field(&key, value, include_diff, facts) else {
            facts.metadata = true;
            continue;
        };
        bounded.insert(key, value);
    }
    Value::Object(bounded)
}

/// Bound one allowlisted metadata value according to its semantic shape.
fn bound_metadata_field(
    key: &str,
    value: Value,
    include_diff: bool,
    facts: &mut TruncationFacts,
) -> Option<Value> {
    Some(match key {
        "display" => bound_display(value, facts),
        "warnings" => bound_warning_rows(value, facts),
        "diagnostics" => bound_diagnostic_rows(value, facts),
        "groups" => bound_grep_groups(value, facts),
        "rows" => bound_grep_rows(value, facts),
        "diff" | "patch" if include_diff => bound_diff_value(value, facts),
        "diff" | "patch" => return None,
        "filediff" => bound_filediff(value, include_diff, facts),
        "preview" | "output" | "text" => bound_display_value(value, facts),
        "outputPath" => {
            let Value::String(path) = value else {
                facts.metadata = true;
                return None;
            };
            if serialized_string_len(&path) > MAX_CODING_OUTPUT_PATH_BYTES {
                facts.metadata = true;
                return None;
            }
            Value::String(path)
        }
        "entries" => bound_json_value(
            value,
            MAX_CODING_GREP_DISPLAY_BYTES,
            8,
            usize::MAX,
            TruncationKind::Metadata,
            facts,
        ),
        "loaded" | "noopLocations" | "matches" | "files" => bound_json_value(
            value,
            MAX_CODING_GREP_DISPLAY_BYTES,
            8,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Metadata,
            facts,
        ),
        _ => bound_json_value(
            value,
            MAX_SMALL_METADATA_STRING_BYTES,
            6,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Metadata,
            facts,
        ),
    })
}

/// Bound a display value, including a Read/Write text preview or Grep groups.
fn bound_display(value: Value, facts: &mut TruncationFacts) -> Value {
    let Value::Object(fields) = value else {
        return bound_display_value(value, facts);
    };
    let mut bounded = Map::new();
    for (key, value) in fields {
        let Some(value) = (match key.as_str() {
            "text" | "preview" | "output" => Some(bound_display_value(value, facts)),
            "groups" => Some(bound_grep_groups(value, facts)),
            "rows" => Some(bound_grep_rows(value, facts)),
            "entries" => Some(bound_json_value(
                value,
                MAX_CODING_GREP_DISPLAY_BYTES,
                8,
                usize::MAX,
                TruncationKind::Display,
                facts,
            )),
            "path" | "type" | "file" => Some(bound_json_value(
                value,
                MAX_SMALL_METADATA_STRING_BYTES,
                4,
                MAX_GENERIC_METADATA_ITEMS,
                TruncationKind::Metadata,
                facts,
            )),
            "lineStart" | "lineEnd" | "totalLines" | "offset" | "totalEntries" | "truncated"
            | "count" | "total" | "isMatch" => Some(bound_json_value(
                value,
                MAX_SMALL_METADATA_STRING_BYTES,
                2,
                8,
                TruncationKind::Metadata,
                facts,
            )),
            _ => {
                facts.unknown = true;
                facts.metadata = true;
                None
            }
        }) else {
            continue;
        };
        bounded.insert(key, value);
    }
    Value::Object(bounded)
}

/// Bound one display string or recursively bound display-shaped value.
fn bound_display_value(value: Value, facts: &mut TruncationFacts) -> Value {
    match value {
        Value::String(text) => Value::String(bound_line_string(
            text,
            MAX_CODING_DISPLAY_BYTES,
            MAX_CODING_DISPLAY_BYTES,
            TruncationKind::Display,
            facts,
        )),
        other => bound_json_value(
            other,
            MAX_CODING_DISPLAY_BYTES,
            8,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Display,
            facts,
        ),
    }
}

/// Bound an Edit diff or patch independently of ordinary display text.
fn bound_diff_value(value: Value, facts: &mut TruncationFacts) -> Value {
    match value {
        Value::String(text) => Value::String(bound_string(
            text,
            MAX_CODING_DIFF_BYTES,
            MAX_CODING_DIFF_BYTES,
            TruncationKind::Diff,
            facts,
        )),
        other => bound_json_value(
            other,
            MAX_CODING_DIFF_BYTES,
            8,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Diff,
            facts,
        ),
    }
}

/// Bound the nested file-diff facts while preserving counts and file identity.
fn bound_filediff(value: Value, include_diff: bool, facts: &mut TruncationFacts) -> Value {
    let Value::Object(fields) = value else {
        return bound_json_value(
            value,
            MAX_CODING_DIFF_BYTES,
            6,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Diff,
            facts,
        );
    };
    let mut bounded = Map::new();
    for (key, value) in fields {
        let value = match key.as_str() {
            "file" => bound_json_value(
                value,
                MAX_SMALL_METADATA_STRING_BYTES,
                4,
                MAX_GENERIC_METADATA_ITEMS,
                TruncationKind::Metadata,
                facts,
            ),
            "patch" if include_diff => bound_diff_value(value, facts),
            "patch" => {
                facts.diff = true;
                continue;
            }
            "additions" | "deletions" => bound_json_value(
                value,
                MAX_SMALL_METADATA_STRING_BYTES,
                2,
                8,
                TruncationKind::Metadata,
                facts,
            ),
            _ => {
                facts.unknown = true;
                facts.metadata = true;
                continue;
            }
        };
        bounded.insert(key, value);
    }
    Value::Object(bounded)
}

/// Bound warning rows to sixteen entries of at most two KiB each.
fn bound_warning_rows(value: Value, facts: &mut TruncationFacts) -> Value {
    bound_limited_rows(
        value,
        MAX_CODING_DIAGNOSTIC_ROWS,
        MAX_CODING_DIAGNOSTIC_ROW_BYTES,
        TruncationKind::Warnings,
        facts,
    )
}

/// Bound diagnostic rows in either flat or path-keyed adapter shape.
fn bound_diagnostic_rows(value: Value, facts: &mut TruncationFacts) -> Value {
    match value {
        Value::Array(rows) => bound_limited_rows(
            Value::Array(rows),
            MAX_CODING_DIAGNOSTIC_ROWS,
            MAX_CODING_DIAGNOSTIC_ROW_BYTES,
            TruncationKind::Diagnostics,
            facts,
        ),
        Value::Object(paths) => bound_path_diagnostics(paths, facts),
        other => bound_json_value(
            other,
            MAX_CODING_DIAGNOSTIC_ROWS.saturating_mul(MAX_CODING_DIAGNOSTIC_ROW_BYTES),
            8,
            MAX_CODING_DIAGNOSTIC_ROWS,
            TruncationKind::Diagnostics,
            facts,
        ),
    }
}

/// Bound path-keyed diagnostics without rewriting path keys or rows.
///
/// # Parameters
/// - `paths`: Diagnostic arrays keyed by adapter-provided file paths.
/// - `facts`: Truncation facts shared by the enclosing coding envelope.
///
/// # Returns
/// A bounded path-keyed object with at most the configured total rows and
/// per-row serialized byte limit; unsafe paths and rows are omitted atomically.
fn bound_path_diagnostics(paths: Map<String, Value>, facts: &mut TruncationFacts) -> Value {
    let mut bounded = Map::new();
    let mut rows_kept = 0usize;
    let mut paths_kept = 0usize;
    for (path, rows) in paths {
        if paths_kept >= MAX_CODING_DIAGNOSTIC_ROWS || rows_kept >= MAX_CODING_DIAGNOSTIC_ROWS {
            facts.diagnostics = true;
            break;
        }
        if serialized_string_len(&path) > MAX_CODING_DIAGNOSTIC_PATH_BYTES {
            facts.diagnostics = true;
            continue;
        }
        let Value::Array(rows) = rows else {
            facts.diagnostics = true;
            continue;
        };
        if rows.is_empty() {
            continue;
        }
        let mut bounded_rows = Vec::new();
        for row in rows {
            if rows_kept >= MAX_CODING_DIAGNOSTIC_ROWS {
                facts.diagnostics = true;
                break;
            }
            let mut row_facts = TruncationFacts::default();
            let bounded_row = bound_json_value(
                row,
                MAX_CODING_DIAGNOSTIC_ROW_BYTES,
                8,
                MAX_GENERIC_METADATA_ITEMS,
                TruncationKind::Diagnostics,
                &mut row_facts,
            );
            if row_facts.any() || serialized_len(&bounded_row) > MAX_CODING_DIAGNOSTIC_ROW_BYTES {
                facts.diagnostics = true;
                continue;
            }
            bounded_rows.push(bounded_row);
            rows_kept += 1;
        }
        if bounded_rows.is_empty() {
            facts.diagnostics = true;
            continue;
        }
        bounded.insert(path, Value::Array(bounded_rows));
        paths_kept += 1;
    }
    Value::Object(bounded)
}

/// Bound a row list with independent count and serialized-byte limits.
fn bound_limited_rows(
    value: Value,
    max_rows: usize,
    max_row_bytes: usize,
    kind: TruncationKind,
    facts: &mut TruncationFacts,
) -> Value {
    let Value::Array(rows) = value else {
        return bound_json_value(
            value,
            max_rows.saturating_mul(max_row_bytes),
            8,
            max_rows,
            kind,
            facts,
        );
    };
    let mut bounded = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        if index >= max_rows {
            facts.mark(kind);
            break;
        }
        let mut row = match row {
            Value::String(text) => Value::String(bound_string(
                text,
                max_row_bytes,
                max_row_bytes,
                kind,
                facts,
            )),
            other => bound_json_value(other, max_row_bytes, 6, 16, kind, facts),
        };
        if serialized_len(&row) > max_row_bytes {
            shrink_value_to_budget(&mut row, max_row_bytes);
            facts.mark(kind);
        }
        bounded.push(row);
    }
    Value::Array(bounded)
}

struct GrepDisplayBudget {
    bytes_left: usize,
    rows_left: usize,
}

/// Bound a standalone Grep row list to the shared display limits.
fn bound_grep_rows(value: Value, facts: &mut TruncationFacts) -> Value {
    let Value::Array(rows) = value else {
        facts.rows = true;
        facts.display = true;
        return Value::Array(Vec::new());
    };
    let mut budget = GrepDisplayBudget {
        // Reserve the serialized array delimiters before fitting complete rows.
        bytes_left: MAX_CODING_GREP_DISPLAY_BYTES.saturating_sub(2),
        rows_left: MAX_CODING_GREP_ROWS,
    };
    Value::Array(bound_grep_row_list(rows, &mut budget, facts))
}

/// Bound Grep groups to one hundred files and two thousand rows.
fn bound_grep_groups(value: Value, facts: &mut TruncationFacts) -> Value {
    let Value::Array(groups) = value else {
        facts.groups = true;
        facts.display = true;
        return Value::Array(Vec::new());
    };
    let mut budget = GrepDisplayBudget {
        // Reserve the serialized array delimiters before fitting complete groups.
        bytes_left: MAX_CODING_GREP_DISPLAY_BYTES.saturating_sub(2),
        rows_left: MAX_CODING_GREP_ROWS,
    };
    let mut bounded = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        if index >= MAX_CODING_GREP_GROUPS {
            facts.groups = true;
            break;
        }
        if !group.is_object() || group.get("rows").is_some_and(|rows| !rows.is_array()) {
            facts.groups = true;
            facts.display = true;
            continue;
        }
        let had_rows = group
            .get("rows")
            .and_then(Value::as_array)
            .is_some_and(|rows| !rows.is_empty());
        let group = bound_grep_group(group, &budget, facts);
        let size = serialized_len(&group);
        let separator = usize::from(!bounded.is_empty());
        if had_rows
            && group
                .get("rows")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        {
            facts.rows = true;
            facts.display = true;
            continue;
        }
        if size.saturating_add(separator) > budget.bytes_left {
            facts.display = true;
            continue;
        }
        let rows_kept = group
            .get("rows")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        budget.bytes_left = budget.bytes_left.saturating_sub(size + separator);
        budget.rows_left = budget.rows_left.saturating_sub(rows_kept);
        bounded.push(group);
    }
    Value::Array(bounded)
}

/// Bound one Grep group and its nested rows.
fn bound_grep_group(
    value: Value,
    budget: &GrepDisplayBudget,
    facts: &mut TruncationFacts,
) -> Value {
    let Value::Object(fields) = value else {
        return bound_json_value(
            value,
            budget.bytes_left,
            6,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Display,
            facts,
        );
    };
    let mut bounded = Map::new();
    let mut rows = None;
    for (key, value) in fields {
        let value = match key.as_str() {
            "path" | "file" | "title" => bound_json_value(
                value,
                MAX_SMALL_METADATA_STRING_BYTES,
                4,
                MAX_GENERIC_METADATA_ITEMS,
                TruncationKind::Metadata,
                facts,
            ),
            "rows" => {
                rows = Some(match value {
                    Value::Array(rows) => rows,
                    other => {
                        facts.rows = true;
                        Vec::from([other])
                    }
                });
                Value::Array(Vec::new())
            }
            "matches" | "count" | "total" | "truncated" => bound_json_value(
                value,
                MAX_SMALL_METADATA_STRING_BYTES,
                2,
                8,
                TruncationKind::Metadata,
                facts,
            ),
            _ => {
                facts.unknown = true;
                facts.metadata = true;
                continue;
            }
        };
        bounded.insert(key, value);
    }

    if let Some(rows) = rows {
        // Reserve the group wrapper once.  The row list then consumes only
        // row payload bytes, so the outer group is not charged a second time.
        let wrapper_bytes = serialized_len(&Value::Object(bounded.clone()));
        let mut local = GrepDisplayBudget {
            bytes_left: budget.bytes_left.saturating_sub(wrapper_bytes),
            rows_left: budget.rows_left,
        };
        let rows = bound_grep_row_list(rows, &mut local, facts);
        bounded.insert("rows".to_string(), Value::Array(rows));
    }
    Value::Object(bounded)
}

/// Bound and count Grep rows while preserving match identity fields.
fn bound_grep_row_list(
    rows: Vec<Value>,
    budget: &mut GrepDisplayBudget,
    facts: &mut TruncationFacts,
) -> Vec<Value> {
    let mut bounded = Vec::new();
    for row in rows {
        if !row.is_object() {
            facts.rows = true;
            facts.display = true;
            continue;
        }
        if budget.rows_left == 0 {
            facts.rows = true;
            break;
        }
        let mut row_facts = TruncationFacts::default();
        let row = bound_grep_row(row, budget.bytes_left, &mut row_facts);
        if row_facts.display {
            facts.merge(&row_facts);
            facts.rows = true;
            continue;
        }
        facts.merge(&row_facts);
        let size = serialized_len(&row);
        let separator = usize::from(!bounded.is_empty());
        if size.saturating_add(separator) > budget.bytes_left {
            facts.rows = true;
            continue;
        }
        budget.bytes_left = budget.bytes_left.saturating_sub(size + separator);
        budget.rows_left -= 1;
        bounded.push(row);
    }
    bounded
}

/// Bound the allowlisted fields of one Grep row.
fn bound_grep_row(value: Value, available_bytes: usize, facts: &mut TruncationFacts) -> Value {
    let Value::Object(fields) = value else {
        return bound_json_value(
            value,
            available_bytes,
            4,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Display,
            facts,
        );
    };
    let mut bounded = Map::new();
    for (key, value) in fields {
        let value = match key.as_str() {
            "text" | "match" | "context" => {
                bound_string_or_json(value, available_bytes, TruncationKind::Display, facts)
            }
            "line" | "isMatch" | "kind" => bound_json_value(
                value,
                MAX_SMALL_METADATA_STRING_BYTES,
                2,
                8,
                TruncationKind::Metadata,
                facts,
            ),
            _ => {
                facts.unknown = true;
                facts.metadata = true;
                continue;
            }
        };
        bounded.insert(key, value);
    }
    Value::Object(bounded)
}

/// Bound a string or recursively bound JSON field to a serialized-byte budget.
fn bound_string_or_json(
    value: Value,
    max_bytes: usize,
    kind: TruncationKind,
    facts: &mut TruncationFacts,
) -> Value {
    match value {
        Value::String(text) => Value::String(bound_string(text, max_bytes, max_bytes, kind, facts)),
        other => bound_json_value(other, max_bytes, 4, MAX_GENERIC_METADATA_ITEMS, kind, facts),
    }
}

/// Bound a recursive JSON value without serializing an attacker-sized input.
fn bound_json_value(
    value: Value,
    max_serialized_bytes: usize,
    depth: usize,
    max_items: usize,
    kind: TruncationKind,
    facts: &mut TruncationFacts,
) -> Value {
    match value {
        Value::String(text) => Value::String(bound_string(
            text,
            max_serialized_bytes,
            max_serialized_bytes,
            kind,
            facts,
        )),
        Value::Array(values) => {
            if depth == 0 || max_serialized_bytes < 2 {
                if !values.is_empty() {
                    facts.mark(kind);
                }
                return Value::Array(Vec::new());
            }
            let mut bounded = Vec::new();
            let mut used = 2usize;
            for (index, value) in values.into_iter().enumerate() {
                if index >= max_items {
                    facts.mark(kind);
                    break;
                }
                let separator = usize::from(!bounded.is_empty());
                let remaining = max_serialized_bytes.saturating_sub(used + separator);
                if remaining == 0 {
                    facts.mark(kind);
                    break;
                }
                let value = bound_json_value(
                    value,
                    remaining,
                    depth.saturating_sub(1),
                    max_items,
                    kind,
                    facts,
                );
                let size = serialized_len(&value);
                if used.saturating_add(separator).saturating_add(size) > max_serialized_bytes {
                    facts.mark(kind);
                    break;
                }
                used += separator + size;
                bounded.push(value);
            }
            Value::Array(bounded)
        }
        Value::Object(fields) => {
            if depth == 0 || max_serialized_bytes < 2 {
                if !fields.is_empty() {
                    facts.mark(kind);
                }
                return Value::Object(Map::new());
            }
            let mut bounded = Map::new();
            let mut used = 2usize;
            for (index, (key, value)) in fields.into_iter().enumerate() {
                if index >= max_items {
                    facts.mark(kind);
                    break;
                }
                let original_key_len = key.len();
                let key = bound_string(
                    key,
                    MAX_METADATA_KEY_BYTES,
                    MAX_METADATA_KEY_BYTES,
                    kind,
                    facts,
                );
                let key_was_truncated = key.len() < original_key_len;
                let key_size = serialized_len(&Value::String(key.clone()));
                let separator = usize::from(!bounded.is_empty());
                let remaining =
                    max_serialized_bytes.saturating_sub(used + separator + key_size + 1);
                if remaining == 0 {
                    facts.mark(kind);
                    break;
                }
                let value = bound_json_value(value, remaining, depth - 1, max_items, kind, facts);
                let value_size = serialized_len(&value);
                if used
                    .saturating_add(separator)
                    .saturating_add(key_size)
                    .saturating_add(1)
                    .saturating_add(value_size)
                    > max_serialized_bytes
                {
                    facts.mark(kind);
                    break;
                }
                used += separator + key_size + 1 + value_size;
                bounded.insert(key, value);
                if key_was_truncated {
                    facts.mark(kind);
                }
            }
            Value::Object(bounded)
        }
        scalar => {
            if serialized_len(&scalar) > max_serialized_bytes {
                facts.mark(kind);
                Value::Null
            } else {
                scalar
            }
        }
    }
}

/// Bound a UTF-8 string by raw bytes and its JSON-escaped serialized bytes.
fn bound_string(
    mut text: String,
    max_raw_bytes: usize,
    max_serialized_bytes: usize,
    kind: TruncationKind,
    facts: &mut TruncationFacts,
) -> String {
    let original_len = text.len();
    let mut end = text.len().min(max_raw_bytes);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    if max_serialized_bytes < 2 {
        end = 0;
    } else {
        let mut used = 2usize;
        let mut serialized_end = 0usize;
        for (offset, character) in text[..end].char_indices() {
            let encoded = json_char_len(character);
            if used.saturating_add(encoded) > max_serialized_bytes {
                break;
            }
            used += encoded;
            serialized_end = offset + character.len_utf8();
        }
        end = serialized_end;
    }
    if end < original_len {
        facts.mark(kind);
        text.truncate(end);
    }
    text
}

/// Return the serialized byte width of one character inside a JSON string.
pub(crate) fn json_char_len(character: char) -> usize {
    match character {
        '"' | '\\' | '\u{08}' | '\u{0c}' | '\n' | '\r' | '\t' => 2,
        character if character < ' ' => 6,
        character => character.len_utf8(),
    }
}
/// Return a JSON string's serialized width without allocating an encoded copy.
///
/// # Parameters
/// - `text`: UTF-8 string whose JSON width is needed.
///
/// # Returns
/// The quoted JSON string width, saturating on arithmetic overflow.
pub(crate) fn serialized_string_len(text: &str) -> usize {
    text.chars().fold(2usize, |used, character| {
        used.saturating_add(json_char_len(character))
    })
}

/// Shrink an already bounded value to a serialized-byte budget.
fn shrink_value_to_budget(value: &mut Value, max_serialized_bytes: usize) {
    if serialized_len(value) <= max_serialized_bytes {
        return;
    }
    let original = std::mem::replace(value, Value::Null);
    let mut local_facts = TruncationFacts::default();
    *value = bound_json_value(
        original,
        max_serialized_bytes,
        8,
        MAX_GENERIC_METADATA_ITEMS,
        TruncationKind::Metadata,
        &mut local_facts,
    );
    if serialized_len(value) > max_serialized_bytes {
        *value = Value::Null;
    }
}

/// Return serialized JSON bytes without touching an unbounded input value.
fn serialized_len<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

/// Add explicit truncation facts to the retained metadata object.
fn add_truncation_facts(metadata: &mut Value, facts: &TruncationFacts) {
    let Value::Object(metadata) = metadata else {
        return;
    };
    if facts.any() {
        metadata.insert("truncated".to_string(), Value::Bool(true));
    }
    let flags = [
        ("outputTruncated", facts.output),
        ("titleTruncated", facts.title),
        ("displayTruncated", facts.display),
        ("diffTruncated", facts.diff),
        ("attachmentsTruncated", facts.attachments),
        ("diagnosticsTruncated", facts.diagnostics),
        ("warningsTruncated", facts.warnings),
        ("rowsTruncated", facts.rows),
        ("groupsTruncated", facts.groups),
        ("metadataTruncated", facts.metadata || facts.unknown),
        ("unknownFieldsDropped", facts.unknown),
        ("envelopeTruncated", facts.envelope),
    ];
    for (key, enabled) in flags {
        if enabled {
            metadata.insert(key.to_string(), Value::Bool(true));
        }
    }
}

/// Drop complete attachment entries until the enclosing envelope fits.
///
/// # Parameters
/// - `envelope`: Mutable coding result envelope being fitted.
/// - `facts`: Truncation facts shared by the enclosing coding result.
///
/// # Returns
/// Nothing; attachments are removed only as complete entries or as a complete
/// container, never by recursively shortening a payload string.
fn drop_attachments_to_budget(envelope: &mut Map<String, Value>, facts: &mut TruncationFacts) {
    loop {
        if serialized_len(envelope) <= MAX_CODING_ENVELOPE_BYTES {
            return;
        }
        let removed_entry = match envelope.get_mut("attachments") {
            Some(Value::Array(entries)) => entries.pop().is_some(),
            Some(_) | None => false,
        };
        if removed_entry {
            facts.mark_attachment();
            refresh_envelope_flags(envelope, facts);
            continue;
        }
        if envelope.remove("attachments").is_some() {
            facts.mark_attachment();
            refresh_envelope_flags(envelope, facts);
        }
        return;
    }
}

/// Return whether a bounded metadata field carries adapter-owned state.
fn is_adapter_metadata_flag(key: &str) -> bool {
    matches!(
        key,
        "truncated"
            | "nextOffset"
            | "classification"
            | "recovered"
            | "bytes"
            | "hardLink"
            | "noopLocations"
            | "count"
            | "matches"
            | "files"
            | "total"
            | "exit"
            | "output"
            | "outputPath"
            | "filepath"
            | "existed"
            | "created"
            | "executable"
            | "timedOut"
            | "durationMs"
            | "timeoutSeconds"
            | "timeoutClamped"
            | "pty"
            | "cwd"
            | "lineStart"
            | "lineEnd"
            | "totalLines"
            | "offset"
            | "totalEntries"
            | "additions"
            | "deletions"
            | "type"
            | "path"
            | "outputTruncated"
            | "titleTruncated"
            | "displayTruncated"
            | "diffTruncated"
            | "attachmentsTruncated"
            | "diagnosticsTruncated"
            | "warningsTruncated"
            | "rowsTruncated"
            | "groupsTruncated"
            | "metadataTruncated"
            | "unknownFieldsDropped"
            | "envelopeTruncated"
    )
}

/// Keep the complete approved envelope below one serialized-byte hard bound.
fn enforce_envelope_bound(envelope: &mut Map<String, Value>, facts: &mut TruncationFacts) {
    if serialized_len(envelope) <= MAX_CODING_ENVELOPE_BYTES {
        return;
    }

    if envelope.contains_key("attachments") {
        drop_attachments_to_budget(envelope, facts);
        refresh_envelope_flags(envelope, facts);
        if serialized_len(envelope) <= MAX_CODING_ENVELOPE_BYTES {
            return;
        }
    }
    for key in ["matches", "paths", "files"] {
        if envelope.contains_key(key) {
            if let Some(value) = envelope.get_mut(key) {
                shrink_value_to_budget(value, 8 * 1024);
            }
            facts.display = true;
            refresh_envelope_flags(envelope, facts);
            if serialized_len(envelope) <= MAX_CODING_ENVELOPE_BYTES {
                return;
            }
        }
    }
    for key in ["content", "stdout", "stderr"] {
        if envelope.contains_key(key) {
            if let Some(value) = envelope.get_mut(key) {
                shrink_value_to_budget(value, 16 * 1024);
            }
            facts.display = true;
            refresh_envelope_flags(envelope, facts);
            if serialized_len(envelope) <= MAX_CODING_ENVELOPE_BYTES {
                return;
            }
        }
    }
    for key in ["diagnostics", "warnings", "display", "filediff"] {
        let changed = if let Some(Value::Object(metadata)) = envelope.get_mut("metadata") {
            if key == "diagnostics" {
                metadata.remove(key).is_some()
            } else if let Some(value) = metadata.get_mut(key) {
                shrink_value_to_budget(value, 16 * 1024);
                true
            } else {
                false
            }
        } else {
            false
        };
        if changed {
            if key == "diagnostics" {
                facts.diagnostics = true;
            } else {
                facts.metadata = true;
            }
            refresh_envelope_flags(envelope, facts);
            if serialized_len(envelope) <= MAX_CODING_ENVELOPE_BYTES {
                return;
            }
        }
    }
    for key in [
        "attachments",
        "matches",
        "paths",
        "files",
        "stdout",
        "stderr",
        "content",
    ] {
        if envelope.remove(key).is_some() {
            facts.display = true;
            refresh_envelope_flags(envelope, facts);
            if serialized_len(envelope) <= MAX_CODING_ENVELOPE_BYTES {
                return;
            }
        }
    }

    facts.envelope = true;
    envelope.retain(|key, _| matches!(key.as_str(), "title" | "output" | "metadata"));
    if let Some(value) = envelope.get_mut("title") {
        shrink_value_to_budget(value, MAX_CODING_TITLE_BYTES);
    }
    if let Some(value) = envelope.get_mut("output") {
        shrink_value_to_budget(value, MAX_CODING_OUTPUT_BYTES);
    }
    if let Some(metadata) = envelope.get_mut("metadata") {
        if let Value::Object(metadata) = metadata {
            metadata.retain(|key, _| is_adapter_metadata_flag(key));
        } else {
            *metadata = Value::Object(Map::new());
        }
    } else {
        envelope.insert("metadata".to_string(), Value::Object(Map::new()));
    }
    refresh_envelope_flags(envelope, facts);
}

/// Refresh flags after the final envelope fitting pass.
fn refresh_envelope_flags(envelope: &mut Map<String, Value>, facts: &TruncationFacts) {
    if let Some(metadata) = envelope.get_mut("metadata") {
        add_truncation_facts(metadata, facts);
    }
}

/// Convert a value to its plain display text for the legacy policy.
fn value_as_display_text(value: &Value) -> String {
    match value.as_str() {
        Some(s) => s.to_string(),
        None => value.to_string(),
    }
}

/// Return the last `n` Unicode scalar values without splitting UTF-8.
fn last_n_chars(text: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let total = text.chars().count();
    if total <= n {
        return text.to_string();
    }
    text.chars().skip(total - n).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn under_limit_preserves_original_value_shape() {
        let original = json!({"results": ["a", "b"]});
        let capped = cap_tool_output(original.clone());
        assert_eq!(capped, original);
    }

    #[test]
    fn over_limit_returns_notice_and_last_chars() {
        let body = "x".repeat(MAX_TOOL_OUTPUT_CHARS + 100);
        let capped = cap_tool_output(Value::String(body.clone()));
        let text = capped.as_str().expect("string");
        assert!(text.starts_with("[tool output truncated: original "));
        assert!(text.contains(&format!("{n} chars", n = body.chars().count())));
        assert!(text.contains(&format!("showing last {MAX_TOOL_OUTPUT_CHARS} chars")));
        let tail = text
            .split_once('\n')
            .map(|(_, rest)| rest)
            .expect("notice + body");
        assert_eq!(tail.chars().count(), MAX_TOOL_OUTPUT_CHARS);
        assert_eq!(tail, &body[body.len() - MAX_TOOL_OUTPUT_CHARS..]);
    }

    #[test]
    fn last_n_handles_multibyte_chars() {
        let body: String = "你".repeat(MAX_TOOL_OUTPUT_CHARS + 10);
        let capped = cap_tool_output(Value::String(body));
        let text = capped.as_str().unwrap();
        let tail = text.split_once('\n').unwrap().1;
        assert_eq!(tail.chars().count(), MAX_TOOL_OUTPUT_CHARS);
        assert!(tail.chars().all(|c| c == '你'));
    }

    #[test]
    fn over_limit_json_object_is_stringified_then_capped() {
        let big = "y".repeat(MAX_TOOL_OUTPUT_CHARS + 50);
        let capped = cap_tool_output(json!({ "blob": big }));
        assert!(capped.is_string());
        let text = capped.as_str().unwrap();
        assert!(text.starts_with("[tool output truncated:"));
        let tail = text.split_once('\n').unwrap().1;
        assert_eq!(tail.chars().count(), MAX_TOOL_OUTPUT_CHARS);
    }

    #[test]
    fn coding_policy_preserves_envelope_and_bounds_output() {
        let result = json!({
            "title": "src/main.rs",
            "output": "x".repeat(MAX_CODING_OUTPUT_BYTES + 100),
            "metadata": {
                "display": {"type": "file", "text": "line\n".repeat(20)},
                "truncated": false,
            },
            "content": "line\n".repeat(20),
        });
        let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
        assert!(capped.is_object());
        assert_eq!(capped["title"], "src/main.rs");
        assert!(capped["output"].as_str().unwrap().len() <= MAX_CODING_OUTPUT_BYTES);
        assert_eq!(capped["metadata"]["outputTruncated"], true);
        assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
    }

    #[test]
    fn hostile_metadata_is_dropped_and_row_bounded() {
        let result = json!({
            "title": "read",
            "output": "ok",
            "metadata": {
                "evil": "z".repeat(MAX_CODING_ENVELOPE_BYTES),
                "warnings": (0..40).map(|_| "w".repeat(MAX_CODING_DIAGNOSTIC_ROW_BYTES + 10)).collect::<Vec<_>>(),
                "display": {"rows": (0..2100).map(|line| json!({"line": line, "text": "x"})).collect::<Vec<_>>()},
            },
        });
        let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
        let metadata = capped["metadata"].as_object().unwrap();
        assert!(!metadata.contains_key("evil"));
        assert_eq!(metadata["unknownFieldsDropped"], true);
        assert!(metadata["warnings"].as_array().unwrap().len() <= MAX_CODING_DIAGNOSTIC_ROWS);
        assert!(metadata["display"]["rows"].as_array().unwrap().len() <= MAX_CODING_GREP_ROWS);
        assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
    }

    #[test]
    fn edit_policy_bounds_diff_separately() {
        let result = json!({
            "title": "src/main.rs",
            "output": "Edit applied",
            "metadata": {
                "diff": "-old\n+new\n".repeat(MAX_CODING_DIFF_BYTES),
                "filediff": {"patch": "p".repeat(MAX_CODING_DIFF_BYTES + 20), "additions": 1},
                "display": {"type": "file", "text": "final"},
            },
        });
        let capped = cap_tool_output_with_policy(result, ToolResultPolicy::CodingWithDiff);
        assert!(capped["metadata"]["diff"].as_str().unwrap().len() <= MAX_CODING_DIFF_BYTES);
        assert_eq!(capped["metadata"]["diffTruncated"], true);
        assert_eq!(capped["metadata"]["display"]["text"], "final");
        assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
    }
    #[test]
    fn coding_metadata_retains_current_write_and_bash_fields() {
        let result = json!({
            "title": "tool",
            "output": "ok",
            "metadata": {
                "existed": true,
                "created": false,
                "executable": true,
                "bytes": 42,
                "hardLink": true,
                "noopLocations": ["2#ABCD"],
                "count": 3,
                "matches": 2,
                "files": 1,
                "total": 2,
                "timedOut": false,
                "durationMs": 12,
                "timeoutSeconds": 300,
                "timeoutClamped": false,
                "pty": true,
                "cwd": "/workspace"
            }
        });
        let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
        let metadata = capped["metadata"].as_object().unwrap();
        assert_eq!(metadata["existed"], true);
        assert_eq!(metadata["created"], false);
        assert_eq!(metadata["executable"], true);
        assert_eq!(metadata["bytes"], 42);
        assert_eq!(metadata["hardLink"], true);
        assert_eq!(metadata["noopLocations"], json!(["2#ABCD"]));
        assert_eq!(metadata["count"], 3);
        assert_eq!(metadata["matches"], 2);
        assert_eq!(metadata["files"], 1);
        assert_eq!(metadata["total"], 2);
        assert_eq!(metadata["timedOut"], false);
        assert_eq!(metadata["durationMs"], 12);
        assert_eq!(metadata["timeoutSeconds"], 300);
        assert_eq!(metadata["timeoutClamped"], false);
        assert_eq!(metadata["pty"], true);
        assert_eq!(metadata["cwd"], "/workspace");
    }

    #[test]
    fn deeply_nested_array_stops_at_depth_bound() {
        let mut value = Value::String("leaf".to_string());
        for _ in 0..64 {
            value = Value::Array(vec![value]);
        }
        let capped = cap_tool_output_with_policy(value, ToolResultPolicy::Coding);
        assert!(serialized_len(&capped) <= MAX_CODING_OUTPUT_BYTES);
        let mut depth = 0;
        let mut current = &capped;
        while let Value::Array(values) = current {
            depth += 1;
            if depth == 9 {
                break;
            }
            let Some(next) = values.first() else {
                break;
            };
            current = next;
        }
        assert!(depth <= 9);
    }

    #[test]
    fn oversized_attachment_is_dropped_without_payload_truncation() {
        let payload = format!(
            "data:image/png;base64,{}",
            "A".repeat(MAX_CODING_ATTACHMENT_BYTES)
        );
        let result = json!({
            "title": "image",
            "output": "Image read successfully",
            "attachments": [{"type": "file", "mime": "image/png", "url": payload}],
            "metadata": {}
        });
        let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
        assert_eq!(capped["attachments"], json!([]));
        assert_eq!(capped["metadata"]["attachmentsTruncated"], true);
        assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
    }

    #[test]
    fn path_keyed_diagnostics_bound_rows_without_rewriting_paths() {
        let long_path = "p".repeat(MAX_CODING_DIAGNOSTIC_PATH_BYTES + 10);
        let mut diagnostics = Map::new();
        diagnostics.insert(long_path.clone(), json!([{"message": "must be omitted"}]));
        diagnostics.insert(
            "/src/big.rs".to_string(),
            json!([{"message": "x".repeat(MAX_CODING_DIAGNOSTIC_ROW_BYTES)}]),
        );
        for index in 0..MAX_CODING_DIAGNOSTIC_ROWS {
            diagnostics.insert(
                format!("/src/{index}.rs"),
                json!([{"message": format!("issue {index}")}]),
            );
        }
        let capped = cap_tool_output_with_policy(
            json!({
                "title": "edit",
                "output": "done",
                "metadata": {"diagnostics": Value::Object(diagnostics)}
            }),
            ToolResultPolicy::Coding,
        );
        let diagnostics = capped["metadata"]["diagnostics"].as_object().unwrap();
        assert!(!diagnostics.contains_key(&long_path));
        let total_rows: usize = diagnostics
            .values()
            .map(|rows| rows.as_array().map_or(0, Vec::len))
            .sum();
        assert!(total_rows <= MAX_CODING_DIAGNOSTIC_ROWS);
        for rows in diagnostics.values() {
            for row in rows.as_array().unwrap() {
                assert!(serialized_len(row) <= MAX_CODING_DIAGNOSTIC_ROW_BYTES);
            }
        }
        assert_eq!(capped["metadata"]["diagnosticsTruncated"], true);
    }

    #[test]
    fn malformed_coding_array_is_bounded_without_stringifying_input() {
        let malformed = Value::Array(vec![Value::String(
            "x".repeat(MAX_CODING_OUTPUT_BYTES.saturating_mul(2)),
        )]);
        let capped = cap_tool_output_with_policy(malformed, ToolResultPolicy::Coding);
        assert!(capped.is_array());
        assert!(serialized_len(&capped) <= MAX_CODING_OUTPUT_BYTES);
    }
    /// Keep complete Grep display groups within one shared byte budget.
    #[test]
    fn coding_policy_counts_grep_group_bytes_once_and_keeps_later_groups() {
        let first_row = "L".repeat(32 * 1024);
        let later_row = "S".repeat(12 * 1024);
        let result = json!({
            "title": "needle",
            "output": "2 matches in 2 files.",
            "metadata": {
                "matches": 2,
                "files": 2,
                "truncated": false,
                "display": {
                    "groups": [
                        {
                            "path": "large.rs",
                            "rows": [{"line": 1, "text": first_row.clone(), "isMatch": true}]
                        },
                        {
                            "path": "later.rs",
                            "rows": [{"line": 2, "text": later_row.clone(), "isMatch": true}]
                        }
                    ]
                }
            }
        });

        let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
        let envelope = capped
            .as_object()
            .expect("coding result must remain an object");
        assert!(envelope.get("title").is_some_and(Value::is_string));
        assert!(envelope.get("output").is_some_and(Value::is_string));
        assert!(envelope.get("metadata").is_some_and(Value::is_object));
        assert_eq!(capped["title"], "needle");
        assert_eq!(capped["output"], "2 matches in 2 files.");

        let metadata = capped["metadata"].as_object().expect("metadata object");
        assert_eq!(metadata["truncated"], false);
        let groups = metadata["display"]["groups"]
            .as_array()
            .expect("Grep display groups must remain an array");
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["path"], "large.rs");
        assert_eq!(groups[1]["path"], "later.rs");
        assert_eq!(groups[0]["rows"][0]["line"], 1);
        assert_eq!(groups[1]["rows"][0]["line"], 2);
        assert_eq!(
            groups[0]["rows"][0]["text"].as_str(),
            Some(first_row.as_str())
        );
        assert_eq!(
            groups[1]["rows"][0]["text"].as_str(),
            Some(later_row.as_str())
        );
        assert!(serialized_len(&metadata["display"]["groups"]) <= MAX_CODING_GREP_DISPLAY_BYTES);
        assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
    }

    /// Keep all Read projections on the same complete-row prefix after capping.
    #[test]
    fn coding_policy_synchronizes_read_rows_and_is_idempotent() {
        let result = json!({
            "title": "notes.txt",
            "output": "<content>\n1#AA:one\n2#BB:two\n3#CC:three\n</content>",
            "content": "one\ntwo",
            "metadata": {
                "nextOffset": 4,
                "display": {
                    "type": "file",
                    "text": "one\ntwo\nthree",
                    "lineStart": 1,
                    "lineEnd": 3,
                    "totalLines": 4,
                    "truncated": false
                }
            }
        });

        let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
        assert_eq!(capped["content"], "one\ntwo");
        assert_eq!(capped["metadata"]["display"]["text"], "one\ntwo");
        assert_eq!(capped["metadata"]["display"]["lineEnd"], 2);
        assert_eq!(capped["metadata"]["nextOffset"], 3);
        let output = capped["output"].as_str().expect("bounded Read output");
        assert!(output.contains("1#AA:one\n2#BB:two"));
        assert!(!output.contains("3#CC:three"));
        assert!(output.contains("Showing lines 1-2 of 4"));
        assert!(output.contains("Use offset=3 to continue"));

        let capped_again = cap_tool_output_with_policy(capped.clone(), ToolResultPolicy::Coding);
        assert_eq!(capped_again, capped);
    }

    /// Keep the hidden legacy numbered Read wrapper synchronized after capping.
    #[test]
    fn coding_policy_synchronizes_legacy_read_rows() {
        let result = json!({
            "title": "notes.txt",
            "output": "<path>notes.txt</path>\n<type>file</type>\n<content>\n1: one\n2: two\n3: three\n\n(End of file - total 4 lines)\n</content>",
            "content": "one\ntwo",
            "metadata": {
                "nextOffset": null,
                "display": {
                    "type": "file",
                    "text": "one\ntwo\nthree",
                    "lineStart": 1,
                    "lineEnd": 3,
                    "totalLines": 4,
                    "truncated": false
                }
            }
        });

        let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
        let output = capped["output"].as_str().expect("legacy Read output");
        assert!(output.contains("1: one\n2: two"));
        assert!(!output.contains("3: three"));
        assert!(output.contains("(Showing lines 1-2 of 4"));
        assert_eq!(capped["content"], "one\ntwo");
        assert_eq!(capped["metadata"]["display"]["lineEnd"], 2);
        assert_eq!(capped["metadata"]["nextOffset"], 3);
    }

    /// Charge serialized array delimiters before accepting a complete Grep row.
    #[test]
    fn grep_row_budget_omits_a_whole_row_that_only_fits_without_its_container() {
        let empty_row = json!({"text": ""});
        let payload_bytes = MAX_CODING_GREP_DISPLAY_BYTES - serialized_len(&empty_row);
        let row = json!({"text": "x".repeat(payload_bytes)});
        assert_eq!(serialized_len(&row), MAX_CODING_GREP_DISPLAY_BYTES);
        let mut facts = TruncationFacts::default();

        let bounded = bound_grep_rows(json!([row]), &mut facts);

        assert_eq!(bounded, json!([]));
        assert!(facts.rows);
        assert!(serialized_len(&bounded) <= MAX_CODING_GREP_DISPLAY_BYTES);
    }

    /// Reject malformed Grep groups atomically instead of slicing their JSON.
    #[test]
    fn grep_group_budget_drops_malformed_groups_atomically() {
        let malformed = Value::String("x".repeat(MAX_CODING_GREP_DISPLAY_BYTES));
        let mut facts = TruncationFacts::default();

        let bounded = bound_grep_groups(json!([malformed]), &mut facts);

        assert_eq!(bounded, json!([]));
        assert!(facts.groups);
        assert!(facts.display);
        assert!(serialized_len(&bounded) <= MAX_CODING_GREP_DISPLAY_BYTES);
    }

    /// Omit an oversized artifact path atomically instead of publishing a slice.
    #[test]
    fn coding_policy_never_slices_output_path() {
        let output_path = "p".repeat(MAX_CODING_OUTPUT_PATH_BYTES);
        let capped = cap_tool_output_with_policy(
            json!({
                "title": "bash",
                "output": "truncated",
                "metadata": {"outputPath": output_path}
            }),
            ToolResultPolicy::Coding,
        );

        assert!(capped["metadata"].get("outputPath").is_none());
        assert_eq!(capped["metadata"]["metadataTruncated"], true);
        assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
    }

    /// Keep raw Read bytes, display text, offsets, and notices synchronized.
    #[test]
    fn coding_policy_refits_escape_heavy_raw_read_with_complete_notice() {
        let row = "\"\\\t\u{0001}".repeat(64);
        let content = (0..400)
            .map(|_| row.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let result = json!({
            "title": "raw.txt",
            "output": content.clone(),
            "content": content.clone(),
            "metadata": {
                "nextOffset": null,
                "display": {
                    "type": "file",
                    "text": content,
                    "lineStart": 1,
                    "lineEnd": 400,
                    "totalLines": 400,
                    "truncated": false
                }
            }
        });

        let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
        let output = capped["output"].as_str().expect("raw Read output");
        let (raw, notice) = output
            .split_once("\n\n[Showing lines ")
            .expect("capped raw Read must retain its continuation notice");
        assert_eq!(capped["content"], raw);
        assert_eq!(capped["metadata"]["display"]["text"], raw);
        let line_end = capped["metadata"]["display"]["lineEnd"]
            .as_u64()
            .expect("lineEnd");
        assert_eq!(u64::try_from(raw.split('\n').count()).unwrap(), line_end);
        assert_eq!(capped["metadata"]["nextOffset"], line_end + 1);
        assert!(notice.contains(&format!("Use offset={} to continue", line_end + 1)));
        assert!(serialized_len(&capped["output"]) <= MAX_CODING_OUTPUT_BYTES);
        assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
    }
}
