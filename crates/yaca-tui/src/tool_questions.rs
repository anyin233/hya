pub(crate) fn summary(value: &serde_json::Value) -> Option<String> {
    if value.get("question").is_some() {
        Some("1 question".to_string())
    } else {
        None
    }
}

pub(crate) fn snapshot_text(
    input: &serde_json::Value,
    output: &serde_json::Value,
) -> Option<String> {
    let question = input
        .get("question")
        .and_then(serde_json::Value::as_str)
        .and_then(clean_text)?;
    let answer = answer_text(output);
    Some(format!(
        "# Questions\nQuestion: {question}\nAnswer: {answer}"
    ))
}

fn answer_text(output: &serde_json::Value) -> String {
    let cancelled = output
        .get("cancelled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if cancelled {
        return "(cancelled)".to_string();
    }
    output
        .get("answer")
        .and_then(serde_json::Value::as_str)
        .and_then(clean_text)
        .unwrap_or_else(|| "(no answer)".to_string())
}

fn clean_text(text: &str) -> Option<String> {
    let cleaned = text.trim().replace('\n', " ");
    (!cleaned.is_empty()).then_some(cleaned)
}
