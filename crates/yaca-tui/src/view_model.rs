use yaca_proto::{PartProjection, Projection, Role, ToolPartState};

use crate::ansi;
use crate::tool_inputs;
use crate::tool_labels::{action_label, websearch_provider_label};
use crate::tool_questions;
use crate::tool_tasks;
use crate::tool_todos;

pub enum TimelinePart {
    Text(String),
    Reasoning(String),
    Tool {
        name: String,
        label: String,
        input: String,
        status: ToolStatus,
    },
}

pub enum ToolStatus {
    Pending,
    Running,
    Completed {
        time_ms: u64,
        output: Option<String>,
        exit_code: Option<i64>,
    },
    Error {
        message: String,
    },
}

pub struct TimelineItem {
    pub role: Role,
    pub duration_ms: Option<u64>,
    pub parts: Vec<TimelinePart>,
}

#[must_use]
pub(crate) fn latest_assistant_duration_ms(projection: &Projection) -> Option<u64> {
    projection
        .session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .and_then(|message| message_duration_ms(message.started_millis, message.completed_millis))
}

#[must_use]
pub fn timeline_items(projection: &Projection) -> Vec<TimelineItem> {
    projection
        .session
        .messages
        .iter()
        .map(|message| TimelineItem {
            role: message.role,
            duration_ms: message_duration_ms(message.started_millis, message.completed_millis),
            parts: message.parts.iter().map(part_to_timeline).collect(),
        })
        .collect()
}

fn message_duration_ms(started: Option<i64>, completed: Option<i64>) -> Option<u64> {
    let elapsed = completed?.checked_sub(started?)?;
    u64::try_from(elapsed).ok()
}

fn part_to_timeline(part: &PartProjection) -> TimelinePart {
    match part {
        PartProjection::Text { text, .. } => TimelinePart::Text(text.clone()),
        PartProjection::Reasoning { text, .. } => TimelinePart::Reasoning(text.clone()),
        PartProjection::Tool { name, state, .. } => {
            let name = name.to_string();
            let status = match state {
                ToolPartState::Pending { .. } => ToolStatus::Pending,
                ToolPartState::Running { .. } => ToolStatus::Running,
                ToolPartState::Completed {
                    input,
                    time_ms,
                    output,
                    ..
                } => ToolStatus::Completed {
                    time_ms: *time_ms,
                    output: completed_tool_output_text(&name, input, output),
                    exit_code: completed_tool_exit_code(&name, output),
                },
                ToolPartState::Error { message, .. } => ToolStatus::Error {
                    message: ellipsize(message, 40),
                },
            };
            TimelinePart::Tool {
                input: tool_input(&name, state),
                label: tool_label(&name, state),
                name,
                status,
            }
        }
    }
}

fn tool_label(name: &str, state: &ToolPartState) -> String {
    if name != "websearch" {
        return action_label(name);
    }

    let provider = match state {
        ToolPartState::Pending { input }
        | ToolPartState::Running { input }
        | ToolPartState::Error { input, .. } => websearch_provider(input),
        ToolPartState::Completed { input, output, .. } => {
            websearch_provider(output).or_else(|| websearch_provider(input))
        }
    };
    websearch_provider_label(provider)
}

fn websearch_provider(value: &serde_json::Value) -> Option<&str> {
    value.get("provider").and_then(serde_json::Value::as_str)
}

fn tool_input(name: &str, state: &ToolPartState) -> String {
    let value = match state {
        ToolPartState::Pending { input }
        | ToolPartState::Running { input }
        | ToolPartState::Error { input, .. } => input,
        ToolPartState::Completed { input, output, .. } => {
            return completed_tool_input(name, input, output);
        }
    };
    if let Some(summary) = tool_inputs::summary(name, value) {
        return summary;
    }
    if value.is_null() {
        String::new()
    } else {
        ellipsize(&value.to_string(), 48)
    }
}

fn completed_tool_input(
    name: &str,
    input: &serde_json::Value,
    output: &serde_json::Value,
) -> String {
    let summary = tool_inputs::summary(name, input).unwrap_or_else(|| {
        if input.is_null() {
            String::new()
        } else {
            ellipsize(&input.to_string(), 48)
        }
    });
    match completed_count_suffix(name, output) {
        Some(suffix) if summary.is_empty() => suffix,
        Some(suffix) => format!("{summary} {suffix}"),
        None => summary,
    }
}

fn completed_count_suffix(name: &str, output: &serde_json::Value) -> Option<String> {
    match name {
        "glob" => match_count_suffix(output, "count"),
        "grep" => match_count_suffix(output, "matches"),
        "websearch" => output
            .get("numResults")
            .and_then(serde_json::Value::as_u64)
            .map(|count| format!("({count} results)")),
        _ => None,
    }
}

fn match_count_suffix(output: &serde_json::Value, key: &str) -> Option<String> {
    output
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .map(|count| {
            let noun = if count == 1 { "match" } else { "matches" };
            format!("({count} {noun})")
        })
}

fn ellipsize(s: &str, max: usize) -> String {
    let cleaned = s.replace('\n', " ");
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let head: String = cleaned.chars().take(max).collect();
        format!("{head}…")
    }
}

fn completed_tool_output_text(
    name: &str,
    input: &serde_json::Value,
    output: &serde_json::Value,
) -> Option<String> {
    if name == "ask_user" {
        return tool_questions::snapshot_text(input, output);
    }
    if name == "todowrite" {
        return tool_todos::snapshot_text(input);
    }
    if name == "task" {
        return tool_tasks::snapshot_text(input);
    }
    if matches!(name, "bash" | "shell") {
        return completed_output_text(output)
            .and_then(|text| clean_multiline_output(&ansi::strip(&text)));
    }

    completed_output_text(output)
}

fn completed_tool_exit_code(name: &str, output: &serde_json::Value) -> Option<i64> {
    match name {
        "bash" | "shell" => output.get("exit_code").and_then(serde_json::Value::as_i64),
        _ => None,
    }
}

fn completed_output_text(output: &serde_json::Value) -> Option<String> {
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
