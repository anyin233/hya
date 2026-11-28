//! Standard pure-Rust `grep` tool for `hya-core`.
//!
//! Performs regex or literal traversal with `.gitignore` / `.ignore` respect,
//! [`hya_tool::ProjectScope`] enforcement, [`hya_tool::Action::Grep`]
//! permission checks, cooperative cancellation, and coding output capping
//! (`grep_budget` via [`hya_tool::ToolResultPolicy::Coding`]). Contains zero
//! hashline logic and zero external process dependencies.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::path::{display_path, resolve_file};
use hya_tool::{
    Action, Resource, Tool, ToolCtx, ToolError, ToolResultPolicy, cap_tool_output_with_policy,
    serialized_string_len,
};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{
    MAX_CODING_OUTPUT_BYTES, assert_external_search_target, check_cancel, closed_obj_schema,
    truncate_utf8, workdir_normalized,
};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const MAX_CONTEXT: usize = 5;
const MAX_DISPLAY_ROWS: usize = 2000;
const MAX_DISPLAY_GROUPS: usize = 100;
const MAX_WARNING_COUNT: usize = 16;
const MAX_WARNING_BYTES: usize = 4096;
const MAX_GLOB_BYTES: usize = 4096;
const MAX_GREP_LOGICAL_LINE_BYTES: usize = 1024 * 1024;
const MAX_IGNORE_LINE_BYTES: usize = 4096;
const MAX_IGNORE_RULES: usize = 10_000;
const DISPLAY_TRUNCATION_NOTICE: &str =
    " Some matching context was omitted because it exceeded the display budget.";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    glob: Option<String>,
    #[serde(rename = "ignoreCase", default)]
    ignore_case: Option<bool>,
    #[serde(default)]
    literal: Option<bool>,
    #[serde(default)]
    context: Option<i64>,
    #[serde(default)]
    limit: Option<usize>,
}

