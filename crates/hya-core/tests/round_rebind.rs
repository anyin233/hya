//! Integration tests for `hya-core`: round-boundary dynamic rebinding.
//!
//! Root turns re-check the runtime generation at every round boundary, so
//! tools, hooks, skills, and agent prompts published between two model calls
//! take effect at the next round without interrupting the in-flight round
//! (design §3, Q8). Any rebind failure is fail-open: the turn keeps the
//! activation-time generation. Bound/Resolved activations (subagents,
//! Workflow members) pin their activation-time binding and never rebind.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::stream;
use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, PreparedAgentBundle,
    PreparedInstallableBundle, ResourceView, SpawnLifecycle,
};
use hya_core::{
    AgentCatalog, AgentSpec, CoreError, CreateSession, EventBus, MemberSpec, MemberStatus,
    RuntimeCatalogRefresh, RuntimeRegistry, SessionEngine, run_team,
};
use hya_proto::{AgentName, FinishReason, MemberId, MessageId, ModelRef, SessionId, ToolCallId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// Tool called in round 1 to force a second round; present from activation.
const FIRST_ROUND_TOOL: &str = "marker";

/// Tool published between the rounds by the armed refresh.
const LATE_TOOL: &str = "late_tool";

/// Records every model request and plays a two-round script per turn: a
/// `marker` tool call (forcing a second round), then a plain stop.
struct RoundRecorder {
    turn: AtomicUsize,
    requests: Mutex<Vec<CompletionRequest>>,
}

impl RoundRecorder {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            turn: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for RoundRecorder {
    fn id(&self) -> &str {
        "fake"
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
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request);
        let idx = self.turn.fetch_add(1, Ordering::SeqCst);
        let script: Vec<FakeStep> = if idx == 0 {
            vec![
                FakeStep::ToolCall {
                    name: FIRST_ROUND_TOOL.to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        } else {
            vec![FakeStep::Finish(FinishReason::Stop)]
        };
        let events = FakeProvider::materialize(&script, session, message);
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

/// Registers one additional tool into the live registry exactly once, on the
/// Nth refresh call (0-based). Call 0 is the activation bind; call 1 is the
/// round-2 rebind.
struct ArmedToolRefresh {
    arm_on_call: usize,
    calls: AtomicUsize,
}

#[async_trait]
impl RuntimeCatalogRefresh for ArmedToolRefresh {
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call != self.arm_on_call {
            return Ok(false);
        }
        runtime
            .refresh(|candidate| candidate.register_tool(support::MarkerTool::new(LATE_TOOL)))?;
        Ok(true)
    }
}

/// Fails every refresh after the activation bind, simulating a broken
/// external catalog while the round-2 rebind is running.
struct FailingRefresh {
    calls: AtomicUsize,
}

#[async_trait]
impl RuntimeCatalogRefresh for FailingRefresh {
    async fn refresh_if_changed(&self, _runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(false);
        }
        Err(CoreError::Invalid("simulated refresh failure".to_string()))
    }
}

/// Publishes a replacement agent catalog exactly once, on the Nth call.
struct ArmedCatalogRefresh {
    arm_on_call: usize,
    calls: AtomicUsize,
    replacement: Arc<AgentCatalog>,
}

#[async_trait]
impl RuntimeCatalogRefresh for ArmedCatalogRefresh {
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call != self.arm_on_call {
            return Ok(false);
        }
        runtime.publish_catalog(Arc::clone(&self.replacement))?;
        Ok(true)
    }
}

/// Counts refresh calls without changing anything.
struct CountingRefresh {
    calls: AtomicUsize,
}

#[async_trait]
impl RuntimeCatalogRefresh for CountingRefresh {
    async fn refresh_if_changed(&self, _runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(false)
    }
}

/// One installed bundle agent with an explicit prompt, over the built-ins.
fn single_agent_catalog(stable_id: &str, prompt: &str) -> Arc<AgentCatalog> {
    let bundle = PreparedAgentBundle {
        format_version: 2,
        identity: BundleIdentity {
            id: format!("hya/round-rebind-{stable_id}"),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        namespace: None,
        digest: format!("test-only-{stable_id}"),
        agent: PreparedAgent {
            id: AgentName::new(stable_id),
            description: None,
            role: AgentRole::Main,
            color: None,
            prompt: Some(prompt.to_string()),
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
    };
    let prepared = PreparedInstallableBundle::Agent(Box::new(bundle));
    let Ok(bundles) = BundleCatalog::from_prepared(&[prepared]) else {
        panic!("test bundle catalog must be valid");
    };
    let Ok(catalog) = AgentCatalog::new(Arc::new(bundles)) else {
        panic!("test agent catalog must be valid");
    };
    Arc::new(catalog)
}

/// Registry over the built-ins with the round-1 marker tool captured in the
/// initial snapshot, so it is visible on every plane (including the
/// internal-public bundle clamp).
fn runtime_with_marker(catalog: Arc<AgentCatalog>) -> Arc<RuntimeRegistry> {
    let tools = ToolRegistry::builtins();
    tools
        .register(support::MarkerTool::new(FIRST_ROUND_TOOL))
        .unwrap();
    Arc::new(RuntimeRegistry::new(tools, catalog))
}

/// Engine with tool-call permissions and the given catalog refresh hook.
async fn engine_with(
    runtime: Arc<RuntimeRegistry>,
    provider: Arc<RoundRecorder>,
    refresh: Arc<dyn RuntimeCatalogRefresh>,
) -> Arc<SessionEngine> {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        "*",
        Mode::Allow,
    )]));
    Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(provider)),
            runtime,
            permission,
            EventBus::default(),
        )
        .with_catalog_refresh(refresh),
    )
}

