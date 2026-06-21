#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio_util::sync::CancellationToken;
use yaca_tool::{
    Action, Decision, Mode, PermissionPlane, PermissionRules, Resource, Rule, ToolCtx, ToolRegistry,
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
    ToolCtx {
        permission,
        workdir,
        cancel: CancellationToken::new(),
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
            allow(Action::Grep, "/**"),
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