pub(super) struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }

    fn schema(&self) -> ToolSchema {
        closed_obj_schema(
            "grep",
            "Search file contents with a regex or literal pattern, respecting .gitignore and .ignore files.",
            json!({
                "pattern": {
                    "type": "string",
                    "description": "Search pattern (regex unless literal is true). Uses Rust `regex` crate syntax: no look-around ((?=...), (?!...), (?<=...), (?<!...)) and no backreferences (\\1) are supported. Escape literal ( ) [ ] { } . + * ? | ^ $ with a backslash, or set literal: true to search for exact text."
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search; defaults to the working directory"
                },
                "glob": {
                    "type": "string",
                    "maxLength": MAX_GLOB_BYTES,
                    "description": "File path filter, for example **/*.rs; prefix with ! to exclude; a directory name matches files under that directory"
                },
                "ignoreCase": {
                    "type": "boolean",
                    "description": "Match without regard to letter case"
                },
                "literal": {
                    "type": "boolean",
                    "description": "Treat the pattern as literal text instead of a regular expression"
                },
                "context": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_CONTEXT,
                    "description": "Number of context lines around each match (0–5, default 0); values outside the range are clamped"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LIMIT,
                    "description": "Maximum matched lines to return (1–200, default 50)"
                }
            }),
            &["pattern"],
        )
    }

    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::Coding
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        check_cancel(ctx)?;
        reject_null_fields(&input)?;
        let input: GrepArgs = super::args(input)?;
        let (context, context_note) = clamp_context(input.context);
        let limit = validate_limit(input.limit)?;
        validate_glob_pattern(input.glob.as_deref())?;
        let ignore_case = input.ignore_case.unwrap_or(false);
        let literal = input.literal.unwrap_or(false);
        let expression = if literal {
            regex::escape(&input.pattern)
        } else {
            input.pattern.clone()
        };
        let matcher = RegexBuilder::new(&expression)
            .case_insensitive(ignore_case)
            .build()
            .map_err(|error| regex_input_error(literal, &error))?;

        let workdir = workdir_normalized(&ctx.workdir);
        let root = input
            .path
            .as_deref()
            .map_or_else(|| workdir.clone(), |path| resolve_file(&workdir, path));

        ctx.permission
            .assert(Action::Grep, Resource::Glob(input.pattern.clone()))
            .await?;
        check_cancel(ctx)?;
        assert_external_search_target(ctx, &root).await?;
        check_cancel(ctx)?;

        let metadata = tokio::fs::metadata(&root).await.map_err(ToolError::Io)?;
        check_cancel(ctx)?;
        let (search_root, explicit_file) = if metadata.is_dir() {
            (root.clone(), None)
        } else if metadata.is_file() {
            let parent = root
                .parent()
                .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
            (parent, Some(root))
        } else {
            return Err(ToolError::Input(format!(
                "grep path is not a file or directory: {}",
                display_path(&root)
            )));
        };
        let ignore_base = if search_root.starts_with(&workdir) {
            workdir.clone()
        } else {
            search_root.clone()
        };

        let mut search = run_native_search(SearchRequest {
            search_root: search_root.clone(),
            explicit_file,
            ignore_base,
            glob_base: search_root,
            glob: input.glob,
            matcher,
            limit,
            cancel: ctx.cancel.clone(),
        })
        .await?;
        check_cancel(ctx)?;

        if let Some(note) = &context_note {
            search.warnings.insert(0, note.clone());
        }
        if search.total == 0 {
            return Ok(cap_tool_output_with_policy(
                no_matches(&input.pattern, search.warnings, context_note.as_deref()),
                ToolResultPolicy::Coding,
            ));
        }

        let mut output_parts = Vec::new();
        let mut output_bytes = 0usize;
        let mut display_bytes = 0usize;
        let mut total_rows = 0usize;
        let mut groups = Vec::new();
        let mut rendered_files = 0usize;
        let mut rendered_matches = Vec::new();
        let warnings = search.warnings;
        let mut display_truncated = false;

        for (path, match_lines) in search.matches {
            check_cancel(ctx)?;
            if groups.len() >= MAX_DISPLAY_GROUPS || total_rows >= MAX_DISPLAY_ROWS {
                display_truncated = true;
                break;
            }
            let Ok(bytes) = tokio::fs::read(&path).await else {
                continue;
            };
            if is_binary(&path, &bytes[..bytes.len().min(4096)]) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let lines = split_logical_lines(&text);
            let total_lines = lines.len();
            let valid_match_lines: Vec<usize> = match_lines
                .into_iter()
                .filter(|line| *line > 0 && *line <= total_lines)
                .collect();
            if valid_match_lines.is_empty() {
                continue;
            }
            let ranges = merged_ranges(&valid_match_lines, context, total_lines);
            if ranges.is_empty() {
                continue;
            }

            let display = display_path_for(&path, &workdir);
            let formatted_body = format_standard_ranges(&lines, &ranges, &valid_match_lines);
            let group_text = format!("{display}:\n{formatted_body}\n---");
            let group_serialized = serialized_string_len(&group_text).saturating_sub(2);
            let group_separator = if output_parts.is_empty() { 0 } else { 2 };
            let mut summary_with_file = summary_text(
                search.total,
                rendered_files.saturating_add(1),
                search.truncated,
                limit,
            );
            summary_with_file.push_str(DISPLAY_TRUNCATION_NOTICE);
            if let Some(note) = &context_note {
                summary_with_file.push_str(&format!(" ({note})"));
            }
            let summary_serialized = serialized_string_len(&summary_with_file).saturating_sub(2);
            if 2usize
                .saturating_add(output_bytes)
                .saturating_add(group_separator)
                .saturating_add(group_serialized)
                .saturating_add(2)
                .saturating_add(summary_serialized)
                > MAX_CODING_OUTPUT_BYTES
            {
                display_truncated = true;
                continue;
            }

            let row_budget = MAX_DISPLAY_ROWS.saturating_sub(total_rows);
            let (rows, rows_truncated) =
                display_rows(&lines, &ranges, &valid_match_lines, row_budget, &ctx.cancel)?;
            if rows_truncated {
                display_truncated = true;
            }
            let group_matches = rows
                .iter()
                .filter(|row| row["isMatch"].as_bool() == Some(true))
                .filter_map(|row| {
                    Some(json!({
                        "file": display.clone(),
                        "line": row["line"].as_u64()?,
                        "text": row["text"].as_str()?,
                    }))
                })
                .collect::<Vec<_>>();
            let row_count = rows.len();
            let group = json!({
                "path": display,
                "rows": rows,
            });
            let group_bytes = serde_json::to_vec(&group).map_err(ToolError::Json)?;
            if display_bytes.saturating_add(group_bytes.len()) > MAX_CODING_OUTPUT_BYTES {
                display_truncated = true;
                continue;
            }
            display_bytes = display_bytes.saturating_add(group_bytes.len());
            total_rows = total_rows.saturating_add(row_count);
            groups.push(group);
            output_bytes = output_bytes
                .saturating_add(group_separator)
                .saturating_add(group_serialized);
            output_parts.push(group_text);
            rendered_files += 1;
            rendered_matches.extend(group_matches);
        }
        check_cancel(ctx)?;

        let mut summary = summary_text(search.total, rendered_files, search.truncated, limit);
        if display_truncated {
            summary.push_str(DISPLAY_TRUNCATION_NOTICE);
        }
        if let Some(note) = &context_note {
            summary.push_str(&format!(" ({note})"));
        }
        let output = if output_parts.is_empty() {
            summary
        } else {
            output_parts.push(summary);
            output_parts.join("\n")
        };
        let mut metadata = json!({
            "matches": search.total,
            "files": rendered_files,
            "truncated": search.truncated || display_truncated,
            "display": {
                "groups": groups,
            },
        });
        if display_truncated {
            metadata["displayTruncated"] = json!(true);
        }
        let warnings = bound_warnings(warnings);
        if !warnings.is_empty() {
            metadata["warnings"] = json!(warnings);
        }

        let raw = json!({
            "title": input.pattern,
            "output": output,
            "metadata": metadata,
            "matches": rendered_matches,
            "total": search.total,
        });
        Ok(cap_tool_output_with_policy(raw, ToolResultPolicy::Coding))
    }
}

