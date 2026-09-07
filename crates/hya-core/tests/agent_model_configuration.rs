//! Configuration-first Agent model snapshots and root-Session overrides.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use hya_core::{
    AgentModelConfiguration, AgentSpec, CreateSession, EventBus, RuntimeRegistry, SessionEngine,
};
use hya_proto::{AgentName, Event, ModelRef, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use tokio::sync::Mutex;

use support::{TestDir, builtin_only_catalog};

fn model(value: &str) -> ModelRef {
    ModelRef::new(format!("fake/{value}"))
}

struct RecordingProvider {
    requests: Arc<Mutex<Vec<ModelRef>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            parallel_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().await.push(request.model);
        let event = Event::MessageFinished {
            session,
            message,
            role: hya_proto::Role::Assistant,
            finish: hya_proto::FinishReason::Stop,
            tokens: None,
        };
        Ok(Box::pin(stream::iter([Ok::<_, ProviderError>(event)])))
    }
}

async fn recording_engine() -> (SessionEngine, Arc<Mutex<Vec<ModelRef>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
    });
    let providers = Arc::new(ProviderRouter::new().with(provider));
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        providers,
        Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            builtin_only_catalog(),
        )),
        permission,
        EventBus::default(),
    );
    (engine, requests)
}

async fn engine() -> SessionEngine {
    let providers =
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(Vec::new()))));
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        providers,
        Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            builtin_only_catalog(),
        )),
        permission,
        EventBus::default(),
    )
}

