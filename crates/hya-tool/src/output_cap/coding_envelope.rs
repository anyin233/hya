use serde_json::{Map, Value};

use super::grep_budget::{bound_grep_groups, bound_grep_rows};
use super::json_budget::{
    bound_json_value, bound_line_string, bound_string, serialized_len, shrink_value_to_budget,
};
use super::read_views::{bound_read_output, synchronize_read_views};
use super::{
    MAX_CODING_ATTACHMENT_BYTES, MAX_CODING_ATTACHMENT_ENTRIES, MAX_CODING_DIAGNOSTIC_PATH_BYTES,
    MAX_CODING_DIAGNOSTIC_ROW_BYTES, MAX_CODING_DIAGNOSTIC_ROWS, MAX_CODING_DIFF_BYTES,
    MAX_CODING_DISPLAY_BYTES, MAX_CODING_ENVELOPE_BYTES, MAX_CODING_GREP_DISPLAY_BYTES,
    MAX_CODING_OUTPUT_BYTES, MAX_CODING_OUTPUT_PATH_BYTES, MAX_CODING_TITLE_BYTES,
    MAX_GENERIC_METADATA_ITEMS, MAX_SMALL_METADATA_STRING_BYTES, TruncationFacts, TruncationKind,
    cap_tool_output, serialized_string_len,
};

/// Bound one coding envelope while retaining semantic fields needed by the TUI.
pub(super) fn cap_coding_result(output: Value, include_diff: bool) -> Value {
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
            | "still_running"
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
            | "deadlineCapped"
            | "pgid"
            | "stillRunning"
            | "exitPath"
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
        "outputPath" | "exitPath" => {
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
            | "deadlineCapped"
            | "pgid"
            | "stillRunning"
            | "exitPath"
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

/// Whether the envelope without its attachment array fits
/// [`MAX_CODING_ENVELOPE_BYTES`]; the array has its own budget.
fn envelope_fits(envelope: &Map<String, Value>) -> bool {
    let rest: std::collections::BTreeMap<&String, &Value> = envelope
        .iter()
        .filter(|(key, _)| key.as_str() != "attachments")
        .collect();
    serialized_len(&rest) <= MAX_CODING_ENVELOPE_BYTES
}

/// Keep the approved envelope, apart from its separately bounded attachments,
/// below one serialized-byte hard bound.
fn enforce_envelope_bound(envelope: &mut Map<String, Value>, facts: &mut TruncationFacts) {
    if envelope_fits(envelope) {
        return;
    }

    for key in ["matches", "paths", "files"] {
        if envelope.contains_key(key) {
            if let Some(value) = envelope.get_mut(key) {
                shrink_value_to_budget(value, 8 * 1024);
            }
            facts.display = true;
            refresh_envelope_flags(envelope, facts);
            if envelope_fits(envelope) {
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
            if envelope_fits(envelope) {
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
            if envelope_fits(envelope) {
                return;
            }
        }
    }
    for key in ["matches", "paths", "files", "stdout", "stderr", "content"] {
        if envelope.remove(key).is_some() {
            facts.display = true;
            refresh_envelope_flags(envelope, facts);
            if envelope_fits(envelope) {
                return;
            }
        }
    }

    facts.envelope = true;
    envelope.retain(|key, _| {
        matches!(
            key.as_str(),
            "title" | "output" | "metadata" | "attachments"
        )
    });
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
