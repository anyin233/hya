//! Interactions, permission modes, saved permission rules, tool-part &
//! member-lifecycle wire contract, and prompt image attachments over HTTP/gRPC.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::http::StatusCode;
use futures::StreamExt;
use hya_api::v2 as pb;
use hya_engine::{EventBus, SessionEngine};
use hya_proto::{
    AgentName, Envelope, Event, FinishReason, MemberId, MemberRunStatus, Message, MessageId,
    ModelRef, Part, PermissionRequestId, QuestionRequestId, Role, SessionId, ToolCallId,
};
use hya_tool::extension::ExtensionEvent;
use hya_tool::{ArchiveReason, ReportOutcome};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_server::router;
use hya_store::{SavedPermission, SessionStore};
use hya_tool::interaction::{
    QuestionInfo, QuestionKind, QuestionOption, QuestionPrompt, QuestionRequest,
};
use hya_tool::permission::{
    Action, AskRequest, Decision, Invocation, InvocationPolicy, InvocationRule, Mode,
    PermissionModel, PermissionTarget, RememberScope, Resource,
};
use hya_tool::{PermissionPlane, PermissionRules, Rule};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::harness::{
    assert_err, b64_encode, base_state, collect_sse, create_session, create_session_id,
    grpc_channel, grpc_code, grpc_json, id_str, ok_del, ok_get, ok_patch, ok_post, open_sse, patch,
    post, sse_frames_until_stream, start_turn, state_of, state_with_rules, wait_not_busy,
    wait_turn,
};
use crate::support;

fn bash_ask(
    id: PermissionRequestId,
    session: SessionId,
    cmd: &str,
    msg: Option<MessageId>,
    call: Option<ToolCallId>,
) -> (AskRequest, oneshot::Receiver<Decision>) {
    let (reply, rx) = oneshot::channel();
    (
        AskRequest {
            id,
            session: Some(session),
            message_id: msg,
            call_id: call,
            action: Action::Bash,
            resource: Resource::Command(cmd.into()),
            remember: RememberScope::LegacyAction,
            reply,
        },
        rx,
    )
}

fn select_question(
    id: QuestionRequestId,
    session: SessionId,
) -> (
    QuestionRequest,
    oneshot::Receiver<Vec<hya_tool::interaction::QuestionAnswer>>,
) {
    let info = QuestionInfo {
        question: "Which branch?".into(),
        header: "Branch".into(),
        options: vec![
            QuestionOption {
                label: "main".into(),
                description: "the default branch".into(),
            },
            QuestionOption {
                label: "dev".into(),
                description: String::new(),
            },
        ],
        multiple: true,
        custom: Some(false),
    };
    let kind = QuestionKind::Select {
        options: vec!["main".into(), "dev".into()],
        allow_custom: false,
    };
    let (tx, rx) = oneshot::channel();
    (
        QuestionRequest {
            id,
            session: Some(session),
            prompt: "Which branch?".into(),
            info: info.clone(),
            kind: kind.clone(),
            questions: vec![QuestionPrompt::new(info, kind)],
            reply: hya_tool::QuestionReply::Many(tx),
        },
        rx,
    )
}

// ---- Interactions & Filtering ----