fn split_logical_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Format standard (non-hashline) line-numbered grep output:
/// `{line}:{text}` for matching lines and `{line}-{text}` for context lines,
/// separated by `--` between non-contiguous ranges.
fn format_standard_ranges(lines: &[&str], ranges: &[(usize, usize)], matches: &[usize]) -> String {
    let mut out = Vec::new();
    for (range_idx, &(start, end)) in ranges.iter().enumerate() {
        if range_idx > 0 {
            out.push("--".to_string());
        }
        for line_number in start..=end {
            let Some(text) = lines.get(line_number.saturating_sub(1)) else {
                continue;
            };
            let sep = if matches.binary_search(&line_number).is_ok() {
                ':'
            } else {
                '-'
            };
            out.push(format!("{line_number}{sep}{text}"));
        }
    }
    out.join("\n")
}

fn reject_null_fields(input: &Value) -> Result<(), ToolError> {
    let Some(object) = input.as_object() else {
        return Ok(());
    };
    for field in ["path", "glob", "ignoreCase", "literal", "context", "limit"] {
        if object.get(field).is_some_and(Value::is_null) {
            return Err(ToolError::Input(format!("{field} must not be null")));
        }
    }
    Ok(())
}

fn clamp_context(context: Option<i64>) -> (usize, Option<String>) {
    let requested = context.unwrap_or(0);
    let used =
        usize::try_from(requested.max(0)).map_or(MAX_CONTEXT, |value| value.min(MAX_CONTEXT));
    let in_range = usize::try_from(requested).is_ok_and(|value| value == used);
    let note = (!in_range).then(|| {
        format!("context clamped to {used}: requested {requested}, allowed 0–{MAX_CONTEXT}")
    });
    (used, note)
}

fn validate_limit(limit: Option<usize>) -> Result<usize, ToolError> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ToolError::Input(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    Ok(limit)
}

fn regex_input_error(literal: bool, error: &regex::Error) -> ToolError {
    if literal {
        return ToolError::Input(error.to_string());
    }
    ToolError::Input(format!(
        "{error}\nhya's grep uses Rust `regex` crate syntax: look-around ((?=...), (?!...), \
(?<=...), (?<!...)) and backreferences (\\1) are not supported. Escape literal ( ) [ ] {{ }} . \
+ * ? | ^ $ with a backslash, or set \"literal\": true to search for the exact text instead."
    ))
}

fn validate_glob_pattern(pattern: Option<&str>) -> Result<(), ToolError> {
    if pattern.is_some_and(|pattern| pattern.len() > MAX_GLOB_BYTES) {
        return Err(ToolError::Input(format!(
            "grep glob pattern exceeds {MAX_GLOB_BYTES} bytes"
        )));
    }
    Ok(())
}

fn no_matches(pattern: &str, warnings: Vec<String>, context_note: Option<&str>) -> Value {
    let mut metadata = json!({
        "matches": 0,
        "files": 0,
        "truncated": false,
        "display": { "groups": [] },
    });
    let warnings = bound_warnings(warnings);
    if !warnings.is_empty() {
        metadata["warnings"] = json!(warnings);
    }
    json!({
        "title": pattern,
        "output": match context_note {
            Some(note) => format!("No matches found for {pattern}. ({note})"),
            None => format!("No matches found for {pattern}."),
        },
        "metadata": metadata,
        "matches": [],
        "total": 0,
    })
}

