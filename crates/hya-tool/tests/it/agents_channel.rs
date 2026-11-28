//! Agent tool contracts.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use hya_proto::{OperationId, SessionId, ToolCallId};
use hya_tool::{Action, MemberOutcome, SpawnMember, SpawnerPlane, ToolError, ToolOperation};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::common::{allow, ctx_with};

#[test]
fn task_schema_contracts() {
    let tool = hya_tool_loader::builtins().get("task").unwrap();
    let schema = tool.schema();
    let props = &schema.input_schema["properties"];
    assert_eq!(schema.input_schema["required"], json!([]));
    assert_eq!(props["tasks"]["minItems"], 1);
    assert!(props.get("members").is_none());
    assert!(props.get("name").is_none());
    assert!(props["tasks"]["items"]["properties"].get("name").is_none());
    for gone in ["task_id", "background", "resident"] {
        assert!(props.get(gone).is_none(), "{gone} removed");
    }
    assert!(schema.description.contains("<subagent_type>-<operator>"));
    assert!(
        props["subagent_type"]["description"]
            .as_str()
            .unwrap()
            .contains("hya-task")
    );

    for desc in [
        props["inline_agent"]["description"].as_str().unwrap(),
        props["tasks"]["items"]["properties"]["inline_agent"]["description"]
            .as_str()
            .unwrap(),
    ] {
        let lower = desc.to_ascii_lowercase();
        assert!(lower.contains("request-scoped") || lower.contains("request scoped"));
        assert!(!lower.contains(".md") && !lower.contains("persist"));
    }
    assert!(
        !props["inline_agent"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("description")
    );
    assert!(
        !props["tasks"]["items"]["properties"]["inline_agent"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("description")
    );
}

#[tokio::test]
async fn task_single_nested_defaults_effort_and_identity() {
    let tool = hya_tool_loader::builtins().get("task").unwrap();
    let parent = SessionId::new();
    let child = SessionId::new();
    let source_call = ToolCallId::new();

    // Missing session errors cleanly.
    let no_sess = ctx_with(vec![allow(Action::Task, "*")], std::env::temp_dir())
        .with_extension(hya_tool::ParentSession(parent));
    assert!(matches!(
        tool.execute(&no_sess, json!({ "prompt": "x", "subagent_type": "quick" }))
            .await,
        Err(ToolError::Other(_))
    ));

    // Subagent nested spawn + operation identity + effort + inline empty description normalization.
    let (spawner, mut rx) = SpawnerPlane::new();
    let mut ctx = ctx_with(vec![allow(Action::Task, "*")], std::env::temp_dir())
        .with_extension(hya_tool::ParentSession(parent))
        .with_extension(spawner.for_session(child));
    ctx.session = Some(child);
    ctx.operation = ToolOperation::from_tool_call(source_call);

    let t = Arc::clone(&tool);
    let c = ctx.clone();
    let h = tokio::spawn(async move {
        t.execute(
            &c,
            json!({
                "description": "Deep dive",
                "prompt": "Find routing",
                "effort": "high",
                "inline_agent": { "description": "", "category": "", "model": "", "name": "", "prompt": "" }
            }),
        )
        .await
    });
    let req = rx.recv().await.unwrap();
    assert_eq!(req.parent, child);
    assert_eq!(req.operation.source_tool_call_id(), source_call);
    assert_eq!(
        req.operation.operation_id(),
        OperationId::from_tool_call(source_call)
    );
    assert_eq!(req.members[0].subagent_type, "hya-task");
    assert_eq!(req.members[0].effort.as_deref(), Some("high"));
    assert!(
        req.members[0]
            .inline_agent
            .as_ref()
            .unwrap()
            .description
            .is_none()
    );
    req.reply
        .send(Ok(vec![MemberOutcome {
            member: "main/explore-1".into(),
            session: "ses_child".into(),
            status: "running".into(),
            summary: "live".into(),
            model: Some("fake/model#high".into()),
        }]))
        .unwrap();
    let out = h.await.unwrap().unwrap();
    assert_eq!(out["metadata"]["model"], "fake/model#high");
    assert_eq!(out["metadata"]["status"], "running");
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .starts_with("<task id=\"ses_child\" model=\"fake/model#high\" state=\"running\">")
    );

    // Foreground done shape.
    let t = Arc::clone(&tool);
    let h = tokio::spawn(async move {
        t.execute(
            &ctx,
            json!({ "description": "Inspect", "prompt": "Check", "subagent_type": "explore" }),
        )
        .await
    });
    let req = rx.recv().await.unwrap();
    req.reply
        .send(Ok(vec![MemberOutcome {
            member: "mbr_1".into(),
            session: "ses_done".into(),
            status: "done".into(),
            summary: "summary".into(),
            model: None,
        }]))
        .unwrap();
    let out = h.await.unwrap().unwrap();
    assert_eq!(
        out["output"],
        "<task id=\"ses_done\" state=\"completed\">\n<task_result>\nsummary\n</task_result>\n</task>"
    );
}

#[tokio::test]
async fn task_batch_validation_removed_name_and_overload() {
    let tool = hya_tool_loader::builtins().get("task").unwrap();
    let session = SessionId::new();

    // Removed `name` parameter and invalid batch inputs are rejected before dispatch.
    let (spawner, mut rx) = SpawnerPlane::new();
    let mut ctx = ctx_with(vec![allow(Action::Task, "*")], std::env::temp_dir())
        .with_extension(spawner.for_session(session));
    ctx.session = Some(session);
    for input in [
        json!({"description": "d", "prompt": "p", "name": "scout"}),
        json!({"members": [{"prompt": "scan", "subagent_type": "explore"}, {"prompt": "b", "subagent_type": "general", "name": "dev"}]}),
    ] {
        let ToolError::Input(msg) = tool.execute(&ctx, input).await.unwrap_err() else {
            panic!("expected Input error");
        };
        assert!(msg.contains("`name` was removed") && msg.contains("subagent_type"));
    }
    for invalid in [
        json!({"tasks": []}),
        json!({"tasks": null, "description": "d", "prompt": "p"}),
        json!({"tasks": [{"prompt": "ok"}, {"prompt": "  "}]}),
        json!({"tasks": [{"prompt": "ok"}], "members": [{"prompt": "other"}]}),
    ] {
        assert!(matches!(
            tool.execute(&ctx, invalid).await,
            Err(ToolError::Input(_))
        ));
    }
    assert!(rx.try_recv().is_err());

    // Batch (`tasks` and `members` alias) + 1-item batch preserve order, effort, and shape.
    for field in ["tasks", "members"] {
        let mut input = json!({"context": "Shared contract", "model": "ignored"});
        input[field] = json!([
            {"description": "Inspect", "prompt": "Inspect files", "effort": "low"},
            {"description": "Review", "prompt": "Review tests", "subagent_type": "reviewer"}
        ]);
        let t = Arc::clone(&tool);
        let c = ctx.clone();
        let h = tokio::spawn(async move { t.execute(&c, input).await });
        let req = rx.recv().await.unwrap();
        assert_eq!(req.members[0].prompt, "Shared contract\n\nInspect files");
        assert_eq!(req.members[0].subagent_type, "hya-task");
        assert_eq!(req.members[0].effort.as_deref(), Some("low"));
        assert_eq!(req.members[1].effort, None);
        assert!(req.members.iter().all(|m| m.model.is_none()));
        req.reply
            .send(Ok(vec![
                MemberOutcome {
                    member: "main/a".into(),
                    session: "sa".into(),
                    status: "running".into(),
                    summary: "live".into(),
                    model: None,
                },
                MemberOutcome {
                    member: "-".into(),
                    session: "-".into(),
                    status: "failed".into(),
                    summary: "boom".into(),
                    model: None,
                },
            ]))
            .unwrap();
        let res = h.await.unwrap().unwrap();
        assert_eq!(res["metadata"]["status"], "running");
        assert!(
            res["output"]
                .as_str()
                .unwrap()
                .contains("1 running, 1 failed")
        );
    }

    // 1-item batch keeps batch shape.
    let t = Arc::clone(&tool);
    let c = ctx.clone();
    let h =
        tokio::spawn(async move { t.execute(&c, json!({"tasks": [{"prompt": "work"}]})).await });
    let req = rx.recv().await.unwrap();
    req.reply
        .send(Ok(vec![MemberOutcome {
            member: "main/a".into(),
            session: "a".into(),
            status: "running".into(),
            summary: "live".into(),
            model: None,
        }]))
        .unwrap();
    assert_eq!(
        h.await.unwrap().unwrap()["metadata"]["members"][0]["member"],
        "main/a"
    );

    // Typed spawn overload when queue is full.
    let (full_spawner, full_rx) = SpawnerPlane::new();
    let qp = full_spawner.for_session(session);
    let queued = tokio::spawn(async move {
        qp.spawn(
            ToolOperation::from_tool_call(ToolCallId::new()),
            vec![SpawnMember::default()],
            CancellationToken::new(),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while full_rx.len() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    ctx.extensions.insert(full_spawner.for_session(session));
    let err = tool
        .execute(
            &ctx,
            json!({"description": "Over", "prompt": "p", "subagent_type": "explore"}),
        )
        .await
        .unwrap_err();
    queued.abort();
    assert!(matches!(err, ToolError::Overloaded(m) if m.contains("overloaded")));
}
