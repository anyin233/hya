//! Public Edit contracts for the native hashline tool.
//!
//! These tests exercise only `ToolRegistry` and `ToolCtx`. The request and
//! result vectors follow `pi-hashline-edit` 0.8.3 (git head
//! `ba7db9943d0f58499b24c1f6bd64722580f772a5`): model input is the closed
//! `{path, edits}` surface, and line references are contextual `LINE#HASH`
//! anchors. The old fuzzy `filePath`/`oldString`/`newString` surface is not a
//! supported compatibility contract.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_tool::{
    Action, FormatterError, FormatterPlane, FormatterProvider, InteractionPlane, LspError,
    LspPlane, LspProvider, LspRequest, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, Tool, ToolCtx, ToolError, ToolRegistry, WebSearchPlane,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const ORIGINAL_WITH_NEWLINE: &str = "alpha\nbeta\ngamma\ndelta\n";

/// Build an allow rule for one tool or resource action.
///
/// # Parameters
/// - `action`: Permission category to allow.
/// - `pattern`: Resource glob accepted by that category.
///
/// # Returns
/// A permissive [`Rule`] for the requested action and pattern.
fn allow(action: Action, pattern: &str) -> Rule {
    Rule::new(action, pattern, Mode::Allow)
}

/// Build a deny rule for one tool or resource action.
///
/// # Parameters
/// - `action`: Permission category to deny.
/// - `pattern`: Resource glob rejected by that category.
///
/// # Returns
/// A denying [`Rule`] for the requested action and pattern.
fn deny(action: Action, pattern: &str) -> Rule {
    Rule::new(action, pattern, Mode::Deny)
}

/// Create an isolated temporary workdir for one public tool scenario.
///
/// # Returns
/// A newly-created deterministic-prefix directory under the process temporary
/// directory. The process id and monotonic counter prevent concurrent tests
/// from sharing a target.
fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-edit-contract-{nanos}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Construct a normal Edit context with disconnected optional planes.
///
/// # Parameters
/// - `rules`: Invocation and resource permission rules for the test.
/// - `workdir`: Session working directory used for relative paths.
///
/// # Returns
/// A [`ToolCtx`] backed by fresh interaction, spawn, formatter, LSP, and
/// cancellation planes.
fn ctx_with(rules: Vec<Rule>, workdir: PathBuf) -> ToolCtx {
    ctx_with_components(
        rules,
        workdir,
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
    )
}

/// Construct an Edit context with a custom formatter provider.
///
/// # Parameters
/// - `rules`: Invocation and resource permission rules for the test.
/// - `workdir`: Session working directory used for relative paths.
/// - `formatter`: Formatter plane to run after mutation.
///
/// # Returns
/// A [`ToolCtx`] with the supplied formatter and disconnected LSP/cancellation
/// defaults.
fn ctx_with_formatter(rules: Vec<Rule>, workdir: PathBuf, formatter: FormatterPlane) -> ToolCtx {
    ctx_with_components(
        rules,
        workdir,
        formatter,
        LspPlane::default(),
        CancellationToken::new(),
    )
}

/// Construct an Edit context with custom integration and cancellation planes.
///
/// # Parameters
/// - `rules`: Invocation and resource permission rules for the test.
/// - `workdir`: Session working directory used for relative paths.
/// - `formatter`: Formatter plane to run after mutation.
/// - `lsp`: LSP plane to touch and diagnose after mutation.
/// - `cancel`: Call-scoped cancellation token.
///
/// # Returns
/// A fully initialized public [`ToolCtx`] for a registry tool call.
fn ctx_with_components(
    rules: Vec<Rule>,
    workdir: PathBuf,
    formatter: FormatterPlane,
    lsp: LspPlane,
    cancel: CancellationToken,
) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp,
        formatter,
        agents: Default::default(),
        workdir,
        cancel,
    }
}

/// Read one contextual anchor through the public Read registry seam.
///
/// # Parameters
/// - `tool`: Public Read tool resolved from the same registry as Edit.
/// - `ctx`: Context with Read permission and the target workdir.
/// - `path`: Target path accepted by Read.
/// - `line`: One-based line whose complete rendered anchor is required.
///
/// # Returns
/// The complete `LINE#HASH:content` row, including its optional text hint.
async fn read_anchor(tool: &dyn Tool, ctx: &ToolCtx, path: &str, line: usize) -> String {
    let output = tool
        .execute(ctx, json!({"path": path}))
        .await
        .expect("public Read must return a hashline result");
    let rendered = output["output"]
        .as_str()
        .expect("Read output must be a string");
    let prefix = format!("{line}#");
    rendered
        .lines()
        .find(|row| row.trim_start().starts_with(&prefix))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("Read output has no anchor for line {line}: {rendered:?}"))
}

/// Assert that a failed public call is a typed input error with one stable code.
///
/// # Parameters
/// - `result`: Completed Edit result expected to fail during input/hashline validation.
/// - `code`: Stable bracketed `E_*` prefix required in the model-visible message.
///
/// # Returns
/// Nothing. The helper panics when the error type or stable code is wrong.
fn assert_input_code(result: Result<Value, ToolError>, code: &str) {
    let error = result.expect_err("Edit input should be rejected");
    match error {
        ToolError::Input(message) => {
            let prefix = format!("[{code}]");
            assert!(
                message.contains(&prefix),
                "typed input error must preserve {prefix}: {message:?}"
            );
        }
        other => panic!("expected typed input error [{code}], got {other:?}"),
    }
}

/// Assert that bounded warning metadata contains one descriptive fragment.
///
/// # Parameters
/// - `result`: Successful Edit result carrying warning metadata.
/// - `fragment`: Case-sensitive text expected in at least one warning.
///
/// # Returns
/// Nothing. The helper panics when warnings are absent or do not match.
fn assert_warning(result: &Value, fragment: &str) {
    let warnings = result["metadata"]["warnings"]
        .as_array()
        .expect("Edit metadata must expose warnings as an array");
    assert!(
        warnings
            .iter()
            .filter_map(Value::as_str)
            .any(|warning| warning.contains(fragment)),
        "warnings {warnings:?} do not contain {fragment:?}"
    );
}