fn summary_text(matches: usize, files: usize, truncated: bool, limit: usize) -> String {
    let match_word = if matches == 1 { "match" } else { "matches" };
    let file_word = if files == 1 { "file" } else { "files" };
    let suffix = if truncated {
        format!(" (truncated at {limit})")
    } else {
        String::new()
    };
    format!("{matches} {match_word} in {files} {file_word}.{suffix}")
}

struct SearchRequest {
    search_root: PathBuf,
    explicit_file: Option<PathBuf>,
    ignore_base: PathBuf,
    glob_base: PathBuf,
    glob: Option<String>,
    matcher: Regex,
    limit: usize,
    cancel: CancellationToken,
}

struct SearchResult {
    matches: BTreeMap<PathBuf, Vec<usize>>,
    total: usize,
    truncated: bool,
    warnings: Vec<String>,
}

enum SearchError {
    Cancelled,
    Io,
    Worker(String),
}

async fn run_native_search(request: SearchRequest) -> Result<SearchResult, ToolError> {
    let cancel = request.cancel.clone();
    let worker = tokio::task::spawn_blocking(move || search_blocking(request));
    let joined = tokio::select! {
        result = worker => result.map_err(|error| SearchError::Worker(error.to_string())),
        _ = cancel.cancelled() => return Err(ToolError::Cancelled),
    };
    match joined {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(SearchError::Cancelled)) | Err(SearchError::Cancelled) => Err(ToolError::Cancelled),
        Ok(Err(SearchError::Io)) | Err(SearchError::Io) => Err(ToolError::Other(
            "grep worker failed while reading".to_string(),
        )),
        Ok(Err(SearchError::Worker(error))) | Err(SearchError::Worker(error)) => {
            Err(ToolError::Other(format!("grep worker failed: {error}")))
        }
    }
}

fn retain_lexical_match(result: &mut SearchResult, path: &Path, line: usize, limit: usize) {
    if result.total < limit {
        result.total = result.total.saturating_add(1);
        result
            .matches
            .entry(path.to_path_buf())
            .or_default()
            .push(line);
        return;
    }

    result.truncated = true;
    let Some((greatest_path, greatest_lines)) = result.matches.last_key_value() else {
        return;
    };
    let Some(&greatest_line) = greatest_lines.last() else {
        return;
    };
    if (path, line) >= (greatest_path.as_path(), greatest_line) {
        return;
    }
    let greatest_path = greatest_path.clone();
    let remove_path = if let Some(lines) = result.matches.get_mut(&greatest_path) {
        lines.pop();
        lines.is_empty()
    } else {
        false
    };
    if remove_path {
        result.matches.remove(&greatest_path);
    }
    result
        .matches
        .entry(path.to_path_buf())
        .or_default()
        .push(line);
}

fn search_blocking(request: SearchRequest) -> Result<SearchResult, SearchError> {
    if request.cancel.is_cancelled() {
        return Err(SearchError::Cancelled);
    }
    let mut result = SearchResult {
        matches: BTreeMap::new(),
        total: 0,
        truncated: false,
        warnings: Vec::new(),
    };
    let mut rules = IgnoreRules::from_ancestor(
        &request.ignore_base,
        &request.search_root,
        &request.cancel,
        &mut result.warnings,
    )?;
    if let Some(path) = request.explicit_file.as_ref() {
        rules.load_directory(&request.search_root, &request.cancel, &mut result.warnings)?;
        if !(rules.matches_with_cancel(path, false, &request.cancel)?)
            && glob_allows(request.glob.as_deref(), path, &request.glob_base)
        {
            scan_file(path, &request, &mut result)?;
        }
    } else {
        visit_directory(&request.search_root, &mut rules, &request, &mut result)?;
    }
    Ok(result)
}

fn visit_directory(
    directory: &Path,
    rules: &mut IgnoreRules,
    request: &SearchRequest,
    result: &mut SearchResult,
) -> Result<(), SearchError> {
    if request.cancel.is_cancelled() {
        return Err(SearchError::Cancelled);
    }
    let inherited_rule_count = rules.rules.len();
    rules.load_directory(directory, &request.cancel, &mut result.warnings)?;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(_) => {
            rules.rules.truncate(inherited_rule_count);
            return Ok(());
        }
    };
    for entry in entries {
        if request.cancel.is_cancelled() {
            return Err(SearchError::Cancelled);
        }
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') || file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if rules.matches_with_cancel(&path, true, &request.cancel)? {
                continue;
            }
            visit_directory(&path, rules, request, result)?;
        } else if file_type.is_file()
            && !(rules.matches_with_cancel(&path, false, &request.cancel)?)
            && glob_allows(request.glob.as_deref(), &path, &request.glob_base)
        {
            scan_file(path.as_path(), request, result)?;
        }
    }
    rules.rules.truncate(inherited_rule_count);
    Ok(())
}

