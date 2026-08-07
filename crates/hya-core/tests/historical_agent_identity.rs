#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Historical AgentName identity for replay / fork / continue (Commit 2).
//!
//! A session whose recorded AgentName is absent from the current catalog must:
//! - replay and project the exact AgentName bytes unchanged (no catalog lookup);
//! - allow read-only fork/copy with identity preserved (no catalog lookup);
//! - fail closed with AGENT_DEFINITION_MISSING before any provider call when
//!   the original or forked session is actually continued — never rewrite to
//!   general/base.

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, ModelPolicy,
    PreparedAgent, PreparedBundle, ResourceView, SpawnLifecycle,
};
use hya_core::{AgentCatalog, AgentSpec, CreateSession, EventBus, RuntimeRegistry, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, ModelRef, Role};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use tokio_util::sync::CancellationToken;

/// Historical id that must never be rewritten to a catalog fallback.
const HISTORICAL_AGENT: &str = "legacy-custom-agent-v1";
const HISTORICAL_BYTES: &str = "legacy-custom-agent-v1";

struct CaptureProvider {
    requests: Mutex<Vec<CompletionRequest>>,
}

#[async_trait]
impl Provider for CaptureProvider {
    fn id(&self) -> &str {
        "capture"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            reasoning_request: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request);
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
            },
        )])))
    }
}

/// Catalog with ordinary fallback agents only — historical id is intentionally absent.
fn catalog_without_historical() -> Arc<AgentCatalog> {
    // One bundle per agent: `base` is the only non-builtin id here, since
    // `build` and `general` are now compiled-in built-ins.
    let bundles = ["base"]
        .into_iter()
        .map(|stable_id| PreparedBundle {
            format_version: 1,
            identity: BundleIdentity {
                id: format!("hya/historical-identity-{stable_id}"),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            digest: format!("test-only-{stable_id}"),
            agent: PreparedAgent {
                id: AgentName::new(stable_id),
                description: None,
                role: AgentRole::Main,
                color: None,
                prompt: Some(format!("{stable_id} prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                resource_view: ResourceView::default(),
                can_spawn: Vec::new(),
                hook_refs: Vec::new(),
            },
            tools: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
        })
        .collect::<Vec<_>>();
    let bundles = BundleCatalog::from_prepared(&bundles).expect("valid bundle catalog");
    Arc::new(AgentCatalog::new(Arc::new(bundles)).expect("valid agent catalog"))
}

async fn engine_with(provider: Arc<CaptureProvider>) -> SessionEngine {
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(
        ToolRegistry::builtins().snapshot(),
        catalog_without_historical(),
    ));
    let router = Arc::new(ProviderRouter::new().with(provider));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        runtime,
        permission,
        EventBus::default(),
    )
}

fn base_agent(workdir: &std::path::Path) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("base"),
        model: ModelRef::new("base-model"),
        system_prompt: "base system prompt".to_string(),
        workdir: workdir.to_path_buf(),
        reasoning: None,
    }
}

async fn create_historical_session(
    engine: &SessionEngine,
    workdir: &std::path::Path,
) -> hya_proto::SessionId {
    engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new(HISTORICAL_AGENT),
            model: ModelRef::new("session-model"),
            workdir: workdir.to_string_lossy().into_owned(),
        })
        .await
        .expect("create historical session")
}

#[tokio::test]
async fn historical_agent_name_replays_and_projects_exact_bytes_without_catalog_lookup() {
    let workdir = support::TestDir::new("hist-replay");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(provider.clone()).await;
    let session = create_historical_session(&engine, workdir.path()).await;

    let envs = engine.replay(session).await.expect("replay");
    assert!(
        !envs.is_empty(),
        "historical session must produce durable events"
    );
    let created = envs.iter().find_map(|envelope| match &envelope.event {
        Event::SessionCreated { agent, .. } => Some(agent.as_str().to_string()),
        _ => None,
    });
    assert_eq!(
        created.as_deref(),
        Some(HISTORICAL_BYTES),
        "SessionCreated must retain exact historical AgentName bytes"
    );

    let projection = engine.read_projection(session).await.expect("projection");
    let projected = projection
        .session
        .agent
        .as_ref()
        .map(AgentName::as_str)
        .expect("projected agent");
    assert_eq!(
        projected, HISTORICAL_BYTES,
        "projection must keep exact historical AgentName; no catalog rewrite"
    );
    assert_ne!(projected, "general");
    assert_ne!(projected, "base");
    assert_ne!(projected, "build");
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "replay/projection must not resolve providers or definitions"
    );
}

