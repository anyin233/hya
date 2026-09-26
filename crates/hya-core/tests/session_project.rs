//! Integration tests for `hya-core`: ADR-0024 session Project and kind.
//! A subagent session's `SessionCreated` carries its parent's Project and kind.

#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use hya_core::{CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, ModelRef, ProjectId, SessionId, SessionKind};
use hya_provider::ProviderRouter;
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

async fn engine() -> SessionEngine {
    let store = SessionStore::connect_memory().await.unwrap();
    let providers = Arc::new(ProviderRouter::new());
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
        permission,
        EventBus::default(),
    )
}

fn spec(parent: Option<SessionId>) -> CreateSession {
    CreateSession {
        parent,
        agent: AgentName::new("build"),
        model: ModelRef::new("fake"),
        workdir: "/tmp/hya-core-session-project".to_string(),
    }
}

async fn root_with(
    engine: &SessionEngine,
    project: Option<ProjectId>,
    kind: SessionKind,
) -> SessionId {
    let root = SessionId::new();
    engine
        .store()
        .append_event(
            root,
            &Event::SessionCreated {
                session: root,
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: "/tmp/hya-core-session-project".to_string(),
                project,
                kind,
            },
        )
        .await
        .unwrap();
    root
}

#[tokio::test]
async fn subagent_session_inherits_parent_project_and_kind() {
    let engine = engine().await;
    let project = ProjectId::new();
    let root = root_with(&engine, Some(project), SessionKind::Project).await;

    let child = engine.create(spec(Some(root))).await.unwrap();
    let grandchild = engine.create(spec(Some(child))).await.unwrap();

    for session in [child, grandchild] {
        let projection = engine.read_projection(session).await.unwrap();
        assert_eq!(projection.session.project, Some(project));
        assert_eq!(projection.session.kind, SessionKind::Project);
    }
}

#[tokio::test]
async fn subagent_of_temporary_session_is_temporary() {
    let engine = engine().await;
    let root = root_with(&engine, None, SessionKind::Temporary).await;

    let child = engine.create(spec(Some(root))).await.unwrap();

    let projection = engine.read_projection(child).await.unwrap();
    assert_eq!(projection.session.project, None);
    assert_eq!(projection.session.kind, SessionKind::Temporary);
}

#[tokio::test]
async fn root_session_records_no_project_yet() {
    let engine = engine().await;
    let root = engine.create(spec(None)).await.unwrap();

    let projection = engine.read_projection(root).await.unwrap();
    assert_eq!(projection.session.project, None);
    assert_eq!(projection.session.kind, SessionKind::Project);
}