/// Formatter provider that deliberately mutates the final file bytes.
struct RewritingFormatter;

#[async_trait]
impl FormatterProvider for RewritingFormatter {
    /// Report no formatter catalog rows for this mutation-order fixture.
    async fn status(
        &self,
        _workdir: &Path,
    ) -> Result<Vec<hya_tool::FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    /// Rewrite the target so the Edit result must describe post-formatter bytes.
    async fn format_file(&self, _workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        tokio::fs::write(file, "formatted\n").await.unwrap();
        Ok(true)
    }
}

/// Formatter provider that writes exact bytes for finalization regressions.
struct StaticBytesFormatter {
    output: &'static [u8],
}

#[async_trait]
impl FormatterProvider for StaticBytesFormatter {
    /// Report no formatter catalog rows for this byte-level fixture.
    async fn status(
        &self,
        _workdir: &Path,
    ) -> Result<Vec<hya_tool::FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    /// Replace the target with the deliberately selected formatter bytes.
    async fn format_file(&self, _workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        tokio::fs::write(file, self.output).await.unwrap();
        Ok(true)
    }
}

/// Formatter that cancels the call and reports an error after Edit committed.
struct CancellingFailingFormatter {
    cancel: CancellationToken,
}

#[async_trait]
impl FormatterProvider for CancellingFailingFormatter {
    /// Report no formatter catalog rows for this cancellation fixture.
    async fn status(
        &self,
        _workdir: &Path,
    ) -> Result<Vec<hya_tool::FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    /// Cancel and fail after the adapter has crossed its first commit boundary.
    async fn format_file(&self, _workdir: &Path, _file: &Path) -> Result<bool, FormatterError> {
        self.cancel.cancel();
        Err(FormatterError("cancelled formatter failure".to_string()))
    }
}

/// Formatter that returns an oversized UTF-8 error after Edit committed.
struct OversizedFailingFormatter;

#[async_trait]
impl FormatterProvider for OversizedFailingFormatter {
    /// Report no formatter catalog rows for this error-bound fixture.
    async fn status(
        &self,
        _workdir: &Path,
    ) -> Result<Vec<hya_tool::FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    /// Return a multibyte error whose tail must not enter durable tool state.
    async fn format_file(&self, _workdir: &Path, _file: &Path) -> Result<bool, FormatterError> {
        Err(FormatterError(format!(
            "{}SECRET_FORMATTER_TAIL",
            "é".repeat(5000)
        )))
    }
}

/// One observed LSP document touch used to prove post-format ordering.
#[derive(Debug)]
struct Touch {
    file: PathBuf,
    kind: String,
    content: String,
}

/// LSP provider that records final bytes and returns bounded diagnostics.
#[derive(Clone)]
struct RecordingLsp {
    touches: Arc<Mutex<Vec<Touch>>>,
    diagnostics: Value,
}

#[async_trait]
impl LspProvider for RecordingLsp {
    /// Report that this fixture has an LSP client for every target.
    async fn has_clients(&self, _file: &Path) -> Result<bool, LspError> {
        Ok(true)
    }

    /// Return no request results because only document touches are observed.
    async fn execute(&self, _request: LspRequest) -> Result<Vec<Value>, LspError> {
        Ok(Vec::new())
    }

    /// Record the final target bytes observed by the post-edit LSP hook.
    async fn touch_file(&self, file: &Path, kind: &str) -> Result<(), LspError> {
        let content = tokio::fs::read_to_string(file)
            .await
            .map_err(|error| LspError(error.to_string()))?;
        self.touches.lock().await.push(Touch {
            file: file.to_path_buf(),
            kind: kind.to_string(),
            content,
        });
        Ok(())
    }

    /// Return the fixture diagnostics that Edit must preserve in metadata.
    async fn diagnostics(&self) -> Result<Value, LspError> {
        Ok(self.diagnostics.clone())
    }
}

/// Build the diagnostics map returned by the recording LSP provider.
///
/// # Parameters
/// - `path`: Absolute target path used as the diagnostics map key.
/// - `message`: Diagnostic message that must remain visible in Edit output.
///
/// # Returns
/// A JSON diagnostics object in the public LSP result shape.
fn error_diagnostics(path: &Path, message: &str) -> Value {
    json!({
        path.to_string_lossy().to_string(): [{
            "severity": 1,
            "range": {
                "start": { "line": 2, "character": 4 },
                "end": { "line": 2, "character": 7 }
            },
            "message": message
        }]
    })
}

