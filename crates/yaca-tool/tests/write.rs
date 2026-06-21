#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio_util::sync::CancellationToken;
use yaca_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry, WebSearchPlane,
};

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
    let dir = std::env::temp_dir().join(format!("yaca-write-{nanos}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with(rules: Vec<Rule>, workdir: PathBuf) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        permission,
        interaction,
        spawner,
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        workdir,
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn write_accepts_open_code_file_path_and_returns_metadata() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());
    let tool = ToolRegistry::builtins().get("write").unwrap();

    // When
    let out = tool
        .execute(
            &ctx,
            json!({ "filePath": "src/generated.txt", "content": "hello\n" }),
        )
        .await
        .unwrap();

    // Then
    let target = dir.join("src/generated.txt");
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "hello\n");
    assert_eq!(out["title"], "src/generated.txt");
    assert_eq!(out["output"], "Wrote file successfully.");
    assert_eq!(
        out["metadata"]["filepath"],
        target.to_string_lossy().as_ref()
    );
    assert_eq!(out["metadata"]["exists"], false);
    assert_eq!(out["ok"], true);
    assert_eq!(out["bytes"], 6);
}

#[tokio::test]
async fn write_reports_existing_file_when_overwriting() {
    // Given
    let dir = tempdir();
    let target = dir.join("notes.txt");
    tokio::fs::write(&target, "old\n").await.unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());
    let tool = ToolRegistry::builtins().get("write").unwrap();

    // When
    let out = tool
        .execute(&ctx, json!({ "filePath": "notes.txt", "content": "new\n" }))
        .await
        .unwrap();

    // Then
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "new\n");
    assert_eq!(out["metadata"]["exists"], true);
}

#[tokio::test]
async fn write_preserves_existing_utf8_bom() {
    // Given
    let dir = tempdir();
    let target = dir.join("bom.txt");
    tokio::fs::write(&target, b"\xEF\xBB\xBFold\n")
        .await
        .unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());
    let tool = ToolRegistry::builtins().get("write").unwrap();

    // When
    tool.execute(&ctx, json!({ "filePath": "bom.txt", "content": "new\n" }))
        .await
        .unwrap();

    // Then
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"\xEF\xBB\xBFnew\n"
    );
}

#[tokio::test]
async fn write_uses_incoming_utf8_bom_without_duplicating_it() {
    // Given
    let dir = tempdir();
    let target = dir.join("incoming-bom.txt");
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());
    let tool = ToolRegistry::builtins().get("write").unwrap();

    // When
    tool.execute(
        &ctx,
        json!({ "filePath": "incoming-bom.txt", "content": "\u{feff}created\n" }),
    )
    .await
    .unwrap();

    // Then
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"\xEF\xBB\xBFcreated\n"
    );
}

#[tokio::test]
async fn write_checks_edit_permission_before_writing_file_path() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![deny(Action::Edit, "*")], dir.clone());
    let tool = ToolRegistry::builtins().get("write").unwrap();

    // When
    let result = tool
        .execute(
            &ctx,
            json!({ "filePath": "blocked.txt", "content": "nope\n" }),
        )
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Permission(_))));
    assert!(!dir.join("blocked.txt").exists());
}

#[tokio::test]
async fn write_requires_external_directory_permission_for_outside_file_path() {
    // Given
    let dir = tempdir();
    let outside = tempdir().join("outside.txt");
    let ctx = ctx_with(
        vec![
            allow(Action::Edit, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        dir,
    );
    let tool = ToolRegistry::builtins().get("write").unwrap();

    // When
    let result = tool
        .execute(
            &ctx,
            json!({ "filePath": outside.to_string_lossy(), "content": "nope\n" }),
        )
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Permission(_))));
    assert!(!outside.exists());
}
