#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_tool::{
    Action, FormatterError, FormatterPlane, FormatterProvider, InteractionPlane, LspPlane, Mode,
    PermissionPlane, PermissionRules, Rule, SkillPlane, SpawnerPlane, TodoPlane, ToolCtx,
    ToolError, ToolRegistry, WebSearchPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn allow(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Allow)
}

fn deny(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Deny)
}

fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("hya-edit-{nanos}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with(rules: Vec<Rule>, workdir: PathBuf) -> ToolCtx {
    ctx_with_formatter(rules, workdir, FormatterPlane::default())
}

fn ctx_with_formatter(rules: Vec<Rule>, workdir: PathBuf, formatter: FormatterPlane) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        permission,
        interaction,
        spawner,
        mailbox: hya_tool::MailboxPlane::disconnected(),
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter,
        agents: hya_tool::AgentCatalogPlane::default(),
        workdir,
        cancel: CancellationToken::new(),
    }
}

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
        tokio::fs::write(file, "formatted\n").await.unwrap();
        Ok(true)
    }
}

#[tokio::test]
async fn edit_requires_external_directory_permission_for_outside_file_path() {
    // Given
    let workdir = tempdir();
    let outside_dir = tempdir();
    let outside = outside_dir.join("outside.txt");
    tokio::fs::write(&outside, "old\n").await.unwrap();
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
                "filePath": outside.to_string_lossy(),
                "oldString": "old\n",
                "newString": "new\n"
            }),
        )
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Permission(_))));
    assert_eq!(tokio::fs::read_to_string(&outside).await.unwrap(), "old\n");
}

#[tokio::test]
async fn edit_preserves_existing_utf8_bom_without_duplicating_incoming_bom() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("bom.txt");
    tokio::fs::write(&target, b"\xEF\xBB\xBFold\n")
        .await
        .unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    tool.execute(
        &ctx,
        json!({
            "filePath": "bom.txt",
            "oldString": "old\n",
            "newString": "\u{feff}new\n"
        }),
    )
    .await
    .unwrap();

    // Then
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"\xEF\xBB\xBFnew\n"
    );
}

#[tokio::test]
async fn edit_runs_formatter_after_successful_edit() {
    // Given
    let dir = tempdir();
    let target = dir.join("target.txt");
    tokio::fs::write(&target, "old\n").await.unwrap();
    let formatter = FormatterPlane::new(Arc::new(RewritingFormatter));
    let ctx = ctx_with_formatter(vec![allow(Action::Edit, "*")], dir.clone(), formatter);
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    tool.execute(
        &ctx,
        json!({
            "filePath": "target.txt",
            "oldString": "old\n",
            "newString": "new\n"
        }),
    )
    .await
    .unwrap();

    // Then
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "formatted\n"
    );
}

#[tokio::test]
async fn edit_matches_lf_parameters_against_crlf_files_and_preserves_crlf() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("windows.txt");
    tokio::fs::write(&target, b"first\r\nold\r\nlast\r\n")
        .await
        .unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    tool.execute(
        &ctx,
        json!({
            "filePath": "windows.txt",
            "oldString": "old\nlast\n",
            "newString": "new\nlast\n"
        }),
    )
    .await
    .unwrap();

    // Then
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"first\r\nnew\r\nlast\r\n"
    );
}

#[tokio::test]
async fn edit_returns_open_code_success_metadata_with_diff() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("notes.txt");
    tokio::fs::write(&target, "one\ntwo\n").await.unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], workdir);
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "filePath": "notes.txt",
                "oldString": "two\n",
                "newString": "three\n"
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["title"], "notes.txt");
    assert_eq!(out["output"], "Edit applied successfully.");
    assert_eq!(out["metadata"]["diagnostics"], json!({}));
    assert_eq!(
        out["metadata"]["filediff"]["file"],
        target.to_string_lossy().as_ref()
    );
    assert_eq!(out["metadata"]["filediff"]["additions"], 1);
    assert_eq!(out["metadata"]["filediff"]["deletions"], 1);
    let diff = out["metadata"]["diff"].as_str().unwrap();
    assert!(diff.contains("--- notes.txt") || diff.contains("--- "));
    assert!(diff.contains("-two"));
    assert!(diff.contains("+three"));
}