#[test]
fn edit_schema_exposes_only_closed_hashline_operations() {
    // Given
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    let schema = tool.schema().input_schema;
    let properties = schema["properties"].as_object().unwrap();

    // Then
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"], json!(["path", "edits"]));
    assert_eq!(properties["path"]["type"], "string");
    assert_eq!(properties["edits"]["type"], "array");
    assert_eq!(properties.len(), 2);
    assert!(properties.contains_key("path"));
    assert!(properties.contains_key("edits"));
    for obsolete in [
        "filePath",
        "file_path",
        "oldString",
        "newString",
        "old",
        "new",
        "replaceAll",
        "replace_all",
    ] {
        assert!(
            !properties.contains_key(obsolete),
            "schema exposes obsolete {obsolete}"
        );
    }

    let variants = properties["edits"]["items"]["anyOf"].as_array().unwrap();
    assert_eq!(variants.len(), 4);
    let mut operations = std::collections::BTreeSet::new();
    for variant in variants {
        assert_eq!(variant["type"], "object");
        assert_eq!(variant["additionalProperties"], false);
        let variant_properties = variant["properties"].as_object().unwrap();
        let op = variant_properties["op"]["enum"][0].as_str().unwrap();
        assert_eq!(variant_properties["op"]["type"], "string");
        assert_eq!(variant_properties["op"]["enum"], json!([op]));
        assert!(operations.insert(op));
        match op {
            "replace" => {
                assert_eq!(variant_properties.len(), 4);
                assert!(variant_properties.contains_key("pos"));
                assert!(variant_properties.contains_key("end"));
                assert!(variant_properties.contains_key("lines"));
                assert_eq!(variant_properties["pos"]["type"], "string");
                assert_eq!(variant_properties["end"]["type"], "string");
                assert_eq!(variant_properties["lines"]["type"], "array");
                assert_eq!(variant["required"], json!(["op", "pos", "lines"]));
            }
            "append" | "prepend" => {
                assert_eq!(variant_properties.len(), 3);
                assert!(variant_properties.contains_key("pos"));
                assert!(variant_properties.contains_key("lines"));
                assert_eq!(variant_properties["pos"]["type"], "string");
                assert_eq!(variant_properties["lines"]["type"], "array");
                assert_eq!(variant["required"], json!(["op", "lines"]));
            }
            "replace_text" => {
                assert_eq!(variant_properties.len(), 3);
                assert!(variant_properties.contains_key("oldText"));
                assert!(variant_properties.contains_key("newText"));
                assert_eq!(variant_properties["oldText"]["type"], "string");
                assert_eq!(variant_properties["newText"]["type"], "string");
                assert_eq!(variant["required"], json!(["op", "oldText", "newText"]));
            }
            other => panic!("unexpected Edit operation schema: {other}"),
        }
    }
}

#[tokio::test]
async fn edit_applies_table_driven_hashline_operations() {
    // Given
    let cases = vec![
        (
            "single replace",
            ORIGINAL_WITH_NEWLINE,
            json!({
                "path": "notes.txt",
                "edits": [{"op": "replace", "pos": "2#JB", "lines": ["BETA"]}]
            }),
            "alpha\nBETA\ngamma\ndelta\n",
        ),
        (
            "range replace",
            ORIGINAL_WITH_NEWLINE,
            json!({
                "path": "notes.txt",
                "edits": [{
                    "op": "replace",
                    "pos": "2#JB",
                    "end": "3#KJ",
                    "lines": ["BETA", "GAMMA"]
                }]
            }),
            "alpha\nBETA\nGAMMA\ndelta\n",
        ),
        (
            "range replacement with deletion",
            ORIGINAL_WITH_NEWLINE,
            json!({
                "path": "notes.txt",
                "edits": [{
                    "op": "replace",
                    "pos": "2#JB",
                    "end": "3#KJ",
                    "lines": ["BETA"]
                }]
            }),
            "alpha\nBETA\ndelta\n",
        ),
        (
            "anchored append",
            ORIGINAL_WITH_NEWLINE,
            json!({
                "path": "notes.txt",
                "edits": [{"op": "append", "pos": "2#JB", "lines": ["inserted"]}]
            }),
            "alpha\nbeta\ninserted\ngamma\ndelta\n",
        ),
        (
            "implicit EOF append",
            ORIGINAL_WITH_NEWLINE,
            json!({
                "path": "notes.txt",
                "edits": [{"op": "append", "lines": ["omega"]}]
            }),
            "alpha\nbeta\ngamma\ndelta\nomega\n",
        ),
        (
            "anchored prepend",
            ORIGINAL_WITH_NEWLINE,
            json!({
                "path": "notes.txt",
                "edits": [{"op": "prepend", "pos": "3#KJ", "lines": ["inserted"]}]
            }),
            "alpha\nbeta\ninserted\ngamma\ndelta\n",
        ),
        (
            "implicit BOF prepend",
            ORIGINAL_WITH_NEWLINE,
            json!({
                "path": "notes.txt",
                "edits": [{"op": "prepend", "lines": ["zero"]}]
            }),
            "zero\nalpha\nbeta\ngamma\ndelta\n",
        ),
        (
            "append to empty origin",
            "",
            json!({
                "path": "notes.txt",
                "edits": [{"op": "append", "lines": ["first"]}]
            }),
            "first",
        ),
        (
            "prepend to empty origin",
            "",
            json!({
                "path": "notes.txt",
                "edits": [{"op": "prepend", "lines": ["first"]}]
            }),
            "first",
        ),
    ];

    // When / Then
    for (name, initial, input, expected) in cases {
        let workdir = tempdir();
        let target = workdir.join("notes.txt");
        tokio::fs::write(&target, initial).await.unwrap();
        let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
        let tool = ToolRegistry::builtins().get("edit").unwrap();
        let output = tool
            .execute(&ctx, input)
            .await
            .unwrap_or_else(|error| panic!("{name} failed: {error}"));
        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            expected,
            "operation case {name}"
        );
        assert!(output["output"].is_string(), "case {name} has no output");
    }
}

#[tokio::test]
async fn edit_supports_replace_text_and_compatibility_dialects() {
    // Given
    let edits = json!([{"op": "replace_text", "oldText": "beta", "newText": "BETA"}]);
    let serialized_edits = serde_json::to_string(&edits).unwrap();
    let cases = vec![
        (
            "canonical edits array",
            json!({"path": "notes.txt", "edits": edits.clone()}),
        ),
        (
            "JSON-string edits compatibility",
            json!({"path": "notes.txt", "edits": serialized_edits}),
        ),
        (
            "top-level file_path compatibility",
            json!({
                "file_path": "notes.txt",
                "edits": [{"op": "replace_text", "oldText": "beta", "newText": "BETA"}]
            }),
        ),
        (
            "top-level camel oldText/newText pair",
            json!({"path": "notes.txt", "oldText": "beta", "newText": "BETA"}),
        ),
        (
            "top-level snake old_text/new_text pair",
            json!({"path": "notes.txt", "old_text": "beta", "new_text": "BETA"}),
        ),
    ];

    // When / Then
    for (name, input) in cases {
        let workdir = tempdir();
        let target = workdir.join("notes.txt");
        tokio::fs::write(&target, "alpha\nbeta\ngamma\n")
            .await
            .unwrap();
        let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
        let tool = ToolRegistry::builtins().get("edit").unwrap();
        tool.execute(&ctx, input)
            .await
            .unwrap_or_else(|error| panic!("{name} failed: {error}"));
        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            "alpha\nBETA\ngamma\n",
            "compatibility case {name}"
        );
    }
}

