use crate::ansi;
use crate::tool_questions;
use crate::tool_tasks;
use crate::tool_todos;

pub(crate) fn completed_text(
    name: &str,
    input: &serde_json::Value,
    output: &serde_json::Value,
    time_ms: u64,
) -> Option<String> {
    if name == "ask_user" {
        return tool_questions::snapshot_text(input, output);
    }
    if name == "todowrite" {
        return tool_todos::snapshot_text(input);
    }
    if name == "task" {
        return tool_tasks::snapshot_text(input, output, time_ms);
    }
    if matches!(name, "bash" | "shell") {
        return output_text(output).and_then(|text| clean_multiline_output(&ansi::strip(&text)));
    }

    output_text(output)
}

pub(crate) fn exit_code(name: &str, output: &serde_json::Value) -> Option<i64> {
    match name {
        "bash" | "shell" => output.get("exit_code").and_then(serde_json::Value::as_i64),
        _ => None,
    }
}

fn output_text(output: &serde_json::Value) -> Option<String> {
    if let Some(text) = output.as_str().and_then(clean_multiline_output) {
        return Some(text);
    }

    for key in [
        "stdout",
        "stderr",
        "output",
        "diff",
        "diagnostics",
        "message",
    ] {
        if let Some(text) = output
            .get(key)
            .and_then(serde_json::Value::as_str)
            .and_then(clean_multiline_output)
        {
            return Some(text);
        }
    }

    None
}

fn clean_multiline_output(text: &str) -> Option<String> {
    let cleaned = text
        .trim_matches(|ch| matches!(ch, '\n' | '\r'))
        .replace('\r', "");
    if cleaned.trim().is_empty() {
        None
    } else {
        Some(cleaned)
    }
}
