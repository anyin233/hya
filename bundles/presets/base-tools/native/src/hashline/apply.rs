//! Pure hashline edit preparation and application.
//!
//! Operation shape, anchor validation, and span semantics are adapted from
//! `pi-hashline-edit` 0.8.3 at git head
//! `ba7db9943d0f58499b24c1f6bd64722580f772a5` (MIT, tarball SHA-1
//! `8985f24c3493be375cc225a5522ed54de8daabc9`). This module does not perform
//! I/O, serialization, permission checks, or logging. Its errors are bounded
//! and never include live file contents.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::HashlineError;
use super::hash::{
    Anchor, DEFAULT_HASH_LENGTH, NIBBLE_STR, bare_hash_prefix, compute_changed_line_range,
    compute_hash_from_context, compute_line_hash, hint_has_signal, hint_matches_line,
    is_fuzzy_equivalent_line, normalize_line_endings, parse_anchor,
};

const CANDIDATE_TOTAL_LIMIT: usize = 8;
const CANDIDATE_PER_ANCHOR_LIMIT: usize = 3;

/// One normalized hashline edit operation with already-parsed anchors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HashlineEdit {
    /// Replace one line or an inclusive range.
    Replace {
        /// Inclusive range start anchor.
        pos: Anchor,
        /// Optional inclusive range end anchor.
        end: Option<Anchor>,
        /// Literal replacement lines.
        lines: Vec<String>,
    },
    /// Insert lines after an optional anchor.
    Append {
        /// Optional insertion anchor; omitted means EOF.
        pos: Option<Anchor>,
        /// Literal inserted lines.
        lines: Vec<String>,
    },
    /// Insert lines before an optional anchor.
    Prepend {
        /// Optional insertion anchor; omitted means BOF.
        pos: Option<Anchor>,
        /// Literal inserted lines.
        lines: Vec<String>,
    },
    /// Replace one exact unique text occurrence.
    ReplaceText {
        /// Exact old text, normalized to LF.
        old_text: String,
        /// Replacement text, normalized to LF.
        new_text: String,
    },
}

/// Plain Rust edit request returned after JSON dialect normalization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EditRequest {
    /// Caller-provided path spelling retained for adapter diagnostics.
    pub(crate) path: String,
    /// Strict parsed operations applied against one original snapshot.
    pub(crate) edits: Vec<HashlineEdit>,
}

/// One operation that resolved to identical existing content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NoopEdit {
    /// Zero-based operation index in the request.
    pub(crate) edit_index: usize,
    /// Bounded anchor or operation location description.
    pub(crate) location: String,
    /// Existing content for display metadata, bounded by the adapter later.
    pub(crate) current_content: String,
}

/// Pure application result for a complete normalized request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ApplyOutcome {
    /// Resulting normalized LF text.
    pub(crate) content: String,
    /// First changed one-based result line, if anything changed.
    pub(crate) first_changed_line: Option<usize>,
    /// Last changed one-based result line, if anything changed.
    pub(crate) last_changed_line: Option<usize>,
    /// Non-fatal duplicate/boundary/fuzzy warnings.
    pub(crate) warnings: Vec<String>,
    /// Edits that were valid but byte-identical no-ops.
    pub(crate) noop_edits: Vec<NoopEdit>,
}

/// Convert a generic JSON value into a bounded hashline error.
fn error(code: &'static str, message: impl Into<String>) -> HashlineError {
    HashlineError::new(code, message)
}

/// Return an optional string field while distinguishing wrong JSON types.
fn optional_string(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Option<String>, HashlineError> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(|value| Some(value.to_owned()))
        .ok_or_else(|| {
            error(
                "E_BAD_REQUEST",
                format!("{context} field \"{key}\" must be a string."),
            )
        })
}

/// Return an optional string-array field while distinguishing wrong JSON types.
fn optional_lines(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Option<Vec<String>>, HashlineError> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    let Some(array) = value.as_array() else {
        return Err(error(
            "E_BAD_REQUEST",
            format!("{context} field \"{key}\" must be a string array."),
        ));
    };
    let mut lines = Vec::with_capacity(array.len());
    for item in array {
        let Some(line) = item.as_str() else {
            return Err(error(
                "E_BAD_REQUEST",
                format!("{context} field \"{key}\" must be a string array."),
            ));
        };
        lines.push(line.to_owned());
    }
    Ok(Some(lines))
}

/// Normalize a text-replace alias pair and reject mixed or incomplete forms.
fn text_pair(
    object: &Map<String, Value>,
    context: &str,
) -> Result<Option<(String, String)>, HashlineError> {
    let has_old_camel = object.contains_key("oldText");
    let has_new_camel = object.contains_key("newText");
    let has_old_snake = object.contains_key("old_text");
    let has_new_snake = object.contains_key("new_text");
    let has_camel = has_old_camel || has_new_camel;
    let has_snake = has_old_snake || has_new_snake;
    if has_camel && has_snake {
        return Err(error(
            "E_BAD_REQUEST",
            format!("{context} cannot mix oldText/newText and old_text/new_text."),
        ));
    }
    if !has_camel && !has_snake {
        return Ok(None);
    }
    let (old_key, new_key) = if has_camel {
        ("oldText", "newText")
    } else {
        ("old_text", "new_text")
    };
    let old = optional_string(object, old_key, context)?.ok_or_else(|| {
        error(
            "E_BAD_OP",
            format!("{context} requires both \"{old_key}\" and \"{new_key}\"."),
        )
    })?;
    let new = optional_string(object, new_key, context)?.ok_or_else(|| {
        error(
            "E_BAD_OP",
            format!("{context} requires both \"{old_key}\" and \"{new_key}\"."),
        )
    })?;
    Ok(Some((
        normalize_line_endings(&old),
        normalize_line_endings(&new),
    )))
}

