//! Global tool-output size guard.
//!
//! Oversized tool results are the dominant cause of subagent context-window
//! failures: a single `find` or MCP explore can inject megabytes into the next
//! model call. Cap every successful tool result to a short tail the agent can
//! still read, and tell it the original length so it can re-query narrowly.

use serde_json::Value;

/// Maximum characters of tool output kept for the model (last N chars).
pub const MAX_TOOL_OUTPUT_CHARS: usize = 5000;

/// Cap a successful tool `output` value for model consumption.
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

fn value_as_display_text(value: &Value) -> String {
    match value.as_str() {
        Some(s) => s.to_string(),
        None => value.to_string(),
    }
}

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
        // 3-byte UTF-8 codepoints; count by chars not bytes.
        let body: String = "你".repeat(MAX_TOOL_OUTPUT_CHARS + 10);
        let capped = cap_tool_output(Value::String(body.clone()));
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
}
