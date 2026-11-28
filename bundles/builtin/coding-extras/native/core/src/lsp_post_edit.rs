//! The post-edit pipeline shared by `PostEditHook` and `apply_patch`: format
//! each edited file, then touch it in LSP and collect diagnostics.

use std::path::Path;
use std::path::PathBuf;

use serde_json::Value;

use hya_tool::ToolError;
use hya_tool::path::display_path;

use crate::service::CodingExtrasState;
use crate::utf8_bom;

const MAX_PER_FILE: usize = 20;

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

/// What [`format_and_diagnose`] did to the edited files.
pub(crate) struct PostEdit {
    /// One message per file the formatter (or the BOM restore after it) failed on.
    pub(crate) formatter_failures: Vec<String>,
    /// LSP diagnostics for the files, `None` when no language server is configured.
    pub(crate) diagnostics: Option<Result<Value, ToolError>>,
}

/// Format each of `paths` with the configured formatter, keeping its UTF-8 BOM
/// state, then touch them in LSP and collect diagnostics, so diagnostics see
/// the formatted bytes.
pub(crate) async fn format_and_diagnose(
    state: &CodingExtrasState,
    workdir: &Path,
    paths: &[PathBuf],
) -> PostEdit {
    let formatter = state.formatter();
    let mut formatter_failures = Vec::new();
    if formatter.is_configured() {
        for path in paths {
            let had_bom = tokio::fs::read(path)
                .await
                .is_ok_and(|bytes| bytes.starts_with(b"\xEF\xBB\xBF"));
            match formatter.format_file(workdir, path).await {
                Ok(true) => {
                    if let Err(error) = utf8_bom::sync_file(path, had_bom).await {
                        formatter_failures.push(format!(
                            "restore the UTF-8 BOM of {}: {error}",
                            display_path(path)
                        ));
                    }
                }
                Ok(false) => {}
                Err(error) => formatter_failures.push(error.to_string()),
            }
        }
    }
    let lsp = state.lsp();
    let diagnostics = if lsp.is_configured() {
        Some(touch_and_diagnose(&lsp, workdir, paths).await)
    } else {
        None
    };
    PostEdit {
        formatter_failures,
        diagnostics,
    }
}

async fn touch_and_diagnose(
    lsp: &crate::lsp_service::LspPlane,
    workdir: &Path,
    paths: &[PathBuf],
) -> Result<Value, ToolError> {
    for path in paths {
        lsp.touch_file(path, "document")
            .await
            .map_err(|error| ToolError::Other(error.to_string()))?;
    }
    let targets: Vec<_> = paths.iter().map(PathBuf::as_path).collect();
    lsp.diagnostics(workdir, &targets)
        .await
        .map_err(|error| ToolError::Other(error.to_string()))
}
