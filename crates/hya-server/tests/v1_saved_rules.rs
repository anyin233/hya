//! v1 saved permission rules: rows are "allow always" grants reported as
//! `RULE_PERMISSION_ALLOW` with their creation time, saved grants reload into
//! the permission plane at startup, and deleting a rule revokes the live grant.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::{SavedPermission, SessionStore};
use hya_tool::permission::{
    AskRequest, Invocation, InvocationPolicy, InvocationRule, Mode, PermissionModel,
    PermissionTarget,
};
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::Value;
use tokio::sync::mpsc;
use tower::ServiceExt;

async fn state_with_plane(
    store: SessionStore,
) -> (
    AppState,
    PermissionPlane,
    mpsc::UnboundedReceiver<AskRequest>,
) {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let policy = InvocationPolicy::compile(
        PermissionModel::Default,
        vec![InvocationRule::new(
            PermissionTarget::Tool,
            "^write$",
            Mode::Ask,
        )],
    )
    .unwrap();
    let (perm, asks) = PermissionPlane::new_with_policy(PermissionRules::default(), policy);
    let engine = SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
        perm.clone(),
        EventBus::default(),
    );
    let state = AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    );
    (state, perm, asks)
}

async fn call(app: &axum::Router, method: Method, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
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

async fn authorized_without_ask(
    plane: &PermissionPlane,
    asks: &mut mpsc::UnboundedReceiver<AskRequest>,
) -> bool {
    let task = {
        let plane = plane.clone();
        tokio::spawn(async move {
            plane
                .authorize(&Invocation::tool("write", Mode::Ask))
                .await
                .is_ok()
        })
    };
    match tokio::time::timeout(Duration::from_millis(300), asks.recv()).await {
        Ok(Some(req)) => {
            let _sent = req.reply.send(hya_tool::Decision::AllowOnce);
            let _ok = task.await.unwrap();
            false
        }
        _ => task.await.unwrap(),
    }
}

#[tokio::test]
async fn saved_rules_report_allow_time_reload_and_revoke() {
    let store = SessionStore::connect_memory().await.unwrap();
    store
        .save_permission(&SavedPermission {
            id: "psv_write".to_string(),
            project_id: "global".to_string(),
            action: "tool".to_string(),
            resource: "write".to_string(),
            time_created_ms: Some(1_700_000_000_000),
        })
        .await
        .unwrap();
    let (state, plane, mut asks) = state_with_plane(store).await;

    let restored = state.restore_saved_permissions().await.unwrap();
    assert_eq!(restored, 1);
    assert!(
        authorized_without_ask(&plane, &mut asks).await,
        "a saved grant must be live after startup restore"
    );

    let app = router(state);
    let (status, body) = call(&app, Method::GET, "/v1/permissions/rules").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rule = &body["rules"][0];
    assert_eq!(rule["id"], "psv_write");
    assert_eq!(rule["permission"], "RULE_PERMISSION_ALLOW");
    assert_eq!(rule["tool"], "write");
    assert_eq!(rule["timeCreated"], "2023-11-14T22:13:20+00:00");

    let (status, body) = call(&app, Method::DELETE, "/v1/permissions/rules/psv_write").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !authorized_without_ask(&plane, &mut asks).await,
        "deleting a rule must revoke the in-memory grant"
    );
    let (_status, body) = call(&app, Method::GET, "/v1/permissions/rules").await;
    assert_eq!(body["rules"].as_array().map(Vec::len).unwrap_or(0), 0);
}
