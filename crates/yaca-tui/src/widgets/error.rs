pub(super) fn is_system_error_text(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    lower.starts_with("turn error:")
        || lower.starts_with("input error:")
        || lower.starts_with("system error:")
        || lower.starts_with("error:")
        || lower.contains("http: 4")
        || lower.contains("http: 5")
        || lower.contains(" forbidden")
}

pub(super) fn display_system_error_segment(segment: &str) -> &str {
    segment
        .trim_start()
        .strip_prefix("error:")
        .map(str::trim_start)
        .unwrap_or(segment)
}
