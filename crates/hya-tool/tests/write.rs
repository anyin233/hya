//! Public Write contracts for the host whole-file mutation tool.
//!
//! These tests exercise only the public `ToolRegistry`/`ToolCtx` seam. The
//! model-facing request is the closed `{path, content}` object; the negative
//! legacy-key case is retained only to prove that the removed `filePath`
//! spelling cannot mutate a file.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_proto::SessionId;
use hya_tool::{
    Action, FormatterError, FormatterPlane, FormatterProvider, InteractionPlane, LspError,
    LspPlane, LspProvider, LspRequest, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry, WebSearchPlane,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const RECOVERY_SOURCE: &str =
    "alpha\nbeta\ngamma\ndelta\nfive\nsix\nseven\neight\nalpha\nbeta\ngamma\ndelta\n";
const RECOVERY_EXTERNAL: &str =
    "EXTERNAL\nbeta\ngamma\ndelta\nfive\nsix\nseven\neight\nalpha\nbeta\ngamma\ndelta\n";
const RECOVERY_FINAL: &str =
    "EXTERNAL\nbeta\ngamma\ndelta\nfive\nsix\nseven\neight\nalpha\nBETA\ngamma\ndelta\n";

/// Build an allow rule for one tool or resource action.
fn allow(action: Action, pattern: &str) -> Rule {
    Rule::new(action, pattern, Mode::Allow)
}

/// Build a deny rule for one tool or resource action.
fn deny(action: Action, pattern: &str) -> Rule {
    Rule::new(action, pattern, Mode::Deny)
}

/// Create an isolated temporary workdir for one public Write scenario.
fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "hya-write-contract-{nanos}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

/// Construct a Write context with disconnected optional planes.
fn ctx_with(rules: Vec<Rule>, workdir: PathBuf) -> ToolCtx {
    ctx_with_components(
        rules,
        workdir,
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        None,
    )
}

/// Construct a Write context with custom formatter, LSP, cancellation, and session state.
fn ctx_with_components(
    rules: Vec<Rule>,
    workdir: PathBuf,
    formatter: FormatterPlane,
    lsp: LspPlane,
    cancel: CancellationToken,
    session: Option<SessionId>,
) -> ToolCtx {
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _interaction_rx) = InteractionPlane::new();
    let (spawner, _spawner_rx) = SpawnerPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        session,
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

/// Assert that one successful result exposes the required bounded Write envelope.
fn assert_write_metadata(result: &Value, target: &Path, bytes: usize, existed: bool) {
    assert_eq!(result["ok"], true);
    assert_eq!(
        result["metadata"]["path"],
        target.to_string_lossy().as_ref()
    );
    assert_eq!(result["metadata"]["bytes"], bytes);
    assert_eq!(result["metadata"]["existed"], existed);
    assert_eq!(result["metadata"]["hardLink"], false);
    assert_eq!(result["metadata"]["executable"], false);
    assert!(result["metadata"]["warnings"].is_array());
    assert!(result["metadata"]["diagnostics"].is_object());
    assert_eq!(result["metadata"]["display"]["type"], "file");
    assert_eq!(
        result["metadata"]["display"]["path"],
        target.to_string_lossy().as_ref()
    );
}

/// Assert that a successful mutation reports one warning containing a fragment.
fn assert_warning(result: &Value, fragment: &str) {
    let warnings = result["metadata"]["warnings"]
        .as_array()
        .expect("Write metadata must expose warnings as an array");
    assert!(
        warnings
            .iter()
            .filter_map(Value::as_str)
            .any(|warning| warning.contains(fragment)),
        "warnings {warnings:?} do not contain {fragment:?}"
    );
}

/// Return the bounded final display text from one successful Write result.
fn display_text(result: &Value) -> &str {
    result["metadata"]["display"]["text"]
        .as_str()
        .expect("Write metadata must expose final display text")
}

/// List replacement temporary files left directly in one workdir.
async fn temporary_files(directory: &Path) -> Vec<PathBuf> {
    let mut entries = tokio::fs::read_dir(directory).await.unwrap();
    let mut temporary = Vec::new();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".hya-hashline-") && name.ends_with(".tmp"))
        {
            temporary.push(path);
        }
    }
    temporary.sort();
    temporary
}

/// Formatter that rewrites the target to a deterministic final body.
struct RewritingFormatter;

