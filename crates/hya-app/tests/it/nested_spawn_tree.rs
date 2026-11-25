//! Integration tests for `hya-app`: nested spawn tree and root roster registration.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use hya_app::spawn_team_supervisor;
use hya_bundle::AgentRole;
use hya_engine::{
    AgentSpec, BoundSpawnSender, CategoryRegistry, CreateSession, EventBus, ResidentSupervisor,
    SessionEngine,
};
use hya_proto::{AgentName, Event, MemberId, ModelRef, SessionId, SubagentMode};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, SpawnMember};

use crate::support;

async fn spawn_one(
    engine: &SessionEngine,
    spawn_sender: &BoundSpawnSender,
    parent: SessionId,
    caller: &str,
    agent_type: &str,
) -> SessionId {
    let binding = engine.bind_runtime(&std::env::temp_dir()).unwrap();
    let agents = engine.agent_roster_for_binding(&binding, caller).unwrap();
    let outcomes = tokio::time::timeout(
        Duration::from_secs(5),
        spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent, agents)
            .spawn(
                hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
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

#[tokio::test]
async fn nested_spawn_reaches_root_tree_and_registers_two_generations_in_roster() {
    let provider_router = Arc::new(ProviderRouter::new().with(Arc::new(
        support::IdentityFakeProvider::new(FakeProvider::scripted(Vec::new())),
    )));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(1);
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            provider_router,
            support::test_runtime(
                Arc::new(hya_tool_loader::builtins()),
                &[
                    ("build", AgentRole::Main, &["explore"]),
                    ("explore", AgentRole::Subagent, &["plan"]),
                    ("general", AgentRole::Main, &[]),
                    ("plan", AgentRole::Subagent, &[]),
                ],
            ),
            permission,
            EventBus::default(),
        )
        .with_spawn_sender(spawn_sender.clone()),
    );
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "test".to_string(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    };
    let resident = ResidentSupervisor::start(engine.clone());
    let _tasks = spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        agent.clone(),
        Arc::new(CategoryRegistry::default()),
        resident,
    );

    let root = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let child = spawn_one(&engine, &spawn_sender, root, "build", "explore").await;
    let grandchild = spawn_one(&engine, &spawn_sender, child, "explore", "plan").await;

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let root_proj = engine.read_projection(root).await.unwrap();
            let child_proj = engine.read_projection(child).await.unwrap();
            let linked = root_proj
                .session
                .members
                .iter()
                .any(|m| m.child == Some(child))
                && child_proj
                    .session
                    .members
                    .iter()
                    .any(|m| m.child == Some(grandchild));
            let settled = root_proj.team.roster.len() >= 2
                && root_proj
                    .team
                    .roster
                    .values()
                    .all(|e| e.status == hya_proto::RosterStatus::Idle);
            if linked && settled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("nested spawn projection timed out");

    // Pending (unlinked) member row does not disrupt descendant roster lookup.
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
                directive: String::new(),
                tool_call: None,
            },
        )
        .await
        .unwrap();

    let projection = engine.read_projection(root).await.unwrap();
    for (session, agent_type) in [
        (child, AgentName::new("explore")),
        (grandchild, AgentName::new("plan")),
    ] {
        let entry = projection
            .team
            .roster
            .values()
            .find(|e| e.session == session)
            .expect("descendant roster entry");
        assert!(!entry.handle.is_empty());
        assert_eq!(entry.agent_type, agent_type);
        assert_eq!(entry.mode, SubagentMode::Resident);
    }
    assert!(
        projection
            .team
            .roster
            .values()
            .all(|e| e.session != root || e.handle == "main")
    );
}