fn scan_file(
    path: &Path,
    request: &SearchRequest,
    result: &mut SearchResult,
) -> Result<(), SearchError> {
    if request.cancel.is_cancelled() {
        return Err(SearchError::Cancelled);
    }
    let Ok(mut sample_file) = File::open(path) else {
        return Ok(());
    };
    let mut sample = [0_u8; 4096];
    let Ok(sample_len) = sample_file.read(&mut sample) else {
        return Ok(());
    };
    if request.cancel.is_cancelled() {
        return Err(SearchError::Cancelled);
    }
    if is_binary(path, &sample[..sample_len]) {
        return Ok(());
    }
    let Ok(file) = File::open(path) else {
        return Ok(());
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut line_number = 0usize;
    let mut oversized_warning_emitted = false;
    loop {
        let line_kind = match read_bounded_line(
            &mut reader,
            &mut line,
            MAX_GREP_LOGICAL_LINE_BYTES,
            &request.cancel,
        ) {
            Ok(line) => line,
            Err(SearchError::Cancelled) => return Err(SearchError::Cancelled),
            Err(SearchError::Io) => return Ok(()),
            Err(error) => return Err(error),
        };
        match line_kind {
            StreamLine::Eof => break,
            StreamLine::Oversized => {
                line_number = line_number.saturating_add(1);
                if !oversized_warning_emitted {
                    push_bounded_warning(
                        &mut result.warnings,
                        format!(
                            "Skipping oversized logical line in {} (limit {} bytes)",
                            display_path(path),
                            MAX_GREP_LOGICAL_LINE_BYTES
                        ),
                    );
                    oversized_warning_emitted = true;
                }
            }
            StreamLine::Retained => {
                line_number = line_number.saturating_add(1);
                let text = String::from_utf8_lossy(&line);
                let matched = request.matcher.is_match(&text);
                if request.cancel.is_cancelled() {
                    return Err(SearchError::Cancelled);
                }
                if matched {
                    retain_lexical_match(result, path, line_number, request.limit);
                }
            }
        }
    }
    Ok(())
}

enum StreamLine {
    Eof,
    Retained,
    Oversized,
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    retained: &mut Vec<u8>,
    max_bytes: usize,
    cancel: &CancellationToken,
) -> Result<StreamLine, SearchError> {
    retained.clear();
    let mut saw_bytes = false;
    let mut oversized = false;
    loop {
        if cancel.is_cancelled() {
            return Err(SearchError::Cancelled);
        }
        let buffer = reader.fill_buf().map_err(|_| SearchError::Io)?;
        if buffer.is_empty() {
            if !saw_bytes {
                return Ok(StreamLine::Eof);
            }
            return Ok(if oversized {
                StreamLine::Oversized
            } else {
                StreamLine::Retained
            });
        }

        let delimiter = buffer
            .iter()
            .position(|byte| matches!(*byte, b'\n' | b'\r'));
        let content_end = delimiter.unwrap_or(buffer.len());
        if delimiter.is_some() || content_end > 0 {
            saw_bytes = true;
        }
        if !oversized {
            if retained.len().saturating_add(content_end) <= max_bytes {
                retained.extend_from_slice(&buffer[..content_end]);
            } else {
                oversized = true;
                retained.clear();
            }
        }

        let Some(delimiter) = delimiter else {
            let consumed = buffer.len();
            reader.consume(consumed);
            continue;
        };
        let delimiter_is_cr = buffer[delimiter] == b'\r';
        reader.consume(delimiter + 1);
        if delimiter_is_cr {
            if cancel.is_cancelled() {
                return Err(SearchError::Cancelled);
            }
            let next = reader.fill_buf().map_err(|_| SearchError::Io)?;
            if next.first() == Some(&b'\n') {
                reader.consume(1);
            }
        }
        return Ok(if oversized {
            StreamLine::Oversized
        } else {
            StreamLine::Retained
        });
    }
}

fn is_binary(path: &Path, bytes: &[u8]) -> bool {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "zip"
            | "tar"
            | "gz"
            | "exe"
            | "dll"
            | "so"
            | "class"
            | "jar"
            | "war"
            | "7z"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "odt"
            | "ods"
            | "odp"
            | "bin"
            | "dat"
            | "obj"
            | "o"
            | "a"
            | "lib"
            | "wasm"
            | "pyc"
            | "pyo"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "pdf"
    ) {
        return true;
    }
    if bytes.is_empty() {
        return false;
    }
    if bytes.contains(&0) {
        return true;
    }
    let non_printable = bytes
        .iter()
        .filter(|byte| **byte < 9 || (**byte > 13 && **byte < 32))
        .count();
    non_printable.saturating_mul(10) > bytes.len().saturating_mul(3)
}

