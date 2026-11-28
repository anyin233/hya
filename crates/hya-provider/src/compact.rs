//! System-message envelope that carries a native compact window between turns.
//!
//! The engine writes it after a native compaction; a protocol that replays
//! compact windows reads it back when encoding the next request.

use serde_json::{Value, json};

use hya_proto::context::TODO_LIST_HEADER;
pub use hya_proto::context::{COMPACT_CONTEXT_MARKER, RESPONSES_COMPACT_ITEMS_MARKER};

use crate::ProviderError;

/// Format a system message body that carries a compact window for re-injection.
#[must_use]
pub fn format_responses_compact_system(items: &[Value]) -> String {
    format!(
        "{COMPACT_CONTEXT_MARKER}\n{RESPONSES_COMPACT_ITEMS_MARKER}\n{}",
        Value::Array(items.to_vec())
    )
}

/// Extract the compact-item array and append any carried todo list as a
/// system item.
pub fn parse_responses_compact_items(content: &str) -> Result<Option<Vec<Value>>, ProviderError> {
    let Some(rest) = content
        .strip_prefix(COMPACT_CONTEXT_MARKER)
        .map(str::trim_start)
    else {
        return Ok(None);
    };
    let Some(json_text) = rest
        .strip_prefix(RESPONSES_COMPACT_ITEMS_MARKER)
        .map(str::trim_start)
    else {
        // Local-summarizer compact (text summary only) — not a Responses window.
        return Ok(None);
    };
    // Native windows store compact JSON on one line. A fold can append
    // the todo list verbatim after that array.
    let (json_text, todos) = match json_text.split_once("\n\n") {
        Some((json, todos)) if todos.starts_with(TODO_LIST_HEADER) => (json, Some(todos)),
        _ => (json_text, None),
    };
    let value: Value = serde_json::from_str(json_text).map_err(|e| {
        ProviderError::Decode(format!("invalid responses compact window payload: {e}"))
    })?;
    match value {
        Value::Array(mut items) => {
            if let Some(todos) = todos {
                items.push(json!({"role": "system", "content": todos}));
            }
            Ok(Some(items))
        }
        other => Err(ProviderError::Decode(format!(
            "responses compact window must be a JSON array, got {}",
            other_kind(&other)
        ))),
    }
}

fn other_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