#[tokio::test]
async fn edit_rejects_mixed_incomplete_wrong_typed_and_unknown_fields() {
    // Given
    let cases = vec![
        (
            "obsolete fuzzy filePath/oldString/newString",
            json!({"filePath": "notes.txt", "oldString": "beta", "newString": "BETA"}),
            "E_BAD_REQUEST",
        ),
        (
            "canonical and compatibility paths mixed",
            json!({
                "path": "notes.txt",
                "file_path": "notes.txt",
                "edits": []
            }),
            "E_BAD_REQUEST",
        ),
        (
            "mixed camel and snake text pairs",
            json!({"path": "notes.txt", "oldText": "beta", "new_text": "BETA"}),
            "E_BAD_REQUEST",
        ),
        (
            "incomplete top-level text pair",
            json!({"path": "notes.txt", "oldText": "beta"}),
            "E_BAD_OP",
        ),
        (
            "top-level text pair mixed with edits",
            json!({
                "path": "notes.txt",
                "edits": [{"op": "replace_text", "oldText": "beta", "newText": "BETA"}],
                "oldText": "beta",
                "newText": "BETA"
            }),
            "E_BAD_REQUEST",
        ),
        (
            "wrong path type",
            json!({"path": 7, "edits": []}),
            "E_BAD_REQUEST",
        ),
        (
            "empty path",
            json!({"path": "", "edits": []}),
            "E_BAD_REQUEST",
        ),
        (
            "wrong edits type",
            json!({"path": "notes.txt", "edits": true}),
            "E_BAD_REQUEST",
        ),
        (
            "wrong operation type",
            json!({
                "path": "notes.txt",
                "edits": [{"op": 1, "pos": "2#JB", "lines": ["BETA"]}]
            }),
            "E_BAD_REQUEST",
        ),
        (
            "missing operation",
            json!({
                "path": "notes.txt",
                "edits": [{"pos": "2#JB", "lines": ["BETA"]}]
            }),
            "E_BAD_OP",
        ),
        (
            "wrong lines type",
            json!({
                "path": "notes.txt",
                "edits": [{"op": "replace", "pos": "2#JB", "lines": "BETA"}]
            }),
            "E_BAD_REQUEST",
        ),
        (
            "unknown top-level field",
            json!({
                "path": "notes.txt",
                "edits": [],
                "replaceAll": true
            }),
            "E_BAD_REQUEST",
        ),
        (
            "unknown nested field",
            json!({
                "path": "notes.txt",
                "edits": [{
                    "op": "replace",
                    "pos": "2#JB",
                    "lines": ["BETA"],
                    "extra": true
                }]
            }),
            "E_BAD_REQUEST",
        ),
        (
            "wrong text pair type",
            json!({"path": "notes.txt", "oldText": 9, "newText": "BETA"}),
            "E_BAD_REQUEST",
        ),
    ];

    // When / Then
    for (name, input, code) in cases {
        let workdir = tempdir();
        let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
        let tool = ToolRegistry::builtins().get("edit").unwrap();
        let result = tool.execute(&ctx, input).await;
        assert_input_code(result, code);
        assert!(!name.is_empty());
    }
}

#[tokio::test]
async fn edit_rejects_hashline_and_diff_display_prefix_payloads() {
    // Given
    let cases = vec![
        ("copied hashline row", "2#JB:beta"),
        ("copied unified diff row", "- 2    beta"),
        ("copied hashline row with plus marker", "+ 2#JB:beta"),
    ];

    // When / Then
    for (name, payload) in cases {
        let workdir = tempdir();
        tokio::fs::write(workdir.join("notes.txt"), ORIGINAL_WITH_NEWLINE)
            .await
            .unwrap();
        let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
        let tool = ToolRegistry::builtins().get("edit").unwrap();
        let result = tool
            .execute(
                &ctx,
                json!({
                    "path": "notes.txt",
                    "edits": [{"op": "replace", "pos": "2#JB", "lines": [payload]}]
                }),
            )
            .await;
        assert_input_code(result, "E_INVALID_PATCH");
        assert!(!name.is_empty());
    }
}

#[tokio::test]
async fn edit_reports_collision_conflict_range_order_and_would_empty_codes() {
    // Given
    let cases = vec![
        (
            "text-hint collision veto",
            json!({
                "path": "notes.txt",
                "edits": [{"op": "replace", "pos": "2#JB:gamma", "lines": ["BETA"]}]
            }),
            "E_STALE_ANCHOR",
        ),
        (
            "bad anchor separator",
            json!({
                "path": "notes.txt",
                "edits": [{"op": "replace", "pos": "2:beta", "lines": ["BETA"]}]
            }),
            "E_BAD_REF",
        ),
        (
            "range out of bounds",
            json!({
                "path": "notes.txt",
                "edits": [{"op": "replace", "pos": "5#JB", "lines": ["BETA"]}]
            }),
            "E_RANGE_OOB",
        ),
        (
            "reverse range order",
            json!({
                "path": "notes.txt",
                "edits": [{
                    "op": "replace",
                    "pos": "3#KJ",
                    "end": "2#JB",
                    "lines": ["BETA"]
                }]
            }),
            "E_BAD_OP",
        ),
        (
            "overlapping replacement conflict",
            json!({
                "path": "notes.txt",
                "edits": [
                    {"op": "replace", "pos": "1#KT", "end": "2#JB", "lines": ["A"]},
                    {"op": "replace", "pos": "2#JB", "end": "3#KJ", "lines": ["B"]}
                ]
            }),
            "E_EDIT_CONFLICT",
        ),
        (
            "same insertion boundary conflict",
            json!({
                "path": "notes.txt",
                "edits": [
                    {"op": "append", "pos": "2#JB", "lines": ["A"]},
                    {"op": "append", "pos": "2#JB", "lines": ["B"]}
                ]
            }),
            "E_EDIT_CONFLICT",
        ),
        (
            "would empty non-empty file",
            json!({
                "path": "notes.txt",
                "edits": [{
                    "op": "replace",
                    "pos": "1#KT",
                    "end": "4#PX",
                    "lines": []
                }]
            }),
            "E_WOULD_EMPTY",
        ),
    ];

    // When / Then
    for (name, input, code) in cases {
        let workdir = tempdir();
        tokio::fs::write(workdir.join("notes.txt"), ORIGINAL_WITH_NEWLINE)
            .await
            .unwrap();
        let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
        let tool = ToolRegistry::builtins().get("edit").unwrap();
        let result = tool.execute(&ctx, input).await;
        assert_input_code(result, code);
        assert!(!name.is_empty());
    }
}

