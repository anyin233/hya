use yaca_proto::{PartProjection, Projection, Role, ToolPartState};

pub enum TimelinePart {
    Text(String),
    Reasoning(String),
    Tool {
        name: String,
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
            TimelinePart::Tool {
                input: tool_input(&name, state),
                name,
                status: match state {
                    ToolPartState::Pending { .. } => ToolStatus::Pending,
                    ToolPartState::Running { .. } => ToolStatus::Running,
                    ToolPartState::Completed {
                        time_ms, output, ..
                    } => ToolStatus::Completed {
                        time_ms: *time_ms,
                        output: completed_output_text(output),
                    },
                    ToolPartState::Error { message, .. } => ToolStatus::Error {
                        message: ellipsize(message, 40),
                    },
                },
            }
        }
    }
}

fn tool_input(name: &str, state: &ToolPartState) -> String {
    let value = match state {
        ToolPartState::Pending { input }
        | ToolPartState::Running { input }
        | ToolPartState::Completed { input, .. }
        | ToolPartState::Error { input, .. } => input,
    };
    if let Some(summary) = known_tool_input(name, value) {
        return summary;
    }
    if value.is_null() {
        String::new()
    } else {
        ellipsize(&value.to_string(), 48)
    }
}

fn known_tool_input(name: &str, value: &serde_json::Value) -> Option<String> {
    match name {
        "read" => field_text(value, "path").map(|path| read_summary(value, path)),
        "edit" | "write" => field_text(value, "path"),
        "shell" | "bash" => field_text(value, "cmd").or_else(|| field_text(value, "command")),
        "grep" => field_text(value, "pattern").map(|pattern| match field_text(value, "path") {
            Some(path) => format!("{pattern} in {path}"),
            None => pattern,
        }),
        "glob" => field_text(value, "pattern"),
        _ => None,
    }
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

fn ellipsize(s: &str, max: usize) -> String {
    let cleaned = s.replace('\n', " ");
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let head: String = cleaned.chars().take(max).collect();
        format!("{head}…")
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
        Some(ellipsize_preserving_lines(&cleaned, 320))
    }
}

fn ellipsize_preserving_lines(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}