/// Parse one raw edit item into a strict operation with parsed anchors.
fn parse_edit_item(value: &Value, index: usize) -> Result<HashlineEdit, HashlineError> {
    let context = format!("Edit {index}");
    let Some(object) = value.as_object() else {
        return Err(error(
            "E_BAD_REQUEST",
            format!("{context} must be an object."),
        ));
    };
    let allowed = ["op", "pos", "end", "lines", "oldText", "newText"];
    let allowed = allowed.into_iter().collect::<BTreeSet<_>>();
    let unknown_total = object
        .keys()
        .filter(|key| !allowed.contains(key.as_str()))
        .count();
    let unknown = object
        .keys()
        .filter(|key| !allowed.contains(key.as_str()))
        .take(16)
        .map(|key| super::bound_utf8_text(key, 128))
        .collect::<Vec<_>>();
    if unknown_total > 0 {
        let omitted = unknown_total.saturating_sub(unknown.len());
        let suffix = if omitted == 0 {
            String::new()
        } else {
            format!(", ... ({omitted} more)")
        };
        return Err(error(
            "E_BAD_REQUEST",
            format!(
                "{context} contains unknown or unsupported fields: {}{suffix}.",
                unknown.join(", ")
            ),
        ));
    }

    let op = optional_string(object, "op", &context)?;
    let pair = text_pair(object, &context)?;
    let op = match (op, pair.as_ref()) {
        (Some(op), _) => op,
        (None, Some(_)) => "replace_text".to_string(),
        (None, None) => {
            return Err(error(
                "E_BAD_OP",
                format!("{context} requires an \"op\" string."),
            ));
        }
    };

    match op.as_str() {
        "replace" => {
            if pair.is_some() {
                return Err(error(
                    "E_BAD_OP",
                    format!("{context} with op \"replace\" does not support oldText/newText."),
                ));
            }
            let pos = optional_string(object, "pos", &context)?.ok_or_else(|| {
                error(
                    "E_BAD_OP",
                    format!("{context} with op \"replace\" requires a \"pos\" anchor string."),
                )
            })?;
            let end = optional_string(object, "end", &context)?;
            let lines = optional_lines(object, "lines", &context)?.ok_or_else(|| {
                error(
                    "E_BAD_REQUEST",
                    format!("{context} requires a \"lines\" field."),
                )
            })?;
            assert_no_display_prefixes(&lines)?;
            Ok(HashlineEdit::Replace {
                pos: parse_anchor(&pos, DEFAULT_HASH_LENGTH)?,
                end: end
                    .as_deref()
                    .map(|reference| parse_anchor(reference, DEFAULT_HASH_LENGTH))
                    .transpose()?,
                lines,
            })
        }
        "append" | "prepend" => {
            if pair.is_some() {
                return Err(error(
                    "E_BAD_OP",
                    format!("{context} with op \"{op}\" does not support oldText/newText."),
                ));
            }
            if object.contains_key("end") {
                return Err(error(
                    "E_BAD_OP",
                    format!("{context} with op \"{op}\" does not support \"end\"."),
                ));
            }
            let pos = optional_string(object, "pos", &context)?;
            let lines = optional_lines(object, "lines", &context)?.ok_or_else(|| {
                error(
                    "E_BAD_REQUEST",
                    format!("{context} requires a \"lines\" field."),
                )
            })?;
            assert_no_display_prefixes(&lines)?;
            let pos = pos
                .as_deref()
                .map(|reference| parse_anchor(reference, DEFAULT_HASH_LENGTH))
                .transpose()?;
            if op == "append" {
                Ok(HashlineEdit::Append { pos, lines })
            } else {
                Ok(HashlineEdit::Prepend { pos, lines })
            }
        }
        "replace_text" => {
            if object.contains_key("pos")
                || object.contains_key("end")
                || object.contains_key("lines")
            {
                return Err(error(
                    "E_BAD_OP",
                    format!("{context} with op \"replace_text\" only supports oldText/newText."),
                ));
            }
            let Some((old_text, new_text)) = pair else {
                return Err(error(
                    "E_BAD_OP",
                    format!("{context} with op \"replace_text\" requires oldText/newText."),
                ));
            };
            Ok(HashlineEdit::ReplaceText { old_text, new_text })
        }
        other => {
            let bounded = super::bound_utf8_text(other, 128);
            Err(error(
                "E_BAD_OP",
                format!(
                    "Edit {index} uses unknown op \"{bounded}\". Expected replace, append, prepend, or replace_text."
                ),
            ))
        }
    }
}

/// Reject hashline display prefixes in literal replacement payload lines.
fn assert_no_display_prefixes(lines: &[String]) -> Result<(), HashlineError> {
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if is_display_prefix(line) || is_diff_minus_prefix(line) {
            return Err(error(
                "E_INVALID_PATCH",
                format!(
                    "lines must contain literal file content, not rendered LINE#HASH or diff +/- prefixes (offending line length: {}).",
                    line.chars().count()
                ),
            ));
        }
    }
    Ok(())
}

/// Detect an unambiguous full `LINE#HASH:` or `#HASH:` display prefix.
fn is_display_prefix(line: &str) -> bool {
    let mut rest = line.trim_start();
    if let Some(after_plus) = rest.strip_prefix('+') {
        rest = after_plus.trim_start();
    }
    let after_hash = if let Some(after_hash) = rest.strip_prefix('#') {
        after_hash.trim_start()
    } else {
        let mut digits = 0usize;
        while rest.as_bytes().get(digits).is_some_and(u8::is_ascii_digit) {
            digits += 1;
        }
        if digits == 0 {
            return false;
        }
        let after_number = rest[digits..].trim_start();
        let Some(after_number) = after_number.strip_prefix('#') else {
            return false;
        };
        after_number.trim_start()
    };
    has_supported_hash_colon(after_hash)
}

/// Detect a supported-width hash token followed immediately by a colon.
fn has_supported_hash_colon(rest: &str) -> bool {
    let mut hash_len = 0usize;
    while rest
        .as_bytes()
        .get(hash_len)
        .is_some_and(|byte| NIBBLE_STR.as_bytes().contains(byte))
    {
        hash_len += 1;
    }
    (2..=4).contains(&hash_len) && rest.as_bytes().get(hash_len) == Some(&b':')
}

/// Detect the pinned diff-minus display prefix shape.
fn is_diff_minus_prefix(line: &str) -> bool {
    let Some(mut rest) = line.strip_prefix('-') else {
        return false;
    };
    rest = rest.trim_start();
    let mut digits = 0usize;
    for character in rest.chars() {
        if !character.is_ascii_digit() {
            break;
        }
        digits += character.len_utf8();
    }
    if digits == 0 {
        return false;
    }
    let whitespace = &rest[digits..];
    whitespace.chars().take(4).count() == 4 && whitespace.chars().take(4).all(char::is_whitespace)
}