#[tokio::test]
async fn edit_accepts_a_fuzzy_hashline_hint_but_not_fuzzy_string_replacement() {
    // Given: the original displayed line uses typographic quotes. Read stores
    // its exact hashline anchor, then the file changes to an equivalent ASCII
    // spelling before Edit receives the stale anchor and copied hint.
    let workdir = tempdir();
    let target = workdir.join("quotes.txt");
    tokio::fs::write(&target, "before\nprintln!(“old”)\nafter\n")
        .await
        .unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir,
    );
    let stale_anchor = read_anchor(read.as_ref(), &ctx, "quotes.txt", 2).await;
    tokio::fs::write(&target, "before\nprintln!(\"old\")\nafter\n")
        .await
        .unwrap();

    // When
    let output = edit
        .execute(
            &ctx,
            json!({
                "path": "quotes.txt",
                "edits": [{"op": "replace", "pos": stale_anchor, "lines": ["replacement"]}]
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "before\nreplacement\nafter\n"
    );
    assert_warning(&output, "fuzzy");
}

#[tokio::test]
async fn edit_returns_bounded_warning_metadata_for_suspicious_insertions() {
    // Given
    let cases = vec![
        (
            "adjacent duplicate insertion",
            json!({
                "path": "notes.txt",
                "edits": [{"op": "append", "pos": "2#JB", "lines": ["gamma"]}]
            }),
            "duplicate",
        ),
        (
            "bare hash prefix",
            json!({
                "path": "notes.txt",
                "edits": [{"op": "append", "lines": ["JB: copied content"]}]
            }),
            "hash and ':'",
        ),
        (
            "literal Unicode placeholder",
            json!({
                "path": "notes.txt",
                "edits": [{"op": "append", "lines": [r"\uDDDD"]}]
            }),
            r"\uDDDD",
        ),
    ];

    // When / Then
    for (name, input, warning) in cases {
        let workdir = tempdir();
        tokio::fs::write(workdir.join("notes.txt"), ORIGINAL_WITH_NEWLINE)
            .await
            .unwrap();
        let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
        let tool = ToolRegistry::builtins().get("edit").unwrap();
        let output = tool
            .execute(&ctx, input)
            .await
            .unwrap_or_else(|error| panic!("{name} failed: {error}"));
        assert_warning(&output, warning);
    }
}

#[tokio::test]
async fn edit_honors_cancellation_before_mutating_the_target() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("notes.txt");
    tokio::fs::write(&target, ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx = ctx_with_components(
        vec![allow(Action::Edit, "*")],
        workdir,
        FormatterPlane::default(),
        LspPlane::default(),
        cancel,
    );
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    let result = tool
        .execute(
            &ctx,
            json!({
                "path": "notes.txt",
                "edits": [{"op": "replace", "pos": "2#JB", "lines": ["BETA"]}]
            }),
        )
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Cancelled)));
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        ORIGINAL_WITH_NEWLINE
    );
}

