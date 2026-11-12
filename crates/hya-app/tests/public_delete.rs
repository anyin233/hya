//! Public delete must join the real Started Workflow owner before deleting logs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_app::{InvocationPolicy, WebSearchConfig, agent_with_model, build_session_engine};
use hya_core::CreateSession;
use hya_proto::{MessageId, ModelRef, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_store::SessionStore;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tower::ServiceExt;

struct PausedStage(Arc<Semaphore>);

#[async_trait]
impl Provider for PausedStage {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            max_context: 200_000,
            ..Default::default()
        })
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        _: SessionId,
        _: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.0.add_permits(1);
        Ok(Box::pin(futures::stream::pending()))
    }
}

async fn call(app: &axum::Router, method: Method, uri: String, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn delete_started_workflow(grpc: bool) {
    let directory = std::env::temp_dir().join(format!("hya-delete-workflow-{}", SessionId::new()));
    std::fs::create_dir_all(directory.join(".hya/workflows")).unwrap();
    std::fs::write(
        directory.join(".hya/workflows/paused.hya.md"),
        r#"---
kind: Workflow
name: paused
description: Deletion ordering regression.
on_failure: collect_all
nodes:
  first:
    agent: hya-task
    model:
      id: fake/model
    directive: Pause here.
  second:
    agent: hya-task
    model:
      id: fake/model
    directive: Must never start after deletion.
---
flowchart TD
  first --> second
"#,
    )
    .unwrap();
    let entered = Arc::new(Semaphore::new(0));
    let provider = ProviderRouter::new().with(Arc::new(PausedStage(entered.clone())));
    let mut agent = agent_with_model("fake/model", None);
    agent.workdir = directory.clone();
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        ProviderRouter::new(),
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            hya_app::config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let engine = built.engine();
    let supervisor = built.resident_supervisor();
    engine.publish_provider_catalog(Arc::new(provider), engine.provider_catalog_snapshot());
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: directory.display().to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let control = Arc::new(built.workflow_control());
    let state = hya_server::AppState::new(engine.clone(), Arc::new(agent))
        .with_workflow_control(control.clone());
    let app = hya_server::router(state.clone());
    let (status, response) = call(
        &app,
        Method::POST,
        format!("/v1/sessions/{root}/workflow"),
        json!({"run": {"name": "paused"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    tokio::time::timeout(Duration::from_secs(5), entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let sessions: Vec<_> = engine
        .store()
        .list_sessions()
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.session)
        .collect();
    assert!(sessions.len() >= 2);
    assert!(hya_server::WorkflowControl::active_run(control.as_ref(), root).is_some());
    if grpc {
        pb::session_server::Session::delete_session(
            &hya_server::V1Grpc::new(state),
            tonic::Request::new(pb::DeleteSessionRequest {
                session: root.to_string(),
            }),
        )
        .await
        .unwrap();
    } else {
        let (status, response) = call(
            &app,
            Method::DELETE,
            format!("/v1/sessions/{root}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
    }
    assert!(
        hya_server::WorkflowControl::active_run(control.as_ref(), root).is_none(),
        "delete replied before Workflow writer settled"
    );
    // Runtime shutdown joins the entire composition subtree: all possible late
    // terminal events must have happened before the final event-log assertions.
    built.shutdown().await.unwrap();
    assert_eq!(supervisor.episode_count(), 0);
    assert!(supervisor.team_cancel(root).is_none());
    assert!(engine.store().active_actor_ids().await.unwrap().is_empty());
    assert!(engine.store().list_sessions().await.unwrap().is_empty());
    for session in sessions {
        assert!(!engine.store().session_exists(session).await.unwrap());
        assert!(
            engine.replay(session).await.unwrap().is_empty(),
            "post-delete event for {session}"
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn http_delete_settles_started_workflow() {
    delete_started_workflow(false).await;
}

#[tokio::test]
async fn grpc_delete_settles_started_workflow() {
    delete_started_workflow(true).await;
}