#[async_trait]
impl FormatterProvider for RewritingFormatter {
    async fn status(
        &self,
        _workdir: &Path,
    ) -> Result<Vec<hya_tool::FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    async fn format_file(&self, _workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        tokio::fs::write(file, "formatted\n")
            .await
            .map_err(|error| FormatterError(error.to_string()))?;
        Ok(true)
    }
}

/// Formatter that fails after the Write atomic commit boundary.
struct FailingFormatter;

#[async_trait]
impl FormatterProvider for FailingFormatter {
    async fn status(
        &self,
        _workdir: &Path,
    ) -> Result<Vec<hya_tool::FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    async fn format_file(&self, _workdir: &Path, _file: &Path) -> Result<bool, FormatterError> {
        Err(FormatterError("forced formatter failure".to_owned()))
    }
}

/// Formatter that cancels its call after the Write commit has completed.
struct CancellingFormatter {
    cancel: CancellationToken,
}

#[async_trait]
impl FormatterProvider for CancellingFormatter {
    async fn status(
        &self,
        _workdir: &Path,
    ) -> Result<Vec<hya_tool::FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    async fn format_file(&self, _workdir: &Path, _file: &Path) -> Result<bool, FormatterError> {
        self.cancel.cancel();
        Ok(false)
    }
}

/// One observed LSP file touch used to prove formatter-before-LSP ordering.
#[derive(Debug)]
struct Touch {
    file: PathBuf,
    kind: String,
    content: String,
}

/// LSP provider that records the final file bytes and returns fixed diagnostics.
#[derive(Clone)]
struct RecordingLsp {
    touches: Arc<Mutex<Vec<Touch>>>,
    diagnostics: Value,
}

#[async_trait]
impl LspProvider for RecordingLsp {
    async fn has_clients(&self, _file: &Path) -> Result<bool, LspError> {
        Ok(true)
    }

    async fn execute(&self, _request: LspRequest) -> Result<Vec<Value>, LspError> {
        Ok(Vec::new())
    }

    async fn touch_file(&self, file: &Path, kind: &str) -> Result<(), LspError> {
        let content = tokio::fs::read_to_string(file)
            .await
            .map_err(|error| LspError(error.to_string()))?;
        self.touches.lock().await.push(Touch {
            file: file.to_path_buf(),
            kind: kind.to_owned(),
            content,
        });
        Ok(())
    }

    async fn diagnostics(&self) -> Result<Value, LspError> {
        Ok(self.diagnostics.clone())
    }
}

/// LSP provider that fails after the target has already been committed.
struct FailingLsp;

#[async_trait]
impl LspProvider for FailingLsp {
    async fn has_clients(&self, _file: &Path) -> Result<bool, LspError> {
        Ok(true)
    }

    async fn execute(&self, _request: LspRequest) -> Result<Vec<Value>, LspError> {
        Ok(Vec::new())
    }

    async fn touch_file(&self, _file: &Path, _kind: &str) -> Result<(), LspError> {
        Err(LspError("forced LSP touch failure".to_owned()))
    }

    async fn diagnostics(&self) -> Result<Value, LspError> {
        Ok(json!({}))
    }
}

/// Build diagnostics keyed by the final target path.
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

#[tokio::test]
async fn write_creates_parents_and_reports_final_metadata() {
    let directory = tempdir();
    let target = directory.join("src/generated/config.txt");
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory.clone());
    let tool = ToolRegistry::builtins().get("write").unwrap();
    let content = "hello\n";

    let result = tool
        .execute(
            &ctx,
            json!({ "path": "src/generated/config.txt", "content": content }),
        )
        .await
        .unwrap();

    let bytes = tokio::fs::read(&target).await.unwrap();
    assert_eq!(bytes, content.as_bytes());
    assert_write_metadata(&result, &target, bytes.len(), false);
    assert_eq!(result["title"], "src/generated/config.txt");
    assert_eq!(
        result["output"],
        "Wrote file successfully.\n\n<content>\nhello\n\n</content>"
    );
    assert!(temporary_files(&directory).await.is_empty());
}

#[tokio::test]
async fn write_overwrites_existing_file_exactly_and_reports_existed() {
    let directory = tempdir();
    let target = directory.join("notes.txt");
    tokio::fs::write(&target, "old bytes\n").await.unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();
    let content = "new bytes\nwith a second line";

    let result = tool
        .execute(&ctx, json!({ "path": "notes.txt", "content": content }))
        .await
        .unwrap();

    assert_eq!(tokio::fs::read(&target).await.unwrap(), content.as_bytes());
    assert_eq!(result["metadata"]["existed"], true);
    assert_eq!(result["metadata"]["bytes"], content.len());
}

