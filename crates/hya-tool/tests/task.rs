//! Integration tests for `hya-tool`: task.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_proto::{OperationId, SessionId, ToolCallId};
use hya_tool::{
    Action, AgentDef, InteractionPlane, LspPlane, MemberOutcome, Mode, PermissionPlane,
    PermissionRules, Rule, SkillPlane, SpawnMember, SpawnerPlane, TodoPlane, ToolCtx, ToolError,
    ToolOperation, ToolRegistry, WebSearchPlane, handle::ArtifactPlane,
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
    let operation = ToolOperation::from_tool_call(ToolCallId::new());
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner: spawner.for_session(session),
        operation,
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: std::sync::Arc::<[AgentDef]>::from([]),
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
        .send(Ok(vec![MemberOutcome {
            member: "mbr_n".to_string(),
            session: "ses_grandchild".to_string(),
            status: "done".to_string(),
            summary: "nested done".to_string(),
        }]))
        .unwrap();
    let out = handle.await.unwrap().unwrap();
    assert_eq!(out["metadata"]["sessionId"], "ses_grandchild");
}

#[tokio::test]
async fn omitted_subagent_type_selects_general() {
    let parent = SessionId::new();
    let (spawner, mut rx) = SpawnerPlane::new();
    let ctx = ctx_with_session(vec![allow(Action::Task, "general")], spawner, parent);
    let tool = ToolRegistry::builtins().get("task").unwrap();

    let mut handle = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({
                "description": "Use default agent",
                "prompt": "handle this task"
            }),
        )
        .await
    });

    let req = tokio::select! {
        result = &mut handle => panic!("omitted target rejected before spawn: {result:?}"),
        req = rx.recv() => req.expect("spawn request"),
    };
    assert_eq!(req.members[0].subagent_type, "general");
    req.reply
        .send(Ok(vec![MemberOutcome {
            member: "mbr_1".to_string(),
            session: "ses_general".to_string(),
            status: "done".to_string(),
            summary: "done".to_string(),
        }]))
        .unwrap();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn task_preserves_the_persisted_tool_call_operation_identity() {
    let parent = SessionId::new();
    let source_tool_call_id = ToolCallId::new();
    let operation = ToolOperation::from_tool_call(source_tool_call_id);
    let (spawner, mut rx) = SpawnerPlane::new();
    let mut ctx = ctx_with_session(vec![allow(Action::Task, "explore")], spawner, parent);
    ctx.operation = operation;
    let tool = ToolRegistry::builtins().get("task").unwrap();

    let handle = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({
                "description": "Preserve identity",
                "prompt": "keep the durable operation identity",
                "subagent_type": "explore"
            }),
        )
        .await
    });

    let req = rx.recv().await.unwrap();
    assert_eq!(req.operation.source_tool_call_id(), source_tool_call_id);
    assert_eq!(
        req.operation.operation_id(),
        OperationId::from_tool_call(source_tool_call_id)
    );
    req.reply.send(Ok(Vec::new())).unwrap();
    let _ = handle.await.unwrap();
}

#[test]
fn task_schema_exposes_open_code_fields() {
    let tool = ToolRegistry::builtins().get("task").unwrap();
    let schema = tool.schema().input_schema;

    assert_eq!(schema["required"], json!(["description", "prompt"]));
    let props = &schema["properties"];
    assert_eq!(props["description"]["type"], "string");
    assert_eq!(props["prompt"]["type"], "string");
    assert_eq!(props["subagent_type"]["type"], "string");
    assert_eq!(props["command"]["type"], "string");
    assert!(
        props.get("task_id").is_none(),
        "task_id is gone: mail revival supersedes resume"
    );
    assert!(
        props.get("background").is_none(),
        "background is gone: every spawn is non-blocking"
    );
    assert!(
        props.get("resident").is_none(),
        "resident is gone: every agent is resident"
    );
}

