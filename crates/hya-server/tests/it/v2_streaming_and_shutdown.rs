//! Live event streaming, global session-list pushes, safe session deletion,
//! graceful stream shutdown (`serverStopping`), and status-bar stream/read
//! projections (token usage, todo list, manual compaction).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::http::StatusCode;
use futures::StreamExt;
use hya_api::v2 as pb;
use hya_engine::{
    AgentSpec, CompactionConfig, EventBus, ResidentSupervisor, SessionEngine, SummarizeOptions,
    Summarizer,
};
use hya_proto::{
    AgentName, Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId, TokenUsage,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_server::{AppState, ShutdownReason, V2Grpc, build, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule};
use serde_json::{Value, json};
use tokio::sync::{Notify, Semaphore};

use crate::harness::{
    GatedProvider, base_state, collect_sse, create_session, create_session_id, default_caps, del,
    get_json, grpc_channel, id_str, ok_get, ok_patch, ok_post, open_sse, run_prompt, serve_server,
    start_turn, state_of, state_with_provider, wait_not_busy, wait_turn,
};
use crate::support;

const CHUNKS: [&str; 5] = ["Hello", ", liv", "e str", "eamin", "g!"];

struct StreamingTextProvider {
    fail: Option<String>,
}

#[async_trait]
impl Provider for StreamingTextProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(default_caps())
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        if let Some(message) = &self.fail {
            return Err(ProviderError::HttpStatus {
                status: 400,
                message: message.clone(),
                retry_after: None,
            });
        }
        let part = PartId::new();
        let mut events = vec![Event::TextStart {
            session,
            message,
            part,
        }];
        events.extend(CHUNKS.iter().map(|c| Event::TextDelta {
            session,
            message,
            part,
            delta: (*c).into(),
        }));
        events.push(Event::TextEnd {
            session,
            message,
            part,
        });
        events.push(Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens: None,
            cause: None,
        });
        Ok(Box::pin(futures::stream::iter(events).then(
            |ev| async move {
                tokio::task::yield_now().await;
                Ok(ev)
            },
        )))
    }
}

async fn assistant_message(app: &axum::Router, session: &str) -> Value {
    ok_get(app, &format!("/v2/sessions/{session}/messages")).await["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == json!("ROLE_ASSISTANT"))
        .cloned()
        .expect("assistant")
}

fn events_for<'a>(frames: &'a [Value], assistant: &str) -> Vec<&'a Value> {
    frames
        .iter()
        .filter_map(|f| f.get("event"))
        .filter(|e| {
            e.as_object()
                .into_iter()
                .flat_map(|o| o.values())
                .any(|p| p.get("message") == Some(&json!(assistant)))
        })
        .collect()
}

