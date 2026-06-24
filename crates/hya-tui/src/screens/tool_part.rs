use hya_sdk::ToolPart;
use serde_json::Value;

use crate::render::text::{Attrs, Line, Span};
use crate::theme::ResolvedTheme;

const MAX_OUTPUT_LINES: usize = 10;
const MAX_GENERIC_LINES: usize = 3;
const SPLIT_DIFF_MIN_WIDTH: usize = 100;

#[must_use]
pub fn render(tool: &ToolPart, width: usize, spinner: &str, theme: &ResolvedTheme) -> Vec<Line> {
    let state = tool.state.as_ref();
    let status = field(state, "status").unwrap_or("pending");
    let name = tool.tool.as_deref().unwrap_or("");
    let mut lines = match name {
        "bash" => bash(state, status, theme),
        "read" => read(state, status, theme),
        "write" => write_file(state, status, theme),
        "edit" => edit(state, status, width, theme),
        "grep" => vec![summary(status, grep_glob("Grep", state, "matches"), theme)],
        "glob" => vec![summary(status, grep_glob("Glob", state, "count"), theme)],
        "webfetch" => vec![summary(
            status,
            format!("WebFetch {}", input_str(state, "url").unwrap_or_default()),
            theme,
        )],
        "websearch" => vec![summary(status, websearch(state), theme)],
        "todowrite" => todowrite(state, status, theme),
        "task" => task(state, status, theme),
        "question" => question(state, status, theme),
        "skill" => vec![summary(
            status,
            format!("Skill \"{}\"", input_str(state, "name").unwrap_or_default()),
            theme,
        )],
        "apply_patch" => apply_patch(state, status, width, theme),
        _ => generic(tool, state, status, theme),
    };

    if status == "running" && !spinner.is_empty() {
        if let Some(first) = lines.first_mut().and_then(|line| line.0.first_mut()) {
            if first.text.starts_with('\u{25d0}') {
                first.text = format!("{spinner} ");
            }
        }
    }

    if status == "error" {
        if let Some(error) = field(state, "error") {
            lines.push(indented(
                format!("  {error}"),
                theme.error,
                Attrs::default(),
            ));
        }
    }
    lines
}

fn bash(state: Option<&Value>, status: &str, theme: &ResolvedTheme) -> Vec<Line> {
    let command = input_str(state, "command").unwrap_or_default();
    let description = input_str(state, "description").unwrap_or("Shell");
    let workdir = input_str(state, "workdir").filter(|w| *w != ".");
    let title = match workdir {
        Some(dir) => format!("{description} in {dir}"),
        None => description.to_owned(),
    };
    let mut lines = vec![block_title(status, &title, theme)];
    lines.push(indented(
        format!("  $ {command}"),
        theme.secondary,
        Attrs::default(),
    ));
    if let Some(output) = meta_str(state, "output") {
        push_collapsed(
            &mut lines,
            strip_ansi(output).trim(),
            MAX_OUTPUT_LINES,
            theme,
        );
    }
    lines
}

fn read(state: Option<&Value>, status: &str, theme: &ResolvedTheme) -> Vec<Line> {
    let path = input_str(state, "filePath").unwrap_or_default();
    let extra = primitive_args(state, &["filePath"]);
    let mut lines = vec![summary(status, format!("Read {path}{extra}"), theme)];
    if let Some(loaded) = meta(state, "loaded").and_then(Value::as_array) {
        for entry in loaded {
            if let Some(name) = entry.as_str() {
                lines.push(indented(
                    format!("  \u{21b3} Loaded {name}"),
                    theme.text_muted,
                    Attrs::default(),
                ));
            }
        }
    }
    lines
}

fn write_file(state: Option<&Value>, status: &str, theme: &ResolvedTheme) -> Vec<Line> {
    let path = input_str(state, "filePath").unwrap_or_default();
    if meta(state, "diagnostics").is_none() {
        return vec![summary(status, format!("Write {path}"), theme)];
    }
    let mut lines = vec![block_title(status, &format!("Wrote {path}"), theme)];
    if let Some(content) = input_str(state, "content") {
        push_highlighted(&mut lines, language_for(path), content.trim_end(), theme);
    }
    push_diagnostics(&mut lines, meta(state, "diagnostics"), path, theme);
    lines
}

fn language_for(path: &str) -> &str {
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("text")
}

