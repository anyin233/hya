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
    let dir = std::env::temp_dir().join(format!(
        "yaca-glob-grep-{nanos}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with(workdir: PathBuf) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        allow(Action::Glob, "*"),
        allow(Action::Grep, "*"),
        allow(Action::ExternalDirectory, "*"),
    ]));
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

fn ctx_with_rules(workdir: PathBuf, rules: Vec<Rule>) -> ToolCtx {
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
        formatter: yaca_tool::FormatterPlane::default(),
        workdir,
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn glob_supports_path_and_open_code_output_shape() {
    let workdir = tempdir();
    let src = workdir.join("src");
    tokio::fs::create_dir_all(&src).await.unwrap();
    tokio::fs::write(src.join("main.rs"), "fn main() {}\n")
        .await
        .unwrap();
    tokio::fs::write(src.join("readme.md"), "# docs\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("glob").unwrap();
    let ctx = ctx_with(workdir.clone());

    let out = tool
        .execute(&ctx, json!({ "pattern": "*.rs", "path": "src" }))
        .await
        .unwrap();

    let file = src.join("main.rs").to_string_lossy().replace('\\', "/");
    assert_eq!(out["title"], "src");
    assert_eq!(out["metadata"]["count"], 1);
    assert_eq!(out["metadata"]["truncated"], false);
    assert_eq!(out["output"], file);
}

#[tokio::test]
async fn glob_rejects_file_path_like_opencode() {
    let workdir = tempdir();
    let src = workdir.join("src");
    tokio::fs::create_dir_all(&src).await.unwrap();
    tokio::fs::write(src.join("main.rs"), "fn main() {}\n")
        .await
        .unwrap();
    tokio::fs::write(src.join("lib.rs"), "pub fn lib() {}\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("glob").unwrap();
    let ctx = ctx_with(workdir.clone());

    let err = tool
        .execute(&ctx, json!({ "pattern": "*.rs", "path": "src/main.rs" }))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("glob path must be a directory"));
}

#[tokio::test]
async fn grep_supports_regex_include_and_open_code_output_shape() {
    let workdir = tempdir();
    let src = workdir.join("src");
    tokio::fs::create_dir_all(&src).await.unwrap();
    tokio::fs::write(src.join("main.rs"), "fn main() {}\nlet x = 1;\n")
        .await
        .unwrap();
    tokio::fs::write(src.join("notes.txt"), "fn main text file\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir.clone());

    let out = tool
        .execute(
            &ctx,
            json!({ "pattern": "fn\\s+main", "path": "src", "include": "*.rs" }),
        )
        .await
        .unwrap();

    let file = src.join("main.rs").to_string_lossy().replace('\\', "/");
    assert_eq!(out["title"], "fn\\s+main");
    assert_eq!(out["metadata"]["matches"], 1);
    assert_eq!(out["metadata"]["truncated"], false);
    let output = out["output"].as_str().unwrap();
    assert!(output.starts_with("Found 1 matches"));
    assert!(output.contains(&format!("{file}:")));
    assert!(output.contains("  Line 1: fn main() {}"));
    assert!(!output.contains("notes.txt"));
}

#[tokio::test]
async fn grep_uses_parent_directory_when_path_is_a_file() {
    let workdir = tempdir();
    let src = workdir.join("src");
    tokio::fs::create_dir_all(&src).await.unwrap();
    tokio::fs::write(src.join("main.rs"), "needle in main\n")
        .await
        .unwrap();
    tokio::fs::write(src.join("lib.rs"), "needle in lib\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let out = tool
        .execute(&ctx, json!({ "pattern": "needle", "path": "src/main.rs" }))
        .await
        .unwrap();

    let output = out["output"].as_str().unwrap();
    assert_eq!(out["metadata"]["matches"], 2);
    assert!(output.contains("main.rs"));
    assert!(output.contains("lib.rs"));
}

#[tokio::test]
async fn glob_requires_external_directory_permission_for_outside_path() {
    let workdir = tempdir();
    let outside = tempdir();
    tokio::fs::write(outside.join("main.rs"), "fn main() {}\n")
        .await
        .unwrap();
    let ctx = ctx_with_rules(
        workdir,
        vec![
            allow(Action::Glob, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
    );
    let tool = ToolRegistry::builtins().get("glob").unwrap();

    let err = tool
        .execute(
            &ctx,
            json!({ "pattern": "*.rs", "path": outside.to_string_lossy() }),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, ToolError::Permission(_)));
}
