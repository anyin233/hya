//! `hya-client` Project calls against the in-process `/v1` router.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use hya_api::v1 as pb;
use hya_bundle::BundleCatalog;
use hya_client::{Client, ClientError};
use hya_core::{AgentSpec, EventBus, RuntimeRegistry, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

async fn serve() -> String {
    let providers =
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(Vec::new()))));
    let tools = Arc::new(ToolRegistry::builtins());
    let catalog = BundleCatalog::from_prepared(&[]).expect("catalog");
    let catalog = hya_core::AgentCatalog::new(Arc::new(catalog)).expect("agent catalog");
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        Arc::new(catalog),
    ));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        providers,
        runtime,
        perm,
        EventBus::default(),
    );
    let app = router(AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

#[tokio::test]
async fn client_manages_projects() {
    let client = Client::new(serve().await);
    let ensured = client
        .ensure_project_for_path("/client/repo")
        .await
        .expect("ensure");
    assert!(ensured.created);
    let project = ensured.project.expect("project");
    assert_eq!(project.name, "repo");
    assert_eq!(project.roots, vec!["/client/repo"]);

    let created = client
        .create_project(&pb::CreateProjectRequest {
            name: "other".into(),
            roots: vec!["/client/other".into()],
        })
        .await
        .expect("create");
    let listed = client.list_projects().await.expect("list");
    assert_eq!(listed.projects.len(), 2);
    assert_eq!(
        client.get_project(&created.id).await.expect("get").name,
        "other"
    );
    let resolved = client
        .resolve_project("/client/repo/src")
        .await
        .expect("resolve");
    assert_eq!(resolved.project.map(|p| p.id), Some(project.id.clone()));

    let session = client
        .create_session(&pb::CreateSessionRequest {
            agent: "build".into(),
            model: "fake".into(),
            workdir: Some("/client/repo/src".into()),
            kind: pb::SessionKind::Project as i32,
            ..Default::default()
        })
        .await
        .expect("session")
        .session
        .expect("session info");
    assert_eq!(session.project_id, project.id);
    assert_eq!(
        client
            .list_project_sessions(&project.id)
            .await
            .expect("sessions")
            .sessions
            .len(),
        1
    );

    let updated = client
        .update_project(&pb::UpdateProjectRequest {
            project: created.id.clone(),
            name: None,
            roots: vec!["/client/a".into(), "/client/b".into()],
        })
        .await
        .expect("update");
    assert_eq!(updated.roots, vec!["/client/a", "/client/b"]);
    client.delete_project(&created.id).await.expect("delete");
    let error = client.get_project(&created.id).await.unwrap_err();
    assert!(
        matches!(&error, ClientError::Api { code, .. } if code == "not_found"),
        "{error:?}"
    );
    let error = client.delete_project(&project.id).await.unwrap_err();
    assert!(
        matches!(&error, ClientError::Api { code, .. } if code == "failed_precondition"),
        "{error:?}"
    );
}
