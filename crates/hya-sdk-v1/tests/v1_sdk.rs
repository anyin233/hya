//! Integration tests for `hya-sdk-v1` against the in-process `/v1` router.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;

use hya_api::v1 as pb;
use hya_bundle::BundleCatalog;
use hya_core::{AgentSpec, EventBus, RuntimeRegistry, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_sdk_v1::{V1Sdk, V1SessionMirror};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

async fn state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("sdk v1 hello".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        providers,
        test_runtime(tools),
        perm,
        EventBus::default(),
    );
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

/// Minimal runtime for the in-process engine: the builtin agent catalog.
fn test_runtime(tools: Arc<ToolRegistry>) -> Arc<RuntimeRegistry> {
    let catalog = BundleCatalog::from_prepared(&[]).expect("sdk test catalog");
    let catalog = hya_core::AgentCatalog::new(Arc::new(catalog)).expect("sdk agent catalog");
    Arc::new(RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        Arc::new(catalog),
    ))
}

/// Serve the router on an ephemeral port and return its base URL.
async fn serve() -> String {
    let app = router(state().await);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

#[tokio::test]
async fn sdk_creates_prompts_and_streams_the_transcript() {
    let base = serve().await;
    let sdk = V1Sdk::new(base.clone(), std::env::temp_dir().to_string_lossy());

    let bootstrap = sdk.bootstrap().await.expect("bootstrap");
    assert!(!bootstrap.models.is_empty(), "bootstrap carries models");

    let session = sdk
        .create_session(pb::CreateSessionRequest {
            agent: "build".into(),
            model: "fake".into(),
            workdir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await
        .expect("create session");
    let session_id = session.id.clone();

    let mut frames = sdk.stream_session(&session_id, 0).await.expect("stream");
    let finished = sdk.prompt(&session_id, "say hello").await.expect("prompt");
    assert_eq!(finished.state, pb::TurnState::Finished as i32);
    assert_eq!(finished.finish, pb::FinishReason::Stop as i32);

    // Fold live frames into the mirror; re-seed from reads on resync.
    let transcript = sdk.list_messages(&session_id).await.expect("messages");
    let mut mirror = V1SessionMirror::from_messages(&transcript.messages);
    let mut guard = 0;
    while guard < 200 {
        match tokio::time::timeout(Duration::from_millis(250), frames.next()).await {
            Ok(Some(Ok(frame))) => {
                if mirror.apply(&frame) {
                    let replay = sdk.list_events(&session_id, mirror.last_seq).await;
                    if let Ok(replay) = replay {
                        for event in replay.events {
                            let frame = pb::StreamFrame {
                                frame: Some(pb::stream_frame::Frame::Event(event)),
                            };
                            mirror.apply(&frame);
                        }
                    }
                }
            }
            _ => break,
        }
        guard += 1;
    }
    let texts: Vec<String> = mirror
        .messages()
        .into_iter()
        .filter_map(|message| {
            message.parts.iter().find_map(|part| {
                part.kind
                    .as_ref()
                    .map(|kind| match kind {
                        pb::part_info::Kind::Text(text) => text.text.clone(),
                        _ => String::new(),
                    })
                    .filter(|text| !text.is_empty())
            })
        })
        .collect();
    assert!(
        texts.iter().any(|text| text.contains("sdk v1 hello")),
        "mirror must fold the streamed assistant text, got {texts:?}"
    );
}

#[tokio::test]
async fn sdk_manages_projects_and_project_sessions() {
    let base = serve().await;
    let sdk = V1Sdk::new(base, "/unused");

    let created = sdk
        .create_project("app", vec!["/sdk/app".into(), "/sdk/docs".into()])
        .await
        .expect("create project");
    assert_eq!(created.roots, vec!["/sdk/app", "/sdk/docs"]);
    assert_eq!(sdk.get_project(&created.id).await.expect("get").name, "app");
    assert_eq!(sdk.list_projects().await.expect("list").len(), 1);

    let resolved = sdk.resolve_project("/sdk/docs/x y").await.expect("resolve");
    assert_eq!(resolved.map(|project| project.id), Some(created.id.clone()));
    assert!(
        sdk.resolve_project("/elsewhere")
            .await
            .expect("resolve")
            .is_none()
    );
    let ensured = sdk
        .ensure_project_for_path("/sdk/app/src")
        .await
        .expect("ensure");
    assert!(!ensured.created);
    assert_eq!(
        ensured.project.map(|project| project.id),
        Some(created.id.clone())
    );

    let session = sdk
        .create_session(pb::CreateSessionRequest {
            agent: "build".into(),
            model: "fake".into(),
            project_id: created.id.clone(),
            kind: pb::SessionKind::Project as i32,
            ..Default::default()
        })
        .await
        .expect("create session");
    assert_eq!(session.workdir, "/sdk/app");
    assert_eq!(session.project_id, created.id);
    assert_eq!(session.kind, pb::SessionKind::Project as i32);
    let listed = sdk
        .list_project_sessions(&created.id, None)
        .await
        .expect("list sessions");
    assert_eq!(listed.sessions.len(), 1);

    let updated = sdk
        .update_project(pb::UpdateProjectRequest {
            project: created.id.clone(),
            name: Some("renamed".into()),
            roots: Vec::new(),
        })
        .await
        .expect("update");
    assert_eq!(updated.name, "renamed");
    assert_eq!(updated.roots, created.roots);
    assert_eq!(updated.session_count, 1);

    let error = sdk.delete_project(&created.id).await.unwrap_err();
    assert!(
        matches!(&error, hya_sdk_v1::SdkError::Api { code, .. } if code == "failed_precondition"),
        "{error:?}"
    );
    let error = sdk.get_project("prj_missing").await.unwrap_err();
    assert!(
        matches!(&error, hya_sdk_v1::SdkError::Api { code, .. } if code == "invalid_argument"),
        "{error:?}"
    );
}