#[tokio::test]
async fn edit_returns_diff_and_filediff_metadata_for_strict_operations() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("notes.txt");
    tokio::fs::write(&target, ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    let output = tool
        .execute(
            &ctx,
            json!({
                "path": "notes.txt",
                "edits": [{"op": "replace", "pos": "2#JB", "lines": ["THREE"]}]
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(output["title"], "notes.txt");
    let rendered = output["output"].as_str().unwrap();
    assert!(
        rendered.contains("#"),
        "Edit output must retain fresh anchors"
    );
    assert!(
        rendered.contains(":THREE"),
        "Edit output must show final content"
    );
    assert_eq!(output["metadata"]["diagnostics"], json!({}));
    assert_eq!(
        output["metadata"]["filediff"]["file"],
        target.to_string_lossy().as_ref()
    );
    assert_eq!(output["metadata"]["filediff"]["additions"], 1);
    assert_eq!(output["metadata"]["filediff"]["deletions"], 1);
    let diff = output["metadata"]["diff"].as_str().unwrap();
    assert!(diff.contains("-beta"));
    assert!(diff.contains("+THREE"));
}

/// Large changed regions require a re-read instead of copying the full file into metadata.
#[tokio::test]
async fn edit_omits_unbounded_display_text_when_fresh_anchor_preview_does_not_fit() {
    let workdir = tempdir();
    let target = workdir.join("large-change.txt");
    let original = (1..=20)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(&target, format!("{original}\n"))
        .await
        .unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir,
    );
    let start = read_anchor(read.as_ref(), &ctx, "large-change.txt", 1).await;
    let end = read_anchor(read.as_ref(), &ctx, "large-change.txt", 20).await;
    let replacement = (1..=20)
        .map(|line| format!("replacement-{line}-{}", "x".repeat(4096)))
        .collect::<Vec<_>>();

    let result = edit
        .execute(
            &ctx,
            json!({
                "path": "large-change.txt",
                "edits": [{"op": "replace", "pos": start, "end": end, "lines": replacement}]
            }),
        )
        .await
        .unwrap();

    assert_eq!(result["metadata"]["display"]["text"], "");
    assert_eq!(result["metadata"]["display"]["truncated"], true);
    assert_eq!(result["metadata"]["display"]["lineEnd"], 0);
    assert_eq!(result["metadata"]["display"]["totalLines"], 20);
    assert!(
        result["output"]
            .as_str()
            .is_some_and(|output| output.contains("Re-read"))
    );
}

#[tokio::test]
async fn edit_runs_formatter_before_emitting_fresh_final_anchors() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("notes.txt");
    tokio::fs::write(&target, ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    let formatter = FormatterPlane::new(Arc::new(RewritingFormatter));
    let ctx = ctx_with_formatter(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir,
        formatter,
    );
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let old_anchor = read_anchor(read.as_ref(), &ctx, "notes.txt", 2).await;

    // When
    let output = edit
        .execute(
            &ctx,
            json!({
                "path": "notes.txt",
                "edits": [{"op": "replace", "pos": old_anchor, "lines": ["BETA"]}]
            }),
        )
        .await
        .unwrap();

    // Then: formatter bytes, not pre-format edit bytes, drive anchors.
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "formatted\n"
    );
    let rendered = output["output"].as_str().unwrap();
    assert!(
        rendered
            .lines()
            .any(|line| line.contains('#') && line.ends_with(":formatted")),
        "final output must contain a fresh anchor for formatted bytes: {rendered:?}"
    );
    assert!(!rendered.contains(":BETA"));
}

#[tokio::test]
async fn edit_restores_original_ending_for_mixed_formatter_output() {
    // Given: a BOM-bearing CRLF file and formatter bytes whose first ending is
    // CRLF but whose later ending is LF.
    let workdir = tempdir();
    let target = workdir.join("mixed.txt");
    tokio::fs::write(&target, b"\xEF\xBB\xBFalpha\r\nbeta\r\ngamma\r\n")
        .await
        .unwrap();
    let formatter = FormatterPlane::new(Arc::new(StaticBytesFormatter {
        output: b"\xEF\xBB\xBFformatted\r\nsecond\n",
    }));
    let ctx = ctx_with_formatter(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir,
        formatter,
    );
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let anchor = read_anchor(read.as_ref(), &ctx, "mixed.txt", 2).await;

    // When: Edit applies a strict replacement and the formatter emits mixed endings.
    let output = edit
        .execute(
            &ctx,
            json!({
                "path": "mixed.txt",
                "edits": [{"op": "replace", "pos": anchor, "lines": ["BETA"]}]
            }),
        )
        .await
        .unwrap();

    // Then: final bytes retain the original BOM and normalize every ending to CRLF.
    let bytes = tokio::fs::read(&target).await.unwrap();
    assert_eq!(bytes, b"\xEF\xBB\xBFformatted\r\nsecond\r\n");
    assert!(std::str::from_utf8(&bytes).is_ok());
    assert_eq!(output["metadata"]["display"]["text"], "formatted\nsecond");
}

#[tokio::test]
async fn edit_rewrites_invalid_formatter_bytes_to_valid_utf8() {
    // Given: a BOM-bearing CRLF file and formatter output with an invalid byte.
    let workdir = tempdir();
    let target = workdir.join("invalid.txt");
    tokio::fs::write(&target, b"\xEF\xBB\xBFalpha\r\nbeta\r\n")
        .await
        .unwrap();
    let formatter = FormatterPlane::new(Arc::new(StaticBytesFormatter {
        output: b"\xEF\xBB\xBFformatted\xFF\r\n",
    }));
    let ctx = ctx_with_formatter(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir,
        formatter,
    );
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let anchor = read_anchor(read.as_ref(), &ctx, "invalid.txt", 2).await;

    // When: Edit succeeds and the formatter writes invalid UTF-8.
    let output = edit
        .execute(
            &ctx,
            json!({
                "path": "invalid.txt",
                "edits": [{"op": "replace", "pos": anchor, "lines": ["BETA"]}]
            }),
        )
        .await
        .unwrap();

    // Then: final bytes preserve BOM/CRLF and replace the invalid byte with U+FFFD.
    let bytes = tokio::fs::read(&target).await.unwrap();
    assert_eq!(bytes, b"\xEF\xBB\xBFformatted\xEF\xBF\xBD\r\n");
    assert!(std::str::from_utf8(&bytes).is_ok());
    assert_eq!(output["metadata"]["display"]["text"], "formatted�");
}

#[cfg(unix)]
#[tokio::test]
async fn edit_metadata_describes_final_formatter_bytes_and_hard_link() {
    use std::os::unix::fs::MetadataExt;

    // Given: a hard-linked file whose formatter output changes its byte length.
    let workdir = tempdir();
    let target = workdir.join("length.txt");
    let sibling = workdir.join("length-sibling.txt");
    tokio::fs::write(&target, ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    std::fs::hard_link(&target, &sibling).unwrap();
    assert!(std::fs::metadata(&target).unwrap().nlink() > 1);
    let formatter = FormatterPlane::new(Arc::new(StaticBytesFormatter {
        output: b"formatted\npost-format expansion\n",
    }));
    let ctx = ctx_with_formatter(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir,
        formatter,
    );
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let anchor = read_anchor(read.as_ref(), &ctx, "length.txt", 2).await;

    // When: Edit commits, then the formatter replaces the content with a longer result.
    let output = edit
        .execute(
            &ctx,
            json!({
                "path": "length.txt",
                "edits": [{"op": "replace", "pos": anchor, "lines": ["BETA"]}]
            }),
        )
        .await
        .unwrap();

    // Then: every metadata view describes the final formatter bytes, not the pre-format edit.
    let final_bytes = tokio::fs::read(&target).await.unwrap();
    assert_eq!(final_bytes, b"formatted\npost-format expansion\n");
    assert_eq!(output["metadata"]["bytes"], json!(final_bytes.len()));
    assert_eq!(output["metadata"]["hardLink"], true);
    assert_eq!(output["metadata"]["filediff"]["additions"], 2);
    assert_eq!(output["metadata"]["filediff"]["deletions"], 4);
    assert_eq!(
        output["metadata"]["filediff"]["patch"],
        output["metadata"]["diff"]
    );
    let diff = output["metadata"]["diff"].as_str().unwrap();
    for line in [
        "-alpha",
        "-beta",
        "-gamma",
        "-delta",
        "+formatted",
        "+post-format expansion",
    ] {
        assert!(
            diff.contains(line),
            "final diff must contain {line:?}: {diff:?}"
        );
    }
    let display = &output["metadata"]["display"];
    assert_eq!(display["type"], "file");
    assert_eq!(display["path"], target.to_string_lossy().as_ref());
    assert_eq!(display["text"], "formatted\npost-format expansion");
    assert_eq!(display["lineStart"], 1);
    assert_eq!(display["lineEnd"], 2);
    assert_eq!(display["totalLines"], 2);
    assert_eq!(display["truncated"], false);
    let rendered = output["output"].as_str().unwrap();
    assert!(rendered.contains(":formatted"));
    assert!(rendered.contains(":post-format expansion"));
    assert!(!rendered.contains(":BETA"));
    assert_eq!(tokio::fs::read(&sibling).await.unwrap(), final_bytes);
}

#[tokio::test]
async fn edit_runs_lsp_after_mutation_and_returns_diagnostics() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("notes.txt");
    tokio::fs::write(&target, ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    let diagnostics = error_diagnostics(&target, "bad edit");
    let touches = Arc::new(Mutex::new(Vec::new()));
    let lsp = LspPlane::new(Arc::new(RecordingLsp {
        touches: touches.clone(),
        diagnostics: diagnostics.clone(),
    }));
    let ctx = ctx_with_components(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir,
        FormatterPlane::default(),
        lsp,
        CancellationToken::new(),
    );
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let anchor = read_anchor(read.as_ref(), &ctx, "notes.txt", 2).await;

    // When
    let output = edit
        .execute(
            &ctx,
            json!({
                "path": "notes.txt",
                "edits": [{"op": "replace", "pos": anchor, "lines": ["EDITED"]}]
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(output["metadata"]["diagnostics"], diagnostics);
    let rendered = output["output"].as_str().unwrap();
    assert!(rendered.contains("LSP errors detected in this file"));
    assert!(rendered.contains("bad edit"));
    let calls = touches.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].file, target);
    assert_eq!(calls[0].kind, "document");
    assert_eq!(calls[0].content, "alpha\nEDITED\ngamma\ndelta\n");
}

#[tokio::test]
async fn edit_requires_external_directory_permission_before_mutation() {
    // Given
    let workdir = tempdir();
    let outside_dir = tempdir();
    let outside = outside_dir.join("outside.txt");
    tokio::fs::write(&outside, ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    let ctx = ctx_with(
        vec![
            allow(Action::Edit, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        workdir,
    );
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    let result = tool
        .execute(
            &ctx,
            json!({
                "path": outside.to_string_lossy(),
                "edits": [{"op": "replace", "pos": "2#JB", "lines": ["BETA"]}]
            }),
        )
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Permission(_))));
    assert_eq!(
        tokio::fs::read_to_string(&outside).await.unwrap(),
        ORIGINAL_WITH_NEWLINE
    );
}

#[tokio::test]
async fn edit_preserves_an_existing_utf8_bom_on_hashline_mutation() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("bom.txt");
    tokio::fs::write(&target, b"\xEF\xBB\xBFalpha\nbeta\ngamma\ndelta\n")
        .await
        .unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    tool.execute(
        &ctx,
        json!({
            "path": "bom.txt",
            "edits": [{"op": "replace", "pos": "2#JB", "lines": ["BETA"]}]
        }),
    )
    .await
    .unwrap();

    // Then
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"\xEF\xBB\xBFalpha\nBETA\ngamma\ndelta\n"
    );
}

#[tokio::test]
async fn edit_matches_hashline_anchors_against_crlf_and_preserves_line_endings() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("windows.txt");
    tokio::fs::write(&target, b"alpha\r\nbeta\r\ngamma\r\ndelta\r\n")
        .await
        .unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    tool.execute(
        &ctx,
        json!({
            "path": "windows.txt",
            "edits": [{
                "op": "replace",
                "pos": "2#JB",
                "end": "3#KJ",
                "lines": ["BETA", "GAMMA"]
            }]
        }),
    )
    .await
    .unwrap();

    // Then
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"alpha\r\nBETA\r\nGAMMA\r\ndelta\r\n"
    );
}

