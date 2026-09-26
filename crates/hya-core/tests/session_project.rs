//! Integration tests for `hya-core`: ADR-0024 session Project and kind.
//! A root session records the Project and kind it was created with; a
//! subagent session's `SessionCreated` carries its parent's instead.

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
    spec_in(parent, None, SessionKind::Project)
}

fn spec_in(
    parent: Option<SessionId>,
    project: Option<ProjectId>,
    kind: SessionKind,
) -> CreateSession {
    CreateSession {
        parent,
        agent: AgentName::new("build"),
        model: ModelRef::new("fake"),
        workdir: "/tmp/hya-core-session-project".to_string(),
        project,
        kind,
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

    // A subagent's own spec never overrides its parent's Project or kind.
    let child = engine
        .create(spec_in(
            Some(root),
            Some(ProjectId::new()),
            SessionKind::Project,
        ))
        .await
        .unwrap();
    let grandchild = engine
        .create(spec_in(Some(child), None, SessionKind::Temporary))
        .await
        .unwrap();

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
async fn root_session_without_a_project_records_none() {
    let engine = engine().await;
    let root = engine.create(spec(None)).await.unwrap();

    let projection = engine.read_projection(root).await.unwrap();
    assert_eq!(projection.session.project, None);
    assert_eq!(projection.session.kind, SessionKind::Project);
}

#[tokio::test]
async fn root_session_records_its_project_and_kind() {
    let engine = engine().await;
    let project = ProjectId::new();

    let in_project = engine
        .create(spec_in(None, Some(project), SessionKind::Project))
        .await
        .unwrap();
    let temporary = engine
        .create(spec_in(None, None, SessionKind::Temporary))
        .await
        .unwrap();

    let projection = engine.read_projection(in_project).await.unwrap();
    assert_eq!(projection.session.project, Some(project));
    assert_eq!(projection.session.kind, SessionKind::Project);
    let projection = engine.read_projection(temporary).await.unwrap();
    assert_eq!(projection.session.project, None);
    assert_eq!(projection.session.kind, SessionKind::Temporary);
}

#[tokio::test]
async fn temporary_root_session_cannot_name_a_project() {
    let engine = engine().await;

    let error = engine
        .create(spec_in(
            None,
            Some(ProjectId::new()),
            SessionKind::Temporary,
        ))
        .await
        .unwrap_err();

    assert!(
        matches!(error, hya_core::CoreError::Invalid(_)),
        "unexpected error: {error:?}"
    );
}
