#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_proto::SessionId;
use hya_tool::{
    Action, InteractionPlane, LspPlane, MemberOutcome, Mode, PermissionPlane, PermissionRules,
    Rule, SkillPlane, SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry, WebSearchPlane,
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
    let dir = std::env::temp_dir().join(format!("hya-task-{nanos}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with_session(rules: Vec<Rule>, spawner: SpawnerPlane, session: SessionId) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    ToolCtx {
        permission,
        interaction,
        spawner: spawner.for_session(session),
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: hya_tool::AgentCatalogPlane::default(),
        workdir: tempdir(),
        cancel: CancellationToken::new(),
    }
}

fn ctx_with_parent(
    rules: Vec<Rule>,
    spawner: SpawnerPlane,
    session: SessionId,
    parent: SessionId,
) -> ToolCtx {
    let mut ctx = ctx_with_session(rules, spawner, session);
    ctx.parent_session = Some(parent);
    ctx
}

#[tokio::test]
async fn subagent_can_spawn_nested_task() {
    // A session WITH a parent (i.e. itself a subagent) must be allowed to call the
    // task tool: nesting is bounded by the governor, not blocked outright.
    let parent = SessionId::new();
    let child = SessionId::new();
    let (spawner, mut rx) = SpawnerPlane::new();
    let ctx = ctx_with_parent(vec![allow(Action::Task, "explore")], spawner, child, parent);
    let tool = ToolRegistry::builtins().get("task").unwrap();

    let handle = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({
                "description": "Nested probe",
                "prompt": "dig deeper",
                "subagent_type": "explore"
            }),
        )
        .await
    });

    // The request reaches the spawner instead of erroring with a lead-only guard.
    let req = rx.recv().await.unwrap();
    assert_eq!(
        req.parent, child,
        "nested spawn is parented at the subagent"
    );
    req.reply
        .send(vec![MemberOutcome {
            member: "mbr_n".to_string(),
            session: "ses_grandchild".to_string(),
            status: "done".to_string(),
            summary: "nested done".to_string(),
        }])
        .unwrap();
    let out = handle.await.unwrap().unwrap();
    assert_eq!(out["metadata"]["sessionId"], "ses_grandchild");
}

#[test]
fn task_schema_exposes_open_code_fields() {
    let tool = ToolRegistry::builtins().get("task").unwrap();
    let schema = tool.schema().input_schema;

    assert_eq!(
        schema["required"],
        json!(["description", "prompt", "subagent_type"])
    );
    let props = &schema["properties"];
    assert_eq!(props["description"]["type"], "string");
    assert_eq!(props["prompt"]["type"], "string");
    assert_eq!(props["subagent_type"]["type"], "string");
    assert_eq!(props["task_id"]["type"], "string");
    assert_eq!(props["command"]["type"], "string");
    assert_eq!(props["background"]["type"], "boolean");
}

#[tokio::test]
async fn task_foreground_result_uses_open_code_output_shape() {
    let parent = SessionId::new();
    let (spawner, mut rx) = SpawnerPlane::new();
    let ctx = ctx_with_session(vec![allow(Action::Task, "explore")], spawner, parent);
    let tool = ToolRegistry::builtins().get("task").unwrap();

    let handle = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({
                "description": "Inspect routing",
                "prompt": "Find the routing entry points",
                "subagent_type": "explore"
            }),
        )
        .await
    });

    let req = rx.recv().await.unwrap();
    assert_eq!(req.parent, parent);
    assert_eq!(req.members.len(), 1);
    assert_eq!(req.members[0].description, "Inspect routing");
    assert_eq!(req.members[0].prompt, "Find the routing entry points");
    assert_eq!(req.members[0].subagent_type, "explore");
    req.reply
        .send(vec![MemberOutcome {
            member: "mbr_1".to_string(),
            session: "ses_child".to_string(),
            status: "done".to_string(),
            summary: "routing summary".to_string(),
        }])
        .unwrap();

    let out = handle.await.unwrap().unwrap();
    assert_eq!(out["title"], "Inspect routing");
    assert_eq!(out["metadata"]["parentSessionId"], parent.to_string());
    assert_eq!(out["metadata"]["sessionId"], "ses_child");
    assert_eq!(out["metadata"]["subagent_type"], "explore");
    assert_eq!(out["metadata"]["status"], "done");
    assert_eq!(
        out["output"],
        "<task id=\"ses_child\" state=\"completed\">\n<task_result>\nrouting summary\n</task_result>\n</task>"
    );
}