async fn root_session(
    engine: &SessionEngine,
    workdir: &support::TestDir,
    agent: &str,
) -> SessionId {
    engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new(agent),
            model: ModelRef::new("fake"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap()
}

fn base_spec(workdir: &support::TestDir) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("general"),
        model: ModelRef::new("fake"),
        system_prompt: "base prompt".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    }
}

fn tool_names(request: &CompletionRequest) -> Vec<String> {
    request
        .tools
        .iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect()
}

/// A tool published between round 1 and round 2 of a Root turn is advertised
/// in round 2's model request; round 1 ran to completion uninterrupted.
#[tokio::test]
async fn root_turn_rebinds_new_tool_at_round_boundary() {
    let provider = RoundRecorder::new();
    let refresh = Arc::new(ArmedToolRefresh {
        arm_on_call: 1,
        calls: AtomicUsize::new(0),
    });
    let runtime = runtime_with_marker(support::builtin_only_catalog());
    let engine = engine_with(runtime, Arc::clone(&provider), refresh.clone()).await;
    let workdir = support::TestDir::new("round-rebind-tool");
    let session = root_session(&engine, &workdir, "general").await;
    engine
        .admit_user_prompt(session, "call the marker tool".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(session, &base_spec(&workdir), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(finish, FinishReason::Stop);
    // One refresh at activation, one at the round-2 rebind.
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 2);
    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        2,
        "round 1 must complete and round 2 must run"
    );
    assert!(!tool_names(&requests[0]).contains(&LATE_TOOL.to_string()));
    assert!(tool_names(&requests[0]).contains(&FIRST_ROUND_TOOL.to_string()));
    assert!(
        tool_names(&requests[1]).contains(&LATE_TOOL.to_string()),
        "the tool published between rounds must be advertised in round 2"
    );
}

/// A rebind failure at a round boundary never kills the turn: the next round
/// proceeds with the activation-time binding (fail-open).
#[tokio::test]
async fn root_turn_survives_round_rebind_failure_with_old_tools() {
    let provider = RoundRecorder::new();
    let refresh = Arc::new(FailingRefresh {
        calls: AtomicUsize::new(0),
    });
    let runtime = runtime_with_marker(support::builtin_only_catalog());
    let engine = engine_with(runtime, Arc::clone(&provider), refresh.clone()).await;
    let workdir = support::TestDir::new("round-rebind-failure");
    let session = root_session(&engine, &workdir, "general").await;
    engine
        .admit_user_prompt(session, "call the marker tool".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(session, &base_spec(&workdir), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        finish,
        FinishReason::Stop,
        "a rebind failure must not fail the turn"
    );
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 2);
    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        2,
        "round 2 must still run after the failed rebind"
    );
    assert_eq!(
        tool_names(&requests[1]),
        tool_names(&requests[0]),
        "the failed rebind must keep the activation-time tools"
    );
}

/// Bound activations (team members) keep their activation-time binding for
/// the whole turn: their rounds never rebind, never refresh.
#[tokio::test]
async fn bound_member_rounds_do_not_rebind() {
    let provider = RoundRecorder::new();
    let refresh = Arc::new(CountingRefresh {
        calls: AtomicUsize::new(0),
    });
    let runtime = runtime_with_marker(support::builtin_only_catalog());
    let engine = engine_with(runtime, Arc::clone(&provider), refresh.clone()).await;
    let workdir = support::TestDir::new("round-rebind-member");
    let lead = root_session(&engine, &workdir, "general").await;
    let spec = MemberSpec {
        id: MemberId::new(),
        agent: base_spec(&workdir),
        binding: engine.bind_runtime(workdir.path()).unwrap(),
        agents: Arc::from([]),
        resources: None,
        guidance: None,
        directive: "two rounds".to_string(),
        description: String::new(),
        session: None,
        sidecar_factory: None,
        tool_call: Some(ToolCallId::new()),
    };

    let evidence = run_team(engine.clone(), lead, vec![spec], CancellationToken::new()).await;

    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Done);
    // The member ran two rounds without a single runtime refresh: bound
    // activations never rebind.
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.requests().len(), 2);
}

/// An agent prompt published in a new generation takes effect at the next
/// round: the request carries the re-materialized system prompt without
/// rebuilding the transcript.
#[tokio::test]
async fn root_turn_applies_agent_prompt_change_at_round_boundary() {
    let provider = RoundRecorder::new();
    let refresh = Arc::new(ArmedCatalogRefresh {
        arm_on_call: 1,
        calls: AtomicUsize::new(0),
        replacement: single_agent_catalog("heated-agent", "round-rebind prompt v2"),
    });
    let runtime = runtime_with_marker(single_agent_catalog(
        "heated-agent",
        "round-rebind prompt v1",
    ));
    let engine = engine_with(runtime, Arc::clone(&provider), refresh.clone()).await;
    let workdir = support::TestDir::new("round-rebind-prompt");
    let session = root_session(&engine, &workdir, "heated-agent").await;
    engine
        .admit_user_prompt(session, "call the marker tool".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(session, &base_spec(&workdir), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(finish, FinishReason::Stop);
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 2);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let first = requests[0].system.as_deref().unwrap_or_default();
    assert!(
        first.contains("round-rebind prompt v1"),
        "round 1 carries the activation-time prompt, got: {first}"
    );
    let second = requests[1].system.as_deref().unwrap_or_default();
    assert!(
        second.contains("round-rebind prompt v2"),
        "round 2 carries the re-materialized prompt, got: {second}"
    );
    assert!(
        !second.contains("round-rebind prompt v1"),
        "the old prompt must be replaced, not stacked, got: {second}"
    );
}