#[test]
fn task_schema_describes_inline_agent_as_request_scoped() {
    let tool = ToolRegistry::builtins().get("task").unwrap();
    let schema = tool.schema().input_schema;
    let props = &schema["properties"];

    let single = props["inline_agent"]["description"]
        .as_str()
        .expect("single-task inline_agent schema description");
    let batch = props["members"]["items"]["properties"]["inline_agent"]["description"]
        .as_str()
        .expect("batch members inline_agent schema description");

    for (label, description) in [("single", single), ("batch", batch)] {
        let lower = description.to_ascii_lowercase();
        assert!(
            lower.contains("request-scoped") || lower.contains("request scoped"),
            "{label} inline_agent description must state request-scoped semantics: {description:?}"
        );
        assert!(
            !lower.contains(".md")
                && !lower.contains("persist")
                && !lower.contains("disk")
                && !lower.contains("reuse it later")
                && !lower.contains("save an"),
            "{label} inline_agent description must not mention .md/disk persistence or legacy file reuse: {description:?}"
        );
    }
}
#[test]
fn task_schema_hides_unsupported_inline_description() {
    let tool = ToolRegistry::builtins().get("task").unwrap();
    let schema = tool.schema().input_schema;
    let props = &schema["properties"];

    let single_properties = props["inline_agent"]["properties"]
        .as_object()
        .expect("single-task inline_agent schema properties");
    let member_properties = props["members"]["items"]["properties"]["inline_agent"]["properties"]
        .as_object()
        .expect("batch members inline_agent schema properties");

    assert!(
        !single_properties.contains_key("description"),
        "single-task inline_agent must not advertise description"
    );
    assert!(
        !member_properties.contains_key("description"),
        "batch members inline_agent must not advertise description"
    );
}

#[tokio::test]
async fn task_normalizes_empty_inline_description() {
    let parent = SessionId::new();
    let (spawner, mut rx) = SpawnerPlane::new();
    let ctx = ctx_with_session(vec![allow(Action::Task, "general")], spawner, parent);
    let tool = ToolRegistry::builtins().get("task").unwrap();

    let handle = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({
                "description": "Captured inline task",
                "prompt": "Run the captured task",
                "subagent_type": "general",
                "inline_agent": {
                    "description": "",
                    "category": "",
                    "model": "",
                    "name": "",
                    "prompt": ""
                }
            }),
        )
        .await
    });

    let req = rx.recv().await.expect("spawn request");
    let inline = req.members[0]
        .inline_agent
        .as_ref()
        .expect("captured inline agent");
    assert!(
        inline.description.is_none(),
        "empty inline description must be absent from SpawnRequest"
    );
    req.reply
        .send(Ok(vec![MemberOutcome {
            member: "mbr_1".to_string(),
            session: "ses_child".to_string(),
            status: "done".to_string(),
            summary: "done".to_string(),
        }]))
        .unwrap();
    handle.await.unwrap().unwrap();
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
        .send(Ok(vec![MemberOutcome {
            member: "mbr_1".to_string(),
            session: "ses_child".to_string(),
            status: "done".to_string(),
            summary: "routing summary".to_string(),
        }]))
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
async fn task_preserves_typed_spawn_overload() {
    let parent = SessionId::new();
    let (spawner, rx) = SpawnerPlane::new();
    let queued_plane = spawner.for_session(parent);
    let queued = tokio::spawn(async move {
        queued_plane
            .spawn(
                ToolOperation::from_tool_call(ToolCallId::new()),
                vec![SpawnMember::default()],
                CancellationToken::new(),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while rx.len() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first request was not queued");

    let ctx = ctx_with_session(vec![allow(Action::Task, "explore")], spawner, parent);
    let tool = ToolRegistry::builtins().get("task").unwrap();
    let error = tool
        .execute(
            &ctx,
            json!({
                "description": "Overloaded spawn",
                "prompt": "must not queue",
                "subagent_type": "explore"
            }),
        )
        .await
        .unwrap_err();

    queued.abort();
    assert!(matches!(error, ToolError::Overloaded(message) if message.contains("overloaded")));
    assert_eq!(rx.len(), 1, "overloaded task must not enter the queue");
}

#[tokio::test]
async fn task_returns_immediately_with_running_handles() {
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
                "subagent_type": "explore"
            }),
        )
        .await
    });

    let req = rx.recv().await.unwrap();
    req.reply
        .send(Ok(vec![MemberOutcome {
            member: "main/explore-1".to_string(),
            session: child.clone(),
            status: "running".to_string(),
            summary: "Resident main/explore-1 is live; results arrive as its report.".to_string(),
        }]))
        .unwrap();

    let out = handle.await.unwrap().unwrap();
    assert_eq!(out["title"], "Inspect routing");
    assert_eq!(out["metadata"]["sessionId"], child);
    assert_eq!(out["metadata"]["status"], "running");
    assert!(
        out["output"].to_string().contains("running"),
        "single-member output stays a task block: {}",
        out["output"]
    );
}
