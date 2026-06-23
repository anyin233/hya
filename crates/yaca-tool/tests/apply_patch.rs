#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
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

fn deny(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Deny)
}

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-apply-patch-test-{nanos}-{}",
        std::process::id()
    ));
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
        formatter: yaca_tool::FormatterPlane::default(),
        workdir,
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn apply_patch_adds_updates_moves_and_deletes_files() {
    let dir = tempdir();
    tokio::fs::create_dir_all(dir.join("src")).await.unwrap();
    tokio::fs::write(dir.join("src/app.txt"), "hello\nold\n")
        .await
        .unwrap();
    tokio::fs::write(dir.join("remove.txt"), "obsolete\n")
        .await
        .unwrap();

    let tool = ToolRegistry::builtins().get("apply_patch").unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());

    let patch = r#"*** Begin Patch
*** Add File: notes/todo.txt
+ship apply_patch
*** Update File: src/app.txt
@@
 hello
-old
+new
*** Update File: src/app.txt
*** Move to: src/main.txt
@@
 hello
 new
*** Delete File: remove.txt
*** End Patch
"#;
    let out = tool
        .execute(&ctx, json!({ "patchText": patch }))
        .await
        .unwrap();

    assert_eq!(
        tokio::fs::read_to_string(dir.join("notes/todo.txt"))
            .await
            .unwrap(),
        "ship apply_patch\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(dir.join("src/main.txt"))
            .await
            .unwrap(),
        "hello\nnew\n"
    );
    assert!(!dir.join("src/app.txt").exists());
    assert!(!dir.join("remove.txt").exists());
    assert!(out["output"].as_str().unwrap().contains("A notes/todo.txt"));
    assert!(out["output"].as_str().unwrap().contains("M src/main.txt"));
    assert!(out["output"].as_str().unwrap().contains("D remove.txt"));
    assert_eq!(out["title"], out["output"]);
    assert!(
        out["metadata"]["diff"]
            .as_str()
            .unwrap()
            .contains("ship apply_patch")
    );
    assert!(
        out["metadata"]["diff"]
            .as_str()
            .unwrap()
            .contains("obsolete")
    );

    let files = out["metadata"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 4);
    let add = files
        .iter()
        .find(|file| file["relativePath"] == "notes/todo.txt")
        .unwrap();
    assert_eq!(
        add["filePath"],
        dir.join("notes/todo.txt").to_string_lossy().as_ref()
    );
    assert_eq!(add["type"], "add");
    assert!(add["patch"].as_str().unwrap().contains("ship apply_patch"));

    let updated = files
        .iter()
        .find(|file| file["relativePath"] == "src/app.txt")
        .unwrap();
    assert_eq!(updated["type"], "update");
    assert!(updated["patch"].as_str().unwrap().contains("old"));

    let moved = files
        .iter()
        .find(|file| file["relativePath"] == "src/main.txt")
        .unwrap();
    assert_eq!(moved["type"], "move");
    assert_eq!(
        moved["movePath"],
        dir.join("src/main.txt").to_string_lossy().as_ref()
    );
    assert!(moved["patch"].is_string());

    let deleted = files
        .iter()
        .find(|file| file["relativePath"] == "remove.txt")
        .unwrap();
    assert_eq!(deleted["type"], "delete");
    assert!(deleted["patch"].as_str().unwrap().contains("obsolete"));
}

#[tokio::test]
async fn apply_patch_requires_edit_permission_before_writing() {
    let dir = tempdir();
    let tool = ToolRegistry::builtins().get("apply_patch").unwrap();
    let ctx = ctx_with(vec![deny(Action::Edit, "*")], dir.clone());

    let err = tool
        .execute(
            &ctx,
            json!({
                "patchText": "*** Begin Patch\n*** Add File: blocked.txt\n+nope\n*** End Patch\n"
            }),
        )
        .await;

    assert!(err.is_err());
    assert!(!dir.join("blocked.txt").exists());
}

#[tokio::test]
async fn apply_patch_rejects_missing_context_without_modifying_file() {
    let dir = tempdir();
    tokio::fs::write(dir.join("app.txt"), "actual\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("apply_patch").unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());

    let err = tool
        .execute(
            &ctx,
            json!({
                "patchText": "*** Begin Patch\n*** Update File: app.txt\n@@\n-missing\n+new\n*** End Patch\n"
            }),
        )
        .await;

    assert!(err.is_err());
    assert_eq!(
        tokio::fs::read_to_string(dir.join("app.txt"))
            .await
            .unwrap(),
        "actual\n"
    );
}