#[test]
fn write_schema_is_closed_and_advertises_only_path_and_content() {
    let tool = ToolRegistry::builtins().get("write").unwrap();
    let schema = tool.schema().input_schema;
    let properties = schema["properties"].as_object().unwrap();

    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"], json!(["path", "content"]));
    assert_eq!(properties.len(), 2);
    assert_eq!(properties["path"]["type"], "string");
    assert_eq!(properties["content"]["type"], "string");
    assert!(!properties.contains_key("filePath"));
}

#[tokio::test]
async fn write_rejects_legacy_file_path_without_mutation() {
    let directory = tempdir();
    let target = directory.join("legacy.txt");
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(
            &ctx,
            json!({ "filePath": "legacy.txt", "content": "must not write\n" }),
        )
        .await;

    assert!(matches!(result, Err(ToolError::Input(_))));
    assert!(!target.exists());
}

#[tokio::test]
async fn write_strips_an_unambiguous_full_hashline_block() {
    let directory = tempdir();
    let target = directory.join("destination.txt");
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory.clone());
    let tool = ToolRegistry::builtins().get("write").unwrap();
    let copied = "[destination.txt#ABCD]\n1#KT:alpha\n2#JB:beta\n3#KJ:gamma\n4#PX:delta";

    let result = tool
        .execute(
            &ctx,
            json!({ "path": "destination.txt", "content": copied }),
        )
        .await
        .unwrap();

    assert!(result["output"].as_str().unwrap().contains("stripped"));

    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"alpha\nbeta\ngamma\ndelta"
    );
}

#[tokio::test]
async fn write_preserves_ambiguous_hashline_looking_content_verbatim() {
    let directory = tempdir();
    let target = directory.join("ambiguous.txt");
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory.clone());
    let tool = ToolRegistry::builtins().get("write").unwrap();
    let content = "[ambiguous.txt#ABCD]\n1#KT:alpha\nthis line has no rendered prefix";

    tool.execute(&ctx, json!({ "path": "ambiguous.txt", "content": content }))
        .await
        .unwrap();

    assert_eq!(tokio::fs::read(&target).await.unwrap(), content.as_bytes());
}

#[tokio::test]
async fn write_checks_edit_permission_before_mutating() {
    let directory = tempdir();
    let target = directory.join("blocked.txt");
    let ctx = ctx_with(vec![deny(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(&ctx, json!({ "path": "blocked.txt", "content": "nope\n" }))
        .await;

    assert!(matches!(result, Err(ToolError::Permission(_))));
    assert!(!target.exists());
}

#[tokio::test]
async fn write_requires_external_directory_permission_for_outside_path() {
    let directory = tempdir();
    let outside = tempdir().join("outside.txt");
    let ctx = ctx_with(
        vec![
            allow(Action::Edit, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        directory,
    );
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(
            &ctx,
            json!({ "path": outside.to_string_lossy(), "content": "nope\n" }),
        )
        .await;

    assert!(matches!(result, Err(ToolError::Permission(_))));
    assert!(!outside.exists());
}

#[tokio::test]
async fn write_preserves_existing_utf8_bom() {
    let directory = tempdir();
    let target = directory.join("bom.txt");
    tokio::fs::write(&target, b"\xEF\xBB\xBFold\n")
        .await
        .unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    tool.execute(&ctx, json!({ "path": "bom.txt", "content": "new\n" }))
        .await
        .unwrap();

    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"\xEF\xBB\xBFnew\n"
    );
}

#[tokio::test]
async fn write_uses_incoming_utf8_bom_without_duplicating_it() {
    let directory = tempdir();
    let target = directory.join("incoming-bom.txt");
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    tool.execute(
        &ctx,
        json!({ "path": "incoming-bom.txt", "content": "\u{feff}created\n" }),
    )
    .await
    .unwrap();

    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"\xEF\xBB\xBFcreated\n"
    );
}

#[tokio::test]
async fn write_preserves_crlf_line_endings() {
    let directory = tempdir();
    let target = directory.join("crlf.txt");
    tokio::fs::write(&target, b"old\r\nline\r\n").await.unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    tool.execute(
        &ctx,
        json!({ "path": "crlf.txt", "content": "new\nline\n" }),
    )
    .await
    .unwrap();

    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"new\r\nline\r\n");
}

#[tokio::test]
async fn write_reports_invalid_utf8_rewrite_warning() {
    let directory = tempdir();
    let target = directory.join("invalid.txt");
    tokio::fs::write(&target, b"old\n\xFF\n").await.unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(&ctx, json!({ "path": "invalid.txt", "content": "new\n" }))
        .await
        .unwrap();

    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"new\n");
    assert_warning(&result, "Invalid UTF-8");
}

#[tokio::test]
async fn write_warns_for_mixed_line_endings() {
    let directory = tempdir();
    let target = directory.join("mixed.txt");
    tokio::fs::write(&target, b"old\r\nline\nlast\r")
        .await
        .unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(
            &ctx,
            json!({ "path": "mixed.txt", "content": "new\nvalue\n" }),
        )
        .await
        .unwrap();

    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"new\r\nvalue\r\n");
    assert_warning(&result, "Mixed line endings");
}