async fn session(engine: &SessionEngine, id: SessionId, parent: Option<SessionId>) -> SessionId {
    engine
        .create_with_id(
            Some(id),
            CreateSession {
                parent,
                agent: AgentName::new("general"),
                model: model("base"),
                workdir: "/tmp".to_string(),
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn configuration_before_session_and_root_override_replay_isolated_and_immutable() {
    let dir = TestDir::new("agent-model-configuration");
    let engine = engine().await;
    let runtime = engine.runtime_registry();

    // Configuration is published before any Session exists, so the first fresh
    // binding observes it without a remembered-preference round trip.
    runtime.publish_agent_model_configuration(AgentModelConfiguration {
        builtin: BTreeMap::from([("general".to_string(), model("configured"))]),
        bundles: BTreeMap::new(),
    });

    let root = session(&engine, SessionId::new(), None).await;
    let child = session(&engine, SessionId::new(), Some(root)).await;
    let other_root = session(&engine, SessionId::new(), None).await;

    let configured_binding = engine.bind_session_runtime(root, dir.path()).await.unwrap();
    assert_eq!(
        configured_binding.configured_agent_model("general"),
        Some(&model("configured"))
    );
    assert_eq!(
        configured_binding
            .resolve_agent("general")
            .unwrap()
            .model_policy
            .model
            .as_deref(),
        Some("fake/configured")
    );

    engine
        .set_agent_model_override(child, AgentName::new("general"), Some(model("temporary")))
        .await
        .unwrap();

    let replay = engine.replay(root).await.unwrap();
    assert!(replay.iter().any(|envelope| matches!(
        &envelope.event,
        Event::SessionAgentModelOverrideSet {
            session,
            agent,
            model: Some(override_model),
        } if *session == root
            && agent.as_str() == "general"
            && override_model == &model("temporary")
    )));
    let projected = engine.read_projection(root).await.unwrap();
    assert_eq!(
        projected.session.agent_model_overrides.get("general"),
        Some(&model("temporary"))
    );

    let old_binding = engine.bind_session_runtime(root, dir.path()).await.unwrap();
    let child_binding = engine
        .bind_session_runtime(child, dir.path())
        .await
        .unwrap();
    let other_binding = engine
        .bind_session_runtime(other_root, dir.path())
        .await
        .unwrap();
    assert_eq!(
        child_binding.session_agent_model("general"),
        Some(&model("temporary"))
    );
    assert_eq!(
        child_binding
            .resolve_agent("general")
            .unwrap()
            .model_policy
            .model
            .as_deref(),
        Some("fake/temporary")
    );
    assert_eq!(other_binding.session_agent_model("general"), None);

    // A later Session event cannot mutate already-captured bindings.
    engine
        .set_agent_model_override(root, AgentName::new("general"), Some(model("new")))
        .await
        .unwrap();
    assert_eq!(
        old_binding.session_agent_model("general"),
        Some(&model("temporary"))
    );
    let new_binding = engine.bind_session_runtime(root, dir.path()).await.unwrap();
    assert_eq!(
        new_binding.session_agent_model("general"),
        Some(&model("new"))
    );

    // Clearing removes the temporary layer and reveals the captured file model.
    engine
        .set_agent_model_override(root, AgentName::new("general"), None)
        .await
        .unwrap();
    let cleared = engine
        .bind_session_runtime(child, dir.path())
        .await
        .unwrap();
    assert_eq!(cleared.session_agent_model("general"), None);
    assert_eq!(
        cleared
            .resolve_agent("general")
            .unwrap()
            .model_policy
            .model
            .as_deref(),
        Some("fake/configured")
    );
}

#[tokio::test]
async fn session_override_drives_omitted_requests_but_explicit_model_stays_local() {
    let dir = TestDir::new("agent-model-request-precedence");
    let (engine, requests) = recording_engine().await;
    engine
        .runtime_registry()
        .publish_agent_model_configuration(AgentModelConfiguration {
            builtin: BTreeMap::from([("general".to_string(), model("configured"))]),
            bundles: BTreeMap::new(),
        });
    let root = session(&engine, SessionId::new(), None).await;
    engine
        .set_agent_model_override(root, AgentName::new("general"), Some(model("temporary")))
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("general"),
        model: model("base"),
        system_prompt: "system".to_string(),
        workdir: dir.path().to_path_buf(),
        reasoning: None,
    };

    engine
        .admit_user_prompt(root, "omitted model".to_string())
        .await
        .unwrap();
    engine
        .run_turn(root, &agent, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(requests.lock().await.as_slice(), &[model("temporary")]);
    requests.lock().await.clear();

    engine
        .admit_user_prompt(root, "explicit model".to_string())
        .await
        .unwrap();
    engine
        .run_turn_with_external_dirs_and_guidance(
            root,
            &agent,
            tokio_util::sync::CancellationToken::new(),
            &[],
            None,
            Some(model("explicit")),
        )
        .await
        .unwrap();
    assert_eq!(requests.lock().await.as_slice(), &[model("explicit")]);
    requests.lock().await.clear();

    // A child started after the explicit parent request receives the durable
    // Session-tree override, never the parent's one-request model.
    let child = session(&engine, SessionId::new(), Some(root)).await;
    engine
        .admit_user_prompt(child, "child model".to_string())
        .await
        .unwrap();
    engine
        .run_turn(child, &agent, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(requests.lock().await.as_slice(), &[model("temporary")]);
    engine
        .set_agent_model_override(root, AgentName::new("general"), None)
        .await
        .unwrap();
    requests.lock().await.clear();
    engine
        .admit_user_prompt(root, "configured fallback".to_string())
        .await
        .unwrap();
    engine
        .run_turn(root, &agent, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(requests.lock().await.as_slice(), &[model("configured")]);
}
#[test]
fn empty_model_layers_preserve_semantic_binding_identity() {
    let dir = TestDir::new("agent-model-empty-identity");
    let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), builtin_only_catalog());
    let baseline = runtime.bind_turn(dir.path()).unwrap();
    let empty = baseline
        .clone()
        .with_agent_model_configuration(AgentModelConfiguration::default())
        .with_session_agent_models(BTreeMap::new());
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    assert_eq!(
        baseline.semantic_fingerprint_v1(&permission),
        empty.semantic_fingerprint_v1(&permission)
    );
}
