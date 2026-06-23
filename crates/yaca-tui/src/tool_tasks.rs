use crate::ansi;

pub(crate) fn summary(value: &serde_json::Value) -> Option<String> {
    Some(
        description(value)
            .or_else(|| kind(value).map(|kind| format!("{kind} Task")))
            .unwrap_or_else(|| "Task".to_string()),
    )
}

pub(crate) fn snapshot_text(
    input: &serde_json::Value,
    output: &serde_json::Value,
    time_ms: u64,
) -> Option<String> {
    let title = format!(
        "# {} Task",
        kind(input).unwrap_or_else(|| "General".to_string())
    );
    let mut lines = vec![title];
    if let Some(description) = description(input) {
        lines.push(description);
    } else if let Some(rows) = member_descriptions(input) {
        lines.extend(rows);
    }
    if let Some(summary) = toolcall_summary(output, time_ms) {
        lines.push(summary);
    }

    Some(lines.join("\n"))
}

fn toolcall_summary(output: &serde_json::Value, time_ms: u64) -> Option<String> {
    let count = toolcall_count(output)?;
    let noun = if count == 1 { "toolcall" } else { "toolcalls" };
    Some(format!("↳ {count} {noun} · {}", format_duration(time_ms)))
}

fn toolcall_count(output: &serde_json::Value) -> Option<u64> {
    for key in [
        "toolcalls",
        "toolcall_count",
        "tool_call_count",
        "tool_calls",
    ] {
        if let Some(value) = output.get(key) {
            return explicit_count(value);
        }
    }

    member_count(output)
}

fn explicit_count(value: &serde_json::Value) -> Option<u64> {
    if let Some(count) = value.as_u64() {
        return (count > 0).then_some(count);
    }
    if let Some(items) = value.as_array() {
        return u64::try_from(items.len()).ok().filter(|count| *count > 0);
    }
    None
}

fn member_count(output: &serde_json::Value) -> Option<u64> {
    u64::try_from(output.get("members")?.as_array()?.len())
        .ok()
        .filter(|count| *count > 0)
}

fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    if ms < 60_000 {
        return format!("{:.1}s", ms as f64 / 1_000.0);
    }
    let minutes = ms / 60_000;
    let seconds = (ms % 60_000) / 1_000;
    format!("{minutes}m {seconds}s")
}

fn kind(value: &serde_json::Value) -> Option<String> {
    value
        .get("subagent_type")
        .and_then(serde_json::Value::as_str)
        .and_then(clean_text)
        .map(title_case_ascii)
}

fn description(value: &serde_json::Value) -> Option<String> {
    value
        .get("description")
        .and_then(serde_json::Value::as_str)
        .and_then(clean_text)
}

fn member_descriptions(value: &serde_json::Value) -> Option<Vec<String>> {
    let rows: Vec<String> = value
        .get("members")?
        .as_array()?
        .iter()
        .filter_map(description)
        .collect();
    (!rows.is_empty()).then_some(rows)
}

fn clean_text(text: &str) -> Option<String> {
    let cleaned = ansi::strip(text)
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!cleaned.is_empty()).then_some(cleaned)
}

fn title_case_ascii(input: String) -> String {
    let mut chars = input.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}
