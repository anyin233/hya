#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolRegistry, WebSearchPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

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
        "hya-edit-fuzzy-{nanos}-{}-{id}",
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
        formatter: hya_tool::FormatterPlane::default(),
        agents: hya_tool::AgentCatalogPlane::default(),
        workdir,
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn edit_matches_line_trimmed_block_like_compat() {
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
async fn edit_matches_whitespace_normalized_substring_like_compat() {
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

#[tokio::test]
async fn edit_matches_similar_block_anchor_like_compat() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("config.ts");
    tokio::fs::write(
        &target,
        [
            "function configure() {",
            "  const enabled = true",
            "  auditLog()",
            "}",
            "",
        ]
        .join("\n"),
    )
    .await
    .unwrap();
    let ctx = ctx_with(workdir);
    let tool = ToolRegistry::builtins().get("edit").unwrap();
    let old = [
        "function configure() {",
        "  const enabled = false",
        "  auditLog()",
        "}",
    ]
    .join("\n");
    let new = [
        "function configure() {",
        "  const enabled = reviewed",
        "  auditLog()",
        "}",
    ]
    .join("\n");

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "filePath": "config.ts",
                "oldString": old,
                "newString": new
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["replaced"], 1);
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "function configure() {\n  const enabled = reviewed\n  auditLog()\n}\n"
    );
}

#[tokio::test]
async fn edit_matches_escape_normalized_string_like_compat() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("text.txt");
    tokio::fs::write(&target, "before\nalpha\nbeta\nafter\n")
        .await
        .unwrap();
    let ctx = ctx_with(workdir);
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "filePath": "text.txt",
                "oldString": "alpha\\nbeta",
                "newString": "gamma"
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["replaced"], 1);
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "before\ngamma\nafter\n"
    );
}

#[tokio::test]
async fn edit_matches_trimmed_boundary_block_like_compat() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("trimmed.txt");
    tokio::fs::write(&target, "alpha\nbeta\ngamma\n")
        .await
        .unwrap();
    let ctx = ctx_with(workdir);
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "filePath": "trimmed.txt",
                "oldString": "\nalpha\nbeta\n",
                "newString": "delta"
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["replaced"], 1);
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "delta\ngamma\n"
    );
}

#[tokio::test]
async fn edit_matches_context_aware_block_like_compat() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("context.ts");
    tokio::fs::write(
        &target,
        ["function configure() {", "  keep()", "  actual()", "}", ""].join("\n"),
    )
    .await
    .unwrap();
    let ctx = ctx_with(workdir);
    let tool = ToolRegistry::builtins().get("edit").unwrap();
    let old = ["function configure() {", "  keep()", "  expected()", "}"].join("\n");
    let new = ["function configure() {", "  keep()", "  replacement()", "}"].join("\n");

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "filePath": "context.ts",
                "oldString": old,
                "newString": new
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["replaced"], 1);
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "function configure() {\n  keep()\n  replacement()\n}\n"
    );
}
