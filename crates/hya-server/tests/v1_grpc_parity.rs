//! Dual-transport conformance: the same `/v1` calls through HTTP and gRPC
//! must produce identical responses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("grpc parity hello".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
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

async fn http_json(
    app: &axum::Router,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
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
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

/// Spawn the full tonic server on an ephemeral port and return the address.
async fn grpc_endpoint(app: AppState) -> std::net::SocketAddr {
    use tonic::transport::Server;
    let grpc = V1Grpc::new(app);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serve = Server::builder()
        .add_service(pb::process_server::ProcessServer::new(grpc.clone()))
        .add_service(pb::catalog_server::CatalogServer::new(grpc.clone()))
        .add_service(pb::agent_models_server::AgentModelsServer::new(
            grpc.clone(),
        ))
        .add_service(pb::auth_server::AuthServer::new(grpc.clone()))
        .add_service(pb::session_server::SessionServer::new(grpc.clone()))
        .add_service(pb::turn_server::TurnServer::new(grpc.clone()))
        .add_service(pb::messages_server::MessagesServer::new(grpc.clone()))
        .add_service(pb::events_server::EventsServer::new(grpc.clone()))
        .add_service(pb::interactions_server::InteractionsServer::new(
            grpc.clone(),
        ))
        .add_service(pb::workflow_server::WorkflowServer::new(grpc.clone()))
        .add_service(pb::files_server::FilesServer::new(grpc.clone()))
        .add_service(pb::project_server::ProjectServer::new(grpc.clone()))
        .add_service(pb::worktrees_server::WorktreesServer::new(grpc.clone()))
        .add_service(pb::mcp_server::McpServer::new(grpc.clone()))
        .add_service(pb::pty_server::PtyServer::new(grpc.clone()))
        .add_service(pb::logs_server::LogsServer::new(grpc.clone()))
        .add_service(pb::bundle_api_server::BundleApiServer::new(grpc.clone()))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(async move {
        let _ = serve.await;
    });
    address
}

fn normalize(value: &Value) -> Value {
    // Timestamps and ids differ between the two calls; the parity suite
    // compares shapes and stable fields by dropping volatile leaves.
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                if matches!(
                    key.as_str(),
                    "timeCreated" | "timeUpdated" | "timeRecorded" | "id"
                ) {
                    out.insert(key.clone(), Value::String("<volatile>".into()));
                } else {
                    out.insert(key.clone(), normalize(item));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        other => other.clone(),
    }
}

#[tokio::test]
async fn http_and_grpc_answers_match_across_representative_calls() {
    let app_state = state().await;
    let app = router(app_state.clone());
    let address = grpc_endpoint(app_state).await;
    let channel = tonic::transport::Channel::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut process = pb::process_client::ProcessClient::new(channel.clone());
    let mut catalog = pb::catalog_client::CatalogClient::new(channel.clone());
    let mut session = pb::session_client::SessionClient::new(channel.clone());
    let mut turn = pb::turn_client::TurnClient::new(channel.clone());
    let mut events = pb::events_client::EventsClient::new(channel.clone());

    // Health.
    let grpc_health: Value = serde_json::to_value(
        process
            .get_health(tonic::Request::new(pb::GetHealthRequest {}))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    let (status, http_health) = http_json(&app, Method::GET, "/v1/health", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(normalize(&grpc_health), normalize(&http_health));

    // Models.
    let grpc_models: Value = serde_json::to_value(
        catalog
            .list_models(tonic::Request::new(pb::ListModelsRequest::default()))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    let (status, http_models) = http_json(&app, Method::GET, "/v1/models", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(normalize(&grpc_models), normalize(&http_models));

    // Session lifecycle.
    let create = pb::CreateSessionRequest {
        agent: "build".into(),
        model: "fake".into(),
        workdir: std::env::temp_dir().to_string_lossy().into_owned(),
        ..Default::default()
    };
    let grpc_session: Value = serde_json::to_value(
        session
            .create_session(tonic::Request::new(create.clone()))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    let (status, http_session) = http_json(
        &app,
        Method::POST,
        "/v1/sessions",
        serde_json::to_value(&create).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{http_session}");
    assert_eq!(normalize(&grpc_session)["session"]["agent"], json!("build"));
    assert_eq!(normalize(&http_session)["session"]["agent"], json!("build"));

    let session_id = grpc_session["session"]["id"].as_str().unwrap().to_owned();

    // Permission modes: the catalog and the session mode match on both.
    let grpc_modes: Value = serde_json::to_value(
        catalog
            .list_permission_modes(tonic::Request::new(pb::ListPermissionModesRequest {}))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    let (status, http_modes) =
        http_json(&app, Method::GET, "/v1/permission-modes", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(grpc_modes, http_modes);
    assert_eq!(grpc_modes["modes"][1]["id"], json!("yolo"));
    let grpc_updated: Value = serde_json::to_value(
        session
            .update_session(tonic::Request::new(pb::UpdateSessionRequest {
                session: session_id.clone(),
                permission_mode: Some("yolo".into()),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    assert_eq!(grpc_updated["permissionMode"], json!("yolo"));
    let (status, http_updated) = http_json(
        &app,
        Method::PATCH,
        &format!("/v1/sessions/{session_id}"),
        json!({"permissionMode": "manual"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{http_updated}");
    assert_eq!(http_updated["permissionMode"], json!("manual"));
    let grpc_invalid = session
        .update_session(tonic::Request::new(pb::UpdateSessionRequest {
            session: session_id.clone(),
            permission_mode: Some("danger".into()),
            ..Default::default()
        }))
        .await;
    assert_eq!(
        grpc_invalid.unwrap_err().code(),
        tonic::Code::InvalidArgument
    );

    // Turn through gRPC, then wait to terminal.
    let admitted = turn
        .create_turn(tonic::Request::new(pb::CreateTurnRequest {
            session: session_id.clone(),
            kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                text: "say hello".into(),
            })),
        }))
        .await
        .unwrap()
        .into_inner();
    let turn_id = admitted.turn.unwrap().id;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut finished = pb::TurnInfo::default();
    while std::time::Instant::now() < deadline {
        finished = turn
            .get_turn(tonic::Request::new(pb::GetTurnRequest {
                session: session_id.clone(),
                turn: turn_id.clone(),
            }))
            .await
            .unwrap()
            .into_inner();
        if finished.state == pb::TurnState::Finished as i32 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(finished.state, pb::TurnState::Finished as i32);
    assert_eq!(finished.finish, pb::FinishReason::Stop as i32);

    // Replay through both transports and compare shapes.
    let grpc_events: Value = serde_json::to_value(
        events
            .list_events(tonic::Request::new(pb::ListEventsRequest {
                session: session_id.clone(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    let (status, http_events) = http_json(
        &app,
        Method::GET,
        &format!("/v1/sessions/{session_id}/events"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        grpc_events["events"].as_array().map(Vec::len),
        http_events["events"].as_array().map(Vec::len),
        "both transports must replay the same curated event count"
    );

    // Agent-models parity: both transports report the missing control.
    let mut agent_models = pb::agent_models_client::AgentModelsClient::new(channel.clone());
    let grpc_models = agent_models
        .list_agent_models(tonic::Request::new(pb::ListAgentModelsRequest::default()))
        .await;
    assert!(grpc_models.is_err());
    assert_eq!(grpc_models.unwrap_err().code(), tonic::Code::Unavailable);
    let (status, body) = http_json(&app, Method::GET, "/v1/agent-models", Value::Null).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], json!("unavailable"));

    // Error parity: unknown session maps to the same code on both.
    let missing = "00000000-0000-0000-0000-000000000000".to_owned();
    let grpc_error = session
        .get_session(tonic::Request::new(pb::GetSessionRequest {
            session: missing.clone(),
        }))
        .await;
    assert!(grpc_error.is_err());
    assert_eq!(grpc_error.unwrap_err().code(), tonic::Code::NotFound);
    let (status, body) = http_json(
        &app,
        Method::GET,
        &format!("/v1/sessions/{missing}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("session_not_found"));
}