#[tokio::test]
async fn live_assistant_deltas_stream_over_sse_and_grpc() {
    let state = state_with_provider(StreamingTextProvider { fail: None }).await;
    let app = router(state.clone());
    let session = create_session(&app, &std::env::temp_dir()).await;

    let mut grpc_stream = pb::events_client::EventsClient::new(grpc_channel(state).await)
        .stream_session_events(pb::StreamSessionEventsRequest {
            session: session.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let count = AtomicUsize::new(0);
    let collector = collect_sse(
        &app,
        &format!("/v2/sessions/{session}/events/stream"),
        move |f| {
            f["event"]["messageFinished"].is_object() && count.fetch_add(1, Ordering::SeqCst) == 1
        },
    )
    .await;

    let turn = start_turn(&app, &session, json!({"prompt": {"text": "stream please"}})).await;
    assert_eq!(
        wait_turn(&app, &session, &turn).await["state"],
        json!("TURN_STATE_FINISHED")
    );

    let frames = collector.await.unwrap();
    let assistant = assistant_message(&app, &session).await;
    let events = events_for(&frames, assistant["id"].as_str().unwrap());

    let finished_at = events
        .iter()
        .position(|e| e["messageFinished"].is_object())
        .unwrap();
    let appended: Vec<(usize, &Value)> = events
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, e)| e["partAppended"].is_object())
        .collect();
    assert_eq!(appended.len(), CHUNKS.len());
    assert!(
        appended
            .iter()
            .all(|(i, e)| *i < finished_at && e.get("seq").is_none())
    );
    assert_eq!(
        appended
            .iter()
            .map(|(_, e)| e["partAppended"]["textDelta"].as_str().unwrap())
            .collect::<String>(),
        CHUNKS.concat()
    );

    let durable_part = assistant["parts"][0]["id"].as_str().unwrap();
    assert!(
        appended
            .iter()
            .all(|(_, e)| e["partAppended"]["part"] == json!(durable_part))
    );
    let started: Vec<_> = events
        .iter()
        .filter(|e| e["partStarted"]["part"] == json!(durable_part))
        .collect();
    assert!(
        started.iter().any(|e| e.get("seq").is_none())
            && started.iter().any(|e| e.get("seq").is_some())
    );
    let replaced = events
        .iter()
        .find(|e| e["partReplaced"]["part"] == json!(durable_part))
        .unwrap();
    assert_eq!(
        (
            &replaced["partReplaced"]["text"],
            replaced.get("seq").is_some()
        ),
        (&json!(CHUNKS.concat()), true)
    );
    assert_eq!(
        assistant["parts"][0]["text"]["text"],
        json!(CHUNKS.concat())
    );

    // gRPC stream receives the same live deltas before assistant finish.
    use pb::stream_event::Payload as P;
    let (mut deltas, mut finishes) = (String::new(), 0);
    tokio::time::timeout(Duration::from_secs(5), async {
        while finishes < 2 {
            let Some(pb::stream_frame::Frame::Event(ev)) =
                grpc_stream.next().await.unwrap().unwrap().frame
            else {
                continue;
            };
            match ev.payload {
                Some(P::PartAppended(a)) if ev.seq == 0 => {
                    assert_eq!(finishes, 1);
                    deltas.push_str(&a.text_delta);
                }
                Some(P::MessageFinished(_)) => finishes += 1,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(deltas, CHUNKS.concat());
}

#[tokio::test]
async fn provider_failure_exposes_error_on_turn_message_stream_and_replay() {
    let app = router(
        state_with_provider(StreamingTextProvider {
            fail: Some("invalid request: prompt rejected by upstream".into()),
        })
        .await,
    );
    let session = create_session(&app, &std::env::temp_dir()).await;
    let collector = collect_sse(
        &app,
        &format!("/v2/sessions/{session}/events/stream"),
        |f| f["event"]["messageFinished"]["finish"] == json!("FINISH_REASON_ERROR"),
    )
    .await;

    let turn = start_turn(&app, &session, json!({"prompt": {"text": "fail please"}})).await;
    let waited = wait_turn(&app, &session, &turn).await;
    assert_eq!(
        (&waited["state"], &waited["errorCode"]),
        (&json!("TURN_STATE_FAILED"), &json!("provider_error"))
    );
    assert!(
        waited["errorMessage"]
            .as_str()
            .is_some_and(|t| t.contains("prompt rejected by upstream"))
    );
    assert_eq!(
        ok_get(&app, &format!("/v2/sessions/{session}/turns/{turn}")).await["errorMessage"],
        waited["errorMessage"]
    );

    let assistant = assistant_message(&app, &session).await;
    assert_eq!(
        (
            &assistant["finish"],
            &assistant["error"]["code"],
            &assistant["error"]["message"]
        ),
        (
            &json!("FINISH_REASON_ERROR"),
            &json!("provider_error"),
            &waited["errorMessage"]
        )
    );

    let frames = collector.await.unwrap();
    let assistant_id = assistant["id"].as_str().unwrap();
    let events = events_for(&frames, assistant_id);
    let reported = events
        .iter()
        .position(|e| e["errorReported"].is_object())
        .unwrap();
    let finished = events
        .iter()
        .position(|e| e["messageFinished"].is_object())
        .unwrap();
    assert!(
        reported < finished
            && events[reported]["errorReported"]["errorMessage"] == waited["errorMessage"]
    );
    assert!(
        ok_get(&app, &format!("/v2/sessions/{session}/events")).await["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["errorReported"]["message"] == json!(assistant_id))
    );
}

fn summary(frame: &Value) -> (String, String) {
    if frame.get("resync").is_some() {
        return (String::new(), "resync".into());
    }
    let event = &frame["event"];
    let session = event["session"].as_str().unwrap_or_default().to_owned();
    let key = event
        .as_object()
        .unwrap()
        .keys()
        .find(|k| !matches!(k.as_str(), "seq" | "session" | "timeRecorded"))
        .cloned()
        .unwrap_or_default();
    let kind = if key == "sessionUpdated" {
        let mut parts: Vec<String> = event["sessionUpdated"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(n, v)| match v {
                Value::Bool(b) => format!("{n}={b}"),
                _ => n.clone(),
            })
            .collect();
        parts.sort();
        format!("sessionUpdated.{}", parts.join(","))
    } else {
        key
    };
    (session, kind)
}

#[tokio::test]
async fn global_stream_pushes_root_session_list_and_hides_children_over_sse_and_grpc() {
    let (gate, entered) = (Arc::new(Semaphore::new(0)), Arc::new(Notify::new()));
    let state = state_with_provider(GatedProvider::with_notify(
        Arc::clone(&gate),
        Arc::clone(&entered),
    ))
    .await;
    let app = router(state.clone());
    let ch = grpc_channel(state).await;
    let mut grpc_events = pb::events_client::EventsClient::new(ch.clone())
        .stream_global_events(pb::StreamGlobalEventsRequest {
            interactions_only: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let mut grpc_sessions = pb::session_client::SessionClient::new(ch);

    let filtered = collect_sse(&app, "/v2/events/stream?interactionsOnly=true", |f| {
        f["event"]["sessionDeleted"].is_object()
    })
    .await;
    let unfiltered = collect_sse(&app, "/v2/events/stream", |f| {
        f["event"]["sessionDeleted"].is_object()
    })
    .await;

    let root = create_session(&app, &std::env::temp_dir()).await;
    let child = create_session_id(
        &app,
        json!({"workdir": std::env::temp_dir().to_string_lossy(), "parent": root}),
    )
    .await;
    for (id, p) in [
        (&root, json!({"title": "Renamed"})),
        (&child, json!({"title": "Child renamed"})),
        (&root, json!({"archived": true})),
        (&root, json!({"archived": false})),
    ] {
        let _ = ok_patch(&app, &format!("/v2/sessions/{id}"), p).await;
    }
    for id in [&child, &root] {
        let notified = entered.notified();
        let turn = start_turn(&app, id, json!({"prompt": {"text": "hi"}})).await;
        tokio::time::timeout(Duration::from_secs(5), notified)
            .await
            .unwrap();
        gate.add_permits(1);
        wait_turn(&app, id, &turn).await;
        wait_not_busy(&app, id).await;
    }
    grpc_sessions
        .delete_session(pb::DeleteSessionRequest {
            session: child.clone(),
        })
        .await
        .unwrap();
    grpc_sessions
        .delete_session(pb::DeleteSessionRequest {
            session: root.clone(),
        })
        .await
        .unwrap();

    // Verify interactionsOnly SSE stream.
    let filtered = filtered.await.unwrap();
    assert!(
        filtered
            .iter()
            .any(|f| f["event"]["projectsUpdated"].is_object())
    );
    let list_frames: Vec<Value> = filtered
        .into_iter()
        .filter(|f| !f["event"]["projectsUpdated"].is_object())
        .collect();
    let rows: Vec<(String, String)> = list_frames.iter().map(summary).collect();
    assert!(rows.iter().all(|(s, k)| *s != child
        && (k.starts_with("sessionStarted")
            || k.starts_with("sessionUpdated")
            || k == "sessionDeleted")));
    let root_kinds: Vec<&str> = rows
        .iter()
        .filter(|(s, _)| *s == root)
        .map(|(_, k)| k.as_str())
        .collect();
    let expected = [
        "sessionStarted",
        "sessionUpdated.title",
        "sessionUpdated.archived=true",
        "sessionUpdated.archived=false",
        "sessionUpdated.busy=true",
        "sessionUpdated.busy=false",
        "sessionDeleted",
    ];
    assert_eq!(root_kinds, expected);
    for frame in &list_frames {
        let ev = &frame["event"];
        let live = ev["sessionDeleted"].is_object() || ev["sessionUpdated"].get("busy").is_some();
        assert_eq!(ev.get("seq").is_none(), live, "{frame}");
    }

    // Verify unfiltered SSE stream has root busy/deleted, no child list frames, and engine traffic.
    let all_rows: Vec<(String, String)> = unfiltered.await.unwrap().iter().map(summary).collect();
    let busy_del = |s: &str| -> Vec<&str> {
        all_rows
            .iter()
            .filter(|(o, k)| {
                o == s && (k.starts_with("sessionUpdated.busy") || k == "sessionDeleted")
            })
            .map(|(_, k)| k.as_str())
            .collect()
    };
    assert_eq!(
        busy_del(&root),
        [
            "sessionUpdated.busy=true",
            "sessionUpdated.busy=false",
            "sessionDeleted"
        ]
    );
    assert!(
        busy_del(&child).is_empty()
            && all_rows
                .iter()
                .any(|(s, k)| *s == root && k == "messageStarted")
    );

    // Verify gRPC interactionsOnly stream matches.
    let mut grpc_rows = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = grpc_events.next().await {
            let Some(pb::stream_frame::Frame::Event(ev)) = frame.unwrap().frame else {
                continue;
            };
            let kind = match &ev.payload {
                Some(pb::stream_event::Payload::SessionStarted(_)) => "sessionStarted".into(),
                Some(pb::stream_event::Payload::SessionUpdated(u)) => {
                    if let Some(b) = u.busy {
                        assert_eq!(ev.seq, 0);
                        format!("sessionUpdated.busy={b}")
                    } else if let Some(a) = u.archived {
                        format!("sessionUpdated.archived={a}")
                    } else if u.title.is_some() {
                        "sessionUpdated.title".into()
                    } else {
                        "sessionUpdated.other".into()
                    }
                }
                Some(pb::stream_event::Payload::SessionDeleted(_)) => {
                    assert_eq!(ev.seq, 0);
                    "sessionDeleted".into()
                }
                Some(pb::stream_event::Payload::ProjectsUpdated(_)) => continue,
                _ => "other".into(),
            };
            let done = kind == "sessionDeleted";
            grpc_rows.push((ev.session, kind));
            if done {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        grpc_rows,
        expected
            .iter()
            .map(|k| (root.clone(), (*k).to_owned()))
            .collect::<Vec<_>>()
    );
}

async fn public_delete(state: AppState, session: SessionId, grpc: bool) -> Result<(), String> {
    if grpc {
        pb::session_server::Session::delete_session(
            &V2Grpc::new(state),
            tonic::Request::new(pb::DeleteSessionRequest {
                session: session.to_string(),
            }),
        )
        .await
        .map(|_| ())
        .map_err(|e| {
            assert_eq!(e.code(), tonic::Code::InvalidArgument);
            e.message().to_owned()
        })
    } else {
        let (s, body) = del(&router(state), &format!("/v2/sessions/{session}")).await;
        if s == StatusCode::OK {
            Ok(())
        } else {
            assert_eq!(
                (s, &body["error"]["code"]),
                (StatusCode::BAD_REQUEST, &json!("invalid_argument"))
            );
            Err(id_str(&body["error"]["message"]))
        }
    }
}

async fn spawn_member(
    engine: &SessionEngine,
    supervisor: &ResidentSupervisor,
    parent: SessionId,
) -> (SessionId, String) {
    let agent = AgentSpec {
        name: AgentName::new("hya-scout"),
        model: ModelRef::new("fake"),
        workdir: ".".into(),
        system_prompt: String::new(),
        reasoning: None,
    };
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let (child, handle) = supervisor
        .spawn_resident(
            parent,
            agent,
            (binding, Arc::from([]), resources, None),
            "work".into(),
            None,
            None,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !engine.turn_active(child) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    (child, handle)
}

#[tokio::test]
async fn delete_session_enforces_team_ownership_and_joins_live_episodes() {
    for (grpc, archived) in [(false, false), (true, true)] {
        let gate = Arc::new(Semaphore::new(0));
        let state = state_with_provider(GatedProvider::new(Arc::clone(&gate))).await;
        let engine = state.engine.clone();
        let supervisor = ResidentSupervisor::start(engine.clone());
        let root: SessionId = create_session(&router(state.clone()), &std::env::temp_dir())
            .await
            .parse()
            .unwrap();
        let (child, handle) = spawn_member(&engine, &supervisor, root).await;
        let (grandchild, _) = spawn_member(&engine, &supervisor, child).await;
        if archived {
            engine.archive_subagent(child).await.unwrap();
        }

        let err = public_delete(state.clone(), child, grpc).await.unwrap_err();
        assert!(
            err.contains("archive the subagent or delete the root"),
            "{err}"
        );
        assert!(
            engine.store().session_exists(child).await.unwrap()
                && engine.store().session_exists(grandchild).await.unwrap()
        );
        let proj = engine.read_projection(root).await.unwrap();
        let team = hya_tool::extension::team_projection(&proj);
        if archived {
            assert_eq!(
                (supervisor.episode_count(), team.archived[&handle].session),
                (0, child)
            );
        } else {
            assert_eq!(
                (supervisor.episode_count(), team.roster[&handle].session),
                (2, child)
            );
            assert!(engine.turn_active(child) && engine.turn_active(grandchild));
        }

        public_delete(state, root, grpc).await.unwrap();
        assert_eq!(supervisor.episode_count(), 0);
        use hya_engine::SessionStoreExt as _;
        assert!(
            supervisor.team_cancel(root).is_none()
                && !engine.turn_active(child)
                && engine.store().active_actor_ids().await.unwrap().is_empty()
        );
        gate.add_permits(10);
        supervisor
            .drain(hya_proto::FinishCause::UserCancel, Duration::from_secs(5))
            .await;
        for s in [root, child, grandchild] {
            assert!(
                !engine.store().session_exists(s).await.unwrap()
                    && engine.replay(s).await.unwrap().is_empty()
            );
        }
    }
}

#[tokio::test]
async fn delete_session_settles_inflight_root_turn_and_auto_title() {
    for (grpc, title) in [(false, true), (true, false)] {
        let (gate, entered) = (Arc::new(Semaphore::new(0)), Arc::new(Semaphore::new(0)));
        let state = state_with_provider(GatedProvider::with_entered(
            Arc::clone(&gate),
            Arc::clone(&entered),
        ))
        .await
        .with_auto_title(title);
        let engine = state.engine.clone();
        let supervisor = ResidentSupervisor::start(engine.clone());
        let app = router(state.clone());
        let root: SessionId = create_session(&app, &std::env::temp_dir())
            .await
            .parse()
            .unwrap();
        let _ = start_turn(&app, &root.to_string(), json!({"prompt": {"text": "hi"}})).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !engine.turn_active(root) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if title {
            tokio::time::timeout(Duration::from_secs(5), entered.acquire_many(2))
                .await
                .unwrap()
                .unwrap()
                .forget();
        }
        public_delete(state, root, grpc).await.unwrap();
        assert!(!engine.turn_active(root));
        gate.add_permits(10);
        supervisor
            .drain(hya_proto::FinishCause::UserCancel, Duration::from_secs(5))
            .await;
        assert!(
            !engine.store().session_exists(root).await.unwrap()
                && engine.replay(root).await.unwrap().is_empty()
        );
    }
}

async fn sse_frames_to_end(stream: &mut axum::body::BodyDataStream) -> Vec<Value> {
    let mut text = String::new();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(bytes)) = stream.next().await {
                text.push_str(&String::from_utf8_lossy(&bytes));
            }
        })
        .await
        .is_ok(),
        "SSE stream did not end: {text}"
    );
    text.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str(d.trim()).ok())
        .collect()
}

fn assert_stopping_last(frames: &[Value], reason: &str, label: &str) {
    let last = frames.last().unwrap_or_else(|| panic!("{label}: no frame"));
    assert_eq!(last["event"]["serverStopping"], json!({"reason": reason}));
    assert!(last["event"].get("seq").is_none() && last["event"].get("session").is_none());
    assert_eq!(
        frames
            .iter()
            .filter(|f| f["event"]["serverStopping"].is_object())
            .count(),
        1
    );
}

#[tokio::test]
async fn shutdown_closes_http_and_grpc_streams_with_reason_and_marks_health_unavailable() {
    let state = base_state().await;
    let shutdown = state.streams();
    let app = router(state.clone());
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let streams = shutdown.clone();
    let (addr, server_task) = serve_server(&build(state), async move {
        let _ = stop_rx.await;
        streams.close(ShutdownReason::Stop);
    })
    .await;

    let _ = ok_get(&app, "/v2/health").await;
    let session = create_session(&app, &std::env::temp_dir()).await;
    let mut global = open_sse(&app, "/v2/events/stream").await;
    let mut asks = open_sse(&app, "/v2/events/stream?interactionsOnly=true").await;
    let mut own = open_sse(&app, &format!("/v2/sessions/{session}/events/stream")).await;

    let ch = tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = pb::events_client::EventsClient::new(ch);
    let grpc_global = client
        .stream_global_events(pb::StreamGlobalEventsRequest {
            interactions_only: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let grpc_own = client
        .stream_session_events(pb::StreamSessionEventsRequest {
            session,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    assert!(
        tokio::time::timeout(Duration::from_millis(25), global.next())
            .await
            .is_err()
    );
    stop_tx.send(()).unwrap();

    for (label, s) in [
        ("global", &mut global),
        ("interactionsOnly", &mut asks),
        ("session", &mut own),
    ] {
        assert_stopping_last(&sse_frames_to_end(s).await, "stop", label);
    }
    assert_eq!(shutdown.reason(), Some(ShutdownReason::Stop));

    let (status, health) = get_json(&app, "/v2/health").await;
    assert_eq!(
        (status, &health["error"]["code"]),
        (StatusCode::SERVICE_UNAVAILABLE, &json!("unavailable"))
    );
    assert_stopping_last(
        &sse_frames_to_end(&mut open_sse(&app, "/v2/events/stream").await).await,
        "stop",
        "late",
    );

    for (label, mut stream) in [("grpc-global", grpc_global), ("grpc-session", grpc_own)] {
        let mut last = None;
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(frame)) = stream.next().await {
                if let Some(pb::stream_frame::Frame::Event(ev)) = frame.frame {
                    last = ev.payload;
                }
            }
        })
        .await
        .unwrap();
        match last {
            Some(pb::stream_event::Payload::ServerStopping(s)) => {
                assert_eq!(s.reason, "stop", "{label}")
            }
            other => panic!("{label}: last payload {other:?}"),
        }
    }
    tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .expect("graceful shutdown")
        .unwrap();

    // Restart & Signal reasons surface on late streams without full TCP setup.
    for (reason, name) in [
        (ShutdownReason::Restart, "restart"),
        (ShutdownReason::Signal, "signal"),
    ] {
        let s = base_state().await;
        s.streams().close(reason);
        let mut late = open_sse(&router(s), "/v2/events/stream").await;
        assert_stopping_last(&sse_frames_to_end(&mut late).await, name, name);
    }
}

struct FixedSummary;
#[async_trait]
impl Summarizer for FixedSummary {
    async fn summarize(
        &self,
        _: &[hya_proto::Message],
        _: SummarizeOptions,
    ) -> Result<String, hya_engine::CoreError> {
        Ok("SUMMARY OF EVERYTHING".into())
    }
}

async fn status_state(provider: FakeProvider, workdir: std::path::PathBuf) -> AppState {
    let (perm, _) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "*",
        Mode::Allow,
    )]));
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            support::test_runtime(Arc::new(hya_tool_loader::builtins())),
            perm,
            EventBus::default(),
        )
        .with_compaction(Arc::new(FixedSummary), CompactionConfig::default()),
    );
    state_of(engine, workdir)
}

