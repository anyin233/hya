#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use yaca_proto::{ToolName, ToolSchema};
use yaca_tool::{
    Action, Decision, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules,
    QuestionAnswer, Resource, Rule, SkillPlane, SpawnerPlane, TodoPlane, Tool, ToolCtx,
    ToolRegistry, WebSearchPlane,
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
    let dir = std::env::temp_dir().join(format!("yaca-tool-test-{nanos}-{}", std::process::id()));
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

struct DuplicateTool;

#[async_trait]
impl Tool for DuplicateTool {
    fn name(&self) -> &str {
        "read"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("read"),
            description: "duplicate".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, yaca_tool::ToolError> {
        Ok(json!({ "ok": true }))
    }
}

#[test]
fn registry_rejects_duplicate_tool_name() {
    let mut registry = ToolRegistry::builtins();
    let result = registry.register(Arc::new(DuplicateTool));
    assert!(result.is_err());
    assert_eq!(
        registry.get("read").unwrap().schema().description,
        "Read a file or directory's contents."
    );
}

#[test]
fn builtins_expose_opencode_names_and_keep_short_aliases_hidden() {
    let registry = ToolRegistry::builtins();
    let visible: Vec<_> = registry
        .schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect();

    for canonical in ["bash", "shell"] {
        assert!(registry.get(canonical).is_some(), "{canonical} missing");
        assert!(
            visible.iter().any(|name| name == canonical),
            "{canonical} schema hidden"
        );
    }

    for (canonical, alias) in [
        ("webfetch", "fetch"),
        ("websearch", "search"),
        ("todowrite", "todo"),
        ("apply_patch", "patch"),
        ("plan_exit", "plan"),
    ] {
        assert!(registry.get(canonical).is_some(), "{canonical} missing");
        assert!(registry.get(alias).is_some(), "{alias} alias missing");
        assert!(
            visible.iter().any(|name| name == canonical),
            "{canonical} schema hidden"
        );
        assert!(
            visible.iter().all(|name| name != alias),
            "{alias} schema should be hidden"
        );
    }
}

#[test]
fn last_rule_wins_and_default_is_ask() {
    let rules = PermissionRules::new(vec![
        allow(Action::Read, "/**"),
        deny(Action::Read, "/etc/*"),
    ]);
    assert_eq!(
        rules.evaluate(Action::Read, &Resource::Path("/etc/passwd".into())),
        Mode::Deny
    );
    assert_eq!(
        rules.evaluate(Action::Read, &Resource::Path("/tmp/a".into())),
        Mode::Allow
    );
    assert_eq!(
        PermissionRules::default().evaluate(Action::Bash, &Resource::Command("ls".into())),
        Mode::Ask
    );
}

#[test]
fn child_deny_cannot_be_bypassed_by_parent_allow() {
    let parent = PermissionRules::new(vec![allow(Action::Read, "/**")]);
    let child = parent.derive_child(vec![deny(Action::Read, "/etc/*")]);
    assert_eq!(
        child.evaluate(Action::Read, &Resource::Path("/etc/shadow".into())),
        Mode::Deny
    );
    assert_eq!(
        child.evaluate(Action::Read, &Resource::Path("/home/x".into())),
        Mode::Allow
    );
}

#[tokio::test]
async fn assert_denies() {
    let (plane, _rx) = PermissionPlane::new(PermissionRules::new(vec![deny(Action::Bash, "*")]));
    let err = plane
        .assert(Action::Bash, Resource::Command("rm -rf /".into()))
        .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn ask_then_allow_once_then_reject() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let p1 = plane.clone();
    let h = tokio::spawn(async move {
        p1.assert(Action::Bash, Resource::Command("ls".into()))
            .await
    });
    let req = rx.recv().await.unwrap();
    req.reply.send(Decision::AllowOnce).unwrap();
    assert!(h.await.unwrap().is_ok());

    let p2 = plane.clone();
    let h2 = tokio::spawn(async move {
        p2.assert(Action::Bash, Resource::Command("ls".into()))
            .await
    });
    let req2 = rx.recv().await.unwrap();
    req2.reply
        .send(Decision::Reject { feedback: None })
        .unwrap();
    assert!(h2.await.unwrap().is_err());
}

#[tokio::test]
async fn read_happy_and_denied() {
    let dir = tempdir();
    let file = dir.join("hello.txt");
    tokio::fs::write(&file, "hi there").await.unwrap();
    let path = file.to_string_lossy().into_owned();

    let reg = ToolRegistry::builtins();
    let read = reg.get("read").unwrap();

    let ctx = ctx_with(vec![allow(Action::Read, "/**")], dir.clone());
    let out = read.execute(&ctx, json!({ "path": path })).await.unwrap();
    assert_eq!(out["content"], "hi there");

    let denied_ctx = ctx_with(vec![deny(Action::Read, "/**")], dir.clone());
    let denied = read
        .execute(&denied_ctx, json!({ "path": file.to_string_lossy() }))
        .await;
    assert!(denied.is_err());
}

#[tokio::test]
async fn write_then_glob_then_grep() {
    let dir = tempdir();
    let reg = ToolRegistry::builtins();
    let ctx = ctx_with(
        vec![
            allow(Action::Edit, "/**"),
            allow(Action::Glob, "*"),
            allow(Action::Grep, "*"),
        ],
        dir.clone(),
    );

    let write = reg.get("write").unwrap();
    let target = dir.join("src/a.rs");
    write
        .execute(
            &ctx,
            json!({ "path": target.to_string_lossy(), "content": "fn needle() {}\n" }),
        )
        .await
        .unwrap();
    assert!(target.exists());

    let glob = reg.get("glob").unwrap();
    let g = glob
        .execute(&ctx, json!({ "pattern": "*.rs" }))
        .await
        .unwrap();
    let paths = g["paths"].as_array().unwrap();
    assert!(paths.iter().any(|p| p.as_str() == Some("src/a.rs")));

    let grep = reg.get("grep").unwrap();
    let r = grep
        .execute(&ctx, json!({ "pattern": "needle" }))
        .await
        .unwrap();
    assert_eq!(r["matches"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn shell_happy_and_cancelled() {
    let dir = tempdir();
    let reg = ToolRegistry::builtins();
    let shell = reg.get("shell").unwrap();

    let ctx = ctx_with(vec![allow(Action::Bash, "*")], dir.clone());
    let out = shell
        .execute(&ctx, json!({ "command": "echo hi" }))
        .await
        .unwrap();
    assert_eq!(out["stdout"], "hi\n");
    assert_eq!(out["exit_code"], 0);

    let cancelled = ToolCtx {
        permission: ctx.permission.clone(),
        interaction: ctx.interaction.clone(),
        spawner: ctx.spawner.clone(),
        session: ctx.session,
        parent_session: None,
        todo: ctx.todo.clone(),
        skills: ctx.skills.clone(),
        websearch: ctx.websearch.clone(),
        lsp: ctx.lsp.clone(),
        workdir: dir,
        cancel: {
            let t = CancellationToken::new();
            t.cancel();
            t
        },
    };
    let err = shell
        .execute(&cancelled, json!({ "command": "echo hi" }))
        .await;
    assert!(matches!(err, Err(yaca_tool::ToolError::Cancelled)));
}

#[tokio::test]
async fn task_tool_is_lead_only() {
    let dir = tempdir();
    let (permission, _prx) =
        PermissionPlane::new(PermissionRules::new(vec![allow(Action::Task, "*")]));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    let ctx = ToolCtx {
        permission,
        interaction,
        spawner,
        session: None,
        parent_session: Some(yaca_proto::SessionId::new()),
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        workdir: dir,
        cancel: CancellationToken::new(),
    };
    let reg = ToolRegistry::builtins();
    let tool = reg.get("task").unwrap();
    let err = tool
        .execute(&ctx, json!({ "prompt": "x", "subagent_type": "quick" }))
        .await;
    assert!(matches!(err, Err(yaca_tool::ToolError::Other(_))));
}

#[tokio::test]
async fn ask_user_select_returns_index_and_answer() {
    let dir = tempdir();
    let (permission, _prx) = PermissionPlane::new(PermissionRules::default());
    let (interaction, mut irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    let ctx = ToolCtx {
        permission,
        interaction,
        spawner,
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        workdir: dir,
        cancel: CancellationToken::new(),
    };
    let reg = ToolRegistry::builtins();
    let tool = reg.get("ask_user").unwrap();
    let handle = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({ "question": "pick", "kind": "select", "options": ["red", "green"] }),
        )
        .await
    });
    let req = irx.recv().await.unwrap();
    req.reply.send(QuestionAnswer::Selected(1)).unwrap();
    let out = handle.await.unwrap().unwrap();
    assert_eq!(out["answer"], "green");
    assert_eq!(out["selected_index"], 1);
}

#[tokio::test]
async fn ls_lists_entries_with_type_and_size() {
    let dir = tempdir();
    tokio::fs::write(dir.join("a.txt"), "hello").await.unwrap();
    tokio::fs::create_dir_all(dir.join("sub")).await.unwrap();
    let reg = ToolRegistry::builtins();
    let ls = reg.get("ls").unwrap();

    let ctx = ctx_with(vec![allow(Action::Read, "/**")], dir.clone());
    let out = ls
        .execute(&ctx, json!({ "path": dir.to_string_lossy() }))
        .await
        .unwrap();
    let entries = out["entries"].as_array().unwrap();
    let a = entries.iter().find(|e| e["name"] == "a.txt").unwrap();
    assert_eq!(a["type"], "file");
    assert_eq!(a["size"], 5);
    let sub = entries.iter().find(|e| e["name"] == "sub").unwrap();
    assert_eq!(sub["type"], "dir");

    let out2 = ls.execute(&ctx, json!({})).await.unwrap();
    assert!(
        out2["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == "a.txt")
    );

    let denied = ctx_with(vec![deny(Action::Read, "/**")], dir.clone());
    assert!(
        ls.execute(&denied, json!({ "path": dir.to_string_lossy() }))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn find_matches_glob_with_metadata() {
    let dir = tempdir();
    tokio::fs::create_dir_all(dir.join("src")).await.unwrap();
    tokio::fs::write(dir.join("src/a.rs"), "fn a(){}")
        .await
        .unwrap();
    tokio::fs::write(dir.join("b.txt"), "x").await.unwrap();
    let reg = ToolRegistry::builtins();
    let find = reg.get("find").unwrap();

    let glob_ctx = ctx_with(vec![allow(Action::Glob, "*")], dir.clone());
    let out = find
        .execute(
            &glob_ctx,
            json!({ "pattern": "*.rs", "path": dir.to_string_lossy() }),
        )
        .await
        .unwrap();
    let results = out["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0]["path"].as_str().unwrap().ends_with("a.rs"));
    assert_eq!(results[0]["size"], 8);

    let denied = ctx_with(vec![deny(Action::Glob, "*")], dir.clone());
    assert!(
        find.execute(&denied, json!({ "pattern": "*.rs" }))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn edit_guards_ambiguous_unless_replace_all() {
    let dir = tempdir();
    let f = dir.join("x.txt");
    tokio::fs::write(&f, "a\na\n").await.unwrap();
    let reg = ToolRegistry::builtins();
    let edit = reg.get("edit").unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "/**")], dir.clone());

    let amb = edit
        .execute(
            &ctx,
            json!({ "path": f.to_string_lossy(), "old": "a", "new": "b" }),
        )
        .await;
    assert!(amb.is_err());
    assert_eq!(tokio::fs::read_to_string(&f).await.unwrap(), "a\na\n");

    let ok = edit
        .execute(
            &ctx,
            json!({ "path": f.to_string_lossy(), "old": "a", "new": "b", "replace_all": true }),
        )
        .await
        .unwrap();
    assert_eq!(ok["replaced"], 2);
    assert_eq!(tokio::fs::read_to_string(&f).await.unwrap(), "b\nb\n");

    tokio::fs::write(&f, "one two\n").await.unwrap();
    let uniq = edit
        .execute(
            &ctx,
            json!({ "path": f.to_string_lossy(), "old": "two", "new": "three" }),
        )
        .await
        .unwrap();
    assert_eq!(uniq["replaced"], 1);
    assert_eq!(tokio::fs::read_to_string(&f).await.unwrap(), "one three\n");
}

#[tokio::test]
async fn edit_accepts_open_code_parameters_and_creates_new_file_from_empty_old_string() {
    let dir = tempdir();
    let file = dir.join("created.txt");
    let ctx = ctx_with(vec![allow(Action::Edit, "/**")], dir.clone());
    let edit = ToolRegistry::builtins().get("edit").unwrap();

    let out = edit
        .execute(
            &ctx,
            json!({
                "filePath": "created.txt",
                "oldString": "",
                "newString": "hello\n"
            }),
        )
        .await
        .unwrap();
    assert_eq!(out["created"], true);
    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "hello\n");

    let same = edit
        .execute(
            &ctx,
            json!({
                "filePath": "created.txt",
                "oldString": "hello\n",
                "newString": "hello\n"
            }),
        )
        .await;
    assert!(
        matches!(same, Err(yaca_tool::ToolError::Other(message)) if message == "No changes to apply: oldString and newString are identical.")
    );
}

#[tokio::test]
async fn dropped_reply_yields_unavailable() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let p = plane.clone();
    let h =
        tokio::spawn(async move { p.assert(Action::Bash, Resource::Command("ls".into())).await });
    let req = rx.recv().await.unwrap();
    drop(req);
    assert!(matches!(
        h.await.unwrap(),
        Err(yaca_tool::PermissionError::Unavailable)
    ));
}