fn push_highlighted(lines: &mut Vec<Line>, language: &str, content: &str, theme: &ResolvedTheme) {
    let highlighted = crate::render::highlight_code(language, content, theme);
    let total = highlighted.len();
    lines.extend(highlighted.into_iter().take(MAX_OUTPUT_LINES));
    if total > MAX_OUTPUT_LINES {
        lines.push(indented(
            format!("  \u{2026} +{} lines", total - MAX_OUTPUT_LINES),
            theme.text_muted,
            Attrs::default(),
        ));
    }
}

fn push_diagnostics(
    lines: &mut Vec<Line>,
    diagnostics: Option<&Value>,
    path: &str,
    theme: &ResolvedTheme,
) {
    let Some(entries) = diagnostics
        .and_then(|value| value.get(path))
        .and_then(Value::as_array)
    else {
        return;
    };
    for entry in entries {
        if entry.get("severity").and_then(Value::as_i64) != Some(1) {
            continue;
        }
        let start = entry.get("range").and_then(|range| range.get("start"));
        let line = start
            .and_then(|position| position.get("line"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1;
        let character = start
            .and_then(|position| position.get("character"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1;
        let Some(message) = entry.get("message").and_then(Value::as_str) else {
            continue;
        };
        lines.push(indented(
            format!("  Error [{line}:{character}] {message}"),
            theme.error,
            Attrs::default(),
        ));
    }
}

fn edit(state: Option<&Value>, status: &str, width: usize, theme: &ResolvedTheme) -> Vec<Line> {
    let path = input_str(state, "filePath").unwrap_or_default();
    let replace_all = input_bool(state, "replaceAll")
        .filter(|v| *v)
        .map_or(String::new(), |_| " [replaceAll=true]".to_owned());
    let mut lines = vec![summary(status, format!("Edit {path}{replace_all}"), theme)];
    if let Some(diff) = meta_str(state, "diff") {
        lines.extend(render_diff(diff, width, theme));
    }
    lines
}

fn apply_patch(
    state: Option<&Value>,
    status: &str,
    width: usize,
    theme: &ResolvedTheme,
) -> Vec<Line> {
    let mut lines = vec![block_title(status, "Apply Patch", theme)];
    if let Some(diff) = meta_str(state, "diff") {
        lines.extend(render_diff(diff, width, theme));
    }
    lines
}

fn render_diff(diff: &str, width: usize, theme: &ResolvedTheme) -> Vec<Line> {
    if width >= SPLIT_DIFF_MIN_WIDTH {
        crate::render::diff::render_split(diff, width, theme)
    } else {
        crate::render::diff::render_unified(diff, theme)
    }
}

fn todowrite(state: Option<&Value>, status: &str, theme: &ResolvedTheme) -> Vec<Line> {
    let Some(todos) = input(state)
        .and_then(|i| i.get("todos"))
        .and_then(Value::as_array)
    else {
        return vec![summary(status, "Updating todos...".to_owned(), theme)];
    };
    let mut lines = vec![block_title(status, "Todos", theme)];
    for todo in todos {
        let content = todo
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let state_str = todo
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        let (glyph, color) = match state_str {
            "completed" => ("\u{2713}", theme.success),
            "in_progress" => ("\u{25d0}", theme.warning),
            "cancelled" => ("\u{2717}", theme.text_muted),
            _ => ("\u{25cb}", theme.text_muted),
        };
        lines.push(Line(vec![
            Span::styled(format!("  {glyph} "), Some(color), None, Attrs::default()),
            Span::styled(content.to_owned(), Some(theme.text), None, Attrs::default()),
        ]));
    }
    lines
}

fn question(state: Option<&Value>, status: &str, theme: &ResolvedTheme) -> Vec<Line> {
    let questions = input(state)
        .and_then(|i| i.get("questions"))
        .and_then(Value::as_array);
    let Some(questions) = questions else {
        return vec![summary(status, "Asked questions".to_owned(), theme)];
    };
    let mut lines = vec![block_title(status, "Questions", theme)];
    for entry in questions {
        let text = entry
            .as_str()
            .or_else(|| entry.get("question").and_then(Value::as_str));
        if let Some(text) = text {
            lines.push(indented(
                format!("  {text}"),
                theme.text_muted,
                Attrs::default(),
            ));
        }
    }
    lines
}

fn generic(
    tool: &ToolPart,
    state: Option<&Value>,
    status: &str,
    theme: &ResolvedTheme,
) -> Vec<Line> {
    let name = tool.tool.as_deref().unwrap_or("tool");
    let args = primitive_args(state, &[]);
    let mut lines = vec![summary(status, format!("{name}{args}"), theme)];
    if let Some(output) = field(state, "output")
        .map(str::trim)
        .filter(|o| !o.is_empty())
    {
        push_collapsed(&mut lines, output, MAX_GENERIC_LINES, theme);
    }
    lines
}

fn grep_glob(label: &str, state: Option<&Value>, count_key: &str) -> String {
    let pattern = input_str(state, "pattern").unwrap_or_default();
    let path = input_str(state, "path");
    let count = meta(state, count_key).and_then(Value::as_i64);
    let where_clause = path.map_or(String::new(), |p| format!(" in {p}"));
    let count_clause = count.map_or(String::new(), |n| format!(" ({n} matches)"));
    format!("{label} \"{pattern}\"{where_clause}{count_clause}")
}

fn websearch(state: Option<&Value>) -> String {
    let query = input_str(state, "query").unwrap_or_default();
    let label = match meta_str(state, "provider") {
        Some("parallel") => "Parallel Web Search",
        Some("exa") => "Exa Web Search",
        _ => "Web Search",
    };
    let count = meta(state, "numResults")
        .and_then(Value::as_i64)
        .map_or(String::new(), |n| format!(" ({n} results)"));
    format!("{label} \"{query}\"{count}")
}

fn task(state: Option<&Value>, status: &str, theme: &ResolvedTheme) -> Vec<Line> {
    let kind = input_str(state, "subagent_type").unwrap_or("general");
    let description = input_str(state, "description").unwrap_or_default();
    let mut lines = vec![summary(
        status,
        format!("{} Task \u{2014} {description}", titlecase(kind)),
        theme,
    )];
    if let Some(duration) = task_duration(state) {
        lines.push(indented(
            format!("  \u{21b3} done in {duration}"),
            theme.text_muted,
            Attrs::default(),
        ));
    }
    if let Some(output) = field(state, "output")
        .map(str::trim)
        .filter(|output| !output.is_empty())
    {
        push_collapsed(&mut lines, output, MAX_OUTPUT_LINES, theme);
    }
    lines
}

fn task_duration(state: Option<&Value>) -> Option<String> {
    let time = state.and_then(|s| s.get("time"))?;
    let start = time.get("start").and_then(Value::as_i64)?;
    let end = time.get("end").and_then(Value::as_i64)?;
    let seconds = (end - start) as f64 / 1000.0;
    Some(format!("{seconds:.1}s"))
}

fn summary(status: &str, text: String, theme: &ResolvedTheme) -> Line {
    let (glyph, color) = marker(status, theme);
    let body = if status == "error" {
        theme.error
    } else {
        theme.text
    };
    Line(vec![
        Span::styled(format!("{glyph} "), Some(color), None, bold()),
        Span::styled(text, Some(body), None, Attrs::default()),
    ])
}

fn block_title(status: &str, title: &str, theme: &ResolvedTheme) -> Line {
    let (glyph, color) = marker(status, theme);
    Line(vec![
        Span::styled(format!("{glyph} "), Some(color), None, bold()),
        Span::styled(title.to_owned(), Some(theme.accent), None, bold()),
    ])
}

fn indented(text: String, color: crate::contracts::Rgba, attrs: Attrs) -> Line {
    Line(vec![Span::styled(text, Some(color), None, attrs)])
}

fn push_collapsed(lines: &mut Vec<Line>, body: &str, max: usize, theme: &ResolvedTheme) {
    if body.is_empty() {
        return;
    }
    let total = body.lines().count();
    for line in body.lines().take(max) {
        lines.push(indented(
            format!("  {line}"),
            theme.text_muted,
            Attrs::default(),
        ));
    }
    if total > max {
        lines.push(indented(
            format!("  \u{2026} +{} lines", total - max),
            theme.text_muted,
            Attrs::default(),
        ));
    }
}

fn marker(status: &str, theme: &ResolvedTheme) -> (&'static str, crate::contracts::Rgba) {
    match status {
        "completed" => ("\u{2713}", theme.success),
        "error" => ("\u{2717}", theme.error),
        "running" => ("\u{25d0}", theme.warning),
        _ => ("\u{7e}", theme.text_muted),
    }
}

fn primitive_args(state: Option<&Value>, exclude: &[&str]) -> String {
    let Some(map) = input(state).and_then(Value::as_object) else {
        return String::new();
    };
    let parts: Vec<String> = map
        .iter()
        .filter(|(key, value)| !exclude.contains(&key.as_str()) && is_primitive(value))
        .map(|(key, value)| format!("{key}={}", primitive_string(value)))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" [{}]", parts.join(", "))
    }
}

fn is_primitive(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

fn primitive_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn titlecase(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for inner in chars.by_ref() {
                    if inner.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn field<'a>(state: Option<&'a Value>, key: &str) -> Option<&'a str> {
    state.and_then(|s| s.get(key)).and_then(Value::as_str)
}

fn input(state: Option<&Value>) -> Option<&Value> {
    state.and_then(|s| s.get("input"))
}

fn input_str<'a>(state: Option<&'a Value>, key: &str) -> Option<&'a str> {
    input(state)
        .and_then(|i| i.get(key))
        .and_then(Value::as_str)
}

fn input_bool(state: Option<&Value>, key: &str) -> Option<bool> {
    input(state)
        .and_then(|i| i.get(key))
        .and_then(Value::as_bool)
}

fn meta<'a>(state: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    state
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get(key))
}