#[tokio::test]
async fn permission_and_question_asks_stream_list_and_resolve_over_http_and_grpc() {
    let (ask_tx, ask_rx) = mpsc::unbounded_channel();
    let (q_tx, q_rx) = mpsc::unbounded_channel();
    let state = base_state()
        .await
        .with_permission_requests(ask_rx)
        .with_question_requests(q_rx);
    let app = router(state.clone());
    let mut grpc = pb::interactions_client::InteractionsClient::new(grpc_channel(state).await);

    // 1. Permission ask streams, matches HTTP & gRPC listings, and "always" persists a saved rule.
    let ask_frames = collect_sse(&app, "/v2/events/stream", |f| {
        f["event"]["permissionRequested"].is_object()
    })
    .await;
    let (perm_id, session, message, call) = (
        PermissionRequestId::new(),
        SessionId::new(),
        MessageId::new(),
        ToolCallId::new(),
    );
    let (req, reply_rx) = bash_ask(
        perm_id,
        session,
        "printf hi > file",
        Some(message),
        Some(call),
    );
    ask_tx.send(req).unwrap();

    let frames = ask_frames.await.unwrap();
    let ev = frames
        .iter()
        .map(|f| &f["event"])
        .find(|e| e["permissionRequested"].is_object())
        .unwrap();
    assert!(ev.get("seq").is_none());
    let expected_perm = json!({
        "id": perm_id.to_string(), "session": session.to_string(),
        "type": "INTERACTION_TYPE_PERMISSION", "title": "bash printf hi > file",
        "payload": { "action": "bash", "resource": "printf hi > file", "always": ["*"], "messageId": message.to_string(), "callId": call.to_string() },
    });
    assert_eq!(ev["permissionRequested"]["interaction"], expected_perm);
    assert_eq!(
        ok_get(&app, &format!("/v2/interactions?session={session}")).await["interactions"],
        json!([expected_perm])
    );
    assert_eq!(
        ok_get(&app, "/v2/interactions").await["interactions"][0]["id"],
        json!(perm_id.to_string())
    );
    assert!(
        ok_get(&app, "/v2/interactions?type=INTERACTION_TYPE_QUESTION")
            .await
            .get("interactions")
            .is_none()
    );
    assert_eq!(
        grpc_json(
            grpc.list_interactions(pb::ListInteractionsRequest::default())
                .await
        )["interactions"],
        json!([expected_perm])
    );

    let resolved_perm = collect_sse(&app, "/v2/events/stream", |f| {
        f["event"]["interactionResolved"].is_object()
    })
    .await;
    assert_eq!(
        ok_post(
            &app,
            &format!("/v2/interactions/{perm_id}/respond"),
            json!({"permission": {"allowed": true, "persist": true}})
        )
        .await["applied"],
        json!(true)
    );
    let _ = reply_rx.await;
    assert!(
        resolved_perm
            .await
            .unwrap()
            .iter()
            .any(|f| f["event"]["interactionResolved"]["request"] == json!(perm_id.to_string()))
    );
    assert!(
        ok_get(&app, "/v2/permissions/rules").await["rules"]
            .as_array()
            .is_some_and(|r| !r.is_empty())
    );

    // 2. Question ask streams with options/header, matches HTTP & gRPC listing, and reject resolves.
    let q_frames = collect_sse(&app, "/v2/events/stream", |f| {
        f["event"]["questionRequested"].is_object()
    })
    .await;
    let q_id = QuestionRequestId::new();
    let (q_req, _q_rx) = select_question(q_id, session);
    q_tx.send(q_req).unwrap();
    let streamed = q_frames
        .await
        .unwrap()
        .into_iter()
        .find_map(|f| {
            f["event"]["questionRequested"]["interaction"]
                .as_object()
                .cloned()
                .map(Value::Object)
        })
        .unwrap();
    assert_eq!(
        (&streamed["options"], &streamed["detail"]),
        (&json!(["main", "dev"]), &json!("Branch"))
    );
    assert_eq!(
        ok_get(&app, "/v2/interactions?type=INTERACTION_TYPE_QUESTION").await["interactions"][0],
        streamed
    );
    let grpc_q = &grpc
        .list_interactions(pb::ListInteractionsRequest::default())
        .await
        .unwrap()
        .into_inner()
        .interactions[0];
    assert_eq!(
        (grpc_q.id.as_str(), grpc_q.detail.as_str()),
        (q_id.to_string().as_str(), "Branch")
    );

    let resolved_q = collect_sse(&app, "/v2/events/stream", |f| {
        f["event"]["interactionResolved"].is_object()
    })
    .await;
    let _ = ok_post(
        &app,
        &format!("/v2/interactions/{q_id}/respond"),
        json!({"question": {"rejected": true}}),
    )
    .await;
    assert!(
        resolved_q
            .await
            .unwrap()
            .iter()
            .any(|f| f["event"]["interactionResolved"]["request"] == json!(q_id.to_string()))
    );
}

#[tokio::test]
async fn stream_filtering_descendant_questions_interactions_only_and_catalog_notices() {
    let (ask_tx, ask_rx) = mpsc::unbounded_channel();
    let (q_tx, q_rx) = mpsc::unbounded_channel();
    let state = base_state()
        .await
        .with_permission_requests(ask_rx)
        .with_question_requests(q_rx);
    let app = router(state.clone());
    let mut events_client = pb::events_client::EventsClient::new(grpc_channel(state.clone()).await);

    // Descendant questions reach parent stream on `include_descendants: true`.
    let root = create_session(&app, &std::env::temp_dir()).await;
    let child = create_session_id(
        &app,
        json!({"parent": root, "workdir": std::env::temp_dir().to_string_lossy()}),
    )
    .await;
    let mut parent_stream = events_client
        .stream_session_events(pb::StreamSessionEventsRequest {
            session: root.clone(),
            include_descendants: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let q_id = QuestionRequestId::new();
    let (q_req, _q_rx) = select_question(q_id, child.parse().unwrap());
    q_tx.send(q_req).unwrap();
    let (ev_session, asked) = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = parent_stream.next().await {
            if let Some(pb::stream_frame::Frame::Event(e)) = frame.unwrap().frame
                && let Some(pb::stream_event::Payload::QuestionRequested(a)) = e.payload
            {
                return (e.session, a);
            }
        }
        panic!("stream ended")
    })
    .await
    .unwrap();
    assert_eq!(
        (ev_session.as_str(), asked.request.as_str()),
        (child.as_str(), q_id.to_string().as_str())
    );

    // Global `interactionsOnly` keeps root list frames, catalog notices, and asks on HTTP & gRPC.
    let is_ask = |f: &Value| f["event"]["permissionRequested"].is_object();
    let filtered = collect_sse(&app, "/v2/events/stream?interactionsOnly=true", is_ask).await;
    let unfiltered = collect_sse(&app, "/v2/events/stream", is_ask).await;
    let session_cat = collect_sse(&app, &format!("/v2/sessions/{root}/events/stream"), |f| {
        f["event"]["catalogUpdated"].is_object()
    })
    .await;
    let mut grpc_global = events_client
        .stream_global_events(pb::StreamGlobalEventsRequest {
            interactions_only: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    let _ = create_session(&app, &std::env::temp_dir()).await;
    state.notify_catalog_updated();
    let (req, _rx) = bash_ask(
        PermissionRequestId::new(),
        SessionId::new(),
        "ls",
        None,
        None,
    );
    ask_tx.send(req).unwrap();

    assert!(
        unfiltered
            .await
            .unwrap()
            .iter()
            .any(|f| f["event"]["sessionStarted"].is_object())
    );
    let filtered_kinds: Vec<String> = filtered
        .await
        .unwrap()
        .iter()
        .filter_map(|f| f["event"].as_object())
        .flat_map(|e| e.keys().cloned().collect::<Vec<_>>())
        .filter(|k| {
            !matches!(
                k.as_str(),
                "seq" | "session" | "timeRecorded" | "projectsUpdated"
            )
        })
        .collect();
    assert!(
        filtered_kinds.contains(&"sessionStarted".into())
            && filtered_kinds.contains(&"catalogUpdated".into())
    );
    assert_eq!(
        filtered_kinds.last().map(String::as_str),
        Some("permissionRequested")
    );

    let last_cat = session_cat.await.unwrap().pop().unwrap();
    assert!(
        last_cat["event"]["catalogUpdated"].is_object() && last_cat["event"].get("seq").is_none()
    );

    let mut grpc_kinds = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = grpc_global.next().await {
            if let Some(pb::stream_frame::Frame::Event(e)) = frame.unwrap().frame {
                match e.payload {
                    Some(pb::stream_event::Payload::SessionStarted(_)) => {
                        grpc_kinds.push("sessionStarted")
                    }
                    Some(pb::stream_event::Payload::CatalogUpdated(_)) => {
                        grpc_kinds.push("catalogUpdated")
                    }
                    Some(pb::stream_event::Payload::PermissionRequested(_)) => {
                        grpc_kinds.push("permissionRequested");
                        break;
                    }
                    _ => {}
                }
            }
        }
    })
    .await
    .unwrap();
    assert!(
        grpc_kinds.contains(&"sessionStarted")
            && grpc_kinds.contains(&"catalogUpdated")
            && grpc_kinds.last() == Some(&"permissionRequested")
    );
}

