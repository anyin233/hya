#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use serde_json::json;
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, FinishReason, ModelRef};
use yaca_provider::{FakeProvider, FakeStep, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-event-api";

async fn state() -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn reasoning_state() -> AppState {
    let providers = Arc::new(
        ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![
            FakeStep::Reasoning("thinking".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ]))),
    );
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn shell_state() -> AppState {
    std::fs::create_dir_all(WORKDIR).unwrap();
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

#[tokio::test]
async fn opencode_v2_event_route_streams_connected_event() {
    let app = router(state().await);
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/event?location%5Bdirectory%5D=/tmp/yaca-opencode-event-api")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let mut stream = resp.into_body().into_data_stream();
    let event = read_sse_json(&mut stream).await;
    assert_eq!(event["type"], "server.connected");
    assert_eq!(event["properties"], json!({}));
    assert!(event.get("location").is_none());
    assert!(event.get("data").is_none());
}

#[tokio::test]
async fn opencode_legacy_event_route_streams_connected_event() {
    assert_event_stream("/event").await;
}

#[tokio::test]
async fn opencode_global_event_route_streams_connected_event() {
    let app = router(state().await);
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/global/event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let mut stream = resp.into_body().into_data_stream();
    let event = read_sse_json(&mut stream).await;
    assert_eq!(event["payload"]["type"], "server.connected");
    assert!(event["payload"].get("location").is_none());
    assert_eq!(event["payload"]["properties"], json!({}));
}

#[tokio::test]
async fn opencode_v2_event_route_streams_session_created_location() {
    let app = router(state().await);
    let directory = "/tmp/yaca-opencode-event-api-scoped";
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/event?location%5Bdirectory%5D={directory}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    let connected = read_sse_json(&mut stream).await;
    assert_eq!(connected["type"], "server.connected");
    assert_eq!(connected["properties"], json!({}));
    assert!(connected.get("location").is_none());

    let created = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"location": {"directory": directory}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let event = read_sse_json(&mut stream).await;
    assert_eq!(event["type"], "session.created");
    assert_eq!(event["location"]["directory"], directory);
    assert!(event.get("properties").is_none());
    let session = event["data"]["sessionID"].as_str().unwrap();
    assert_eq!(event["data"]["info"]["id"], session);
    assert_eq!(event["data"]["info"]["directory"], directory);
}

#[tokio::test]
async fn opencode_v2_event_route_filters_session_events_by_location() {
    let app = router(state().await);
    let visible = "/tmp/yaca-opencode-event-api-visible";
    let hidden = "/tmp/yaca-opencode-event-api-hidden";
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/event?location%5Bdirectory%5D={visible}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    let connected = read_sse_json(&mut stream).await;
    assert_eq!(connected["type"], "server.connected");
    assert!(connected.get("location").is_none());

    let hidden_created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"location": {"directory": hidden}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hidden_created.status(), StatusCode::OK);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .is_err(),
        "unexpected event for hidden location"
    );

    let visible_created = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"location": {"directory": visible}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(visible_created.status(), StatusCode::OK);

    let event = read_sse_json(&mut stream).await;
    assert_eq!(event["type"], "session.created");
    assert_eq!(event["location"]["directory"], visible);
    assert_eq!(event["data"]["info"]["directory"], visible);
}

#[tokio::test]
async fn opencode_legacy_event_route_streams_session_updated_properties() {
    let app = router(state().await);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    let connected = read_sse_json(&mut stream).await;
    assert_eq!(connected["type"], "server.connected");

    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let created_event = read_sse_json(&mut stream).await;
    assert_eq!(created_event["type"], "session.created");
    let session = created_event["properties"]["sessionID"]
        .as_str()
        .unwrap()
        .to_string();

    let updated = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/session/{session}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"title": "Renamed"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);

    let updated_event = read_sse_json(&mut stream).await;
    assert_eq!(updated_event["type"], "session.updated");
    assert!(updated_event.get("location").is_none());
    assert_eq!(updated_event["properties"]["sessionID"], session);
    assert_eq!(updated_event["properties"]["info"]["id"], session);
    assert_eq!(updated_event["properties"]["info"]["title"], "Renamed");

    let metadata_updated = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/session/{session}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"metadata": {"ticket": "OC-42"}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(metadata_updated.status(), StatusCode::OK);

    let metadata_event = read_sse_json(&mut stream).await;
    assert_eq!(metadata_event["type"], "session.updated");
    assert_eq!(metadata_event["properties"]["sessionID"], session);
    assert_eq!(metadata_event["properties"]["info"]["id"], session);
    assert_eq!(
        metadata_event["properties"]["info"]["metadata"]["ticket"],
        "OC-42"
    );
}