#[tokio::test]
async fn edit_replace_text_requires_one_exact_non_empty_match() {
    // Given
    let cases = vec![
        (
            "no match",
            "alpha\nbeta\ngamma\n",
            json!({"path": "notes.txt", "oldText": "missing", "newText": "BETA"}),
            "E_NO_MATCH",
        ),
        (
            "multiple matches",
            "alpha\nbeta\nbeta\n",
            json!({"path": "notes.txt", "oldText": "beta", "newText": "BETA"}),
            "E_MULTI_MATCH",
        ),
        (
            "overlapping matches",
            "ababa",
            json!({"path": "notes.txt", "oldText": "aba", "newText": "x"}),
            "E_MULTI_MATCH",
        ),
        (
            "empty old text",
            "alpha\n",
            json!({"path": "notes.txt", "oldText": "", "newText": "x"}),
            "E_BAD_OP",
        ),
    ];

    // When / Then
    for (name, initial, input, code) in cases {
        let workdir = tempdir();
        tokio::fs::write(workdir.join("notes.txt"), initial)
            .await
            .unwrap();
        let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
        let tool = ToolRegistry::builtins().get("edit").unwrap();
        let result = tool.execute(&ctx, input).await;
        assert_input_code(result, code);
        assert!(!name.is_empty());
    }
}