#[tokio::test]
async fn historical_agent_name_survives_read_only_fork_copy_without_catalog_lookup() {
    let workdir = support::TestDir::new("hist-fork");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(provider.clone()).await;
    let source = create_historical_session(&engine, workdir.path()).await;
    engine
        .admit_user_prompt(source, "historical message".to_string())
        .await
        .unwrap();

    let source_projection = engine.read_projection(source).await.unwrap();
    let source_agent = source_projection
        .session
        .agent
        .clone()
        .expect("source agent");
    assert_eq!(source_agent.as_str(), HISTORICAL_BYTES);

    // Mirror compat session fork: create with projected agent bytes, copy messages.
    // No catalog lookup on this path.
    let forked = engine
        .create(CreateSession {
            parent: None,
            agent: source_agent.clone(),
            model: source_projection
                .session
                .model
                .clone()
                .unwrap_or_else(|| ModelRef::new("session-model")),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .expect("fork create with historical identity");
    engine
        .copy_messages_to_session(forked, &source_projection, None)
        .await
        .expect("fork message copy");

    let forked_projection = engine.read_projection(forked).await.unwrap();
    let forked_agent = forked_projection
        .session
        .agent
        .as_ref()
        .map(AgentName::as_str)
        .expect("forked agent");
    assert_eq!(
        forked_agent, HISTORICAL_BYTES,
        "forked session must preserve exact historical AgentName bytes"
    );
    assert_eq!(
        forked_agent,
        source_agent.as_str(),
        "fork identity must match source identity bytes"
    );
    assert_ne!(forked_agent, "general");
    assert_ne!(forked_agent, "base");

    let forked_created = engine
        .replay(forked)
        .await
        .unwrap()
        .into_iter()
        .find_map(|envelope| match envelope.event {
            Event::SessionCreated { agent, .. } => Some(agent),
            _ => None,
        })
        .expect("fork SessionCreated");
    assert_eq!(forked_created.as_str(), HISTORICAL_BYTES);
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "fork/copy must not consult provider or rewrite missing definitions"
    );
}

#[tokio::test]
async fn historical_agent_continue_fails_definition_missing_before_provider_no_general_rewrite() {
    let workdir = support::TestDir::new("hist-continue");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(provider.clone()).await;
    let session = create_historical_session(&engine, workdir.path()).await;
    engine
        .admit_user_prompt(session, "continue me".to_string())
        .await
        .unwrap();

    // Identity still exact after admit (no rewrite on write path).
    let before = engine.read_projection(session).await.unwrap();
    assert_eq!(
        before.session.agent.as_ref().map(AgentName::as_str),
        Some(HISTORICAL_BYTES)
    );

    let err = engine
        .run_turn(
            session,
            &base_agent(workdir.path()),
            CancellationToken::new(),
        )
        .await
        .expect_err("missing historical definition must fail on continue");
    assert!(
        err.to_string().contains("AGENT_DEFINITION_MISSING"),
        "expected AGENT_DEFINITION_MISSING, got {err}"
    );
    assert!(
        err.to_string().contains(HISTORICAL_AGENT),
        "error must name the recorded historical id, got {err}"
    );
    assert!(
        !err.to_string().contains("general") || err.to_string().contains(HISTORICAL_AGENT),
        "must not rewrite failure to general/base"
    );
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "continue must not call provider after definition miss"
    );

    // Projection identity still unchanged after failed continue.
    let after = engine.read_projection(session).await.unwrap();
    assert_eq!(
        after.session.agent.as_ref().map(AgentName::as_str),
        Some(HISTORICAL_BYTES),
        "failed continue must not rewrite durable AgentName"
    );
}

#[tokio::test]
async fn forked_historical_session_continue_fails_definition_missing_before_provider() {
    let workdir = support::TestDir::new("hist-fork-continue");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(provider.clone()).await;
    let source = create_historical_session(&engine, workdir.path()).await;
    engine
        .admit_user_prompt(source, "source prompt".to_string())
        .await
        .unwrap();
    let source_projection = engine.read_projection(source).await.unwrap();
    let forked = engine
        .create(CreateSession {
            parent: None,
            agent: source_projection
                .session
                .agent
                .clone()
                .expect("source agent"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .copy_messages_to_session(forked, &source_projection, None)
        .await
        .unwrap();
    engine
        .admit_user_prompt(forked, "fork continue".to_string())
        .await
        .unwrap();

    let err = engine
        .run_turn(
            forked,
            &base_agent(workdir.path()),
            CancellationToken::new(),
        )
        .await
        .expect_err("forked historical continue must fail closed");
    assert!(
        err.to_string().contains("AGENT_DEFINITION_MISSING"),
        "expected AGENT_DEFINITION_MISSING, got {err}"
    );
    assert!(
        err.to_string().contains(HISTORICAL_AGENT),
        "error must name the recorded historical id, got {err}"
    );
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "forked continue must not call provider"
    );
    let forked_projection = engine.read_projection(forked).await.unwrap();
    assert_eq!(
        forked_projection
            .session
            .agent
            .as_ref()
            .map(AgentName::as_str),
        Some(HISTORICAL_BYTES)
    );
}

/// Wrap one prepared bundle as an agent catalog alongside the compiled-in built-ins.
fn agent_catalog(bundle: PreparedBundle) -> Arc<AgentCatalog> {
    let bundles = BundleCatalog::from_prepared(&[bundle]).expect("valid bundle catalog");
    Arc::new(AgentCatalog::new(Arc::new(bundles)).expect("valid agent catalog"))
}
