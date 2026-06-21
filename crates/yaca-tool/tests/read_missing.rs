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

fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "yaca-read-missing-{nanos}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with(workdir: PathBuf) -> ToolCtx {
    let (permission, _rx) =
        PermissionPlane::new(PermissionRules::new(vec![allow(Action::Read, "*")]));
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
async fn read_suggests_similar_files_when_path_is_missing() {
    // Given
    let workdir = tempdir();
    let suggestion = workdir.join("config.toml");
    tokio::fs::write(&suggestion, "ok = true\n").await.unwrap();
    let missing = workdir.join("config");
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let result = tool.execute(&ctx, json!({ "filePath": "config" })).await;

    // Then
    assert!(
        matches!(result, Err(ToolError::Other(message)) if message == format!(
            "File not found: {}\n\nDid you mean one of these?\n{}",
            missing.to_string_lossy(),
            suggestion.to_string_lossy()
        ))
    );
}
