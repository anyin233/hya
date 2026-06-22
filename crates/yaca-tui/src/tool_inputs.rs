use crate::tool_questions;
use crate::tool_tasks;
use crate::tool_todos;

pub(crate) fn summary(name: &str, value: &serde_json::Value) -> Option<String> {
    match name {
        "read" => field_text(value, "path").map(|path| read_summary(value, path)),
        "ls" => field_text(value, "path"),
        "edit" | "write" => field_text(value, "path"),
        "shell" | "bash" => field_text(value, "cmd").or_else(|| field_text(value, "command")),
        "find" => search_summary(value, "pattern"),
        "grep" => search_summary(value, "pattern"),
        "glob" => field_text(value, "pattern").map(|pattern| quoted(&pattern)),
        "webfetch" => field_text(value, "url"),
        "websearch" => field_text(value, "query").map(|query| quoted(&query)),
        "ask_user" => tool_questions::summary(value),
        "task" => tool_tasks::summary(value),
        "todowrite" => tool_todos::summary(value),
        _ => None,
    }
}

fn search_summary(value: &serde_json::Value, key: &str) -> Option<String> {
    field_text(value, key).map(|pattern| match field_text(value, "path") {
        Some(path) => format!("{} in {path}", quoted(&pattern)),
        None => quoted(&pattern),
    })
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\\\""))
}

fn field_text(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .and_then(clean_inline_text)
}

fn clean_inline_text(text: &str) -> Option<String> {
    let cleaned = text.trim().replace('\n', " ");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn read_summary(value: &serde_json::Value, path: String) -> String {
    let mut args = Vec::new();
    if let Some(limit) = value.get("limit").and_then(serde_json::Value::as_u64) {
        args.push(format!("limit={limit}"));
    }
    if let Some(offset) = value.get("offset").and_then(serde_json::Value::as_u64) {
        args.push(format!("offset={offset}"));
    }
    if args.is_empty() {
        path
    } else {
        format!("{path} [{}]", args.join(", "))
    }
}