/// Normalize all supported top-level edit dialects into strict operations.
pub(super) fn parse_edit_request(input: Value) -> Result<EditRequest, HashlineError> {
    let Some(object) = input.as_object() else {
        return Err(error("E_BAD_REQUEST", "Edit request must be an object."));
    };
    let allowed = [
        "path",
        "file_path",
        "edits",
        "oldText",
        "newText",
        "old_text",
        "new_text",
    ];
    let allowed = allowed.into_iter().collect::<BTreeSet<_>>();
    let unknown_total = object
        .keys()
        .filter(|key| !allowed.contains(key.as_str()))
        .count();
    let unknown = object
        .keys()
        .filter(|key| !allowed.contains(key.as_str()))
        .take(16)
        .map(|key| super::bound_utf8_text(key, 128))
        .collect::<Vec<_>>();
    if unknown_total > 0 {
        let omitted = unknown_total.saturating_sub(unknown.len());
        let suffix = if omitted == 0 {
            String::new()
        } else {
            format!(", ... ({omitted} more)")
        };
        return Err(error(
            "E_BAD_REQUEST",
            format!(
                "Edit request contains unknown or unsupported fields: {}{suffix}.",
                unknown.join(", ")
            ),
        ));
    }

    let has_path = object.contains_key("path");
    let has_file_path = object.contains_key("file_path");
    if has_path && has_file_path {
        return Err(error(
            "E_BAD_REQUEST",
            "Edit request cannot contain both path and file_path.",
        ));
    }
    let path = if has_path {
        optional_string(object, "path", "Edit request")?
    } else {
        optional_string(object, "file_path", "Edit request")?
    }
    .ok_or_else(|| {
        error(
            "E_BAD_REQUEST",
            "Edit request requires a non-empty path string.",
        )
    })?;
    if path.is_empty() {
        return Err(error(
            "E_BAD_REQUEST",
            "Edit request requires a non-empty path string.",
        ));
    }

    let top_level_pair = text_pair(object, "Edit request")?;
    let has_edits = object.contains_key("edits");
    let mut edits_values = Vec::new();
    if let Some(edits_value) = object.get("edits") {
        if let Some(array) = edits_value.as_array() {
            edits_values.extend(array.iter().cloned());
        } else if let Some(serialized) = edits_value.as_str() {
            let parsed: Value = serde_json::from_str(serialized).map_err(|_| {
                error(
                    "E_BAD_REQUEST",
                    "Edit request field \"edits\" must be an array.",
                )
            })?;
            let Some(array) = parsed.as_array() else {
                return Err(error(
                    "E_BAD_REQUEST",
                    "Edit request field \"edits\" must be an array.",
                ));
            };
            edits_values.extend(array.iter().cloned());
        } else {
            return Err(error(
                "E_BAD_REQUEST",
                "Edit request field \"edits\" must be an array.",
            ));
        }
    }

    if (!has_edits || edits_values.is_empty())
        && let Some((old_text, new_text)) = top_level_pair.as_ref()
    {
        return Ok(EditRequest {
            path,
            edits: vec![HashlineEdit::ReplaceText {
                old_text: old_text.clone(),
                new_text: new_text.clone(),
            }],
        });
    }
    if top_level_pair.is_some() {
        return Err(error(
            "E_BAD_REQUEST",
            "Top-level oldText/newText cannot be combined with non-empty edits.",
        ));
    }
    if !has_edits {
        return Err(error(
            "E_BAD_REQUEST",
            "Edit request requires an \"edits\" array.",
        ));
    }
    let edits = edits_values
        .iter()
        .enumerate()
        .map(|(index, value)| parse_edit_item(value, index))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(EditRequest { path, edits })
}

/// Hash mismatch collected before any span is applied.
struct HashMismatch {
    /// One-based stale anchor line.
    line: usize,
    /// Expected stale hash.
    expected: String,
    /// Optional copied content hint.
    text_hint: Option<String>,
}