#[tokio::test]
async fn write_runs_formatter_and_reports_final_preview_truth() {
    let directory = tempdir();
    let target = directory.join("formatted.txt");
    let formatter = FormatterPlane::new(Arc::new(RewritingFormatter));
    let ctx = ctx_with_components(
        vec![allow(Action::Edit, "*")],
        directory,
        formatter,
        LspPlane::default(),
        CancellationToken::new(),
        None,
    );
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(&ctx, json!({ "path": "formatted.txt", "content": "raw\n" }))
        .await
        .unwrap();

    let bytes = tokio::fs::read(&target).await.unwrap();
    assert_eq!(bytes, b"formatted\n");
    assert_eq!(result["metadata"]["bytes"], bytes.len());
    assert!(display_text(&result).contains("formatted"));
    assert!(!display_text(&result).contains("raw"));
    let output = result["output"].as_str().unwrap();
    assert!(output.contains("formatted"));
    assert!(!output.contains("raw"));
}

#[tokio::test]
async fn write_post_commit_formatter_failure_reports_changed_file() {
    let directory = tempdir();
    let target = directory.join("formatter-failure.txt");
    let formatter = FormatterPlane::new(Arc::new(FailingFormatter));
    let ctx = ctx_with_components(
        vec![allow(Action::Edit, "*")],
        directory.clone(),
        formatter,
        LspPlane::default(),
        CancellationToken::new(),
        None,
    );
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let error = tool
        .execute(
            &ctx,
            json!({ "path": "formatter-failure.txt", "content": "committed\n" }),
        )
        .await
        .expect_err("formatter failure must remain visible");

    let message = error.to_string();
    assert!(
        message.contains("File changed"),
        "missing changed-file context: {message}"
    );
    assert!(message.contains(&target.to_string_lossy().to_string()));
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"committed\n");
    assert!(temporary_files(&directory).await.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn write_creates_mode_0600_file_and_reports_non_executable() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir();
    let target = directory.join("new-mode.txt");
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(&ctx, json!({ "path": "new-mode.txt", "content": "new\n" }))
        .await
        .unwrap();

    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    assert_eq!(result["metadata"]["executable"], false);
}

#[cfg(unix)]
#[tokio::test]
async fn write_preserves_existing_mode() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir();
    let target = directory.join("existing-mode.txt");
    tokio::fs::write(&target, "old\n").await.unwrap();
    let mut permissions = std::fs::metadata(&target).unwrap().permissions();
    permissions.set_mode(0o640);
    std::fs::set_permissions(&target, permissions).unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(
            &ctx,
            json!({ "path": "existing-mode.txt", "content": "new\n" }),
        )
        .await
        .unwrap();

    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
        0o640
    );
    assert_eq!(result["metadata"]["existed"], true);
}

#[cfg(unix)]
#[tokio::test]
async fn write_updates_hard_link_in_place_and_reports_fact() {
    use std::os::unix::fs::MetadataExt;

    let directory = tempdir();
    let target = directory.join("target.txt");
    let sibling = directory.join("sibling.txt");
    tokio::fs::write(&target, "old\n").await.unwrap();
    std::fs::hard_link(&target, &sibling).unwrap();
    let inode_before = std::fs::metadata(&target).unwrap().ino();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(&ctx, json!({ "path": "target.txt", "content": "new\n" }))
        .await
        .unwrap();

    assert_eq!(std::fs::metadata(&target).unwrap().ino(), inode_before);
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"new\n");
    assert_eq!(tokio::fs::read(&sibling).await.unwrap(), b"new\n");
    assert_eq!(result["metadata"]["hardLink"], true);
}

