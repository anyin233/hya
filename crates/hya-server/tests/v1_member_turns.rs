//! Team member identity gates client mutations before turn preparation.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{AgentSpec, CreateSession, EventBus, ResidentSupervisor, SessionEngine};
use hya_proto::{AgentName, Event, MessageId, ModelRef, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

struct WaitingProvider(tokio::sync::Notify);
#[async_trait]
impl Provider for WaitingProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        _: SessionId,
        _: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.0.notify_one();
        Ok(Box::pin(futures::stream::pending()))
    }
}

async fn fixture() -> (AppState, Arc<ResidentSupervisor>, SessionId, SessionId) {
    let provider = Arc::new(WaitingProvider(tokio::sync::Notify::new()));
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(provider.clone())),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    ));
    let agent = Arc::new(AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    });
    let spec = || CreateSession {
        parent: None,
        agent: agent.name.clone(),
        model: agent.model.clone(),
        workdir: agent.workdir.to_string_lossy().into_owned(),
        project: None,
        kind: hya_proto::SessionKind::Project,
    };
    let root = engine.create(spec()).await.unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            ..spec()
        })
        .await
        .unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("build").unwrap();
    supervisor
        .ensure_main(
            root,
            (*agent).clone(),
            (binding, Arc::from([]), resources),
            None,
            None,
        )
        .await
        .unwrap();
    supervisor
        .register_existing_resident(
            root,
            child,
            "worker".into(),
            (*agent).clone(),
            Some("work".into()),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), provider.0.notified())
        .await
        .unwrap();
    assert!(engine.cancel_user_turn(child));
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine.turn_active(child) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(engine.stopped_by_user(child));
    (AppState::new(engine, agent), supervisor, root, child)
}

async fn post(
    state: &AppState,
    child: SessionId,
    suffix: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = router(state.clone())
        .oneshot(
            Request::post(format!("/v1/sessions/{child}/{suffix}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, body)
}

#[tokio::test]
async fn stopped_member_prompt_refuses_before_side_effects_on_http_and_grpc() {
    refused_turn(json!({"prompt": {"text": "must not run"}})).await;
}

#[tokio::test]
async fn stopped_member_command_refuses_before_model_switch_on_http_and_grpc() {
    refused_turn(
        json!({"command": {"command": "custom", "text": "must not run", "model": "other"}}),
    )
    .await;
}

#[tokio::test]
async fn stopped_member_command_refuses_before_workflow_interception_on_http_and_grpc() {
    // A syntax error would prove slash interception ran before the gate.
    refused_turn(json!({"command": {"command": "workflow", "arguments": "invalid-operation", "model": "other"}})).await;
}

#[tokio::test]
async fn stopped_member_shell_refuses_before_preparation_on_http_and_grpc() {
    refused_turn(json!({"shell": {"command": "echo must-not-run"}})).await;
}

async fn refused_turn(body: Value) {
    use pb::turn_server::Turn;
    let (state, supervisor, root, child) = fixture().await;
    let before = state.engine.replay(child).await.unwrap().len();
    let (status, response) = post(&state, child, "turns", body.clone()).await;
    assert!(
        state.engine.stopped_by_user(child),
        "rejected HTTP request lifted user stop: {response}"
    );
    assert_eq!(
        state.engine.replay(child).await.unwrap().len(),
        before,
        "rejected request changed member log"
    );
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert_eq!(response["error"]["code"], "invalid_argument");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("steer this subagent with mail"),
        "{response}"
    );
    let mut request: pb::CreateTurnRequest = serde_json::from_value(body).unwrap();
    request.session = child.to_string();
    let error = V1Grpc::new(state.clone())
        .create_turn(tonic::Request::new(request))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(
        error.message().contains("steer this subagent with mail"),
        "{error}"
    );
    assert!(state.engine.stopped_by_user(child));
    assert_eq!(state.engine.replay(child).await.unwrap().len(), before);
    supervisor.end_member(root, "main/worker").await.unwrap();
}

#[tokio::test]
async fn archived_member_create_turn_is_a_client_error_and_appends_nothing() {
    use pb::turn_server::Turn;
    let (state, supervisor, root, child) = fixture().await;
    supervisor.end_member(root, "main/worker").await.unwrap();
    let before = state.engine.replay(child).await.unwrap().len();
    let (status, body) = post(
        &state,
        child,
        "turns",
        json!({"prompt": {"text": "must not run"}}),
    )
    .await;
    assert_eq!(
        state.engine.replay(child).await.unwrap().len(),
        before,
        "archived member log changed: {body}"
    );
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("steer this subagent with mail")
    );
    let error = V1Grpc::new(state.clone())
        .create_turn(tonic::Request::new(pb::CreateTurnRequest {
            session: child.to_string(),
            kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                text: "must not run".into(),
                ..Default::default()
            })),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(error.message().contains("steer this subagent with mail"));
    assert_eq!(state.engine.replay(child).await.unwrap().len(), before);
    assert!(
        state
            .engine
            .read_projection(root)
            .await
            .unwrap()
            .team
            .archived
            .values()
            .any(|e| e.session == child)
    );
}

#[tokio::test]
async fn member_fork_copies_a_snapshot_without_mutating_the_source() {
    let (state, supervisor, root, child) = fixture().await;
    for archived in [false, true] {
        if archived {
            supervisor.end_member(root, "main/worker").await.unwrap();
        }
        let before = state.engine.replay(child).await.unwrap().len();
        let (status, body) = post(&state, child, "fork", json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let fork: SessionId = body["session"]["id"].as_str().unwrap().parse().unwrap();
        assert!(
            state
                .engine
                .read_projection(fork)
                .await
                .unwrap()
                .session
                .parent
                .is_none()
        );
        state
            .engine
            .admit_user_prompt(fork, "independent root".into())
            .await
            .unwrap();
        assert_eq!(state.engine.replay(child).await.unwrap().len(), before);
    }
    assert!(
        !state
            .engine
            .replay(child)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(
                e.event,
                Event::ModelSwitched { .. } | Event::SessionUnarchived { .. }
            ))
    );
}

#[tokio::test]
async fn member_model_agent_and_revert_mutations_are_client_errors() {
    let (state, supervisor, root, child) = fixture().await;
    for archived in [false, true] {
        if archived {
            supervisor.end_member(root, "main/worker").await.unwrap();
        }
        for body in [
            json!({"model": "other", "title": "must not change"}),
            json!({"agent": "other", "title": "must not change"}),
        ] {
            let before = state.engine.replay(child).await.unwrap().len();
            let response = router(state.clone())
                .oneshot(
                    Request::patch(format!("/v1/sessions/{child}"))
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let response: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert!(
                response["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("steer this subagent with mail"),
                "{response}"
            );
            assert_eq!(state.engine.replay(child).await.unwrap().len(), before);
        }
        for undo in [false, true] {
            let before = state.engine.replay(child).await.unwrap().len();
            let (status, response) = post(&state, child, "revert", json!({"undo": undo})).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
            assert!(
                response["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("steer this subagent with mail"),
                "{response}"
            );
            assert_eq!(state.engine.replay(child).await.unwrap().len(), before);
        }
    }
}
