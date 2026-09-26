//! v1 status data a client renders outside the transcript: each model's
//! context/output limits, per-message and per-session usage (with the live
//! `tokensRecorded` frame), the todo list as a projection with its live
//! `todoUpdated` frame, and a manual compaction's `compactionApplied`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{
    AgentSpec, CompactionConfig, EventBus, SessionEngine, SummarizeOptions, Summarizer,
};
use hya_proto::{AgentName, FinishReason, ModelRef, TokenUsage};
use hya_provider::{
    Capabilities, FakeProvider, FakeStep, ModelCatalogSource, ProviderCatalogSnapshot,
    ProviderModel, ProviderRouter,
};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

fn tempdir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-v1-status-{tag}-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct FixedSummary;

#[async_trait]
impl Summarizer for FixedSummary {
    async fn summarize(
        &self,
        _messages: &[hya_proto::Message],
        _options: SummarizeOptions,
    ) -> Result<String, hya_core::CoreError> {
        Ok("SUMMARY OF EVERYTHING".to_string())
    }
}

async fn state_with(provider: FakeProvider, workdir: PathBuf) -> (AppState, Arc<SessionEngine>) {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let (perm, _asks) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Read, "*", Mode::Allow),
        Rule::new(Action::TodoWrite, "*", Mode::Allow),
    ]));
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            providers,
            support::test_runtime(Arc::new(ToolRegistry::builtins())),
            perm,
            EventBus::default(),
        )
        .with_compaction(Arc::new(FixedSummary), CompactionConfig::default()),
    );
    let state = AppState::new(
        Arc::clone(&engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir,
            reasoning: None,
        }),
    );
    (state, engine)
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn create_session(app: &axum::Router, workdir: &std::path::Path) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/sessions",
        json!({"agent": "build", "model": "fake", "workdir": workdir.to_string_lossy()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session"]["id"].as_str().unwrap().to_owned()
}

/// Collect SSE frames until `done` matches one (inclusive) or 10 s pass.
async fn sse_frames_until(
    app: axum::Router,
    uri: String,
    done: impl Fn(&Value) -> bool + Send + 'static,
) -> Vec<Value> {
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    let mut frames = Vec::new();
    let mut buffer = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let chunk = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
        let Ok(Some(Ok(bytes))) = chunk else {
            continue;
        };
        buffer.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(end) = buffer.find("\n\n") {
            let record: String = buffer.drain(..end + 2).collect();
            for line in record.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(frame) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                let finished = done(&frame);
                frames.push(frame);
                if finished {
                    return frames;
                }
            }
        }
    }
    frames
}

/// Done predicate: the second `FINISH_REASON_STOP` (the user prompt's
/// message finishes first, then the assistant's).
fn second_stop() -> impl Fn(&Value) -> bool + Send + 'static {
    let stops = std::sync::atomic::AtomicUsize::new(0);
    move |frame| {
        frame["event"]["messageFinished"]["finish"] == json!("FINISH_REASON_STOP")
            && stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
    }
}