fn glob_allows(pattern: Option<&str>, path: &Path, base: &Path) -> bool {
    let Some(pattern) = pattern else {
        return true;
    };
    let pattern = pattern.replace('\\', "/");
    let excluded = pattern.strip_prefix('!');
    let pattern = excluded.unwrap_or(&pattern);
    let relative = path
        .strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let directory_pattern = pattern.trim_end_matches('/');
    let directory_literal = !directory_pattern.is_empty()
        && !directory_pattern
            .chars()
            .any(|character| matches!(character, '*' | '?' | '['))
        && base.join(directory_pattern).is_dir();
    let pattern = if directory_literal {
        format!("{directory_pattern}/**")
    } else {
        pattern.to_string()
    };
    let candidate = if pattern.contains('/') {
        relative.as_str()
    } else {
        relative.rsplit('/').next().unwrap_or(relative.as_str())
    };
    let matched = wildcard_match(&pattern, candidate);
    if excluded.is_some() {
        !matched
    } else {
        matched
    }
}

fn merged_ranges(lines: &[usize], context: usize, total_lines: usize) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &line in lines {
        let start = line.saturating_sub(context).max(1);
        let end = line.saturating_add(context).min(total_lines);
        if let Some(last) = ranges.last_mut()
            && start <= last.1.saturating_add(1)
        {
            last.1 = last.1.max(end);
        } else {
            ranges.push((start, end));
        }
    }
    ranges
}

fn display_rows(
    lines: &[&str],
    ranges: &[(usize, usize)],
    matches: &[usize],
    max_rows: usize,
    cancel: &CancellationToken,
) -> Result<(Vec<Value>, bool), ToolError> {
    let mut rows = Vec::new();
    for &(start, end) in ranges {
        for line_number in start..=end {
            if cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let Some(text) = lines.get(line_number.saturating_sub(1)) else {
                continue;
            };
            if rows.len() >= max_rows {
                return Ok((rows, true));
            }
            rows.push(json!({
                "line": line_number,
                "text": text,
                "isMatch": matches.binary_search(&line_number).is_ok(),
            }));
        }
    }
    Ok((rows, false))
}

fn display_path_for(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .map_or_else(|_| display_path(path), display_path)
}

fn bound_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut bounded = Vec::new();
    let mut bytes = 0usize;
    for warning in warnings {
        if bounded.len() >= MAX_WARNING_COUNT {
            break;
        }
        let remaining = MAX_WARNING_BYTES.saturating_sub(bytes);
        if remaining == 0 {
            break;
        }
        let warning = truncate_utf8(&warning, remaining);
        bytes = bytes.saturating_add(warning.len());
        bounded.push(warning);
    }
    bounded
}

fn push_bounded_warning(warnings: &mut Vec<String>, warning: String) {
    if warnings.len() >= MAX_WARNING_COUNT {
        return;
    }
    let used = warnings.iter().map(String::len).sum::<usize>();
    let remaining = MAX_WARNING_BYTES.saturating_sub(used);
    if remaining > 0 {
        warnings.push(truncate_utf8(&warning, remaining));
    }
}

#[derive(Clone)]
struct IgnoreRule {
    base: PathBuf,
    pattern: String,
    directory: bool,
    negated: bool,
    anchored: bool,
}

#[derive(Clone, Default)]
struct IgnoreRules {
    rules: Vec<IgnoreRule>,
}

impl IgnoreRules {
    fn from_ancestor(
        ancestor: &Path,
        target: &Path,
        cancel: &CancellationToken,
        warnings: &mut Vec<String>,
    ) -> Result<Self, SearchError> {
        let mut rules = Self::default();
        if target.starts_with(ancestor) {
            let relative = target.strip_prefix(ancestor).unwrap_or(target);
            let mut components = relative.components().peekable();
            if components.peek().is_some() {
                rules.load_directory(ancestor, cancel, warnings)?;
                let mut directory = ancestor.to_path_buf();
                while let Some(component) = components.next() {
                    if cancel.is_cancelled() {
                        return Err(SearchError::Cancelled);
                    }
                    directory.push(component.as_os_str());
                    if components.peek().is_some() {
                        rules.load_directory(&directory, cancel, warnings)?;
                    }
                }
            }
        }
        Ok(rules)
    }