#[cfg(unix)]
#[tokio::test]
async fn write_follows_symlink_and_reports_resolved_target() {
    use std::os::unix::fs::symlink;

    let directory = tempdir();
    let target = directory.join("real.txt");
    let link = directory.join("relative.txt");
    tokio::fs::write(&target, "old\n").await.unwrap();
    symlink("real.txt", &link).unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(&ctx, json!({ "path": "relative.txt", "content": "new\n" }))
        .await
        .unwrap();

    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"new\n");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        PathBuf::from("real.txt")
    );
    assert_eq!(
        result["metadata"]["path"],
        target.to_string_lossy().as_ref()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn write_marks_shebang_executable_and_reports_fact() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir();
    let target = directory.join("script.sh");
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(
            &ctx,
            json!({ "path": "script.sh", "content": "#!/bin/sh\nprintf 'ok\\n'\n" }),
        )
        .await
        .unwrap();

    let mode = std::fs::metadata(&target).unwrap().permissions().mode();
    assert_ne!(mode & 0o111, 0, "shebang Write must add execute bits");
    assert_eq!(result["metadata"]["executable"], true);
}

#[tokio::test]
async fn write_touches_lsp_after_formatter_and_returns_diagnostics() {
    let directory = tempdir();
    let target = directory.join("main.rs");
    let diagnostics = error_diagnostics(&target, "bad write");
    let touches = Arc::new(Mutex::new(Vec::new()));
    let lsp = LspPlane::new(Arc::new(RecordingLsp {
        touches: touches.clone(),
        diagnostics: diagnostics.clone(),
    }));
    let formatter = FormatterPlane::new(Arc::new(RewritingFormatter));
    let ctx = ctx_with_components(
        vec![allow(Action::Edit, "*")],
        directory,
        formatter,
        lsp,
        CancellationToken::new(),
        None,
    );
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(&ctx, json!({ "path": "main.rs", "content": "raw\n" }))
        .await
        .unwrap();

    assert_eq!(result["metadata"]["diagnostics"], diagnostics);
    assert!(result["output"].as_str().unwrap().contains("bad write"));
    let calls = touches.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].file, target);
    assert_eq!(calls[0].kind, "document");
    assert_eq!(calls[0].content, "formatted\n");
}

#[tokio::test]
async fn write_post_commit_lsp_failure_reports_changed_file() {
    let directory = tempdir();
    let target = directory.join("lsp-failure.txt");
    let ctx = ctx_with_components(
        vec![allow(Action::Edit, "*")],
        directory,
        FormatterPlane::default(),
        LspPlane::new(Arc::new(FailingLsp)),
        CancellationToken::new(),
        None,
    );
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let error = tool
        .execute(
            &ctx,
            json!({ "path": "lsp-failure.txt", "content": "committed\n" }),
        )
        .await
        .expect_err("LSP failure must remain visible");

    let message = error.to_string();
    assert!(
        message.contains("File changed"),
        "missing changed-file context: {message}"
    );
    assert!(message.contains("LSP"));
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"committed\n");
}

#[tokio::test]
async fn cancelled_write_returns_without_mutation_or_snapshot() {
    let directory = tempdir();
    let target = directory.join("cancelled.txt");
    let session = SessionId::new();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx = ctx_with_components(
        vec![allow(Action::Edit, "*")],
        directory.clone(),
        FormatterPlane::default(),
        LspPlane::default(),
        cancel,
        Some(session),
    );
    let registry = ToolRegistry::builtins();
    let write = registry.get("write").unwrap();

    let result = write
        .execute(
            &ctx,
            json!({ "path": "cancelled.txt", "content": "alpha\nbeta\ngamma\ndelta\n" }),
        )
        .await;
    assert!(matches!(result, Err(ToolError::Cancelled)));
    assert!(!target.exists());

    // A later external file with a stale pinned anchor must not recover from
    // the cancelled call's nonexistent snapshot.
    tokio::fs::write(&target, "EXTERNAL\nbeta\ngamma\ndelta\n")
        .await
        .unwrap();
    let edit_ctx = ctx_with_components(
        vec![allow(Action::Edit, "*")],
        directory,
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        Some(session),
    );
    let edit = registry.get("edit").unwrap();
    let edit_result = edit
        .execute(
            &edit_ctx,
            json!({
                "path": "cancelled.txt",
                "edits": [{"op": "replace", "pos": "2#JB", "lines": ["BETA"]}]
            }),
        )
        .await;
    let message = edit_result
        .expect_err("cancelled Write must not seed recovery state")
        .to_string();
    assert!(message.contains("E_STALE_ANCHOR"));
}

