use serde_json::Value;

pub(super) fn questions(request: &Value) -> &[Value] {
    request
        .get("questions")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
}

pub(super) fn question_at(request: &Value, tab: usize) -> Option<&Value> {
    questions(request).get(tab)
}

pub(super) fn options(question: &Value) -> &[Value] {
    question
        .get("options")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
}

pub(super) fn option_label(question: &Value, index: usize) -> Option<&str> {
    options(question).get(index)?.get("label")?.as_str()
}

pub(super) fn text_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

pub(super) fn multiple(question: &Value) -> bool {
    question
        .get("multiple")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(super) fn custom_enabled(question: &Value) -> bool {
    question.get("custom").and_then(Value::as_bool) != Some(false)
}

pub(super) fn single(request: &Value) -> bool {
    questions(request).len() == 1 && !questions(request).first().is_some_and(multiple)
}

pub(super) fn tabs(request: &Value) -> usize {
    if single(request) {
        1
    } else {
        questions(request).len() + 1
    }
}
