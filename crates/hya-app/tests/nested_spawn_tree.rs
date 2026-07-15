#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use hya_app::spawn_team_supervisor;
use hya_core::{
    AgentSpec, CategoryRegistry, CreateSession, EventBus, ResidentSupervisor, SessionEngine,
};
use hya_proto::{AgentName, Event, MemberId, ModelRef, SessionId, SubagentMode};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, SpawnMember, SpawnerPlane, ToolRegistry};
use serde_json::Value;
use tower::ServiceExt;

struct NestedSpawn {
    engine: Arc<SessionEngine>,
    app: axum::Router,
    root: SessionId,
    child: SessionId,
    grandchild: SessionId,
}

async fn nested_spawn() -> NestedSpawn {
    let provider_router =
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(Vec::new()))));
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
    let (spawner, spawn_rx) = SpawnerPlane::new();
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            provider_router.clone(),
            Arc::new(ToolRegistry::builtins()),
            permission,
            EventBus::default(),
        )
        .with_spawner(spawner.clone()),
    );
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "test".to_string(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    };
    let resident = ResidentSupervisor::start(engine.clone());
    spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        agent.clone(),
        false,
        provider_router,
        Arc::new(CategoryRegistry::default()),
        resident,
    );

    let root = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let child = spawn_one(&spawner, root, "explore").await;
    let grandchild = spawn_one(&spawner, child, "plan").await;

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let root_projection = engine.read_projection(root).await.unwrap();
            let child_projection = engine.read_projection(child).await.unwrap();
            let root_has_child = root_projection
                .session
                .members
                .iter()
                .any(|member| member.child == Some(child));
            let child_has_grandchild = child_projection
                .session
                .members
                .iter()
                .any(|member| member.child == Some(grandchild));
            if root_has_child && child_has_grandchild {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("nested spawn projection timed out");

    engine
        .store()
        .append_event(
            root,
            &Event::MemberSpawned {
                session: root,
                member: MemberId::new(),
                child: None,
                subagent_type: AgentName::new("pending"),
                description: "pending fixture member".to_string(),
                depth: 1,
            },
        )
        .await
        .unwrap();

    let app = router(AppState::new(engine.clone(), Arc::new(agent)));
    NestedSpawn {
        engine,
        app,
        root,
        child,
        grandchild,
    }
}

async fn spawn_one(spawner: &SpawnerPlane, parent: SessionId, agent_type: &str) -> SessionId {
    let outcomes = tokio::time::timeout(
        Duration::from_secs(5),
        spawner.for_session(parent).spawn_background(
            vec![SpawnMember {
                description: format!("spawn {agent_type}"),
                prompt: format!("run {agent_type}"),
                subagent_type: agent_type.to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        ),
    )
    .await
    .expect("spawn timed out")
    .expect("spawn failed");
    outcomes[0].session.parse().expect("valid child session")
}

async fn tree(fixture: &NestedSpawn) -> Value {
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/session/{}/tree", fixture.child))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn nested_spawn_reaches_root_tree() {
    let fixture = nested_spawn().await;
    let tree = tree(&fixture).await;

    assert_eq!(tree["session"], fixture.root.to_string());
    assert_eq!(tree["children"][0]["session"], fixture.child.to_string());
    assert_eq!(
        tree["children"][0]["children"][0]["session"],
        fixture.grandchild.to_string()
    );
}

#[tokio::test]
async fn nested_spawn_registers_two_generations_in_root_roster() {
    let fixture = nested_spawn().await;
    let projection = fixture.engine.read_projection(fixture.root).await.unwrap();
    for (session, agent_type) in [
        (fixture.child, AgentName::new("explore")),
        (fixture.grandchild, AgentName::new("plan")),
    ] {
        let entry = projection
            .team
            .roster
            .values()
            .find(|entry| entry.session == session)
            .expect("descendant roster entry");
        assert!(!entry.handle.is_empty());
        assert_eq!(entry.agent_type, agent_type);
        assert_eq!(entry.mode, SubagentMode::Transient);
    }
}

#[tokio::test]
async fn tree_endpoint_attaches_roster_to_child_and_grandchild() {
    let fixture = nested_spawn().await;
    let projection = fixture.engine.read_projection(fixture.root).await.unwrap();
    let tree = tree(&fixture).await;

    assert!(tree.get("roster").is_none());
    let pending = tree["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node.get("session").is_none())
        .expect("member-only node");
    assert!(pending.get("roster").is_none());

    for (session, node) in [
        (fixture.child, &tree["children"][0]),
        (fixture.grandchild, &tree["children"][0]["children"][0]),
    ] {
        let entry = projection
            .team
            .roster
            .values()
            .find(|entry| entry.session == session)
            .expect("descendant roster entry");
        assert_eq!(node["roster"], serde_json::to_value(entry).unwrap());
    }
}