fn second_stop() -> impl Fn(&Value) -> bool + Send + 'static {
    let stops = AtomicUsize::new(0);
    move |f| {
        f["event"]["messageFinished"]["finish"] == json!("FINISH_REASON_STOP")
            && stops.fetch_add(1, Ordering::SeqCst) == 1
    }
}

fn of_kind(events: impl IntoIterator<Item = Value>, kind: &str) -> Vec<Value> {
    events
        .into_iter()
        .filter_map(|e| e.get(kind).cloned())
        .collect()
}

fn frame_events(frames: &[Value]) -> Vec<Value> {
    frames
        .iter()
        .filter_map(|f| f.get("event").cloned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn message_and_session_usage_fold_from_recorded_rounds() {
    let dir = support::tempdir("status-usage");
    std::fs::write(dir.join("a.txt"), "a").unwrap();
    let usage = |input, cache_read, cache_write, output| TokenUsage {
        input,
        output,
        reasoning: 0,
        cache_read,
        cache_write,
        reasoning_unknown: true,
    };
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".into(),
                input: json!({"path": dir.join("a.txt")}),
            },
            FakeStep::Usage(usage(100, 1000, 50, 20)),
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".into()),
            FakeStep::Usage(usage(30, 1150, 0, 10)),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let state = status_state(provider, dir.clone()).await;
    let app = router(state.clone());
    let session = create_session(&app, &dir).await;
    let collector = collect_sse(
        &app,
        &format!("/v2/sessions/{session}/events/stream"),
        second_stop(),
    )
    .await;
    run_prompt(&app, &session, "read it").await;

    let assistant = assistant_message(&app, &session).await;
    assert_eq!(
        assistant["usage"],
        json!({"input": "130", "output": "30", "cacheRead": "2150", "cacheWrite": "50", "reasoningUnknown": true})
    );
    assert_eq!(
        assistant["roundUsage"],
        json!({"input": "30", "output": "10", "cacheRead": "1150", "reasoningUnknown": true})
    );
    assert_eq!(
        ok_get(&app, &format!("/v2/sessions/{session}")).await["usage"],
        assistant["usage"]
    );
    let message_id = id_str(&assistant["id"]);

    let expected = vec![
        json!({"message": message_id, "model": "fake", "usage": {"input": "100", "output": "20", "cacheRead": "1000", "cacheWrite": "50", "reasoningUnknown": true}}),
        json!({"message": message_id, "model": "fake", "usage": {"input": "30", "output": "10", "cacheRead": "1150", "reasoningUnknown": true}}),
    ];
    assert_eq!(
        of_kind(frame_events(&collector.await.unwrap()), "tokensRecorded"),
        expected
    );
    assert_eq!(
        of_kind(
            ok_get(&app, &format!("/v2/sessions/{session}/events")).await["events"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            "tokensRecorded"
        ),
        expected
    );

    let msgs = pb::messages_client::MessagesClient::new(grpc_channel(state).await)
        .list_messages(pb::ListMessagesRequest {
            session,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let round = msgs
        .messages
        .iter()
        .find(|m| m.id == message_id)
        .unwrap()
        .round_usage
        .unwrap();
    assert_eq!(
        (round.input, round.cache_read, round.cache_write),
        (30, 1150, 0)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn todo_tool_calls_stream_and_fold_into_todo_read() {
    let dir = support::tempdir("status-todo");
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "todo".into(),
                input: json!({"ops": [{"op": "add", "content": "first"}, {"op": "add", "content": "second"}]}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::ToolCall {
                name: "todo".into(),
                input: json!({"ops": [{"op": "status", "id": "1", "status": "in_progress"}]}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::ToolCall {
                name: "todo".into(),
                input: json!({"ops": []}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("planned".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let state = status_state(provider, dir.clone()).await;
    let app = router(state.clone());
    let session = create_session(&app, &dir).await;
    let collector = collect_sse(
        &app,
        &format!("/v2/sessions/{session}/events/stream"),
        second_stop(),
    )
    .await;
    run_prompt(&app, &session, "plan it").await;

    let added = json!({"items": [{"id": "1", "content": "first", "status": "TODO_STATUS_PENDING"}, {"id": "2", "content": "second", "status": "TODO_STATUS_PENDING"}]});
    let started = json!({"items": [{"id": "1", "content": "first", "status": "TODO_STATUS_IN_PROGRESS"}, {"id": "2", "content": "second", "status": "TODO_STATUS_PENDING"}]});
    assert_eq!(
        of_kind(frame_events(&collector.await.unwrap()), "todoUpdated"),
        vec![added.clone(), started.clone()]
    );
    assert_eq!(
        of_kind(
            ok_get(&app, &format!("/v2/sessions/{session}/events")).await["events"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            "todoUpdated"
        ),
        vec![added, started.clone()]
    );
    assert_eq!(
        ok_get(&app, &format!("/v2/sessions/{session}/todo")).await,
        started
    );

    let listed = pb::messages_client::MessagesClient::new(grpc_channel(state).await)
        .get_session_todo(pb::GetSessionTodoRequest { session })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        (listed.items.len(), listed.items[0].status),
        (2, pb::TodoStatus::InProgress as i32)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_compaction_streams_and_records_over_http_and_grpc() {
    let dir = support::tempdir("status-compact");
    let provider = FakeProvider::scripted_turns(
        ["one", "two", "three"]
            .into_iter()
            .map(|t| {
                vec![
                    FakeStep::Text(t.into()),
                    FakeStep::Finish(FinishReason::Stop),
                ]
            })
            .collect(),
    );
    let state = status_state(provider, dir.clone()).await;
    let app = router(state.clone());
    let session = create_session(&app, &dir).await;
    run_prompt(&app, &session, "first").await;
    run_prompt(&app, &session, "second").await;

    let collector = collect_sse(
        &app,
        &format!("/v2/sessions/{session}/events/stream"),
        |f| f["event"]["compactionApplied"].is_object(),
    )
    .await;
    assert_eq!(
        ok_post(&app, &format!("/v2/sessions/{session}/compact"), json!({})).await["strategy"],
        json!("rewrite")
    );

    let applied = of_kind(frame_events(&collector.await.unwrap()), "compactionApplied");
    assert_eq!(applied.len(), 1);
    assert_eq!(
        (
            &applied[0]["strategy"],
            &applied[0]["manual"],
            &applied[0]["foldedCount"]
        ),
        (&json!("rewrite"), &json!(true), &json!(3))
    );
    let summary_msg = ok_get(&app, &format!("/v2/sessions/{session}/messages")).await["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == json!("ROLE_SYSTEM"))
        .cloned()
        .unwrap();
    assert_eq!(applied[0]["message"], summary_msg["id"]);
    assert_eq!(
        of_kind(
            ok_get(&app, &format!("/v2/sessions/{session}/events")).await["events"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            "compactionApplied"
        ),
        applied
    );

    // gRPC CompactSession reports the same record.
    run_prompt(&app, &session, "third").await;
    let ch = grpc_channel(state).await;
    let mut events = pb::events_client::EventsClient::new(ch.clone())
        .stream_session_events(pb::StreamSessionEventsRequest {
            session: session.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    pb::session_client::SessionClient::new(ch)
        .compact_session(pb::CompactSessionRequest {
            session,
            ..Default::default()
        })
        .await
        .unwrap();
    let applied = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = events.next().await {
            if let Some(pb::stream_frame::Frame::Event(e)) = frame.unwrap().frame
                && let Some(pb::stream_event::Payload::CompactionApplied(a)) = e.payload
            {
                return a;
            }
        }
        panic!("stream ended")
    })
    .await
    .unwrap();
    assert!(applied.manual && applied.strategy == "rewrite");
    let _ = std::fs::remove_dir_all(&dir);
}