#[tokio::test]
async fn write_records_snapshot_for_stale_edit_recovery() {
    let directory = tempdir();
    let target = directory.join("snapshot.txt");
    let session = SessionId::new();
    let registry = ToolRegistry::builtins();
    let write_ctx = ctx_with_components(
        vec![allow(Action::Edit, "*")],
        directory.clone(),
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        Some(session),
    );
    let write = registry.get("write").unwrap();
    write
        .execute(
            &write_ctx,
            json!({ "path": "snapshot.txt", "content": RECOVERY_SOURCE }),
        )
        .await
        .unwrap();

    tokio::fs::write(&target, RECOVERY_EXTERNAL).await.unwrap();
    let edit_ctx = ctx_with_components(
        vec![allow(Action::Edit, "*")],
        directory,
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        Some(session),
    );
    let edit = registry.get("edit").unwrap();
    let result = edit
        .execute(
            &edit_ctx,
            json!({
                "path": "snapshot.txt",
                "edits": [
                    {"op": "replace", "pos": "2#JB", "lines": ["beta"]},
                    {"op": "replace", "pos": "10#JB", "lines": ["BETA"]}
                ]
            }),
        )
        .await
        .unwrap();

    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        RECOVERY_FINAL.as_bytes()
    );
    assert_eq!(result["metadata"]["recovered"], true);
}

#[tokio::test]
async fn cancellation_after_write_commit_reconciles_final_state() {
    let directory = tempdir();
    let target = directory.join("cancel-after-commit.txt");
    let session = SessionId::new();
    let cancel = CancellationToken::new();
    let formatter = FormatterPlane::new(Arc::new(CancellingFormatter {
        cancel: cancel.clone(),
    }));
    let ctx = ctx_with_components(
        vec![allow(Action::Edit, "*")],
        directory.clone(),
        formatter,
        LspPlane::default(),
        cancel,
        Some(session),
    );
    let registry = ToolRegistry::builtins();
    let write = registry.get("write").unwrap();

    let result = write
        .execute(
            &ctx,
            json!({ "path": "cancel-after-commit.txt", "content": RECOVERY_SOURCE }),
        )
        .await;
    assert!(matches!(result, Err(ToolError::Cancelled)));
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        RECOVERY_SOURCE.as_bytes()
    );

    tokio::fs::write(&target, RECOVERY_EXTERNAL).await.unwrap();
    let edit_ctx = ctx_with_components(
        vec![allow(Action::Edit, "*")],
        directory,
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        Some(session),
    );
    let edit = registry.get("edit").unwrap();
    let recovered = edit
        .execute(
            &edit_ctx,
            json!({
                "path": "cancel-after-commit.txt",
                "edits": [
                    {"op": "replace", "pos": "2#JB", "lines": ["beta"]},
                    {"op": "replace", "pos": "10#JB", "lines": ["BETA"]}
                ]
            }),
        )
        .await
        .unwrap();
    assert_eq!(recovered["metadata"]["recovered"], true);
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        RECOVERY_FINAL.as_bytes()
    );
}

#[tokio::test]
async fn write_bounds_result_and_display_metadata_without_losing_facts() {
    let directory = tempdir();
    let target = directory.join("large.txt");
    let content = "x\n".repeat(30_000);
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], directory);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    let result = tool
        .execute(&ctx, json!({ "path": "large.txt", "content": content }))
        .await
        .unwrap();

    let output = result["output"].as_str().unwrap();
    let display = &result["metadata"]["display"];
    assert!(output.len() <= 50 * 1024);
    assert!(display_text(&result).len() <= 50 * 1024);
    assert!(result["content"].as_str().unwrap().len() <= 50 * 1024);
    assert_eq!(display["truncated"], true);
    assert!(result["metadata"]["bytes"].as_u64().unwrap() > 50 * 1024);
    assert!(result["metadata"]["path"].is_string());
    assert!(result["metadata"]["warnings"].is_array());
    assert!(result["metadata"]["diagnostics"].is_object());
    assert!(
        result["metadata"]["display"]["totalLines"]
            .as_u64()
            .unwrap()
            > 1
    );
    assert_eq!(
        tokio::fs::metadata(&target).await.unwrap().len(),
        result["metadata"]["bytes"].as_u64().unwrap()
    );
}
