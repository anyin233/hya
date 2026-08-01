#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, BundleOrigin, HarnessAccess, ModelPolicy,
    PreparedAgent, PreparedBundle, ResourceView, SpawnLifecycle,
};
use hya_core::{AgentSpec, CreateSession, EventBus, RuntimeRegistry, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

use crate::{AppState, ServerState};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-server-reference-test-{nanos}-{serial}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn prepared(stable_id: &str, role: AgentRole) -> PreparedAgent {
    PreparedAgent {
        local_id: stable_id.to_string(),
        stable_id: AgentName::new(stable_id),
        description: None,
        role,
        color: None,
        prompt: None,
        prompt_source: None,
        prompt_digest: None,
        model_policy: ModelPolicy::default(),
        workdir: None,
        spawn_lifecycle: SpawnLifecycle::Transient,
        harness_access: HarnessAccess::Full,
        resource_view: ResourceView::default(),
        can_spawn: Vec::new(),
        hook_refs: Vec::new(),
    }
}

fn test_runtime(tools: Arc<ToolRegistry>) -> Arc<RuntimeRegistry> {
    let bundle = PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: "hya/reference-unit-tests".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        origin: BundleOrigin::Builtin,
        immutable: true,
        digest: "test-only".to_string(),
        agents: vec![
            prepared("build", AgentRole::Main),
            prepared("plan", AgentRole::Main),
            prepared("general", AgentRole::Subagent),
        ],
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
    };
    let catalog = BundleCatalog::from_prepared(&[bundle]).expect("reference unit catalog");
    Arc::new(RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        Arc::new(catalog),
    ))
}

async fn state(workdir: PathBuf) -> ServerState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let runtime = test_runtime(tools);
    let engine = SessionEngine::new(store, providers, runtime, permission, EventBus::default());
    ServerState::new(AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "system".to_string(),
            workdir,
            reasoning: None,
        }),
    ))
}

#[tokio::test]
async fn session_agent_with_guidance_uses_session_workdir() {
    let server_dir = tempdir();
    let session_dir = tempdir();
    let st = state(server_dir.clone()).await;
    let session = st
        .engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: session_dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();

    let turn = super::reference::session_agent_with_guidance(&st, session).await;

    assert_eq!(turn.agent.workdir, session_dir);
    assert_ne!(turn.agent.workdir, server_dir);
    // Guidance is a separate layer, never concatenated into AgentSpec.
    assert!(!turn.agent.system_prompt.contains("<available_references>"));
}

#[tokio::test]
async fn init_agent_with_guidance_uses_session_agent_and_workdir() {
    let server_dir = tempdir();
    let session_dir = tempdir();
    let st = state(server_dir.clone()).await;
    let session = st
        .engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("plan"),
            model: ModelRef::new("fake"),
            workdir: session_dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();

    let turn = super::session_legacy::init_agent_with_guidance(&st, session).await;

    assert_eq!(turn.agent.name.as_str(), "plan");
    assert_eq!(turn.agent.workdir, session_dir);
    assert_ne!(turn.agent.workdir, server_dir);
}

/// e. No legacy disk prompt/reasoning consumer remains in reference.rs.
#[test]
fn reference_rs_has_no_legacy_disk_prompt_reasoning_consumer() {
    let source = include_str!("reference.rs");
    assert!(
        !source.contains("apply_agent_entry"),
        "legacy apply_agent_entry consumer must be removed from reference.rs"
    );
    assert!(
        !source.contains("entry.prompt"),
        "legacy disk entry.prompt must not be consumed in reference.rs"
    );
    assert!(
        !source.contains("resolve_reasoning"),
        "legacy disk reasoning overlay must not remain in reference.rs"
    );
    assert!(
        !source.contains("agent.system_prompt = format!"),
        "guidance must not be concatenated into AgentSpec.system_prompt"
    );
    assert!(
        !source.contains("agent.system_prompt = prompt"),
        "legacy disk prompt assignment must not remain in reference.rs"
    );
}