#[tokio::test]
async fn opencode_legacy_event_route_streams_message_updated_properties() {
    let app = router(state().await);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    assert_eq!(read_sse_json(&mut stream).await["type"], "server.connected");

    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created_event = read_sse_json(&mut stream).await;
    let session = created_event["properties"]["sessionID"].as_str().unwrap();

    let command = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/sessions/{session}/command"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "command": "init",
                        "arguments": "audit",
                        "text": "/init audit"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(command.status(), StatusCode::OK);

    let started = read_sse_json(&mut stream).await;
    assert_eq!(started["type"], "message.updated");
    assert_eq!(started["properties"]["sessionID"], session);
    let message = started["properties"]["info"]["id"].as_str().unwrap();
    assert!(!message.is_empty());
    assert_eq!(started["properties"]["info"]["role"], "user");
    assert!(started["properties"]["info"].get("finish").is_none());

    let part_started = read_sse_json(&mut stream).await;
    assert_eq!(part_started["type"], "message.part.updated");
    assert_eq!(part_started["properties"]["sessionID"], session);
    assert_eq!(
        part_started["properties"]["time"],
        part_started["properties"]["part"]["time"]["start"]
    );
    assert_eq!(part_started["properties"]["part"]["sessionID"], session);
    assert_eq!(part_started["properties"]["part"]["messageID"], message);
    assert_eq!(part_started["properties"]["part"]["type"], "text");
    assert_eq!(part_started["properties"]["part"]["text"], "");
    let part = part_started["properties"]["part"]["id"].as_str().unwrap();

    let delta = read_sse_json(&mut stream).await;
    assert_eq!(delta["type"], "message.part.delta");
    assert_eq!(delta["properties"]["sessionID"], session);
    assert_eq!(delta["properties"]["messageID"], message);
    assert_eq!(delta["properties"]["partID"], part);
    assert_eq!(delta["properties"]["field"], "text");
    assert_eq!(delta["properties"]["delta"], "/init audit");

    let final_text = read_next_part_updated(&mut stream, "text").await;
    assert_eq!(final_text["properties"]["part"]["id"], part);
    assert_eq!(final_text["properties"]["part"]["text"], "/init audit");

    let command_event = read_next_command_executed(&mut stream).await;
    assert_eq!(command_event["properties"]["sessionID"], session);
    assert_eq!(command_event["properties"]["messageID"], message);
    assert_eq!(command_event["properties"]["name"], "init");
    assert_eq!(command_event["properties"]["arguments"], "audit");

    let assistant_finished = read_next_message(&mut stream, "assistant", Some("stop")).await;
    assert_eq!(assistant_finished["properties"]["sessionID"], session);
    assert_eq!(
        assistant_finished["properties"]["info"]["role"],
        "assistant"
    );
    assert_eq!(assistant_finished["properties"]["info"]["finish"], "stop");

    let delete_part = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/session/{session}/message/{message}/part/{part}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_part.status(), StatusCode::OK);
    let part_removed = read_sse_json(&mut stream).await;
    assert_eq!(part_removed["type"], "message.part.removed");
    assert_eq!(part_removed["properties"]["sessionID"], session);
    assert_eq!(part_removed["properties"]["messageID"], message);
    assert_eq!(part_removed["properties"]["partID"], part);

    let delete_message = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/session/{session}/message/{message}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_message.status(), StatusCode::OK);
    let message_removed = read_sse_json(&mut stream).await;
    assert_eq!(message_removed["type"], "message.removed");
    assert_eq!(message_removed["properties"]["sessionID"], session);
    assert_eq!(message_removed["properties"]["messageID"], message);
}

#[tokio::test]
async fn opencode_legacy_event_route_streams_reasoning_part_events() {
    let app = router(reasoning_state().await);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    assert_eq!(read_sse_json(&mut stream).await["type"], "server.connected");

    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created_event = read_sse_json(&mut stream).await;
    let session = created_event["properties"]["sessionID"].as_str().unwrap();

    let prompt = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/sessions/{session}/prompt"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"text": "hello"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(prompt.status(), StatusCode::OK);

    let started = read_next_part_updated(&mut stream, "reasoning").await;
    assert_eq!(started["properties"]["sessionID"], session);
    assert_eq!(started["properties"]["part"]["type"], "reasoning");
    assert_eq!(started["properties"]["part"]["text"], "");
    let message = started["properties"]["part"]["messageID"].as_str().unwrap();
    let part = started["properties"]["part"]["id"].as_str().unwrap();

    let delta = read_next_part_delta(&mut stream, part).await;
    assert_eq!(delta["properties"]["sessionID"], session);
    assert_eq!(delta["properties"]["messageID"], message);
    assert_eq!(delta["properties"]["field"], "text");
    assert_eq!(delta["properties"]["delta"], "thinking");

    let final_reasoning = read_next_part_updated(&mut stream, "reasoning").await;
    assert_eq!(final_reasoning["properties"]["part"]["id"], part);
    assert_eq!(final_reasoning["properties"]["part"]["text"], "thinking");
}

