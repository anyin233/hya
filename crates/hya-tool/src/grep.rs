//! Native hashline-aware Grep adapter.
//!
//! This module ports the observable Grep behavior of `pi-hashline-edit` 0.8.3
//! (MIT, git head `ba7db9943d0f58499b24c1f6bd64722580f772a5`) without making
//! ripgrep or another process part of hya's tool runtime. Traversal and matching
//! happen in a cancellable blocking worker; the shared hashline runtime owns
//! normalized reloads and recovery snapshots.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::hashline::{
    HashlineRuntime, MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_TARGETS, ReadRuntimeError,
};
use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::permission::{Action, Resource};
use crate::read_media::{ReadFileKind, classify_file};
use crate::tool::{Tool, ToolCtx, ToolError, ToolResultPolicy, assert_external_directory_lexical};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const MAX_CONTEXT: usize = 5;
const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const MAX_DISPLAY_ROWS: usize = 4096;
const MAX_WARNING_COUNT: usize = 16;
const MAX_WARNING_BYTES: usize = 4096;
const MAX_GLOB_BYTES: usize = 4096;
const MAX_GREP_LOGICAL_LINE_BYTES: usize = 1024 * 1024;
const MAX_IGNORE_LINE_BYTES: usize = 4096;
const MAX_IGNORE_RULES: usize = 10_000;
const DISPLAY_TRUNCATION_NOTICE: &str =
    " Some matching context was omitted because it exceeded the display budget.";
/// Accepted Grep arguments. The closed Serde object intentionally has no
/// `include` compatibility field: `glob` is the only filename filter.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepInput {
    /// Regex or literal text to search for.
    pattern: String,
    /// File or directory to search, relative to the session workdir when not absolute.
    #[serde(default)]
    path: Option<String>,
    /// Filename/path glob filter.
    #[serde(default)]
    glob: Option<String>,
    /// Match letters without regard to case.
    #[serde(rename = "ignoreCase", default)]
    ignore_case: Option<bool>,
    /// Treat `pattern` as literal text instead of a regex.
    #[serde(default)]
    literal: Option<bool>,
    /// Number of surrounding context lines.
    #[serde(default)]
    context: Option<usize>,
    /// Maximum number of matched lines to retain.
    #[serde(default)]
    limit: Option<usize>,
}

/// Public native Grep adapter backed by the registry's shared hashline runtime.
pub struct GrepTool {
    runtime: Arc<HashlineRuntime>,
}

impl GrepTool {
    /// Construct a standalone Grep adapter with an isolated runtime.
    ///
    /// Registry construction should use [`Self::with_runtime`] so Read, Edit,
    /// Write, and Grep share recovery snapshots.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a Grep adapter using a registry-owned shared runtime.
    ///
    /// # Parameters
    /// - `runtime`: Runtime shared by the registry's filesystem coding tools.
    ///
    /// # Returns
    /// A Grep adapter that records snapshots in `runtime` after rendering.
    #[must_use]
    pub(crate) fn with_runtime(runtime: Arc<HashlineRuntime>) -> Self {
        Self { runtime }
    }
}