/// Return the bounded stale-anchor error and content-free candidate hints.
fn stale_error(mismatches: &[HashMismatch], file_lines: &[String]) -> HashlineError {
    const DIAGNOSTIC_ANCHOR_LIMIT: usize = 16;
    const CANDIDATE_HINT_BYTES: usize = 512;

    let reported = mismatches
        .iter()
        .take(DIAGNOSTIC_ANCHOR_LIMIT)
        .collect::<Vec<_>>();
    let mut stale_refs = reported
        .iter()
        .map(|mismatch| format!("{}#{}", mismatch.line, mismatch.expected))
        .collect::<Vec<_>>()
        .join(", ");
    let omitted = mismatches.len().saturating_sub(reported.len());
    if omitted > 0 {
        stale_refs.push_str(&format!(", ... ({omitted} more)"));
    }
    let mut message = format!(
        "{} stale anchor{}: {stale_refs}. Re-read the file to get current anchors; keep both endpoints for range replaces.",
        mismatches.len(),
        if mismatches.len() == 1 { "" } else { "s" }
    );
    let hinted = reported.into_iter().filter(|mismatch| {
        mismatch
            .text_hint
            .as_deref()
            .is_some_and(|hint| hint.len() <= CANDIDATE_HINT_BYTES && hint_has_signal(hint))
    });
    let mut hints = Vec::new();
    let mut total = 0usize;
    for mismatch in hinted {
        let Some(text_hint) = mismatch.text_hint.as_deref() else {
            continue;
        };
        let mut match_count = 0usize;
        let mut candidate_lines = Vec::with_capacity(CANDIDATE_PER_ANCHOR_LIMIT);
        for (index, line) in file_lines.iter().enumerate() {
            if hint_matches_line(text_hint, line) {
                match_count = match_count.saturating_add(1);
                if candidate_lines.len() < CANDIDATE_PER_ANCHOR_LIMIT {
                    candidate_lines.push(index + 1);
                }
            }
        }
        if match_count == 0 {
            continue;
        }
        if total.saturating_add(match_count) > CANDIDATE_TOTAL_LIMIT {
            hints.push(format!(
                "{match_count} similar lines found for {}#{} — re-read to disambiguate",
                mismatch.line, mismatch.expected
            ));
            continue;
        }
        if match_count > CANDIDATE_PER_ANCHOR_LIMIT {
            hints.push(format!(
                "{match_count} similar lines found for {}#{} — re-read to disambiguate",
                mismatch.line, mismatch.expected
            ));
            total = total.saturating_add(match_count);
            continue;
        }
        total = total.saturating_add(match_count);
        for line_number in candidate_lines {
            let hash = compute_line_hash(file_lines, line_number - 1);
            hints.push(format!(
                "{line_number}#{hash} (candidate for {}#{})",
                mismatch.line, mismatch.expected
            ));
        }
    }
    if !hints.is_empty() {
        message.push_str("\nDid you mean (content-matched candidates for stale anchors):\n");
        message.push_str(
            &hints
                .iter()
                .map(|hint| format!("  {hint}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    HashlineError::with_hints("E_STALE_ANCHOR", message, hints)
}

/// Describe an operation without echoing literal file content.
fn describe_edit(edit: &HashlineEdit) -> String {
    match edit {
        HashlineEdit::Replace { pos, end, .. } => end.as_ref().map_or_else(
            || format!("replace {}#{}", pos.line, pos.hash),
            |end| {
                format!(
                    "replace {}#{}-{}#{}",
                    pos.line, pos.hash, end.line, end.hash
                )
            },
        ),
        HashlineEdit::Append { pos: Some(pos), .. } => {
            format!("append after {}#{}", pos.line, pos.hash)
        }
        HashlineEdit::Append { pos: None, .. } => "append at EOF".to_string(),
        HashlineEdit::Prepend { pos: Some(pos), .. } => {
            format!("prepend before {}#{}", pos.line, pos.hash)
        }
        HashlineEdit::Prepend { pos: None, .. } => "prepend at BOF".to_string(),
        HashlineEdit::ReplaceText { old_text, .. } => {
            format!("replace_text of {} characters", old_text.chars().count())
        }
    }
}

/// Source line index used by every operation in one application pass.
struct LineIndex {
    /// Split lines, including a terminal empty sentinel when present.
    file_lines: Vec<String>,
    /// Byte offset for every split line.
    line_starts: Vec<usize>,
    /// Whether source content ends with LF.
    has_terminal_newline: bool,
    /// Model-visible count excluding a terminal sentinel.
    visible_line_count: usize,
}

/// Build one line index without allocating a second joined document.
fn build_line_index(content: &str) -> LineIndex {
    let file_lines = content.split('\n').map(str::to_owned).collect::<Vec<_>>();
    let mut line_starts = Vec::with_capacity(file_lines.len());
    let mut offset = 0usize;
    for (index, line) in file_lines.iter().enumerate() {
        line_starts.push(offset);
        offset = offset.saturating_add(line.len());
        if index + 1 < file_lines.len() {
            offset = offset.saturating_add(1);
        }
    }
    let has_terminal_newline = content.ends_with('\n');
    let visible_line_count = if has_terminal_newline {
        file_lines.len().saturating_sub(1)
    } else {
        file_lines.len()
    };
    LineIndex {
        file_lines,
        line_starts,
        has_terminal_newline,
        visible_line_count,
    }
}

/// Validate one anchor against the same pre-edit line index.
fn validate_anchor(
    anchor: &Anchor,
    line_index: &LineIndex,
    mismatches: &mut Vec<HashMismatch>,
    warnings: &mut Vec<String>,
    accepted_fuzzy: &mut BTreeSet<String>,
) -> Result<bool, HashlineError> {
    if anchor.line == 0 || anchor.line > line_index.file_lines.len() {
        return Err(error(
            "E_RANGE_OOB",
            format!(
                "Line {} does not exist (file has {} lines)",
                anchor.line, line_index.visible_line_count
            ),
        ));
    }
    let Some(line) = line_index.file_lines.get(anchor.line - 1) else {
        return Err(error(
            "E_RANGE_OOB",
            format!("Line {} does not exist", anchor.line),
        ));
    };
    let actual = compute_line_hash(&line_index.file_lines, anchor.line - 1);
    if actual == anchor.hash {
        if anchor
            .text_hint
            .as_deref()
            .is_some_and(|hint| !hint_matches_line(hint, line))
        {
            mismatches.push(HashMismatch {
                line: anchor.line,
                expected: anchor.hash.clone(),
                text_hint: anchor.text_hint.clone(),
            });
            return Ok(false);
        }
        return Ok(true);
    }
    if let Some(hint) = anchor.text_hint.as_deref() {
        let previous = anchor
            .line
            .checked_sub(2)
            .and_then(|position| line_index.file_lines.get(position))
            .map_or("", String::as_str);
        let next = line_index
            .file_lines
            .get(anchor.line)
            .map_or("", String::as_str);
        let hinted_hash = compute_hash_from_context(previous, hint, next);
        if hinted_hash == anchor.hash && is_fuzzy_equivalent_line(hint, line) {
            let key = format!("{}:{}:{}", anchor.line, anchor.hash, hint);
            if accepted_fuzzy.insert(key) {
                warnings.push(format!(
                    "Accepted fuzzy anchor validation at line {}: exact hash mismatched, but copied line content matched after whitespace/Unicode normalization.",
                    anchor.line
                ));
            }
            return Ok(true);
        }
    }
    mismatches.push(HashMismatch {
        line: anchor.line,
        expected: anchor.hash.clone(),
        text_hint: anchor.text_hint.clone(),
    });
    Ok(false)
}

/// Warn about duplicate content adjacent to an append or prepend boundary.
fn warn_duplicate_insert(edit: &HashlineEdit, line_index: &LineIndex, warnings: &mut Vec<String>) {
    let (op, pos, lines) = match edit {
        HashlineEdit::Append { pos, lines } => ("append", pos.as_ref(), lines),
        HashlineEdit::Prepend { pos, lines } => ("prepend", pos.as_ref(), lines),
        _ => return,
    };
    if lines.is_empty() {
        return;
    }
    let n = lines.len();
    let (start, end) = if op == "append" {
        if let Some(pos) = pos {
            (pos.line, pos.line.saturating_add(n))
        } else {
            (
                line_index.visible_line_count.saturating_sub(n),
                line_index.visible_line_count,
            )
        }
    } else if let Some(pos) = pos {
        (
            pos.line.saturating_sub(1).saturating_sub(n),
            pos.line.saturating_sub(1),
        )
    } else {
        (0, n)
    };
    if start > end || end > line_index.visible_line_count {
        return;
    }
    let Some(adjacent) = line_index.file_lines.get(start..end) else {
        return;
    };
    if adjacent.len() != n {
        return;
    }
    if lines
        .iter()
        .zip(adjacent)
        .all(|(left, right)| left.trim() == right.trim())
        && lines
            .iter()
            .any(|line| line.chars().any(char::is_alphanumeric))
    {
        warnings.push(format!(
            "Potential duplicate insert at {}: inserted lines are identical to adjacent lines; do not resend a previous successful edit.",
            describe_edit(edit)
        ));
    }
}

/// Reject the terminal split sentinel for replacement operations.
fn reject_replace_sentinel(anchor: &Anchor, line_index: &LineIndex) -> Result<(), HashlineError> {
    let visible_line_count = if line_index.file_lines.len() == 1
        && line_index.file_lines.first().is_some_and(String::is_empty)
    {
        0
    } else {
        line_index.visible_line_count
    };
    if anchor.line == 0 || anchor.line > visible_line_count {
        return Err(error(
            "E_RANGE_OOB",
            format!(
                "Line {} does not exist (file has {} lines)",
                anchor.line, visible_line_count
            ),
        ));
    }
    Ok(())
}

/// Validate all anchors and operation-specific boundaries before resolving spans.
fn validate_edits(
    edits: &[HashlineEdit],
    line_index: &LineIndex,
    warnings: &mut Vec<String>,
) -> Result<(), HashlineError> {
    let mut mismatches = Vec::new();
    let mut accepted_fuzzy = BTreeSet::new();
    for edit in edits {
        match edit {
            HashlineEdit::Replace { pos, end, lines } => {
                reject_replace_sentinel(pos, line_index)?;
                if let Some(end) = end {
                    reject_replace_sentinel(end, line_index)?;
                    if pos.line > end.line {
                        return Err(error(
                            "E_BAD_OP",
                            format!(
                                "Range start line {} must be <= end line {}",
                                pos.line, end.line
                            ),
                        ));
                    }
                    let start_ok = validate_anchor(
                        pos,
                        line_index,
                        &mut mismatches,
                        warnings,
                        &mut accepted_fuzzy,
                    )?;
                    let end_ok = validate_anchor(
                        end,
                        line_index,
                        &mut mismatches,
                        warnings,
                        &mut accepted_fuzzy,
                    )?;
                    if !start_ok || !end_ok {
                        continue;
                    }
                } else if !validate_anchor(
                    pos,
                    line_index,
                    &mut mismatches,
                    warnings,
                    &mut accepted_fuzzy,
                )? {
                    continue;
                }
                if end.is_none() && lines.len() > 1 {
                    warnings.push(format!(
                        "Single-anchor replace at {} supplied {} replacement lines; add end for a range.",
                        describe_edit(edit), lines.len()
                    ));
                }
                let end_line = end.as_ref().map_or(pos.line, |anchor| anchor.line);
                if let Some(next_line) = line_index.file_lines.get(end_line)
                    && lines.last().is_some_and(|line| {
                        line.chars().any(char::is_alphanumeric) && line.trim() == next_line.trim()
                    })
                {
                    warnings.push(format!(
                        "Potential boundary duplication after {}: replacement ends with the next surviving line.",
                        describe_edit(edit)
                    ));
                }
                if pos.line > 1 {
                    let previous = line_index.file_lines.get(pos.line - 2);
                    if lines
                        .first()
                        .is_some_and(|line| line.chars().any(char::is_alphanumeric))
                        && previous.is_some_and(|previous| lines[0].trim() == previous.trim())
                    {
                        warnings.push(format!(
                            "Potential boundary duplication before {}: replacement starts with the preceding surviving line.",
                            describe_edit(edit)
                        ));
                    }
                }
            }
            HashlineEdit::Append { pos, lines } => {
                let anchor_ok = if let Some(pos) = pos {
                    validate_anchor(
                        pos,
                        line_index,
                        &mut mismatches,
                        warnings,
                        &mut accepted_fuzzy,
                    )?
                } else {
                    true
                };
                if lines.is_empty() && anchor_ok {
                    return Err(error(
                        "E_BAD_OP",
                        "Append with empty lines payload. Provide content to insert or remove the edit.",
                    ));
                }
                warn_duplicate_insert(edit, line_index, warnings);
            }
            HashlineEdit::Prepend { pos, lines } => {
                let anchor_ok = if let Some(pos) = pos {
                    validate_anchor(
                        pos,
                        line_index,
                        &mut mismatches,
                        warnings,
                        &mut accepted_fuzzy,
                    )?
                } else {
                    true
                };
                if lines.is_empty() && anchor_ok {
                    return Err(error(
                        "E_BAD_OP",
                        "Prepend with empty lines payload. Provide content to insert or remove the edit.",
                    ));
                }
                warn_duplicate_insert(edit, line_index, warnings);
            }
            HashlineEdit::ReplaceText { .. } => {}
        }
    }
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(stale_error(&mismatches, &line_index.file_lines))
    }
}

/// Character-level span resolved against the original text.
struct ResolvedSpan {
    /// Replacement or insertion kind.
    kind: SpanKind,
    /// Original operation index.
    index: usize,
    /// Content-free operation label.
    label: String,
    /// Inclusive byte start in original content.
    start: usize,
    /// Exclusive byte end in original content.
    end: usize,
    /// Literal replacement bytes.
    replacement: String,
    /// Logical insertion boundary for conflict checks.
    boundary: Option<usize>,
    /// Special empty-origin newline behavior.
    insert_mode: Option<InsertMode>,
}

/// Span category used by conflict and ordering checks.
#[derive(Clone, Copy, Eq, PartialEq)]
enum SpanKind {
    /// Replaces a non-empty original range.
    Replace,
    /// Inserts at a zero-width original boundary.
    Insert,
}

/// Empty-document insertion mode copied from the source semantics.
#[derive(Clone, Copy, Eq, PartialEq)]
enum InsertMode {
    /// Append on an empty origin.
    AppendEmptyOrigin,
    /// Prepend on an empty origin.
    PrependEmptyOrigin,
}

/// Return a bounded preview descriptor for a replace-text operation.
fn replace_text_location(old_text: &str) -> String {
    format!("replace_text of {} characters", old_text.chars().count())
}

/// Find one exact non-empty text match and reject overlap/multiplicity.
fn find_exact_unique_text_match(
    content: &str,
    old_text: &str,
) -> Result<(usize, usize), HashlineError> {
    if old_text.is_empty() {
        return Err(error(
            "E_BAD_OP",
            "replace_text requires non-empty oldText.",
        ));
    }
    if old_text.len() > content.len() {
        return Err(error(
            "E_NO_MATCH",
            "replace_text found no exact unique match in the current file.",
        ));
    }
    let Some(first_relative) = content.find(old_text) else {
        return Err(error(
            "E_NO_MATCH",
            "replace_text found no exact unique match in the current file.",
        ));
    };
    let first = first_relative;
    let next_character_len = content[first..].chars().next().map_or(0, char::len_utf8);
    let from = first.saturating_add(next_character_len);
    if from <= content.len().saturating_sub(old_text.len())
        && let Some(second_relative) = content[from..].find(old_text)
    {
        let second = from.saturating_add(second_relative);
        if second.saturating_sub(first) < old_text.len() {
            return Err(error(
                "E_MULTI_MATCH",
                "replace_text found overlapping exact matches; re-read and use hashline edits.",
            ));
        }
        return Err(error(
            "E_MULTI_MATCH",
            "replace_text found multiple exact matches in the current file. Re-read and use hashline edits.",
        ));
    }
    Ok((first, first.saturating_add(old_text.len())))
}

/// Compute the logical insertion boundary of one append/prepend operation.
fn insertion_boundary(edit: &HashlineEdit, line_index: &LineIndex) -> usize {
    match edit {
        HashlineEdit::Prepend { pos, .. } => {
            pos.as_ref().map_or(0, |pos| pos.line.saturating_sub(1))
        }
        HashlineEdit::Append { pos, .. } => {
            pos.as_ref().map_or(line_index.visible_line_count, |pos| {
                if line_index.has_terminal_newline && pos.line == line_index.file_lines.len() {
                    line_index.visible_line_count
                } else {
                    pos.line
                }
            })
        }
        _ => 0,
    }
}

/// Resolve one validated operation into an original-content span.
fn resolve_edit_span(
    edit: &HashlineEdit,
    index: usize,
    content: &str,
    line_index: &LineIndex,
    noops: &mut Vec<NoopEdit>,
) -> Result<ResolvedSpan, HashlineError> {
    let label = describe_edit(edit);
    match edit {
        HashlineEdit::Replace { pos, end, lines } => {
            let start_line = pos.line;
            let end_line = end.as_ref().map_or(start_line, |anchor| anchor.line);
            let Some(original_lines) = line_index.file_lines.get(start_line - 1..end_line) else {
                return Err(error("E_RANGE_OOB", format!("Invalid range for {label}")));
            };
            if original_lines.len() == lines.len()
                && original_lines
                    .iter()
                    .zip(lines)
                    .all(|(left, right)| left == right)
            {
                noops.push(NoopEdit {
                    edit_index: index,
                    location: format!("{}#{}", pos.line, pos.hash),
                    current_content: original_lines.join("\n"),
                });
                return Ok(ResolvedSpan {
                    kind: SpanKind::Insert,
                    index,
                    label,
                    start: 0,
                    end: 0,
                    replacement: String::new(),
                    boundary: None,
                    insert_mode: None,
                });
            }
            let start = *line_index
                .line_starts
                .get(start_line - 1)
                .ok_or_else(|| error("E_RANGE_OOB", format!("Invalid range for {label}")))?;
            let start = if lines.is_empty() && end_line == line_index.file_lines.len() {
                start.saturating_sub(1)
            } else {
                start
            };
            let end = if lines.is_empty() {
                if start_line == 1 && end_line == line_index.file_lines.len() {
                    content.len()
                } else if end_line < line_index.file_lines.len() {
                    *line_index
                        .line_starts
                        .get(end_line)
                        .ok_or_else(|| error("E_RANGE_OOB", format!("Invalid range for {label}")))?
                } else {
                    line_index
                        .line_starts
                        .get(end_line - 1)
                        .copied()
                        .unwrap_or(start)
                        .saturating_add(
                            line_index
                                .file_lines
                                .get(end_line - 1)
                                .map_or(0, String::len),
                        )
                }
            } else {
                line_index
                    .line_starts
                    .get(end_line - 1)
                    .copied()
                    .unwrap_or(start)
                    .saturating_add(
                        line_index
                            .file_lines
                            .get(end_line - 1)
                            .map_or(0, String::len),
                    )
            };
            Ok(ResolvedSpan {
                kind: SpanKind::Replace,
                index,
                label,
                start,
                end,
                replacement: lines.join("\n"),
                boundary: None,
                insert_mode: None,
            })
        }
        HashlineEdit::Append { pos, lines } => {
            let inserted = lines.join("\n");
            let boundary = insertion_boundary(edit, line_index);
            if content.is_empty() {
                return Ok(ResolvedSpan {
                    kind: SpanKind::Insert,
                    index,
                    label,
                    start: 0,
                    end: 0,
                    replacement: inserted,
                    boundary: Some(boundary),
                    insert_mode: Some(InsertMode::AppendEmptyOrigin),
                });
            }
            let sentinel = line_index.has_terminal_newline
                && pos
                    .as_ref()
                    .is_some_and(|pos| pos.line == line_index.file_lines.len());
            let start = if sentinel {
                content.len()
            } else if let Some(pos) = pos {
                line_index
                    .line_starts
                    .get(pos.line - 1)
                    .copied()
                    .unwrap_or(content.len())
                    .saturating_add(
                        line_index
                            .file_lines
                            .get(pos.line - 1)
                            .map_or(0, String::len),
                    )
            } else {
                content.len()
            };
            let replacement = if sentinel || pos.is_none() && line_index.has_terminal_newline {
                format!("{inserted}\n")
            } else {
                format!("\n{inserted}")
            };
            Ok(ResolvedSpan {
                kind: SpanKind::Insert,
                index,
                label,
                start,
                end: start,
                replacement,
                boundary: Some(boundary),
                insert_mode: None,
            })
        }
        HashlineEdit::Prepend { pos, lines } => {
            let inserted = lines.join("\n");
            let boundary = insertion_boundary(edit, line_index);
            let start = pos
                .as_ref()
                .and_then(|pos| line_index.line_starts.get(pos.line - 1).copied())
                .unwrap_or(0);
            Ok(ResolvedSpan {
                kind: SpanKind::Insert,
                index,
                label,
                start,
                end: start,
                replacement: if content.is_empty() {
                    inserted
                } else {
                    format!("{inserted}\n")
                },
                boundary: Some(boundary),
                insert_mode: content.is_empty().then_some(InsertMode::PrependEmptyOrigin),
            })
        }
        HashlineEdit::ReplaceText { old_text, new_text } => {
            let (start, end) = find_exact_unique_text_match(content, old_text)?;
            if old_text == new_text {
                noops.push(NoopEdit {
                    edit_index: index,
                    location: replace_text_location(old_text),
                    current_content: old_text.clone(),
                });
                return Ok(ResolvedSpan {
                    kind: SpanKind::Insert,
                    index,
                    label,
                    start: 0,
                    end: 0,
                    replacement: String::new(),
                    boundary: None,
                    insert_mode: None,
                });
            }
            Ok(ResolvedSpan {
                kind: SpanKind::Replace,
                index,
                label,
                start,
                end,
                replacement: new_text.clone(),
                boundary: None,
                insert_mode: None,
            })
        }
    }
}

/// Return whether two resolved spans are identical and can be deduplicated.
fn same_span(left: &ResolvedSpan, right: &ResolvedSpan) -> bool {
    left.kind == right.kind
        && left.start == right.start
        && left.end == right.end
        && left.boundary == right.boundary
        && left.replacement == right.replacement
}

/// Raise a bounded conflict error naming two operation labels.
fn conflict(left: &ResolvedSpan, right: &ResolvedSpan, reason: &str) -> HashlineError {
    error(
        "E_EDIT_CONFLICT",
        format!(
            "Conflicting edits in one request: edit {} ({}) and edit {} ({}) {reason}.",
            left.index, left.label, right.index, right.label
        ),
    )
}

/// Reject overlapping replacements and insertions inside replacement ranges.
fn assert_no_conflicting_spans(spans: &[ResolvedSpan]) -> Result<(), HashlineError> {
    for left_index in 0..spans.len() {
        for right_index in left_index + 1..spans.len() {
            let left = &spans[left_index];
            let right = &spans[right_index];
            match (left.kind, right.kind) {
                (SpanKind::Insert, SpanKind::Insert) => {
                    if left.boundary == right.boundary {
                        return Err(conflict(left, right, "target the same insertion boundary"));
                    }
                }
                (SpanKind::Replace, SpanKind::Replace) => {
                    if left.start < right.end && right.start < left.end {
                        return Err(conflict(left, right, "overlap on the same original range"));
                    }
                }
                _ => {
                    let (replace, insert) = if left.kind == SpanKind::Replace {
                        (left, right)
                    } else {
                        (right, left)
                    };
                    if insert.start >= replace.start && insert.start < replace.end {
                        return Err(conflict(
                            left,
                            right,
                            "insert inside a replaced original range",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Resolve, deduplicate, conflict-check, and order all operation spans.
fn resolve_spans(
    request: &EditRequest,
    content: &str,
    line_index: &LineIndex,
    noops: &mut Vec<NoopEdit>,
) -> Result<Vec<ResolvedSpan>, HashlineError> {
    let mut spans = Vec::new();
    for (index, edit) in request.edits.iter().enumerate() {
        let span = resolve_edit_span(edit, index, content, line_index, noops)?;
        if span.start == 0
            && span.end == 0
            && span.replacement.is_empty()
            && span.boundary.is_none()
        {
            continue;
        }
        if spans.iter().any(|existing| same_span(existing, &span)) {
            continue;
        }
        spans.push(span);
    }
    assert_no_conflicting_spans(&spans)?;
    spans.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| match (left.kind, right.kind) {
                (SpanKind::Replace, SpanKind::Insert) => Ordering::Less,
                (SpanKind::Insert, SpanKind::Replace) => Ordering::Greater,
                _ => left.index.cmp(&right.index),
            })
    });
    Ok(spans)
}

/// Assemble zero-origin prepend, replacement, and append spans with the
/// source operation's deterministic newline boundaries.
fn assemble_empty_origin(spans: &[ResolvedSpan], capacity: usize) -> String {
    let prepend = spans
        .iter()
        .find(|span| span.insert_mode == Some(InsertMode::PrependEmptyOrigin));
    let append = spans
        .iter()
        .find(|span| span.insert_mode == Some(InsertMode::AppendEmptyOrigin));
    let has_replacement = spans.iter().any(|span| span.kind == SpanKind::Replace);
    let mut result = String::with_capacity(capacity.saturating_add(2));

    if let Some(span) = prepend {
        result.push_str(&span.replacement);
        if has_replacement || append.is_some() {
            result.push('\n');
        }
    }
    for span in spans {
        if span.kind == SpanKind::Replace {
            result.push_str(&span.replacement);
        }
    }
    if let Some(span) = append {
        if has_replacement {
            result.push('\n');
        }
        result.push_str(&span.replacement);
    }
    result
}

/// Assemble all non-overlapping spans in one forward pass over original text.
fn assemble(content: &str, spans: &[ResolvedSpan]) -> Result<String, HashlineError> {
    let mut removed = 0usize;
    let mut inserted = 0usize;
    for span in spans {
        removed = removed
            .checked_add(span.end.saturating_sub(span.start))
            .ok_or_else(|| {
                error(
                    "E_CAPACITY",
                    "Edit result size exceeds the supported capacity.",
                )
            })?;
        inserted = inserted
            .checked_add(span.replacement.len())
            .ok_or_else(|| {
                error(
                    "E_CAPACITY",
                    "Edit result size exceeds the supported capacity.",
                )
            })?;
    }
    let capacity = content
        .len()
        .checked_sub(removed)
        .and_then(|length| length.checked_add(inserted))
        .ok_or_else(|| {
            error(
                "E_CAPACITY",
                "Edit result size exceeds the supported capacity.",
            )
        })?;
    let mut result = String::with_capacity(capacity);
    let mut cursor = 0usize;
    if content.is_empty() && spans.iter().any(|span| span.insert_mode.is_some()) {
        return Ok(assemble_empty_origin(spans, capacity));
    }
    for span in spans {
        if span.start < cursor || span.end < span.start || span.end > content.len() {
            return Err(error(
                "E_EDIT_CONFLICT",
                "Resolved edit spans are not ordered or overlap.",
            ));
        }
        result.push_str(&content[cursor..span.start]);
        match span.insert_mode {
            Some(InsertMode::AppendEmptyOrigin | InsertMode::PrependEmptyOrigin) => {
                result.push_str(&span.replacement);
            }
            None => result.push_str(&span.replacement),
        }
        cursor = span.end;
    }
    result.push_str(&content[cursor..]);
    Ok(result)
}

/// Reject a mutation that would make a non-empty file byte-empty.
fn assert_not_empty(original: &str, result: &str) -> Result<(), HashlineError> {
    if !original.is_empty() && result.is_empty() {
        return Err(error(
            "E_WOULD_EMPTY",
            "Refusing to empty a non-empty file through edit. Use write or bash for an intentional full-file replacement.",
        ));
    }
    Ok(())
}

/// Warn about suspicious literal Unicode escape placeholders in inserted lines.
fn warn_unicode_placeholder(edits: &[HashlineEdit], warnings: &mut Vec<String>) {
    for edit in edits {
        let lines = match edit {
            HashlineEdit::Replace { lines, .. }
            | HashlineEdit::Append { lines, .. }
            | HashlineEdit::Prepend { lines, .. } => lines,
            HashlineEdit::ReplaceText { .. } => continue,
        };
        if lines
            .iter()
            .any(|line| line.to_ascii_lowercase().contains("\\udddd"))
        {
            warnings.push(
                "Detected literal \\uDDDD in edit content; no autocorrection applied. Verify whether this should be a real Unicode escape or plain text.".to_string(),
            );
        }
    }
}

/// Warn when a payload line looks like a copied bare hash prefix.
fn warn_bare_hash_prefix(
    edits: &[HashlineEdit],
    file_lines: &[String],
    warnings: &mut Vec<String>,
) {
    let mut suspects = Vec::new();
    for edit in edits {
        let lines = match edit {
            HashlineEdit::Replace { lines, .. }
            | HashlineEdit::Append { lines, .. }
            | HashlineEdit::Prepend { lines, .. } => lines,
            HashlineEdit::ReplaceText { .. } => continue,
        };
        for line in lines {
            if let Some(hash) = bare_hash_prefix(line, DEFAULT_HASH_LENGTH) {
                suspects.push((line, hash.to_string()));
            }
        }
    }
    if suspects.is_empty() {
        return;
    }
    let hashes = file_lines
        .iter()
        .enumerate()
        .map(|(index, _)| compute_line_hash(file_lines, index))
        .collect::<BTreeSet<_>>();
    let matches = suspects
        .iter()
        .filter(|(_, hash)| hashes.contains(hash))
        .count();
    if matches > 0 || suspects.len() >= 2 {
        warnings.push(format!(
            "{} edit line(s) start with a hash and ':'; resend literal content if these were copied from read output ({} prefix(es) match existing hashes).",
            suspects.len(), matches
        ));
    }
}

/// Apply one complete normalized hashline request without I/O or repeated full
/// document assembly.
pub(super) fn apply_hashline_edits(
    content: &str,
    request: &EditRequest,
) -> Result<ApplyOutcome, HashlineError> {
    if request.edits.is_empty() {
        return Ok(ApplyOutcome {
            content: content.to_string(),
            first_changed_line: None,
            last_changed_line: None,
            warnings: Vec::new(),
            noop_edits: Vec::new(),
        });
    }
    let line_index = build_line_index(content);
    let mut warnings = Vec::new();
    validate_edits(&request.edits, &line_index, &mut warnings)?;
    warn_bare_hash_prefix(&request.edits, &line_index.file_lines, &mut warnings);
    warn_unicode_placeholder(&request.edits, &mut warnings);

    let mut noops = Vec::new();
    let spans = resolve_spans(request, content, &line_index, &mut noops)?;
    let result = assemble(content, &spans)?;
    assert_not_empty(content, &result)?;
    let changed = compute_changed_line_range(content, &result);
    Ok(ApplyOutcome {
        content: result,
        first_changed_line: changed.map(|range| range.0),
        last_changed_line: changed.map(|range| range.1),
        warnings,
        noop_edits: noops,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(value: Value) -> EditRequest {
        parse_edit_request(value).unwrap_or_else(|error| panic!("{}", error.diagnostic()))
    }

    #[test]
    fn normalizes_top_level_replace_text_alias() {
        let parsed = request(json!({"path":"x","old_text":"a\r\nb","new_text":"c"}));
        assert_eq!(parsed.edits.len(), 1);
        assert!(
            matches!(&parsed.edits[0], HashlineEdit::ReplaceText { old_text, .. } if old_text == "a\nb")
        );
    }

    #[test]
    fn applies_strict_single_line_replace() {
        let parsed =
            request(json!({"path":"x","edits":[{"op":"replace","pos":"2#JB","lines":["BETA"]}]}));
        let result = apply_hashline_edits("alpha\nbeta\ngamma\ndelta\n", &parsed)
            .unwrap_or_else(|error| panic!("{}", error.diagnostic()));
        assert_eq!(result.content, "alpha\nBETA\ngamma\ndelta\n");
    }

    #[test]
    fn rejects_display_prefixes_and_empty_result() {
        let display = parse_edit_request(
            json!({"path":"x","edits":[{"op":"replace","pos":"2#JB","lines":["2#JB:beta"]}]}),
        );
        assert_eq!(display.unwrap_err().code, "E_INVALID_PATCH");
        let one_line = vec!["a".to_string(), String::new()];
        let one_line_hash = compute_line_hash(&one_line, 0);
        let parsed = request(
            json!({"path":"x","edits":[{"op":"replace","pos":format!("1#{one_line_hash}"),"lines":[]}]}),
        );
        let result = apply_hashline_edits("a\n", &parsed);
        assert_eq!(result.unwrap_err().code, "E_WOULD_EMPTY");
    }
    #[test]
    fn orders_empty_origin_prepend_before_append() {
        let parsed = request(json!({
            "path": "x",
            "edits": [
                {"op": "append", "lines": ["A"]},
                {"op": "prepend", "lines": ["P"]}
            ]
        }));
        let result = apply_hashline_edits("", &parsed)
            .unwrap_or_else(|error| panic!("{}", error.diagnostic()));
        assert_eq!(result.content, "P\nA");
    }
    #[test]
    fn stale_empty_boundary_payload_reports_stale_anchor_first() {
        let lines = vec!["alpha".to_string()];
        let actual_hash = compute_line_hash(&lines, 0);
        let stale_hash = if actual_hash == "ZZ" { "ZY" } else { "ZZ" };
        for operation in ["append", "prepend"] {
            let parsed = request(json!({
                "path": "x",
                "edits": [{
                    "op": operation,
                    "pos": format!("1#{stale_hash}"),
                    "lines": []
                }]
            }));
            let error = apply_hashline_edits("alpha", &parsed).unwrap_err();
            assert_eq!(error.code, "E_STALE_ANCHOR");
        }
    }

    #[test]
    fn valid_empty_boundary_payload_reports_bad_operation() {
        let lines = vec!["alpha".to_string()];
        let actual_hash = compute_line_hash(&lines, 0);
        for operation in ["append", "prepend"] {
            let parsed = request(json!({
                "path": "x",
                "edits": [{
                    "op": operation,
                    "pos": format!("1#{actual_hash}"),
                    "lines": []
                }]
            }));
            let error = apply_hashline_edits("alpha", &parsed).unwrap_err();
            assert_eq!(error.code, "E_BAD_OP");
        }
    }
}