fn meta_str<'a>(state: Option<&'a Value>, key: &str) -> Option<&'a str> {
    meta(state, key).and_then(Value::as_str)
}

fn bold() -> Attrs {
    Attrs {
        bold: true,
        ..Attrs::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{builtin_theme, resolve, Mode, DEFAULT_THEME};

    fn theme() -> ResolvedTheme {
        resolve(&builtin_theme(DEFAULT_THEME).unwrap().unwrap(), Mode::Dark).unwrap()
    }

    fn flatten(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|line| {
                line.0
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn write_with_diagnostics_highlights_content_and_lists_only_errors() {
        let theme = theme();
        let state = serde_json::json!({
            "input": { "filePath": "foo.rs", "content": "fn main() { let x = 1; }" },
            "metadata": {
                "diagnostics": {
                    "foo.rs": [
                        { "severity": 1, "range": { "start": { "line": 0, "character": 12 } }, "message": "unused variable: x" },
                        { "severity": 2, "range": { "start": { "line": 0, "character": 0 } }, "message": "a warning skipped" }
                    ]
                }
            }
        });
        let rendered = flatten(&write_file(Some(&state), "completed", &theme));
        assert!(rendered.contains("Wrote foo.rs"), "title: {rendered}");
        assert!(
            rendered.contains("fn main"),
            "highlighted content present: {rendered}"
        );
        assert!(
            rendered.contains("Error [1:13] unused variable: x"),
            "error diagnostic with line+1:char+1: {rendered}"
        );
        assert!(
            !rendered.contains("a warning skipped"),
            "severity != 1 is skipped: {rendered}"
        );
    }

    #[test]
    fn write_without_diagnostics_is_compact_summary() {
        let theme = theme();
        let state = serde_json::json!({ "input": { "filePath": "foo.rs", "content": "x" } });
        let lines = write_file(Some(&state), "completed", &theme);
        assert_eq!(lines.len(), 1, "no content/diagnostics block");
        assert!(
            flatten(&lines).contains("Write foo.rs"),
            "compact summary only"
        );
    }

    #[test]
    fn task_completed_shows_title_duration_and_output() {
        let theme = theme();
        let state = serde_json::json!({
            "status": "completed",
            "input": { "subagent_type": "oracle", "description": "review the plan" },
            "output": "The plan looks solid. Ship it.",
            "time": { "start": 1000, "end": 3500 }
        });
        let rendered = flatten(&task(Some(&state), "completed", &theme));
        assert!(
            rendered.contains("Oracle Task"),
            "titlecased subagent: {rendered}"
        );
        assert!(
            rendered.contains("review the plan"),
            "description: {rendered}"
        );
        assert!(
            rendered.contains("done in 2.5s"),
            "duration (end-start)/1000: {rendered}"
        );
        assert!(
            rendered.contains("The plan looks solid"),
            "subagent output collapsed: {rendered}"
        );
    }
}