impl Default for GrepTool {
    /// Construct a standalone Grep adapter with an isolated runtime.
    fn default() -> Self {
        Self {
            runtime: Arc::new(HashlineRuntime::new()),
        }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("grep"),
            description: "Search file contents with a regex or literal pattern.".to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern (regex unless literal is true)"
                    },
                    "path": {
                        "type": "string",
                        "description": "File or directory to search; defaults to the workdir"
                    },
                    "glob": {
                        "type": "string",
                        "maxLength": MAX_GLOB_BYTES,
                        "description": "Filename/path glob filter, for example **/*.rs"
                    },
                    "ignoreCase": {
                        "type": "boolean",
                        "description": "Match without regard to letter case"
                    },
                    "literal": {
                        "type": "boolean",
                        "description": "Treat the pattern as literal text"
                    },
                    "context": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 5,
                        "description": "Number of context lines around each match"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 200,
                        "description": "Maximum matched lines to return"
                    }
                },
                "required": ["pattern"]
            }),
            output_schema: None,
        }
    }
    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::Coding
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        check_cancel(ctx)?;
        reject_null_fields(&input)?;
        let input: GrepInput =
            serde_json::from_value(input).map_err(|error| ToolError::Input(error.to_string()))?;
        let context = validate_context(input.context)?;
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
            .map_err(|error| ToolError::Input(error.to_string()))?;

        let workdir = normalize(&absolutize(&ctx.workdir));
        let root = input
            .path
            .as_deref()
            .map_or_else(|| workdir.clone(), |path| resolve_file(&workdir, path));
        ctx.permission
            .assert(Action::Grep, Resource::Glob(input.pattern.clone()))
            .await?;
        check_cancel(ctx)?;
        assert_external_directory_lexical(ctx, &root).await?;
        check_cancel(ctx)?;

        let metadata = tokio::fs::metadata(&root).await.map_err(ToolError::Io)?;
        check_cancel(ctx)?;
        let is_directory = metadata.is_dir();

        let (search_root, explicit_file) = if is_directory {
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
        let search = run_native_search(SearchRequest {
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

        if search.total == 0 {
            return Ok(no_matches(&input.pattern, search.warnings));
        }

        let mut output_parts = Vec::new();
        let mut output_bytes = 0usize;
        let mut display_bytes = 0usize;
        let mut groups = Vec::new();
        let mut rendered_files = 0usize;
        let mut rendered_matches = Vec::new();
        let mut warnings = search.warnings;
        let mut display_truncated = false;
        let mut pending_snapshots: VecDeque<(PathBuf, String)> = VecDeque::new();
        let mut pending_snapshot_bytes = 0usize;

        for (path, match_lines) in search.matches {
            check_cancel(ctx)?;
            let kind = match classify_file(&path).await {
                Ok(kind) => kind,
                Err(_) => continue,
            };
            if !matches!(kind, ReadFileKind::Text) {
                continue;
            }
            let loaded = match self
                .runtime
                .load_text_for_grep(&path, ctx.cancel.clone())
                .await
            {
                Ok(loaded) => loaded,
                Err(ReadRuntimeError::Cancelled) => return Err(ToolError::Cancelled),
                Err(ReadRuntimeError::Io(_)) => continue,
                Err(ReadRuntimeError::Hashline(error)) => {
                    return Err(ToolError::Input(error.diagnostic()));
                }
            };
            check_cancel(ctx)?;
            let valid_match_lines = match_lines
                .into_iter()
                .filter(|line| *line > 0 && *line <= loaded.total_lines)
                .collect::<Vec<_>>();
            if valid_match_lines.is_empty() {
                continue;
            }
            let ranges = merged_ranges(&valid_match_lines, context, loaded.total_lines);
            if ranges.is_empty() {
                continue;
            }
            let hashline =
                match self
                    .runtime
                    .format_hashline_ranges(&loaded.text, &ranges, &ctx.cancel)
                {
                    Ok(hashline) => hashline,
                    Err(ReadRuntimeError::Cancelled) => return Err(ToolError::Cancelled),
                    Err(ReadRuntimeError::Io(error)) => return Err(ToolError::Io(error)),
                    Err(ReadRuntimeError::Hashline(_)) => {
                        display_truncated = true;
                        push_bounded_warning(
                            &mut warnings,
                            format!(
                                "Skipping over-budget Grep display for {}",
                                display_path(&loaded.requested_path)
                            ),
                        );
                        continue;
                    }
                };
            let display = display_path_for(&loaded.requested_path, &workdir);
            let group_text = format!("{display}:\n{hashline}\n---");
            let group_separator = usize::from(!output_parts.is_empty());
            let mut summary_with_file = summary_text(
                search.total,
                rendered_files.saturating_add(1),
                search.truncated,
                limit,
            );
            summary_with_file.push_str(DISPLAY_TRUNCATION_NOTICE);
            if output_bytes
                .saturating_add(group_separator)
                .saturating_add(group_text.len())
                .saturating_add(1)
                .saturating_add(summary_with_file.len())
                > MAX_OUTPUT_BYTES
            {
                display_truncated = true;
                continue;
            }

            let (rows, rows_truncated) =
                display_rows(&loaded.text, &ranges, &valid_match_lines, &ctx.cancel)?;
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
            let group = json!({
                "path": display,
                "rows": rows,
            });
            let group_bytes = serde_json::to_vec(&group).map_err(ToolError::Json)?;
            if display_bytes.saturating_add(group_bytes.len()) > MAX_OUTPUT_BYTES {
                display_truncated = true;
                continue;
            }
            display_bytes = display_bytes.saturating_add(group_bytes.len());
            groups.push(group);
            output_bytes = output_bytes
                .saturating_add(group_separator)
                .saturating_add(group_text.len());
            output_parts.push(group_text);
            rendered_files += 1;
            rendered_matches.extend(group_matches);
            warnings.extend(loaded.warnings);
            let snapshot_bytes = loaded.text.len();
            if snapshot_bytes <= MAX_SNAPSHOT_BYTES {
                while pending_snapshots.len() >= MAX_SNAPSHOT_TARGETS
                    || pending_snapshot_bytes.saturating_add(snapshot_bytes) > MAX_SNAPSHOT_BYTES
                {
                    let Some((_, evicted)) = pending_snapshots.pop_front() else {
                        break;
                    };
                    pending_snapshot_bytes = pending_snapshot_bytes.saturating_sub(evicted.len());
                }
                pending_snapshot_bytes = pending_snapshot_bytes.saturating_add(snapshot_bytes);
                pending_snapshots.push_back((loaded.path, loaded.text));
            }
        }
        check_cancel(ctx)?;
        for (path, text) in pending_snapshots {
            self.runtime
                .remember_grep_snapshot(ctx.session, &workdir, &path, &text);
        }

        let mut summary = summary_text(search.total, rendered_files, search.truncated, limit);
        if display_truncated {
            summary.push_str(DISPLAY_TRUNCATION_NOTICE);
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
            "truncated": search.truncated,
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

        Ok(json!({
            "title": input.pattern,
            "output": output,
            "metadata": metadata,
            "matches": rendered_matches,
            "total": search.total,
        }))
    }
}

/// Reject JSON null for optional schema fields, which Serde's `Option` otherwise accepts.
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

/// Validate the bounded context argument and apply the pinned default.
fn validate_context(context: Option<usize>) -> Result<usize, ToolError> {
    let context = context.unwrap_or(0);
    if context > MAX_CONTEXT {
        return Err(ToolError::Input(format!(
            "context must be between 0 and {MAX_CONTEXT}"
        )));
    }
    Ok(context)
}

/// Validate the bounded match limit and apply the pinned default.
fn validate_limit(limit: Option<usize>) -> Result<usize, ToolError> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ToolError::Input(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    Ok(limit)
}

/// Reject a caller-provided filename glob that exceeds the native matcher bound.
fn validate_glob_pattern(pattern: Option<&str>) -> Result<(), ToolError> {
    if pattern.is_some_and(|pattern| pattern.len() > MAX_GLOB_BYTES) {
        return Err(ToolError::Input(format!(
            "grep glob pattern exceeds {MAX_GLOB_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Return the stable no-match result envelope, including bounded traversal warnings.
fn no_matches(pattern: &str, warnings: Vec<String>) -> Value {
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
        "output": format!("No matches found for {pattern}."),
        "metadata": metadata,
        "matches": [],
        "total": 0,
    })
}

/// Build the package-compatible summary line.
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

/// Check the call cancellation token before any potentially expensive stage.
fn check_cancel(ctx: &ToolCtx) -> Result<(), ToolError> {
    if ctx.cancel.is_cancelled() {
        Err(ToolError::Cancelled)
    } else {
        Ok(())
    }
}

/// Native blocking-search request transferred to the worker thread.
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

/// Search output retaining only the bounded match events and deterministic paths.
struct SearchResult {
    matches: BTreeMap<PathBuf, Vec<usize>>,
    total: usize,
    truncated: bool,
    warnings: Vec<String>,
}

/// Blocking worker failure that remains content-free at the Tool boundary.
enum SearchError {
    Cancelled,
    Io,
    Worker(String),
}

/// Run native traversal without blocking the Tokio executor.
async fn run_native_search(request: SearchRequest) -> Result<SearchResult, ToolError> {
    let cancel = request.cancel.clone();
    let worker = tokio::task::spawn_blocking(move || search_blocking(request));
    let joined = tokio::select! {
        result = worker => result.map_err(|error| SearchError::Worker(error.to_string())),
        _ = cancel.cancelled() => return Err(ToolError::Cancelled),
    };
    match joined {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(SearchError::Cancelled)) => Err(ToolError::Cancelled),
        Ok(Err(SearchError::Io)) => Err(ToolError::Other(
            "grep worker failed while reading".to_string(),
        )),
        Ok(Err(SearchError::Worker(error))) => {
            Err(ToolError::Other(format!("grep worker failed: {error}")))
        }
        Err(SearchError::Cancelled) => Err(ToolError::Cancelled),
        Err(SearchError::Io) => Err(ToolError::Other(
            "grep worker failed while reading".to_string(),
        )),
        Err(SearchError::Worker(error)) => {
            Err(ToolError::Other(format!("grep worker failed: {error}")))
        }
    }
}

/// Retain one matched line in the bounded lexical result set.
///
/// # Parameters
/// - `result`: Search state holding at most the caller's match limit.
/// - `path`: Candidate file path.
/// - `line`: One-based matched line number.
/// - `limit`: Positive validated match capacity.
///
/// # Returns
/// Nothing. Observing any extra match marks truncation; a lexically earlier
/// candidate replaces the current greatest retained row.
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

/// Traverse all candidates while retaining only the lexical first `limit` matches.
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

/// Recursively visit directory entries without retaining sort or rule-clone buffers.
fn visit_directory(
    directory: &Path,
    rules: &mut IgnoreRules,
    request: &SearchRequest,
    result: &mut SearchResult,
) -> Result<bool, SearchError> {
    if request.cancel.is_cancelled() {
        return Err(SearchError::Cancelled);
    }
    let inherited_rule_count = rules.rules.len();
    rules.load_directory(directory, &request.cancel, &mut result.warnings)?;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(_) => {
            rules.rules.truncate(inherited_rule_count);
            return Ok(true);
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
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
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
    Ok(true)
}

/// Scan one candidate file using a reusable line buffer and bounded sample.
fn scan_file(
    path: &Path,
    request: &SearchRequest,
    result: &mut SearchResult,
) -> Result<bool, SearchError> {
    if request.cancel.is_cancelled() {
        return Err(SearchError::Cancelled);
    }
    let mut sample_file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return Ok(true),
    };
    let mut sample = [0_u8; 4096];
    let sample_len = match sample_file.read(&mut sample) {
        Ok(length) => length,
        Err(_) => return Ok(true),
    };
    if request.cancel.is_cancelled() {
        return Err(SearchError::Cancelled);
    }
    if is_binary(path, &sample[..sample_len]) {
        return Ok(true);
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return Ok(true),
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
            Err(SearchError::Io) => return Ok(true),
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
    Ok(true)
}

/// One logical line returned by the bounded streaming reader.
enum StreamLine {
    /// No bytes remain in the input stream.
    Eof,
    /// A line whose content fits within the retained-byte budget.
    Retained,
    /// A line that was discarded through its delimiter after exceeding the budget.
    Oversized,
}

/// Read one CR/LF-delimited logical line without retaining over-budget bytes.
///
/// # Parameters
/// - `reader`: Buffered source to consume.
/// - `retained`: Reusable storage for the current line's content.
/// - `max_bytes`: Maximum content bytes retained for one line.
/// - `cancel`: Cooperative cancellation token checked for every buffer.
///
/// # Returns
/// The next line classification, or a cancellation/I/O worker error.
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
                // Do not reserve or copy any bytes after the retained budget is crossed.
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
/// Return whether a sampled file should be excluded as binary data.
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
    ) {
        return true;
    }
    if bytes.is_empty() || bytes.contains(&0) {
        return !bytes.is_empty() && bytes.contains(&0);
    }
    let non_printable = bytes
        .iter()
        .filter(|byte| **byte < 9 || (**byte > 13 && **byte < 32))
        .count();
    non_printable.saturating_mul(10) > bytes.len().saturating_mul(3)
}

/// Return whether a candidate passes an optional ripgrep-style glob filter.
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
    let candidate = if pattern.contains('/') {
        relative.as_str()
    } else {
        relative.rsplit('/').next().unwrap_or(relative.as_str())
    };
    let matched = wildcard_match(pattern, candidate);
    if excluded.is_some() {
        !matched
    } else {
        matched
    }
}

/// Convert a loaded normalized file into merged context ranges.
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

/// Build bounded display rows while retaining match identity separately.
///
/// The source is scanned once and no all-lines pointer index is retained.
fn display_rows(
    text: &str,
    ranges: &[(usize, usize)],
    matches: &[usize],
    cancel: &CancellationToken,
) -> Result<(Vec<Value>, bool), ToolError> {
    let mut rows = Vec::new();
    let mut range_index = 0usize;
    for (index, text) in text.split('\n').enumerate() {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let line_number = index.saturating_add(1);
        while range_index < ranges.len() && line_number > ranges[range_index].1 {
            range_index += 1;
        }
        if range_index >= ranges.len() {
            break;
        }
        let (start, end) = ranges[range_index];
        if line_number < start || line_number > end {
            continue;
        }
        if rows.len() >= MAX_DISPLAY_ROWS {
            return Ok((rows, true));
        }
        rows.push(json!({
            "line": line_number,
            "text": text,
            "isMatch": matches.binary_search(&line_number).is_ok(),
        }));
    }
    Ok((rows, false))
}

/// Produce a slash-normalized path relative to the session workdir.
fn display_path_for(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .map_or_else(|_| display_path(path), display_path)
}

/// Bound warning count and bytes without changing warning order.
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

/// Append one warning while enforcing the shared count and byte budgets.
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

/// Truncate text at a valid UTF-8 boundary.
fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// One parsed ignore rule with the directory that owns its pattern.
#[derive(Clone)]
struct IgnoreRule {
    base: PathBuf,
    pattern: String,
    directory: bool,
    negated: bool,
    anchored: bool,
}

/// Ordered gitignore and .ignore rules inherited by one traversal branch.
#[derive(Clone, Default)]
struct IgnoreRules {
    rules: Vec<IgnoreRule>,
}

impl IgnoreRules {
    /// Load ignore files from an ancestor through the parent of a search root.
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

    /// Load the two native ignore filenames from one directory with bounded state.
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

    #[cfg(test)]
    /// Evaluate ordered ignore rules using last-match-wins negation.
    fn matches(&self, path: &Path, is_directory: bool) -> bool {
        let mut ignored = false;
        for rule in &self.rules {
            if rule.matches(path, is_directory) {
                ignored = !rule.negated;
            }
        }
        ignored
    }

    /// Evaluate inherited rules while checking cancellation between rules.
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
    /// Match one candidate path against this rule's relative scope.
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
            if self.anchored || pattern.contains('/') {
                let end = if is_directory {
                    components.len()
                } else {
                    components.len().saturating_sub(1)
                };
                return (1..=end)
                    .any(|index| wildcard_match(pattern, &components[..index].join("/")));
            }
            let end = if is_directory {
                components.len()
            } else {
                components.len().saturating_sub(1)
            };
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

/// Parse one gitignore rule while retaining significant escaped spaces.
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

/// Remove only unescaped trailing spaces and tabs from an ignore rule.
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

/// Match a slash-aware glob using the same `*`, `?`, class, and `**` rules for
/// user filename filters and ignore patterns.
pub(crate) fn wildcard_match(pattern: &str, value: &str) -> bool {
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

/// Return one escaped literal byte from a glob pattern.
fn escaped_literal(pattern: &[u8], start: usize) -> Option<u8> {
    (pattern.get(start) == Some(&b'\\')).then(|| pattern.get(start + 1).copied())?
}

/// Parse one simple bracket class, including `!` and `^` negation.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globstar_matches_root_and_nested_files() {
        assert!(wildcard_match("**/*.rs", "main.rs"));
        assert!(wildcard_match("**/*.rs", "src/deep/main.rs"));
        assert!(!wildcard_match("**/*.rs", "src/main.txt"));
    }

    #[test]
    fn wildcard_tokens_keep_slash_escape_range_and_empty_semantics() {
        for (pattern, value, expected) in [
            ("", "", true),
            ("", "value", false),
            ("*", "", true),
            ("*", "a/b", false),
            ("a?c", "abc", true),
            ("a?c", "a/c", false),
            ("file-[0-9].txt", "file-7.txt", true),
            ("file-[!0-9].txt", "file-a.txt", true),
            ("file-[^0-9].txt", "file-7.txt", false),
            (r"literal\*name", "literal*name", true),
            ("**", "any/depth", true),
            ("root/**/file", "root/file", true),
            ("root/**/file", "root/a/b/file", true),
            ("a[b", "a[b", true),
        ] {
            assert_eq!(
                wildcard_match(pattern, value),
                expected,
                "pattern {pattern:?} against {value:?}"
            );
        }
    }

    #[test]
    fn match_retention_is_lexical_and_exact_limit_is_not_truncated() {
        let mut exact = SearchResult {
            matches: BTreeMap::new(),
            total: 0,
            truncated: false,
            warnings: Vec::new(),
        };
        retain_lexical_match(&mut exact, Path::new("b.txt"), 1, 2);
        retain_lexical_match(&mut exact, Path::new("a.txt"), 1, 2);
        assert_eq!(exact.total, 2);
        assert!(!exact.truncated);

        retain_lexical_match(&mut exact, Path::new("z.txt"), 1, 2);
        retain_lexical_match(&mut exact, Path::new("0.txt"), 1, 2);
        assert!(exact.truncated);
        assert_eq!(
            exact.matches.keys().cloned().collect::<Vec<_>>(),
            vec![PathBuf::from("0.txt"), PathBuf::from("a.txt")]
        );
    }

    #[test]
    fn ignore_rule_negation_is_last_match_wins() {
        let root = PathBuf::from("/tmp/root");
        let mut rules = IgnoreRules::default();
        rules.rules.push(IgnoreRule {
            base: root.clone(),
            pattern: "*.log".to_string(),
            directory: false,
            negated: false,
            anchored: false,
        });
        rules.rules.push(IgnoreRule {
            base: root.clone(),
            pattern: "important.log".to_string(),
            directory: false,
            negated: true,
            anchored: false,
        });
        assert!(rules.matches(&root.join("debug.log"), false));
        assert!(!rules.matches(&root.join("important.log"), false));
    }
}
