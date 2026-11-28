//! Agent, channel, interaction, plan, and workflow tool contracts:
//! `ask_user`/`question`, `plan_exit`, `send`, `list_channel`, `workflow`, and `task`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_proto::{
    ChannelKind, MailEndpoint, MailKind, OperationId, SessionId, ToolCallId, WorkflowRevision,
};
use hya_tool::handle::{DynamicUriContent, DynamicUriCtx, DynamicUriHandler, HandleError};
use hya_tool::{
    Action, ChannelRow, InteractionPlane, MailReceipt, MailboxPlane, MailboxRequest, MemberOutcome,
    MemberStatusRow, QuestionAnswer, QuestionKind, SpawnMember, SpawnerPlane, ToolError,
    ToolOperation, WorkflowCommand, WorkflowCommandResult, WorkflowHostError, WorkflowPlane,
    WorkflowProjection, WorkflowRequest, WorkflowRequestSink, WorkflowSendError,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::common::{allow, ctx_with};

#[tokio::test]
async fn ask_user_batch_multiselect_cancel_errors_and_question_alias() {
    let reg = hya_tool_loader::builtins();
    let ask = reg.get("ask_user").unwrap();
    let alias = reg.get("question").unwrap();
    assert_eq!(alias.name(), "ask_user");
    let canonical: Vec<_> = reg
        .snapshot()
        .canonical_tools()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(
        canonical.contains(&"ask_user".to_string()) && !canonical.contains(&"question".to_string())
    );

    // 1. Batch select + freetext.
    let session = SessionId::new();
    let (interaction, mut rx) = InteractionPlane::new();
    let mut ctx = ctx_with(vec![], PathBuf::from("."));
    ctx.session = Some(session);
    ctx.interaction = interaction.for_session(session);
    let ask1 = Arc::clone(&ask);
    let h = tokio::spawn(async move {
        ask1.execute(
            &ctx,
            json!({
                "questions": [
                    { "question": "Pick a color", "header": "Color", "options": [
                        { "label": "red", "description": "Warm" },
                        { "label": "green", "description": "Calm" }
                    ]},
                    { "question": "Branch name?", "header": "Branch", "options": [] }
                ]
            }),
        )
        .await
    });
    let req = rx.recv().await.unwrap();
    assert_eq!(req.session, Some(session));
    assert_eq!(req.questions.len(), 2);
    assert_eq!(
        req.kind,
        QuestionKind::Select {
            options: vec!["red".into(), "green".into()],
            allow_custom: true
        }
    );
    req.reply
        .send_many(vec![
            QuestionAnswer::Selected(1),
            QuestionAnswer::FreeText("codex/todo".into()),
        ])
        .unwrap();
    let out = h.await.unwrap().unwrap();
    assert_eq!(out["title"], "Asked 2 questions");
    assert_eq!(out["metadata"]["answers"][0]["answer"], json!(["green"]));
    assert_eq!(
        out["metadata"]["answers"][1]["answer"],
        json!(["codex/todo"])
    );

    // 2. Multi-select + allow_custom=false, legacy `custom` alias, cancel, validation, unavailable plane.
    let (interaction, mut rx) = InteractionPlane::new();
    let mut ctx = ctx_with(vec![], PathBuf::from("."));
    ctx.session = Some(session);
    ctx.interaction = interaction.for_session(session);
    let ask2 = Arc::clone(&ask);
    let ctx2 = ctx.clone();
    let h = tokio::spawn(async move {
        ask2.execute(
            &ctx2,
            json!({
                "questions": [{
                    "question": "Pick colors", "header": "Color", "multiple": true, "allow_custom": false,
                    "options": [
                        { "label": "red", "description": "Warm" },
                        { "label": "green", "description": "Calm" },
                        { "label": "blue", "description": "Cool" }
                    ]
                }]
            }),
        )
        .await
    });
    let req = rx.recv().await.unwrap();
    assert!(req.info.multiple);
    req.reply
        .send(QuestionAnswer::SelectedMany(vec![0, 2]))
        .unwrap();
    assert_eq!(
        h.await.unwrap().unwrap()["metadata"]["answers"][0]["answer"],
        json!(["red", "blue"])
    );

    // Legacy `custom: false` through hidden `question` alias.
    let ctx3 = ctx.clone();
    let h = tokio::spawn(async move {
        alias
            .execute(
                &ctx3,
                json!({ "questions": [{ "question": "Pick", "header": "h", "custom": false, "options": [{ "label": "a", "description": "d" }] }] }),
            )
            .await
    });
    let req = rx.recv().await.unwrap();
    assert_eq!(
        req.kind,
        QuestionKind::Select {
            options: vec!["a".into()],
            allow_custom: false
        }
    );
    req.reply.send(QuestionAnswer::Selected(0)).unwrap();
    assert_eq!(h.await.unwrap().unwrap()["title"], "Asked 1 question");

    // Cancel returns ToolError::Cancelled.
    let ask3 = Arc::clone(&ask);
    let ctx4 = ctx.clone();
    let h = tokio::spawn(async move {
        ask3.execute(
            &ctx4,
            json!({ "questions": [{ "question": "Ship?", "header": "c", "options": [{ "label": "yes", "description": "go" }] }] }),
        )
        .await
    });
    rx.recv()
        .await
        .unwrap()
        .reply
        .send(QuestionAnswer::Cancelled)
        .unwrap();
    assert!(matches!(h.await.unwrap(), Err(ToolError::Cancelled)));

    // Missing description rejected; dropped receiver surfaces unavailable.
    assert!(matches!(
        ask.execute(&ctx, json!({ "questions": [{ "question": "q", "header": "h", "options": [{ "label": "red" }] }] })).await,
        Err(ToolError::Input(m)) if m.contains("description")
    ));
    drop(rx);
    assert!(matches!(
        ask.execute(&ctx, json!({ "questions": [{ "question": "q", "header": "h", "options": [{ "label": "y", "description": "d" }] }] })).await,
        Err(ToolError::Other(m)) if m.contains("unavailable")
    ));
}

#[tokio::test]
async fn plan_exit_approval_and_decline() {
    let tool = hya_tool_loader::builtins().get("plan_exit").unwrap();
    for (choice, expect_ok) in [(0usize, true), (1usize, false)] {
        let session = SessionId::new();
        let (interaction, mut rx) = InteractionPlane::new();
        let mut ctx = ctx_with(vec![], PathBuf::from("."));
        ctx.session = Some(session);
        ctx.interaction = interaction.for_session(session);
        let t = Arc::clone(&tool);
        let h = tokio::spawn(async move { t.execute(&ctx, json!({})).await });
        let req = rx.recv().await.unwrap();
        assert_eq!(req.session, Some(session));
        assert_eq!(
            req.kind,
            QuestionKind::Select {
                options: vec!["Yes".into(), "No".into()],
                allow_custom: false
            }
        );
        req.reply.send(QuestionAnswer::Selected(choice)).unwrap();
        let res = h.await.unwrap();
        if expect_ok {
            let out = res.unwrap();
            assert_eq!(out["title"], "Switching to build agent");
        } else {
            assert!(matches!(res, Err(ToolError::Other(_))));
        }
    }
}

#[tokio::test]
async fn send_and_list_channel_routing_team_status_and_hash_read() {
    let reg = hya_tool_loader::builtins();
    assert!(reg.get("send").is_some() && reg.get("dm").is_none() && reg.get("broadcast").is_none());

    #[derive(Debug)]
    enum Captured {
        Send { to: MailEndpoint, kind: MailKind },
        SendDefault,
    }
    let (plane, mut rx) = MailboxPlane::new();
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();
    let channels = vec![ChannelRow {
        id: "DM-aB12Cd34".into(),
        kind: ChannelKind::Dm,
        can_speak: true,
        peer: Some("main/worker-1".into()),
        unread: 2,
    }];
    let team = vec![
        MemberStatusRow {
            handle: "main/worker-1".into(),
            status: "busy".into(),
            last_active_seconds: Some(42),
        },
        MemberStatusRow {
            handle: "main/worker-2".into(),
            status: "idle".into(),
            last_active_seconds: None,
        },
    ];
    let service = tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            match req {
                MailboxRequest::Send {
                    to, kind, reply, ..
                } => {
                    let _ = seen_tx.send(Captured::Send {
                        to: to.clone(),
                        kind,
                    });
                    let _ = reply.send(Ok(MailReceipt {
                        from: "main/lead".into(),
                        to,
                        recipients: 2,
                    }));
                }
                MailboxRequest::SendDefault { reply, .. } => {
                    let _ = seen_tx.send(Captured::SendDefault);
                    let _ = reply.send(Ok(MailReceipt {
                        from: "main/lead".into(),
                        to: MailEndpoint::Channel("announce-aB12Cd34".into()),
                        recipients: 2,
                    }));
                }
                MailboxRequest::ListChannels { reply, .. } => {
                    let _ = reply.send(Ok(channels.clone()));
                }
                MailboxRequest::TeamStatus { reply, .. } => {
                    let _ = reply.send(Ok(team.clone()));
                }
                MailboxRequest::ReadChannel { channel, reply, .. } => {
                    let _ = reply.send(Ok((
                        channel.clone(),
                        vec![("main/scout-a".into(), "HELLO".into())],
                        0,
                        Some(format!("served `{channel}`")),
                    )));
                }
                _ => {}
            }
        }
    });

    let session = SessionId::new();
    let mut ctx = ctx_with(vec![], PathBuf::from("."));
    ctx.session = Some(session);
    ctx.mailbox = plane.for_session(session);
    let send = reg.get("send").unwrap();

    for (input, expected) in [
        (
            json!({ "channel": "#ops", "body": "hi" }),
            Some(MailEndpoint::Channel("ops".into())),
        ),
        (
            json!({ "channel": "DM-aB12Cd34", "body": "psst" }),
            Some(MailEndpoint::Channel("DM-aB12Cd34".into())),
        ),
        (
            json!({ "channel": "main/worker-1", "body": "work" }),
            Some(MailEndpoint::Handle("main/worker-1".into())),
        ),
        (
            json!({ "channel": "^parent", "body": "done" }),
            Some(MailEndpoint::Handle("^parent".into())),
        ),
        (json!({ "body": "status" }), None),
    ] {
        let out = send.execute(&ctx, input).await.unwrap();
        let cap = seen_rx.recv().await.unwrap();
        match (expected, cap) {
            (Some(exp), Captured::Send { to, kind }) => {
                assert_eq!(to, exp);
                assert_eq!(kind, MailKind::Message);
            }
            (None, Captured::SendDefault) => {
                assert_eq!(out["metadata"]["to"], "#announce-aB12Cd34");
            }
            other => panic!("unexpected routing: {other:?}"),
        }
    }
    assert!(matches!(
        send.execute(&ctx, json!({ "channel": "#ops", "body": "   " })).await,
        Err(ToolError::Input(m)) if m.contains("empty")
    ));

    // list_channel renders team heartbeats (present & absent).
    let list_channel = reg.get("list_channel").unwrap();
    let lc = list_channel.execute(&ctx, json!({})).await.unwrap();
    let txt = lc["output"].as_str().unwrap();
    assert!(txt.contains("worker-1 · busy · last active 42s ago (heartbeat)"));
    assert!(txt.contains("worker-2 · idle · no heartbeat yet"));
    assert!(txt.contains("#DM-aB12Cd34 · dm · peer main/worker-1 · 2 unread"));

    // Dynamic `channel://` URI handler on HandleRouter serves channel history via `read`.
    struct TestChannelHandler(MailboxPlane);
    #[async_trait]
    impl DynamicUriHandler for TestChannelHandler {
        fn scheme(&self) -> &'static str {
            "channel"
        }
        async fn read(
            &self,
            _ctx: DynamicUriCtx<'_>,
            uri: &str,
        ) -> Result<DynamicUriContent, HandleError> {
            let channel = uri
                .strip_prefix("channel://")
                .unwrap_or_default()
                .to_string();
            let (_ch, messages, _unread, warning) = self
                .0
                .read_channel(channel, None)
                .await
                .map_err(|e| HandleError::Handler(e.to_string()))?;
            let rendered = messages
                .into_iter()
                .map(|(from, body)| format!("[{from}] {body}"))
                .collect::<Vec<_>>()
                .join("\n");
            let body = match warning {
                Some(w) => format!("[warning] {w}\n{rendered}"),
                None => rendered,
            };
            Ok(DynamicUriContent {
                body,
                media_type: "text/plain".to_string(),
                metadata: None,
            })
        }
    }
    let mut read_ctx = ctx_with(vec![allow(Action::Read, "*")], PathBuf::from("."));
    read_ctx.session = Some(session);
    read_ctx.mailbox = ctx.mailbox.clone();
    read_ctx = read_ctx
        .with_uri_handler(Arc::new(TestChannelHandler(ctx.mailbox.clone())))
        .unwrap();
    let read_out = reg
        .get("read")
        .unwrap()
        .execute(&read_ctx, json!({"path": "channel://main/scout-a"}))
        .await
        .unwrap();
    let rtxt = read_out["output"].as_str().unwrap();
    assert!(rtxt.contains("served `main/scout-a`") && rtxt.contains("[main/scout-a] HELLO"));
    service.abort();

    // Without children, list_channel omits the `team` key.
    let (p2, mut rx2) = MailboxPlane::new();
    let svc2 = tokio::spawn(async move {
        while let Some(req) = rx2.recv().await {
            match req {
                MailboxRequest::ListChannels { reply, .. } => {
                    let _ = reply.send(Ok(Vec::new()));
                }
                MailboxRequest::TeamStatus { reply, .. } => {
                    let _ = reply.send(Ok(Vec::new()));
                }
                _ => {}
            }
        }
    });
    let mut ctx2 = ctx_with(vec![], PathBuf::from("."));
    ctx2.session = Some(session);
    ctx2.mailbox = p2.for_session(session);
    let empty_lc = list_channel.execute(&ctx2, json!({})).await.unwrap();
    assert!(empty_lc.get("team").is_none());
    svc2.abort();
}

