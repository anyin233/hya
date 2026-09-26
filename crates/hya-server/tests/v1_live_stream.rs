//! v1 live delivery of provider deltas and provider-failure surfacing.
//!
//! Assistant text streams as live-only (`seq == 0`) frames while a provider
//! round is in flight; the durable log keeps one `text_start` +
//! `text_replace(full)` + `text_end` per part. The v1 streams must deliver
//! the live frames (they carry the same message/part ids the durable events
//! use), and a failed turn must expose the provider's error text.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const CHUNKS: [&str; 5] = ["Hello", ", liv", "e str", "eamin", "g!"];

/// Streams [`CHUNKS`] as separate text deltas with a pause between them, or
/// fails the round before any stream with a non-retryable provider error.
struct SlowTextProvider {
    fail: Option<String>,
}

#[async_trait]
impl Provider for SlowTextProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
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
        events.extend(CHUNKS.iter().map(|chunk| Event::TextDelta {
            session,
            message,
            part,
            delta: (*chunk).to_string(),
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
        let stream = futures::stream::iter(events).then(|event| async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            Ok(event)
        });
        Ok(Box::pin(stream))
    }
}

async fn state(fail: Option<&str>) -> AppState {
    let provider = SlowTextProvider {
        fail: fail.map(str::to_owned),
    };
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        providers,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    );
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    )
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