    fn load_directory(
        &mut self,
        directory: &Path,
        cancel: &CancellationToken,
        warnings: &mut Vec<String>,
    ) -> Result<(), SearchError> {
        for name in [".gitignore", ".ignore"] {
            if cancel.is_cancelled() {
                return Err(SearchError::Cancelled);
            }
            let path = directory.join(name);
            let Ok(file) = File::open(&path) else {
                continue;
            };
            let mut reader = BufReader::new(file);
            let mut line_bytes = Vec::new();
            let mut parsed = Vec::new();
            let mut skip_source = false;
            loop {
                if cancel.is_cancelled() {
                    return Err(SearchError::Cancelled);
                }
                let line_kind = match read_bounded_line(
                    &mut reader,
                    &mut line_bytes,
                    MAX_IGNORE_LINE_BYTES,
                    cancel,
                ) {
                    Ok(line) => line,
                    Err(SearchError::Cancelled) => return Err(SearchError::Cancelled),
                    Err(SearchError::Io) => {
                        skip_source = true;
                        break;
                    }
                    Err(error) => return Err(error),
                };
                match line_kind {
                    StreamLine::Eof => break,
                    StreamLine::Oversized => {
                        push_bounded_warning(
                            warnings,
                            format!(
                                "Skipping oversized ignore file {} (limit {} bytes per rule)",
                                display_path(&path),
                                MAX_IGNORE_LINE_BYTES
                            ),
                        );
                        skip_source = true;
                        break;
                    }
                    StreamLine::Retained => {
                        let Ok(line) = std::str::from_utf8(&line_bytes) else {
                            skip_source = true;
                            break;
                        };
                        if let Some(rule) = parse_ignore_rule(line, directory) {
                            if parsed.len() >= MAX_IGNORE_RULES {
                                push_bounded_warning(
                                    warnings,
                                    format!(
                                        "Skipping oversized ignore file {} (more than {} rules)",
                                        display_path(&path),
                                        MAX_IGNORE_RULES
                                    ),
                                );
                                skip_source = true;
                                break;
                            }
                            parsed.push(rule);
                        }
                    }
                }
            }
            if !skip_source {
                if self.rules.len().saturating_add(parsed.len()) > MAX_IGNORE_RULES {
                    push_bounded_warning(
                        warnings,
                        format!(
                            "Skipping oversized ignore file {} (more than {} inherited rules)",
                            display_path(&path),
                            MAX_IGNORE_RULES
                        ),
                    );
                } else {
                    self.rules.extend(parsed);
                }
            }
        }
        Ok(())
    }

    fn matches_with_cancel(
        &self,
        path: &Path,
        is_directory: bool,
        cancel: &CancellationToken,
    ) -> Result<bool, SearchError> {
        let mut ignored = false;
        for rule in &self.rules {
            if cancel.is_cancelled() {
                return Err(SearchError::Cancelled);
            }
            if rule.matches(path, is_directory) {
                ignored = !rule.negated;
            }
        }
        Ok(ignored)
    }
}