#[derive(Default)]
struct CaptureWorkflowSink {
    commands: Mutex<Vec<WorkflowCommand>>,
    operations: Mutex<Vec<ToolOperation>>,
    state: WorkflowProjection,
}

impl WorkflowRequestSink for CaptureWorkflowSink {
    fn try_send(&self, request: WorkflowRequest) -> Result<(), WorkflowSendError> {
        self.commands.lock().unwrap().push(request.command);
        self.operations.lock().unwrap().push(request.operation);
        let _ = request.reply.send(Ok(WorkflowCommandResult::State {
            state: self.state.clone(),
        }));
        Ok(())
    }
}

struct RejectingWorkflowSink;

impl WorkflowRequestSink for RejectingWorkflowSink {
    fn try_send(&self, request: WorkflowRequest) -> Result<(), WorkflowSendError> {
        let _ = request.reply.send(Err(WorkflowHostError::new(
            "WORKFLOW_BUSY",
            "another run is active",
        )));
        Ok(())
    }
}

#[tokio::test]
async fn workflow_commands_route_fields_defaults_and_host_errors() {
    let tool = hya_tool_loader::builtins().get("workflow").unwrap();
    let session = SessionId::new();
    let run = hya_proto::WorkflowRunId::new();
    let member = hya_proto::MemberId::new();
    let revision = WorkflowRevision::from_bytes([7; 32]);
    let state: WorkflowProjection = serde_json::from_value(json!({
        "run": {
            "id": run,
            "workflow": { "source": "test:routed", "name": "routed", "revision": revision.to_string() },
            "request_hash": "hash",
            "owner": hya_proto::OwnerRunId::new(),
            "status": "running",
            "stages": [{
                "id": "execute", "agent": "general", "mode": "once", "level": 0,
                "worker_model": { "id": "fake/primary", "reasoning": "high", "fallback": [{"id": "fake/fallback", "reasoning": "medium"}] },
                "selected_worker_model": { "index": 0, "id": "fake/primary", "reasoning": "high" },
                "status": "running",
                "members": [{"member": member, "role": "worker", "iteration": 0}],
                "route_outcomes": [{
                    "session": session, "run": run, "stage": "execute", "member": member,
                    "role": "worker", "iteration": 0, "step": 0, "candidate_index": 0,
                    "model": "fake/primary", "reasoning": "high", "failure_class": "none"
                }]
            }]
        }
    }))
    .unwrap();
    let sink = Arc::new(CaptureWorkflowSink {
        state,
        ..Default::default()
    });
    let mut ctx = ctx_with(vec![allow(Action::Task, "*")], std::env::temp_dir());
    ctx.session = Some(session);
    ctx.workflows =
        WorkflowPlane::from_sink(Arc::clone(&sink) as Arc<dyn hya_tool::WorkflowRequestSink>)
            .for_session(session);

    for input in [
        json!({}),
        json!({"action": "info", "name": "alpha"}),
        json!({"action": "select", "name": "alpha", "expected_revision": revision.to_string()}),
        json!({"action": "state"}),
        json!({"action": "run", "name": "alpha", "expected_revision": revision.to_string(), "inputs": {"count": 3, "message": "hello"}}),
    ] {
        let res = tool.execute(&ctx, input).await.unwrap();
        assert_eq!(res["kind"], "state");
        assert_eq!(
            res["state"]["run"]["stages"][0]["worker_model"]["fallback"][0]["id"],
            "fake/fallback"
        );
        assert_eq!(
            res["state"]["run"]["stages"][0]["route_outcomes"][0]["failure_class"],
            "none"
        );
    }
    assert_eq!(
        *sink.commands.lock().unwrap(),
        vec![
            WorkflowCommand::List,
            WorkflowCommand::Info {
                name: "alpha".into()
            },
            WorkflowCommand::Select {
                name: "alpha".into(),
                expected_revision: Some(revision)
            },
            WorkflowCommand::State,
            WorkflowCommand::Run {
                name: Some("alpha".into()),
                expected_revision: Some(revision),
                inputs: BTreeMap::from([
                    ("count".into(), "3".into()),
                    ("message".into(), "hello".into())
                ]),
                run: None,
            },
        ]
    );
    assert_eq!(
        sink.operations.lock().unwrap().as_slice(),
        &[ctx.operation; 5]
    );

    assert!(
        tool.execute(&ctx, json!({"action": "other"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("expected list|info|select|run|state")
    );

    ctx.workflows = WorkflowPlane::from_sink(Arc::new(RejectingWorkflowSink)).for_session(session);
    let err = tool
        .execute(&ctx, json!({"action": "state"}))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ToolError::WorkflowControl { ref code, ref message }
            if code == "WORKFLOW_BUSY" && message == "another run is active"
    ));
}

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
    let mut no_sess = ctx_with(vec![allow(Action::Task, "*")], std::env::temp_dir());
    no_sess.parent_session = Some(parent);
    assert!(matches!(
        tool.execute(&no_sess, json!({ "prompt": "x", "subagent_type": "quick" }))
            .await,
        Err(ToolError::Other(_))
    ));

    // Subagent nested spawn + operation identity + effort + inline empty description normalization.
    let (spawner, mut rx) = SpawnerPlane::new();
    let mut ctx = ctx_with(vec![allow(Action::Task, "*")], std::env::temp_dir());
    ctx.session = Some(child);
    ctx.parent_session = Some(parent);
    ctx.spawner = spawner.for_session(child);
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
    let mut ctx = ctx_with(vec![allow(Action::Task, "*")], std::env::temp_dir());
    ctx.session = Some(session);
    ctx.spawner = spawner.for_session(session);
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
    ctx.spawner = full_spawner.for_session(session);
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