#[tokio::test]
async fn opencode_legacy_event_route_streams_tool_part_events() {
    let app = router(shell_state().await);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    assert_eq!(read_sse_json(&mut stream).await["type"], "server.connected");

    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created_event = read_sse_json(&mut stream).await;
    let session = created_event["properties"]["sessionID"].as_str().unwrap();

    let shell = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/sessions/{session}/shell"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"command": "printf opencode-tool-event"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(shell.status(), StatusCode::OK);

    let running = read_next_tool_state(&mut stream, "running").await;
    assert_eq!(running["properties"]["part"]["sessionID"], session);
    assert_eq!(running["properties"]["part"]["tool"], "shell");
    assert_eq!(
        running["properties"]["part"]["state"]["input"]["command"],
        "printf opencode-tool-event"
    );
    let part = running["properties"]["part"]["id"].as_str().unwrap();
    let message = running["properties"]["part"]["messageID"].as_str().unwrap();
    let call = running["properties"]["part"]["callID"].as_str().unwrap();

    let completed = read_next_tool_state(&mut stream, "completed").await;
    assert_eq!(completed["properties"]["part"]["id"], part);
    assert!(
        completed["properties"]["part"]["state"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("opencode-tool-event"))
    );

    let updated = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/session/{session}/message/{message}/part/{part}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "id": part,
                        "sessionID": session,
                        "messageID": message,
                        "type": "tool",
                        "callID": call,
                        "tool": "shell",
                        "state": {
                            "status": "error",
                            "input": {"command": "printf opencode-tool-event"},
                            "error": "forced error"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);

    let error = read_next_tool_state(&mut stream, "error").await;
    assert_eq!(error["properties"]["part"]["id"], part);
    assert_eq!(
        error["properties"]["part"]["state"]["error"],
        "forced error"
    );
}

async fn assert_event_stream(uri: &str) {
    let app = router(state().await);
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let mut stream = resp.into_body().into_data_stream();
    let event = read_sse_json(&mut stream).await;
    assert_eq!(event["type"], "server.connected");
    assert!(event.get("location").is_none());
    assert_eq!(event["properties"], json!({}));
}

async fn read_next_message(
    stream: &mut axum::body::BodyDataStream,
    role: &str,
    finish: Option<&str>,
) -> serde_json::Value {
    for _ in 0..24 {
        let event = read_sse_json(stream).await;
        let info = &event["properties"]["info"];
        let finish_matches = match finish {
            Some(finish) => info["finish"] == finish,
            None => info.get("finish").is_none(),
        };
        if event["type"] == "message.updated" && info["role"] == role && finish_matches {
            return event;
        }
    }
    panic!("message.updated role {role} not found");
}

async fn read_next_part_updated(
    stream: &mut axum::body::BodyDataStream,
    part_type: &str,
) -> serde_json::Value {
    for _ in 0..32 {
        let event = read_sse_json(stream).await;
        if event["type"] == "message.part.updated"
            && event["properties"]["part"]["type"] == part_type
        {
            return event;
        }
    }
    panic!("message.part.updated {part_type} not found");
}

async fn read_next_part_delta(
    stream: &mut axum::body::BodyDataStream,
    part: &str,
) -> serde_json::Value {
    for _ in 0..32 {
        let event = read_sse_json(stream).await;
        if event["type"] == "message.part.delta" && event["properties"]["partID"] == part {
            return event;
        }
    }
    panic!("message.part.delta {part} not found");
}

async fn read_next_command_executed(stream: &mut axum::body::BodyDataStream) -> serde_json::Value {
    for _ in 0..32 {
        let event = read_sse_json(stream).await;
        if event["type"] == "command.executed" {
            return event;
        }
    }
    panic!("command.executed not found");
}

async fn read_next_tool_state(
    stream: &mut axum::body::BodyDataStream,
    status: &str,
) -> serde_json::Value {
    for _ in 0..48 {
        let event = read_sse_json(stream).await;
        if event["type"] == "message.part.updated"
            && event["properties"]["part"]["type"] == "tool"
            && event["properties"]["part"]["state"]["status"] == status
        {
            return event;
        }
    }
    panic!("tool state {status} not found");
}

async fn read_sse_json(stream: &mut axum::body::BodyDataStream) -> serde_json::Value {
    let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("event")
        .expect("body chunk")
        .expect("valid chunk");
    let frame = String::from_utf8(chunk.to_vec()).unwrap();
    assert!(frame.contains("data:"));
    let data = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("data line");
    serde_json::from_str(data).unwrap()
}
