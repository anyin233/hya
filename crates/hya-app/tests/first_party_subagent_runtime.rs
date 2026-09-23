//! The installed first-party worker definition spawns as a resident actor.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use hya_app::{InstalledBundleRefresh, spawn_team_supervisor};
use hya_bundle::{BundleSource, prepare_package};
use hya_core::{
    AgentSpec, BoundSpawnSender, CategoryRegistry, CreateSession, EventBus, ResidentSupervisor,
    RuntimeRegistry, SessionEngine,
};
use hya_proto::{AgentName, MailEndpoint, MailKind, ModelRef, Role, SessionId, ToolCallId};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_store::{BundleInstallCandidate, BundleRegistry, SessionStore};
use hya_tool::{PermissionPlane, PermissionRules, SpawnMember, ToolOperation, ToolRegistry};

#[tokio::test]
async fn installed_subagent_bundle_spawns_a_resident_worker_that_replays_mail() {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../bundles/first-party/subagents");
    let prepared = prepare_package(BundleSource::read_directory(source_root).unwrap()).unwrap();
    let registry_path = std::env::temp_dir().join(format!("hya-subagents-{}.db", SessionId::new()));
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x55; 32],
                prepared_digest: prepared.digest().to_string(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1,
            },
        )
        .await
        .unwrap();

    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        hya_app::builtin_agent_catalog().unwrap(),
    ));
    InstalledBundleRefresh::new(registry_path.clone())
        .refresh_if_changed(runtime.as_ref())
        .await
        .unwrap();
    let workdir = std::env::temp_dir();
    let binding = runtime.bind_turn(&workdir).unwrap();
    assert!(binding.resolve_spawn("build", "hya-worker").is_ok());
    assert!(
        binding
            .resolve_spawn("build", "hya-transient-worker")
            .is_err()
    );
    assert!(
        binding
            .resolve_spawn("build", "hya-resident-worker")
            .is_err()
    );

    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(Vec::new()))));
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(4);
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            router.clone(),
            runtime,
            permission,
            EventBus::default(),
        )
        .with_spawn_sender(spawn_sender.clone()),
    );
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "parent".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let resident = ResidentSupervisor::start(engine.clone());
    spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        base.clone(),
        router,
        Arc::new(CategoryRegistry::default()),
        resident,
    );
    let parent = engine
        .create(CreateSession {
            parent: None,
            agent: base.name,
            model: base.model,
            workdir: workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();
    let spawner = spawn_sender
        .for_binding(&binding)
        .for_session_with_agents(parent, agents);

    let resident = spawner
        .spawn(
            ToolOperation::from_tool_call(ToolCallId::new()),
            vec![SpawnMember {
                description: "resident fixture".to_string(),
                prompt: "first resident turn".to_string(),
                subagent_type: "hya-worker".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(resident[0].status, "running");
    let resident_session: SessionId = resident[0].session.parse().unwrap();
    wait_for_assistant_messages(&engine, resident_session, 1).await;
    engine
        .mail_send(
            parent,
            MailEndpoint::Handle(resident[0].member.clone()),
            MailKind::Message,
            "second resident turn".to_string(),
        )
        .await
        .unwrap();
    wait_for_assistant_messages(&engine, resident_session, 2).await;
    let replay = engine.store().replay(resident_session).await.unwrap();
    assert!(replay.iter().any(|envelope| matches!(
        &envelope.event,
        hya_proto::Event::TextDelta { delta, .. } if delta.contains("second resident turn")
    )));

    drop(registry);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", registry_path.display()));
    }
}

async fn wait_for_assistant_messages(engine: &SessionEngine, session: SessionId, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let projection = engine.read_projection(session).await.unwrap();
            if projection
                .session
                .messages
                .iter()
                .filter(|message| message.role == Role::Assistant)
                .count()
                >= count
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("resident activation completed");
}