async fn run_turn(app: &axum::Router, session: &str, text: &str) {
    let (status, created) = call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({"prompt": {"text": text}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let turn = created["turn"]["id"].as_str().unwrap().to_owned();
    let (status, waited) = call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{waited}");
    assert_eq!(waited["state"], json!("TURN_STATE_FINISHED"), "{waited}");
}

/// Payload objects of one kind across frames (or `ListEvents` rows).
fn of_kind(events: impl IntoIterator<Item = Value>, kind: &str) -> Vec<Value> {
    events
        .into_iter()
        .filter_map(|event| event.get(kind).cloned())
        .collect()
}

fn frame_events(frames: &[Value]) -> Vec<Value> {
    frames
        .iter()
        .filter_map(|frame| frame.get("event").cloned())
        .collect()
}

async fn list_events(app: &axum::Router, session: &str) -> Vec<Value> {
    let (status, body) = call(
        app,
        Method::GET,
        &format!("/v1/sessions/{session}/events"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["events"].as_array().cloned().unwrap_or_default()
}

async fn grpc_channel(state: AppState) -> tonic::transport::Channel {
    let grpc = V1Grpc::new(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serve = tonic::transport::Server::builder()
        .add_service(pb::catalog_server::CatalogServer::new(grpc.clone()))
        .add_service(pb::session_server::SessionServer::new(grpc.clone()))
        .add_service(pb::messages_server::MessagesServer::new(grpc.clone()))
        .add_service(pb::events_server::EventsServer::new(grpc))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(async move {
        let _ = serve.await;
    });
    tonic::transport::Channel::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn model_summaries_carry_context_and_output_limits() {
    let dir = tempdir("limits");
    let (state, engine) = state_with(FakeProvider::scripted(vec![]), dir.clone()).await;
    let row = |model: &str, context: u32, output: u32| ProviderModel {
        provider_id: "acme".to_string(),
        model_id: model.to_string(),
        capabilities: Capabilities {
            max_context: context,
            max_output: output,
            ..Capabilities::default()
        },
        reasoning_variants: Vec::new(),
        reasoning_default: None,
        reasoning: None,
        display_name: None,
        source: ModelCatalogSource::Configured,
    };
    engine.publish_provider_catalog(
        engine.provider_router(),
        Arc::new(ProviderCatalogSnapshot::build(
            [row("big", 400_000, 128_000), row("small", 32_000, 0)],
            [],
            None,
        )),
    );
    let app = router(state.clone());
    let (status, body) = call(&app, Method::GET, "/v1/models", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let models = body["models"].as_array().unwrap();
    let big = models
        .iter()
        .find(|model| model["id"] == "acme/big")
        .unwrap();
    assert_eq!(big["contextLimit"], json!("400000"), "{big}");
    assert_eq!(big["outputLimit"], json!("128000"), "{big}");
    let small = models
        .iter()
        .find(|model| model["id"] == "acme/small")
        .unwrap();
    assert_eq!(small["contextLimit"], json!("32000"), "{small}");
    assert!(small.get("outputLimit").is_none(), "unknown is 0: {small}");

    let listed = pb::catalog_client::CatalogClient::new(grpc_channel(state).await)
        .list_models(pb::ListModelsRequest::default())
        .await
        .unwrap()
        .into_inner();
    let big = listed
        .models
        .iter()
        .find(|model| model.id == "acme/big")
        .unwrap();
    assert_eq!((big.context_limit, big.output_limit), (400_000, 128_000));
    let _ = std::fs::remove_dir_all(&dir);
}

fn usage(input: u64, cache_read: u64, cache_write: u64, output: u64) -> TokenUsage {
    TokenUsage {
        input,
        output,
        reasoning: 0,
        cache_read,
        cache_write,
        reasoning_unknown: true,
    }
}

/// A two-round turn with provider usage: the message carries the sum and
/// its latest round, the session the total, and each round streams (and
/// replays) as `tokensRecorded` with its message and serving model.
#[tokio::test(flavor = "multi_thread")]
async fn message_and_session_usage_fold_from_recorded_rounds() {
    let dir = tempdir("usage");
    std::fs::write(dir.join("a.txt"), "a").unwrap();
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: json!({"path": dir.join("a.txt")}),
            },
            FakeStep::Usage(usage(100, 1000, 50, 20)),
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".to_string()),
            FakeStep::Usage(usage(30, 1150, 0, 10)),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let (state, _engine) = state_with(provider, dir.clone()).await;
    let app = router(state.clone());
    let session = create_session(&app, &dir).await;
    let collector = tokio::spawn(sse_frames_until(
        app.clone(),
        format!("/v1/sessions/{session}/events/stream"),
        second_stop(),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    run_turn(&app, &session, "read it").await;

    let (status, listed) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{session}/messages"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let assistant = listed["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == json!("ROLE_ASSISTANT"))
        .cloned()
        .unwrap();
    assert_eq!(
        assistant["usage"],
        json!({"input": "130", "output": "30", "cacheRead": "2150", "cacheWrite": "50", "reasoningUnknown": true}),
        "{assistant:#}"
    );
    assert_eq!(
        assistant["roundUsage"],
        json!({"input": "30", "output": "10", "cacheRead": "1150", "reasoningUnknown": true}),
        "{assistant:#}"
    );
    assert_eq!(assistant["model"], json!("fake"));
    let message_id = assistant["id"].as_str().unwrap().to_owned();

    let (status, info) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{session}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    assert_eq!(
        info["usage"],
        json!({"input": "130", "output": "30", "cacheRead": "2150", "cacheWrite": "50", "reasoningUnknown": true}),
        "{info:#}"
    );

    let expected_rounds = vec![
        json!({"message": message_id, "model": "fake", "usage": {"input": "100", "output": "20", "cacheRead": "1000", "cacheWrite": "50", "reasoningUnknown": true}}),
        json!({"message": message_id, "model": "fake", "usage": {"input": "30", "output": "10", "cacheRead": "1150", "reasoningUnknown": true}}),
    ];
    let frames = collector.await.unwrap();
    assert_eq!(
        of_kind(frame_events(&frames), "tokensRecorded"),
        expected_rounds,
        "{frames:#?}"
    );
    assert_eq!(
        of_kind(list_events(&app, &session).await, "tokensRecorded"),
        expected_rounds
    );

    // gRPC parity: the same message usage on ListMessages.
    let channel = grpc_channel(state).await;
    let messages = pb::messages_client::MessagesClient::new(channel)
        .list_messages(pb::ListMessagesRequest {
            session: session.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let assistant = messages
        .messages
        .iter()
        .find(|message| message.id == message_id)
        .unwrap();
    let round = assistant.round_usage.unwrap();
    assert_eq!(
        (round.input, round.cache_read, round.cache_write),
        (30, 1150, 0)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A todo tool call streams `todoUpdated` with the full list, records it
/// durably (replay), and `GetSessionTodo` reads the same list.
#[tokio::test(flavor = "multi_thread")]
async fn todo_tool_calls_stream_todo_updated_and_fold_into_the_todo_read() {
    let dir = tempdir("todo");
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "todo__update_content".to_string(),
                input: json!({"operations": [
                    {"op": "add", "content": "first"},
                    {"op": "add", "content": "second"}
                ]}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::ToolCall {
                name: "todo__update_status".to_string(),
                input: json!({"updates": [{"id": "1", "status": "in_progress"}]}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::ToolCall {
                name: "todo__read".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("planned".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let (state, _engine) = state_with(provider, dir.clone()).await;
    let app = router(state.clone());
    let session = create_session(&app, &dir).await;
    let collector = tokio::spawn(sse_frames_until(
        app.clone(),
        format!("/v1/sessions/{session}/events/stream"),
        second_stop(),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    run_turn(&app, &session, "plan it").await;

    let added = json!({"items": [
        {"id": "1", "content": "first", "status": "TODO_STATUS_PENDING"},
        {"id": "2", "content": "second", "status": "TODO_STATUS_PENDING"},
    ]});
    let started = json!({"items": [
        {"id": "1", "content": "first", "status": "TODO_STATUS_IN_PROGRESS"},
        {"id": "2", "content": "second", "status": "TODO_STATUS_PENDING"},
    ]});
    // `todo__read` changes nothing, so it records no update.
    let frames = collector.await.unwrap();
    assert_eq!(
        of_kind(frame_events(&frames), "todoUpdated"),
        vec![added.clone(), started.clone()],
        "{frames:#?}"
    );
    assert_eq!(
        of_kind(list_events(&app, &session).await, "todoUpdated"),
        vec![added, started.clone()]
    );
    let (status, todo) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{session}/todo"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{todo}");
    assert_eq!(todo, started);

    let listed = pb::messages_client::MessagesClient::new(grpc_channel(state).await)
        .get_session_todo(pb::GetSessionTodoRequest {
            session: session.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.items.len(), 2);
    assert_eq!(listed.items[0].status, pb::TodoStatus::InProgress as i32);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A manual compaction records the same `compactionApplied` the automatic
/// strategies do: on the live stream and in `ListEvents`, naming the summary
/// message and the folded count, flagged `manual`.
#[tokio::test(flavor = "multi_thread")]
async fn manual_compaction_streams_and_records_compaction_applied() {
    let dir = tempdir("compact");
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::Text("one".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text("two".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let (state, _engine) = state_with(provider, dir.clone()).await;
    let app = router(state.clone());
    let session = create_session(&app, &dir).await;
    run_turn(&app, &session, "first").await;
    run_turn(&app, &session, "second").await;

    let collector = tokio::spawn(sse_frames_until(
        app.clone(),
        format!("/v1/sessions/{session}/events/stream"),
        |frame| frame["event"]["compactionApplied"].is_object(),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{session}/compact"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The response names the strategy the recorded event carries.
    assert_eq!(body["strategy"], json!("local_summarizer"), "{body}");

    let frames = collector.await.unwrap();
    let applied = of_kind(frame_events(&frames), "compactionApplied");
    assert_eq!(applied.len(), 1, "{frames:#?}");
    let applied = &applied[0];
    assert_eq!(applied["strategy"], json!("local_summarizer"), "{applied}");
    assert_eq!(applied["manual"], json!(true), "{applied}");
    assert_eq!(applied["foldedCount"], json!(4), "{applied}");

    let (_, listed) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{session}/messages"),
        Value::Null,
    )
    .await;
    let summary = listed["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == json!("ROLE_SYSTEM"))
        .cloned()
        .unwrap();
    assert_eq!(applied["message"], summary["id"], "{listed:#}");

    let recorded = of_kind(list_events(&app, &session).await, "compactionApplied");
    assert_eq!(recorded, vec![applied.clone()]);

    // gRPC CompactSession reports the same record.
    let channel = grpc_channel(state).await;
    let mut events = pb::events_client::EventsClient::new(channel.clone())
        .stream_session_events(pb::StreamSessionEventsRequest {
            session: session.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    tokio::time::sleep(Duration::from_millis(100)).await;
    pb::session_client::SessionClient::new(channel)
        .compact_session(pb::CompactSessionRequest {
            session: session.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let applied = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = events.next().await {
            if let Some(pb::stream_frame::Frame::Event(event)) = frame.unwrap().frame
                && let Some(pb::stream_event::Payload::CompactionApplied(applied)) = event.payload
            {
                return applied;
            }
        }
        panic!("stream ended");
    })
    .await
    .expect("compactionApplied over gRPC");
    assert!(applied.manual);
    assert_eq!(applied.strategy, "local_summarizer");
    let _ = std::fs::remove_dir_all(&dir);
}
