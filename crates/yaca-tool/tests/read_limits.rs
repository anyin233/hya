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
        "yaca-read-limits-{nanos}-{}-{id}",
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
        formatter: yaca_tool::FormatterPlane::default(),
        workdir,
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn read_truncates_individual_lines_to_open_code_limit() {
    // Given
    let workdir = tempdir();
    let long_line = "a".repeat(2001);
    tokio::fs::write(workdir.join("long.txt"), format!("{long_line}\n"))
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);
    let expected = format!("{}... (line truncated to 2000 chars)", "a".repeat(2000));

    // When
    let out = tool
        .execute(&ctx, json!({ "filePath": "long.txt" }))
        .await
        .unwrap();

    // Then
    assert_eq!(out["content"], expected);
    assert_eq!(out["metadata"]["display"]["text"], expected);
    assert_eq!(out["metadata"]["truncated"], false);
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains(&format!("1: {expected}"))
    );
}

#[tokio::test]
async fn read_caps_file_output_at_fifty_kilobytes() {
    // Given
    let workdir = tempdir();
    let line = "a".repeat(1000);
    let content = (0..60)
        .map(|_| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(workdir.join("large.txt"), content)
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(&ctx, json!({ "filePath": "large.txt" }))
        .await
        .unwrap();

    // Then
    assert_eq!(out["metadata"]["truncated"], true);
    assert_eq!(out["metadata"]["display"]["lineEnd"], 51);
    assert_eq!(out["metadata"]["display"]["totalLines"], 52);
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains("(Output capped at 50 KB. Showing lines 1-51. Use offset=52 to continue.)")
    );
}