async fn create_session(app: &axum::Router) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/sessions",
        json!({
            "agent": "build",
            "model": "fake",
            "workdir": std::env::temp_dir().to_string_lossy(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session"]["id"].as_str().unwrap().to_owned()
}

/// Collect SSE `StreamFrame`s until `done` matches one (inclusive) or 10 s pass.
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

/// The (first) assistant message of the session transcript.
async fn assistant_message(app: &axum::Router, session: &str) -> Value {
    let (status, body) = call(
        app,
        Method::GET,
        &format!("/v1/sessions/{session}/messages"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == json!("ROLE_ASSISTANT"))
        .cloned()
        .expect("an assistant message")
}

fn events_for<'a>(frames: &'a [Value], assistant: &str) -> Vec<&'a Value> {
    frames
        .iter()
        .filter_map(|frame| frame.get("event"))
        .filter(|event| {
            event
                .as_object()
                .into_iter()
                .flat_map(|object| object.values())
                .any(|payload| payload.get("message") == Some(&json!(assistant)))
        })
        .collect()
}

#[tokio::test]
async fn sse_stream_delivers_live_assistant_deltas_before_message_finished() {
    let app = router(state(None).await);
    let session = create_session(&app).await;

    // Collect everything until the assistant's finish. The user message
    // finishes on admission with `seq`; the assistant one is the second
    // `messageFinished` of the turn.
    let collector = tokio::spawn(sse_frames_until(
        app.clone(),
        format!("/v1/sessions/{session}/events/stream"),
        {
            let count = std::sync::atomic::AtomicUsize::new(0);
            move |frame| {
                frame["event"]["messageFinished"].is_object()
                    && count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
            }
        },
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, created) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({"prompt": {"text": "stream please"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let frames = collector.await.unwrap();
    let assistant = assistant_message(&app, &session).await;
    let assistant_id = assistant["id"].as_str().unwrap();
    let events = events_for(&frames, assistant_id);

    let finished_at = events
        .iter()
        .position(|event| event["messageFinished"].is_object())
        .unwrap_or_else(|| panic!("assistant messageFinished missing: {frames:#?}"));
    let appended: Vec<(usize, &Value)> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event["partAppended"].is_object())
        .map(|(index, event)| (index, *event))
        .collect();
    assert_eq!(
        appended.len(),
        CHUNKS.len(),
        "every live text delta must arrive as partAppended: {frames:#?}"
    );
    assert!(
        appended.iter().all(|(index, _)| *index < finished_at),
        "deltas must arrive before the finish: {frames:#?}"
    );
    // Live frames carry no seq (protojson omits 0).
    assert!(appended.iter().all(|(_, event)| event.get("seq").is_none()));
    let streamed: String = appended
        .iter()
        .map(|(_, event)| event["partAppended"]["textDelta"].as_str().unwrap())
        .collect();
    assert_eq!(streamed, CHUNKS.concat());

    // The live part id is the durable part id the projection uses.
    let durable_part = assistant["parts"][0]["id"].as_str().unwrap();
    assert!(
        appended
            .iter()
            .all(|(_, event)| event["partAppended"]["part"] == json!(durable_part)),
        "live deltas must reference the durable part {durable_part}: {frames:#?}"
    );
    let started: Vec<&&Value> = events
        .iter()
        .filter(|event| event["partStarted"]["part"] == json!(durable_part))
        .collect();
    assert!(
        started.iter().any(|event| event.get("seq").is_none())
            && started.iter().any(|event| event.get("seq").is_some()),
        "partStarted arrives live and durable for the same id: {started:#?}"
    );
    // The durable full text arrives as a replace snapshot.
    let replaced = events
        .iter()
        .find(|event| event["partReplaced"]["part"] == json!(durable_part))
        .unwrap_or_else(|| panic!("durable partReplaced missing: {frames:#?}"));
    assert_eq!(replaced["partReplaced"]["text"], json!(CHUNKS.concat()));
    assert!(replaced.get("seq").is_some());
    assert_eq!(
        assistant["parts"][0]["text"]["text"],
        json!(CHUNKS.concat())
    );
}

#[tokio::test]
async fn provider_failure_exposes_the_error_on_turn_message_and_stream() {
    let app = router(state(Some("invalid request: prompt rejected by upstream")).await);
    let session = create_session(&app).await;
    let collector = tokio::spawn(sse_frames_until(
        app.clone(),
        format!("/v1/sessions/{session}/events/stream"),
        |frame| frame["event"]["messageFinished"]["finish"] == json!("FINISH_REASON_ERROR"),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, created) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({"prompt": {"text": "fail please"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let turn = created["turn"]["id"].as_str().unwrap();
    let (status, waited) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{waited}");
    assert_eq!(waited["state"], json!("TURN_STATE_FAILED"), "{waited}");
    assert_eq!(waited["errorCode"], json!("provider_error"), "{waited}");
    assert!(
        waited["errorMessage"]
            .as_str()
            .is_some_and(|text| text.contains("prompt rejected by upstream")),
        "{waited}"
    );
    let (status, got) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{session}/turns/{turn}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["errorMessage"], waited["errorMessage"]);

    let assistant = assistant_message(&app, &session).await;
    assert_eq!(assistant["finish"], json!("FINISH_REASON_ERROR"));
    assert_eq!(assistant["error"]["code"], json!("provider_error"));
    assert_eq!(assistant["error"]["message"], waited["errorMessage"]);

    let frames = collector.await.unwrap();
    let assistant_id = assistant["id"].as_str().unwrap();
    let events = events_for(&frames, assistant_id);
    let reported = events
        .iter()
        .position(|event| event["errorReported"].is_object())
        .unwrap_or_else(|| panic!("errorReported missing: {frames:#?}"));
    let finished = events
        .iter()
        .position(|event| event["messageFinished"].is_object())
        .unwrap();
    assert!(reported < finished, "the error precedes the finish");
    assert_eq!(
        events[reported]["errorReported"]["errorMessage"],
        waited["errorMessage"]
    );

    // Replay carries the same error event durably.
    let (status, replay) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{session}/events"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        replay["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["errorReported"]["message"] == json!(assistant_id)),
        "{replay}"
    );
}

/// Spawn a tonic server exposing the Events and Turn services.
async fn grpc_endpoint(app: AppState) -> std::net::SocketAddr {
    use tonic::transport::Server;
    let grpc = V1Grpc::new(app);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serve = Server::builder()
        .add_service(pb::session_server::SessionServer::new(grpc.clone()))
        .add_service(pb::turn_server::TurnServer::new(grpc.clone()))
        .add_service(pb::events_server::EventsServer::new(grpc.clone()))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(async move {
        let _ = serve.await;
    });
    address
}

#[tokio::test]
async fn grpc_session_stream_delivers_live_assistant_deltas() {
    let app_state = state(None).await;
    let app = router(app_state.clone());
    let session = create_session(&app).await;
    let address = grpc_endpoint(app_state).await;
    let channel = tonic::transport::Channel::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut events = pb::events_client::EventsClient::new(channel.clone());
    let mut turns = pb::turn_client::TurnClient::new(channel);
    let mut stream = events
        .stream_session_events(pb::StreamSessionEventsRequest {
            session: session.clone(),
            since_seq: 0,
            include_descendants: false,
        })
        .await
        .unwrap()
        .into_inner();
    turns
        .create_turn(pb::CreateTurnRequest {
            session: session.clone(),
            kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                text: "stream please".into(),
                ..Default::default()
            })),
        })
        .await
        .unwrap();

    use pb::stream_event::Payload as P;
    let mut deltas = String::new();
    let mut finishes = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while finishes < 2 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = tokio::time::timeout(remaining, stream.next())
            .await
            .expect("stream stalled")
            .expect("stream ended")
            .unwrap();
        let Some(pb::stream_frame::Frame::Event(event)) = frame.frame else {
            continue;
        };
        match event.payload {
            Some(P::PartAppended(appended)) if event.seq == 0 => {
                assert_eq!(finishes, 1, "deltas arrive before the assistant finish");
                deltas.push_str(&appended.text_delta);
            }
            Some(P::MessageFinished(_)) => finishes += 1,
            _ => {}
        }
    }
    assert_eq!(deltas, CHUNKS.concat());
}
