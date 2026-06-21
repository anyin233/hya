#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio_util::sync::CancellationToken;
use yaca_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolRegistry, WebSearchPlane,
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
        "yaca-edit-fuzzy-{nanos}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with(workdir: PathBuf) -> ToolCtx {
    let (permission, _rx) =
        PermissionPlane::new(PermissionRules::new(vec![allow(Action::Edit, "*")]));
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
async fn edit_matches_line_trimmed_block_like_opencode() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("main.rs");
    tokio::fs::write(&target, "fn main() {\n    println!(\"old\");\n}\n")
        .await
        .unwrap();
    let ctx = ctx_with(workdir);
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "filePath": "main.rs",
                "oldString": "fn main() {\nprintln!(\"old\");\n}",
                "newString": "fn main() {\n    println!(\"new\");\n}"
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["replaced"], 1);
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "fn main() {\n    println!(\"new\");\n}\n"
    );
}

#[tokio::test]
async fn edit_matches_whitespace_normalized_substring_like_opencode() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("math.rs");
    tokio::fs::write(&target, "let value = alpha   +   beta;\n")
        .await
        .unwrap();
    let ctx = ctx_with(workdir);
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "filePath": "math.rs",
                "oldString": "alpha + beta",
                "newString": "gamma"
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["replaced"], 1);
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "let value = gamma;\n"
    );
}