#[tokio::test]
async fn task_forwards_task_id_to_spawner_for_resume() {
    let parent = SessionId::new();
    let child = SessionId::new().to_string();
    let child_for_input = child.clone();
    let (spawner, mut rx) = SpawnerPlane::new();
    let ctx = ctx_with_session(vec![allow(Action::Task, "explore")], spawner, parent);
    let tool = ToolRegistry::builtins().get("task").unwrap();

    let handle = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({
                "description": "Continue routing",
                "prompt": "Use the prior findings",
                "subagent_type": "explore",
                "task_id": child_for_input
            }),
        )
        .await
    });

    let req = rx.recv().await.unwrap();
    assert_eq!(req.members[0].task_id.as_deref(), Some(child.as_str()));
    req.reply
        .send(vec![MemberOutcome {
            member: "mbr_1".to_string(),
            session: child.clone(),
            status: "done".to_string(),
            summary: "continued".to_string(),
        }])
        .unwrap();

    let out = handle.await.unwrap().unwrap();
    assert_eq!(out["metadata"]["sessionId"], child);
    assert_eq!(
        out["output"],
        format!(
            "<task id=\"{child}\" state=\"completed\">\n<task_result>\ncontinued\n</task_result>\n</task>"
        )
    );
}

#[tokio::test]
async fn task_rejects_invalid_task_id() {
    let parent = SessionId::new();
    let (spawner, _rx) = SpawnerPlane::new();
    let ctx = ctx_with_session(vec![allow(Action::Task, "explore")], spawner, parent);
    let tool = ToolRegistry::builtins().get("task").unwrap();

    let err = tool
        .execute(
            &ctx,
            json!({
                "description": "Continue routing",
                "prompt": "Use the prior findings",
                "subagent_type": "explore",
                "task_id": "not-a-session-id"
            }),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, ToolError::Input(message) if message.contains("invalid task_id")));
}

#[tokio::test]
async fn task_background_returns_running_task_result() {
    let parent = SessionId::new();
    let child = SessionId::new().to_string();
    let (spawner, mut rx) = SpawnerPlane::new();
    let ctx = ctx_with_session(vec![allow(Action::Task, "explore")], spawner, parent);
    let tool = ToolRegistry::builtins().get("task").unwrap();

    let handle = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({
                "description": "Inspect routing",
                "prompt": "Find the routing entry points",
                "subagent_type": "explore",
                "background": true
            }),
        )
        .await
    });

    let req = rx.recv().await.unwrap();
    assert!(req.background);
    req.reply
        .send(vec![MemberOutcome {
            member: "mbr_1".to_string(),
            session: child.clone(),
            status: "running".to_string(),
            summary: "The task is working in the background.".to_string(),
        }])
        .unwrap();

    let out = handle.await.unwrap().unwrap();
    assert_eq!(out["title"], "Inspect routing");
    assert_eq!(out["metadata"]["background"], true);
    assert_eq!(out["metadata"]["jobId"], child);
    assert_eq!(out["metadata"]["sessionId"], child);
    assert_eq!(
        out["output"],
        format!(
            "<task id=\"{child}\" state=\"running\">\n<summary>Background task started</summary>\n<task_result>\nThe task is working in the background.\n</task_result>\n</task>"
        )
    );
}
