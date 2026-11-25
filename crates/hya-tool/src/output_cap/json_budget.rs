use serde_json::{Map, Value};

use super::{
    MAX_GENERIC_METADATA_ITEMS, MAX_METADATA_KEY_BYTES, TruncationFacts, TruncationKind,
    json_char_len,
};

/// Bound a string to complete newline-delimited rows where possible.
pub(super) fn bound_line_string(
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

/// Bound a string or recursively bound JSON field to a serialized-byte budget.
pub(super) fn bound_string_or_json(
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
pub(super) fn bound_json_value(
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
pub(super) fn bound_string(
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

/// Shrink an already bounded value to a serialized-byte budget.
pub(super) fn shrink_value_to_budget(value: &mut Value, max_serialized_bytes: usize) {
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
pub(super) fn serialized_len<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}