/// Committed integration failures are UTF-8-safe and bounded before ToolError storage.
#[tokio::test]
async fn edit_bounds_oversized_formatter_error_after_commit() {
    let workdir = tempdir();
    let target = workdir.join("oversized-error.txt");
    tokio::fs::write(&target, ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let read_ctx = ctx_with(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir.clone(),
    );
    let anchor = read_anchor(read.as_ref(), &read_ctx, "oversized-error.txt", 2).await;
    let edit_ctx = ctx_with_formatter(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir,
        FormatterPlane::new(Arc::new(OversizedFailingFormatter)),
    );

    let result = edit
        .execute(
            &edit_ctx,
            json!({
                "path": "oversized-error.txt",
                "edits": [{"op": "replace", "pos": anchor, "lines": ["BETA"]}]
            }),
        )
        .await;

    let message = match result {
        Err(ToolError::Other(message)) => message,
        other => panic!("oversized committed failure changed type: {other:?}"),
    };
    assert!(message.starts_with("File changed at "));
    assert!(message.contains("[truncated]"));
    assert!(!message.contains("SECRET_FORMATTER_TAIL"));
    let maximum_message_bytes = 8 * 1024 + target.to_string_lossy().len() + 128;
    assert!(message.len() <= maximum_message_bytes);
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"alpha\nBETA\ngamma\ndelta\n"
    );
}

/// A formatter error delivered with cancellation remains typed cancellation after commit.
#[tokio::test]
async fn edit_reconciles_formatter_error_with_cancellation_after_commit() {
    let workdir = tempdir();
    let target = workdir.join("cancel-formatter.txt");
    tokio::fs::write(&target, ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let read_ctx = ctx_with(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir.clone(),
    );
    let anchor = read_anchor(read.as_ref(), &read_ctx, "cancel-formatter.txt", 2).await;
    let cancel = CancellationToken::new();
    let edit_ctx = ctx_with_components(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir.clone(),
        FormatterPlane::new(Arc::new(CancellingFailingFormatter {
            cancel: cancel.clone(),
        })),
        LspPlane::default(),
        cancel,
    );
    let request = json!({
        "path": "cancel-formatter.txt",
        "edits": [{"op": "replace", "pos": anchor.clone(), "lines": ["BETA"]}]
    });

    let result = edit.execute(&edit_ctx, request.clone()).await;

    assert!(
        matches!(result, Err(ToolError::Cancelled)),
        "committed formatter cancellation must keep its typed outcome: {result:?}"
    );
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"alpha\nBETA\ngamma\ndelta\n"
    );
    let retry_ctx = ctx_with(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir,
    );
    let duplicate = edit.execute(&retry_ctx, request).await;
    assert!(
        matches!(&duplicate, Err(ToolError::Input(message)) if message.contains("[E_DUPLICATE_EDIT]")),
        "formatter cancellation reconciliation must retain final payload state: {duplicate:?}"
    );
}

/// LSP provider that records committed bytes before cancelling the Edit call.
#[derive(Clone)]
struct CancellingLsp {
    cancel: CancellationToken,
    observed: Arc<Mutex<Vec<Vec<u8>>>>,
}

#[async_trait]
impl LspProvider for CancellingLsp {
    /// Report an available client so post-edit reconciliation reaches LSP.
    async fn has_clients(&self, _file: &Path) -> Result<bool, LspError> {
        Ok(true)
    }

    /// Return no request results for this cancellation-only fixture.
    async fn execute(&self, _request: LspRequest) -> Result<Vec<Value>, LspError> {
        Ok(Vec::new())
    }

    /// Observe the committed bytes, then cancel while the LSP stage is active.
    async fn touch_file(&self, file: &Path, _kind: &str) -> Result<(), LspError> {
        let bytes = tokio::fs::read(file)
            .await
            .map_err(|error| LspError(error.to_string()))?;
        self.observed.lock().await.push(bytes);
        self.cancel.cancel();
        Ok(())
    }

    /// Return empty diagnostics after the cancellation signal is delivered.
    async fn diagnostics(&self) -> Result<Value, LspError> {
        Ok(json!({}))
    }
}

#[tokio::test]
async fn edit_reconciles_lsp_cancellation_after_commit() {
    // Given: a committed edit whose LSP touch cancels after observing final
    // bytes. The follow-up call uses the same registry-owned runtime.
    let workdir = tempdir();
    let target = workdir.join("cancel-lsp.txt");
    tokio::fs::write(&target, ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let lsp = LspPlane::new(Arc::new(CancellingLsp {
        cancel: cancel.clone(),
        observed: observed.clone(),
    }));
    let ctx = ctx_with_components(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir.clone(),
        FormatterPlane::default(),
        lsp,
        cancel,
    );
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let anchor = read_anchor(read.as_ref(), &ctx, "cancel-lsp.txt", 2).await;

    // When
    let result = edit
        .execute(
            &ctx,
            json!({
                "path": "cancel-lsp.txt",
                "edits": [{"op": "replace", "pos": anchor.clone(), "lines": ["BETA"]}]
            }),
        )
        .await;

    // Then: cancellation is reported even though the commit happened, and
    // the LSP observer saw the authoritative post-commit bytes.
    assert!(
        matches!(result, Err(ToolError::Cancelled)),
        "post-commit LSP cancellation must remain typed: {result:?}"
    );
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"alpha\nBETA\ngamma\ndelta\n"
    );
    let observed_bytes = observed.lock().await;
    assert_eq!(observed_bytes.len(), 1);
    assert_eq!(observed_bytes[0], b"alpha\nBETA\ngamma\ndelta\n");

    // Reconciliation records the committed payload guard, so retrying the
    // exact request cannot apply it a second time on the final bytes.
    let follow_up = ctx_with(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        workdir,
    );
    let duplicate = edit
        .execute(
            &follow_up,
            json!({
                "path": "cancel-lsp.txt",
                "edits": [{"op": "replace", "pos": anchor, "lines": ["BETA"]}]
            }),
        )
        .await;
    assert!(
        matches!(&duplicate, Err(ToolError::Input(message)) if message.contains("[E_DUPLICATE_EDIT]")),
        "reconciled cancellation must retain the duplicate guard: {duplicate:?}"
    );
}