impl IgnoreRule {
    fn matches(&self, path: &Path, is_directory: bool) -> bool {
        let Ok(relative) = path.strip_prefix(&self.base) else {
            return false;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if relative.is_empty() {
            return false;
        }
        let pattern = self.pattern.trim_end_matches('/');
        if self.directory {
            let components = relative.split('/').collect::<Vec<_>>();
            let end = if is_directory {
                components.len()
            } else {
                components.len().saturating_sub(1)
            };
            if self.anchored || pattern.contains('/') {
                return (1..=end)
                    .any(|index| wildcard_match(pattern, &components[..index].join("/")));
            }
            return components[..end]
                .iter()
                .any(|component| wildcard_match(pattern, component));
        }
        if self.anchored || pattern.contains('/') {
            wildcard_match(pattern, &relative)
        } else {
            relative
                .rsplit('/')
                .next()
                .is_some_and(|name| wildcard_match(pattern, name))
        }
    }
}

fn parse_ignore_rule(line: &str, base: &Path) -> Option<IgnoreRule> {
    let line = trim_ignore_whitespace(line);
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (negated, line) = if let Some(stripped) = line.strip_prefix('!') {
        (true, stripped)
    } else {
        (false, line)
    };
    if line.is_empty() {
        return None;
    }
    let anchored = line.starts_with('/');
    let directory = line.ends_with('/');
    let pattern = line
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string();
    (!pattern.is_empty()).then_some(IgnoreRule {
        base: base.to_path_buf(),
        pattern,
        directory,
        negated,
        anchored,
    })
}

fn trim_ignore_whitespace(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut end = bytes.len();
    while end > 0 && matches!(bytes[end - 1], b' ' | b'\t') {
        let mut backslashes = 0usize;
        let mut index = end - 1;
        while index > 0 && bytes[index - 1] == b'\\' {
            backslashes += 1;
            index -= 1;
        }
        if backslashes % 2 == 1 {
            break;
        }
        end -= 1;
    }
    &line[..end]
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut states = vec![false; value.len().saturating_add(1)];
    let mut next = vec![false; value.len().saturating_add(1)];
    states[0] = true;
    let mut pattern_index = 0usize;

    while pattern_index < pattern.len() {
        next.fill(false);
        if pattern.get(pattern_index..pattern_index.saturating_add(3)) == Some(b"**/") {
            let mut reachable = false;
            for value_index in 0..=value.len() {
                reachable |= states[value_index];
                next[value_index] |= states[value_index];
                if reachable && value.get(value_index) == Some(&b'/') {
                    next[value_index + 1] = true;
                }
            }
            pattern_index = pattern_index.saturating_add(3);
        } else if pattern.get(pattern_index..pattern_index.saturating_add(2)) == Some(b"**") {
            let mut reachable = false;
            for value_index in 0..=value.len() {
                reachable |= states[value_index];
                next[value_index] = reachable;
            }
            pattern_index = pattern_index.saturating_add(2);
        } else if pattern[pattern_index] == b'*' {
            let mut reachable = false;
            for value_index in 0..=value.len() {
                reachable |= states[value_index];
                next[value_index] = reachable;
                if value.get(value_index) == Some(&b'/') {
                    reachable = false;
                }
            }
            pattern_index = pattern_index.saturating_add(1);
        } else if let Some(literal) = escaped_literal(pattern, pattern_index) {
            for value_index in 0..value.len() {
                if states[value_index] && value[value_index] == literal {
                    next[value_index + 1] = true;
                }
            }
            pattern_index = pattern_index.saturating_add(2);
        } else if pattern[pattern_index] == b'?' {
            for value_index in 0..value.len() {
                if states[value_index] && value[value_index] != b'/' {
                    next[value_index + 1] = true;
                }
            }
            pattern_index = pattern_index.saturating_add(1);
        } else if let Some((_, next_pattern_index)) =
            bracket_class_matches(pattern, pattern_index, 0)
        {
            for value_index in 0..value.len() {
                if !states[value_index] || value[value_index] == b'/' {
                    continue;
                }
                if bracket_class_matches(pattern, pattern_index, value[value_index])
                    .is_some_and(|(matched, _)| matched)
                {
                    next[value_index + 1] = true;
                }
            }
            pattern_index = next_pattern_index;
        } else {
            let literal = pattern[pattern_index];
            for value_index in 0..value.len() {
                if states[value_index] && value[value_index] == literal {
                    next[value_index + 1] = true;
                }
            }
            pattern_index = pattern_index.saturating_add(1);
        }
        std::mem::swap(&mut states, &mut next);
    }

    states[value.len()]
}

fn escaped_literal(pattern: &[u8], start: usize) -> Option<u8> {
    (pattern.get(start) == Some(&b'\\')).then(|| pattern.get(start + 1).copied())?
}

fn bracket_class_matches(pattern: &[u8], start: usize, value: u8) -> Option<(bool, usize)> {
    if pattern.get(start) != Some(&b'[') || pattern.get(start + 1) == Some(&b']') {
        return None;
    }
    let mut index = start + 1;
    let negated = matches!(pattern.get(index), Some(b'!' | b'^'));
    if negated {
        index += 1;
    }
    let mut matched = false;
    while index < pattern.len() {
        if pattern[index] == b']' {
            return Some((if negated { !matched } else { matched }, index + 1));
        }
        if index + 2 < pattern.len() && pattern[index + 1] == b'-' && pattern[index + 2] != b']' {
            let (lower, upper) = if pattern[index] <= pattern[index + 2] {
                (pattern[index], pattern[index + 2])
            } else {
                (pattern[index + 2], pattern[index])
            };
            matched |= lower <= value && value <= upper;
            index += 3;
        } else {
            matched |= pattern[index] == value;
            index += 1;
        }
    }
    None
}
