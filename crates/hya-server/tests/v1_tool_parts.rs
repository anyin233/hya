//! v1 tool-call contract: a tool part carries its call id, arguments,
//! output/duration or error text on the transcript read; the stream carries
//! the argument fragments and the state changes with their payloads; a
//! subagent spawn surfaces as `memberUpdated` on the parent's stream and in
//! `SessionInfo.members`; and a permission ask names the tool and its
//! arguments.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{
    AgentName, Envelope, Event, FinishReason, MemberId, MemberRunStatus, ModelRef, ReportOutcome,
    SessionId, ToolCallId,
};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::permission::AskRequest;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tower::ServiceExt;

fn tempdir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-v1-tool-parts-{tag}-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state_with(
    provider: FakeProvider,
    rules: PermissionRules,
    workdir: PathBuf,
) -> (
    AppState,
    mpsc::UnboundedReceiver<AskRequest>,
    Arc<SessionEngine>,
) {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let (perm, asks) = PermissionPlane::new(rules);
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        providers,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    ));
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
    (state, asks, engine)
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
        json!({
            "agent": "build",
            "model": "fake",
            "workdir": workdir.to_string_lossy(),
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

async fn start_turn(app: &axum::Router, session: &str, text: &str) -> String {
    let (status, created) = call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({"prompt": {"text": text}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    created["turn"]["id"].as_str().unwrap().to_owned()
}

async fn wait_turn(app: &axum::Router, session: &str, turn: &str) -> Value {
    let (status, waited) = call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{waited}");
    waited
}

fn parse_json(text: &Value) -> Value {
    serde_json::from_str(text.as_str().unwrap_or_else(|| panic!("JSON text: {text}")))
        .unwrap_or_else(|error| panic!("invalid JSON text {text}: {error}"))
}

/// Stream payloads (the single payload object of each frame's event).
fn payloads(frames: &[Value]) -> Vec<(String, Value, bool)> {
    frames
        .iter()
        .filter_map(|frame| frame.get("event"))
        .filter_map(|event| {
            let durable = event.get("seq").is_some();
            event.as_object().and_then(|object| {
                object
                    .iter()
                    .find(|(key, _)| !matches!(key.as_str(), "seq" | "session" | "timeRecorded"))
                    .map(|(key, value)| (key.clone(), value.clone(), durable))
            })
        })
        .collect()
}

async fn grpc_endpoint(app: AppState) -> std::net::SocketAddr {
    use tonic::transport::Server;
    let grpc = V1Grpc::new(app);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serve = Server::builder()
        .add_service(pb::session_server::SessionServer::new(grpc.clone()))
        .add_service(pb::messages_server::MessagesServer::new(grpc.clone()))
        .add_service(pb::events_server::EventsServer::new(grpc.clone()))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(async move {
        let _ = serve.await;
    });
    address
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_parts_carry_arguments_output_and_errors_on_reads_and_the_stream() {
    let dir = tempdir("read");
    let present = dir.join("present.txt");
    std::fs::write(&present, "hello tool output").unwrap();
    let missing = dir.join("missing.txt");
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: json!({ "path": present }),
            },
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: json!({ "path": missing }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let rules = PermissionRules::new(vec![Rule::new(Action::Read, "/**", Mode::Allow)]);
    let (state, _asks, _engine) = state_with(provider, rules, dir.clone()).await;
    let app = router(state.clone());
    let session = create_session(&app, &dir).await;

    let saw_tool = Arc::new(AtomicBool::new(false));
    let collector = tokio::spawn(sse_frames_until(
        app.clone(),
        format!("/v1/sessions/{session}/events/stream"),
        {
            let saw_tool = Arc::clone(&saw_tool);
            move |frame| {
                if frame["event"]["toolStateChanged"].is_object() {
                    saw_tool.store(true, Ordering::SeqCst);
                }
                saw_tool.load(Ordering::SeqCst) && frame["event"]["messageFinished"].is_object()
            }
        },
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let turn = start_turn(&app, &session, "read both").await;
    let waited = wait_turn(&app, &session, &turn).await;
    assert_eq!(waited["state"], json!("TURN_STATE_FINISHED"), "{waited}");

    // Transcript read: one tool part per call, with everything a card needs.
    let (status, listed) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{session}/messages"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let tools: Vec<(String, Value)> = listed["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["parts"].as_array().cloned().unwrap_or_default())
        .filter(|part| part["toolCall"].is_object())
        .map(|part| {
            (
                part["id"].as_str().unwrap().to_owned(),
                part["toolCall"].clone(),
            )
        })
        .collect();
    assert_eq!(tools.len(), 2, "{listed:#}");
    let (ok_part, ok) = &tools[0];
    let (err_part, err) = &tools[1];
    assert_eq!(ok["tool"], json!("read"));
    assert_eq!(ok["state"], json!("TOOL_EXECUTION_STATE_OK"), "{ok:#}");
    assert!(
        ok["callId"].as_str().is_some_and(|id| !id.is_empty()),
        "{ok:#}"
    );
    assert_eq!(parse_json(&ok["inputJson"]), json!({ "path": present }));
    let output = ok["outputJson"].as_str().expect("outputJson on an OK call");
    serde_json::from_str::<Value>(output).expect("outputJson is JSON");
    assert!(output.contains("hello tool output"), "{output}");
    // protojson omits a zero duration; when present it is a count.
    if let Some(duration) = ok.get("durationMs") {
        assert!(duration.as_str().unwrap().parse::<u64>().is_ok(), "{ok:#}");
    }
    assert!(ok.get("errorMessage").is_none(), "{ok:#}");

    assert_eq!(err["state"], json!("TOOL_EXECUTION_STATE_ERROR"), "{err:#}");
    assert!(err["callId"].as_str().is_some_and(|id| !id.is_empty()));
    assert_ne!(err["callId"], ok["callId"]);
    assert_eq!(parse_json(&err["inputJson"]), json!({ "path": missing }));
    assert!(
        err["errorMessage"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "{err:#}"
    );
    assert!(err.get("outputJson").is_none(), "{err:#}");

    // Stream: argument fragments and state changes, durable, before the finish.
    let frames = collector.await.unwrap();
    let stream = payloads(&frames);
    let position = |pred: &dyn Fn(&str, &Value) -> bool| {
        stream
            .iter()
            .position(|(kind, value, _)| pred(kind, value))
            .unwrap_or_else(|| panic!("frame missing: {frames:#?}"))
    };
    let started = position(&|kind, value| kind == "partStarted" && value["part"] == json!(ok_part));
    let started_payload = &stream[started].1;
    assert_eq!(started_payload["kind"], json!("tool_call"));
    assert_eq!(started_payload["tool"], json!("read"));
    assert_eq!(started_payload["callId"], ok["callId"]);
    let appended =
        position(&|kind, value| kind == "partAppended" && value["part"] == json!(ok_part));
    assert_eq!(
        parse_json(&stream[appended].1["textDelta"]),
        json!({ "path": present })
    );
    let running = position(&|kind, value| {
        kind == "toolStateChanged"
            && value["part"] == json!(ok_part)
            && value["state"] == json!("TOOL_EXECUTION_STATE_RUNNING")
    });
    let running_payload = &stream[running].1;
    assert_eq!(running_payload["callId"], ok["callId"]);
    assert_eq!(running_payload["tool"], json!("read"));
    assert_eq!(
        parse_json(&running_payload["inputJson"]),
        json!({ "path": present })
    );
    let done = position(&|kind, value| {
        kind == "toolStateChanged"
            && value["part"] == json!(ok_part)
            && value["state"] == json!("TOOL_EXECUTION_STATE_OK")
    });
    assert_eq!(stream[done].1["outputJson"], ok["outputJson"]);
    assert_eq!(stream[done].1["callId"], ok["callId"]);
    let failed = position(&|kind, value| {
        kind == "toolStateChanged"
            && value["part"] == json!(err_part)
            && value["state"] == json!("TOOL_EXECUTION_STATE_ERROR")
    });
    assert_eq!(stream[failed].1["errorMessage"], err["errorMessage"]);
    assert_eq!(stream[failed].1["errorCode"], err["errorCode"]);
    let finished = stream
        .iter()
        .rposition(|(kind, _, _)| kind == "messageFinished")
        .unwrap();
    assert!(
        started < appended && appended < running && running < done && done < finished,
        "{frames:#?}"
    );
    assert!(failed < finished);
    for index in [started, appended, running, done, failed] {
        assert!(
            stream[index].2,
            "tool frames are durable: {:?}",
            stream[index]
        );
    }

    // gRPC parity: the transcript read and the replay are identical.
    let address = grpc_endpoint(state).await;
    let channel = tonic::transport::Channel::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut messages = pb::messages_client::MessagesClient::new(channel.clone());
    let grpc_listed = serde_json::to_value(
        messages
            .list_messages(tonic::Request::new(pb::ListMessagesRequest {
                session: session.clone(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    assert_eq!(grpc_listed, listed);
    let mut events = pb::events_client::EventsClient::new(channel);
    let grpc_events = serde_json::to_value(
        events
            .list_events(tonic::Request::new(pb::ListEventsRequest {
                session: session.clone(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    let (status, http_events) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{session}/events"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(grpc_events, http_events);
    assert!(
        http_events["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["toolStateChanged"]["inputJson"].is_string()),
        "the replay carries the tool arguments: {http_events:#}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn member_updates_stream_on_the_parent_and_fold_into_session_info() {
    let dir = tempdir("member");
    let provider = FakeProvider::scripted(vec![]);
    let (state, _asks, engine) =
        state_with(provider, PermissionRules::default(), dir.clone()).await;
    let app = router(state);
    let parent_id = create_session(&app, &dir).await;
    let parent: SessionId = parent_id.parse().unwrap();

    let collector = tokio::spawn(sse_frames_until(
        app.clone(),
        format!("/v1/sessions/{parent_id}/events/stream"),
        |frame| frame["event"]["memberUpdated"]["status"] == json!("MEMBER_STATUS_DONE"),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The parent-log events a `task` spawn records, appended and published
    // the way the engine does.
    let member = MemberId::new();
    let child = SessionId::new();
    let call_id = ToolCallId::new();
    for event in [
        Event::MemberSpawned {
            session: parent,
            member,
            child: Some(child),
            subagent_type: AgentName::new("general"),
            description: "survey the repo".to_string(),
            depth: 1,
            directive: "look around".to_string(),
            tool_call: Some(call_id),
        },
        Event::MemberStatusChanged {
            session: parent,
            member,
            status: MemberRunStatus::Running,
        },
        Event::MemberFinished {
            session: parent,
            member,
            status: MemberRunStatus::Done,
            summary: "found 3 crates".to_string(),
            child: Some(child),
        },
    ] {
        let (seq, ts_millis) = engine.store().append_event(parent, &event).await.unwrap();
        engine.bus().publish(Envelope {
            seq,
            ts_millis,
            event,
        });
    }

    let frames = collector.await.unwrap();
    let members: Vec<Value> = payloads(&frames)
        .into_iter()
        .filter(|(kind, _, _)| kind == "memberUpdated")
        .map(|(_, value, durable)| {
            assert!(durable, "member frames are durable");
            value
        })
        .collect();
    assert_eq!(members.len(), 3, "{frames:#?}");
    assert_eq!(
        members[0],
        json!({
            "member": member.to_string(),
            "child": child.to_string(),
            "agent": "general",
            "description": "survey the repo",
            "status": "MEMBER_STATUS_SPAWNING",
            "callId": call_id.to_string(),
            "depth": 1,
        })
    );
    assert_eq!(
        members[1],
        json!({"member": member.to_string(), "status": "MEMBER_STATUS_RUNNING"})
    );
    assert_eq!(
        members[2],
        json!({
            "member": member.to_string(),
            "child": child.to_string(),
            "status": "MEMBER_STATUS_DONE",
            "summary": "found 3 crates",
        })
    );

    // The read folds the same rows.
    let (status, info) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{parent_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    assert_eq!(
        info["members"],
        json!([{
            "member": member.to_string(),
            "child": child.to_string(),
            "agent": "general",
            "description": "survey the repo",
            "status": "MEMBER_STATUS_DONE",
            "summary": "found 3 crates",
            "callId": call_id.to_string(),
            "depth": 1,
        }]),
        "{info:#}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A resident member's terminal report (`SubagentReported`, the ADR-0015
/// report marker) streams as `memberUpdated` with the outcome and the report,
/// and folds into `SessionInfo.members`.
#[tokio::test(flavor = "multi_thread")]
async fn resident_report_streams_as_a_terminal_member_update() {
    let dir = tempdir("member-report");
    let provider = FakeProvider::scripted(vec![]);
    let (state, _asks, engine) =
        state_with(provider, PermissionRules::default(), dir.clone()).await;
    let app = router(state);
    let parent_id = create_session(&app, &dir).await;
    let parent: SessionId = parent_id.parse().unwrap();

    let collector = tokio::spawn(sse_frames_until(
        app.clone(),
        format!("/v1/sessions/{parent_id}/events/stream"),
        |frame| frame["event"]["memberUpdated"]["status"] == json!("MEMBER_STATUS_FAILED"),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let member = MemberId::new();
    let child = SessionId::new();
    let call_id = ToolCallId::new();
    for event in [
        Event::MemberSpawned {
            session: parent,
            member,
            child: Some(child),
            subagent_type: AgentName::new("general"),
            description: "survey the repo".to_string(),
            depth: 1,
            directive: "look around".to_string(),
            tool_call: Some(call_id),
        },
        Event::MemberStatusChanged {
            session: parent,
            member,
            status: MemberRunStatus::Running,
        },
        Event::SubagentReported {
            session: parent,
            member,
            child,
            handle: "main/general-exusiai".to_string(),
            outcome: ReportOutcome::Failed,
            report: "turn error: model exploded".to_string(),
        },
    ] {
        let (seq, ts_millis) = engine.store().append_event(parent, &event).await.unwrap();
        engine.bus().publish(Envelope {
            seq,
            ts_millis,
            event,
        });
    }

    let frames = collector.await.unwrap();
    let members: Vec<Value> = payloads(&frames)
        .into_iter()
        .filter(|(kind, _, _)| kind == "memberUpdated")
        .map(|(_, value, _)| value)
        .collect();
    assert_eq!(members.len(), 3, "{frames:#?}");
    assert_eq!(
        members[2],
        json!({
            "member": member.to_string(),
            "child": child.to_string(),
            "status": "MEMBER_STATUS_FAILED",
            "summary": "turn error: model exploded",
        })
    );

    let (status, info) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{parent_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    assert_eq!(info["members"][0]["status"], json!("MEMBER_STATUS_FAILED"));
    assert_eq!(info["members"][0]["callId"], json!(call_id.to_string()));
    assert_eq!(
        info["members"][0]["summary"],
        json!("turn error: model exploded")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_interactions_name_the_tool_and_its_arguments() {
    let dir = tempdir("perm");
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "bash".to_string(),
                input: json!({ "command": "printf guarded", "timeout": 5000 }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let rules = PermissionRules::new(vec![Rule::new(Action::Bash, "*", Mode::Ask)]);
    let (state, asks, _engine) = state_with(provider, rules, dir.clone()).await;
    let app = router(state.with_permission_requests(asks));
    let session = create_session(&app, &dir).await;

    let collector = tokio::spawn(sse_frames_until(
        app.clone(),
        "/v1/events/stream".to_string(),
        |frame| frame["event"]["permissionRequested"].is_object(),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let turn = start_turn(&app, &session, "run it").await;

    let frames = collector.await.unwrap();
    let asked = frames
        .iter()
        .find_map(|frame| frame["event"]["permissionRequested"].as_object().cloned())
        .unwrap_or_else(|| panic!("permissionRequested missing: {frames:#?}"));
    let streamed = &asked["interaction"]["payload"];

    let (status, listed) = call(
        &app,
        Method::GET,
        &format!("/v1/interactions?session={session}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let interaction = listed["interactions"][0].clone();
    assert_eq!(interaction["id"], asked["request"]);
    assert!(
        interaction["title"]
            .as_str()
            .is_some_and(|title| title.contains("printf guarded")),
        "{interaction:#}"
    );
    for payload in [streamed, &interaction["payload"]] {
        assert_eq!(payload["action"], json!("bash"), "{payload:#}");
        assert_eq!(payload["resource"], json!("printf guarded"), "{payload:#}");
        assert_eq!(payload["tool"], json!("bash"), "{payload:#}");
        // A `Struct` carries numbers as doubles.
        assert_eq!(payload["input"]["command"], json!("printf guarded"));
        assert_eq!(payload["input"]["timeout"].as_f64(), Some(5000.0));
        assert!(payload["callId"].as_str().is_some_and(|id| !id.is_empty()));
        assert!(
            payload["messageId"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
    }

    let request = asked["request"].as_str().unwrap();
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/interactions/{request}/respond"),
        json!({"permission": {"allowed": false}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_turn(&app, &session, &turn).await;

    // The tool part the ask was about carries the same call id.
    let (_, listed) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{session}/messages"),
        Value::Null,
    )
    .await;
    let call_ids: Vec<Value> = listed["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["parts"].as_array().cloned().unwrap_or_default())
        .filter_map(|part| part["toolCall"].get("callId").cloned())
        .collect();
    assert_eq!(call_ids, vec![streamed["callId"].clone()]);
    let _ = std::fs::remove_dir_all(&dir);
}
