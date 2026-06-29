#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use hya_core::{CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::ProviderRouter;
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

async fn engine() -> SessionEngine {
    let store = SessionStore::connect_memory().await.unwrap();
    let providers = Arc::new(ProviderRouter::new());
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    SessionEngine::new(store, providers, tools, permission, EventBus::default())
}

fn spec() -> CreateSession {
    CreateSession {
        parent: None,
        agent: AgentName::new("build"),
        model: ModelRef::new("fake"),
        workdir: "/tmp/hya-core-session-cleanup".to_string(),
    }
}

#[tokio::test]
async fn cleanup_empty_unnamed_session_deletes_empty_session() {
    let engine = engine().await;
    let session = engine.create(spec()).await.unwrap();

    let deleted = engine.cleanup_empty_unnamed_session(session).await.unwrap();

    assert!(deleted);
    assert!(engine.replay(session).await.unwrap().is_empty());
}

#[tokio::test]
async fn cleanup_empty_unnamed_session_keeps_non_empty_session() {
    let engine = engine().await;
    let session = engine.create(spec()).await.unwrap();
    engine
        .admit_user_prompt(session, "hello".to_string())
        .await
        .unwrap();

    let deleted = engine.cleanup_empty_unnamed_session(session).await.unwrap();

    assert!(!deleted);
    assert!(!engine.replay(session).await.unwrap().is_empty());
}

#[tokio::test]
async fn cleanup_empty_unnamed_session_keeps_manually_titled_session() {
    let engine = engine().await;
    let session = engine.create(spec()).await.unwrap();
    engine
        .set_title(session, "Manual title".to_string())
        .await
        .unwrap();

    let deleted = engine.cleanup_empty_unnamed_session(session).await.unwrap();

    assert!(!deleted);
    assert!(!engine.replay(session).await.unwrap().is_empty());
}

#[tokio::test]
async fn cleanup_empty_unnamed_session_is_idempotent_after_delete() {
    let engine = engine().await;
    let session = engine.create(spec()).await.unwrap();

    assert!(engine.cleanup_empty_unnamed_session(session).await.unwrap());
    let deleted_again = engine.cleanup_empty_unnamed_session(session).await.unwrap();

    assert!(!deleted_again);
}
