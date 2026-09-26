//! v1 root-session archive: `UpdateSession.archived`, `SessionInfo.archived`
//! / `archivedAt`, the `ListSessions` archive filters, the `sessionUpdated`
//! stream mapping, archiving a running session (its turn finishes), a new
//! prompt or shell turn unarchiving implicitly, and gRPC parity.

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
use tokio::sync::Semaphore;
use tower::ServiceExt;

/// Answers every round with one text part, but only after the test adds a
/// permit to `gate`, so a turn stays running until released.
struct GatedProvider {
    gate: Arc<Semaphore>,
}

#[async_trait]
impl Provider for GatedProvider {
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
        let gate = Arc::clone(&self.gate);
        let part = PartId::new();
        let events = vec![
            Event::TextStart {
                session,
                message,
                part,
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: "done".to_string(),
            },
            Event::TextEnd {
                session,
                message,
                part,
            },
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ];
        let stream = futures::stream::once(async move {
            gate.acquire().await.unwrap().forget();
        })
        .flat_map(move |()| futures::stream::iter(events.clone().into_iter().map(Ok)));
        Ok(Box::pin(stream))
    }
}

async fn state(gate: Arc<Semaphore>) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(GatedProvider { gate })));
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

async fn create(app: &axum::Router, parent: Option<&str>) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/sessions",
        json!({
            "agent": "build",
            "model": "fake",
            "workdir": std::env::temp_dir().to_string_lossy(),
            "parent": parent.unwrap_or_default(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session"]["id"].as_str().unwrap().to_owned()
}

async fn set_archived(app: &axum::Router, session: &str, archived: bool) -> (StatusCode, Value) {
    call(
        app,
        Method::PATCH,
        &format!("/v1/sessions/{session}"),
        json!({ "archived": archived }),
    )
    .await
}

async fn info(app: &axum::Router, session: &str) -> Value {
    let (status, body) = call(
        app,
        Method::GET,
        &format!("/v1/sessions/{session}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn listed(app: &axum::Router, query: &str) -> Vec<String> {
    let (status, body) = call(
        app,
        Method::GET,
        &format!("/v1/sessions{query}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["sessions"].as_array().map_or_else(Vec::new, |rows| {
        rows.iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect()
    })
}

async fn prompt(app: &axum::Router, session: &str, text: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({ "prompt": { "text": text } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["turn"]["id"].as_str().unwrap().to_owned()
}

async fn wait(app: &axum::Router, session: &str, turn: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn archive_flags_the_root_and_list_filters_it() {
    let app = router(state(Arc::new(Semaphore::new(0))).await);
    let root = create(&app, None).await;
    let other = create(&app, None).await;
    let child = create(&app, Some(&root)).await;
    let fresh = info(&app, &root).await;
    assert!(fresh.get("archived").is_none(), "{fresh}");
    assert!(fresh.get("archivedAt").is_none(), "{fresh}");

    let (status, body) = set_archived(&app, &root, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["archived"], json!(true));
    assert!(body["archivedAt"].is_string(), "{body}");
    let stamp = body["archivedAt"].clone();
    // Archiving again keeps the original stamp.
    let (status, again) = set_archived(&app, &root, true).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["archivedAt"], stamp);

    // Children are not archived themselves and cannot be.
    assert!(info(&app, &child).await.get("archived").is_none());
    let (status, body) = set_archived(&app, &child, true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("invalid_argument"));

    let default = listed(&app, "").await;
    assert!(!default.contains(&root), "{default:?}");
    assert!(default.contains(&other) && default.contains(&child));
    let all = listed(&app, "?includeArchived=true").await;
    assert!(all.contains(&root) && all.contains(&other) && all.contains(&child));
    assert_eq!(
        listed(&app, "?archivedOnly=true").await,
        std::slice::from_ref(&root)
    );

    let (status, body) = set_archived(&app, &root, false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("archived").is_none(), "{body}");
    assert!(body.get("archivedAt").is_none(), "{body}");
    assert!(listed(&app, "").await.contains(&root));
    assert!(listed(&app, "?archivedOnly=true").await.is_empty());

    // Both changes replay as `sessionUpdated` frames on the root.
    let (status, events) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{root}/events"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{events}");
    let flags: Vec<Value> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event.get("sessionUpdated"))
        .filter_map(|updated| updated.get("archived").cloned())
        .collect();
    assert_eq!(flags, [json!(true), json!(false)]);
}

#[tokio::test]
async fn archiving_a_running_session_lets_its_turn_finish() {
    let gate = Arc::new(Semaphore::new(0));
    let app = router(state(Arc::clone(&gate)).await);
    let root = create(&app, None).await;
    let turn = prompt(&app, &root, "work").await;
    assert_eq!(info(&app, &root).await["busy"], json!(true));

    let (status, body) = set_archived(&app, &root, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["archived"], json!(true));
    assert_eq!(body["busy"], json!(true), "archiving does not cancel");

    gate.add_permits(1);
    let finished = wait(&app, &root, &turn).await;
    assert_eq!(
        finished["state"],
        json!("TURN_STATE_FINISHED"),
        "{finished}"
    );
    assert_eq!(
        finished["finish"],
        json!("FINISH_REASON_STOP"),
        "{finished}"
    );
    let after = info(&app, &root).await;
    assert_eq!(after["archived"], json!(true), "{after}");
}

#[tokio::test]
async fn a_new_prompt_or_shell_turn_unarchives_the_session() {
    let gate = Arc::new(Semaphore::new(0));
    let app = router(state(Arc::clone(&gate)).await);
    let root = create(&app, None).await;
    assert_eq!(set_archived(&app, &root, true).await.0, StatusCode::OK);

    // The stream announces the implicit unarchive to other clients.
    let stream_app = app.clone();
    let uri = format!("/v1/sessions/{root}/events/stream");
    let announced = tokio::spawn(async move {
        let resp = stream_app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let mut stream = resp.into_body().into_data_stream();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            let Ok(Some(Ok(bytes))) =
                tokio::time::timeout(Duration::from_millis(500), stream.next()).await
            else {
                continue;
            };
            for line in String::from_utf8_lossy(&bytes).lines() {
                if let Some(data) = line.strip_prefix("data:")
                    && let Ok(frame) = serde_json::from_str::<Value>(data.trim())
                    && frame["event"]["sessionUpdated"]["archived"] == json!(false)
                {
                    return true;
                }
            }
        }
        false
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    gate.add_permits(1);
    let turn = prompt(&app, &root, "again").await;
    assert!(info(&app, &root).await.get("archived").is_none());
    assert!(announced.await.unwrap(), "no sessionUpdated archived=false");
    wait(&app, &root, &turn).await;

    assert_eq!(set_archived(&app, &root, true).await.0, StatusCode::OK);
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{root}/turns"),
        json!({ "shell": { "command": "true" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(info(&app, &root).await.get("archived").is_none());
}

#[tokio::test]
async fn archive_matches_over_grpc() {
    use tonic::transport::Server;
    let app_state = state(Arc::new(Semaphore::new(0))).await;
    let app = router(app_state.clone());
    let grpc = V1Grpc::new(app_state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serve = Server::builder()
        .add_service(pb::session_server::SessionServer::new(grpc))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(async move {
        let _ = serve.await;
    });
    let channel = tonic::transport::Channel::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut session = pb::session_client::SessionClient::new(channel);

    let root = create(&app, None).await;
    let other = create(&app, None).await;
    let updated = session
        .update_session(tonic::Request::new(pb::UpdateSessionRequest {
            session: root.clone(),
            archived: Some(true),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(updated.archived);
    assert!(updated.archived_at.is_some());
    let http = info(&app, &root).await;
    assert_eq!(
        serde_json::to_value(&updated).unwrap()["archivedAt"],
        http["archivedAt"]
    );

    let list = |include_archived, archived_only| pb::ListSessionsRequest {
        include_archived,
        archived_only,
        ..Default::default()
    };
    let ids = |response: pb::ListSessionsResponse| -> Vec<String> {
        response.sessions.into_iter().map(|row| row.id).collect()
    };
    let default = ids(session
        .list_sessions(tonic::Request::new(list(false, false)))
        .await
        .unwrap()
        .into_inner());
    assert_eq!(default, std::slice::from_ref(&other));
    assert_eq!(default, listed(&app, "").await);
    let archived = ids(session
        .list_sessions(tonic::Request::new(list(false, true)))
        .await
        .unwrap()
        .into_inner());
    assert_eq!(archived, std::slice::from_ref(&root));
    let all = ids(session
        .list_sessions(tonic::Request::new(list(true, false)))
        .await
        .unwrap()
        .into_inner());
    assert_eq!(all, listed(&app, "?includeArchived=true").await);
    assert_eq!(all.len(), 2);
}
