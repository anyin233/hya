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
    let dir = std::env::temp_dir().join(format!("yaca-shell-{nanos}-{}-{id}", std::process::id()));
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
async fn shell_runs_in_open_code_workdir_and_returns_description_metadata() {
    // Given
    let dir = tempdir();
    let subdir = dir.join("subdir");
    tokio::fs::create_dir_all(&subdir).await.unwrap();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir.clone());
    let tool = ToolRegistry::builtins().get("shell").unwrap();

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "command": "pwd",
                "description": "Prints working directory",
                "workdir": "subdir",
                "timeout": 1000
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["title"], "Prints working directory");
    assert_eq!(out["metadata"]["description"], "Prints working directory");
    assert_eq!(out["metadata"]["exit"], 0);
    assert_eq!(out["metadata"]["truncated"], false);
    assert_eq!(out["exit_code"], 0);
    assert_eq!(
        out["stdout"].as_str().unwrap().trim(),
        subdir.to_string_lossy()
    );
    assert_eq!(
        out["output"].as_str().unwrap().trim(),
        subdir.to_string_lossy()
    );
}

#[tokio::test]
async fn shell_merges_input_env_into_child_process() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("shell").unwrap();

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "command": "printf %s \"$YACA_SHELL_ENV\"",
                "timeout": 1000,
                "env": { "YACA_SHELL_ENV": "from-plugin" }
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["exit_code"], 0);
    assert_eq!(out["stdout"], "from-plugin");
}

#[tokio::test]
async fn shell_times_out_and_reports_shell_metadata() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("shell").unwrap();

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "command": "sleep 1",
                "description": "Sleeps too long",
                "timeout": 50
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["title"], "Sleeps too long");
    assert_eq!(out["metadata"]["exit"], json!(null));
    assert!(out["output"].as_str().unwrap().contains("<shell_metadata>"));
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains("exceeding timeout 50 ms")
    );
}

#[tokio::test]
async fn shell_checks_bash_permission_before_running() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![deny(Action::Bash, "*")], dir);
    let tool = ToolRegistry::builtins().get("shell").unwrap();

    // When
    let result = tool
        .execute(
            &ctx,
            json!({ "command": "echo blocked", "description": "Prints blocked" }),
        )
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Permission(_))));
}

#[tokio::test]
async fn shell_saves_full_output_when_truncated() {
    // Given
    let dir = tempdir();
    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir.clone());
    let tool = ToolRegistry::builtins().get("shell").unwrap();
    let command = "python3 - <<'PY'\nprint('a' * 20000)\nPY";

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "command": command,
                "description": "Print large output",
                "timeout": 1000
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["metadata"]["truncated"], true);
    let output_path = out["metadata"]["outputPath"].as_str().unwrap();
    assert!(output_path.starts_with(dir.to_string_lossy().as_ref()));
    let saved = tokio::fs::read_to_string(output_path).await.unwrap();
    assert_eq!(saved, format!("{}\n", "a".repeat(20000)));

    let output = out["output"].as_str().unwrap();
    assert!(output.contains("Full output saved to:"));
    assert!(output.contains(output_path));
    assert!(output.len() < saved.len());
}

#[tokio::test]
async fn shell_requires_external_directory_permission_for_outside_workdir() {
    // Given
    let dir = tempdir();
    let outside = tempdir();
    let ctx = ctx_with(
        vec![
            allow(Action::Bash, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        dir,
    );
    let tool = ToolRegistry::builtins().get("shell").unwrap();

    // When
    let result = tool
        .execute(
            &ctx,
            json!({
                "command": "pwd",
                "description": "Print outside directory",
                "workdir": outside.to_string_lossy()
            }),
        )
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Permission(_))));
}
