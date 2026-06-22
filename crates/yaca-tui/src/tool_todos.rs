pub(crate) fn summary(value: &serde_json::Value) -> Option<String> {
    let todos = value.get("todos")?.as_array()?;
    Some(format!("{} total", todos.len()))
}

pub(crate) fn snapshot_text(input: &serde_json::Value) -> Option<String> {
    let todos = input.get("todos")?.as_array()?;
    let mut lines = vec!["# Todos".to_string()];
    for item in todos {
        let Some(content) = item
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|content| !content.is_empty())
        else {
            continue;
        };
        let mark = match item.get("status").and_then(serde_json::Value::as_str) {
            Some("completed") => "[✓]",
            Some("in_progress") => "[•]",
            _ => "[ ]",
        };
        lines.push(format!("{mark} {content}"));
    }

    (lines.len() > 1).then(|| lines.join("\n"))
}
