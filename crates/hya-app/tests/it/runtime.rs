//! Runtime construction through the public application API.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{body::Body, http::Request};
use hya_app::{HyaRuntime, OfflineNotice, RuntimeOptions, builtin_agent_catalog};
use std::path::PathBuf;
use tower::ServiceExt;

#[test]
fn builtin_agent_catalog_includes_first_party_workflow_bundle() {
    let catalog = builtin_agent_catalog().expect("builtin agent catalog must build");
    assert!(
        catalog.bundles().bundles().len() >= 2,
        "fresh process includes the immutable first-party bundles"
    );
    assert!(
        catalog
            .bundles()
            .resolve_workflow("plan-impl-review")
            .is_some(),
        "first-party Workflow must be cataloged"
    );
    let goal = catalog
        .bundles()
        .bundles()
        .iter()
        .find(|bundle| bundle.identity().id == "hya/goal")
        .expect("goal first-party bundle");
    assert_eq!(goal.kind(), hya_bundle::PreparedBundleKind::Bundle);
    assert_eq!(goal.agents().len(), 2);
    for skill in ["goal-contract", "guided-goal", "evaluator-prompt"] {
        assert!(
            goal.skills()
                .iter()
                .any(|resource| resource.local_id == skill)
        );
    }
    assert!(
        catalog
            .bundles()
            .bundles()
            .iter()
            .all(|bundle| bundle.identity().id != "hya/goal-loop"),
        "legacy hya/goal-loop must be retired once hya/goal and hya/loop are extracted"
    );
    let loop_bundle = catalog
        .bundles()
        .bundles()
        .iter()
        .find(|bundle| bundle.identity().id == "hya/loop")
        .expect("loop first-party bundle");
    assert_eq!(loop_bundle.kind(), hya_bundle::PreparedBundleKind::Bundle);
    for skill in ["loop-verifier-prompt", "loop-planner-prompt"] {
        assert!(
            loop_bundle
                .skills()
                .iter()
                .any(|resource| resource.local_id == skill)
        );
    }
    for id in [
        "hya-main",
        "hya-plan",
        "hya-scout",
        "hya-task",
        "hya-reviewer",
    ] {
        assert!(
            catalog.resolve(id).is_some(),
            "builtin `{id}` must resolve without any installed bundle"
        );
    }
}

#[test]
fn offline_notice_names_path_offline_mode_and_the_fix() {
    let notice = OfflineNotice {
        config_path: PathBuf::from("/home/u/.config/hya/config.yaml"),
    };
    let text = notice.render();
    // (a) where the missing config is expected,
    assert!(text.contains("/home/u/.config/hya/config.yaml"));
    // (b) that we are in offline/echo mode,
    assert!(text.contains("OFFLINE"));
    assert!(text.contains("echoes"));
    // (c) how to fix it.
    assert!(text.contains("hya login"));
    assert!(text.contains("docs/configuration.md"));
}

#[tokio::test]
async fn hya_runtime_starts_and_shuts_down_an_offline_server() {
    let mut runtime = HyaRuntime::start(RuntimeOptions {
        model: None,
        db: String::new(),
        yolo: false,
        default_agent: None,
        force_offline: true,
    })
    .await
    .unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &runtime.engine(),
        &runtime.app_state().engine
    ));
    let response = runtime
        .router()
        .clone()
        .oneshot(Request::get("/v2/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let state = runtime.app_state();
    let store = state.engine.store();
    let owner = hya_proto::OwnerRunId::new();
    assert!(matches!(
        store.claim_runtime_owner(owner),
        Err(hya_store::StoreError::RuntimeOwnerBusy)
    ));
    runtime.shutdown().await.unwrap();
    assert_eq!(
        state.streams().reason(),
        Some(hya_server::ShutdownReason::Stop)
    );
    store.claim_runtime_owner(owner).unwrap();
    store.release_runtime_owner(owner).unwrap();
}