// ---- Permission Modes ----

#[tokio::test]
async fn permission_modes_list_update_replay_and_yolo_approves_tree_asks() {
    let (ask_tx, ask_rx) = mpsc::unbounded_channel();
    let app = router(base_state().await.with_permission_requests(ask_rx));
    let dir = std::env::temp_dir();

    let modes = ok_get(&app, "/v2/permission-modes").await;
    assert_eq!(
        (&modes["modes"][0]["id"], &modes["modes"][1]["id"]),
        (&json!("manual"), &json!("yolo"))
    );

    let root = create_session(&app, &dir).await;
    let child = create_session_id(
        &app,
        json!({"parent": root, "workdir": dir.to_string_lossy()}),
    )
    .await;
    let other = create_session(&app, &dir).await;

    for invalid in ["danger", "acme/approver/careful", "careful"] {
        assert_err(
            patch(
                &app,
                &format!("/v2/sessions/{root}"),
                json!({"permissionMode": invalid}),
            )
            .await,
            StatusCode::BAD_REQUEST,
            "invalid_argument",
        );
    }

    let send_ask = |session: &str, cmd: &str| {
        let id = PermissionRequestId::new();
        let (req, rx) = bash_ask(id, session.parse().unwrap(), cmd, None, None);
        ask_tx.send(req).unwrap();
        (id.to_string(), rx)
    };
    let (child_id, child_rx) = send_ask(&child, "printf child");
    let (_, root_rx) = send_ask(&root, "printf root");
    let (other_id, mut other_rx) = send_ask(&other, "printf other");
    tokio::time::timeout(Duration::from_secs(5), async {
        while
            ok_get(&app, "/v2/interactions?type=INTERACTION_TYPE_PERMISSION").await["interactions"]
                .as_array()
                .map_or(0, Vec::len)
                < 3
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    let resolved = collect_sse(&app, "/v2/events/stream", {
        let child_id = child_id.clone();
        move |f| f["event"]["interactionResolved"]["request"] == json!(child_id)
    })
    .await;

    assert_eq!(
        ok_patch(
            &app,
            &format!("/v2/sessions/{child}"),
            json!({"permissionMode": "yolo"})
        )
        .await["permissionMode"],
        json!("yolo")
    );
    assert_eq!(
        ok_get(&app, &format!("/v2/sessions/{root}")).await["permissionMode"],
        json!("yolo")
    );
    assert_eq!(
        (child_rx.await.unwrap(), root_rx.await.unwrap()),
        (Decision::AllowOnce, Decision::AllowOnce)
    );
    assert!(
        resolved
            .await
            .unwrap()
            .iter()
            .any(|f| f["event"]["interactionResolved"]["request"] == json!(child_id))
    );
    assert!(other_rx.try_recv().is_err());
    assert!(
        ok_get(&app, &format!("/v2/sessions/{root}/events")).await["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["sessionUpdated"]["permissionMode"] == json!("yolo"))
    );

    let (_, late_rx) = send_ask(&child, "printf late");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), late_rx)
            .await
            .unwrap()
            .unwrap(),
        Decision::AllowOnce
    );
    let _ = ok_patch(
        &app,
        &format!("/v2/sessions/{root}"),
        json!({"permissionMode": "manual"}),
    )
    .await;
    let (_, mut m_rx) = send_ask(&child, "printf manual");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ids: Vec<String> = ok_get(
                &app,
                "/v2/interactions?type=INTERACTION_TYPE_PERMISSION",
            )
            .await["interactions"]
                .as_array()
                .map_or_else(Vec::new, |r| r.iter().map(|x| id_str(&x["id"])).collect());
            if ids.len() >= 2 && ids.contains(&other_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(m_rx.try_recv().is_err());
}

// ---- Saved Permission Rules ----

async fn authorized_without_ask(
    plane: &PermissionPlane,
    asks: &mut mpsc::UnboundedReceiver<AskRequest>,
) -> bool {
    let mut task = {
        let plane = plane.clone();
        tokio::spawn(async move {
            plane
                .authorize(&Invocation::tool("write", Mode::Ask))
                .await
                .is_ok()
        })
    };
    tokio::select! {
        res = &mut task => res.unwrap(),
        ask = tokio::time::timeout(Duration::from_secs(2), asks.recv()) => {
            if let Ok(Some(req)) = ask { let _ = req.reply.send(Decision::AllowOnce); let _ = task.await.unwrap(); false }
            else { task.await.unwrap() }
        }
    }
}

#[tokio::test]
async fn saved_rules_report_allow_time_reload_and_revoke() {
    let store = SessionStore::connect_memory().await.unwrap();
    for row in [
        SavedPermission {
            id: "psv_write".into(),
            project_id: "global".into(),
            action: "tool".into(),
            resource: "write".into(),
            time_created_ms: Some(1_700_000_000_000),
        },
        SavedPermission {
            id: "psv_outside".into(),
            project_id: "prj_scoped".into(),
            action: "externaldirectory".into(),
            resource: "/outside/*".into(),
            time_created_ms: Some(1_700_000_000_000),
        },
    ] {
        store.save_permission(&row).await.unwrap();
    }
    let policy = InvocationPolicy::compile(
        PermissionModel::Default,
        vec![InvocationRule::new(
            PermissionTarget::Tool,
            "^write$",
            Mode::Ask,
        )],
    )
    .unwrap();
    let (plane, mut asks) = PermissionPlane::new_with_policy(PermissionRules::default(), policy);
    let engine = Arc::new(SessionEngine::new(
        store,
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![])))),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        plane.clone(),
        EventBus::default(),
    ));
    let state = state_of(engine, std::env::temp_dir());
    assert_eq!(state.restore_saved_permissions().await.unwrap(), 2);
    assert!(authorized_without_ask(&plane, &mut asks).await);

    let app = router(state);
    let rules = ok_get(&app, "/v2/permissions/rules").await["rules"]
        .as_array()
        .cloned()
        .unwrap();
    let w = rules.iter().find(|r| r["id"] == "psv_write").unwrap();
    assert_eq!(
        (
            &w["permission"],
            &w["tool"],
            &w["timeCreated"],
            &w["projectId"]
        ),
        (
            &json!("RULE_PERMISSION_ALLOW"),
            &json!("write"),
            &json!("2023-11-14T22:13:20+00:00"),
            &json!("global")
        )
    );
    let ext = rules.iter().find(|r| r["id"] == "psv_outside").unwrap();
    assert_eq!(
        (&ext["tool"], &ext["pattern"], &ext["projectId"]),
        (
            &json!("externaldirectory"),
            &json!("/outside/*"),
            &json!("prj_scoped")
        )
    );

    let _ = ok_del(&app, "/v2/permissions/rules/psv_write").await;
    assert!(!authorized_without_ask(&plane, &mut asks).await);
    assert_eq!(
        ok_get(&app, "/v2/permissions/rules").await["rules"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

// ---- Tool Parts & Member Updates ----

fn payloads(frames: &[Value]) -> Vec<(String, Value, bool)> {
    frames
        .iter()
        .filter_map(|f| f.get("event"))
        .filter_map(|e| {
            let durable = e.get("seq").is_some();
            e.as_object()?
                .iter()
                .find(|(k, _)| !matches!(k.as_str(), "seq" | "session" | "timeRecorded"))
                .map(|(k, v)| (k.clone(), v.clone(), durable))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_parts_stream_live_args_states_and_match_grpc_reads_and_replay() {
    let dir = support::tempdir("tool-parts");
    let (present, missing) = (dir.join("present.txt"), dir.join("missing.txt"));
    std::fs::write(&present, "hello tool output").unwrap();
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".into(),
                input: json!({"path": present}),
            },
            FakeStep::ToolCall {
                name: "read".into(),
                input: json!({"path": missing}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let (state, _, _) = state_with_rules(
        provider,
        PermissionRules::new(vec![Rule::new(Action::Read, "/**", Mode::Allow)]),
        dir.clone(),
    )
    .await;
    let app = router(state.clone());
    let session = create_session(&app, &dir).await;

    let ch = grpc_channel(state).await;
    let mut events_client = pb::events_client::EventsClient::new(ch.clone());
    let mut grpc_stream = events_client
        .stream_session_events(pb::StreamSessionEventsRequest {
            session: session.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let saw_tool = Arc::new(AtomicBool::new(false));
    let collector = collect_sse(&app, &format!("/v2/sessions/{session}/events/stream"), {
        let saw_tool = Arc::clone(&saw_tool);
        move |f| {
            if f["event"]["toolStateChanged"].is_object() {
                saw_tool.store(true, Ordering::SeqCst);
            }
            saw_tool.load(Ordering::SeqCst) && f["event"]["messageFinished"].is_object()
        }
    })
    .await;

    let turn = start_turn(&app, &session, json!({"prompt": {"text": "read both"}})).await;
    assert_eq!(
        wait_turn(&app, &session, &turn).await["state"],
        json!("TURN_STATE_FINISHED")
    );

    let listed = ok_get(&app, &format!("/v2/sessions/{session}/messages")).await;
    let tools: Vec<(String, Value)> = listed["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["parts"].as_array().cloned().unwrap_or_default())
        .filter(|p| p["toolCall"].is_object())
        .map(|p| (id_str(&p["id"]), p["toolCall"].clone()))
        .collect();
    assert_eq!(tools.len(), 2);
    let ((ok_part, ok), (err_part, err)) = (&tools[0], &tools[1]);
    assert_eq!(
        (&ok["tool"], &ok["state"]),
        (&json!("read"), &json!("TOOL_EXECUTION_STATE_OK"))
    );
    assert_eq!(
        serde_json::from_str::<Value>(ok["inputJson"].as_str().unwrap()).unwrap(),
        json!({"path": present})
    );
    assert!(
        ok["outputJson"]
            .as_str()
            .unwrap()
            .contains("hello tool output")
    );
    assert_eq!(err["state"], json!("TOOL_EXECUTION_STATE_ERROR"));
    assert!(err["errorMessage"].as_str().is_some_and(|t| !t.is_empty()));

    let stream = payloads(&collector.await.unwrap());
    let pos =
        |p: &dyn Fn(&str, &Value) -> bool| stream.iter().position(|(k, v, _)| p(k, v)).unwrap();
    let started = pos(&|k, v| k == "partStarted" && v["part"] == json!(ok_part));
    let appended = pos(&|k, v| k == "partAppended" && v["part"] == json!(ok_part));
    let running = pos(&|k, v| {
        k == "toolStateChanged"
            && v["part"] == json!(ok_part)
            && v["state"] == json!("TOOL_EXECUTION_STATE_RUNNING")
    });
    let done = pos(&|k, v| {
        k == "toolStateChanged"
            && v["part"] == json!(ok_part)
            && v["state"] == json!("TOOL_EXECUTION_STATE_OK")
    });
    let failed = pos(&|k, v| {
        k == "toolStateChanged"
            && v["part"] == json!(err_part)
            && v["state"] == json!("TOOL_EXECUTION_STATE_ERROR")
    });
    assert!(started < appended && appended < running && running < done);
    assert!(
        !stream[appended].2
            && stream[started].2
            && stream[running].2
            && stream[done].2
            && stream[failed].2
    );

    // gRPC stream receives identical live argument deltas, and gRPC reads/replay match HTTP.
    let grpc_frames = tokio::time::timeout(Duration::from_secs(5), async {
        let (mut out, mut saw) = (Vec::new(), false);
        while let Some(frame) = grpc_stream.next().await {
            let f = serde_json::to_value(frame.unwrap()).unwrap();
            saw |= f["event"]["toolStateChanged"].is_object();
            let fin = saw && f["event"]["messageFinished"].is_object();
            out.push(f);
            if fin {
                break;
            }
        }
        out
    })
    .await
    .unwrap();
    let arg_deltas = |s: &[(String, Value, bool)]| -> Vec<Value> {
        s.iter()
            .filter(|(k, v, _)| {
                k == "partAppended" && (v["part"] == json!(ok_part) || v["part"] == json!(err_part))
            })
            .map(|(_, v, d)| {
                assert!(!d);
                v.clone()
            })
            .collect()
    };
    assert_eq!(arg_deltas(&payloads(&grpc_frames)), arg_deltas(&stream));
    assert_eq!(
        grpc_json(
            pb::messages_client::MessagesClient::new(ch)
                .list_messages(pb::ListMessagesRequest {
                    session: session.clone(),
                    ..Default::default()
                })
                .await
        ),
        listed
    );
    assert_eq!(
        grpc_json(
            events_client
                .list_events(pb::ListEventsRequest {
                    session: session.clone(),
                    ..Default::default()
                })
                .await
        ),
        ok_get(&app, &format!("/v2/sessions/{session}/events")).await
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_interactions_name_tool_and_correlate_call_id() {
    let dir = support::tempdir("tool-perm");
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "bash".into(),
                input: json!({"command": "printf guarded", "timeout": 5000}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let (state, asks, _) = state_with_rules(
        provider,
        PermissionRules::new(vec![Rule::new(Action::Bash, "*", Mode::Ask)]),
        dir.clone(),
    )
    .await;
    let app = router(state.with_permission_requests(asks));
    let session = create_session(&app, &dir).await;

    let collector = collect_sse(&app, "/v2/events/stream", |f| {
        f["event"]["permissionRequested"].is_object()
    })
    .await;
    let turn = start_turn(&app, &session, json!({"prompt": {"text": "run it"}})).await;
    let asked = collector
        .await
        .unwrap()
        .into_iter()
        .find_map(|f| f["event"]["permissionRequested"].as_object().cloned())
        .unwrap();
    let streamed = &asked["interaction"]["payload"];
    let listed = ok_get(&app, &format!("/v2/interactions?session={session}")).await["interactions"]
        [0]
    .clone();
    assert_eq!(listed["id"], asked["request"]);
    for payload in [streamed, &listed["payload"]] {
        assert_eq!(
            (&payload["action"], &payload["tool"], &payload["resource"]),
            (&json!("bash"), &json!("bash"), &json!("printf guarded"))
        );
        assert_eq!(payload["input"]["timeout"].as_f64(), Some(5000.0));
        assert!(
            payload["callId"].as_str().is_some_and(|s| !s.is_empty())
                && payload["messageId"].as_str().is_some_and(|s| !s.is_empty())
        );
    }
    let _ = ok_post(
        &app,
        &format!(
            "/v2/interactions/{}/respond",
            asked["request"].as_str().unwrap()
        ),
        json!({"permission": {"allowed": false}}),
    )
    .await;
    wait_turn(&app, &session, &turn).await;

    let msgs = ok_get(&app, &format!("/v2/sessions/{session}/messages")).await;
    let call_ids: Vec<Value> = msgs["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["parts"].as_array().cloned().unwrap_or_default())
        .filter_map(|p| p["toolCall"].get("callId").cloned())
        .collect();
    assert_eq!(call_ids, vec![streamed["callId"].clone()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn member_updates_reports_and_archival_revival_match_across_sse_grpc_and_replay() {
    let dir = support::tempdir("member-lifecycle");
    let (state, _, engine) = state_with_rules(
        FakeProvider::scripted(vec![]),
        PermissionRules::default(),
        dir.clone(),
    )
    .await;
    let app = router(state.clone());
    let root_id = create_session(&app, &dir).await;
    let lead_id = create_session_id(
        &app,
        json!({"parent": root_id, "workdir": dir.to_string_lossy()}),
    )
    .await;
    let child_id = create_session_id(
        &app,
        json!({"parent": lead_id, "workdir": dir.to_string_lossy()}),
    )
    .await;
    let (root, lead, child): (SessionId, SessionId, SessionId) = (
        root_id.parse().unwrap(),
        lead_id.parse().unwrap(),
        child_id.parse().unwrap(),
    );

    let member = MemberId::new();
    let handle = "main/lead/helper";
    let reg = |s: SessionId, h: &str, p: Option<&str>| {
        ExtensionEvent::AgentRegistered {
            session: root,
            agent_session: s,
            handle: h.into(),
            parent: p.map(str::to_owned),
            agent_type: AgentName::new("build"),
            mode: hya_proto::SubagentMode::Resident,
        }
        .into_event()
    };
    let emit = |ev: Event| {
        let engine = engine.clone();
        async move {
            let (seq, ts_millis) = engine
                .store()
                .append_event(ev.session().unwrap(), &ev)
                .await
                .unwrap();
            engine.bus().publish(Envelope {
                seq,
                ts_millis,
                event: ev,
            });
        }
    };

    let member_collector = collect_sse(
        &app,
        &format!("/v2/sessions/{lead_id}/events/stream"),
        |f| {
            f["event"]["extensionEvent"]["bundle"] == json!("hya/multi-agent")
                && f["event"]["extensionEvent"]["kind"] == json!("subagent_reported")
        },
    )
    .await;
    for ev in [
        reg(root, "main", None),
        reg(lead, "lead", Some("main")),
        reg(child, "helper", Some("main/lead")),
        ExtensionEvent::MemberSpawned {
            session: lead,
            member,
            child: Some(child),
            subagent_type: AgentName::new("build"),
            description: "nested review".into(),
            depth: 2,
            directive: "review".into(),
            tool_call: Some(ToolCallId::new()),
        }
        .into_event(),
        ExtensionEvent::MemberStatusChanged {
            session: lead,
            member,
            status: MemberRunStatus::Running,
        }
        .into_event(),
        ExtensionEvent::SubagentReported {
            session: lead,
            member,
            child,
            handle: handle.into(),
            outcome: ReportOutcome::Done,
            report: "review complete".into(),
        }
        .into_event(),
    ] {
        emit(ev).await;
    }
    let updates: Vec<Value> = payloads(&member_collector.await.unwrap())
        .into_iter()
        .filter(|(k, v, _)| {
            k == "extensionEvent" && v["bundle"] == json!("hya/multi-agent")
        })
        .map(|(_, v, d)| {
            assert!(d);
            v
        })
        .collect();
    assert_eq!(
        (
            updates.len(),
            updates[2]["kind"].as_str(),
            updates[2]["payload"]["handle"].as_str(),
        ),
        (3, Some("subagent_reported"), Some(handle))
    );
    let lead_snap = ok_get(&app, &format!("/v2/sessions/{lead_id}")).await;
    assert_eq!(
        lead_snap["extensions"]["hya/multi-agent"]["members"][0]["status"],
        json!("done")
    );

    // Archival & revival match snapshots, replay, SSE, and gRPC.
    let sse = open_sse(&app, &format!("/v2/sessions/{root_id}/events/stream")).await;
    let ch = grpc_channel(state).await;
    let mut live = pb::events_client::EventsClient::new(ch.clone())
        .stream_global_events(pb::StreamGlobalEventsRequest {
            interactions_only: false,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let mut session_client = pb::session_client::SessionClient::new(ch);

    let mut live_frames = Vec::new();
    for (archived, expected_kind, ev) in [
        (
            true,
            "agent_archived",
            ExtensionEvent::AgentArchived {
                session: root,
                child,
                handle: handle.into(),
                reason: ArchiveReason::Reported,
            }
            .into_event(),
        ),
        (
            false,
            "agent_restarted",
            ExtensionEvent::AgentRestarted {
                session: root,
                child,
                handle: handle.into(),
                epoch: hya_proto::ActorEpoch::INITIAL,
            }
            .into_event(),
        ),
    ] {
        if !archived {
            engine
                .store()
                .append_event(root, &reg(child, "helper", Some("main/lead")))
                .await
                .unwrap();
        }
        emit(ev).await;
        let json = serde_json::to_value(
            tokio::time::timeout(Duration::from_secs(5), live.message())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            (
                json["event"]["extensionEvent"]["bundle"].as_str(),
                json["event"]["extensionEvent"]["kind"].as_str(),
            ),
            (Some("hya/multi-agent"), Some(expected_kind))
        );
        live_frames.push(json);

        let snap = ok_get(&app, &format!("/v2/sessions/{root_id}")).await;
        let ma = &snap["extensions"]["hya/multi-agent"];
        if archived {
            assert_eq!(ma["archived"][handle]["reason"], json!("reported"));
            assert!(ma["roster"].get(handle).is_none());
        } else {
            assert!(ma["roster"].get(handle).is_some());
        }
        assert_eq!(
            grpc_json(
                session_client
                    .get_session(pb::GetSessionRequest {
                        session: root_id.clone()
                    })
                    .await
            )["extensions"]["hya/multi-agent"],
            *ma
        );
    }
    let sse_frames = sse_frames_until_stream(sse, |f| {
        f["event"]["extensionEvent"]["kind"] == json!("agent_restarted")
    })
    .await;
    assert_eq!(sse_frames, live_frames);
    let replay: Vec<_> = ok_get(&app, &format!("/v2/sessions/{root_id}/events")).await["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e["extensionEvent"]["kind"] == json!("agent_archived")
                || e["extensionEvent"]["kind"] == json!("agent_restarted")
        })
        .cloned()
        .collect();
    assert_eq!(
        replay,
        live_frames
            .iter()
            .map(|f| f["event"].clone())
            .collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- Prompt Attachments ----

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRpixel";
const JPEG: &[u8] = b"\xff\xd8\xff\xe0\x00\x10JFIFpixel";

struct RecordingVisionProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    image_input: Option<bool>,
}

#[async_trait]
impl Provider for RecordingVisionProvider {
    fn id(&self) -> &str {
        "recording"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            image_input: self.image_input,
            ..Capabilities::default()
        })
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        )])))
    }
}

fn req_media(
    requests: &Mutex<Vec<CompletionRequest>>,
    idx: usize,
) -> Vec<(String, String, Option<String>)> {
    requests.lock().unwrap()[idx]
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::User { parts, .. } => Some(parts),
            _ => None,
        })
        .flatten()
        .filter_map(|p| match p {
            Part::Media {
                media_type,
                data,
                filename,
                ..
            } => Some((media_type.clone(), data.clone(), filename.clone())),
            _ => None,
        })
        .collect()
}

fn attachment_parts(message: &Value) -> Vec<Value> {
    message["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p.get("attachment").is_some())
        .cloned()
        .collect()
}

#[tokio::test]
async fn prompt_attachments_round_trip_sniff_fork_and_validation() {
    let dir = support::tempdir("attach");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = router(
        state_with_rules(
            RecordingVisionProvider {
                requests: Arc::clone(&requests),
                image_input: None,
            },
            PermissionRules::default(),
            dir.clone(),
        )
        .await
        .0,
    );
    let session = create_session(&app, &dir).await;

    // PNG reaches provider, lists as AttachmentPart without bytes, replays `partsAdded`.
    let turn = start_turn(&app, &session, json!({"prompt": {"text": "what?", "attachments": [{"name": "pixel.png", "mime": "image/png", "data": b64_encode(PNG), "path": "/tmp/pixel.png"}]}})).await;
    wait_turn(&app, &session, &turn).await;
    wait_not_busy(&app, &session).await;
    assert_eq!(
        req_media(&requests, 0),
        vec![(
            "image/png".into(),
            format!("data:image/png;base64,{}", b64_encode(PNG)),
            Some("pixel.png".into())
        )]
    );
    let msgs = ok_get(&app, &format!("/v2/sessions/{session}/messages")).await["messages"]
        .as_array()
        .cloned()
        .unwrap();
    let att = &attachment_parts(msgs.iter().find(|m| m["id"] == json!(turn)).unwrap())[0];
    assert_eq!(
        (
            &att["attachment"]["name"],
            &att["attachment"]["mime"],
            &att["attachment"]["size"]
        ),
        (
            &json!("pixel.png"),
            &json!("image/png"),
            &json!(PNG.len().to_string())
        )
    );
    assert!(att["attachment"].get("data").is_none());
    let added = ok_get(&app, &format!("/v2/sessions/{session}/events")).await["events"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|e| e.get("partsAdded").cloned())
        .unwrap();
    assert_eq!(
        (&added["message"], &added["parts"][0]["id"]),
        (&json!(turn), &att["id"])
    );

    // MIME sniffed when omitted, resent on later turns, and kept across fork.
    let s2 = create_session(&app, &dir).await;
    let t2 = start_turn(&app, &s2, json!({"prompt": {"text": "look", "attachments": [{"name": "photo.jpg", "data": b64_encode(JPEG)}]}})).await;
    wait_turn(&app, &s2, &t2).await;
    wait_not_busy(&app, &s2).await;
    let fork = id_str(
        &ok_post(&app, &format!("/v2/sessions/{s2}/fork"), json!({})).await["session"]["id"],
    );
    let t3 = start_turn(&app, &fork, json!({"prompt": {"text": "in fork"}})).await;
    wait_turn(&app, &fork, &t3).await;
    assert_eq!(req_media(&requests, 1), req_media(&requests, 2));
    assert_eq!(req_media(&requests, 1)[0].0, "image/jpeg");

    // Invalid attachments & non-vision models rejected before admission.
    let s3 = create_session(&app, &dir).await;
    let mut eleven = PNG.to_vec();
    eleven.resize(11 * 1024 * 1024, 1);
    for bad in [
        json!({"text": "t", "attachments": [{"name": "a.txt", "mime": "text/plain", "data": b64_encode(b"hello")}]}),
        json!({"text": "t", "attachments": [{"name": "a.svg", "mime": "image/svg+xml", "data": b64_encode(b"<svg/>")}]}),
        json!({"text": "t", "attachments": [{"name": "a.png", "mime": "image/png", "data": b64_encode(JPEG)}]}),
        json!({"text": "t", "attachments": [{"name": "a.bin", "data": b64_encode(b"not an image")}]}),
        json!({"text": "t", "attachments": [{"name": "empty.png", "mime": "image/png", "data": ""}]}),
        json!({"text": "t", "attachments": [{"name": "", "mime": "image/png", "data": b64_encode(PNG)}]}),
        json!({"text": "t", "attachments": [{"name": "huge.png", "mime": "image/png", "data": b64_encode(&eleven)}]}),
    ] {
        assert_err(
            post(
                &app,
                &format!("/v2/sessions/{s3}/turns"),
                json!({"prompt": bad}),
            )
            .await,
            StatusCode::BAD_REQUEST,
            "invalid_argument",
        );
    }
    assert!(
        ok_get(&app, &format!("/v2/sessions/{s3}/messages")).await["messages"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );

    let nv_requests = Arc::new(Mutex::new(Vec::new()));
    let nv_app = router(
        state_with_rules(
            RecordingVisionProvider {
                requests: Arc::clone(&nv_requests),
                image_input: Some(false),
            },
            PermissionRules::default(),
            dir.clone(),
        )
        .await
        .0,
    );
    let nv_s = create_session(&nv_app, &dir).await;
    assert_err(post(&nv_app, &format!("/v2/sessions/{nv_s}/turns"), json!({"prompt": {"text": "t", "attachments": [{"name": "a.png", "mime": "image/png", "data": b64_encode(PNG)}]}})).await, StatusCode::BAD_REQUEST, "invalid_argument");
    assert!(nv_requests.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn prompt_attachments_five_mib_image_matches_between_http_and_grpc() {
    let dir = support::tempdir("attach-grpc");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let state = state_with_rules(
        RecordingVisionProvider {
            requests: Arc::clone(&requests),
            image_input: None,
        },
        PermissionRules::default(),
        dir.clone(),
    )
    .await
    .0;
    let app = router(state.clone());
    let ch = grpc_channel(state).await;
    let mut turns = pb::turn_client::TurnClient::new(ch.clone())
        .max_encoding_message_size(hya_server::MAX_TURN_GRPC_MESSAGE_BYTES);
    let mut messages = pb::messages_client::MessagesClient::new(ch);

    let (grpc_s, http_s) = (
        create_session(&app, &dir).await,
        create_session(&app, &dir).await,
    );
    let mut image = PNG.to_vec();
    image.resize(5 * 1024 * 1024, 3);

    let created = turns
        .create_turn(pb::CreateTurnRequest {
            session: grpc_s.clone(),
            kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                text: "see".into(),
                attachments: vec![pb::PromptAttachment {
                    name: "pixel.png".into(),
                    mime: "image/png".into(),
                    data: image.clone(),
                    path: String::new(),
                }],
            })),
        })
        .await
        .unwrap()
        .into_inner();
    wait_turn(&app, &grpc_s, &created.turn.unwrap().id).await;
    let http_turn = start_turn(&app, &http_s, json!({"prompt": {"text": "see", "attachments": [{"name": "pixel.png", "mime": "image/png", "data": b64_encode(&image)}]}})).await;
    wait_turn(&app, &http_s, &http_turn).await;

    assert_eq!(req_media(&requests, 0), req_media(&requests, 1));
    let grpc_user = grpc_json(
        messages
            .list_messages(pb::ListMessagesRequest {
                session: grpc_s.clone(),
                ..Default::default()
            })
            .await,
    )["messages"][0]
        .clone();
    let http_user =
        ok_get(&app, &format!("/v2/sessions/{http_s}/messages")).await["messages"][0].clone();
    assert_eq!(
        attachment_parts(&grpc_user)[0]["attachment"],
        attachment_parts(&http_user)[0]["attachment"]
    );
    assert_eq!(
        grpc_code(
            turns
                .create_turn(pb::CreateTurnRequest {
                    session: grpc_s,
                    kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                        text: "t".into(),
                        attachments: vec![pb::PromptAttachment {
                            name: "a.txt".into(),
                            mime: "text/plain".into(),
                            data: b"hello".to_vec(),
                            path: String::new()
                        }],
                    })),
                })
                .await
        ),
        tonic::Code::InvalidArgument
    );
    let _ = std::fs::remove_dir_all(&dir);
}
