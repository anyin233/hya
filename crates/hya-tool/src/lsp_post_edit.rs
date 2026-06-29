use std::path::Path;
use std::path::PathBuf;

use serde_json::Value;

use crate::lsp_path::display_path;
use crate::lsp_plane::LspPlane;
use crate::tool::ToolError;

const MAX_PER_FILE: usize = 20;
const MAX_PROJECT_FILES: usize = 5;

pub(crate) async fn touch_and_diagnostics(lsp: &LspPlane, path: &Path) -> Result<Value, ToolError> {
    lsp.touch_file(path, "document")
        .await
        .map_err(|error| ToolError::Other(error.to_string()))?;
    lsp.diagnostics()
        .await
        .map_err(|error| ToolError::Other(error.to_string()))
}

pub(crate) fn append_edit_diagnostics(output: &mut String, file: &Path, diagnostics: &Value) {
    if let Some(block) = report_for_path(file, diagnostics) {
        output.push_str("\n\nLSP errors detected in this file, please fix:\n");
        output.push_str(&block);
    }
}

pub(crate) fn append_write_diagnostics(output: &mut String, file: &Path, diagnostics: &Value) {
    let current = path_key(file);
    if let Some(block) = report_for_key(&current, diagnostics.get(&current)) {
        output.push_str("\n\nLSP errors detected in this file, please fix:\n");
        output.push_str(&block);
    }

    let Some(files) = diagnostics.as_object() else {
        return;
    };
    let mut count = 0usize;
    for (path, issues) in files {
        if path == &current || count >= MAX_PROJECT_FILES {
            continue;
        }
        let Some(block) = report_for_key(path, Some(issues)) else {
            continue;
        };
        count += 1;
        output.push_str("\n\nLSP errors detected in other files:\n");
        output.push_str(&block);
    }
}

pub(crate) fn append_patch_diagnostics(
    output: &mut String,
    file: &Path,
    label: &str,
    diagnostics: &Value,
) {
    if let Some(block) = report_for_path(file, diagnostics) {
        output.push_str("\n\nLSP errors detected in ");
        output.push_str(label);
        output.push_str(", please fix:\n");
        output.push_str(&block);
    }
}

fn report_for_path(file: &Path, diagnostics: &Value) -> Option<String> {
    let key = path_key(file);
    report_for_key(&key, diagnostics.get(&key))
}

fn report_for_key(file: &str, issues: Option<&Value>) -> Option<String> {
    let errors: Vec<&Value> = issues?
        .as_array()?
        .iter()
        .filter(|issue| issue.get("severity").and_then(Value::as_u64) == Some(1))
        .collect();
    if errors.is_empty() {
        return None;
    }

    let lines = errors
        .iter()
        .take(MAX_PER_FILE)
        .map(|issue| pretty(issue))
        .collect::<Vec<_>>()
        .join("\n");
    let more = errors.len().saturating_sub(MAX_PER_FILE);
    let suffix = if more > 0 {
        format!("\n... and {more} more")
    } else {
        String::new()
    };
    Some(format!(
        "<diagnostics file=\"{file}\">\n{lines}{suffix}\n</diagnostics>"
    ))
}

fn pretty(issue: &Value) -> String {
    let line = issue
        .pointer("/range/start/line")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    let column = issue
        .pointer("/range/start/character")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    let message = issue.get("message").and_then(Value::as_str).unwrap_or("");
    format!("ERROR [{line}:{column}] {message}")
}

fn path_key(file: &Path) -> String {
    display_path(file)
}

pub(crate) async fn touch_many_and_diagnostics(
    lsp: &LspPlane,
    paths: &[PathBuf],
) -> Result<Value, ToolError> {
    for path in paths {
        lsp.touch_file(path, "document")
            .await
            .map_err(|error| ToolError::Other(error.to_string()))?;
    }
    lsp.diagnostics()
        .await
        .map_err(|error| ToolError::Other(error.to_string()))
}
