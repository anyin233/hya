//! Mail refusals are client errors on both transports, including user steering.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, ModelRef, SessionId, SubagentMode};
use hya_provider::ProviderRouter;
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn fixture() -> (AppState, SessionId, SessionId) {
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new()),
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
    let spec = CreateSession {
        parent: None,
        agent: agent.name.clone(),
        model: agent.model.clone(),
        workdir: agent.workdir.to_string_lossy().into_owned(),
        project: None,
        kind: hya_proto::SessionKind::Project,
    };
    let root = engine.create(spec).await.unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    for (session, handle, parent, mode) in [
        (root, "main", None, SubagentMode::Transient),
        (
            child,
            "worker-1",
            Some("main".to_string()),
            SubagentMode::Resident,
        ),
    ] {
        engine
            .store()
            .append_event(
                root,
                &Event::AgentRegistered {
                    session: root,
                    agent_session: session,
                    handle: handle.into(),
                    parent,
                    agent_type: agent.name.clone(),
                    mode,
                },
            )
            .await
            .unwrap();
    }
    engine
        .store()
        .append_event(
            root,
            &Event::ChannelCreated {
                session: root,
                channel: "DM-12345678".into(),
                kind: hya_proto::ChannelKind::Dm,
                unit: None,
                members: vec!["main".into(), "main/worker-1".into()],
            },
        )
        .await
        .unwrap();
    (AppState::new(engine, agent), root, child)
}

#[tokio::test]
async fn steer_mail_rejections_are_permission_denied_in_http_and_grpc() {
    let (state, root, child) = fixture().await;
    let before = state.engine.replay(root).await.unwrap().len();
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{child}/steer"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"text": "please continue"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "permission_denied");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("sender `main`"), "{message}");
    assert!(message.contains("DM-12345678"), "{message}");
    let grpc = pb::session_server::Session::steer_subagent(
        &V1Grpc::new(state.clone()),
        tonic::Request::new(pb::SteerSubagentRequest {
            session: child.to_string(),
            text: "please continue".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(grpc.code(), tonic::Code::PermissionDenied);
    assert_eq!(grpc.message(), message);
    assert_eq!(state.engine.replay(root).await.unwrap().len(), before);
}
