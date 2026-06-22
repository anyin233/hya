pub(crate) fn summary(value: &serde_json::Value) -> Option<String> {
    description(value).or_else(|| kind(value).map(|kind| format!("{kind} Task")))
}

pub(crate) fn snapshot_text(input: &serde_json::Value) -> Option<String> {
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

    Some(lines.join("\n"))
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
    let cleaned = text.trim().replace('\n', " ");
    (!cleaned.is_empty()).then_some(cleaned)
}

fn title_case_ascii(input: String) -> String {
    let mut chars = input.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}
