//! `/v1/sessions/{session}/views[/{bundle}/{view}]`: bundle views resolved in
//! the live runtime generation, over HTTP and gRPC, with stable error codes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_api::v1::session_server::Session as _;
use hya_core::{
    AgentSpec, BundleViewProvider, EventBus, RuntimeSource, RuntimeSourceId, SessionEngine,
    SourceView,
};
use hya_proto::{AgentName, ModelRef, SessionId};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const BUNDLE: &str = "hya/server-tests-viewer";
const BUNDLE_SEGMENT: &str = "hya%2Fserver-tests-viewer";

/// Echoes the request; the `broken` view fails like a crashed process.
struct EchoViews;

#[async_trait]
impl BundleViewProvider for EchoViews {
    async fn get_view(
        &self,
        view: &str,
        session: SessionId,
        query: BTreeMap<String, String>,
    ) -> Result<Value, String> {
        if view == "broken" {
            return Err("plugin connection closed".to_string());
        }
        Ok(json!({
            "view": view,
            "session": session,
            "query": query,
            "big": 12_345_678_901_u64,
        }))
    }
}

async fn state() -> AppState {
    let providers =
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(Vec::new()))));
    let tools = Arc::new(ToolRegistry::builtins());
    let runtime = support::runtime_with_catalog(
        tools,
        &[
            support::AgentFixture::main("build"),
            support::AgentFixture::subagent("viewer"),
        ],
    );
    runtime
        .refresh(|candidate| {
            candidate.upsert_sources(vec![
                RuntimeSource::new(
                    RuntimeSourceId::bundle(BUNDLE),
                    [7; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_views(
                    vec![
                        SourceView {
                            id: "usage".into(),
                            description: "Token usage".into(),
                        },
                        SourceView {
                            id: "broken".into(),
                            description: String::new(),
                        },
                    ],
                    Arc::new(EchoViews),
                ),
            ])
        })
        .unwrap();
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, runtime, perm, EventBus::default());
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

async fn send(app: axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = app
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
    let (status, body) = send(
        app.clone(),
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

#[tokio::test]
async fn views_are_listed_and_served_with_stable_errors() {
    let app = router(state().await);
    let session = create_session(&app).await;

    let (status, list) = send(
        app.clone(),
        Method::GET,
        &format!("/v1/sessions/{session}/views"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(
        list["views"],
        json!([
            {"bundle": BUNDLE, "view": "broken"},
            {"bundle": BUNDLE, "view": "usage", "description": "Token usage"},
        ])
    );

    let (status, view) = send(
        app.clone(),
        Method::GET,
        &format!("/v1/sessions/{session}/views/{BUNDLE_SEGMENT}/usage?scope=tree&x=1"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["bundle"], BUNDLE);
    assert_eq!(view["view"], "usage");
    assert_eq!(view["contentType"], "application/json");
    assert_eq!(view["body"]["session"], session);
    assert_eq!(view["body"]["query"], json!({"scope": "tree", "x": "1"}));
    assert_eq!(
        view["body"]["big"].as_u64(),
        Some(12_345_678_901),
        "HTTP bodies keep integers exact"
    );

    for (uri, status, code) in [
        (
            format!(
                "/v1/sessions/{}/views/{BUNDLE_SEGMENT}/usage",
                SessionId::new()
            ),
            StatusCode::NOT_FOUND,
            "session_not_found",
        ),
        (
            format!("/v1/sessions/{session}/views/hya%2Fmissing/usage"),
            StatusCode::NOT_FOUND,
            "view_not_found",
        ),
        (
            format!("/v1/sessions/{session}/views/{BUNDLE_SEGMENT}/missing"),
            StatusCode::NOT_FOUND,
            "view_not_found",
        ),
        (
            format!("/v1/sessions/{session}/views/{BUNDLE_SEGMENT}/broken"),
            StatusCode::BAD_GATEWAY,
            "view_failed",
        ),
        (
            format!("/v1/sessions/{}/views", SessionId::new()),
            StatusCode::NOT_FOUND,
            "session_not_found",
        ),
    ] {
        let (actual, body) = send(app.clone(), Method::GET, &uri, Value::Null).await;
        assert_eq!(actual, status, "{uri}: {body}");
        assert_eq!(body["error"]["code"], code, "{uri}");
    }
}

#[tokio::test]
async fn grpc_get_session_view_matches_http() {
    let state = state().await;
    let app = router(state.clone());
    let session = create_session(&app).await;
    let grpc = V1Grpc::new(state);

    let view = grpc
        .get_session_view(tonic::Request::new(pb::GetSessionViewRequest {
            session: session.clone(),
            bundle: BUNDLE.into(),
            view: "usage".into(),
            query: [("scope".to_string(), "session".to_string())].into(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(view.bundle, BUNDLE);
    assert_eq!(view.content_type, "application/json");
    let body = serde_json::to_value(view.body.unwrap()).unwrap();
    assert_eq!(body["query"]["scope"], "session");
    assert_eq!(body["session"], session);

    let listed = grpc
        .list_session_views(tonic::Request::new(pb::ListSessionViewsRequest {
            session: session.clone(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.views.len(), 2);

    let missing = grpc
        .get_session_view(tonic::Request::new(pb::GetSessionViewRequest {
            session,
            bundle: BUNDLE.into(),
            view: "missing".into(),
            query: Default::default(),
        }))
        .await
        .unwrap_err();
    assert_eq!(missing.code(), tonic::Code::NotFound);
}
